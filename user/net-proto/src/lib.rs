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
//! - IPv6 (ADR-0043): the operations above that carry addresses have
//!   IPv6-capable versions (`SEND_TO6`, `RECV6`, `TCP_CONNECT6`, `INFO6`)
//!   carrying 16-byte addresses, IPv4 as IPv4-mapped (`::ffff:a.b.c.d`).
//!   The original operations are unchanged, so IPv4-only programs keep
//!   working. Clients: [`Socket::send_to_ip`], [`Socket::recv_ip`],
//!   [`TcpStream::connect_ip`], [`info6`], and name resolution choosing
//!   between families ([`resolve_ip`], [`lookup`], [`connect_host`]).
//!
//! Requests are IPC calls; replies carry a [`Status`] label.

#![no_std]

use oceans_rt::{Error, Handle, prot, rights};

pub use oceans_inet::{
    Colons, Dotted, IpAddr, Ipv4, Ipv6, parse_ip, parse_ipv4, parse_ipv6, split_host_port,
};

pub type Mac = [u8; 6];

/// Largest datagram payload per call.
pub const MAX_DATA: usize = 240;
/// Largest datagram payload per `SEND_TO6` or `RECV6` call (the 16-byte
/// address leaves less room in a message).
pub const MAX_DATA6: usize = 236;

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
    /// On a socket (ADR-0043): data = `[address 16][port u16][payload]`,
    /// the address IPv6 or IPv4-mapped; payload at most
    /// [`MAX_DATA6`](super::MAX_DATA6).
    pub const SEND_TO6: u64 = 16;
    /// On a socket: → `[address 16][port u16][flags u8][payload]` (IPv4
    /// senders IPv4-mapped), or `Empty`. Flag 1: the payload was cut to
    /// [`MAX_DATA6`](super::MAX_DATA6). (`RECV` skips datagrams from IPv6
    /// senders, which it cannot express.)
    pub const RECV6: u64 = 17;
    /// data = `[address 16][port u16][bits u64]`, handle = a notification
    /// → a connection handle, connecting in the background.
    pub const TCP_CONNECT6: u64 = 18;
    /// On the stack's endpoint: → the IPv6 configuration, see
    /// [`NetInfo6`](super::NetInfo6).
    pub const INFO6: u64 = 19;
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

/// Address states in [`NetInfo6`].
pub mod address_state {
    pub const TENTATIVE: u8 = 0;
    pub const PREFERRED: u8 = 1;
    pub const DEPRECATED: u8 = 2;
    pub const DUPLICATE: u8 = 3;
}

/// One IPv6 address of the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address6 {
    pub address: Ipv6,
    pub prefix: u8,
    /// See [`address_state`].
    pub state: u8,
    /// Link-local (else from a router's prefix, SLAAC).
    pub link_local: bool,
}

impl Address6 {
    const SIZE: usize = 19;

    /// Usable for traffic (preferred or deprecated).
    pub fn usable(&self) -> bool {
        matches!(
            self.state,
            address_state::PREFERRED | address_state::DEPRECATED
        )
    }
}

/// The stack's IPv6 configuration (`INFO6`): `[flags u8][hop limit u8]
/// [mtu u16][count u8]`, `count` × `[address 16][prefix u8][state u8]
/// [link-local u8]`, then `[router 16][dns 16]` (zeros when unset). Flag
/// 1: IPv6 is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetInfo6 {
    pub enabled: bool,
    pub hop_limit: u8,
    pub mtu: u16,
    addresses: [Address6; Self::MAX_ADDRESSES],
    count: usize,
    pub router: Option<Ipv6>,
    pub dns: Option<Ipv6>,
}

impl NetInfo6 {
    pub const MAX_ADDRESSES: usize = 8;
    pub const MAX_SIZE: usize = 5 + Self::MAX_ADDRESSES * Address6::SIZE + 32;
    const NONE: Address6 = Address6 {
        address: [0; 16],
        prefix: 0,
        state: 0,
        link_local: false,
    };

