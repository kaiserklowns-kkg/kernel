//! TCP (RFC 9293), ADR-0024, over IPv4 and IPv6 (ADR-0043).
//!
//! - Active and passive open, with the MSS option.
//! - Sliding-window send and receive with 16 KiB buffers each.
//! - Retransmission with RFC 6298 timeouts (Karn's rule, exponential
//!   backoff); after 8 retries the connection times out.
//! - Zero-window probing.
//! - Graceful close in both directions, with TIME_WAIT.
//! - RST handling hardened per RFC 5961: an RST or SYN in the window but
//!   not exactly at `rcv_nxt` only draws a challenge ACK.
//!
//! Deliberate simplifications:
//! - Out-of-order segments are dropped (the sender retransmits);
//! - there is no window scaling, SACK or congestion control beyond backoff.
//!
//! Every segment is untrusted: lengths, data offsets and checksums are
//! verified before anything is read.

use super::*;

pub(crate) const PROTOCOL_TCP: u8 = 6;
const TCP_HEADER: usize = 20;

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

/// Bytes buffered per direction per connection.
pub const TCP_BUFFER: usize = 16 * 1024;
/// Our maximum segment size over IPv4: what an Ethernet MTU carries.
const OUR_MSS: u16 = (MTU - IP_HEADER - TCP_HEADER) as u16;
/// The smallest MSS an IPv6 path guarantees (1280-byte minimum MTU).
pub(crate) const MIN_MSS6: u16 = (ipv6::MIN_MTU - ipv6::HEADER - TCP_HEADER) as u16;
/// The peer's when it does not say (RFC 9293).
const DEFAULT_MSS: u16 = 536;
const INITIAL_RTO_MS: u64 = 1_000;
const MIN_RTO_MS: u64 = 200;
const MAX_RTO_MS: u64 = 60_000;
const MAX_RETRIES: u32 = 8;
const MAX_SYN_RETRIES: u32 = 5;
/// 2 × MSL, with a short MSL: closed connections linger this long.
const TIME_WAIT_MS: u64 = 10_000;
/// An abandoned connection waiting for the peer's FIN gives up after this.
const ORPHAN_FIN_WAIT_MS: u64 = 60_000;
/// Connections a listener holds before `accept` (established + opening).
pub const BACKLOG: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

/// Why a connection ended abnormally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpError {
    /// The peer answered the SYN with RST: nothing listens there.
    Refused,
    Reset,
    TimedOut,
}

/// Result of a receive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recv {
    /// Bytes copied.
    Data(usize),
    /// Nothing yet.
    WouldBlock,
    /// The peer closed its side; no more data will come.
    Eof,
}

fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn seq_le(a: u32, b: u32) -> bool {
    a == b || seq_lt(a, b)
}

/// A segment to send (addressing comes from its connection).
struct Segment {
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    mss: Option<u16>,
    data: Vec<u8>,
}

/// A received segment, already checked.
struct Incoming<'a> {
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    mss: Option<u16>,
    data: &'a [u8],
}

pub(crate) struct Tcb {
    pub(crate) state: TcpState,
    pub(crate) remote: IpAddr,
    remote_port: u16,
    /// Our address on this connection: the family, and for IPv6 which of
    /// our addresses (source address selection, or the peer's choice).
    pub(crate) local: IpAddr,
    pub(crate) local_port: u16,
    /// The MSS we announce: what the link MTU carries in this family.
    our_mss: u16,
    pub(crate) iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    pub(crate) mss: u16,
    /// Unacknowledged and unsent data; byte 0 has sequence `snd_una`.
    send: VecDeque<u8>,
    /// The user closed its side: a FIN follows the data.
    fin_queued: bool,
    rcv_nxt: u32,
    pub(crate) recv: VecDeque<u8>,
    fin_received: bool,
    /// The window we last advertised.
    advertised: u32,
    rto: u64,
    srtt: Option<u64>,
    rttvar: u64,
    /// A segment being timed: (sequence acknowledging it, sent at).
    sample: Option<(u32, u64)>,
    timer: Option<u64>,
    retries: u32,
    pub(crate) error: Option<TcpError>,
    /// The listener, until the connection is accepted.
    pub(crate) parent: Option<SocketId>,
    /// The user released it: freed once closed.
    pub(crate) orphan: bool,
}

impl Tcb {
    fn new(
        (local, local_port): (IpAddr, u16),
        (remote, remote_port): (IpAddr, u16),
        our_mss: u16,
        iss: u32,
        state: TcpState,
    ) -> Self {
        Self {
            state,
            remote,
            remote_port,
            local,
            local_port,
            our_mss,
            iss,
            snd_una: iss,
            snd_nxt: iss.wrapping_add(1),
            snd_wnd: 0,
            mss: DEFAULT_MSS,
            send: VecDeque::new(),
            fin_queued: false,
            rcv_nxt: 0,
            recv: VecDeque::new(),
            fin_received: false,
            advertised: 0,
            rto: INITIAL_RTO_MS,
            srtt: None,
            rttvar: 0,
            sample: None,
            timer: None,
            retries: 0,
            error: None,
            parent: None,
            orphan: false,
        }
    }

