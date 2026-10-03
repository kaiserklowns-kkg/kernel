//! The Oceans network protocols (ADR-0023, ADR-0024).
//!
//! - [`netdev`]: a network driver (`virtio-net`) to the stack (`net`).
//!   Ethernet frames move through a shared buffer; the driver signals the
//!   stack's notification when frames arrive, so neither side ever blocks
//!   on the other.
//! - Sockets ([`Socket`]): the stack to programs. A socket is a badged
//!   capability; the stack signals the program's notification when it
//!   becomes readable. Datagram payloads are inline, at most [`MAX_DATA`]
//!   bytes.
//! - TCP ([`TcpStream`], [`TcpListener`]): the same model; the notification
//!   is signalled on every change (connected, data, space, end, error).
//! - DNS ([`resolve`]): a resolver over a UDP socket, run by the program
//!   itself (it blocks only its caller), using `oceans-dns`.
//!
//! Requests are IPC calls; replies carry a [`Status`] label.

#![no_std]

use oceans_rt::{Error, Handle, prot, rights};

pub type Ipv4 = [u8; 4];
pub type Mac = [u8; 6];

/// Largest datagram payload per call.
pub const MAX_DATA: usize = 240;

/// Reply status (reply label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    BadRequest = 1,
    /// No address yet (DHCP still running).
    NotConfigured = 2,
    AddressInUse = 3,
    NoRoute = 4,
    TooLarge = 5,
    /// Nothing to receive.
    Empty = 6,
    /// A queue or table is full.
    NoBuffers = 7,
    /// No network device.
    NoDevice = 8,
    /// TCP: nothing listens there.
    Refused = 9,
    /// TCP: the peer reset the connection.
    Reset = 10,
    /// TCP: the peer stopped answering.
    TimedOut = 11,
    /// TCP: not connected.
    NotConnected = 12,
    /// TCP: our sending side is shut down.
    Closed = 13,
    /// TCP: the peer closed its side; no more data.
    Eof = 14,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            2 => Self::NotConfigured,
            3 => Self::AddressInUse,
            4 => Self::NoRoute,
            5 => Self::TooLarge,
            6 => Self::Empty,
            7 => Self::NoBuffers,
            8 => Self::NoDevice,
            9 => Self::Refused,
            10 => Self::Reset,
            11 => Self::TimedOut,
            12 => Self::NotConnected,
            13 => Self::Closed,
            14 => Self::Eof,
            _ => Self::BadRequest,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::BadRequest => "bad request",
            Self::NotConfigured => "network not configured yet",
            Self::AddressInUse => "port in use",
            Self::NoRoute => "no route to host",
            Self::TooLarge => "too large",
            Self::Empty => "nothing received",
            Self::NoBuffers => "out of buffers",
            Self::NoDevice => "no network device",
            Self::Refused => "connection refused",
            Self::Reset => "connection reset by peer",
            Self::TimedOut => "timed out",
            Self::NotConnected => "not connected",
            Self::Closed => "connection closed",
            Self::Eof => "end of stream",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    Status(Status),
    Ipc(Error),
}

impl NetError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Status(status) => status.message(),
            Self::Ipc(Error::PeerClosed) => "network service unavailable",
            Self::Ipc(_) => "network request failed",
        }
    }
}

fn request(
    handle: Handle,
    op: u64,
    data: &[u8],
    send: &[Handle],
    reply: &mut [u8],
    reply_handles: &mut [Handle],
) -> Result<(usize, usize), NetError> {
    let got = oceans_rt::ipc_call_msg(handle, op, data, send, reply, reply_handles)
        .map_err(NetError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok((got.data_len, got.handles_len)),
        status => Err(NetError::Status(status)),
    }
}

