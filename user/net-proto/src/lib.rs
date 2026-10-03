//! The Oceans network protocols (ADR-0023).
//!
//! - [`netdev`]: a network driver (`virtio-net`) to the stack (`net`).
//!   Ethernet frames move through a shared buffer; the driver signals the
//!   stack's notification when frames arrive, so neither side ever blocks
//!   on the other.
//! - Sockets ([`Socket`]): the stack to programs. A socket is a badged
//!   capability; the stack signals the program's notification when it
//!   becomes readable. Datagram payloads are inline, at most [`MAX_DATA`]
//!   bytes.
//!
//! Requests are IPC calls; replies carry a [`Status`] label.

#![no_std]

use oceans_rt::{Error, Handle, rights};

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
        let _ = oceans_rt::close(self.notification);
    }
}
