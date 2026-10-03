//! IPv6 (RFC 8200), ADR-0043: the network layer, ICMPv6 (RFC 4443),
//! Neighbor Discovery (RFC 4861), stateless address autoconfiguration
//! (RFC 4862) with stable interface identifiers (RFC 7217), RDNSS
//! (RFC 8106) and the MLDv2 reports (RFC 3810) a host sends.
//!
//! - **Addresses.** A link-local address at start, and one per prefix a
//!   router advertises for autoconfiguration. Interface identifiers are
//!   SipHash-2-4 of the prefix and the MAC under a secret key (RFC 7217),
//!   not the MAC itself (EUI-64): addresses do not identify the machine
//!   across networks. Every address goes through duplicate address
//!   detection before use; a duplicate is regenerated (up to
//!   [`IDGEN_RETRIES`] times) with the next DAD counter.
//! - **Neighbor cache.** INCOMPLETE, REACHABLE, STALE, DELAY and PROBE
//!   with RFC 4861's timers; packets wait (bounded) while their next hop is
//!   resolved.
//! - **Routers.** Router solicitations at start; advertisements give
//!   default routers, on-link prefixes, SLAAC prefixes, the link MTU, hop
//!   limit, timers and DNS servers, each with its lifetime.
//! - **Packets.** Hop-by-hop, destination options and routing headers are
//!   processed as RFC 8200 requires; fragments are not reassembled (only
//!   atomic fragments pass); ICMPv6 errors are rate-limited.
//!
//! Everything received is untrusted: lengths are checked before use, ND
//! messages need hop limit 255 (they cannot come from off the link), and
//! all tables are bounded.

use super::*;
use oceans_inet::{is_link_local, is_link_scope_multicast, is_multicast, same_prefix};

pub(crate) const ETHERTYPE_IPV6: u16 = 0x86dd;
pub(crate) const HEADER: usize = 40;
/// Every IPv6 link carries this much (RFC 8200 §5).
pub(crate) const MIN_MTU: usize = 1280;
/// Largest UDP payload in one datagram over an Ethernet link.
pub const MAX_UDP6_PAYLOAD: usize = MTU - HEADER - UDP_HEADER;
/// Largest echo payload over an Ethernet link.
pub const MAX_PING6_PAYLOAD: usize = MTU - HEADER - ICMP_HEADER;

const NEXT_HOP_BY_HOP: u8 = 0;
const NEXT_ROUTING: u8 = 43;
const NEXT_FRAGMENT: u8 = 44;
const NEXT_ICMPV6: u8 = 58;
const NEXT_NONE: u8 = 59;
const NEXT_DESTINATION: u8 = 60;

const ICMP_UNREACHABLE: u8 = 1;
const UNREACHABLE_PORT: u8 = 4;
const ICMP_TOO_BIG: u8 = 2;
const ICMP_TIME_EXCEEDED: u8 = 3;
const ICMP_PARAMETER: u8 = 4;
const PARAMETER_HEADER: u8 = 0;
const PARAMETER_NEXT_HEADER: u8 = 1;
const PARAMETER_OPTION: u8 = 2;
const ICMP_ECHO_REQUEST: u8 = 128;
const ICMP_ECHO_REPLY: u8 = 129;
const MLD_QUERY: u8 = 130;
const ND_ROUTER_SOLICIT: u8 = 133;
const ND_ROUTER_ADVERT: u8 = 134;
const ND_NEIGHBOR_SOLICIT: u8 = 135;
const ND_NEIGHBOR_ADVERT: u8 = 136;
const MLD2_REPORT: u8 = 143;

const OPTION_SOURCE_MAC: u8 = 1;
const OPTION_TARGET_MAC: u8 = 2;
const OPTION_PREFIX: u8 = 3;
const OPTION_MTU: u8 = 5;
const OPTION_RDNSS: u8 = 25;

const NA_ROUTER: u8 = 0x80;
const NA_SOLICITED: u8 = 0x40;
const NA_OVERRIDE: u8 = 0x20;
const PREFIX_ON_LINK: u8 = 0x80;
const PREFIX_AUTONOMOUS: u8 = 0x40;

/// MLDv2 record types (RFC 3810 §5.2.12).
const MODE_IS_EXCLUDE: u8 = 2;
const CHANGE_TO_INCLUDE: u8 = 3;
const CHANGE_TO_EXCLUDE: u8 = 4;

/// RFC 4861 §10 protocol constants (milliseconds).
const MAX_RTR_SOLICITATION_DELAY_MS: u64 = 1_000;
const RTR_SOLICITATION_INTERVAL_MS: u64 = 4_000;
const MAX_RTR_SOLICITATIONS: u32 = 3;
const MAX_MULTICAST_SOLICIT: u8 = 3;
const MAX_UNICAST_SOLICIT: u8 = 3;
const REACHABLE_TIME_MS: u64 = 30_000;
const RETRANS_TIMER_MS: u64 = 1_000;
const DELAY_FIRST_PROBE_TIME_MS: u64 = 5_000;
/// Bounds for advertised timers: a hostile router cannot make us probe in
/// a tight loop or never.
const MIN_RETRANS_MS: u64 = 100;
const MAX_TIMER_MS: u64 = 3_600_000;
/// RFC 4862: one DAD probe; RFC 7217 §6: regenerate a duplicate this often.
const DAD_TRANSMITS: u8 = 1;
pub const IDGEN_RETRIES: u8 = 3;
/// RFC 4862 §5.5.3 (e): lifetimes are not cut below two hours by
/// unauthenticated advertisements.
const TWO_HOURS_MS: u64 = 2 * 3_600_000;
/// Lifetimes in advertisements: all ones is infinity.
const INFINITE: u32 = u32::MAX;
/// MLDv2: unsolicited reports are sent this often (the robustness
/// variable), this far apart at most.
const MLD_ROBUSTNESS: u8 = 2;
const MLD_UNSOLICITED_INTERVAL_MS: u64 = 1_000;
/// ICMPv6 error rate limit (RFC 4443 §2.4 f): a token bucket.
const ERROR_BURST: u32 = 10;
const ERROR_REFILL_MS: u64 = 100;
/// Learned path MTUs are forgotten after this (RFC 8201 §4).
const PMTU_LIFETIME_MS: u64 = 600_000;

/// Table bounds.
const MAX_ADDRESSES: usize = 8;
const MAX_ROUTERS: usize = 4;
const MAX_PREFIXES: usize = 8;
const MAX_NEIGHBORS: usize = 32;
const MAX_PMTU: usize = 16;
/// Packets held per unresolved neighbor, and in all.
const NEIGHBOR_QUEUE: usize = 4;
const WAITING_TOTAL: usize = 32;

const ALL_NODES: Ipv6 = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const ALL_ROUTERS: Ipv6 = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
const ALL_MLDV2_ROUTERS: Ipv6 = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16];
const LINK_LOCAL_PREFIX: Ipv6 = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const UNSPECIFIED6: Ipv6 = [0; 16];

/// The solicited-node multicast group of `address` (RFC 4291 §2.7.1).
pub fn solicited_node(address: &Ipv6) -> Ipv6 {
    let mut group = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff, 0, 0, 0];
    group[13..].copy_from_slice(&address[13..]);
    group
}

/// The Ethernet address of an IPv6 multicast group (RFC 2464 §7).
pub fn multicast_mac(group: &Ipv6) -> Mac {
    [0x33, 0x33, group[12], group[13], group[14], group[15]]
}

fn is_solicited_node(address: &Ipv6) -> bool {
    address[..13] == [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff]
}

/// An address lifetime from an advertisement: `None` is infinite.
fn lifetime(now: u64, seconds: u32) -> Option<u64> {
    (seconds != INFINITE).then(|| now + u64::from(seconds) * 1000)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressState {
    /// Duplicate address detection is running: not usable yet.
    Tentative,
    Preferred,
    /// Its preferred lifetime ended: kept for existing traffic, not chosen
    /// for new.
    Deprecated,
    /// Another node uses it (and no more retries are left).
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressOrigin {
    LinkLocal,
    /// Stateless autoconfiguration from a router's prefix.
    Slaac,
}

/// One of our addresses, for reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address6 {
    pub address: Ipv6,
    pub prefix: u8,
    pub state: AddressState,
    pub origin: AddressOrigin,
}

/// The IPv6 configuration, for reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv6Config {
    pub addresses: Vec<Address6>,
    /// The default router in use.
    pub router: Option<Ipv6>,
    /// The DNS server a router advertised (RDNSS).
    pub dns: Option<Ipv6>,
    pub mtu: u16,
    pub hop_limit: u8,
}