/// The driver protocol.
pub mod netdev {
    /// Operations (request labels).
    pub mod op {
        /// → `[mac 6][mtu u16]`.
        pub const INFO: u64 = 1;
        /// data = `[bits u64]`, handles = the shared buffer (`READ`,
        /// `WRITE`, `MAP`, `TRANSFER`) and a notification (`SIGNAL`,
        /// `TRANSFER`) → a session handle. The driver signals `bits` when
        /// frames are waiting. One session at a time.
        pub const OPEN: u64 = 2;
        /// On the session: data = `[offset u32][len u16]`: sends the frame
        /// at that offset of the buffer's transmit area.
        pub const SEND: u64 = 3;
        /// On the session: fills the receive area with records `[len u16]
        /// [frame]` (each padded to an even length) → `[count u16]`.
        pub const RECV: u64 = 4;
    }

    /// Size of the shared buffer: the receive area, then the transmit area.
    pub const BUFFER_SIZE: usize = 64 * 1024;
    pub const RX_AREA: core::ops::Range<usize> = 0..32 * 1024;
    pub const TX_AREA: core::ops::Range<usize> = 32 * 1024..64 * 1024;
    /// Largest Ethernet frame (without FCS).
    pub const MAX_FRAME: usize = 1514;
}

/// Socket protocol operations (request labels).
pub mod op {
    /// On the stack's endpoint: → `[configured u8][address 4][prefix u8]
    /// [gateway 4][dns 4][mac 6]` (unset addresses are zeros).
    pub const INFO: u64 = 1;
    /// data = `[port u16][bits u64]`, handle = a notification (`SIGNAL`,
    /// `TRANSFER`) → a socket handle + `[port u16]`. Port 0: ephemeral.
    pub const UDP_OPEN: u64 = 2;
    /// data = `[bits u64]`, handle = a notification → an echo socket.
    pub const PING_OPEN: u64 = 3;
    /// On a socket: data = `[address 4][port u16][payload]`.
    pub const SEND_TO: u64 = 4;
    /// On a socket: → `[address 4][port u16][flags u8][payload]`, or
    /// `Empty`. Flag 1: the payload was cut to [`MAX_DATA`](super::MAX_DATA).
    pub const RECV: u64 = 5;
    /// data = `[address 4][port u16][bits u64]`, handle = a notification →
    /// a connection handle, connecting in the background.
    pub const TCP_CONNECT: u64 = 6;
    /// data = `[port u16][bits u64]`, handle = a notification → a listener.
    pub const TCP_LISTEN: u64 = 7;
    /// On a listener: data = `[bits u64]`, handle = a notification for the
    /// new connection → its handle, or `Empty`.
    pub const TCP_ACCEPT: u64 = 8;
    /// On a connection: data = bytes → `[accepted u32]` (0: buffer full).
    pub const TCP_SEND: u64 = 9;
    /// On a connection: → bytes (at most [`MAX_STREAM`](super::MAX_STREAM)),
    /// `Empty` (nothing yet) or `Eof`.
    pub const TCP_RECV: u64 = 10;
    /// On a connection: no more data from us (FIN after what is queued).
    pub const TCP_SHUTDOWN: u64 = 11;
    /// On a connection or listener: → `[state u8][error status u8]`.
    pub const TCP_STATUS: u64 = 12;
    /// On a connection (ADR-0030): handle = a memory object (`READ`,
    /// `WRITE`, `MAP`, `TRANSFER`) of 4 KiB to 1 MiB, the connection's
    /// shared buffer.
    pub const TCP_ATTACH: u64 = 13;
    /// On a connection: data = `[offset u32][len u32]`: sends those bytes
    /// of the shared buffer → `[accepted u32]` (0: full).
    pub const TCP_SEND_BUF: u64 = 14;
    /// On a connection: data = `[offset u32][capacity u32]`: receives into
    /// the shared buffer → `[len u32]`, `Empty` or `Eof`.
    pub const TCP_RECV_BUF: u64 = 15;
}

