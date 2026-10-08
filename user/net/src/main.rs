//! The Oceans network service (ADR-0023, ADR-0024, ADR-0043): the IPv4
//! and IPv6 stack (`oceans-net`, with TCP) between a network driver and
//! the programs that use the network.
//!
//! - Toward the driver (`use = netdev`): a session with a shared frame
//!   buffer. The driver signals when frames arrive; the stack sends with
//!   short calls.
//! - Toward programs (`provide = net`): sockets, each a badged capability.
//!   A program passes a notification when it opens one and is signalled
//!   when it becomes readable.
//! - One notification, bound to the service's endpoint, carries both the
//!   driver's "frames waiting" and the stack's timer (DHCP and ARP
//!   retransmissions), so a single thread serves everything.
//!
//! The IPv4 address comes from DHCP; IPv6 addresses from stateless
//! autoconfiguration (a link-local one, then one per advertised prefix).
//! Without a driver the service still runs and reports `NoDevice`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use core::fmt::Write;

use alloc::vec::Vec;
use oceans_net::{
    AddressOrigin, AddressState, Colons, Config, Dotted, IpAddr, Ipv6, Ipv6Config,
    NetError as StackError, Recv, SocketId, Stack, TcpState,
};
use oceans_net_proto::netdev::{self, BUFFER_SIZE, RX_AREA, TX_AREA};
use oceans_net_proto::{
    Address6, MAX_DATA, MAX_DATA6, MAX_STREAM, MAX_STREAM_BUFFER, MIN_STREAM_BUFFER, NetInfo,
    NetInfo6, READER_BADGE, Status, TRUNCATED, address_state, op, state,
};
use oceans_rt::{Buffer, Directory, Error, Handle, Start, prot, rights};

oceans_rt::entry!(main);

/// Notification bits.
const FRAMES: u64 = 1;
const TIMER: u64 = 2;

struct Netdev {
    session: Handle,
    buffer: *mut u8,
}

struct Client {
    socket: SocketId,
    notification: Handle,
    bits: u64,
    /// TCP: the shared buffer (ADR-0030), mapped here.
    buffer: Option<(*mut u8, usize)>,
}

struct Net {
    log: Handle,
    server: Handle,
    events: Handle,
    device: Option<Netdev>,
    stack: Stack,
    clients: BTreeMap<u64, Client>,
    next_badge: u64,
    announced: Option<Config>,
    /// IPv6 as last logged: usable and duplicate addresses, router, DNS.
    announced6: Announced6,
}