/// RFC 4861 §7.3.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeighborState {
    /// Address resolution is running.
    Incomplete,
    /// Confirmed recently.
    Reachable,
    /// Not confirmed lately; checked when next used.
    Stale,
    /// Used while stale: waiting for upper layers before probing.
    Delay,
    /// Being probed with unicast solicitations.
    Probe,
}

struct Address {
    address: Ipv6,
    prefix: u8,
    state: AddressState,
    origin: AddressOrigin,
    /// RFC 7217's DAD_Counter: which attempt made this address.
    dad_counter: u8,
    /// Tentative: probes still to send; the next is due (or DAD ends) at
    /// `dad_at`.
    dad_probes: u8,
    dad_at: u64,
    preferred_until: Option<u64>,
    valid_until: Option<u64>,
}

impl Address {
    fn usable(&self) -> bool {
        matches!(
            self.state,
            AddressState::Preferred | AddressState::Deprecated
        )
    }

    /// Whether it belongs to a multicast group (all but duplicates).
    fn joined(&self) -> bool {
        self.state != AddressState::Duplicate
    }
}

struct Router {
    address: Ipv6,
    expires: u64,
}

struct Prefix {
    prefix: Ipv6,
    len: u8,
    expires: Option<u64>,
}

struct Neighbor {
    address: Ipv6,
    mac: Option<Mac>,
    state: NeighborState,
    timer: Option<u64>,
    probes: u8,
    is_router: bool,
    /// The source of the packet that started resolution: our solicitations
    /// come from it (RFC 4861 §7.2.2).
    source: Ipv6,
    /// Packets (IPv6, without Ethernet header) waiting for resolution.
    queue: Vec<Vec<u8>>,
    last_used: u64,
}

struct PathMtu {
    destination: Ipv6,
    mtu: u16,
    expires: u64,
}

/// IPv6 state, created by [`Stack::enable_ipv6`].
pub(crate) struct V6 {
    key: [u8; 16],
    addresses: Vec<Address>,
    routers: Vec<Router>,
    prefixes: Vec<Prefix>,
    dns: Option<(Ipv6, Option<u64>)>,
    neighbors: Vec<Neighbor>,
    pmtu: Vec<PathMtu>,
    hop_limit: u8,
    mtu: u16,
    reachable_ms: u64,
    retrans_ms: u64,
    rs_sent: u32,
    rs_at: Option<u64>,
    /// MLDv2: when to send a report, and how many unsolicited ones remain.
    mld_at: Option<u64>,
    mld_unsolicited: u8,
    /// A state-change report is pending (else a current-state one).
    mld_change: bool,
    error_tokens: u32,
    error_refilled: u64,
    rng: u64,
}

impl V6 {
    fn random(&mut self) -> u64 {
        // splitmix64 over a state seeded from the key.
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.rng;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    /// RFC 4861 §6.3.2: ReachableTime is the base, scaled by a random
    /// factor in [0.5, 1.5).
    fn randomize_reachable(&mut self, base: u64) {
        self.reachable_ms = base / 2 + self.random() % base.max(1);
    }

    fn address_index(&self, address: &Ipv6) -> Option<usize> {
        self.addresses.iter().position(|a| a.address == *address)
    }

    fn is_ours(&self, address: &Ipv6) -> bool {
        self.addresses
            .iter()
            .any(|a| a.address == *address && a.usable())
    }

    /// The multicast groups we belong to (besides all-nodes): one
    /// solicited-node group per address.
    fn in_group(&self, group: &Ipv6) -> bool {
        *group == ALL_NODES
            || (is_solicited_node(group)
                && self
                    .addresses
                    .iter()
                    .any(|a| a.joined() && solicited_node(&a.address) == *group))
    }

    pub(crate) fn wants_mac(&self, mac: &Mac) -> bool {
        if mac[..2] != [0x33, 0x33] {
            return false;
        }
        mac[2..] == [0, 0, 0, 1]
            || (mac[2] == 0xff
                && self
                    .addresses
                    .iter()
                    .any(|a| a.joined() && a.address[13..] == mac[3..]))
    }

    /// RFC 6724 source address selection, simplified for one interface:
    /// link-local for link-scoped destinations; otherwise a preferred
    /// address before a deprecated one, then the longest matching prefix.
    pub(crate) fn select_source(&self, dst: &Ipv6) -> Option<Ipv6> {
        let link_scoped = is_link_local(dst) || is_link_scope_multicast(dst);
        self.addresses
            .iter()
            .filter(|a| a.usable() && (a.origin == AddressOrigin::LinkLocal) == link_scoped)
            .max_by_key(|a| {
                (
                    a.state == AddressState::Preferred,
                    oceans_inet::common_prefix_len(&a.address, dst),
                )
            })
            .map(|a| a.address)
    }

    fn on_link(&self, dst: &Ipv6) -> bool {
        is_link_local(dst)
            || self
                .prefixes
                .iter()
                .any(|p| same_prefix(&p.prefix, dst, p.len))
    }

    /// The default router: one believed reachable if possible
    /// (RFC 4861 §6.3.6).
    fn default_router(&self) -> Option<Ipv6> {
        let reachable = self.routers.iter().find(|r| {
            self.neighbors.iter().any(|n| {
                n.address == r.address
                    && !matches!(n.state, NeighborState::Incomplete | NeighborState::Probe)
            })
        });
        reachable.or(self.routers.first()).map(|r| r.address)
    }

    /// The next hop toward `dst`.
    fn next_hop(&self, dst: &Ipv6) -> Result<Ipv6, NetError> {
        if self.on_link(dst) {
            return Ok(*dst);
        }
        self.default_router().ok_or(NetError::NoRoute)
    }

    fn path_mtu(&self, dst: &Ipv6) -> usize {
        self.pmtu
            .iter()
            .find(|p| p.destination == *dst)
            .map_or(usize::from(self.mtu), |p| usize::from(p.mtu))
    }

    /// When [`Stack::poll_v6`] next has work.
    pub(crate) fn deadline(&self) -> Option<u64> {
        let addresses = self.addresses.iter().flat_map(|a| {
            let dad = (a.state == AddressState::Tentative).then_some(a.dad_at);
            let preferred = a
                .preferred_until
                .filter(|_| a.state == AddressState::Preferred);
            [dad, preferred, a.valid_until]
        });
        let routers = self.routers.iter().map(|r| Some(r.expires));
        let prefixes = self.prefixes.iter().map(|p| p.expires);
        let neighbors = self.neighbors.iter().map(|n| n.timer);
        let pmtu = self.pmtu.iter().map(|p| Some(p.expires));
        let dns = self.dns.and_then(|(_, expires)| expires);
        addresses
            .chain(routers)
            .chain(prefixes)
            .chain(neighbors)
            .chain(pmtu)
            .chain([self.rs_at, self.mld_at, dns])
            .flatten()
            .min()
    }

    /// RFC 7217 interface identifier for `prefix`: the first 64 bits of
    /// SipHash-2-4(prefix | MAC | DAD counter) under the secret key, never
    /// a reserved identifier (RFC 5453).
    fn interface_id(&self, prefix: &Ipv6, mac: &Mac, mut counter: u8) -> [u8; 8] {
        loop {
            let mut input = [0u8; 15];
            input[..8].copy_from_slice(&prefix[..8]);
            input[8..14].copy_from_slice(mac);
            input[14] = counter;
            let id = crate::siphash::siphash24(&self.key, &input);
            // Subnet-router anycast (0) and the reserved anycast range.
            if id != 0 && id < 0xfdff_ffff_ffff_ff80 {
                return id.to_be_bytes();
            }
            counter = counter.wrapping_add(1);
        }
    }

    /// Adds a tentative address; DAD starts at `dad_at`.
    #[expect(clippy::too_many_arguments, reason = "an address and its lifetimes")]
    fn add_address(
        &mut self,
        mac: &Mac,
        prefix: &Ipv6,
        origin: AddressOrigin,
        dad_counter: u8,
        dad_at: u64,
        preferred_until: Option<u64>,
        valid_until: Option<u64>,
    ) -> bool {
        if self.addresses.len() >= MAX_ADDRESSES {
            return false;
        }
        let mut address = *prefix;
        address[8..].copy_from_slice(&self.interface_id(prefix, mac, dad_counter));
        if self.address_index(&address).is_some() {
            return false;
        }
        self.addresses.push(Address {
            address,
            prefix: 64,
            state: AddressState::Tentative,
            origin,
            dad_counter,
            dad_probes: DAD_TRANSMITS,
            dad_at,
            preferred_until,
            valid_until,
        });
        // Join its solicited-node group before probing (RFC 4862 §5.4.2).
        self.mld_change = true;
        self.mld_unsolicited = MLD_ROBUSTNESS;
        self.mld_at = Some(self.mld_at.map_or(dad_at, |at| at.min(dad_at)));
        true
    }

    fn neighbor(&self, address: &Ipv6) -> Option<usize> {
        self.neighbors.iter().position(|n| n.address == *address)
    }

    /// Room for a new neighbor entry: evicts the least recently used one
    /// that is not being resolved.
    fn make_room(&mut self, stats: &mut Stats) -> bool {
        if self.neighbors.len() < MAX_NEIGHBORS {
            return true;
        }
        let victim = (0..self.neighbors.len())
            .filter(|&i| self.neighbors[i].state != NeighborState::Incomplete)
            .min_by_key(|&i| self.neighbors[i].last_used);
        match victim {
            Some(index) => {
                let gone = self.neighbors.swap_remove(index);
                stats.dropped += gone.queue.len() as u64;
                true
            }
            None => false,
        }
    }

    fn info(&self) -> Ipv6Config {
        Ipv6Config {
            addresses: self
                .addresses
                .iter()
                .map(|a| Address6 {
                    address: a.address,
                    prefix: a.prefix,
                    state: a.state,
                    origin: a.origin,
                })
                .collect(),
            router: self.default_router(),
            dns: self.dns.map(|(address, _)| address),
            mtu: self.mtu,
            hop_limit: self.hop_limit,
        }
    }
}

/// The ND options of a message, validated (RFC 4861 §4.6: every option has
/// a nonzero length and fits).
fn nd_options(options: &[u8]) -> Option<impl Iterator<Item = (u8, &[u8])>> {
    let mut at = 0;
    while at < options.len() {
        let len = usize::from(*options.get(at + 1)?) * 8;
        if len == 0 || at + len > options.len() {
            return None;
        }
        at += len;
    }
    let mut at = 0;
    Some(core::iter::from_fn(move || {
        let kind = *options.get(at)?;
        let len = usize::from(options[at + 1]) * 8;
        let option = &options[at..at + len];
        at += len;
        Some((kind, option))
    }))
}

/// A link-layer address option's MAC (Ethernet: one 8-byte unit).
fn option_mac(option: &[u8]) -> Option<Mac> {
    option.get(2..8)?.try_into().ok()
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn ipv6_at(bytes: &[u8], at: usize) -> Ipv6 {
    bytes[at..at + 16].try_into().expect("16 bytes")
}

/// An IPv6 packet: header and payload.
fn packet(src: &Ipv6, dst: &Ipv6, next_header: u8, hop_limit: u8, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER + payload.len());
    packet.extend_from_slice(&[0x60, 0, 0, 0]);
    packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    packet.extend_from_slice(&[next_header, hop_limit]);
    packet.extend_from_slice(src);
    packet.extend_from_slice(dst);
    packet.extend_from_slice(payload);
    packet
}

/// Fills in an ICMPv6 message's checksum.
fn icmp6_checksum(src: &Ipv6, dst: &Ipv6, message: &mut [u8]) {
    message[2..4].copy_from_slice(&[0, 0]);
    let sum = pseudo_sum(
        IpAddr::V6(*src),
        IpAddr::V6(*dst),
        NEXT_ICMPV6,
        message.len(),
    );
    let sum = checksum_finish(checksum_add(sum, message));
    message[2..4].copy_from_slice(&sum.to_be_bytes());
}

/// Where a received packet was addressed, among the addresses we answer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum To {
    /// One of our usable addresses.
    Unicast,
    /// One of our tentative addresses: only DAD messages count.
    Tentative,
    /// A group we belong to.
    Multicast,
}

