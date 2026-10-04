//! Endpoints: synchronous call/reply between clients and a server.
//!
//! An endpoint has one [`ServerEnd`] (receive, needs `RECEIVE`) and any
//! number of [`ClientEnd`]s (call, needs `SEND`), each shared by capability.
//! When the server end goes, calls fail with `PeerClosed`; when the last
//! client end goes, `receive` does, so nobody blocks forever.
//!
//! A call blocks the client until the reply. If a server is already waiting
//! in `receive`, the caller switches straight to it (direct switch); the
//! reply makes the client runnable again. Each received call yields a
//! single-use [`ReplyToken`]; dropping it unanswered fails the call with
//! `NoReply`.
//!
//! **Badges** (ADR-0019): the server can mint extra client ends that carry a
//! non-zero badge. Calls report the badge of the end they came through, so
//! one endpoint can stand for many objects (e.g. open files). When a badged
//! end is closed, the server receives a close event for its badge, so it can
//! release per-handle state.
//!
//! **Bound notifications** (ADR-0023): a notification bound to the endpoint
//! wakes its receiver too, and `receive_event` reports its bits as an
//! event, so one thread serves calls and handles asynchronous events.

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use spin::Mutex;

use super::{IpcError, Message, Notification};
use crate::arch;
use crate::sched::{self, Thread};

struct Call {
    caller: Arc<Thread>,
    badge: u64,
    request: Option<Message>,
    reply: Option<Result<Message, IpcError>>,
}

type CallSlot = Arc<Mutex<Call>>;

struct State {
    /// Server threads blocked in `receive`.
    receivers: VecDeque<Arc<Thread>>,
    /// Calls not yet received; their callers are blocked.
    pending: VecDeque<CallSlot>,
    /// Badges of closed client ends, not yet reported to the server.
    closed: VecDeque<u64>,
    server_open: bool,
    /// Live client ends.
    clients: usize,
    /// The notification whose signals are delivered as events.
    bound: Option<Arc<Notification>>,
}

pub struct Endpoint {
    state: Mutex<State>,
}

impl Endpoint {
    /// A new endpoint, returned as its server end and an unbadged client end.
    pub fn create() -> (Arc<ServerEnd>, Arc<ClientEnd>) {
        let endpoint = Arc::new(Self {
            state: Mutex::new(State {
                receivers: VecDeque::new(),
                pending: VecDeque::new(),
                closed: VecDeque::new(),
                server_open: true,
                clients: 1,
                bound: None,
            }),
        });
        (
            Arc::new(ServerEnd {
                endpoint: endpoint.clone(),
            }),
            Arc::new(ClientEnd { endpoint, badge: 0 }),
        )
    }

    /// A bound notification was signalled: wake a waiting receiver.
    pub fn wake_receiver(&self) {
        let receiver = arch::without_interrupts(|| self.state.lock().receivers.pop_front());
        if let Some(receiver) = receiver {
            sched::wake(receiver);
        }
    }
}

pub struct ServerEnd {
    endpoint: Arc<Endpoint>,
}

pub struct ClientEnd {
    endpoint: Arc<Endpoint>,
    badge: u64,
}

impl core::fmt::Debug for ServerEnd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ServerEnd")
    }
}

impl core::fmt::Debug for ClientEnd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ClientEnd(badge {})", self.badge)
    }
}

/// What `ServerEnd::receive_event` delivers.
#[expect(
    clippy::large_enum_variant,
    reason = "short-lived stack value; boxing would add an allocation to every receive"
)]
pub enum Event {
    /// A call through a client end with `badge` (0 = unbadged).
    Call {
        request: Message,
        badge: u64,
        token: ReplyToken,
    },
    /// The last capability to the client end with `badge` was closed.
    Closed { badge: u64 },
    /// The bound notification was signalled with these bits.
    Notification { bits: u64 },
}