    /// No IPv6.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            hop_limit: 0,
            mtu: 0,
            addresses: [Self::NONE; Self::MAX_ADDRESSES],
            count: 0,
            router: None,
            dns: None,
        }
    }

    pub fn addresses(&self) -> &[Address6] {
        &self.addresses[..self.count]
    }

    /// Adds an address (beyond [`MAX_ADDRESSES`](Self::MAX_ADDRESSES),
    /// returns false).
    pub fn push(&mut self, address: Address6) -> bool {
        if self.count >= Self::MAX_ADDRESSES {
            return false;
        }
        self.addresses[self.count] = address;
        self.count += 1;
        true
    }

    /// Whether IPv6 can reach beyond the link: a usable address that is
    /// not link-local.
    pub fn global(&self) -> bool {
        self.addresses().iter().any(|a| a.usable() && !a.link_local)
    }

    /// Writes the encoding; returns its length.
    pub fn encode(&self, out: &mut [u8; Self::MAX_SIZE]) -> usize {
        out[0] = u8::from(self.enabled);
        out[1] = self.hop_limit;
        out[2..4].copy_from_slice(&self.mtu.to_le_bytes());
        out[4] = self.count as u8;
        let mut at = 5;
        for address in self.addresses() {
            out[at..at + 16].copy_from_slice(&address.address);
            out[at + 16] = address.prefix;
            out[at + 17] = address.state;
            out[at + 18] = u8::from(address.link_local);
            at += Address6::SIZE;
        }
        out[at..at + 16].copy_from_slice(&self.router.unwrap_or([0; 16]));
        out[at + 16..at + 32].copy_from_slice(&self.dns.unwrap_or([0; 16]));
        at + 32
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut info = Self::disabled();
        info.enabled = *bytes.first()? & 1 != 0;
        info.hop_limit = *bytes.get(1)?;
        info.mtu = u16::from_le_bytes(bytes.get(2..4)?.try_into().ok()?);
        let count = usize::from(*bytes.get(4)?);
        if count > Self::MAX_ADDRESSES {
            return None;
        }
        let mut at = 5;
        for _ in 0..count {
            let record = bytes.get(at..at + Address6::SIZE)?;
            info.push(Address6 {
                address: record[..16].try_into().ok()?,
                prefix: record[16],
                state: record[17],
                link_local: record[18] != 0,
            });
            at += Address6::SIZE;
        }
        let set = |address: Ipv6| (address != [0; 16]).then_some(address);
        info.router = set(bytes.get(at..at + 16)?.try_into().ok()?);
        info.dns = set(bytes.get(at + 16..at + 32)?.try_into().ok()?);
        Some(info)
    }
}