/// Shared buffer `TcpStream` attaches to each connection (ADR-0030).
pub const STREAM_BUFFER: usize = 64 * 1024;
pub const MIN_STREAM_BUFFER: usize = 4096;
pub const MAX_STREAM_BUFFER: usize = 1024 * 1024;

/// Largest TCP payload per send or receive call.
pub const MAX_STREAM: usize = 248;

/// TCP connection states, as `TCP_STATUS` reports them.
pub mod state {
    pub const CLOSED: u8 = 0;
    pub const LISTEN: u8 = 1;
    pub const SYN_SENT: u8 = 2;
    pub const SYN_RECEIVED: u8 = 3;
    pub const ESTABLISHED: u8 = 4;
    pub const FIN_WAIT_1: u8 = 5;
    pub const FIN_WAIT_2: u8 = 6;
    pub const CLOSE_WAIT: u8 = 7;
    pub const CLOSING: u8 = 8;
    pub const LAST_ACK: u8 = 9;
    pub const TIME_WAIT: u8 = 10;
}

/// `RECV` flags.
pub const TRUNCATED: u8 = 1;

/// The stack's configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetInfo {
    pub configured: bool,
    pub address: Ipv4,
    pub prefix: u8,
    pub gateway: Ipv4,
    pub dns: Ipv4,
    pub mac: Mac,
}

impl NetInfo {
    pub const SIZE: usize = 20;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0] = u8::from(self.configured);
        out[1..5].copy_from_slice(&self.address);
        out[5] = self.prefix;
        out[6..10].copy_from_slice(&self.gateway);
        out[10..14].copy_from_slice(&self.dns);
        out[14..20].copy_from_slice(&self.mac);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..Self::SIZE)?;
        Some(Self {
            configured: bytes[0] != 0,
            address: bytes[1..5].try_into().ok()?,
            prefix: bytes[5],
            gateway: bytes[6..10].try_into().ok()?,
            dns: bytes[10..14].try_into().ok()?,
            mac: bytes[14..20].try_into().ok()?,
        })
    }
}

/// Parses dotted-quad text (`10.0.2.2`).
pub fn parse_ipv4(text: &str) -> Option<Ipv4> {
    let mut address = [0u8; 4];
    let mut parts = text.split('.');
    for byte in &mut address {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *byte = part.parse().ok()?;
    }
    parts.next().is_none().then_some(address)
}

/// `a.b.c.d` for display.
pub struct Dotted(pub Ipv4);

impl core::fmt::Display for Dotted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

/// The configuration of the stack behind `net`.
pub fn info(net: Handle) -> Result<NetInfo, NetError> {
    let mut reply = [0u8; NetInfo::SIZE];
    let (len, _) = request(net, op::INFO, &[], &[], &mut reply, &mut [])?;
    NetInfo::decode(&reply[..len]).ok_or(NetError::Status(Status::BadRequest))
}

/// What a socket received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received {
    pub from: Ipv4,
    /// UDP: source port. Echo: sequence number.
    pub port: u16,
    pub len: usize,
    pub truncated: bool,
}

/// A socket, with the notification the stack signals when it is readable.
pub struct Socket {
    handle: Handle,
    notification: Handle,
    /// The notification is ours to close (not shared with other sockets).
    owned: bool,
}

/// Notification bit: the socket is readable.
pub const READABLE: u64 = 1;