impl ClientEnd {
    /// Sends `request` and blocks until the server replies.
    ///
    /// Capabilities attached to `request` move to the server. If the call
    /// fails with `PeerClosed` before delivery, they are destroyed with the
    /// message (the server they were meant for is gone).
    pub fn call(&self, request: Message) -> Result<Message, IpcError> {
        arch::without_interrupts(|| {
            if sched::interrupted() {
                return Err(IpcError::Interrupted);
            }
            let slot = Arc::new(Mutex::new(Call {
                caller: sched::current(),
                badge: self.badge,
                request: Some(request),
                reply: None,
            }));
            let receiver = {
                let mut state = self.endpoint.state.lock();
                if !state.server_open {
                    return Err(IpcError::PeerClosed);
                }
                state.pending.push_back(slot.clone());
                state.receivers.pop_front()
            };
            match receiver {
                Some(server) => sched::block_and_switch_to(server),
                None => sched::block(),
            }
            let reply = slot.lock().reply.take();
            match reply {
                Some(reply) => reply,
                // Interrupted: withdraw the call if it was not received
                // (a server holding it may still answer; nobody listens).
                None if sched::interrupted() => {
                    self.endpoint
                        .state
                        .lock()
                        .pending
                        .retain(|pending| !Arc::ptr_eq(pending, &slot));
                    Err(IpcError::Interrupted)
                }
                None => panic!("caller woken without a reply"),
            }
        })
    }
}

impl ServerEnd {
    /// A new client end carrying `badge` (non-zero).
    pub fn mint(&self, badge: u64) -> Arc<ClientEnd> {
        debug_assert_ne!(badge, 0);
        arch::without_interrupts(|| self.endpoint.state.lock().clients += 1);
        Arc::new(ClientEnd {
            endpoint: self.endpoint.clone(),
            badge,
        })
    }

    /// Binds `notification`: its signals become events of this endpoint,
    /// replacing an earlier binding. Fails if it is bound elsewhere.
    pub fn bind(&self, notification: Arc<Notification>) -> Result<(), IpcError> {
        notification
            .bind(Arc::downgrade(&self.endpoint))
            .map_err(|()| IpcError::Busy)?;
        let old = arch::without_interrupts(|| {
            self.endpoint
                .state
                .lock()
                .bound
                .replace(notification.clone())
        });
        if let Some(old) = old
            && !Arc::ptr_eq(&old, &notification)
        {
            old.unbind();
        }
        Ok(())
    }

    /// Blocks until a call or a close event arrives, or (if
    /// `notifications`) the bound notification is signalled.
    pub fn receive_event(&self, notifications: bool) -> Result<Event, IpcError> {
        arch::without_interrupts(|| {
            loop {
                {
                    let mut state = self.endpoint.state.lock();
                    if let Some(slot) = state.pending.pop_front() {
                        let (request, badge) = {
                            let mut call = slot.lock();
                            (call.request.take(), call.badge)
                        };
                        let request = request.expect("pending call has a request");
                        return Ok(Event::Call {
                            request,
                            badge,
                            token: ReplyToken { slot: Some(slot) },
                        });
                    }
                    if let Some(badge) = state.closed.pop_front() {
                        return Ok(Event::Closed { badge });
                    }
                    if notifications && let Some(bound) = &state.bound {
                        let bits = bound.take_bits();
                        if bits != 0 {
                            return Ok(Event::Notification { bits });
                        }
                    }
                    if state.clients == 0 {
                        return Err(IpcError::PeerClosed);
                    }
                    if sched::interrupted() {
                        let me = sched::current();
                        state
                            .receivers
                            .retain(|receiver| !Arc::ptr_eq(receiver, &me));
                        return Err(IpcError::Interrupted);
                    }
                    state.receivers.push_back(sched::current());
                }
                sched::block();
            }
        })
    }

    /// Blocks until a call arrives (close events are skipped). Returns the
    /// request and the token that answers it.
    pub fn receive(&self) -> Result<(Message, ReplyToken), IpcError> {
        loop {
            if let Event::Call { request, token, .. } = self.receive_event(false)? {
                return Ok((request, token));
            }
        }
    }
}