    fn window(&self) -> u32 {
        (TCP_BUFFER - self.recv.len()) as u32
    }

    fn segment(&mut self, seq: u32, flags: u8, data: Vec<u8>) -> Segment {
        let window = self.window().min(u32::from(u16::MAX));
        if flags & ACK != 0 {
            self.advertised = window;
        }
        Segment {
            seq,
            ack: if flags & ACK != 0 { self.rcv_nxt } else { 0 },
            flags,
            window: window as u16,
            mss: (flags & SYN != 0).then_some(self.our_mss),
            data,
        }
    }

    fn ack(&mut self) -> Segment {
        self.segment(self.snd_nxt, ACK, Vec::new())
    }

    fn syn(&mut self) -> Segment {
        let flags = if self.state == TcpState::SynReceived {
            SYN | ACK
        } else {
            SYN
        };
        self.segment(self.iss, flags, Vec::new())
    }

    fn synchronized(&self) -> bool {
        !matches!(
            self.state,
            TcpState::Closed | TcpState::Listen | TcpState::SynSent | TcpState::SynReceived
        )
    }

    fn update_rtt(&mut self, sample: u64) {
        match self.srtt {
            None => {
                self.srtt = Some(sample);
                self.rttvar = sample / 2;
            }
            Some(srtt) => {
                self.rttvar = (3 * self.rttvar + srtt.abs_diff(sample)) / 4;
                self.srtt = Some((7 * srtt + sample) / 8);
            }
        }
        let srtt = self.srtt.unwrap_or(sample);
        self.rto = (srtt + (4 * self.rttvar).max(10)).clamp(MIN_RTO_MS, MAX_RTO_MS);
    }

    /// Sends what the window allows: data, then a FIN once the user closed.
    fn output(&mut self, now: u64, out: &mut Vec<Segment>) {
        if !matches!(
            self.state,
            TcpState::Established
                | TcpState::CloseWait
                | TcpState::FinWait1
                | TcpState::Closing
                | TcpState::LastAck
        ) {
            return;
        }
        loop {
            let sent = self.snd_nxt.wrapping_sub(self.snd_una) as usize;
            if sent < self.send.len() {
                let window_end = self.snd_una.wrapping_add(self.snd_wnd);
                let usable = if seq_lt(self.snd_nxt, window_end) {
                    window_end.wrapping_sub(self.snd_nxt) as usize
                } else {
                    0
                };
                if usable == 0 {
                    // Zero window: the timer probes it.
                    if self.timer.is_none() {
                        self.timer = Some(now + self.rto);
                    }
                    return;
                }
                let len = (self.send.len() - sent)
                    .min(usize::from(self.mss))
                    .min(usable);
                let data: Vec<u8> = self.send.range(sent..sent + len).copied().collect();
                let seq = self.snd_nxt;
                let segment = self.segment(seq, ACK | PSH, data);
                out.push(segment);
                self.snd_nxt = seq.wrapping_add(len as u32);
                if self.sample.is_none() {
                    self.sample = Some((self.snd_nxt, now));
                }
                if self.timer.is_none() {
                    self.timer = Some(now + self.rto);
                }
                continue;
            }
            if self.fin_queued && sent == self.send.len() {
                let seq = self.snd_nxt;
                let segment = self.segment(seq, FIN | ACK, Vec::new());
                out.push(segment);
                self.snd_nxt = seq.wrapping_add(1);
                self.state = match self.state {
                    TcpState::Established => TcpState::FinWait1,
                    TcpState::CloseWait => TcpState::LastAck,
                    other => other,
                };
                if self.timer.is_none() {
                    self.timer = Some(now + self.rto);
                }
            }
            return;
        }
    }

    fn enter_time_wait(&mut self, now: u64) {
        self.state = TcpState::TimeWait;
        self.timer = Some(now + TIME_WAIT_MS);
    }

    /// Fails the connection and sends a reset if it was synchronized.
    fn abort(&mut self, error: Option<TcpError>, out: &mut Vec<Segment>) {
        if self.synchronized() || self.state == TcpState::SynReceived {
            let seq = self.snd_nxt;
            out.push(self.segment(seq, RST | ACK, Vec::new()));
        }
        self.state = TcpState::Closed;
        self.error = self.error.or(error);
        self.timer = None;
    }