impl Socket {
    fn open(net: Handle, op: u64, port: Option<u16>) -> Result<(Self, u16), NetError> {
        let ipc = NetError::Ipc;
        let notification = oceans_rt::notification_create().map_err(ipc)?;
        let shared = match oceans_rt::duplicate(notification, rights::SIGNAL | rights::TRANSFER) {
            Ok(shared) => shared,
            Err(error) => {
                let _ = oceans_rt::close(notification);
                return Err(ipc(error));
            }
        };
        let mut data = [0u8; 10];
        let len = match port {
            Some(port) => {
                data[..2].copy_from_slice(&port.to_le_bytes());
                data[2..].copy_from_slice(&READABLE.to_le_bytes());
                10
            }
            None => {
                data[..8].copy_from_slice(&READABLE.to_le_bytes());
                8
            }
        };
        let mut reply = [0u8; 2];
        let mut handles = [Handle(0); 1];
        match request(net, op, &data[..len], &[shared], &mut reply, &mut handles) {
            Ok((_, 1)) => Ok((
                Self {
                    handle: handles[0],
                    notification,
                    owned: true,
                },
                u16::from_le_bytes(reply),
            )),
            Ok(_) => {
                let _ = oceans_rt::close(notification);
                Err(NetError::Status(Status::BadRequest))
            }
            Err(error) => {
                let _ = oceans_rt::close(notification);
                Err(error)
            }
        }
    }

    /// A UDP socket on `port` (0: ephemeral); returns it and its port.
    pub fn udp(net: Handle, port: u16) -> Result<(Self, u16), NetError> {
        Self::open(net, op::UDP_OPEN, Some(port))
    }

    /// A UDP socket that signals `bits` on a notification the caller
    /// shares among several sockets (and keeps).
    pub fn udp_on(
        net: Handle,
        port: u16,
        notification: Handle,
        bits: u64,
    ) -> Result<Self, NetError> {
        let mut data = [0u8; 10];
        data[..2].copy_from_slice(&port.to_le_bytes());
        data[2..].copy_from_slice(&bits.to_le_bytes());
        let handle = open_shared(net, op::UDP_OPEN, &data, notification)?;
        Ok(Self {
            handle,
            notification,
            owned: false,
        })
    }

    /// An ICMP echo socket.
    pub fn ping(net: Handle) -> Result<Self, NetError> {
        Self::open(net, op::PING_OPEN, None).map(|(socket, _)| socket)
    }

    /// Our notification: wait on it, or set timers on it (bits other than
    /// [`READABLE`]).
    pub fn notification(&self) -> Handle {
        self.notification
    }

    pub fn send_to(&self, address: Ipv4, port: u16, payload: &[u8]) -> Result<(), NetError> {
        if payload.len() > MAX_DATA {
            return Err(NetError::Status(Status::TooLarge));
        }
        let mut data = [0u8; 6 + MAX_DATA];
        data[..4].copy_from_slice(&address);
        data[4..6].copy_from_slice(&port.to_le_bytes());
        data[6..6 + payload.len()].copy_from_slice(payload);
        request(
            self.handle,
            op::SEND_TO,
            &data[..6 + payload.len()],
            &[],
            &mut [],
            &mut [],
        )
        .map(drop)
    }