/// What the extension headers left: the upper layer and where it starts,
/// or why the packet is dropped.
enum Walk {
    Upper {
        protocol: u8,
        at: usize,
    },
    /// Processed completely (No Next Header), nothing more to do.
    Done,
    Drop,
    /// Dropped with a Parameter Problem (code, pointer) to the source.
    Problem(u8, u32),
}

/// Walks the extension headers (RFC 8200 §4).
fn walk_headers(packet: &[u8], multicast: bool) -> Walk {
    let mut next = packet[6];
    let mut next_field = 6;
    let mut at = HEADER;
    loop {
        match next {
            NEXT_HOP_BY_HOP | NEXT_DESTINATION | NEXT_ROUTING => {
                // Hop-by-hop options must come first.
                if next == NEXT_HOP_BY_HOP && at != HEADER {
                    return Walk::Problem(PARAMETER_NEXT_HEADER, next_field as u32);
                }
                let Some(&length) = packet.get(at + 1) else {
                    return Walk::Drop;
                };
                let len = (usize::from(length) + 1) * 8;
                if at + len > packet.len() {
                    return Walk::Drop;
                }
                if next == NEXT_ROUTING {
                    // We are not a router: a header still naming hops is
                    // refused (and type 0 is deprecated, RFC 5095).
                    if packet[at + 3] != 0 {
                        return Walk::Problem(PARAMETER_HEADER, (at + 2) as u32);
                    }
                } else if let Some(walk) = options(packet, at + 2, at + len, multicast) {
                    return walk;
                }
                next_field = at;
                next = packet[at];
                at += len;
            }
            NEXT_FRAGMENT => {
                if at + 8 > packet.len() {
                    return Walk::Drop;
                }
                // No reassembly: only atomic fragments (offset 0, no more
                // fragments) are whole packets (RFC 6946).
                if be16(packet, at + 2) & 0xfff9 != 0 {
                    return Walk::Drop;
                }
                next_field = at;
                next = packet[at];
                at += 8;
            }
            NEXT_NONE => return Walk::Done,
            NEXT_ICMPV6 | PROTOCOL_UDP | tcp::PROTOCOL_TCP => {
                return Walk::Upper { protocol: next, at };
            }
            _ => return Walk::Problem(PARAMETER_NEXT_HEADER, next_field as u32),
        }
    }
}

/// Processes the options in `packet[from..to]`: `Some` if they drop the
/// packet (RFC 8200 §4.2, by the option type's two high bits).
fn options(packet: &[u8], from: usize, to: usize, multicast: bool) -> Option<Walk> {
    let mut at = from;
    while at < to {
        let kind = packet[at];
        if kind == 0 {
            at += 1; // Pad1
            continue;
        }
        let Some(&len) = packet.get(at + 1) else {
            return Some(Walk::Drop);
        };
        let len = usize::from(len);
        if at + 2 + len > to {
            return Some(Walk::Drop);
        }
        // PadN, and Router Alert (MLD in hop-by-hop): nothing to do.
        if !matches!(kind, 1 | 5) {
            match kind >> 6 {
                0 => {}
                1 => return Some(Walk::Drop),
                2 => return Some(Walk::Problem(PARAMETER_OPTION, at as u32)),
                _ if multicast => return Some(Walk::Drop),
                _ => return Some(Walk::Problem(PARAMETER_OPTION, at as u32)),
            }
        }
        at += 2 + len;
    }
    None
}