    /// The retransmission, persist or TIME_WAIT timer fired.
    fn on_timer(&mut self, now: u64, out: &mut Vec<Segment>) {
        match self.state {
            TcpState::TimeWait => {
                self.state = TcpState::Closed;
                self.timer = None;
            }
            TcpState::FinWait2 => {
                // Only orphans time out here.
                self.state = TcpState::Closed;
                self.timer = None;
            }
            TcpState::SynSent | TcpState::SynReceived => {
                self.retries += 1;
                if self.retries > MAX_SYN_RETRIES {
                    self.abort(Some(TcpError::TimedOut), out);
                    return;
                }
                self.rto = (self.rto * 2).min(MAX_RTO_MS);
                self.sample = None;
                let syn = self.syn();
                out.push(syn);
                self.timer = Some(now + self.rto);
            }
            TcpState::Closed | TcpState::Listen => self.timer = None,
            _ => {
                let outstanding = self.snd_una != self.snd_nxt;
                let waiting = (self.snd_nxt.wrapping_sub(self.snd_una) as usize) < self.send.len();
                if outstanding {
                    self.retries += 1;
                    if self.retries > MAX_RETRIES {
                        self.abort(Some(TcpError::TimedOut), out);
                        return;
                    }
                    // Go back to the oldest unacknowledged byte.
                    self.rto = (self.rto * 2).min(MAX_RTO_MS);
                    self.sample = None;
                    self.snd_nxt = self.snd_una;
                    self.timer = None;
                    self.output(now, out);
                    self.timer = Some(now + self.rto);
                } else if waiting && self.snd_wnd == 0 {
                    // Persist: probe the closed window with one byte.
                    let seq = self.snd_nxt;
                    let byte = vec![self.send[seq.wrapping_sub(self.snd_una) as usize]];
                    let probe = self.segment(seq, ACK, byte);
                    out.push(probe);
                    self.rto = (self.rto * 2).min(MAX_RTO_MS);
                    self.timer = Some(now + self.rto);
                } else {
                    self.timer = None;
                }
            }
        }
    }

    /// Processes a segment for this connection. Returns whether anything a
    /// user waits for changed.
    fn input(&mut self, segment: &Incoming<'_>, now: u64, out: &mut Vec<Segment>) -> bool {
        match self.state {
            TcpState::Closed | TcpState::Listen => false,
            TcpState::SynSent => self.input_syn_sent(segment, now, out),
            _ => self.input_synchronized(segment, now, out),
        }
    }

    fn input_syn_sent(&mut self, segment: &Incoming<'_>, now: u64, out: &mut Vec<Segment>) -> bool {
        let acceptable_ack = segment.ack == self.iss.wrapping_add(1);
        if segment.flags & ACK != 0 && !acceptable_ack {
            if segment.flags & RST == 0 {
                out.push(Segment {
                    seq: segment.ack,
                    ack: 0,
                    flags: RST,
                    window: 0,
                    mss: None,
                    data: Vec::new(),
                });
            }
            return false;
        }
        if segment.flags & RST != 0 {
            if segment.flags & ACK != 0 {
                self.state = TcpState::Closed;
                self.error = Some(TcpError::Refused);
                self.timer = None;
                return true;
            }
            return false;
        }
        if segment.flags & SYN == 0 || segment.flags & ACK == 0 {
            return false; // simultaneous open is not supported
        }
        self.rcv_nxt = segment.seq.wrapping_add(1);
        self.mss = segment.mss.unwrap_or(DEFAULT_MSS).clamp(64, self.our_mss);
        self.snd_una = segment.ack;
        self.snd_nxt = segment.ack;
        self.snd_wnd = u32::from(segment.window);
        if self.retries == 0
            && let Some(timer) = self.timer
        {
            self.update_rtt(now.saturating_sub(timer - self.rto));
        }
        self.state = TcpState::Established;
        self.timer = None;
        self.retries = 0;
        let ack = self.ack();
        out.push(ack);
        self.output(now, out);
        true
    }