/// The IPv6 configuration of the stack behind `net`.
pub fn info6(net: Handle) -> Result<NetInfo6, NetError> {
    let mut reply = [0u8; NetInfo6::MAX_SIZE];
    let (len, _) = request(net, op::INFO6, &[], &[], &mut reply, &mut [])?;
    NetInfo6::decode(&reply[..len]).ok_or(NetError::Status(Status::BadRequest))
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

/// What a socket received, from either family ([`Socket::recv_ip`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceivedFrom {
    pub from: IpAddr,
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

    /// Sends to an address of either family (UDP: to `port`; echo: `port`
    /// ignored). IPv6 payloads are limited to [`MAX_DATA6`].
    pub fn send_to_ip(&self, address: IpAddr, port: u16, payload: &[u8]) -> Result<(), NetError> {
        let address = match address {
            IpAddr::V4(address) => return self.send_to(address, port, payload),
            IpAddr::V6(address) => address,
        };
        if payload.len() > MAX_DATA6 {
            return Err(NetError::Status(Status::TooLarge));
        }
        let mut data = [0u8; 18 + MAX_DATA6];
        data[..16].copy_from_slice(&address);
        data[16..18].copy_from_slice(&port.to_le_bytes());
        data[18..18 + payload.len()].copy_from_slice(payload);
        request(
            self.handle,
            op::SEND_TO6,
            &data[..18 + payload.len()],
            &[],
            &mut [],
            &mut [],
        )
        .map(drop)
    }

    /// The next received datagram from either family, copied into
    /// `buffer`, or `None` if nothing is waiting.
    pub fn recv_ip(&self, buffer: &mut [u8]) -> Result<Option<ReceivedFrom>, NetError> {
        let mut reply = [0u8; 19 + MAX_DATA6];
        match request(self.handle, op::RECV6, &[], &[], &mut reply, &mut []) {
            Ok((len, _)) if len >= 19 => {
                let payload = &reply[19..len];
                let take = payload.len().min(buffer.len());
                buffer[..take].copy_from_slice(&payload[..take]);
                let address: Ipv6 = reply[..16].try_into().expect("16 bytes");
                Ok(Some(ReceivedFrom {
                    from: IpAddr::from_mapped(address),
                    port: u16::from_le_bytes([reply[16], reply[17]]),
                    len: take,
                    truncated: reply[18] & TRUNCATED != 0 || take < payload.len(),
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

    /// The `TCP_CONNECT6` request for `address:port`.
    fn connect6_request(address: Ipv6, port: u16, bits: u64) -> [u8; 26] {
        let mut data = [0u8; 26];
        data[..16].copy_from_slice(&address);
        data[16..18].copy_from_slice(&port.to_le_bytes());
        data[18..].copy_from_slice(&bits.to_le_bytes());
        data
    }

    /// Starts connecting to `address:port`, either family; see
    /// [`wait_connected`](Self::wait_connected).
    pub fn connect_ip(net: Handle, address: IpAddr, port: u16) -> Result<Self, NetError> {
        let address = match address {
            IpAddr::V4(address) => return Self::connect(net, address, port),
            IpAddr::V6(address) => address,
        };
        let data = Self::connect6_request(address, port, READABLE);
        let (handle, notification) = open_with_notification(net, op::TCP_CONNECT6, &data)?;
        Ok(Self::new(handle, notification, true))
    }

    /// Like [`connect_ip`](Self::connect_ip), signalling `bits` on a
    /// caller's notification (shared, not owned by the result).
    pub fn connect_ip_on(
        net: Handle,
        address: IpAddr,
        port: u16,
        notification: Handle,
        bits: u64,
    ) -> Result<Self, NetError> {
        let handle = match address {
            IpAddr::V4(address) => {
                let mut data = [0u8; 14];
                data[..4].copy_from_slice(&address);
                data[4..6].copy_from_slice(&port.to_le_bytes());
                data[6..].copy_from_slice(&bits.to_le_bytes());
                open_shared(net, op::TCP_CONNECT, &data, notification)?
            }
            IpAddr::V6(address) => {
                let data = Self::connect6_request(address, port, bits);
                open_shared(net, op::TCP_CONNECT6, &data, notification)?
            }
        };
        Ok(Self::new(handle, notification, false))
    }

    /// Where the connection stands: `Some(Ok)` up, `Some(Err)` failed,
    /// `None` still connecting.
    fn progress(&self) -> Option<Result<(), NetError>> {
        match self.status() {
            Err(error) => Some(Err(error)),
            Ok((_, Some(error))) => Some(Err(NetError::Status(error))),
            Ok((state::ESTABLISHED | state::CLOSE_WAIT, _)) => Some(Ok(())),
            Ok((state::SYN_SENT | state::SYN_RECEIVED, _)) => None,
            Ok(_) => Some(Err(NetError::Status(Status::NotConnected))),
        }
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
        wait_until(self.notification, timeout_ms, || {
            self.progress().transpose()
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

/// Why a name did not resolve (or, for [`connect_host`], connect).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveError {
    BadName,
    /// No DNS server is configured.
    NoServer,
    NotFound,
    /// The name exists but has no address of a usable kind.
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
            Self::NoAddress => "has no address",
            Self::Server => "DNS server failure",
            Self::Timeout => "DNS server did not answer",
            Self::Net(error) => error.message(),
        }
    }
}

const DNS_TRIES: u32 = 3;
const DNS_TIMEOUT_MS: u64 = 1_500;

/// What the stack can use now, and the DNS server it was given (DHCP's
/// first, else a router's RDNSS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reach {
    pub v4: bool,
    /// A usable IPv6 address beyond the link.
    pub v6: bool,
    pub dns: Option<IpAddr>,
}

/// Asks the stack what it can reach.
pub fn reach(net: Handle) -> Result<Reach, NetError> {
    let info = info(net)?;
    // A stack without IPv6 support answers INFO6 with BadRequest.
    let info6 = match info6(net) {
        Ok(info6) => info6,
        Err(NetError::Status(Status::BadRequest)) => NetInfo6::disabled(),
        Err(error) => return Err(error),
    };
    let dns4 = (info.configured && info.dns != [0; 4]).then_some(IpAddr::V4(info.dns));
    Ok(Reach {
        v4: info.configured,
        v6: info6.global(),
        dns: dns4.or(info6.dns.map(IpAddr::V6)),
    })
}

/// The addresses a name has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Addresses {
    pub v4: Option<Ipv4>,
    pub v6: Option<Ipv6>,
}

impl Addresses {
    /// The addresses in the order to try (RFC 6724 §6, reduced to what one
    /// interface needs): IPv6 first when the stack has a global IPv6
    /// address, IPv4 first otherwise; a family the stack cannot use last.
    pub fn ordered(&self, reach: Reach) -> [Option<IpAddr>; 2] {
        let v4 = self.v4.map(IpAddr::V4);
        let v6 = self.v6.map(IpAddr::V6);
        let [first, second] = if reach.v6 { [v6, v4] } else { [v4, v6] };
        match first {
            Some(_) => [first, second],
            None => [second, None],
        }
    }
}

/// One query for `name`'s records of type `R`, retried on silence.
fn query<R: oceans_dns::Record>(
    socket: &Socket,
    server: IpAddr,
    port: u16,
    name: &str,
) -> Result<oceans_dns::Answer<R>, ResolveError> {
    use oceans_dns::{build_query_for, parse_response_for};

    let mut query = [0u8; oceans_dns::MAX_MESSAGE];
    // Unpredictable ids make forged answers much harder (ADR-0026).
    let id = oceans_rt::random_u64() as u16;
    let len = build_query_for::<R>(id, name, &mut query).map_err(|_| ResolveError::BadName)?;
    if len > MAX_DATA6 {
        return Err(ResolveError::BadName);
    }
    for _ in 0..DNS_TRIES {
        socket
            .send_to_ip(server, port, &query[..len])
            .map_err(ResolveError::Net)?;
        let answer = wait_until(socket.notification(), DNS_TIMEOUT_MS, || {
            let mut response = [0u8; MAX_DATA6];
            while let Some(received) = socket.recv_ip(&mut response)? {
                // Only the server's answer to this query counts.
                if received.from != server || received.port != port {
                    continue;
                }
                if let Ok(answer) = parse_response_for::<R>(id, name, &response[..received.len]) {
                    return Ok(Some(answer));
                }
            }
            Ok(None)
        });
        match answer {
            Ok(answer) => return Ok(answer),
            Err(NetError::Status(Status::TimedOut)) => continue,
            Err(error) => return Err(ResolveError::Net(error)),
        }
    }
    Err(ResolveError::Timeout)
}

/// What one record type said: an address, or why not.
fn record<R>(answer: oceans_dns::Answer<R>) -> Result<Option<R>, ResolveError> {
    use oceans_dns::Answer;
    match answer {
        Answer::Address(address, _) => Ok(Some(address)),
        Answer::NotFound => Err(ResolveError::NotFound),
        Answer::NoAddress => Ok(None),
        Answer::ServerError(_) => Err(ResolveError::Server),
    }
}

/// `name`'s IPv4 and IPv6 addresses, asking `server:port`: A and AAAA
/// records as `want` says (`(A, AAAA)`). A failed query does not hide
/// what the other found.
pub fn lookup_via(
    net: Handle,
    name: &str,
    server: IpAddr,
    port: u16,
    (want_a, want_aaaa): (bool, bool),
) -> Result<Addresses, ResolveError> {
    if !oceans_dns::valid_name(name) {
        return Err(ResolveError::BadName);
    }
    let (socket, _) = Socket::udp(net, 0).map_err(ResolveError::Net)?;
    let a = want_a.then(|| query::<Ipv4>(&socket, server, port, name).and_then(record));
    let aaaa = want_aaaa.then(|| query::<Ipv6>(&socket, server, port, name).and_then(record));
    let found = Addresses {
        v4: a.and_then(|r| r.ok()).flatten(),
        v6: aaaa.and_then(|r| r.ok()).flatten(),
    };
    if found.v4.is_some() || found.v6.is_some() {
        return Ok(found);
    }
    // Nothing: the first failure explains it, else there is no address.
    match (a, aaaa) {
        (Some(Err(error)), _) | (_, Some(Err(error))) => Err(error),
        _ => Err(ResolveError::NoAddress),
    }
}

/// `name`'s addresses from the configured DNS server: both kinds.
pub fn lookup(net: Handle, name: &str) -> Result<Addresses, ResolveError> {
    let reach = reach(net).map_err(ResolveError::Net)?;
    let server = reach.dns.ok_or(ResolveError::NoServer)?;
    lookup_via(net, name, server, oceans_dns::PORT, (true, true))
}

/// The address of `name` to use (an address literal is returned as is):
/// asks for the record kinds the stack can use, and picks as
/// [`Addresses::ordered`] does.
pub fn resolve_ip(net: Handle, name: &str) -> Result<IpAddr, ResolveError> {
    if let Some(address) = parse_ip(name) {
        return Ok(address);
    }
    let reach = reach(net).map_err(ResolveError::Net)?;
    let server = reach.dns.ok_or(ResolveError::NoServer)?;
    let want = (reach.v4 || !reach.v6, reach.v6);
    let addresses = lookup_via(net, name, server, oceans_dns::PORT, want)?;
    addresses.ordered(reach)[0].ok_or(ResolveError::NoAddress)
}

/// The IPv4 address of `name` (a dotted quad is returned as is), asking
/// the configured DNS server.
pub fn resolve(net: Handle, name: &str) -> Result<Ipv4, ResolveError> {
    if let Some(address) = parse_ipv4(name) {
        return Ok(address);
    }
    let reach = reach(net).map_err(ResolveError::Net)?;
    let server = reach.dns.ok_or(ResolveError::NoServer)?;
    let found = lookup_via(net, name, server, oceans_dns::PORT, (true, false))?;
    found.v4.ok_or(ResolveError::NoAddress)
}

/// Like [`resolve`], asking `server:port`.
pub fn resolve_via(net: Handle, name: &str, server: Ipv4, port: u16) -> Result<Ipv4, ResolveError> {
    let found = lookup_via(net, name, IpAddr::V4(server), port, (true, false))?;
    found.v4.ok_or(ResolveError::NoAddress)
}

/// How long the preferred address gets before the other one is tried too
/// (RFC 8305's connection attempt delay).
const ATTEMPT_DELAY_MS: u64 = 250;
/// Notification bit for that delay.
const ATTEMPT_TIMER: u64 = 1 << 2;

/// Connects to `host:port`: an address literal, or a name whose
/// addresses (both families) race as RFC 8305 ("Happy Eyeballs")
/// describes: the preferred one first, the other after
/// [`ATTEMPT_DELAY_MS`] or as soon as the first fails. The first
/// connection up wins.
pub fn connect_host(
    net: Handle,
    host: &str,
    port: u16,
    timeout_ms: u64,
) -> Result<TcpStream, ResolveError> {
    let candidates = match parse_ip(host) {
        Some(address) => [Some(address), None],
        None => {
            let reach = reach(net).map_err(ResolveError::Net)?;
            let server = reach.dns.ok_or(ResolveError::NoServer)?;
            let want = (reach.v4 || !reach.v6, reach.v6);
            lookup_via(net, host, server, oceans_dns::PORT, want)?.ordered(reach)
        }
    };
    let [Some(first), second] = candidates else {
        return Err(ResolveError::NoAddress);
    };
    race(net, first, second, port, timeout_ms).map_err(ResolveError::Net)
}

fn race(
    net: Handle,
    first: IpAddr,
    mut second: Option<IpAddr>,
    port: u16,
    timeout_ms: u64,
) -> Result<TcpStream, NetError> {
    let notification = oceans_rt::notification_create().map_err(NetError::Ipc)?;
    let mut attempts: [Option<TcpStream>; 2] = [None, None];
    let mut last_error = None;
    match TcpStream::connect_ip_on(net, first, port, notification, READABLE) {
        Ok(stream) => attempts[0] = Some(stream),
        Err(error) => last_error = Some(error),
    }
    let started = oceans_rt::clock_ms();
    if second.is_some() {
        let _ = oceans_rt::timer_set(notification, ATTEMPT_TIMER, ATTEMPT_DELAY_MS);
    }
    let winner = wait_until(notification, timeout_ms, || {
        for slot in &mut attempts {
            match slot.as_ref().and_then(TcpStream::progress) {
                Some(Ok(())) => return Ok(Some(slot.take().expect("checked"))),
                Some(Err(error)) => {
                    last_error = Some(error);
                    *slot = None;
                }
                None => {}
            }
        }
        let head_start_over = oceans_rt::clock_ms() - started >= ATTEMPT_DELAY_MS;
        if let Some(address) = second
            && (attempts[0].is_none() || head_start_over)
        {
            second = None;
            match TcpStream::connect_ip_on(net, address, port, notification, READABLE) {
                Ok(stream) => attempts[1] = Some(stream),
                Err(error) => last_error = Some(error),
            }
            // Its first state change signals the notification.
        }
        if attempts.iter().all(Option::is_none) && second.is_none() {
            return Err(last_error.unwrap_or(NetError::Status(Status::NotConnected)));
        }
        Ok(None)
    });
    let _ = oceans_rt::timer_set(notification, ATTEMPT_TIMER, 0);
    // Losers are dropped (closing them) before the winner takes over the
    // notification.
    drop(attempts);
    match winner {
        Ok(mut stream) => {
            stream.owned = true;
            Ok(stream)
        }
        Err(error) => {
            let _ = oceans_rt::close(notification);
            Err(error)
        }
    }
}