impl Stack {
    /// Starts IPv6 on the interface: a link-local address (after DAD),
    /// then router solicitation and autoconfiguration. `key` keys the
    /// interface identifiers (RFC 7217); give it 128 unpredictable bits.
    pub fn enable_ipv6(&mut self, key: [u8; 16], now: u64) {
        if self.v6.is_some() {
            return;
        }
        let mut v6 = Box::new(V6 {
            key,
            addresses: Vec::new(),
            routers: Vec::new(),
            prefixes: Vec::new(),
            dns: None,
            neighbors: Vec::new(),
            pmtu: Vec::new(),
            hop_limit: TTL,
            mtu: MTU as u16,
            reachable_ms: REACHABLE_TIME_MS,
            retrans_ms: RETRANS_TIMER_MS,
            rs_sent: 0,
            rs_at: None,
            mld_at: None,
            mld_unsolicited: 0,
            mld_change: false,
            error_tokens: ERROR_BURST,
            error_refilled: now,
            rng: u64::from_le_bytes(key[..8].try_into().expect("8 bytes"))
                ^ u64::from_le_bytes(key[8..].try_into().expect("8 bytes")).rotate_left(17),
        });
        v6.randomize_reachable(REACHABLE_TIME_MS);
        // A random delay before the first probe (RFC 4862 §5.4.2), so
        // machines starting together do not collide.
        let delay = v6.random() % MAX_RTR_SOLICITATION_DELAY_MS;
        let mac = self.mac;
        v6.add_address(
            &mac,
            &LINK_LOCAL_PREFIX,
            AddressOrigin::LinkLocal,
            0,
            now + delay,
            None,
            None,
        );
        v6.mld_at = Some(now);
        self.v6 = Some(v6);
    }

    /// The IPv6 configuration, if IPv6 is enabled.
    pub fn ipv6_config(&self) -> Option<Ipv6Config> {
        self.v6.as_ref().map(|v6| v6.info())
    }

    /// A neighbor cache entry's state and MAC, for diagnostics and tests.
    pub fn neighbor_state(&self, address: &Ipv6) -> Option<(NeighborState, Option<Mac>)> {
        let v6 = self.v6.as_ref()?;
        let n = &v6.neighbors[v6.neighbor(address)?];
        Some((n.state, n.mac))
    }

    /// Runs `f` with the IPv6 state taken out of the stack, so both can be
    /// borrowed. `f` must not reach code that needs `self.v6`.
    fn with_v6<R>(&mut self, f: impl FnOnce(&mut Self, &mut V6) -> R) -> Option<R> {
        let mut v6 = self.v6.take()?;
        let result = f(self, &mut v6);
        self.v6 = Some(v6);
        Some(result)
    }

    /// TCP's MSS over IPv6: what the link MTU carries.
    pub(crate) fn mss6(&self) -> u16 {
        let mtu = self.v6.as_ref().map_or(MIN_MTU, |v6| usize::from(v6.mtu));
        (mtu - HEADER - 20) as u16
    }

    /// Source address and path MTU for sending to `dst`, if it can be
    /// reached at all.
    pub(crate) fn route6(&self, dst: &Ipv6) -> Result<(Ipv6, usize), NetError> {
        let v6 = self.v6.as_ref().ok_or(NetError::NotConfigured)?;
        let source = v6.select_source(dst).ok_or(NetError::NotConfigured)?;
        if !is_multicast(dst) {
            v6.next_hop(dst)?;
        }
        Ok((source, v6.path_mtu(dst)))
    }

    /// UDP datagrams and echo requests to an IPv6 destination.
    pub(crate) fn send_to6(
        &mut self,
        id: SocketId,
        dst: Ipv6,
        port: u16,
        data: &[u8],
        now: u64,
    ) -> Result<(), NetError> {
        let (src, _) = self.route6(&dst)?;
        let socket = self
            .sockets
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or(NetError::BadSocket)?;
        let (next_header, message) = match socket.kind {
            SocketKind::Udp => {
                if data.len() > MAX_UDP6_PAYLOAD {
                    return Err(NetError::TooLarge);
                }
                let segment = udp_segment(src, dst, socket.port, port, data);
                (PROTOCOL_UDP, segment)
            }
            SocketKind::Ping => {
                if data.len() > MAX_PING6_PAYLOAD {
                    return Err(NetError::TooLarge);
                }
                socket.sequence = socket.sequence.wrapping_add(1);
                let mut message = icmp_echo(ICMP_ECHO_REQUEST, socket.port, socket.sequence, data);
                icmp6_checksum(&src, &dst, &mut message);
                (NEXT_ICMPV6, message)
            }
            SocketKind::Tcp | SocketKind::Listen => return Err(NetError::BadSocket),
        };
        self.send_ip6(src, dst, next_header, message, now)
    }

    /// Sends an upper-layer payload over IPv6 (used by TCP and UDP).
    pub(crate) fn send_ip6(
        &mut self,
        src: Ipv6,
        dst: Ipv6,
        next_header: u8,
        payload: Vec<u8>,
        now: u64,
    ) -> Result<(), NetError> {
        self.with_v6(|stack, v6| {
            let hop_limit = if is_link_scope_multicast(&dst) {
                1
            } else {
                v6.hop_limit
            };
            let packet = packet(&src, &dst, next_header, hop_limit, &payload);
            stack.route_packet(v6, &dst, packet, now)
        })
        .unwrap_or(Err(NetError::NotConfigured))
    }

    /// Puts an IPv6 packet on the link toward `dst`: multicast directly,
    /// unicast through the neighbor cache (resolving the next hop first if
    /// needed).
    fn route_packet(
        &mut self,
        v6: &mut V6,
        dst: &Ipv6,
        packet: Vec<u8>,
        now: u64,
    ) -> Result<(), NetError> {
        if is_multicast(dst) {
            return self.emit(multicast_mac(dst), ETHERTYPE_IPV6, &packet);
        }
        let hop = v6.next_hop(dst)?;
        self.send_to_neighbor(v6, hop, packet, now)
    }

    /// Sends a packet to the on-link neighbor `hop` through the neighbor
    /// cache, starting address resolution if it is unknown.
    fn send_to_neighbor(
        &mut self,
        v6: &mut V6,
        hop: Ipv6,
        packet: Vec<u8>,
        now: u64,
    ) -> Result<(), NetError> {
        let index = match v6.neighbor(&hop) {
            Some(index) => index,
            None => {
                let waiting: usize = v6.neighbors.iter().map(|n| n.queue.len()).sum();
                if waiting >= WAITING_TOTAL || !v6.make_room(&mut self.stats) {
                    self.stats.dropped += 1;
                    return Err(NetError::NoBuffers);
                }
                let source = ipv6_at(&packet, 8);
                v6.neighbors.push(Neighbor {
                    address: hop,
                    mac: None,
                    state: NeighborState::Incomplete,
                    timer: Some(now + v6.retrans_ms),
                    probes: 1,
                    is_router: false,
                    source,
                    queue: vec![packet],
                    last_used: now,
                });
                self.send_solicitation(&hop, &source, None);
                return Ok(());
            }
        };
        let delay = DELAY_FIRST_PROBE_TIME_MS;
        let neighbor = &mut v6.neighbors[index];
        neighbor.last_used = now;
        match (neighbor.state, neighbor.mac) {
            (NeighborState::Incomplete, _) | (_, None) => {
                if neighbor.queue.len() >= NEIGHBOR_QUEUE {
                    self.stats.dropped += 1;
                    return Err(NetError::NoBuffers);
                }
                neighbor.queue.push(packet);
                Ok(())
            }
            (state, Some(mac)) => {
                if state == NeighborState::Stale {
                    neighbor.state = NeighborState::Delay;
                    neighbor.timer = Some(now + delay);
                }
                self.emit(mac, ETHERTYPE_IPV6, &packet)
            }
        }
    }