    fn input_synchronized(
        &mut self,
        segment: &Incoming<'_>,
        now: u64,
        out: &mut Vec<Segment>,
    ) -> bool {
        let mut data = segment.data;
        let mut seq = segment.seq;
        let syn_fin = u32::from(segment.flags & SYN != 0) + u32::from(segment.flags & FIN != 0);
        let len = data.len() as u32 + syn_fin;
        let window = self.window();
        let in_window =
            |s: u32| seq_le(self.rcv_nxt, s) && seq_lt(s, self.rcv_nxt.wrapping_add(window.max(1)));
        let acceptable = if len == 0 {
            if window == 0 {
                seq == self.rcv_nxt
            } else {
                in_window(seq)
            }
        } else {
            // Retransmissions overlapping what we have are trimmed below.
            in_window(seq)
                || in_window(seq.wrapping_add(len - 1))
                || (seq_lt(seq, self.rcv_nxt) && seq_lt(self.rcv_nxt, seq.wrapping_add(len)))
        };
        if !acceptable {
            if segment.flags & RST == 0 {
                let ack = self.ack();
                out.push(ack);
            }
            return false;
        }
        if segment.flags & RST != 0 {
            if seq != self.rcv_nxt {
                let ack = self.ack(); // challenge ACK (RFC 5961)
                out.push(ack);
                return false;
            }
            if self.state != TcpState::SynReceived {
                self.error = Some(TcpError::Reset);
            }
            self.state = TcpState::Closed;
            self.timer = None;
            return true;
        }
        if segment.flags & SYN != 0 {
            let ack = self.ack(); // challenge ACK (RFC 5961)
            out.push(ack);
            return false;
        }
        if segment.flags & ACK == 0 {
            return false;
        }
        let mut changed = false;
        if self.state == TcpState::SynReceived {
            if segment.ack != self.iss.wrapping_add(1) {
                out.push(Segment {
                    seq: segment.ack,
                    ack: 0,
                    flags: RST,
                    window: 0,
                    mss: None,
                    data: Vec::new(),
                });
                return false;
            }
            self.snd_una = segment.ack;
            self.snd_nxt = segment.ack;
            self.state = TcpState::Established;
            self.timer = None;
            self.retries = 0;
            changed = true;
        }
        // Acknowledgement.
        let ack = segment.ack;
        if seq_lt(self.snd_una, ack) && seq_le(ack, self.snd_nxt) {
            let acked = ack.wrapping_sub(self.snd_una) as usize;
            let data_acked = acked.min(self.send.len());
            self.send.drain(..data_acked);
            self.snd_una = ack;
            if let Some((target, at)) = self.sample
                && seq_le(target, ack)
            {
                self.update_rtt(now.saturating_sub(at));
                self.sample = None;
            }
            self.retries = 0;
            self.timer = (self.snd_una != self.snd_nxt).then_some(now + self.rto);
            changed = true;
            let fin_acked = self.fin_queued && self.send.is_empty() && self.snd_una == self.snd_nxt;
            if fin_acked {
                match self.state {
                    TcpState::FinWait1 => {
                        self.state = TcpState::FinWait2;
                        if self.orphan {
                            self.timer = Some(now + ORPHAN_FIN_WAIT_MS);
                        }
                    }
                    TcpState::Closing => self.enter_time_wait(now),
                    TcpState::LastAck => {
                        self.state = TcpState::Closed;
                        self.timer = None;
                    }
                    _ => {}
                }
            }
        } else if seq_lt(self.snd_nxt, ack) {
            let ack = self.ack();
            out.push(ack);
            return changed;
        }
        if seq_le(self.snd_una, ack) {
            let opened = self.snd_wnd == 0 && segment.window > 0;
            self.snd_wnd = u32::from(segment.window);
            if opened && self.snd_una == self.snd_nxt {
                self.timer = None;
            }
        }
        // Data, in order only; a retransmission's known part is trimmed.
        let mut need_ack = false;
        if !data.is_empty() {
            if seq_lt(seq, self.rcv_nxt) {
                let skip = (self.rcv_nxt.wrapping_sub(seq) as usize).min(data.len());
                data = &data[skip..];
                seq = self.rcv_nxt;
            }
            if matches!(
                self.state,
                TcpState::Established | TcpState::FinWait1 | TcpState::FinWait2
            ) && seq == self.rcv_nxt
                && !data.is_empty()
            {
                if self.orphan {
                    // Nobody will read it.
                    self.abort(None, out);
                    return true;
                }
                let take = data.len().min(self.window() as usize);
                self.recv.extend(&data[..take]);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(take as u32);
                data = &data[take..];
                changed |= take > 0;
            }
            need_ack = true;
        }
        // FIN, once everything before it has arrived.
        if segment.flags & FIN != 0 {
            let fin_seq = segment.seq.wrapping_add(segment.data.len() as u32);
            if !self.fin_received && data.is_empty() && fin_seq == self.rcv_nxt {
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
                self.fin_received = true;
                changed = true;
                match self.state {
                    TcpState::Established | TcpState::SynReceived => {
                        self.state = TcpState::CloseWait;
                    }
                    TcpState::FinWait1 => self.state = TcpState::Closing,
                    TcpState::FinWait2 => self.enter_time_wait(now),
                    _ => {}
                }
            } else if self.state == TcpState::TimeWait {
                self.enter_time_wait(now); // their FIN again: our ACK was lost
            }
            need_ack = true;
        }
        if need_ack {
            let ack = self.ack();
            out.push(ack);
        }
        self.output(now, out);
        changed
    }