    /// The next received datagram, copied into `buffer`, or `None` if
    /// nothing is waiting.
    pub fn recv(&self, buffer: &mut [u8]) -> Result<Option<Received>, NetError> {
        let mut reply = [0u8; 7 + MAX_DATA];
        match request(self.handle, op::RECV, &[], &[], &mut reply, &mut []) {
            Ok((len, _)) if len >= 7 => {
                let payload = &reply[7..len];
                let take = payload.len().min(buffer.len());
                buffer[..take].copy_from_slice(&payload[..take]);
                Ok(Some(Received {
                    from: reply[..4].try_into().expect("4 bytes"),
                    port: u16::from_le_bytes([reply[4], reply[5]]),
                    len: take,
                    truncated: reply[6] & TRUNCATED != 0 || take < payload.len(),
                }))
            }
            Ok(_) => Err(NetError::Status(Status::BadRequest)),
            Err(NetError::Status(Status::Empty)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Blocks until the stack (or a timer) signals; returns the bits.
    pub fn wait(&self) -> Result<u64, NetError> {
        oceans_rt::notification_wait(self.notification).map_err(NetError::Ipc)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.handle);
        if self.owned {
            let _ = oceans_rt::close(self.notification);
        }
    }
}

/// Notification bit for timeouts set by these clients.
const TIMEOUT: u64 = 1 << 1;

/// A notification for a new socket: ours (to wait on) and a `SIGNAL` copy
/// to hand to the stack.
fn new_notification() -> Result<(Handle, Handle), NetError> {
    let notification = oceans_rt::notification_create().map_err(NetError::Ipc)?;
    match oceans_rt::duplicate(notification, rights::SIGNAL | rights::TRANSFER) {
        Ok(shared) => Ok((notification, shared)),
        Err(error) => {
            let _ = oceans_rt::close(notification);
            Err(NetError::Ipc(error))
        }
    }
}

/// Opens a socket-like object signalling a caller's notification (shared,
/// not owned by the result).
fn open_shared(
    handle: Handle,
    op: u64,
    data: &[u8],
    notification: Handle,
) -> Result<Handle, NetError> {
    let shared = oceans_rt::duplicate(notification, rights::SIGNAL | rights::TRANSFER)
        .map_err(NetError::Ipc)?;
    let mut opened = [Handle(0); 1];
    // Room for replies with data (a UDP open returns its port).
    let mut reply = [0u8; 8];
    match request(handle, op, data, &[shared], &mut reply, &mut opened) {
        Ok((_, 1)) => Ok(opened[0]),
        Ok(_) => Err(NetError::Status(Status::BadRequest)),
        Err(error) => Err(error),
    }
}

/// Opens a socket-like object: `op` with `data` and a notification.
fn open_with_notification(
    handle: Handle,
    op: u64,
    data: &[u8],
) -> Result<(Handle, Handle), NetError> {
    let (notification, shared) = new_notification()?;
    let mut opened = [Handle(0); 1];
    // Room for replies with data (a UDP open returns its port).
    let mut reply = [0u8; 8];
    match request(handle, op, data, &[shared], &mut reply, &mut opened) {
        Ok((_, 1)) => Ok((opened[0], notification)),
        result => {
            let _ = oceans_rt::close(notification);
            Err(result.err().unwrap_or(NetError::Status(Status::BadRequest)))
        }
    }
}

/// Waits on `notification` until `done` says yes, or `timeout_ms` passes.
fn wait_until<T>(
    notification: Handle,
    timeout_ms: u64,
    mut done: impl FnMut() -> Result<Option<T>, NetError>,
) -> Result<T, NetError> {
    let _ = oceans_rt::timer_set(notification, TIMEOUT, timeout_ms);
    let result = loop {
        match done() {
            Ok(Some(value)) => break Ok(value),
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        match oceans_rt::notification_wait(notification) {
            Ok(bits) if bits & TIMEOUT != 0 => {
                // One last look: the event may have come with the timeout.
                break match done() {
                    Ok(Some(value)) => Ok(value),
                    Ok(None) => Err(NetError::Status(Status::TimedOut)),
                    Err(error) => Err(error),
                };
            }
            Ok(_) => {}
            Err(error) => break Err(NetError::Ipc(error)),
        }
    };
    let _ = oceans_rt::timer_set(notification, TIMEOUT, 0);
    result
}

/// What a stream read returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    Data(usize),
    /// Nothing yet: wait for the notification.
    WouldBlock,
    /// The peer finished sending.
    Eof,
}

/// A TCP connection. It moves data through a shared buffer attached at
/// creation (ADR-0030), or inline [`MAX_STREAM`] bytes at a time if the
/// buffer could not be set up.
pub struct TcpStream {
    handle: Handle,
    notification: Handle,
    owned: bool,
    /// The shared buffer, mapped here.
    shared: Option<(*mut u8, usize)>,
}

impl TcpStream {
    fn new(handle: Handle, notification: Handle, owned: bool) -> Self {
        let mut stream = Self {
            handle,
            notification,
            owned,
            shared: None,
        };
        stream.shared = stream.attach(STREAM_BUFFER).ok();
        stream
    }

    /// Gives the connection a shared buffer of `size` bytes.
    fn attach(&self, size: usize) -> Result<(*mut u8, usize), NetError> {
        let ipc = NetError::Ipc;
        let memory = oceans_rt::memory_create(size as u64).map_err(ipc)?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let shared = oceans_rt::duplicate(
            memory,
            rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
        );
        let _ = oceans_rt::close(memory);
        let base = base.map_err(ipc)?;
        let result = shared.map_err(ipc).and_then(|shared| {
            request(
                self.handle,
                op::TCP_ATTACH,
                &[],
                &[shared],
                &mut [],
                &mut [],
            )
        });
        match result {
            Ok(_) => Ok((base, size)),
            Err(error) => {
                let _ = oceans_rt::memory_unmap(base);
                Err(error)
            }
        }
    }

    /// Starts connecting to `address:port`; see [`wait_connected`](Self::wait_connected).
    pub fn connect(net: Handle, address: Ipv4, port: u16) -> Result<Self, NetError> {
        let mut data = [0u8; 14];
        data[..4].copy_from_slice(&address);
        data[4..6].copy_from_slice(&port.to_le_bytes());
        data[6..].copy_from_slice(&READABLE.to_le_bytes());
        let (handle, notification) = open_with_notification(net, op::TCP_CONNECT, &data)?;
        Ok(Self::new(handle, notification, true))
    }

    /// The notification signalled on every change; also usable for timers
    /// (bits other than [`READABLE`] and bit 1).
    pub fn notification(&self) -> Handle {
        self.notification
    }

    /// `(state, error)`: see [`state`].
    pub fn status(&self) -> Result<(u8, Option<Status>), NetError> {
        let mut reply = [0u8; 2];
        request(self.handle, op::TCP_STATUS, &[], &[], &mut reply, &mut [])?;
        let error = (reply[1] != 0).then(|| Status::from_label(u64::from(reply[1])));
        Ok((reply[0], error))
    }

    /// Waits until the connection is up, or fails.
    pub fn wait_connected(&self, timeout_ms: u64) -> Result<(), NetError> {
        wait_until(self.notification, timeout_ms, || match self.status()? {
            (_, Some(error)) => Err(NetError::Status(error)),
            (state::ESTABLISHED | state::CLOSE_WAIT, _) => Ok(Some(())),
            (state::SYN_SENT | state::SYN_RECEIVED, _) => Ok(None),
            _ => Err(NetError::Status(Status::NotConnected)),
        })
    }

    /// Queues bytes (as many as the shared buffer holds); returns how many
    /// (0: the connection's send buffer is full).
    pub fn send(&self, data: &[u8]) -> Result<usize, NetError> {
        if let Some((base, size)) = self.shared {
            let take = data.len().min(size);
            // SAFETY: `base` maps `size` bytes read-write for our lifetime;
            // the service reads them only during the call below.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), base, take) };
            let mut request_data = [0u8; 8];
            request_data[4..].copy_from_slice(&(take as u32).to_le_bytes());
            let mut reply = [0u8; 4];
            request(
                self.handle,
                op::TCP_SEND_BUF,
                &request_data,
                &[],
                &mut reply,
                &mut [],
            )?;
            return Ok(u32::from_le_bytes(reply) as usize);
        }
        let take = data.len().min(MAX_STREAM);
        let mut reply = [0u8; 4];
        request(
            self.handle,
            op::TCP_SEND,
            &data[..take],
            &[],
            &mut reply,
            &mut [],
        )?;
        Ok(u32::from_le_bytes(reply) as usize)
    }

    /// Sends all of `data`, waiting while the buffer is full.
    pub fn send_all(&self, mut data: &[u8], timeout_ms: u64) -> Result<(), NetError> {
        while !data.is_empty() {
            let sent = wait_until(self.notification, timeout_ms, || {
                self.send(data).map(|n| (n > 0).then_some(n))
            })?;
            data = &data[sent..];
        }
        Ok(())
    }

    /// Copies received bytes into `buffer`.
    pub fn read(&self, buffer: &mut [u8]) -> Result<Read, NetError> {
        if let Some((base, size)) = self.shared {
            let capacity = buffer.len().min(size);
            let mut request_data = [0u8; 8];
            request_data[4..].copy_from_slice(&(capacity as u32).to_le_bytes());
            let mut reply = [0u8; 4];
            return match request(
                self.handle,
                op::TCP_RECV_BUF,
                &request_data,
                &[],
                &mut reply,
                &mut [],
            ) {
                Ok(_) => {
                    let len = (u32::from_le_bytes(reply) as usize).min(capacity);
                    // SAFETY: the service wrote `len` bytes at the start of
                    // the shared buffer during the call.
                    unsafe { core::ptr::copy_nonoverlapping(base, buffer.as_mut_ptr(), len) };
                    Ok(Read::Data(len))
                }
                Err(NetError::Status(Status::Empty)) => Ok(Read::WouldBlock),
                Err(NetError::Status(Status::Eof)) => Ok(Read::Eof),
                Err(error) => Err(error),
            };
        }
        let mut reply = [0u8; MAX_STREAM];
        match request(self.handle, op::TCP_RECV, &[], &[], &mut reply, &mut []) {
            Ok((len, _)) => {
                let take = len.min(buffer.len());
                buffer[..take].copy_from_slice(&reply[..take]);
                Ok(Read::Data(take))
            }
            Err(NetError::Status(Status::Empty)) => Ok(Read::WouldBlock),
            Err(NetError::Status(Status::Eof)) => Ok(Read::Eof),
            Err(error) => Err(error),
        }
    }

    /// Reads, waiting up to `timeout_ms` for data or the end of the stream.
    pub fn read_wait(&self, buffer: &mut [u8], timeout_ms: u64) -> Result<Read, NetError> {
        wait_until(self.notification, timeout_ms, || {
            match self.read(buffer)? {
                Read::WouldBlock => Ok(None),
                other => Ok(Some(other)),
            }
        })
    }

    /// No more data from us; the peer reads end of stream.
    pub fn shutdown(&self) -> Result<(), NetError> {
        request(self.handle, op::TCP_SHUTDOWN, &[], &[], &mut [], &mut []).map(drop)
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.handle);
        if let Some((base, _)) = self.shared {
            let _ = oceans_rt::memory_unmap(base);
        }
        if self.owned {
            let _ = oceans_rt::close(self.notification);
        }
    }
}

/// A TCP listener.
pub struct TcpListener {
    handle: Handle,
    notification: Handle,
    owned: bool,
}

impl TcpListener {
    pub fn listen(net: Handle, port: u16) -> Result<Self, NetError> {
        let mut data = [0u8; 10];
        data[..2].copy_from_slice(&port.to_le_bytes());
        data[2..].copy_from_slice(&READABLE.to_le_bytes());
        let (handle, notification) = open_with_notification(net, op::TCP_LISTEN, &data)?;
        Ok(Self {
            handle,
            notification,
            owned: true,
        })
    }