    /// A neighbor solicitation for `target`: multicast to its
    /// solicited-node group (resolution, DAD) or unicast to `mac` (probes).
    /// From `source`; from `::` (DAD) it carries no link-layer address.
    fn send_solicitation(&mut self, target: &Ipv6, source: &Ipv6, mac: Option<Mac>) {
        let mut message = vec![ND_NEIGHBOR_SOLICIT, 0, 0, 0, 0, 0, 0, 0];
        message.extend_from_slice(target);
        if *source != UNSPECIFIED6 {
            message.extend_from_slice(&[OPTION_SOURCE_MAC, 1]);
            message.extend_from_slice(&self.mac);
        }
        let dst = match mac {
            Some(_) => *target,
            None => solicited_node(target),
        };
        icmp6_checksum(source, &dst, &mut message);
        let packet = packet(source, &dst, NEXT_ICMPV6, 255, &message);
        let _ = self.emit(
            mac.unwrap_or_else(|| multicast_mac(&dst)),
            ETHERTYPE_IPV6,
            &packet,
        );
    }

    fn send_router_solicitation(&mut self, v6: &V6) {
        // From the link-local address once it is usable, else from `::`
        // without our link-layer address (RFC 4861 §4.1).
        let source = v6
            .addresses
            .iter()
            .find(|a| a.origin == AddressOrigin::LinkLocal && a.usable())
            .map_or(UNSPECIFIED6, |a| a.address);
        let mut message = vec![ND_ROUTER_SOLICIT, 0, 0, 0, 0, 0, 0, 0];
        if source != UNSPECIFIED6 {
            message.extend_from_slice(&[OPTION_SOURCE_MAC, 1]);
            message.extend_from_slice(&self.mac);
        }
        icmp6_checksum(&source, &ALL_ROUTERS, &mut message);
        let packet = packet(&source, &ALL_ROUTERS, NEXT_ICMPV6, 255, &message);
        let _ = self.emit(multicast_mac(&ALL_ROUTERS), ETHERTYPE_IPV6, &packet);
    }