#[derive(Default, PartialEq, Eq)]
struct Announced6 {
    usable: Vec<Ipv6>,
    duplicates: Vec<Ipv6>,
    router: Option<Ipv6>,
    dns: Option<Ipv6>,
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_str("net: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(server)) = (directory.find("log", "log"), directory.find_kind("provide"))
    else {
        return 3;
    };
    let Ok(events) = oceans_rt::notification_create() else {
        return 4;
    };
    if oceans_rt::endpoint_bind(server, events).is_err() {
        return 4;
    }
    let (device, mac) = match directory.find("use", "netdev").map(|d| connect(d, events)) {
        Some(Ok(connected)) => connected,
        Some(Err(problem)) => {
            say(log, format_args!("network device unavailable ({problem})"));
            (None, [0; 6])
        }
        None => {
            say(log, format_args!("no network device granted"));
            (None, [0; 6])
        }
    };
    let mut stack = Stack::new(mac);
    // Keys TCP initial sequence numbers (ADR-0024) with kernel randomness
    // (ADR-0026).
    stack.set_secret(oceans_rt::random_u64());
    if device.is_some() {
        // IPv6 interface identifiers (RFC 7217, ADR-0043) are keyed per
        // boot: addresses are stable while the system runs, and do not
        // follow the machine from boot to boot or network to network.
        let mut key = [0u8; 16];
        match oceans_rt::random(&mut key) {
            Ok(()) => stack.enable_ipv6(key, oceans_rt::clock_ms()),
            Err(_) => say(log, format_args!("no randomness: IPv6 stays off")),
        }
    }
    let mut net = Net {
        log,
        server,
        events,
        stack,
        device,
        clients: BTreeMap::new(),
        next_badge: 1,
        announced: None,
        announced6: Announced6::default(),
    };
    net.serve()
}

/// Opens a session with the driver; returns it and the device's MAC.
fn connect(driver: Handle, events: Handle) -> Result<(Option<Netdev>, [u8; 6]), &'static str> {
    let mut info = [0u8; 8];
    let got = oceans_rt::ipc_call_msg(driver, netdev::op::INFO, &[], &[], &mut info, &mut [])
        .map_err(|_| "driver not running")?;
    if got.label != Status::Ok as u64 || got.data_len < 6 {
        return Err("bad driver reply");
    }
    let mac: [u8; 6] = info[..6].try_into().expect("6 bytes");
    let memory = oceans_rt::memory_create(BUFFER_SIZE as u64).map_err(|_| "no memory")?;
    let buffer = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
    let shared = oceans_rt::duplicate(
        memory,
        rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
    );
    let _ = oceans_rt::close(memory);
    let buffer = buffer.map_err(|_| "cannot map the frame buffer")?;
    let shared = shared.map_err(|_| "cannot share the frame buffer")?;
    let signal = oceans_rt::duplicate(events, rights::SIGNAL | rights::TRANSFER)
        .map_err(|_| "cannot share the notification")?;
    let mut session = [Handle(0); 1];
    let got = oceans_rt::ipc_call_msg(
        driver,
        netdev::op::OPEN,
        &FRAMES.to_le_bytes(),
        &[shared, signal],
        &mut [],
        &mut session,
    )
    .map_err(|_| "driver refused the session")?;
    if got.label != Status::Ok as u64 || got.handles_len != 1 {
        return Err("driver refused the session");
    }
    Ok((
        Some(Netdev {
            session: session[0],
            buffer,
        }),
        mac,
    ))
}

fn status(error: StackError) -> Status {
    match error {
        StackError::NotConfigured => Status::NotConfigured,
        StackError::AddressInUse => Status::AddressInUse,
        StackError::NoRoute => Status::NoRoute,
        StackError::TooLarge => Status::TooLarge,
        StackError::BadSocket => Status::BadRequest,
        StackError::NoBuffers => Status::NoBuffers,
        StackError::NotConnected => Status::NotConnected,
        StackError::Closed => Status::Closed,
        StackError::Refused => Status::Refused,
        StackError::Reset => Status::Reset,
        StackError::TimedOut => Status::TimedOut,
    }
}

fn state_code(state: TcpState) -> u8 {
    match state {
        TcpState::Closed => state::CLOSED,
        TcpState::Listen => state::LISTEN,
        TcpState::SynSent => state::SYN_SENT,
        TcpState::SynReceived => state::SYN_RECEIVED,
        TcpState::Established => state::ESTABLISHED,
        TcpState::FinWait1 => state::FIN_WAIT_1,
        TcpState::FinWait2 => state::FIN_WAIT_2,
        TcpState::CloseWait => state::CLOSE_WAIT,
        TcpState::Closing => state::CLOSING,
        TcpState::LastAck => state::LAST_ACK,
        TcpState::TimeWait => state::TIME_WAIT,
    }
}

/// The `[... bits u64]` at the end of an open request.
fn bits_at_end(data: &[u8], len: usize) -> Result<u64, Status> {
    if data.len() != len {
        return Err(Status::BadRequest);
    }
    let bits = u64::from_le_bytes(data[len - 8..].try_into().expect("8 bytes"));
    if bits == 0 {
        return Err(Status::BadRequest);
    }
    Ok(bits)
}