    /// After the user read: advertise a window that opened significantly.
    fn window_update(&mut self) -> Option<Segment> {
        let window = self.window();
        let opened = window.saturating_sub(self.advertised);
        let worth = self.advertised < u32::from(self.mss) || opened >= (TCP_BUFFER / 2) as u32;
        (self.synchronized() && !self.fin_received && opened > 0 && worth).then(|| self.ack())
    }
}

/// A fast 64-bit mix (splitmix64 finalizer).
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

impl Stack {
    /// Keys initial sequence numbers (RFC 6528); feed it unpredictable
    /// bits (e.g. the TSC at boot).
    pub fn set_secret(&mut self, secret: u64) {
        self.isn_secret = mix(secret ^ self.isn_secret);
    }

    fn isn(&self, local_port: u16, remote: IpAddr, remote_port: u16, now: u64) -> u32 {
        let ports = (u64::from(local_port) << 16) | u64::from(remote_port);
        let hash = match remote {
            IpAddr::V4(remote) => {
                mix(self.isn_secret ^ (u64::from(u32::from_be_bytes(remote)) << 32) ^ ports)
            }
            IpAddr::V6(remote) => {
                let (high, low) = remote.split_at(8);
                let high = u64::from_be_bytes(high.try_into().expect("8 bytes"));
                let low = u64::from_be_bytes(low.try_into().expect("8 bytes"));
                mix(mix(self.isn_secret ^ high) ^ low ^ ports)
            }
        };
        (hash as u32).wrapping_add((now as u32).wrapping_mul(250))
    }

    pub(crate) fn tcp(&self, id: SocketId) -> Option<&Tcb> {
        self.sockets.get(id)?.as_ref()?.tcp.as_deref()
    }

    fn tcp_mut(&mut self, id: SocketId) -> Option<&mut Tcb> {
        self.sockets.get_mut(id)?.as_mut()?.tcp.as_deref_mut()
    }

    fn tcp_port_used(&self, port: u16) -> bool {
        self.sockets
            .iter()
            .flatten()
            .any(|s| matches!(s.kind, SocketKind::Tcp | SocketKind::Listen) && s.port == port)
    }

    fn new_tcp_socket(
        &mut self,
        kind: SocketKind,
        port: u16,
        tcb: Option<Tcb>,
    ) -> Result<SocketId, NetError> {
        let id = self.new_socket(kind, port)?;
        if let Some(Some(socket)) = self.sockets.get_mut(id) {
            socket.tcp = tcb.map(Box::new);
        }
        Ok(id)
    }

    /// A listening socket on `port`.
    pub fn tcp_listen(&mut self, port: u16) -> Result<SocketId, NetError> {
        if port == 0 {
            return Err(NetError::BadSocket);
        }
        if self.tcp_port_used(port) {
            return Err(NetError::AddressInUse);
        }
        self.new_tcp_socket(SocketKind::Listen, port, None)
    }

    /// Starts a connection to `dst:port` (either family) from an ephemeral
    /// port.
    pub fn tcp_connect(
        &mut self,
        dst: impl Into<IpAddr>,
        port: u16,
        now: u64,
    ) -> Result<SocketId, NetError> {
        let dst = dst.into();
        let (local, our_mss) = match dst {
            IpAddr::V4(dst) => {
                let config = self.config.ok_or(NetError::NotConfigured)?;
                if !config.on_link(dst) && config.gateway.is_none() {
                    return Err(NetError::NoRoute);
                }
                (IpAddr::V4(config.address), OUR_MSS)
            }
            IpAddr::V6(dst) => {
                let (source, mtu) = self.route6(&dst)?;
                let mss = (mtu - ipv6::HEADER - TCP_HEADER) as u16;
                (IpAddr::V6(source), mss.max(MIN_MSS6))
            }
        };
        let span = EPHEMERAL_PORTS.end() - EPHEMERAL_PORTS.start() + 1;
        let mut local_port = None;
        for _ in 0..span {
            let candidate = self.next_ephemeral;
            self.next_ephemeral = if candidate == *EPHEMERAL_PORTS.end() {
                *EPHEMERAL_PORTS.start()
            } else {
                candidate + 1
            };
            if !self.tcp_port_used(candidate) {
                local_port = Some(candidate);
                break;
            }
        }
        let local_port = local_port.ok_or(NetError::AddressInUse)?;
        let iss = self.isn(local_port, dst, port, now);
        let mut tcb = Tcb::new(
            (local, local_port),
            (dst, port),
            our_mss,
            iss,
            TcpState::SynSent,
        );
        let syn = tcb.syn();
        tcb.timer = Some(now + tcb.rto);
        let id = self.new_tcp_socket(SocketKind::Tcp, local_port, Some(tcb))?;
        self.transmit_segments(id, [syn].into_iter().collect(), now);
        Ok(id)
    }