impl Drop for ServerEnd {
    fn drop(&mut self) {
        arch::without_interrupts(|| {
            let (pending, bound) = {
                let mut state = self.endpoint.state.lock();
                state.server_open = false;
                state.closed.clear();
                (core::mem::take(&mut state.pending), state.bound.take())
            };
            if let Some(bound) = bound {
                bound.unbind();
            }
            for slot in pending {
                complete(&slot, Err(IpcError::PeerClosed));
            }
        });
    }
}

impl Drop for ClientEnd {
    fn drop(&mut self) {
        arch::without_interrupts(|| {
            let receivers = {
                let mut state = self.endpoint.state.lock();
                state.clients -= 1;
                if state.clients == 0 {
                    // They re-check, see no clients left and fail.
                    core::mem::take(&mut state.receivers)
                } else if self.badge != 0 && state.server_open {
                    state.closed.push_back(self.badge);
                    state.receivers.pop_front().into_iter().collect()
                } else {
                    VecDeque::new()
                }
            };
            for server in receivers {
                sched::wake(server);
            }
        });
    }
}

/// Answers one received call. Single use; dropping it fails the call with
/// [`IpcError::NoReply`].
pub struct ReplyToken {
    slot: Option<CallSlot>,
}

impl ReplyToken {
    pub fn reply(mut self, reply: Message) {
        if let Some(slot) = self.slot.take() {
            complete(&slot, Ok(reply));
        }
    }
}

impl Drop for ReplyToken {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            complete(&slot, Err(IpcError::NoReply));
        }
    }
}

/// Stores the outcome of a call and wakes its (blocked) caller.
fn complete(slot: &CallSlot, outcome: Result<Message, IpcError>) {
    arch::without_interrupts(|| {
        let caller = {
            let mut call = slot.lock();
            call.reply = Some(outcome);
            call.caller.clone()
        };
        sched::wake(caller);
    });
}

