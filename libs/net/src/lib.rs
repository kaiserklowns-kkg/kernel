//! The Oceans network stack core (ADR-0023): Ethernet II, ARP, IPv4, ICMP
//! echo, UDP and a DHCP client.
//!
//! A pure state machine with no I/O of its own. The `net` service feeds it
//! received frames ([`Stack::receive`]) and the time ([`Stack::poll`]), and
//! sends what it produces ([`Stack::transmit`]); clients use sockets. The
//! same code runs in the host tests.
//!
//! Frames come from the network: untrusted. Every header is length-checked
//! before use, checksums are verified, and fragments, options and anything
//! not understood are dropped, never guessed at. Queues are bounded.

#![no_std]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

pub type Mac = [u8; 6];
pub type Ipv4 = [u8; 4];
pub type SocketId = usize;

pub const BROADCAST_MAC: Mac = [0xff; 6];
pub const BROADCAST: Ipv4 = [255; 4];
pub const UNSPECIFIED: Ipv4 = [0; 4];

/// Largest IP packet (Ethernet payload).
pub const MTU: usize = 1500;
const ETH_HEADER: usize = 14;
const IP_HEADER: usize = 20;
const UDP_HEADER: usize = 8;
const ICMP_HEADER: usize = 8;
/// Largest UDP payload in one (unfragmented) datagram.
pub const MAX_UDP_PAYLOAD: usize = MTU - IP_HEADER - UDP_HEADER;
pub const MAX_PING_PAYLOAD: usize = MTU - IP_HEADER - ICMP_HEADER;

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const PROTOCOL_ICMP: u8 = 1;
const PROTOCOL_UDP: u8 = 17;
const TTL: u8 = 64;

const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_UNREACHABLE: u8 = 3;
const ICMP_PORT_UNREACHABLE: u8 = 3;
const ICMP_ECHO_REQUEST: u8 = 8;

const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_MAGIC: [u8; 4] = [99, 130, 83, 99];
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;
const DHCP_NAK: u8 = 6;
const DHCP_FIRST_RETRY_MS: u64 = 2_000;
const DHCP_MAX_RETRY_MS: u64 = 16_000;
const DHCP_REQUEST_TRIES: u32 = 4;
const DHCP_DEFAULT_LEASE_S: u64 = 3_600;
const DHCP_RENEW_RETRY_MS: u64 = 60_000;

const ARP_CACHE: usize = 16;
const ARP_ENTRY_MS: u64 = 60_000;
const ARP_RETRY_MS: u64 = 1_000;
const ARP_TRIES: u32 = 3;
const ARP_WAITING: usize = 16;

const MAX_SOCKETS: usize = 64;
const SOCKET_QUEUE: usize = 32;
const OUTBOX: usize = 64;
const EPHEMERAL_PORTS: core::ops::RangeInclusive<u16> = 49_152..=65_535;

/// Internet checksum (RFC 1071) of `data`, continuing from `sum`.
fn checksum_add(mut sum: u32, data: &[u8]) -> u32 {
    let (pairs, rest) = data.as_chunks::<2>();
    for pair in pairs {
        sum += u32::from(u16::from_be_bytes(*pair));
    }
    if let [last] = rest {
        sum += u32::from(*last) << 8;
    }
    sum
}