    /// An established connection waiting on a listener, if any.
    pub fn tcp_accept(&mut self, listener: SocketId) -> Result<Option<SocketId>, NetError> {
        let socket = self
            .sockets
            .get_mut(listener)
            .and_then(Option::as_mut)
            .filter(|s| s.kind == SocketKind::Listen)
            .ok_or(NetError::BadSocket)?;
        let Some(child) = socket.backlog.pop_front() else {
            return Ok(None);
        };
        if let Some(tcb) = self.tcp_mut(child) {
            tcb.parent = None;
        }
        Ok(Some(child))
    }

    /// The state and error of a connection.
    pub fn tcp_status(&self, id: SocketId) -> Option<(TcpState, Option<TcpError>)> {
        match self.sockets.get(id)?.as_ref()?.kind {
            SocketKind::Listen => Some((TcpState::Listen, None)),
            _ => self.tcp(id).map(|t| (t.state, t.error)),
        }
    }

    /// Queues up to `data.len()` bytes; returns how many fit (0: the send
    /// buffer is full, wait for an event).
    pub fn tcp_send(&mut self, id: SocketId, data: &[u8], now: u64) -> Result<usize, NetError> {
        let tcb = self.tcp_mut(id).ok_or(NetError::BadSocket)?;
        if let Some(error) = tcb.error {
            return Err(error.into());
        }
        if tcb.fin_queued {
            return Err(NetError::Closed);
        }
        if !matches!(
            tcb.state,
            TcpState::SynSent | TcpState::SynReceived | TcpState::Established | TcpState::CloseWait
        ) {
            return Err(NetError::NotConnected);
        }
        let take = data.len().min(TCP_BUFFER - tcb.send.len());
        tcb.send.extend(&data[..take]);
        let mut out = Vec::new();
        tcb.output(now, &mut out);
        self.transmit_segments(id, out, now);
        Ok(take)
    }

    /// Copies received bytes into `buffer`.
    pub fn tcp_recv(
        &mut self,
        id: SocketId,
        buffer: &mut [u8],
        now: u64,
    ) -> Result<Recv, NetError> {
        let tcb = self.tcp_mut(id).ok_or(NetError::BadSocket)?;
        if !tcb.recv.is_empty() {
            let take = buffer.len().min(tcb.recv.len());
            for (slot, byte) in buffer.iter_mut().zip(tcb.recv.drain(..take)) {
                *slot = byte;
            }
            if let Some(update) = tcb.window_update() {
                self.transmit_segments(id, [update].into_iter().collect(), now);
            }
            return Ok(Recv::Data(take));
        }
        if tcb.fin_received {
            return Ok(Recv::Eof);
        }
        if let Some(error) = tcb.error {
            return Err(error.into());
        }
        if tcb.state == TcpState::Closed {
            return Ok(Recv::Eof);
        }
        Ok(Recv::WouldBlock)
    }

    /// Closes our sending side once queued data has gone (FIN).
    pub fn tcp_shutdown(&mut self, id: SocketId, now: u64) -> Result<(), NetError> {
        let tcb = self.tcp_mut(id).ok_or(NetError::BadSocket)?;
        if let Some(error) = tcb.error {
            return Err(error.into());
        }
        tcb.fin_queued = true;
        let mut out = Vec::new();
        tcb.output(now, &mut out);
        self.transmit_segments(id, out, now);
        Ok(())
    }

    /// The user let go of a TCP socket: listeners and unread connections
    /// are reset; others close gracefully in the background.
    pub(crate) fn tcp_release(&mut self, id: SocketId, now: u64) {
        let Some(socket) = self.sockets.get(id).and_then(Option::as_ref) else {
            return;
        };
        if socket.kind == SocketKind::Listen {
            let children: Vec<SocketId> = (0..self.sockets.len())
                .filter(|&c| self.tcp(c).is_some_and(|t| t.parent == Some(id)))
                .collect();
            for child in children {
                self.tcp_reset_and_free(child, now);
            }
            self.free_socket(id);
            return;
        }
        let Some(tcb) = self.tcp_mut(id) else {
            return;
        };
        let unread = !tcb.recv.is_empty();
        match tcb.state {
            TcpState::Closed => self.free_socket(id),
            TcpState::SynSent => self.free_socket(id),
            _ if unread => self.tcp_reset_and_free(id, now),
            TcpState::FinWait2 => {
                tcb.orphan = true;
                tcb.timer = Some(now + ORPHAN_FIN_WAIT_MS);
            }
            _ => {
                tcb.orphan = true;
                tcb.fin_queued = true;
                let mut out = Vec::new();
                tcb.output(now, &mut out);
                self.transmit_segments(id, out, now);
            }
        }
    }