impl Net {
    fn serve(&mut self) -> i64 {
        let mut data = [0u8; 256];
        let mut handles = [Handle(0); 4];
        self.stack.poll(oceans_rt::clock_ms());
        loop {
            self.after_events();
            let got = match oceans_rt::ipc_receive_msg(self.server, &mut data, &mut handles) {
                Ok(got) => got,
                Err(Error::PeerClosed) => return 0,
                Err(_) => return 5,
            };
            let now = oceans_rt::clock_ms();
            if got.signals != 0 {
                if got.signals & FRAMES != 0 {
                    self.receive_frames(now);
                }
                self.stack.poll(now);
                continue;
            }
            if got.closed {
                if let Some(client) = self.clients.remove(&got.badge) {
                    self.stack.release(client.socket, now);
                    if let Some((base, _)) = client.buffer {
                        let _ = oceans_rt::memory_unmap(base);
                    }
                    let _ = oceans_rt::close(client.notification);
                }
                continue;
            }
            let received = &handles[..got.handles_len];
            // Room for a datagram (7 + MAX_DATA) or a stream read (MAX_STREAM).
            let mut reply = [0u8; 256];
            let mut reply_handle = None;
            let result = self.handle(
                got.badge,
                got.label,
                &data[..got.data_len],
                received,
                &mut reply,
                &mut reply_handle,
                now,
            );
            let opened = matches!(
                got.label,
                op::UDP_OPEN
                    | op::PING_OPEN
                    | op::TCP_CONNECT
                    | op::TCP_CONNECT6
                    | op::TCP_LISTEN
                    | op::TCP_ACCEPT
            );
            let (status, len) = match result {
                Ok(len) => (Status::Ok, len),
                Err(status) => (status, 0),
            };
            // Capabilities are kept only by a successful open.
            if !(opened && status == Status::Ok) {
                for &handle in received {
                    let _ = oceans_rt::close(handle);
                }
            }
            let reply_handles: &[Handle] = match &reply_handle {
                Some(handle) => core::slice::from_ref(handle),
                None => &[],
            };
            if oceans_rt::ipc_reply_msg(status as u64, &reply[..len], reply_handles).is_err()
                && let Some(handle) = reply_handle
            {
                let _ = oceans_rt::close(handle);
            }
        }
    }

    /// After anything happened: send what the stack produced, wake readable
    /// sockets' owners, report configuration changes and re-arm the timer.
    fn after_events(&mut self) {
        while let Some(frame) = self.stack.transmit() {
            self.send_frame(&frame);
        }
        for socket in self.stack.take_ready() {
            if let Some(client) = self.clients.values().find(|c| c.socket == socket) {
                let _ = oceans_rt::notification_signal(client.notification, client.bits);
            }
        }
        let config = self.stack.config();
        if config != self.announced {
            match config {
                Some(c) => {
                    let mut line = Buffer::<120>::new();
                    let _ = write!(line, "{}/{}", Dotted(c.address), c.prefix);
                    if let Some(gateway) = c.gateway {
                        let _ = write!(line, " gateway {}", Dotted(gateway));
                    }
                    if let Some(dns) = c.dns {
                        let _ = write!(line, " dns {}", Dotted(dns));
                    }
                    say(
                        self.log,
                        format_args!("configured {} (DHCP)", line.as_str()),
                    );
                }
                None => say(self.log, format_args!("address lost; asking DHCP again")),
            }
            self.announced = config;
        }
        if let Some(config) = self.stack.ipv6_config() {
            self.announce6(&config);
        }
        let now = oceans_rt::clock_ms();
        let delay = match self.stack.next_deadline() {
            Some(deadline) => deadline.saturating_sub(now).max(1),
            None => 0, // cancels
        };
        if self.device.is_some() {
            let _ = oceans_rt::timer_set(self.events, TIMER, delay);
        }
    }