    /// A listener signalling `bits` on a shared notification.
    pub fn listen_on(
        net: Handle,
        port: u16,
        notification: Handle,
        bits: u64,
    ) -> Result<Self, NetError> {
        let mut data = [0u8; 10];
        data[..2].copy_from_slice(&port.to_le_bytes());
        data[2..].copy_from_slice(&bits.to_le_bytes());
        let handle = open_shared(net, op::TCP_LISTEN, &data, notification)?;
        Ok(Self {
            handle,
            notification,
            owned: false,
        })
    }

    /// Like [`accept`](Self::accept), the connection signalling `bits` on a
    /// shared notification.
    pub fn accept_on(
        &self,
        notification: Handle,
        bits: u64,
    ) -> Result<Option<TcpStream>, NetError> {
        match open_shared(
            self.handle,
            op::TCP_ACCEPT,
            &bits.to_le_bytes(),
            notification,
        ) {
            Ok(handle) => Ok(Some(TcpStream::new(handle, notification, false))),
            Err(NetError::Status(Status::Empty)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn notification(&self) -> Handle {
        self.notification
    }

    /// An established connection, if one is waiting.
    pub fn accept(&self) -> Result<Option<TcpStream>, NetError> {
        match open_with_notification(self.handle, op::TCP_ACCEPT, &READABLE.to_le_bytes()) {
            Ok((handle, notification)) => Ok(Some(TcpStream::new(handle, notification, true))),
            Err(NetError::Status(Status::Empty)) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.handle);
        if self.owned {
            let _ = oceans_rt::close(self.notification);
        }
    }
}

/// Why a name did not resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveError {
    BadName,
    /// No DNS server is configured.
    NoServer,
    NotFound,
    NoAddress,
    /// The server failed.
    Server,
    Timeout,
    Net(NetError),
}

impl ResolveError {
    pub fn message(self) -> &'static str {
        match self {
            Self::BadName => "not a valid host name",
            Self::NoServer => "no DNS server configured",
            Self::NotFound => "not found",
            Self::NoAddress => "has no IPv4 address",
            Self::Server => "DNS server failure",
            Self::Timeout => "DNS server did not answer",
            Self::Net(error) => error.message(),
        }
    }
}

const DNS_TRIES: u32 = 3;
const DNS_TIMEOUT_MS: u64 = 1_500;

/// The address of `name` (a dotted quad is returned as is), asking the
/// configured DNS server.
pub fn resolve(net: Handle, name: &str) -> Result<Ipv4, ResolveError> {
    if let Some(address) = parse_ipv4(name) {
        return Ok(address);
    }
    let info = info(net).map_err(ResolveError::Net)?;
    if !info.configured || info.dns == [0; 4] {
        return Err(ResolveError::NoServer);
    }
    resolve_via(net, name, info.dns, oceans_dns::PORT)
}

/// Like [`resolve`], asking `server:port`.
pub fn resolve_via(net: Handle, name: &str, server: Ipv4, port: u16) -> Result<Ipv4, ResolveError> {
    use oceans_dns::{Answer, build_query, parse_response};

    let mut query = [0u8; oceans_dns::MAX_MESSAGE];
    // Unpredictable ids make forged answers much harder (ADR-0026).
    let id = oceans_rt::random_u64() as u16;
    let len = build_query(id, name, &mut query).map_err(|_| ResolveError::BadName)?;
    if len > MAX_DATA {
        return Err(ResolveError::BadName);
    }
    let (socket, _) = Socket::udp(net, 0).map_err(ResolveError::Net)?;
    for _ in 0..DNS_TRIES {
        socket
            .send_to(server, port, &query[..len])
            .map_err(ResolveError::Net)?;
        let answer = wait_until(socket.notification(), DNS_TIMEOUT_MS, || {
            let mut response = [0u8; MAX_DATA];
            while let Some(received) = socket.recv(&mut response)? {
                // Only the server's answer to this query counts.
                if received.from != server || received.port != port {
                    continue;
                }
                if let Ok(answer) = parse_response(id, name, &response[..received.len]) {
                    return Ok(Some(answer));
                }
            }
            Ok(None)
        });
        match answer {
            Ok(Answer::Address(address, _)) => return Ok(address),
            Ok(Answer::NotFound) => return Err(ResolveError::NotFound),
            Ok(Answer::NoAddress) => return Err(ResolveError::NoAddress),
            Ok(Answer::ServerError(_)) => return Err(ResolveError::Server),
            Err(NetError::Status(Status::TimedOut)) => continue,
            Err(error) => return Err(ResolveError::Net(error)),
        }
    }
    Err(ResolveError::Timeout)
}