    /// An MLDv2 report (RFC 3810 §5.2) for our solicited-node groups:
    /// state-change records after joining, current-state ones for queries.
    /// `leave` lists groups we just left.
    fn send_mld_report(&mut self, v6: &V6, change: bool, leave: &[Ipv6]) {
        let mut groups: Vec<Ipv6> = Vec::new();
        for address in v6.addresses.iter().filter(|a| a.joined()) {
            let group = solicited_node(&address.address);
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
        let record = if change {
            CHANGE_TO_EXCLUDE
        } else {
            MODE_IS_EXCLUDE
        };
        let mut message = vec![MLD2_REPORT, 0, 0, 0, 0, 0];
        let count = groups.len() + leave.len();
        message.extend_from_slice(&(count as u16).to_be_bytes());
        for (kind, group) in groups
            .iter()
            .map(|g| (record, g))
            .chain(leave.iter().map(|g| (CHANGE_TO_INCLUDE, g)))
        {
            message.extend_from_slice(&[kind, 0, 0, 0]);
            message.extend_from_slice(group);
        }
        if count == 0 {
            return;
        }
        // From the link-local address, or `::` while it is tentative
        // (RFC 3810 §5.2.13).
        let source = v6
            .addresses
            .iter()
            .find(|a| a.origin == AddressOrigin::LinkLocal && a.usable())
            .map_or(UNSPECIFIED6, |a| a.address);
        icmp6_checksum(&source, &ALL_MLDV2_ROUTERS, &mut message);
        // Hop-by-hop header with Router Alert (MLD), padded to 8 bytes.
        let mut payload = vec![NEXT_ICMPV6, 0, 5, 2, 0, 0, 1, 0];
        payload.extend_from_slice(&message);
        let packet = packet(&source, &ALL_MLDV2_ROUTERS, NEXT_HOP_BY_HOP, 1, &payload);
        let _ = self.emit(multicast_mac(&ALL_MLDV2_ROUTERS), ETHERTYPE_IPV6, &packet);
    }

    /// Answers a neighbor solicitation for our `target`: to the solicitor
    /// (at `mac` if it said, which it is on-link by definition), or to all
    /// nodes if it was probing from `::`.
    fn send_advertisement(
        &mut self,
        v6: &mut V6,
        target: &Ipv6,
        to: Option<(Ipv6, Option<Mac>)>,
        now: u64,
    ) {
        let solicited = if to.is_some() { NA_SOLICITED } else { 0 };
        let dst = to.map_or(ALL_NODES, |(address, _)| address);
        let mut message = vec![
            ND_NEIGHBOR_ADVERT,
            0,
            0,
            0,
            solicited | NA_OVERRIDE,
            0,
            0,
            0,
        ];
        message.extend_from_slice(target);
        message.extend_from_slice(&[OPTION_TARGET_MAC, 1]);
        message.extend_from_slice(&self.mac);
        icmp6_checksum(target, &dst, &mut message);
        let packet = packet(target, &dst, NEXT_ICMPV6, 255, &message);
        // A lost advertisement is asked for again.
        let _ = match to {
            None => self.emit(multicast_mac(&ALL_NODES), ETHERTYPE_IPV6, &packet),
            Some((_, Some(mac))) => self.emit(mac, ETHERTYPE_IPV6, &packet),
            Some((address, None)) => self.send_to_neighbor(v6, address, packet, now),
        };
    }

    /// An ICMPv6 error about `offending` (RFC 4443 §2.4): never about an
    /// error, a packet from `::` or a multicast source, or (except option
    /// problems) one sent to a multicast group; rate-limited.
    fn send_icmp6_error(
        &mut self,
        v6: &mut V6,
        (kind, code, parameter): (u8, u8, u32),
        offending: &[u8],
        now: u64,
    ) {
        let src = ipv6_at(offending, 8);
        let dst = ipv6_at(offending, 24);
        let to_group = is_multicast(&dst);
        if src == UNSPECIFIED6
            || is_multicast(&src)
            || (to_group && !(kind == ICMP_PARAMETER && code == PARAMETER_OPTION))
        {
            return;
        }
        let refills = (now.saturating_sub(v6.error_refilled) / ERROR_REFILL_MS) as u32;
        if refills > 0 {
            v6.error_tokens = (v6.error_tokens + refills).min(ERROR_BURST);
            v6.error_refilled = now;
        }
        if v6.error_tokens == 0 {
            return;
        }
        v6.error_tokens -= 1;
        let from = if v6.is_ours(&dst) {
            dst
        } else {
            match v6.select_source(&src) {
                Some(from) => from,
                None => return,
            }
        };
        let mut message = vec![kind, code, 0, 0];
        message.extend_from_slice(&parameter.to_be_bytes());
        // As much of the packet as fits in the minimum MTU.
        let room = MIN_MTU - HEADER - ICMP_HEADER;
        message.extend_from_slice(&offending[..offending.len().min(room)]);
        icmp6_checksum(&from, &src, &mut message);
        let packet = packet(&from, &src, NEXT_ICMPV6, v6.hop_limit, &message);
        let _ = self.route_packet(v6, &src, packet, now);
    }

    // ---- Receiving ---------------------------------------------------------

    pub(crate) fn on_ipv6(&mut self, packet: &[u8], now: u64) -> bool {
        let Some(v6) = self.v6.as_ref() else {
            return false;
        };
        if packet.len() < HEADER || packet[0] >> 4 != 6 {
            return false;
        }
        let total = HEADER + usize::from(be16(packet, 4));
        if total > packet.len() {
            return false;
        }
        let packet = &packet[..total];
        let src = ipv6_at(packet, 8);
        let dst = ipv6_at(packet, 24);
        if is_multicast(&src) {
            return false;
        }
        let to = match v6.address_index(&dst).map(|i| &v6.addresses[i]) {
            Some(address) if address.usable() => To::Unicast,
            Some(address) if address.state == AddressState::Tentative => To::Tentative,
            Some(_) => return false,
            None if v6.in_group(&dst) => To::Multicast,
            None => return false,
        };
        let (protocol, at) = match walk_headers(packet, to == To::Multicast) {
            Walk::Upper { protocol, at } => (protocol, at),
            Walk::Done => return true,
            Walk::Drop => return false,
            Walk::Problem(code, pointer) => {
                if to == To::Unicast || code == PARAMETER_OPTION {
                    self.with_v6(|stack, v6| {
                        stack.send_icmp6_error(v6, (ICMP_PARAMETER, code, pointer), packet, now);
                    });
                }
                return false;
            }
        };
        let payload = &packet[at..];
        match protocol {
            NEXT_ICMPV6 => self
                .with_v6(|stack, v6| stack.on_icmp6(v6, packet, payload, to, now))
                .unwrap_or(false),
            PROTOCOL_UDP if to == To::Unicast => self
                .with_v6(|stack, v6| stack.on_udp6(v6, packet, payload, now))
                .unwrap_or(false),
            tcp::PROTOCOL_TCP if to == To::Unicast => {
                self.on_tcp(IpAddr::V6(src), IpAddr::V6(dst), payload, now)
            }
            _ => false,
        }
    }

    fn on_udp6(&mut self, v6: &mut V6, packet: &[u8], segment: &[u8], now: u64) -> bool {
        if segment.len() < UDP_HEADER {
            return false;
        }
        let length = usize::from(be16(segment, 4));
        if length < UDP_HEADER || length > segment.len() {
            return false;
        }
        let segment = &segment[..length];
        let (src, dst) = (ipv6_at(packet, 8), ipv6_at(packet, 24));
        // The checksum is mandatory over IPv6 (RFC 8200 §8.1).
        let sum = pseudo_sum(IpAddr::V6(src), IpAddr::V6(dst), PROTOCOL_UDP, length);
        if be16(segment, 6) == 0 || checksum_finish(checksum_add(sum, segment)) != 0 {
            return false;
        }
        let src_port = be16(segment, 0);
        let dst_port = be16(segment, 2);
        let socket = self.sockets.iter().position(|s| {
            s.as_ref()
                .is_some_and(|s| s.kind == SocketKind::Udp && s.port == dst_port)
        });
        match socket {
            Some(id) => {
                self.deliver(
                    id,
                    Datagram {
                        from: IpAddr::V6(src),
                        port: src_port,
                        data: segment[UDP_HEADER..].to_vec(),
                    },
                );
                true
            }
            None => {
                self.send_icmp6_error(v6, (ICMP_UNREACHABLE, UNREACHABLE_PORT, 0), packet, now);
                false
            }
        }
    }

    fn on_icmp6(&mut self, v6: &mut V6, packet: &[u8], message: &[u8], to: To, now: u64) -> bool {
        let (src, dst) = (ipv6_at(packet, 8), ipv6_at(packet, 24));
        if message.len() < ICMP_HEADER {
            return false;
        }
        let sum = pseudo_sum(IpAddr::V6(src), IpAddr::V6(dst), NEXT_ICMPV6, message.len());
        if checksum_finish(checksum_add(sum, message)) != 0 {
            return false;
        }
        let hop_limit = packet[7];
        // Neighbor Discovery comes from the link itself: hop limit 255.
        let nd = hop_limit == 255 && message[1] == 0;
        match message[0] {
            // Only ND (for DAD) reaches a tentative address.
            ND_NEIGHBOR_SOLICIT | ND_NEIGHBOR_ADVERT if !nd => false,
            ND_NEIGHBOR_SOLICIT => self.on_solicitation(v6, &src, &dst, message, now),
            ND_NEIGHBOR_ADVERT => self.on_advertisement(v6, &dst, message, now),
            _ if to == To::Tentative => false,
            ICMP_ECHO_REQUEST => {
                let from = match to {
                    To::Unicast => Some(dst),
                    _ => v6.select_source(&src),
                };
                let Some(from) = from else {
                    return false;
                };
                let mut reply = message.to_vec();
                reply[0] = ICMP_ECHO_REPLY;
                icmp6_checksum(&from, &src, &mut reply);
                let reply = self::packet(&from, &src, NEXT_ICMPV6, v6.hop_limit, &reply);
                let _ = self.route_packet(v6, &src, reply, now);
                true
            }
            ICMP_ECHO_REPLY if to == To::Unicast => {
                let ident = be16(message, 4);
                let socket = self.sockets.iter().position(|s| {
                    s.as_ref()
                        .is_some_and(|s| s.kind == SocketKind::Ping && s.port == ident)
                });
                match socket {
                    Some(id) => {
                        self.deliver(
                            id,
                            Datagram {
                                from: IpAddr::V6(src),
                                port: be16(message, 6),
                                data: message[ICMP_HEADER..].to_vec(),
                            },
                        );
                        true
                    }
                    None => false,
                }
            }
            ND_ROUTER_ADVERT if nd => self.on_router_advertisement(v6, &src, message, now),
            MLD_QUERY => {
                // RFC 3810 §5.1: link-local source, hop limit 1. Answer
                // after a random part of the maximum response delay.
                if hop_limit != 1 || !is_link_local(&src) || message.len() < 24 {
                    return false;
                }
                let code = u64::from(be16(message, 4));
                let max_delay = if code < 32_768 {
                    code
                } else {
                    ((code & 0x0fff) | 0x1000) << (((code >> 12) & 7) + 3)
                };
                let at = now + v6.random() % max_delay.max(1);
                v6.mld_at = Some(v6.mld_at.map_or(at, |t| t.min(at)));
                true
            }
            ICMP_TOO_BIG => self.on_too_big(v6, message, now),
            // Other errors and messages (router solicitations, redirects,
            // MLDv1) need nothing from a host like this one.
            ICMP_UNREACHABLE | ICMP_TIME_EXCEEDED | ICMP_PARAMETER => true,
            _ => false,
        }
    }

    /// RFC 4861 §7.1.1 and §7.2.3.
    fn on_solicitation(
        &mut self,
        v6: &mut V6,
        src: &Ipv6,
        dst: &Ipv6,
        message: &[u8],
        now: u64,
    ) -> bool {
        if message.len() < 24 {
            return false;
        }
        let target = ipv6_at(message, 8);
        let Some(options) = nd_options(&message[24..]) else {
            return false;
        };
        let source_mac = options
            .filter(|(kind, _)| *kind == OPTION_SOURCE_MAC)
            .find_map(|(_, option)| option_mac(option));
        let from_unspecified = *src == UNSPECIFIED6;
        if is_multicast(&target)
            || (from_unspecified && (!is_solicited_node(dst) || source_mac.is_some()))
        {
            return false;
        }
        let Some(index) = v6.address_index(&target) else {
            return false;
        };
        match v6.addresses[index].state {
            AddressState::Tentative => {
                // Another node is probing the same address: both lose it
                // (RFC 4862 §5.4.3).
                if from_unspecified {
                    self.duplicate_detected(v6, index, now);
                }
                true
            }
            AddressState::Duplicate => false,
            AddressState::Preferred | AddressState::Deprecated => {
                if !from_unspecified && let Some(mac) = source_mac {
                    self.learn_neighbor(v6, src, mac, false, now);
                }
                let to = (!from_unspecified).then_some((*src, source_mac));
                self.send_advertisement(v6, &target, to, now);
                true
            }
        }
    }

    /// A solicitation or advertisement told us `address` is at `mac`
    /// (RFC 4861 §7.2.3, §6.3.4): a new entry is STALE, as is a changed
    /// one; a known one keeps its state.
    fn learn_neighbor(&mut self, v6: &mut V6, address: &Ipv6, mac: Mac, router: bool, now: u64) {
        if mac[0] & 1 != 0 {
            return; // never a multicast address
        }
        match v6.neighbor(address) {
            Some(index) => {
                let neighbor = &mut v6.neighbors[index];
                neighbor.is_router |= router;
                if neighbor.mac != Some(mac) {
                    neighbor.mac = Some(mac);
                    neighbor.state = NeighborState::Stale;
                    neighbor.timer = None;
                    neighbor.probes = 0;
                    self.flush_neighbor(v6, index);
                }
            }
            None => {
                if !v6.make_room(&mut self.stats) {
                    return;
                }
                v6.neighbors.push(Neighbor {
                    address: *address,
                    mac: Some(mac),
                    state: NeighborState::Stale,
                    timer: None,
                    probes: 0,
                    is_router: router,
                    source: UNSPECIFIED6,
                    queue: Vec::new(),
                    last_used: now,
                });
            }
        }
    }

    /// Sends the packets that waited for a neighbor's link-layer address.
    fn flush_neighbor(&mut self, v6: &mut V6, index: usize) {
        let neighbor = &mut v6.neighbors[index];
        let Some(mac) = neighbor.mac else {
            return;
        };
        for packet in core::mem::take(&mut neighbor.queue) {
            let _ = self.emit(mac, ETHERTYPE_IPV6, &packet);
        }
    }

    /// RFC 4861 §7.1.2 and §7.2.5.
    fn on_advertisement(&mut self, v6: &mut V6, dst: &Ipv6, message: &[u8], now: u64) -> bool {
        if message.len() < 24 {
            return false;
        }
        let flags = message[4];
        let target = ipv6_at(message, 8);
        let Some(options) = nd_options(&message[24..]) else {
            return false;
        };
        if is_multicast(&target) || (is_multicast(dst) && flags & NA_SOLICITED != 0) {
            return false;
        }
        let target_mac = options
            .filter(|(kind, _)| *kind == OPTION_TARGET_MAC)
            .find_map(|(_, option)| option_mac(option));
        if let Some(index) = v6.address_index(&target) {
            // Someone else holds one of our addresses.
            if v6.addresses[index].state == AddressState::Tentative {
                self.duplicate_detected(v6, index, now);
            } else {
                self.stats.dropped += 1;
            }
            return true;
        }
        let Some(index) = v6.neighbor(&target) else {
            return true; // unsolicited news about a stranger
        };
        let reachable = now + v6.reachable_ms;
        let neighbor = &mut v6.neighbors[index];
        let was_router = neighbor.is_router;
        neighbor.is_router = flags & NA_ROUTER != 0;
        if neighbor.state == NeighborState::Incomplete {
            let Some(mac) = target_mac.filter(|m| m[0] & 1 == 0) else {
                return true;
            };
            neighbor.mac = Some(mac);
            if flags & NA_SOLICITED != 0 {
                neighbor.state = NeighborState::Reachable;
                neighbor.timer = Some(reachable);
            } else {
                neighbor.state = NeighborState::Stale;
                neighbor.timer = None;
            }
            neighbor.probes = 0;
            self.flush_neighbor(v6, index);
        } else {
            let differs = target_mac.is_some_and(|m| Some(m) != neighbor.mac);
            if flags & NA_OVERRIDE == 0 && differs {
                // Not overriding a known address: only mark it doubtful.
                if neighbor.state == NeighborState::Reachable {
                    neighbor.state = NeighborState::Stale;
                    neighbor.timer = None;
                }
            } else {
                if let Some(mac) = target_mac.filter(|m| m[0] & 1 == 0) {
                    neighbor.mac = Some(mac);
                }
                if flags & NA_SOLICITED != 0 {
                    neighbor.state = NeighborState::Reachable;
                    neighbor.timer = Some(reachable);
                    neighbor.probes = 0;
                } else if differs {
                    neighbor.state = NeighborState::Stale;
                    neighbor.timer = None;
                }
            }
        }
        if was_router && flags & NA_ROUTER == 0 {
            v6.routers.retain(|r| r.address != target);
        }
        true
    }

    /// DAD found `index` in use elsewhere (RFC 4862 §5.4.5): it is never
    /// used; a new address is tried with the next DAD counter (RFC 7217
    /// §6), up to [`IDGEN_RETRIES`] times.
    fn duplicate_detected(&mut self, v6: &mut V6, index: usize, now: u64) {
        let old = &mut v6.addresses[index];
        old.state = AddressState::Duplicate;
        let (address, origin, counter) = (old.address, old.origin, old.dad_counter);
        let (preferred, valid) = (old.preferred_until, old.valid_until);
        self.send_mld_report(v6, true, &[solicited_node(&address)]);
        if counter < IDGEN_RETRIES {
            v6.addresses.remove(index);
            let mut prefix = [0u8; 16];
            prefix[..8].copy_from_slice(&address[..8]);
            let delay = v6.random() % MAX_RTR_SOLICITATION_DELAY_MS;
            let mac = self.mac;
            v6.add_address(
                &mac,
                &prefix,
                origin,
                counter + 1,
                now + delay,
                preferred,
                valid,
            );
        }
    }

    /// RFC 4861 §6.1.2 and §6.3.4; prefixes per RFC 4862 §5.5.3.
    fn on_router_advertisement(
        &mut self,
        v6: &mut V6,
        src: &Ipv6,
        message: &[u8],
        now: u64,
    ) -> bool {
        if !is_link_local(src) || message.len() < 16 {
            return false;
        }
        let Some(options) = nd_options(&message[16..]) else {
            return false;
        };
        let hop_limit = message[4];
        let router_lifetime = u64::from(be16(message, 6)) * 1000;
        let reachable = u64::from(be32(message, 8));
        let retrans = u64::from(be32(message, 12));
        if hop_limit != 0 {
            v6.hop_limit = hop_limit;
        }
        if reachable != 0 {
            v6.randomize_reachable(reachable.min(MAX_TIMER_MS));
        }
        if retrans != 0 {
            v6.retrans_ms = retrans.clamp(MIN_RETRANS_MS, MAX_TIMER_MS);
        }
        match v6.routers.iter().position(|r| r.address == *src) {
            Some(index) if router_lifetime == 0 => {
                v6.routers.remove(index);
            }
            Some(index) => v6.routers[index].expires = now + router_lifetime,
            None if router_lifetime > 0 && v6.routers.len() < MAX_ROUTERS => {
                v6.routers.push(Router {
                    address: *src,
                    expires: now + router_lifetime,
                });
            }
            None => {}
        }
        if !v6.routers.is_empty() {
            v6.rs_at = None; // solicitation answered
        }
        let mut source_mac = None;
        for (kind, option) in options {
            match kind {
                OPTION_SOURCE_MAC => source_mac = option_mac(option),
                OPTION_MTU if option.len() == 8 => {
                    let mtu = be32(option, 4) as usize;
                    if (MIN_MTU..=MTU).contains(&mtu) {
                        v6.mtu = mtu as u16;
                    }
                }
                OPTION_PREFIX if option.len() == 32 => self.on_prefix(v6, option, now),
                OPTION_RDNSS if option.len() >= 24 => {
                    let lifetime_s = be32(option, 4);
                    let server = ipv6_at(option, 8);
                    if lifetime_s == 0 {
                        if v6.dns.is_some_and(|(dns, _)| dns == server) {
                            v6.dns = None;
                        }
                    } else if !is_multicast(&server) && server != UNSPECIFIED6 {
                        v6.dns = Some((server, lifetime(now, lifetime_s)));
                    }
                }
                _ => {}
            }
        }
        if let Some(mac) = source_mac {
            self.learn_neighbor(v6, src, mac, true, now);
        } else if let Some(index) = v6.neighbor(src) {
            v6.neighbors[index].is_router = true;
        }
        true
    }

    /// A prefix information option.
    fn on_prefix(&mut self, v6: &mut V6, option: &[u8], now: u64) {
        let len = option[2];
        let flags = option[3];
        let valid_s = be32(option, 4);
        let preferred_s = be32(option, 8);
        let mut prefix = ipv6_at(option, 16);
        if len > 128 || is_link_local(&prefix) || is_multicast(&prefix) {
            return;
        }
        // Bits past the prefix length are ignored.
        for (i, byte) in prefix.iter_mut().enumerate() {
            let bits = usize::from(len).saturating_sub(i * 8).min(8);
            *byte &= !(0xffu8.checked_shr(bits as u32).unwrap_or(0));
        }
        if flags & PREFIX_ON_LINK != 0 {
            match v6
                .prefixes
                .iter()
                .position(|p| p.len == len && p.prefix == prefix)
            {
                Some(index) if valid_s == 0 => {
                    v6.prefixes.remove(index);
                }
                Some(index) => v6.prefixes[index].expires = lifetime(now, valid_s),
                None if valid_s > 0 && v6.prefixes.len() < MAX_PREFIXES => {
                    v6.prefixes.push(Prefix {
                        prefix,
                        len,
                        expires: lifetime(now, valid_s),
                    });
                }
                None => {}
            }
        }
        // Autoconfiguration needs a 64-bit prefix (our identifiers are 64
        // bits) and sane lifetimes.
        if flags & PREFIX_AUTONOMOUS == 0 || len != 64 || preferred_s > valid_s {
            return;
        }
        let existing = v6
            .addresses
            .iter()
            .position(|a| a.origin == AddressOrigin::Slaac && a.address[..8] == prefix[..8]);
        match existing {
            Some(index) => {
                let address = &mut v6.addresses[index];
                // RFC 4862 §5.5.3 (e): the two-hour rule.
                let remaining = address
                    .valid_until
                    .map_or(u64::MAX, |until| until.saturating_sub(now));
                let received = if valid_s == INFINITE {
                    u64::MAX
                } else {
                    u64::from(valid_s) * 1000
                };
                if received > TWO_HOURS_MS || received > remaining {
                    address.valid_until = lifetime(now, valid_s);
                } else if remaining > TWO_HOURS_MS {
                    address.valid_until = Some(now + TWO_HOURS_MS);
                }
                address.preferred_until = lifetime(now, preferred_s);
                if address.state == AddressState::Deprecated && preferred_s > 0 {
                    address.state = AddressState::Preferred;
                }
            }
            None if valid_s > 0 => {
                let mac = self.mac;
                v6.add_address(
                    &mac,
                    &prefix,
                    AddressOrigin::Slaac,
                    0,
                    now,
                    lifetime(now, preferred_s),
                    lifetime(now, valid_s),
                );
            }
            None => {}
        }
    }

    /// Packet Too Big (RFC 8201): remember the path MTU, and shrink TCP
    /// segments to that destination.
    fn on_too_big(&mut self, v6: &mut V6, message: &[u8], now: u64) -> bool {
        // The offending packet's header must be there and be ours.
        if message.len() < ICMP_HEADER + HEADER {
            return false;
        }
        let quoted = &message[ICMP_HEADER..];
        let (source, destination) = (ipv6_at(quoted, 8), ipv6_at(quoted, 24));
        if !v6.is_ours(&source) {
            return false;
        }
        let mtu = (be32(message, 4) as usize).clamp(MIN_MTU, usize::from(v6.mtu)) as u16;
        match v6.pmtu.iter_mut().find(|p| p.destination == destination) {
            Some(entry) => {
                entry.mtu = entry.mtu.min(mtu);
                entry.expires = now + PMTU_LIFETIME_MS;
            }
            None => {
                if v6.pmtu.len() >= MAX_PMTU {
                    v6.pmtu.remove(0);
                }
                v6.pmtu.push(PathMtu {
                    destination,
                    mtu,
                    expires: now + PMTU_LIFETIME_MS,
                });
            }
        }
        let mss = (usize::from(mtu) - HEADER - 20) as u16;
        for tcb in self
            .sockets
            .iter_mut()
            .flatten()
            .filter_map(|s| s.tcp.as_deref_mut())
        {
            if tcb.remote == IpAddr::V6(destination) {
                tcb.mss = tcb.mss.min(mss);
            }
        }
        true
    }

    // ---- Timers ------------------------------------------------------------

    pub(crate) fn poll_v6(&mut self, now: u64) {
        self.with_v6(|stack, v6| stack.poll_v6_with(v6, now));
    }

    fn poll_v6_with(&mut self, v6: &mut V6, now: u64) {
        // Group membership first: a new address's group is joined before
        // its duplicate detection probe goes out (RFC 4862 §5.4.2).
        if let Some(at) = v6.mld_at
            && now >= at
        {
            let change = v6.mld_change;
            self.send_mld_report(v6, change, &[]);
            if v6.mld_unsolicited > 1 {
                v6.mld_unsolicited -= 1;
                let delay = v6.random() % MLD_UNSOLICITED_INTERVAL_MS;
                v6.mld_at = Some(now + delay.max(1));
            } else {
                v6.mld_unsolicited = 0;
                v6.mld_change = false;
                v6.mld_at = None;
            }
        }
        // Addresses: duplicate detection, then lifetimes.
        let mut left: Vec<Ipv6> = Vec::new();
        let mut index = 0;
        while index < v6.addresses.len() {
            let address = &mut v6.addresses[index];
            if address.valid_until.is_some_and(|until| now >= until) {
                let gone = v6.addresses.remove(index);
                if gone.joined() {
                    left.push(solicited_node(&gone.address));
                }
                continue;
            }
            if address.state == AddressState::Tentative && now >= address.dad_at {
                if address.dad_probes > 0 {
                    address.dad_probes -= 1;
                    address.dad_at = now + v6.retrans_ms;
                    let target = address.address;
                    self.send_solicitation(&target, &UNSPECIFIED6, None);
                } else {
                    address.state = AddressState::Preferred;
                    if address.origin == AddressOrigin::LinkLocal {
                        // Now routers can be asked (RFC 4861 §6.3.7).
                        v6.rs_at = Some(now);
                    }
                }
            }
            let address = &mut v6.addresses[index];
            if address.state == AddressState::Preferred
                && address.preferred_until.is_some_and(|until| now >= until)
            {
                address.state = AddressState::Deprecated;
            }
            index += 1;
        }
        // Groups we left, unless another address still needs them.
        left.retain(|group| !v6.in_group(group));
        if !left.is_empty() {
            self.send_mld_report(v6, true, &left);
        }
        if let Some(at) = v6.rs_at
            && now >= at
        {
            if v6.rs_sent < MAX_RTR_SOLICITATIONS && v6.routers.is_empty() {
                self.send_router_solicitation(v6);
                v6.rs_sent += 1;
                v6.rs_at = Some(now + RTR_SOLICITATION_INTERVAL_MS);
            } else {
                v6.rs_at = None;
            }
        }
        v6.routers.retain(|r| r.expires > now);
        v6.prefixes.retain(|p| p.expires.is_none_or(|e| e > now));
        v6.pmtu.retain(|p| p.expires > now);
        if v6
            .dns
            .is_some_and(|(_, expires)| expires.is_some_and(|e| e <= now))
        {
            v6.dns = None;
        }
        self.poll_neighbors(v6, now);
    }

    /// RFC 4861 §7.3.3 timers.
    fn poll_neighbors(&mut self, v6: &mut V6, now: u64) {
        let mut index = 0;
        while index < v6.neighbors.len() {
            let neighbor = &mut v6.neighbors[index];
            if neighbor.timer.is_none_or(|t| t > now) {
                index += 1;
                continue;
            }
            let (address, source, mac) = (neighbor.address, neighbor.source, neighbor.mac);
            match neighbor.state {
                NeighborState::Incomplete if neighbor.probes < MAX_MULTICAST_SOLICIT => {
                    neighbor.probes += 1;
                    neighbor.timer = Some(now + v6.retrans_ms);
                    let source = v6.select_source(&address).unwrap_or(source);
                    self.send_solicitation(&address, &source, None);
                }
                NeighborState::Probe if neighbor.probes < MAX_UNICAST_SOLICIT => {
                    neighbor.probes += 1;
                    neighbor.timer = Some(now + v6.retrans_ms);
                    if let Some(source) = v6.select_source(&address) {
                        self.send_solicitation(&address, &source, mac);
                    }
                }
                NeighborState::Incomplete | NeighborState::Probe => {
                    // No answer: the neighbor is gone, and so is what
                    // waited for it.
                    let gone = v6.neighbors.remove(index);
                    self.stats.dropped += gone.queue.len() as u64;
                    continue;
                }
                NeighborState::Reachable => {
                    neighbor.state = NeighborState::Stale;
                    neighbor.timer = None;
                }
                NeighborState::Delay => {
                    neighbor.state = NeighborState::Probe;
                    neighbor.probes = 1;
                    neighbor.timer = Some(now + v6.retrans_ms);
                    if let Some(source) = v6.select_source(&address) {
                        self.send_solicitation(&address, &source, mac);
                    }
                }
                NeighborState::Stale => neighbor.timer = None,
            }
            index += 1;
        }
    }
}