    /// Logs IPv6 addresses as they become usable or turn out duplicate,
    /// and router or DNS changes.
    fn announce6(&mut self, config: &Ipv6Config) {
        let now = Announced6 {
            usable: config
                .addresses
                .iter()
                .filter(|a| matches!(a.state, AddressState::Preferred | AddressState::Deprecated))
                .map(|a| a.address)
                .collect(),
            duplicates: config
                .addresses
                .iter()
                .filter(|a| a.state == AddressState::Duplicate)
                .map(|a| a.address)
                .collect(),
            router: config.router,
            dns: config.dns,
        };
        if now == self.announced6 {
            return;
        }
        for address in &config.addresses {
            let known = self.announced6.usable.contains(&address.address)
                || self.announced6.duplicates.contains(&address.address);
            if known {
                continue;
            }
            let origin = match address.origin {
                AddressOrigin::LinkLocal => "link-local",
                AddressOrigin::Slaac => "SLAAC",
            };
            match address.state {
                AddressState::Preferred | AddressState::Deprecated => say(
                    self.log,
                    format_args!(
                        "IPv6 {}/{} ({origin})",
                        Colons(address.address),
                        address.prefix
                    ),
                ),
                AddressState::Duplicate => say(
                    self.log,
                    format_args!(
                        "IPv6 {} is in use by another node: not used",
                        Colons(address.address)
                    ),
                ),
                AddressState::Tentative => {}
            }
        }
        if (now.router, now.dns) != (self.announced6.router, self.announced6.dns) {
            let mut line = Buffer::<120>::new();
            match now.router {
                Some(router) => {
                    let _ = write!(line, "router {}", Colons(router));
                }
                None => {
                    let _ = line.write_str("no router");
                }
            }
            if let Some(dns) = now.dns {
                let _ = write!(line, " dns {}", Colons(dns));
            }
            say(self.log, format_args!("IPv6 {}", line.as_str()));
        }
        self.announced6 = now;
    }