/// Endpoint self-test and round-trip benchmark, for smoke-test boots.
pub fn self_test() {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use oceans_capability::Rights;

    use crate::object::{self, CapTable, Capability, KernelObject, MemoryObject, ObjectKind};
    use crate::{klog, time};

    const ECHO_REVERSED: u64 = 1;
    const SUM_MEMORY: u64 = 2;
    const PING: u64 = 3;
    const BENCH_CALLS: u64 = 10_000;

    static SERVER_DONE: AtomicBool = AtomicBool::new(false);
    static SERVER_CALLS: AtomicU64 = AtomicU64::new(0);
    static SILENT_DONE: AtomicBool = AtomicBool::new(false);

    // A server with its own capability table, as a process would have.
    fn server(arg: usize) {
        // SAFETY: `arg` is the `Box<Arc<ServerEnd>>` leaked by the spawner.
        let end = unsafe { Box::from_raw(arg as *mut Arc<ServerEnd>) };
        let mut table = CapTable::new(object::DEFAULT_CAP_LIMIT);
        while let Ok((mut request, token)) = end.receive() {
            SERVER_CALLS.fetch_add(1, Ordering::Relaxed);
            let reply = match request.label {
                ECHO_REVERSED => {
                    let mut data = [0u8; super::MAX_INLINE_BYTES];
                    let len = request.data().len();
                    for (i, &b) in request.data().iter().rev().enumerate() {
                        data[i] = b;
                    }
                    Message::new(ECHO_REVERSED, &data[..len])
                }
                SUM_MEMORY => {
                    let mut sum = 0u64;
                    for capability in request.take_capabilities() {
                        let handle = table.insert(capability).expect("server table has room");
                        let memory = object::memory(&mut table, handle, Rights::READ)
                            .expect("received a readable memory object");
                        let mut bytes = [0u8; 16];
                        memory.read(0, &mut bytes).expect("read memory object");
                        sum += bytes.iter().map(|&b| u64::from(b)).sum::<u64>();
                        table.remove(handle).expect("close");
                    }
                    Message::new(SUM_MEMORY, &sum.to_le_bytes())
                }
                _ => Message::new(PING, &[]),
            };
            token.reply(reply.expect("reply fits"));
        }
        drop(end);
        SERVER_DONE.store(true, Ordering::Release);
    }

    // Receives one call and drops it unanswered, then closes its end.
    fn silent_server(arg: usize) {
        // SAFETY: as for `server`.
        let end = unsafe { Box::from_raw(arg as *mut Arc<ServerEnd>) };
        let (_request, token) = end.receive().expect("one call");
        drop(token);
        drop(end);
        SILENT_DONE.store(true, Ordering::Release);
    }

    fn wait_for(flag: &AtomicBool, what: &str) {
        let deadline = time::ticks() + 5 * u64::from(time::HZ);
        while !flag.load(Ordering::Acquire) {
            assert!(
                time::ticks() < deadline,
                "IPC self-test: timed out waiting for {what}"
            );
            sched::sleep_ms(5);
        }
    }

    // Wire up through capabilities, as processes will.
    let (server_end, client_end) = Endpoint::create();
    let mut client_table = CapTable::new(object::DEFAULT_CAP_LIMIT);
    let client = client_table
        .insert(Capability::new(
            KernelObject::EndpointClient(client_end),
            object::default_rights(ObjectKind::EndpointClient),
        ))
        .expect("insert client end");
    let client_end = object::client_end(&mut client_table, client, Rights::SEND).expect("SEND");
    let mut server_table = CapTable::new(object::DEFAULT_CAP_LIMIT);
    let server_handle = server_table
        .insert(Capability::new(
            KernelObject::EndpointServer(server_end),
            object::default_rights(ObjectKind::EndpointServer),
        ))
        .expect("insert server end");
    assert!(
        object::server_end(&mut server_table, server_handle, Rights::SEND).is_err(),
        "a server end must not grant SEND"
    );
    let server_end =
        object::server_end(&mut server_table, server_handle, Rights::RECEIVE).expect("RECEIVE");
    drop(server_table);
    sched::spawn(
        "ipc-server",
        server,
        Box::into_raw(Box::new(server_end)) as usize,
    )
    .expect("spawn server");

    // 1. Inline data round trip.
    let reply = client_end
        .call(Message::new(ECHO_REVERSED, b"oceans").expect("fits"))
        .expect("echo call");
    assert_eq!(reply.data(), b"snaeco");

    // 2. Capability transfer: the memory object moves to the server.
    let memory = MemoryObject::new(4096).expect("memory object");
    memory.write(0, &[1, 2, 3, 4]).expect("write");
    let handle = client_table
        .insert(Capability::new(
            KernelObject::Memory(memory),
            object::default_rights(ObjectKind::Memory),
        ))
        .expect("insert memory");
    client_table
        .get(handle, Rights::TRANSFER)
        .expect("TRANSFER right");
    let capability = client_table.remove(handle).expect("take for transfer");
    let mut request = Message::new(SUM_MEMORY, &[]).expect("fits");
    request.attach(capability).expect("attach");
    let reply = client_end.call(request).expect("sum call");
    assert_eq!(reply.data(), &10u64.to_le_bytes());
    assert!(
        client_table.get(handle, Rights::NONE).is_err(),
        "capability moved out"
    );

    // 3. Round-trip latency.
    let start_ticks = time::ticks();
    let start_cycles = arch::cycles();
    for _ in 0..BENCH_CALLS {
        client_end
            .call(Message::new(PING, &[]).expect("fits"))
            .expect("ping");
    }
    let cycles = (arch::cycles() - start_cycles) / BENCH_CALLS;
    let elapsed_ms = (time::ticks() - start_ticks) * 1000 / u64::from(time::HZ);
    klog::info!(
        "IPC call/reply round trip: {cycles} cycles avg over {BENCH_CALLS} calls (~{} µs, tick resolution)",
        elapsed_ms * 1000 / BENCH_CALLS
    );

    // 4. Closing the client end ends the server's receive loop.
    drop(client_end);
    client_table.remove(client).expect("close client end");
    wait_for(&SERVER_DONE, "server shutdown on PeerClosed");
    assert_eq!(SERVER_CALLS.load(Ordering::Relaxed), BENCH_CALLS + 2);

    // 5. Unanswered call → NoReply; closed server → PeerClosed.
    let (server_end, client_end) = Endpoint::create();
    sched::spawn(
        "ipc-silent",
        silent_server,
        Box::into_raw(Box::new(server_end)) as usize,
    )
    .expect("spawn silent server");
    let outcome = client_end.call(Message::new(PING, &[]).expect("fits"));
    assert_eq!(outcome.err(), Some(IpcError::NoReply));
    wait_for(&SILENT_DONE, "silent server exit");
    let outcome = client_end.call(Message::new(PING, &[]).expect("fits"));
    assert_eq!(outcome.err(), Some(IpcError::PeerClosed));

    // 6. Badges: calls report the minted end's badge; closing it sends a
    //    close event; the unbadged end's close sends none.
    static BADGES_SEEN: AtomicU64 = AtomicU64::new(0);
    static BADGE_DONE: AtomicBool = AtomicBool::new(false);
    fn badge_server(arg: usize) {
        // SAFETY: as for `server`.
        let end = unsafe { Box::from_raw(arg as *mut Arc<ServerEnd>) };
        // Expected sequence: call with badge 7, close of 7, then PeerClosed.
        let mut log = 0u64;
        loop {
            match end.receive_event(false) {
                Ok(Event::Call { badge, token, .. }) => {
                    log = log * 10 + badge;
                    token.reply(Message::new(PING, &[]).expect("fits"));
                }
                Ok(Event::Closed { badge }) => log = log * 10 + 100 + badge,
                Ok(Event::Notification { .. }) => unreachable!("not requested"),
                Err(_) => break,
            }
        }
        BADGES_SEEN.store(log, Ordering::Relaxed);
        BADGE_DONE.store(true, Ordering::Release);
    }
    let (server_end, unbadged) = Endpoint::create();
    let badged = server_end.mint(7);
    sched::spawn(
        "ipc-badges",
        badge_server,
        Box::into_raw(Box::new(server_end)) as usize,
    )
    .expect("spawn badge server");
    badged
        .call(Message::new(PING, &[]).expect("fits"))
        .expect("badged call");
    drop(badged);
    drop(unbadged);
    wait_for(&BADGE_DONE, "badge server exit");
    // 7 (call), then 107 (close of 7): 7 * 10 + 107 = 177.
    assert_eq!(BADGES_SEEN.load(Ordering::Relaxed), 177);

    // 7. A bound notification wakes the receiver with its bits; calls still
    //    come first, and a notification binds to one endpoint only.
    static EVENTS_SEEN: AtomicU64 = AtomicU64::new(0);
    static EVENTS_DONE: AtomicBool = AtomicBool::new(false);
    fn event_server(arg: usize) {
        // SAFETY: as for `server`.
        let end = unsafe { Box::from_raw(arg as *mut Arc<ServerEnd>) };
        let mut log = 0u64;
        while let Ok(event) = end.receive_event(true) {
            match event {
                Event::Call { token, .. } => {
                    log = log * 10 + 1;
                    token.reply(Message::new(PING, &[]).expect("fits"));
                }
                Event::Notification { bits } => log = log * 10 + bits,
                Event::Closed { .. } => {}
            }
        }
        EVENTS_SEEN.store(log, Ordering::Relaxed);
        EVENTS_DONE.store(true, Ordering::Release);
    }
    let (server_end, client) = Endpoint::create();
    let notification = Notification::new();
    server_end.bind(notification.clone()).expect("bind");
    let (other, _other_client) = Endpoint::create();
    assert_eq!(other.bind(notification.clone()).err(), Some(IpcError::Busy));
    sched::spawn(
        "ipc-events",
        event_server,
        Box::into_raw(Box::new(server_end)) as usize,
    )
    .expect("spawn event server");
    sched::sleep_ms(20); // let it block in receive
    notification.signal(4);
    sched::sleep_ms(20);
    client
        .call(Message::new(PING, &[]).expect("fits"))
        .expect("call");
    notification.signal(2);
    sched::sleep_ms(20);
    drop(client);
    wait_for(&EVENTS_DONE, "event server exit");
    // 4 (signal), 1 (call), 2 (signal): 412.
    assert_eq!(EVENTS_SEEN.load(Ordering::Relaxed), 412);

    klog::info!("endpoint self-test passed");
}
