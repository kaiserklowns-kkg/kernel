//! Endpoints: synchronous call/reply between a client and a server.
//!
//! An endpoint has one [`ServerEnd`] (receive, needs `RECEIVE`) and one
//! [`ClientEnd`] (call, needs `SEND`), each shared by capability. Closing all
//! capabilities to one end fails the other side with `PeerClosed` instead of
//! leaving it blocked forever.
//!
//! A call blocks the client until the reply. If a server is already waiting
//! in `receive`, the caller switches straight to it (direct switch); the
//! reply makes the client runnable again. Each received call yields a
//! single-use [`ReplyToken`]; dropping it unanswered fails the call with
//! `NoReply`.

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use spin::Mutex;

use super::{IpcError, Message};
use crate::arch;
use crate::sched::{self, Thread};

struct Call {
    caller: Arc<Thread>,
    request: Option<Message>,
    reply: Option<Result<Message, IpcError>>,
}

type CallSlot = Arc<Mutex<Call>>;

struct State {
    /// Server threads blocked in `receive`.
    receivers: VecDeque<Arc<Thread>>,
    /// Calls not yet received; their callers are blocked.
    pending: VecDeque<CallSlot>,
    server_open: bool,
    client_open: bool,
}

pub struct Endpoint {
    state: Mutex<State>,
}

impl Endpoint {
    /// A new endpoint, returned as its two ends.
    pub fn create() -> (Arc<ServerEnd>, Arc<ClientEnd>) {
        let endpoint = Arc::new(Self {
            state: Mutex::new(State {
                receivers: VecDeque::new(),
                pending: VecDeque::new(),
                server_open: true,
                client_open: true,
            }),
        });
        (
            Arc::new(ServerEnd {
                endpoint: endpoint.clone(),
            }),
            Arc::new(ClientEnd { endpoint }),
        )
    }
}

pub struct ServerEnd {
    endpoint: Arc<Endpoint>,
}

pub struct ClientEnd {
    endpoint: Arc<Endpoint>,
}

impl core::fmt::Debug for ServerEnd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ServerEnd")
    }
}

impl core::fmt::Debug for ClientEnd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ClientEnd")
    }
}

impl ClientEnd {
    /// Sends `request` and blocks until the server replies.
    ///
    /// Capabilities attached to `request` move to the server. If the call
    /// fails with `PeerClosed` before delivery, they are destroyed with the
    /// message (the server they were meant for is gone).
    pub fn call(&self, request: Message) -> Result<Message, IpcError> {
        arch::without_interrupts(|| {
            let slot = Arc::new(Mutex::new(Call {
                caller: sched::current(),
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
            reply.expect("caller woken without a reply")
        })
    }
}

impl ServerEnd {
    /// Blocks until a call arrives. Returns the request and the token that
    /// answers it.
    pub fn receive(&self) -> Result<(Message, ReplyToken), IpcError> {
        arch::without_interrupts(|| {
            loop {
                {
                    let mut state = self.endpoint.state.lock();
                    if let Some(slot) = state.pending.pop_front() {
                        let request = slot.lock().request.take();
                        let request = request.expect("pending call has a request");
                        return Ok((request, ReplyToken { slot: Some(slot) }));
                    }
                    if !state.client_open {
                        return Err(IpcError::PeerClosed);
                    }
                    state.receivers.push_back(sched::current());
                }
                sched::block();
            }
        })
    }
}

impl Drop for ServerEnd {
    fn drop(&mut self) {
        arch::without_interrupts(|| {
            let pending = {
                let mut state = self.endpoint.state.lock();
                state.server_open = false;
                core::mem::take(&mut state.pending)
            };
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
                state.client_open = false;
                core::mem::take(&mut state.receivers)
            };
            // They re-check, see the closed client end and fail.
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

    klog::info!("endpoint self-test passed");
}