fn checksum_finish(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The Internet checksum of `data`.
pub fn checksum(data: &[u8]) -> u16 {
    checksum_finish(checksum_add(0, data))
}

/// UDP checksum over the pseudo-header and `segment` (header + data).
fn udp_checksum(src: Ipv4, dst: Ipv4, segment: &[u8]) -> u16 {
    let mut sum = checksum_add(0, &src);
    sum = checksum_add(sum, &dst);
    sum += u32::from(PROTOCOL_UDP);
    sum += segment.len() as u32;
    match checksum_finish(checksum_add(sum, segment)) {
        0 => 0xffff, // 0 means "no checksum" in UDP
        value => value,
    }
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn ip_at(bytes: &[u8], at: usize) -> Ipv4 {
    [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
}

/// Network configuration, from DHCP or static.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub address: Ipv4,
    pub prefix: u8,
    pub gateway: Option<Ipv4>,
    pub dns: Option<Ipv4>,
}

impl Config {
    fn mask(&self) -> u32 {
        match self.prefix {
            0 => 0,
            p => u32::MAX << (32 - u32::from(p.min(32))),
        }
    }

    pub fn on_link(&self, address: Ipv4) -> bool {
        let mask = self.mask();
        u32::from_be_bytes(address) & mask == u32::from_be_bytes(self.address) & mask
    }

    pub fn subnet_broadcast(&self) -> Ipv4 {
        (u32::from_be_bytes(self.address) | !self.mask()).to_be_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    /// No address yet (DHCP still running).
    NotConfigured,
    AddressInUse,
    /// No gateway for an off-link destination.
    NoRoute,
    TooLarge,
    BadSocket,
    /// Too many sockets, or the queue toward the network is full.
    NoBuffers,
}

/// A received datagram: UDP (`port` = source port) or an echo reply
/// (`port` = sequence number).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
    pub from: Ipv4,
    pub port: u16,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketKind {
    Udp,
    /// ICMP echo: requests out, replies with this socket's identifier in.
    Ping,
}

struct Socket {
    kind: SocketKind,
    /// UDP: local port. Ping: echo identifier.
    port: u16,
    sequence: u16,
    queue: VecDeque<Datagram>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dhcp {
    /// Static configuration: no DHCP.
    Off,
    Discovering {
        xid: u32,
        next_send: u64,
        interval: u64,
    },
    Requesting {
        xid: u32,
        offered: Ipv4,
        server: Ipv4,
        next_send: u64,
        tries: u32,
    },
    Bound {
        xid: u32,
        server: Ipv4,
        renew_at: u64,
        expires_at: u64,
    },
}

struct ArpEntry {
    ip: Ipv4,
    mac: Mac,
    expires: u64,
}

struct Waiting {
    hop: Ipv4,
    packet: Vec<u8>,
    last_request: u64,
    tries: u32,
}

/// Counters, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub received: u64,
    pub sent: u64,
    pub dropped: u64,
}

pub struct Stack {
    mac: Mac,
    config: Option<Config>,
    dhcp: Dhcp,
    dhcp_attempts: u32,
    arp: Vec<ArpEntry>,
    waiting: Vec<Waiting>,
    sockets: Vec<Option<Socket>>,
    ready: Vec<SocketId>,
    outbox: VecDeque<Vec<u8>>,
    ip_id: u16,
    next_ident: u16,
    next_ephemeral: u16,
    stats: Stats,
}

impl Stack {
    /// A stack for the interface with address `mac`, configuring itself by
    /// DHCP from the first [`poll`](Self::poll).
    pub fn new(mac: Mac) -> Self {
        let mut stack = Self {
            mac,
            config: None,
            dhcp: Dhcp::Off,
            dhcp_attempts: 0,
            arp: Vec::new(),
            waiting: Vec::new(),
            sockets: Vec::new(),
            ready: Vec::new(),
            outbox: VecDeque::new(),
            ip_id: u16::from_be_bytes([mac[4], mac[5]]),
            next_ident: u16::from_be_bytes([mac[5], mac[3]]) | 1,
            next_ephemeral: *EPHEMERAL_PORTS.start(),
            stats: Stats::default(),
        };
        stack.restart_dhcp(0);
        stack
    }

    /// Uses a static configuration instead of DHCP.
    pub fn configure(&mut self, config: Config) {
        self.config = Some(config);
        self.dhcp = Dhcp::Off;
    }

    pub fn config(&self) -> Option<Config> {
        self.config
    }

    pub fn mac(&self) -> Mac {
        self.mac
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Whether DHCP holds a lease (false for static configurations).
    pub fn dhcp_bound(&self) -> bool {
        matches!(self.dhcp, Dhcp::Bound { .. })
    }

    /// The next frame to put on the wire.
    pub fn transmit(&mut self) -> Option<Vec<u8>> {
        self.outbox.pop_front()
    }

    /// Sockets that became readable since the last call.
    pub fn take_ready(&mut self) -> Vec<SocketId> {
        core::mem::take(&mut self.ready)
    }

    /// When [`poll`](Self::poll) next has work to do (milliseconds).
    pub fn next_deadline(&self) -> Option<u64> {
        let dhcp = match self.dhcp {
            Dhcp::Off => None,
            Dhcp::Discovering { next_send, .. } | Dhcp::Requesting { next_send, .. } => {
                Some(next_send)
            }
            Dhcp::Bound {
                renew_at,
                expires_at,
                ..
            } => Some(renew_at.min(expires_at)),
        };
        let arp = self
            .waiting
            .iter()
            .map(|w| w.last_request + ARP_RETRY_MS)
            .min();
        match (dhcp, arp) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    // ---- Sockets -----------------------------------------------------------

    fn new_socket(&mut self, kind: SocketKind, port: u16) -> Result<SocketId, NetError> {
        let socket = Socket {
            kind,
            port,
            sequence: 0,
            queue: VecDeque::new(),
        };
        if let Some(id) = self.sockets.iter().position(Option::is_none) {
            self.sockets[id] = Some(socket);
            return Ok(id);
        }
        if self.sockets.len() >= MAX_SOCKETS {
            return Err(NetError::NoBuffers);
        }
        self.sockets.push(Some(socket));
        Ok(self.sockets.len() - 1)
    }

    fn udp_port_used(&self, port: u16) -> bool {
        port == DHCP_CLIENT_PORT
            || self
                .sockets
                .iter()
                .flatten()
                .any(|s| s.kind == SocketKind::Udp && s.port == port)
    }

    /// A UDP socket on `port` (0: an ephemeral port).
    pub fn udp_bind(&mut self, port: u16) -> Result<SocketId, NetError> {
        let port = if port == 0 {
            let span = EPHEMERAL_PORTS.end() - EPHEMERAL_PORTS.start() + 1;
            let mut candidate = None;
            for _ in 0..span {
                let port = self.next_ephemeral;
                self.next_ephemeral = if port == *EPHEMERAL_PORTS.end() {
                    *EPHEMERAL_PORTS.start()
                } else {
                    port + 1
                };
                if !self.udp_port_used(port) {
                    candidate = Some(port);
                    break;
                }
            }
            candidate.ok_or(NetError::AddressInUse)?
        } else if self.udp_port_used(port) {
            return Err(NetError::AddressInUse);
        } else {
            port
        };
        self.new_socket(SocketKind::Udp, port)
    }

    /// An ICMP echo socket with its own identifier.
    pub fn ping_open(&mut self) -> Result<SocketId, NetError> {
        let ident = loop {
            let ident = self.next_ident;
            self.next_ident = self.next_ident.wrapping_add(1).max(1);
            let used = self
                .sockets
                .iter()
                .flatten()
                .any(|s| s.kind == SocketKind::Ping && s.port == ident);
            if !used {
                break ident;
            }
        };
        self.new_socket(SocketKind::Ping, ident)
    }

    /// The local port (UDP) or identifier (ping) of a socket.
    pub fn local_port(&self, id: SocketId) -> Option<u16> {
        Some(self.sockets.get(id)?.as_ref()?.port)
    }

    pub fn close(&mut self, id: SocketId) {
        if let Some(slot) = self.sockets.get_mut(id) {
            *slot = None;
        }
        self.ready.retain(|&r| r != id);
    }

    /// The oldest datagram received on the socket.
    pub fn recv(&mut self, id: SocketId) -> Option<Datagram> {
        self.sockets.get_mut(id)?.as_mut()?.queue.pop_front()
    }

    /// Sends `data` to `dst`: a UDP datagram to `port`, or an echo request
    /// (`port` ignored; the sequence number is the socket's next).
    pub fn send_to(
        &mut self,
        id: SocketId,
        dst: Ipv4,
        port: u16,
        data: &[u8],
        now: u64,
    ) -> Result<(), NetError> {
        let config = self.config.ok_or(NetError::NotConfigured)?;
        let socket = self
            .sockets
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(NetError::BadSocket)?;
        match socket.kind {
            SocketKind::Udp => {
                if data.len() > MAX_UDP_PAYLOAD {
                    return Err(NetError::TooLarge);
                }
                let segment = udp_segment(config.address, dst, socket.port, port, data);
                self.send_ip(dst, PROTOCOL_UDP, segment, now)
            }
            SocketKind::Ping => {
                if data.len() > MAX_PING_PAYLOAD {
                    return Err(NetError::TooLarge);
                }
                socket.sequence = socket.sequence.wrapping_add(1);
                let message = icmp_echo(ICMP_ECHO_REQUEST, socket.port, socket.sequence, data);
                self.send_ip(dst, PROTOCOL_ICMP, message, now)
            }
        }
    }

    /// The sequence number of the last echo request a ping socket sent.
    pub fn last_sequence(&self, id: SocketId) -> Option<u16> {
        Some(self.sockets.get(id)?.as_ref()?.sequence)
    }

    fn deliver(&mut self, id: SocketId, datagram: Datagram) {
        let Some(Some(socket)) = self.sockets.get_mut(id) else {
            return;
        };
        if socket.queue.len() >= SOCKET_QUEUE {
            self.stats.dropped += 1;
            return;
        }
        socket.queue.push_back(datagram);
        if !self.ready.contains(&id) {
            self.ready.push(id);
        }
    }

    // ---- Sending -----------------------------------------------------------

    fn emit(&mut self, dst: Mac, ethertype: u16, payload: &[u8]) -> Result<(), NetError> {
        if self.outbox.len() >= OUTBOX {
            self.stats.dropped += 1;
            return Err(NetError::NoBuffers);
        }
        let mut frame = Vec::with_capacity(ETH_HEADER + payload.len());
        frame.extend_from_slice(&dst);
        frame.extend_from_slice(&self.mac);
        frame.extend_from_slice(&ethertype.to_be_bytes());
        frame.extend_from_slice(payload);
        self.outbox.push_back(frame);
        self.stats.sent += 1;
        Ok(())
    }

    fn ip_packet(&mut self, src: Ipv4, dst: Ipv4, protocol: u8, payload: &[u8]) -> Vec<u8> {
        self.ip_id = self.ip_id.wrapping_add(1);
        let mut packet = Vec::with_capacity(IP_HEADER + payload.len());
        packet.extend_from_slice(&[0x45, 0]);
        packet.extend_from_slice(&((IP_HEADER + payload.len()) as u16).to_be_bytes());
        packet.extend_from_slice(&self.ip_id.to_be_bytes());
        packet.extend_from_slice(&0x4000u16.to_be_bytes()); // don't fragment
        packet.extend_from_slice(&[TTL, protocol, 0, 0]);
        packet.extend_from_slice(&src);
        packet.extend_from_slice(&dst);
        let sum = checksum(&packet[..IP_HEADER]);
        packet[10..12].copy_from_slice(&sum.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    /// Routes an IP payload: broadcast, on-link, or via the gateway,
    /// resolving the next hop with ARP.
    fn send_ip(
        &mut self,
        dst: Ipv4,
        protocol: u8,
        payload: Vec<u8>,
        now: u64,
    ) -> Result<(), NetError> {
        let config = self.config.ok_or(NetError::NotConfigured)?;
        let packet = self.ip_packet(config.address, dst, protocol, &payload);
        if dst == BROADCAST || dst == config.subnet_broadcast() {
            return self.emit(BROADCAST_MAC, ETHERTYPE_IPV4, &packet);
        }
        let hop = if config.on_link(dst) {
            dst
        } else {
            config.gateway.ok_or(NetError::NoRoute)?
        };
        if let Some(mac) = self.lookup(hop, now) {
            return self.emit(mac, ETHERTYPE_IPV4, &packet);
        }
        if self.waiting.len() >= ARP_WAITING {
            self.stats.dropped += 1;
            return Err(NetError::NoBuffers);
        }
        let asked = self.waiting.iter().any(|w| w.hop == hop);
        self.waiting.push(Waiting {
            hop,
            packet,
            last_request: now,
            tries: 1,
        });
        if !asked {
            self.send_arp(1, BROADCAST_MAC, [0; 6], hop);
        }
        Ok(())
    }

    fn lookup(&self, ip: Ipv4, now: u64) -> Option<Mac> {
        self.arp
            .iter()
            .find(|e| e.ip == ip && e.expires > now)
            .map(|e| e.mac)
    }

    fn send_arp(&mut self, operation: u16, dst: Mac, target_mac: Mac, target_ip: Ipv4) {
        let sender_ip = self.config.map_or(UNSPECIFIED, |c| c.address);
        let mut arp = Vec::with_capacity(28);
        arp.extend_from_slice(&[0, 1, 8, 0, 6, 4]);
        arp.extend_from_slice(&operation.to_be_bytes());
        arp.extend_from_slice(&self.mac);
        arp.extend_from_slice(&sender_ip);
        arp.extend_from_slice(&target_mac);
        arp.extend_from_slice(&target_ip);
        let _ = self.emit(dst, ETHERTYPE_ARP, &arp);
    }

    fn learn(&mut self, ip: Ipv4, mac: Mac, now: u64) {
        if ip == UNSPECIFIED || mac == BROADCAST_MAC || mac[0] & 1 != 0 {
            return;
        }
        let expires = now + ARP_ENTRY_MS;
        if let Some(entry) = self.arp.iter_mut().find(|e| e.ip == ip) {
            entry.mac = mac;
            entry.expires = expires;
        } else {
            if self.arp.len() >= ARP_CACHE {
                // Evict the entry closest to expiry.
                let oldest = (0..self.arp.len())
                    .min_by_key(|&i| self.arp[i].expires)
                    .expect("cache is full");
                self.arp.swap_remove(oldest);
            }
            self.arp.push(ArpEntry { ip, mac, expires });
        }
        // Packets waiting for this hop can go now.
        let mut index = 0;
        while index < self.waiting.len() {
            if self.waiting[index].hop == ip {
                let waiting = self.waiting.swap_remove(index);
                let _ = self.emit(mac, ETHERTYPE_IPV4, &waiting.packet);
            } else {
                index += 1;
            }
        }
    }

    // ---- Timers ------------------------------------------------------------

    /// Retransmissions and expiry: call at [`next_deadline`](Self::next_deadline)
    /// (calling more often is harmless).
    pub fn poll(&mut self, now: u64) {
        self.poll_dhcp(now);
        // ARP: retry unanswered requests, then give up on their packets.
        let mut hops: Vec<Ipv4> = Vec::new();
        for waiting in &mut self.waiting {
            if now >= waiting.last_request + ARP_RETRY_MS && !hops.contains(&waiting.hop) {
                hops.push(waiting.hop);
            }
        }
        for hop in hops {
            let tries = self
                .waiting
                .iter()
                .filter(|w| w.hop == hop)
                .map(|w| w.tries)
                .max()
                .unwrap_or(0);
            if tries >= ARP_TRIES {
                let before = self.waiting.len();
                self.waiting.retain(|w| w.hop != hop);
                self.stats.dropped += (before - self.waiting.len()) as u64;
            } else {
                for waiting in self.waiting.iter_mut().filter(|w| w.hop == hop) {
                    waiting.tries = tries + 1;
                    waiting.last_request = now;
                }
                self.send_arp(1, BROADCAST_MAC, [0; 6], hop);
            }
        }
        self.arp.retain(|e| e.expires > now);
    }

    // ---- DHCP --------------------------------------------------------------

    fn next_xid(&mut self) -> u32 {
        self.dhcp_attempts = self.dhcp_attempts.wrapping_add(1);
        u32::from_be_bytes([self.mac[2], self.mac[3], self.mac[4], self.mac[5]])
            ^ self.dhcp_attempts.rotate_left(16)
    }

    fn restart_dhcp(&mut self, now: u64) {
        let xid = self.next_xid();
        self.dhcp = Dhcp::Discovering {
            xid,
            next_send: now,
            interval: DHCP_FIRST_RETRY_MS,
        };
    }

    fn poll_dhcp(&mut self, now: u64) {
        match self.dhcp {
            Dhcp::Off => {}
            Dhcp::Discovering {
                xid,
                next_send,
                interval,
            } if now >= next_send => {
                self.send_dhcp(DHCP_DISCOVER, xid, None, None, UNSPECIFIED);
                self.dhcp = Dhcp::Discovering {
                    xid,
                    next_send: now + interval,
                    interval: (interval * 2).min(DHCP_MAX_RETRY_MS),
                };
            }
            Dhcp::Requesting {
                xid,
                offered,
                server,
                next_send,
                tries,
            } if now >= next_send => {
                if tries >= DHCP_REQUEST_TRIES {
                    self.restart_dhcp(now);
                } else {
                    self.send_dhcp(DHCP_REQUEST, xid, Some(offered), Some(server), UNSPECIFIED);
                    self.dhcp = Dhcp::Requesting {
                        xid,
                        offered,
                        server,
                        next_send: now + DHCP_FIRST_RETRY_MS,
                        tries: tries + 1,
                    };
                }
            }
            Dhcp::Bound {
                xid,
                server,
                renew_at,
                expires_at,
            } => {
                if now >= expires_at {
                    self.config = None;
                    self.restart_dhcp(now);
                } else if now >= renew_at {
                    let address = self.config.map(|c| c.address);
                    self.send_dhcp(
                        DHCP_REQUEST,
                        xid,
                        address,
                        Some(server),
                        address.unwrap_or(UNSPECIFIED),
                    );
                    self.dhcp = Dhcp::Bound {
                        xid,
                        server,
                        renew_at: now + DHCP_RENEW_RETRY_MS,
                        expires_at,
                    };
                }
            }
            _ => {}
        }
    }

    /// Broadcasts a DHCP message (from 0.0.0.0, or our address when
    /// renewing).
    fn send_dhcp(
        &mut self,
        kind: u8,
        xid: u32,
        requested: Option<Ipv4>,
        server: Option<Ipv4>,
        client_address: Ipv4,
    ) {
        let mut message = vec![0u8; 240];
        message[0] = 1; // BOOTREQUEST
        message[1] = 1; // Ethernet
        message[2] = 6;
        message[4..8].copy_from_slice(&xid.to_be_bytes());
        // Ask for broadcast replies only while we have no address.
        if client_address == UNSPECIFIED {
            message[10] = 0x80;
        }
        message[12..16].copy_from_slice(&client_address);
        message[28..34].copy_from_slice(&self.mac);
        message[236..240].copy_from_slice(&DHCP_MAGIC);
        message.extend_from_slice(&[53, 1, kind]);
        let mut client_id = [0u8; 9];
        client_id[..2].copy_from_slice(&[61, 7]);
        client_id[2] = 1;
        client_id[3..].copy_from_slice(&self.mac);
        message.extend_from_slice(&client_id);
        if let Some(requested) = requested.filter(|_| client_address == UNSPECIFIED) {
            message.extend_from_slice(&[50, 4]);
            message.extend_from_slice(&requested);
        }
        if let Some(server) = server.filter(|_| client_address == UNSPECIFIED) {
            message.extend_from_slice(&[54, 4]);
            message.extend_from_slice(&server);
        }
        message.extend_from_slice(&[55, 4, 1, 3, 6, 51, 255]);
        if message.len() < 300 {
            message.resize(300, 0); // the BOOTP minimum
        }
        let segment = udp_segment(
            client_address,
            BROADCAST,
            DHCP_CLIENT_PORT,
            DHCP_SERVER_PORT,
            &message,
        );
        let packet = self.ip_packet(client_address, BROADCAST, PROTOCOL_UDP, &segment);
        let _ = self.emit(BROADCAST_MAC, ETHERTYPE_IPV4, &packet);
    }

    fn on_dhcp(&mut self, message: &[u8], now: u64) {
        if message.len() < 240 || message[0] != 2 || message[236..240] != DHCP_MAGIC {
            return;
        }
        if message[28..34] != self.mac {
            return;
        }
        let xid = u32::from_be_bytes([message[4], message[5], message[6], message[7]]);
        let yiaddr = ip_at(message, 16);
        let mut kind = None;
        let mut mask = None;
        let mut router = None;
        let mut dns = None;
        let mut lease = None;
        let mut server = None;
        let mut at = 240;
        while at < message.len() {
            let code = message[at];
            if code == 255 {
                break;
            }
            if code == 0 {
                at += 1;
                continue;
            }
            let Some(&len) = message.get(at + 1) else {
                return;
            };
            let Some(value) = message.get(at + 2..at + 2 + usize::from(len)) else {
                return;
            };
            match (code, value.len()) {
                (53, 1) => kind = Some(value[0]),
                (1, 4) => mask = Some(ip_at(value, 0)),
                (3, 4..) => router = Some(ip_at(value, 0)),
                (6, 4..) => dns = Some(ip_at(value, 0)),
                (51, 4) => {
                    lease = Some(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
                }
                (54, 4) => server = Some(ip_at(value, 0)),
                _ => {}
            }
            at += 2 + usize::from(len);
        }
        match (self.dhcp, kind) {
            (Dhcp::Discovering { xid: ours, .. }, Some(DHCP_OFFER))
                if ours == xid && yiaddr != UNSPECIFIED =>
            {
                let server = server.unwrap_or(ip_at(message, 20));
                self.send_dhcp(DHCP_REQUEST, xid, Some(yiaddr), Some(server), UNSPECIFIED);
                self.dhcp = Dhcp::Requesting {
                    xid,
                    offered: yiaddr,
                    server,
                    next_send: now + DHCP_FIRST_RETRY_MS,
                    tries: 1,
                };
            }
            (
                Dhcp::Requesting {
                    xid: ours,
                    server: offered_by,
                    ..
                }
                | Dhcp::Bound {
                    xid: ours,
                    server: offered_by,
                    ..
                },
                Some(DHCP_ACK),
            ) if ours == xid && yiaddr != UNSPECIFIED => {
                let prefix = mask.map_or(24, |m| u32::from_be_bytes(m).leading_ones() as u8);
                self.config = Some(Config {
                    address: yiaddr,
                    prefix,
                    gateway: router,
                    dns,
                });
                let lease_ms =
                    u64::from(lease.unwrap_or(DHCP_DEFAULT_LEASE_S as u32)).max(60) * 1000;
                self.dhcp = Dhcp::Bound {
                    xid,
                    server: server.unwrap_or(offered_by),
                    renew_at: now + lease_ms / 2,
                    expires_at: now + lease_ms,
                };
            }
            (
                Dhcp::Requesting { xid: ours, .. } | Dhcp::Bound { xid: ours, .. },
                Some(DHCP_NAK),
            ) if ours == xid => {
                self.config = None;
                self.restart_dhcp(now);
            }
            _ => {}
        }
    }

    // ---- Receiving ---------------------------------------------------------

    /// Processes one received Ethernet frame.
    pub fn receive(&mut self, frame: &[u8], now: u64) {
        self.stats.received += 1;
        if frame.len() < ETH_HEADER {
            self.stats.dropped += 1;
            return;
        }
        let dst: Mac = frame[..6].try_into().expect("6 bytes");
        if dst != self.mac && dst != BROADCAST_MAC {
            return;
        }
        let src: Mac = frame[6..12].try_into().expect("6 bytes");
        let payload = &frame[ETH_HEADER..];
        let handled = match be16(frame, 12) {
            ETHERTYPE_ARP => self.on_arp(payload, now),
            ETHERTYPE_IPV4 => self.on_ipv4(src, payload, now),
            _ => false,
        };
        if !handled {
            self.stats.dropped += 1;
        }
    }

    fn on_arp(&mut self, arp: &[u8], now: u64) -> bool {
        if arp.len() < 28 || arp[..6] != [0, 1, 8, 0, 6, 4] {
            return false;
        }
        let operation = be16(arp, 6);
        let sender_mac: Mac = arp[8..14].try_into().expect("6 bytes");
        let sender_ip = ip_at(arp, 14);
        let target_ip = ip_at(arp, 24);
        let ours = self.config.map(|c| c.address);
        let for_us = Some(target_ip) == ours;
        // Learn from anything addressed to us, and update known entries.
        if for_us
            || self.arp.iter().any(|e| e.ip == sender_ip)
            || self.waiting.iter().any(|w| w.hop == sender_ip)
        {
            self.learn(sender_ip, sender_mac, now);
        }
        if operation == 1 && for_us {
            self.send_arp(2, sender_mac, sender_mac, sender_ip);
        }
        true
    }

    fn on_ipv4(&mut self, src_mac: Mac, packet: &[u8], now: u64) -> bool {
        if packet.len() < IP_HEADER || packet[0] >> 4 != 4 {
            return false;
        }
        let header_len = usize::from(packet[0] & 0xf) * 4;
        let total = usize::from(be16(packet, 2));
        if header_len < IP_HEADER || total < header_len || total > packet.len() {
            return false;
        }
        if checksum(&packet[..header_len]) != 0 {
            return false;
        }
        // Fragments are not reassembled.
        if be16(packet, 6) & 0x3fff != 0 {
            return false;
        }
        let src = ip_at(packet, 12);
        let dst = ip_at(packet, 16);
        let payload = &packet[header_len..total];
        let config = self.config;
        let ours = config.is_some_and(|c| dst == c.address);
        let broadcast = dst == BROADCAST || config.is_some_and(|c| dst == c.subnet_broadcast());
        // Before configuration, only DHCP replies (sent to the offered
        // address or broadcast) are accepted.
        let dhcp = packet[9] == PROTOCOL_UDP
            && payload.len() >= UDP_HEADER
            && be16(payload, 2) == DHCP_CLIENT_PORT
            && be16(payload, 0) == DHCP_SERVER_PORT;
        if !ours && !broadcast && !dhcp {
            return false;
        }
        // A reply's sender is on-link (or the gateway): remember its MAC.
        if ours && config.is_some_and(|c| c.on_link(src)) && self.arp.iter().any(|e| e.ip == src) {
            self.learn(src, src_mac, now);
        }
        match packet[9] {
            PROTOCOL_ICMP => self.on_icmp(src, ours, payload, now),
            PROTOCOL_UDP => self.on_udp(src, dst, ours, &packet[..header_len], payload, now),
            _ => false,
        }
    }

    fn on_icmp(&mut self, src: Ipv4, ours: bool, message: &[u8], now: u64) -> bool {
        if message.len() < ICMP_HEADER || checksum(message) != 0 {
            return false;
        }
        let ident = be16(message, 4);
        let sequence = be16(message, 6);
        match message[0] {
            ICMP_ECHO_REQUEST if ours => {
                let reply = icmp_echo(ICMP_ECHO_REPLY, ident, sequence, &message[ICMP_HEADER..]);
                let _ = self.send_ip(src, PROTOCOL_ICMP, reply, now);
                true
            }
            ICMP_ECHO_REPLY if ours => {
                let socket = self.sockets.iter().position(|s| {
                    s.as_ref()
                        .is_some_and(|s| s.kind == SocketKind::Ping && s.port == ident)
                });
                match socket {
                    Some(id) => {
                        self.deliver(
                            id,
                            Datagram {
                                from: src,
                                port: sequence,
                                data: message[ICMP_HEADER..].to_vec(),
                            },
                        );
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn on_udp(
        &mut self,
        src: Ipv4,
        dst: Ipv4,
        ours: bool,
        ip_header: &[u8],
        segment: &[u8],
        now: u64,
    ) -> bool {
        if segment.len() < UDP_HEADER {
            return false;
        }
        let length = usize::from(be16(segment, 4));
        if length < UDP_HEADER || length > segment.len() {
            return false;
        }
        let segment = &segment[..length];
        if be16(segment, 6) != 0 {
            let mut sum = checksum_add(0, &src);
            sum = checksum_add(sum, &dst);
            sum += u32::from(PROTOCOL_UDP) + length as u32;
            if checksum_finish(checksum_add(sum, segment)) != 0 {
                return false;
            }
        }
        let src_port = be16(segment, 0);
        let dst_port = be16(segment, 2);
        let data = &segment[UDP_HEADER..];
        if dst_port == DHCP_CLIENT_PORT && src_port == DHCP_SERVER_PORT {
            self.on_dhcp(data, now);
            return true;
        }
        let socket = self.sockets.iter().position(|s| {
            s.as_ref()
                .is_some_and(|s| s.kind == SocketKind::Udp && s.port == dst_port)
        });
        match socket {
            Some(id) => {
                self.deliver(
                    id,
                    Datagram {
                        from: src,
                        port: src_port,
                        data: data.to_vec(),
                    },
                );
                true
            }
            None => {
                // Port unreachable, never in answer to a broadcast.
                if ours {
                    let mut message =
                        vec![ICMP_UNREACHABLE, ICMP_PORT_UNREACHABLE, 0, 0, 0, 0, 0, 0];
                    message.extend_from_slice(ip_header);
                    message.extend_from_slice(&segment[..segment.len().min(8)]);
                    let sum = checksum(&message);
                    message[2..4].copy_from_slice(&sum.to_be_bytes());
                    let _ = self.send_ip(src, PROTOCOL_ICMP, message, now);
                }
                false
            }
        }
    }
}

fn udp_segment(src: Ipv4, dst: Ipv4, src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let length = (UDP_HEADER + data.len()) as u16;
    let mut segment = Vec::with_capacity(usize::from(length));
    segment.extend_from_slice(&src_port.to_be_bytes());
    segment.extend_from_slice(&dst_port.to_be_bytes());
    segment.extend_from_slice(&length.to_be_bytes());
    segment.extend_from_slice(&[0, 0]);
    segment.extend_from_slice(data);
    let sum = udp_checksum(src, dst, &segment);
    segment[6..8].copy_from_slice(&sum.to_be_bytes());
    segment
}

fn icmp_echo(kind: u8, ident: u16, sequence: u16, data: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(ICMP_HEADER + data.len());
    message.extend_from_slice(&[kind, 0, 0, 0]);
    message.extend_from_slice(&ident.to_be_bytes());
    message.extend_from_slice(&sequence.to_be_bytes());
    message.extend_from_slice(data);
    let sum = checksum(&message);
    message[2..4].copy_from_slice(&sum.to_be_bytes());
    message
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

#[cfg(test)]
mod tests;