    fn send_frame(&mut self, frame: &[u8]) {
        let Some(device) = &self.device else {
            return;
        };
        if frame.len() > TX_AREA.end - TX_AREA.start {
            return;
        }
        // SAFETY: the transmit area of the mapped frame buffer holds the
        // frame (checked); the driver reads it only during this call.
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                device.buffer.add(TX_AREA.start),
                frame.len(),
            );
        }
        let mut request = [0u8; 6];
        request[..4].copy_from_slice(&(TX_AREA.start as u32).to_le_bytes());
        request[4..].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        // A full transmit queue drops the frame, as a NIC would.
        let _ = oceans_rt::ipc_call_msg(
            device.session,
            netdev::op::SEND,
            &request,
            &[],
            &mut [],
            &mut [],
        );
    }

    fn receive_frames(&mut self, now: u64) {
        let Some(device) = &self.device else {
            return;
        };
        let (session, buffer) = (device.session, device.buffer);
        loop {
            let mut reply = [0u8; 2];
            let Ok(got) =
                oceans_rt::ipc_call_msg(session, netdev::op::RECV, &[], &[], &mut reply, &mut [])
            else {
                return;
            };
            let count = u16::from_le_bytes(reply);
            if got.label != Status::Ok as u64 || count == 0 {
                return;
            }
            // SAFETY: the receive area of the mapped frame buffer, written
            // by the driver during the call that just returned.
            let area = unsafe {
                core::slice::from_raw_parts(buffer.add(RX_AREA.start), RX_AREA.end - RX_AREA.start)
            };
            let mut at = 0;
            for _ in 0..count {
                let Some(len) = area
                    .get(at..at + 2)
                    .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
                else {
                    break;
                };
                let Some(frame) = area.get(at + 2..at + 2 + len) else {
                    break;
                };
                self.stack.receive(frame, now);
                at += 2 + len.next_multiple_of(2);
            }
            // Answer ARP and pings promptly, even during a burst.
            while let Some(frame) = self.stack.transmit() {
                self.send_frame(&frame);
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one request's inputs and outputs"
    )]
    fn handle(
        &mut self,
        badge: u64,
        label: u64,
        data: &[u8],
        received: &[Handle],
        reply: &mut [u8],
        reply_handle: &mut Option<Handle>,
        now: u64,
    ) -> Result<usize, Status> {
        match (label, badge) {
            (op::INFO, _) => {
                let config = self.stack.config();
                let info = NetInfo {
                    configured: config.is_some(),
                    address: config.map_or([0; 4], |c| c.address),
                    prefix: config.map_or(0, |c| c.prefix),
                    gateway: config.and_then(|c| c.gateway).unwrap_or([0; 4]),
                    dns: config.and_then(|c| c.dns).unwrap_or([0; 4]),
                    mac: self.stack.mac(),
                };
                reply[..NetInfo::SIZE].copy_from_slice(&info.encode());
                Ok(NetInfo::SIZE)
            }
            (op::INFO6, _) => {
                let mut info = NetInfo6::disabled();
                if let Some(config) = self.stack.ipv6_config() {
                    info.enabled = true;
                    info.hop_limit = config.hop_limit;
                    info.mtu = config.mtu;
                    info.router = config.router;
                    info.dns = config.dns;
                    for address in &config.addresses {
                        let state = match address.state {
                            AddressState::Tentative => address_state::TENTATIVE,
                            AddressState::Preferred => address_state::PREFERRED,
                            AddressState::Deprecated => address_state::DEPRECATED,
                            AddressState::Duplicate => address_state::DUPLICATE,
                        };
                        // The stack holds at most as many as fit.
                        info.push(Address6 {
                            address: address.address,
                            prefix: address.prefix,
                            state,
                            link_local: address.origin == AddressOrigin::LinkLocal,
                        });
                    }
                }
                let mut encoded = [0u8; NetInfo6::MAX_SIZE];
                let len = info.encode(&mut encoded);
                reply[..len].copy_from_slice(&encoded[..len]);
                Ok(len)
            }
            // A reader end (ADR-0096): its badge is no socket's, so only
            // `INFO` and `INFO6` answer on it.
            (op::READER, 0) => {
                let end = oceans_rt::endpoint_mint(self.server, READER_BADGE)
                    .map_err(|_| Status::NoBuffers)?;
                *reply_handle = Some(end);
                Ok(0)
            }
            (op::TCP_CONNECT6, 0) => {
                if self.device.is_none() {
                    return Err(Status::NoDevice);
                }
                let bits = bits_at_end(data, 26)?;
                if received.len() != 1 {
                    return Err(Status::BadRequest);
                }
                let address: Ipv6 = data[..16].try_into().expect("16 bytes");
                let port = u16::from_le_bytes([data[16], data[17]]);
                let socket = self
                    .stack
                    .tcp_connect(IpAddr::from_mapped(address), port, now)
                    .map_err(status)?;
                self.open_client(socket, received[0], bits, reply_handle, now)?;
                Ok(0)
            }
            (op::SEND_TO6, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                if data.len() < 18 || data.len() > 18 + MAX_DATA6 {
                    return Err(Status::BadRequest);
                }
                let address: Ipv6 = data[..16].try_into().expect("16 bytes");
                let port = u16::from_le_bytes([data[16], data[17]]);
                self.stack
                    .send_to(socket, IpAddr::from_mapped(address), port, &data[18..], now)
                    .map_err(status)?;
                Ok(0)
            }
            (op::RECV6, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                let datagram = self.stack.recv(socket).ok_or(Status::Empty)?;
                let take = datagram.data.len().min(MAX_DATA6);
                reply[..16].copy_from_slice(&datagram.from.to_mapped());
                reply[16..18].copy_from_slice(&datagram.port.to_le_bytes());
                reply[18] = if take < datagram.data.len() {
                    TRUNCATED
                } else {
                    0
                };
                reply[19..19 + take].copy_from_slice(&datagram.data[..take]);
                Ok(19 + take)
            }
            (op::UDP_OPEN | op::PING_OPEN, 0) => {
                if self.device.is_none() {
                    return Err(Status::NoDevice);
                }
                let udp = label == op::UDP_OPEN;
                let bits = bits_at_end(data, if udp { 10 } else { 8 })?;
                if received.len() != 1 {
                    return Err(Status::BadRequest);
                }
                let socket = if udp {
                    self.stack.udp_bind(u16::from_le_bytes([data[0], data[1]]))
                } else {
                    self.stack.ping_open()
                }
                .map_err(status)?;
                self.open_client(socket, received[0], bits, reply_handle, now)?;
                let port = self.stack.local_port(socket).unwrap_or(0);
                reply[..2].copy_from_slice(&port.to_le_bytes());
                Ok(2)
            }
            (op::TCP_CONNECT, 0) => {
                if self.device.is_none() {
                    return Err(Status::NoDevice);
                }
                let bits = bits_at_end(data, 14)?;
                if received.len() != 1 {
                    return Err(Status::BadRequest);
                }
                let address: [u8; 4] = data[..4].try_into().expect("4 bytes");
                let port = u16::from_le_bytes([data[4], data[5]]);
                let socket = self.stack.tcp_connect(address, port, now).map_err(status)?;
                self.open_client(socket, received[0], bits, reply_handle, now)?;
                Ok(0)
            }
            (op::TCP_LISTEN, 0) => {
                if self.device.is_none() {
                    return Err(Status::NoDevice);
                }
                let bits = bits_at_end(data, 10)?;
                if received.len() != 1 {
                    return Err(Status::BadRequest);
                }
                let socket = self
                    .stack
                    .tcp_listen(u16::from_le_bytes([data[0], data[1]]))
                    .map_err(status)?;
                self.open_client(socket, received[0], bits, reply_handle, now)?;
                Ok(0)
            }
            (op::TCP_ACCEPT, badge) if badge != 0 => {
                let listener = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                let bits = bits_at_end(data, 8)?;
                if received.len() != 1 {
                    return Err(Status::BadRequest);
                }
                let socket = self
                    .stack
                    .tcp_accept(listener)
                    .map_err(status)?
                    .ok_or(Status::Empty)?;
                self.open_client(socket, received[0], bits, reply_handle, now)?;
                Ok(0)
            }
            (op::TCP_ATTACH, badge) if badge != 0 => {
                let client = self.clients.get_mut(&badge).ok_or(Status::BadRequest)?;
                if received.len() != 1 {
                    return Err(Status::BadRequest);
                }
                let size =
                    oceans_rt::memory_size(received[0]).map_err(|_| Status::BadRequest)? as usize;
                if !(MIN_STREAM_BUFFER..=MAX_STREAM_BUFFER).contains(&size) {
                    return Err(Status::BadRequest);
                }
                let base = oceans_rt::memory_map(received[0], 0, prot::READ | prot::WRITE)
                    .map_err(|_| Status::BadRequest)?;
                // The mapping keeps the memory; its handle is closed after
                // the reply.
                if let Some((old, _)) = client.buffer.replace((base, size)) {
                    let _ = oceans_rt::memory_unmap(old);
                }
                Ok(0)
            }
            (op::TCP_SEND_BUF | op::TCP_RECV_BUF, badge) if badge != 0 => {
                let client = self.clients.get(&badge).ok_or(Status::BadRequest)?;
                let (base, size) = client.buffer.ok_or(Status::BadRequest)?;
                let socket = client.socket;
                if data.len() != 8 {
                    return Err(Status::BadRequest);
                }
                let offset = u32::from_le_bytes(data[..4].try_into().expect("4 bytes")) as usize;
                let len = u32::from_le_bytes(data[4..].try_into().expect("4 bytes")) as usize;
                if offset.checked_add(len).is_none_or(|end| end > size) {
                    return Err(Status::BadRequest);
                }
                // SAFETY: `offset..offset + len` lies inside the client's
                // shared buffer (checked), mapped read-write here; the client
                // waits in this call while we use it.
                let window = unsafe { core::slice::from_raw_parts_mut(base.add(offset), len) };
                let moved = if label == op::TCP_SEND_BUF {
                    self.stack.tcp_send(socket, window, now).map_err(status)?
                } else {
                    match self.stack.tcp_recv(socket, window, now).map_err(status)? {
                        Recv::Data(len) => len,
                        Recv::WouldBlock => return Err(Status::Empty),
                        Recv::Eof => return Err(Status::Eof),
                    }
                };
                reply[..4].copy_from_slice(&(moved as u32).to_le_bytes());
                Ok(4)
            }
            (op::TCP_SEND, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                let sent = self.stack.tcp_send(socket, data, now).map_err(status)?;
                reply[..4].copy_from_slice(&(sent as u32).to_le_bytes());
                Ok(4)
            }
            (op::TCP_RECV, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                match self
                    .stack
                    .tcp_recv(socket, &mut reply[..MAX_STREAM], now)
                    .map_err(status)?
                {
                    Recv::Data(len) => Ok(len),
                    Recv::WouldBlock => Err(Status::Empty),
                    Recv::Eof => Err(Status::Eof),
                }
            }
            (op::TCP_SHUTDOWN, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                self.stack.tcp_shutdown(socket, now).map_err(status)?;
                Ok(0)
            }
            (op::TCP_STATUS, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                let (tcp_state, error) = self.stack.tcp_status(socket).ok_or(Status::BadRequest)?;
                reply[0] = state_code(tcp_state);
                reply[1] = error.map_or(0, |e| status(e.into()) as u8);
                Ok(2)
            }
            (op::SEND_TO, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                if data.len() < 6 {
                    return Err(Status::BadRequest);
                }
                let address: [u8; 4] = data[..4].try_into().expect("4 bytes");
                let port = u16::from_le_bytes([data[4], data[5]]);
                self.stack
                    .send_to(socket, address, port, &data[6..], now)
                    .map_err(status)?;
                Ok(0)
            }
            (op::RECV, badge) if badge != 0 => {
                let socket = self.clients.get(&badge).ok_or(Status::BadRequest)?.socket;
                // This (IPv4) form cannot name an IPv6 sender: such
                // datagrams are skipped (`RECV6` returns them).
                let (from, datagram) = loop {
                    let datagram = self.stack.recv(socket).ok_or(Status::Empty)?;
                    if let IpAddr::V4(from) = datagram.from {
                        break (from, datagram);
                    }
                };
                let take = datagram.data.len().min(MAX_DATA);
                reply[..4].copy_from_slice(&from);
                reply[4..6].copy_from_slice(&datagram.port.to_le_bytes());
                reply[6] = if take < datagram.data.len() {
                    TRUNCATED
                } else {
                    0
                };
                reply[7..7 + take].copy_from_slice(&datagram.data[..take]);
                Ok(7 + take)
            }
            _ => Err(Status::BadRequest),
        }
    }

    /// Hands out a badged handle for a new socket, signalled through the
    /// client's notification. On failure the socket is released.
    fn open_client(
        &mut self,
        socket: SocketId,
        notification: Handle,
        bits: u64,
        reply_handle: &mut Option<Handle>,
        now: u64,
    ) -> Result<(), Status> {
        let handle = match oceans_rt::endpoint_mint(self.server, self.next_badge) {
            Ok(handle) => handle,
            Err(_) => {
                self.stack.release(socket, now);
                return Err(Status::NoBuffers);
            }
        };
        self.clients.insert(
            self.next_badge,
            Client {
                socket,
                notification,
                bits,
                buffer: None,
            },
        );
        self.next_badge += 1;
        *reply_handle = Some(handle);
        Ok(())
    }
}