    fn tcp_reset_and_free(&mut self, id: SocketId, now: u64) {
        let mut out = Vec::new();
        if let Some(tcb) = self.tcp_mut(id) {
            tcb.abort(None, &mut out);
        }
        self.transmit_segments(id, out, now);
        self.free_socket(id);
    }

    fn free_socket(&mut self, id: SocketId) {
        if let Some(slot) = self.sockets.get_mut(id) {
            *slot = None;
        }
        self.ready.retain(|&r| r != id);
        for socket in self.sockets.iter_mut().flatten() {
            socket.backlog.retain(|&c| c != id);
        }
    }

    /// Sends a connection's segments.
    fn transmit_segments(&mut self, id: SocketId, segments: Vec<Segment>, now: u64) {
        let Some(tcb) = self.tcp(id) else {
            return;
        };
        let local = (tcb.local, tcb.local_port);
        let remote = (tcb.remote, tcb.remote_port);
        for segment in segments {
            self.send_tcp(local, remote, &segment, now);
        }
    }

    /// Sends one segment from `local` to `remote` (address, port). IPv4
    /// segments go from the current configured address.
    fn send_tcp(
        &mut self,
        (local, local_port): (IpAddr, u16),
        (remote, remote_port): (IpAddr, u16),
        segment: &Segment,
        now: u64,
    ) {
        let local = match local {
            IpAddr::V4(_) => match self.config {
                Some(config) => IpAddr::V4(config.address),
                None => return,
            },
            v6 => v6,
        };
        let options = if segment.mss.is_some() { 4 } else { 0 };
        let header = TCP_HEADER + options;
        let mut bytes = Vec::with_capacity(header + segment.data.len());
        bytes.extend_from_slice(&local_port.to_be_bytes());
        bytes.extend_from_slice(&remote_port.to_be_bytes());
        bytes.extend_from_slice(&segment.seq.to_be_bytes());
        bytes.extend_from_slice(&segment.ack.to_be_bytes());
        bytes.push(((header / 4) as u8) << 4);
        bytes.push(segment.flags);
        bytes.extend_from_slice(&segment.window.to_be_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        if let Some(mss) = segment.mss {
            bytes.extend_from_slice(&[2, 4]);
            bytes.extend_from_slice(&mss.to_be_bytes());
        }
        bytes.extend_from_slice(&segment.data);
        let sum = pseudo_sum(local, remote, PROTOCOL_TCP, bytes.len());
        let sum = checksum_finish(checksum_add(sum, &bytes));
        bytes[16..18].copy_from_slice(&sum.to_be_bytes());
        // A lost segment is retransmitted; nothing else to do on failure.
        let _ = match (local, remote) {
            (IpAddr::V6(local), IpAddr::V6(remote)) => {
                self.send_ip6(local, remote, PROTOCOL_TCP, bytes, now)
            }
            (_, IpAddr::V4(remote)) => self.send_ip(remote, PROTOCOL_TCP, bytes, now),
            _ => Err(NetError::NoRoute),
        };
    }

    fn mark_ready(&mut self, id: SocketId) {
        if !self.ready.contains(&id) {
            self.ready.push(id);
        }
    }

    /// Delivers a received segment (IP layer already checked it is ours).
    pub(crate) fn on_tcp(&mut self, src: IpAddr, dst: IpAddr, bytes: &[u8], now: u64) -> bool {
        if bytes.len() < TCP_HEADER {
            return false;
        }
        let offset = usize::from(bytes[12] >> 4) * 4;
        if offset < TCP_HEADER || offset > bytes.len() {
            return false;
        }
        let sum = pseudo_sum(src, dst, PROTOCOL_TCP, bytes.len());
        if checksum_finish(checksum_add(sum, bytes)) != 0 {
            return false;
        }
        let src_port = be16(bytes, 0);
        let dst_port = be16(bytes, 2);
        let flags = bytes[13] & 0x3f;
        // Options: only MSS is used; malformed lists are ignored.
        let mut mss = None;
        let mut at = TCP_HEADER;
        while at < offset {
            match bytes[at] {
                0 => break,
                1 => at += 1,
                kind => {
                    let Some(&len) = bytes.get(at + 1) else {
                        break;
                    };
                    let len = usize::from(len);
                    if len < 2 || at + len > offset {
                        break;
                    }
                    if kind == 2 && len == 4 {
                        mss = Some(be16(bytes, at + 2));
                    }
                    at += len;
                }
            }
        }
        let segment = Incoming {
            seq: u32::from_be_bytes(bytes[4..8].try_into().expect("4 bytes")),
            ack: u32::from_be_bytes(bytes[8..12].try_into().expect("4 bytes")),
            flags,
            window: be16(bytes, 14),
            mss,
            data: &bytes[offset..],
        };

        let connection = (0..self.sockets.len()).find(|&id| {
            self.tcp(id).is_some_and(|t| {
                t.local_port == dst_port
                    && t.remote == src
                    && t.remote_port == src_port
                    // IPv6 hosts have several addresses; IPv4 follows the
                    // configured one.
                    && (t.local == dst || matches!(t.local, IpAddr::V4(_)))
            })
        });
        if let Some(id) = connection {
            let mut out = Vec::new();
            let tcb = self.tcp_mut(id).expect("found");
            let was = tcb.state;
            let changed = tcb.input(&segment, now, &mut out);
            let (state, parent, orphan) = (tcb.state, tcb.parent, tcb.orphan);
            self.transmit_segments(id, out, now);
            if was == TcpState::SynReceived && state == TcpState::Established {
                if let Some(listener) = parent
                    && let Some(Some(socket)) = self.sockets.get_mut(listener)
                {
                    socket.backlog.push_back(id);
                    self.mark_ready(listener);
                }
            } else if state == TcpState::Closed && parent.is_some() {
                self.free_socket(id); // never accepted
            }
            if orphan && state == TcpState::Closed {
                self.free_socket(id);
            } else if changed {
                self.mark_ready(id);
            }
            return true;
        }

        // A new connection for a listener?
        let listener = (0..self.sockets.len()).find(|&id| {
            self.sockets[id]
                .as_ref()
                .is_some_and(|s| s.kind == SocketKind::Listen && s.port == dst_port)
        });
        if let Some(listener) = listener
            && flags & (SYN | ACK | RST) == SYN
        {
            let children = self
                .sockets
                .iter()
                .flatten()
                .filter_map(|s| s.tcp.as_deref())
                .filter(|t| t.parent == Some(listener))
                .count();
            if children >= BACKLOG {
                return false; // full: the peer retries
            }
            let iss = self.isn(dst_port, src, src_port, now);
            let our_mss = match dst {
                IpAddr::V4(_) => OUR_MSS,
                IpAddr::V6(_) => self.mss6(),
            };
            let mut tcb = Tcb::new(
                (dst, dst_port),
                (src, src_port),
                our_mss,
                iss,
                TcpState::SynReceived,
            );
            tcb.rcv_nxt = segment.seq.wrapping_add(1);
            tcb.mss = mss.unwrap_or(DEFAULT_MSS).clamp(64, our_mss);
            tcb.snd_wnd = u32::from(segment.window);
            tcb.parent = Some(listener);
            let syn_ack = tcb.syn();
            tcb.timer = Some(now + tcb.rto);
            let Ok(id) = self.new_tcp_socket(SocketKind::Tcp, dst_port, Some(tcb)) else {
                return false;
            };
            self.transmit_segments(id, [syn_ack].into_iter().collect(), now);
            return true;
        }

        // Nothing here: reset (never in answer to a reset).
        if flags & RST == 0 {
            let reset = if flags & ACK != 0 {
                Segment {
                    seq: segment.ack,
                    ack: 0,
                    flags: RST,
                    window: 0,
                    mss: None,
                    data: Vec::new(),
                }
            } else {
                let len = segment.data.len() as u32
                    + u32::from(flags & SYN != 0)
                    + u32::from(flags & FIN != 0);
                Segment {
                    seq: 0,
                    ack: segment.seq.wrapping_add(len),
                    flags: RST | ACK,
                    window: 0,
                    mss: None,
                    data: Vec::new(),
                }
            };
            self.send_tcp((dst, dst_port), (src, src_port), &reset, now);
        }
        false
    }

    /// Runs the connections' timers.
    pub(crate) fn poll_tcp(&mut self, now: u64) {
        for id in 0..self.sockets.len() {
            let Some(tcb) = self.tcp_mut(id) else {
                continue;
            };
            if tcb.timer.is_none_or(|t| t > now) {
                continue;
            }
            let mut out = Vec::new();
            tcb.on_timer(now, &mut out);
            let (state, orphan, parent) = (tcb.state, tcb.orphan, tcb.parent);
            self.transmit_segments(id, out, now);
            if state == TcpState::Closed && (orphan || parent.is_some()) {
                self.free_socket(id);
            } else {
                self.mark_ready(id);
            }
        }
    }

    pub(crate) fn tcp_deadline(&self) -> Option<u64> {
        self.sockets
            .iter()
            .flatten()
            .filter_map(|s| s.tcp.as_ref()?.timer)
            .min()
    }
}

impl From<TcpError> for NetError {
    fn from(error: TcpError) -> Self {
        match error {
            TcpError::Refused => NetError::Refused,
            TcpError::Reset => NetError::Reset,
            TcpError::TimedOut => NetError::TimedOut,
        }
    }
}
