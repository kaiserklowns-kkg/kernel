//! IPv6 (ADR-0043): Neighbor Discovery, SLAAC, ICMPv6, extension headers,
//! UDP and TCP over IPv6, driven frame by frame.

extern crate std;

use super::ipv6::{IDGEN_RETRIES, multicast_mac, solicited_node};
use super::*;
use std::vec::Vec as StdVec;

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

const OURS: Mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const ROUTER_MAC: Mac = [0x52, 0x55, 10, 0, 2, 2];
const OTHER_MAC: Mac = [0x52, 0x55, 10, 0, 2, 0x99];
const KEY: [u8; 16] = [7; 16];

fn v6(text: &str) -> Ipv6 {
    parse_ipv6(text).unwrap()
}

fn router_ll() -> Ipv6 {
    v6("fe80::2")
}

fn host() -> Ipv6 {
    v6("fec0::2")
}

fn all_nodes() -> Ipv6 {
    v6("ff02::1")
}

/// An IPv6 packet (header and payload).
fn ip6(src: Ipv6, dst: Ipv6, next: u8, hop: u8, payload: &[u8]) -> StdVec<u8> {
    let mut packet = StdVec::from([0x60, 0, 0, 0]);
    packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    packet.extend_from_slice(&[next, hop]);
    packet.extend_from_slice(&src);
    packet.extend_from_slice(&dst);
    packet.extend_from_slice(payload);
    packet
}

fn eth(dst: Mac, src: Mac, packet: &[u8]) -> StdVec<u8> {
    let mut frame = StdVec::new();
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&0x86ddu16.to_be_bytes());
    frame.extend_from_slice(packet);
    frame
}

/// Fills in the upper-layer checksum at `at`.
fn checksummed(src: Ipv6, dst: Ipv6, next: u8, mut message: StdVec<u8>, at: usize) -> StdVec<u8> {
    message[at..at + 2].copy_from_slice(&[0, 0]);
    let sum = pseudo_sum(IpAddr::V6(src), IpAddr::V6(dst), next, message.len());
    let sum = checksum_finish(checksum_add(sum, &message));
    message[at..at + 2].copy_from_slice(&sum.to_be_bytes());
    message
}

/// An ICMPv6 message in a frame, hop limit `hop`.
fn icmp_frame(
    dst_mac: Mac,
    src_mac: Mac,
    src: Ipv6,
    dst: Ipv6,
    hop: u8,
    message: &[u8],
) -> StdVec<u8> {
    let message = checksummed(src, dst, 58, message.to_vec(), 2);
    eth(dst_mac, src_mac, &ip6(src, dst, 58, hop, &message))
}

fn udp6(src: Ipv6, dst: Ipv6, sport: u16, dport: u16, data: &[u8]) -> StdVec<u8> {
    let mut segment = StdVec::new();
    segment.extend_from_slice(&sport.to_be_bytes());
    segment.extend_from_slice(&dport.to_be_bytes());
    segment.extend_from_slice(&((8 + data.len()) as u16).to_be_bytes());
    segment.extend_from_slice(&[0, 0]);
    segment.extend_from_slice(data);
    checksummed(src, dst, 17, segment, 6)
}

/// A frame the stack sent, checked and taken apart (past a hop-by-hop
/// header, which only MLD reports carry).
#[derive(Debug)]
struct Sent {
    dst_mac: Mac,
    src: Ipv6,
    dst: Ipv6,
    hop: u8,
    next: u8,
    hop_by_hop: bool,
    payload: StdVec<u8>,
}

fn parse(frame: &[u8]) -> Option<Sent> {
    if be16(frame, 12) != 0x86dd {
        return None;
    }
    assert_eq!(&frame[6..12], &OURS, "source MAC");
    let packet = &frame[14..];
    assert_eq!(packet[0] >> 4, 6);
    assert_eq!(usize::from(be16(packet, 4)) + 40, packet.len(), "length");
    let (mut next, mut at) = (packet[6], 40);
    let hop_by_hop = next == 0;
    if hop_by_hop {
        next = packet[40];
        at += (usize::from(packet[41]) + 1) * 8;
    }
    let sent = Sent {
        dst_mac: frame[..6].try_into().unwrap(),
        src: packet[8..24].try_into().unwrap(),
        dst: packet[24..40].try_into().unwrap(),
        hop: packet[7],
        next,
        hop_by_hop,
        payload: packet[at..].to_vec(),
    };
    if matches!(next, 6 | 17 | 58) {
        let sum = pseudo_sum(
            IpAddr::V6(sent.src),
            IpAddr::V6(sent.dst),
            next,
            sent.payload.len(),
        );
        assert_eq!(
            checksum_finish(checksum_add(sum, &sent.payload)),
            0,
            "checksum of {next}"
        );
    }
    Some(sent)
}

fn sent(stack: &mut Stack) -> StdVec<Sent> {
    let mut out = StdVec::new();
    while let Some(frame) = stack.transmit() {
        if let Some(sent) = parse(&frame) {
            out.push(sent);
        }
    }
    out
}

fn icmp_of(sent: &[Sent], kind: u8) -> StdVec<&Sent> {
    sent.iter()
        .filter(|s| s.next == 58 && s.payload[0] == kind)
        .collect()
}

/// A test bench: a stack and a clock.
struct Bench {
    stack: Stack,
    now: u64,
}

impl Bench {
    fn new() -> Self {
        let mut stack = Stack::new(OURS);
        stack.configure(Config {
            address: [10, 0, 2, 15],
            prefix: 24,
            gateway: Some([10, 0, 2, 2]),
            dns: None,
        });
        stack.enable_ipv6(KEY, 0);
        Self { stack, now: 0 }
    }

    /// Lets `ms` pass, polling as a service would; returns what was sent.
    fn advance(&mut self, ms: u64) -> StdVec<Sent> {
        let end = self.now + ms;
        let mut out = StdVec::new();
        loop {
            self.stack.poll(self.now);
            out.extend(sent(&mut self.stack));
            if self.now >= end {
                return out;
            }
            self.now = (self.now + 50).min(end);
        }
    }

    fn receive(&mut self, frame: &[u8]) -> StdVec<Sent> {
        self.stack.receive(frame, self.now);
        sent(&mut self.stack)
    }

    fn config(&self) -> Ipv6Config {
        self.stack.ipv6_config().unwrap()
    }

    fn address(&self, origin: AddressOrigin) -> Option<Address6> {
        self.config()
            .addresses
            .into_iter()
            .find(|a| a.origin == origin)
    }

    fn link_local(&self) -> Ipv6 {
        self.address(AddressOrigin::LinkLocal).unwrap().address
    }

    fn slaac(&self) -> Ipv6 {
        self.address(AddressOrigin::Slaac).unwrap().address
    }

    /// Link-local usable, router solicited.
    fn up() -> (Self, StdVec<Sent>) {
        let mut bench = Self::new();
        let sent = bench.advance(2_100);
        assert_eq!(
            bench.address(AddressOrigin::LinkLocal).unwrap().state,
            AddressState::Preferred
        );
        (bench, sent)
    }

    /// The host fec0::2 solicits our SLAAC address, so we know its MAC
    /// (STALE) and answer it at once.
    fn know_host(&mut self) {
        let ours = self.slaac();
        let out = self.receive(&solicitation(
            host(),
            solicited_node(&ours),
            ours,
            Some(ROUTER_MAC),
            255,
        ));
        assert_eq!(icmp_of(&out, 136).len(), 1);
    }

    /// Up, with a SLAAC address from QEMU's user network's advertisement.
    fn configured() -> Self {
        let (mut bench, _) = Self::up();
        bench.receive(&slirp_advertisement());
        bench.advance(1_100);
        assert_eq!(
            bench.address(AddressOrigin::Slaac).unwrap().state,
            AddressState::Preferred
        );
        bench
    }
}

/// A router advertisement from fe80::2.
fn advertisement(router_lifetime: u16, options: &[StdVec<u8>]) -> StdVec<u8> {
    let mut message = StdVec::from([134, 0, 0, 0, 64, 0]);
    message.extend_from_slice(&router_lifetime.to_be_bytes());
    message.extend_from_slice(&[0; 8]); // reachable, retrans: unspecified
    for option in options {
        message.extend_from_slice(option);
    }
    icmp_frame(
        multicast_mac(&all_nodes()),
        ROUTER_MAC,
        router_ll(),
        all_nodes(),
        255,
        &message,
    )
}

fn prefix_option(prefix: Ipv6, len: u8, flags: u8, valid: u32, preferred: u32) -> StdVec<u8> {
    let mut option = StdVec::from([3, 4, len, flags]);
    option.extend_from_slice(&valid.to_be_bytes());
    option.extend_from_slice(&preferred.to_be_bytes());
    option.extend_from_slice(&[0; 4]);
    option.extend_from_slice(&prefix);
    option
}

fn mac_option(kind: u8, mac: Mac) -> StdVec<u8> {
    let mut option = StdVec::from([kind, 1]);
    option.extend_from_slice(&mac);
    option
}

fn rdnss_option(server: Ipv6, lifetime: u32) -> StdVec<u8> {
    let mut option = StdVec::from([25, 3, 0, 0]);
    option.extend_from_slice(&lifetime.to_be_bytes());
    option.extend_from_slice(&server);
    option
}

/// What QEMU's user network (slirp) advertises: fec0::/64, on-link and
/// autonomous, DNS at fec0::3.
fn slirp_advertisement() -> StdVec<u8> {
    advertisement(
        1800,
        &[
            mac_option(1, ROUTER_MAC),
            prefix_option(v6("fec0::"), 64, 0xc0, 86_400, 14_400),
            StdVec::from([5, 1, 0, 0, 0, 0, 0x05, 0xdc]),
            rdnss_option(v6("fec0::3"), 1800),
        ],
    )
}

fn solicitation(
    src: Ipv6,
    dst: Ipv6,
    target: Ipv6,
    source_mac: Option<Mac>,
    hop: u8,
) -> StdVec<u8> {
    let mut message = StdVec::from([135, 0, 0, 0, 0, 0, 0, 0]);
    message.extend_from_slice(&target);
    if let Some(mac) = source_mac {
        message.extend_from_slice(&mac_option(1, mac));
    }
    let dst_mac = if dst[0] == 0xff {
        multicast_mac(&dst)
    } else {
        OURS
    };
    icmp_frame(
        dst_mac,
        source_mac.unwrap_or(OTHER_MAC),
        src,
        dst,
        hop,
        &message,
    )
}

fn advertisement_na(
    from_mac: Mac,
    src: Ipv6,
    dst: Ipv6,
    target: Ipv6,
    flags: u8,
    target_mac: Option<Mac>,
) -> StdVec<u8> {
    let mut message = StdVec::from([136, 0, 0, 0, flags, 0, 0, 0]);
    message.extend_from_slice(&target);
    if let Some(mac) = target_mac {
        message.extend_from_slice(&mac_option(2, mac));
    }
    let dst_mac = if dst[0] == 0xff {
        multicast_mac(&dst)
    } else {
        OURS
    };
    icmp_frame(dst_mac, from_mac, src, dst, 255, &message)
}

#[test]
fn link_local_address_comes_up_after_duplicate_detection() {
    let mut bench = Bench::new();
    let tentative = bench.address(AddressOrigin::LinkLocal).unwrap();
    assert_eq!(tentative.state, AddressState::Tentative);
    assert_eq!(tentative.prefix, 64);
    let address = tentative.address;
    assert_eq!(address[..8], [0xfe, 0x80, 0, 0, 0, 0, 0, 0]);
    // A stable identifier (RFC 7217), not the MAC (EUI-64 would put
    // ff:fe in the middle).
    assert_ne!(address[11..13], [0xff, 0xfe]);

    let first = bench.advance(0);
    // MLDv2: join the solicited-node group (hop limit 1, router alert).
    let report = icmp_of(&first, 143);
    assert_eq!(report.len(), 1);
    let report = report[0];
    assert!(report.hop_by_hop);
    assert_eq!(report.hop, 1);
    assert_eq!(report.dst, v6("ff02::16"));
    assert_eq!(report.dst_mac, [0x33, 0x33, 0, 0, 0, 0x16]);
    assert_eq!(report.src, [0; 16], "from :: while tentative");
    assert_eq!(be16(&report.payload, 6), 1, "one record");
    assert_eq!(report.payload[8], 4, "CHANGE_TO_EXCLUDE");
    assert_eq!(report.payload[12..28], solicited_node(&address));

    // Probing: a solicitation from :: to the solicited-node group.
    let probing = bench.advance(1_000);
    let probe = icmp_of(&probing, 135);
    assert_eq!(probe.len(), 1);
    let probe = probe[0];
    assert_eq!(probe.src, [0; 16]);
    assert_eq!(probe.dst, solicited_node(&address));
    assert_eq!(probe.dst_mac, multicast_mac(&solicited_node(&address)));
    assert_eq!(probe.hop, 255);
    assert_eq!(probe.payload[8..24], address);
    assert_eq!(probe.payload.len(), 24, "no link-layer option from ::");
    assert_eq!(
        bench.address(AddressOrigin::LinkLocal).unwrap().state,
        AddressState::Tentative
    );

    // Unchallenged: usable, and routers are asked.
    let after = bench.advance(1_100);
    assert_eq!(
        bench.address(AddressOrigin::LinkLocal).unwrap().state,
        AddressState::Preferred
    );
    let solicit = icmp_of(&after, 133);
    assert_eq!(solicit.len(), 1);
    assert_eq!(solicit[0].src, address);
    assert_eq!(solicit[0].dst, v6("ff02::2"));
    assert_eq!(solicit[0].payload[8..16], mac_option(1, OURS)[..]);
    // Retried every 4 s, three times in all, then given up.
    let later = bench.advance(20_000);
    assert_eq!(icmp_of(&later, 133).len(), 2);

    // The same key and MAC give the same address; another key does not.
    assert_eq!(Bench::new().link_local(), address);
    let mut other = Stack::new(OURS);
    other.enable_ipv6([8; 16], 0);
    assert_ne!(other.ipv6_config().unwrap().addresses[0].address, address);
}

#[test]
fn dhcp_runs_alongside_ipv6() {
    // As the service does: IPv6 enabled at once, polled at deadlines.
    let mut stack = Stack::new(OURS);
    stack.enable_ipv6(KEY, 1_500);
    let mut now = 1_500;
    let mut dhcp = StdVec::new();
    for _ in 0..200 {
        stack.poll(now);
        while let Some(frame) = stack.transmit() {
            if be16(&frame, 12) == ETHERTYPE_IPV4 {
                dhcp.push(now);
            }
        }
        let next = stack.next_deadline().expect("DHCP has a deadline");
        assert!(next >= now, "deadline {next} before {now}");
        now = next.max(now + 1);
        if now > 40_000 {
            break;
        }
    }
    assert_eq!(dhcp, [1_500, 3_500, 7_500, 15_500, 31_500]);
}

#[test]
fn ipv6_is_off_until_enabled() {
    let mut stack = Stack::new(OURS);
    assert!(stack.ipv6_config().is_none());
    let ll = v6("fe80::1234");
    stack.receive(
        &solicitation(router_ll(), solicited_node(&ll), ll, Some(ROUTER_MAC), 255),
        0,
    );
    assert!(stack.transmit().is_none());
    let ping = stack.ping_open().unwrap();
    assert_eq!(
        stack.send_to(ping, host(), 0, b"x", 0),
        Err(NetError::NotConfigured)
    );
}

#[test]
fn autoconfigures_from_router_advertisements() {
    let (mut bench, _) = Bench::up();
    bench.receive(&slirp_advertisement());
    let config = bench.config();
    assert_eq!(config.router, Some(router_ll()));
    assert_eq!(config.dns, Some(v6("fec0::3")));
    assert_eq!(config.mtu, 1500);
    let slaac = bench.address(AddressOrigin::Slaac).unwrap();
    assert_eq!(slaac.state, AddressState::Tentative);
    assert_eq!(slaac.address[..8], v6("fec0::")[..8]);
    assert_ne!(
        slaac.address[8..],
        bench.link_local()[8..],
        "per-prefix identifier"
    );
    // The router's address was learned from its option.
    assert_eq!(
        bench.stack.neighbor_state(&router_ll()),
        Some((NeighborState::Stale, Some(ROUTER_MAC)))
    );

    let probing = bench.advance(1_100);
    let probe = icmp_of(&probing, 135);
    assert_eq!(probe.len(), 1);
    assert_eq!(probe[0].payload[8..24], slaac.address);
    assert_eq!(
        bench.address(AddressOrigin::Slaac).unwrap().state,
        AddressState::Preferred
    );
    // No more solicitations once a router answered.
    assert!(icmp_of(&bench.advance(10_000), 133).is_empty());
    // The same advertisement again changes nothing.
    bench.receive(&slirp_advertisement());
    assert_eq!(bench.config().addresses.len(), 2);
}

#[test]
fn ignores_invalid_advertisements() {
    let (mut bench, _) = Bench::up();
    let good = slirp_advertisement();
    // Not from a link-local address.
    let mut packet = good[14..].to_vec();
    packet[8..24].copy_from_slice(&host());
    let message = checksummed(host(), all_nodes(), 58, packet[40..].to_vec(), 2);
    let forged = eth(
        multicast_mac(&all_nodes()),
        ROUTER_MAC,
        &ip6(host(), all_nodes(), 58, 255, &message),
    );
    bench.receive(&forged);
    // Routed from elsewhere (hop limit below 255).
    let message = good[14 + 40..].to_vec();
    bench.receive(&eth(
        multicast_mac(&all_nodes()),
        ROUTER_MAC,
        &ip6(router_ll(), all_nodes(), 58, 254, &message),
    ));
    // A zero-length option.
    let mut zero = good.clone();
    zero[14 + 40 + 16 + 1] = 0;
    bench.receive(&zero);
    // A bad checksum.
    let mut corrupt = good.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    bench.receive(&corrupt);
    let config = bench.config();
    assert_eq!(config.router, None);
    assert_eq!(config.addresses.len(), 1);
    // Prefixes that cannot be autoconfigured.
    bench.receive(&advertisement(
        0,
        &[
            prefix_option(v6("fe80::"), 64, 0xc0, 100, 100),
            prefix_option(v6("2001:db8::"), 48, 0xc0, 100, 100),
            prefix_option(v6("2001:db9::"), 64, 0xc0, 100, 200),
            prefix_option(v6("2001:dba::"), 64, 0x80, 100, 100),
        ],
    ));
    assert_eq!(bench.config().addresses.len(), 1);
}

#[test]
fn lifetimes_expire_and_follow_the_two_hour_rule() {
    let mut bench = Bench::new();
    bench.advance(2_100);
    bench.receive(&advertisement(
        60,
        &[
            mac_option(1, ROUTER_MAC),
            prefix_option(v6("2001:db8::"), 64, 0xc0, 10_000, 30),
            rdnss_option(v6("2001:db8::53"), 20),
        ],
    ));
    bench.advance(1_100);
    let address = bench.slaac();
    assert_eq!(bench.config().dns, Some(v6("2001:db8::53")));
    // DNS server lifetime.
    bench.advance(20_000);
    assert_eq!(bench.config().dns, None);
    // Preferred lifetime: deprecated, still there.
    bench.advance(10_000);
    assert_eq!(
        bench.address(AddressOrigin::Slaac).unwrap().state,
        AddressState::Deprecated
    );
    // The router's lifetime ran out.
    bench.advance(30_000);
    assert_eq!(bench.config().router, None);
    // An advertisement cannot cut the valid lifetime below two hours.
    bench.receive(&advertisement(
        0,
        &[prefix_option(v6("2001:db8::"), 64, 0xc0, 5, 5)],
    ));
    bench.advance(10_000);
    let still = bench.address(AddressOrigin::Slaac).unwrap();
    assert_eq!(still.address, address);
    assert_eq!(
        still.state,
        AddressState::Deprecated,
        "preferred 5 s passed"
    );
    // But it does expire when its (remaining) valid lifetime ends.
    bench.advance(10_000_000);
    assert!(bench.address(AddressOrigin::Slaac).is_none());
    // A router lifetime of zero removes the router at once.
    bench.receive(&slirp_advertisement());
    assert_eq!(bench.config().router, Some(router_ll()));
    bench.receive(&advertisement(0, &[]));
    assert_eq!(bench.config().router, None);
}

#[test]
fn duplicates_are_regenerated_then_given_up() {
    let (mut bench, _) = Bench::up();
    bench.receive(&slirp_advertisement());
    let mut seen = StdVec::new();
    for attempt in 0..=IDGEN_RETRIES {
        let tentative = bench.address(AddressOrigin::Slaac).unwrap();
        assert_eq!(
            tentative.state,
            AddressState::Tentative,
            "attempt {attempt}"
        );
        assert!(
            !seen.contains(&tentative.address),
            "a new address each time"
        );
        seen.push(tentative.address);
        // Someone answers for it (on even attempts), or probes it too.
        let frame = if attempt % 2 == 0 {
            advertisement_na(
                OTHER_MAC,
                tentative.address,
                all_nodes(),
                tentative.address,
                0x20,
                Some(OTHER_MAC),
            )
        } else {
            solicitation(
                [0; 16],
                solicited_node(&tentative.address),
                tentative.address,
                None,
                255,
            )
        };
        bench.receive(&frame);
    }
    let gone = bench.address(AddressOrigin::Slaac).unwrap();
    assert_eq!(gone.state, AddressState::Duplicate);
    // Never usable: probing, echo and sending all ignore it.
    bench.advance(5_000);
    assert_eq!(
        bench.address(AddressOrigin::Slaac).unwrap().state,
        AddressState::Duplicate
    );
    let udp = bench.stack.udp_bind(0).unwrap();
    assert_eq!(
        bench.stack.send_to(udp, host(), 7, b"x", bench.now),
        Err(NetError::NotConfigured)
    );
}

#[test]
fn answers_neighbor_solicitations() {
    let (mut bench, _) = Bench::up();
    let ours = bench.link_local();
    let group = solicited_node(&ours);
    let replies = bench.receive(&solicitation(
        router_ll(),
        group,
        ours,
        Some(ROUTER_MAC),
        255,
    ));
    let na = icmp_of(&replies, 136);
    assert_eq!(na.len(), 1);
    let na = na[0];
    assert_eq!(na.dst_mac, ROUTER_MAC);
    assert_eq!(na.src, ours);
    assert_eq!(na.dst, router_ll());
    assert_eq!(na.hop, 255);
    assert_eq!(na.payload[4], 0x60, "solicited, override, not a router");
    assert_eq!(na.payload[8..24], ours);
    assert_eq!(na.payload[24..32], mac_option(2, OURS)[..]);
    assert!(matches!(
        bench.stack.neighbor_state(&router_ll()),
        Some((
            NeighborState::Stale | NeighborState::Delay,
            Some(ROUTER_MAC)
        ))
    ));

    // Probes from :: are answered to all nodes, unsolicited.
    let replies = bench.receive(&solicitation([0; 16], group, ours, None, 255));
    let na = icmp_of(&replies, 136);
    assert_eq!(na[0].dst, all_nodes());
    assert_eq!(na[0].payload[4], 0x20);

    // Ignored: routed (hop limit), from :: with a link-layer address, for
    // someone else, for a multicast target.
    for frame in [
        solicitation(router_ll(), group, ours, Some(ROUTER_MAC), 64),
        solicitation([0; 16], group, ours, Some(ROUTER_MAC), 255),
        solicitation(router_ll(), group, v6("fe80::77"), Some(ROUTER_MAC), 255),
        solicitation(router_ll(), group, all_nodes(), Some(ROUTER_MAC), 255),
    ] {
        assert!(icmp_of(&bench.receive(&frame), 136).is_empty());
    }
    // Groups we are not in are filtered by MAC.
    let before = bench.stack.stats().dropped;
    let other = v6("fe80::77");
    bench.receive(&solicitation(
        router_ll(),
        solicited_node(&other),
        other,
        Some(ROUTER_MAC),
        255,
    ));
    assert_eq!(bench.stack.stats().dropped, before, "not even looked at");
    // Our own frames looped back are dropped.
    let mut looped = solicitation([0; 16], group, ours, None, 255);
    looped[6..12].copy_from_slice(&OURS);
    assert!(bench.receive(&looped).is_empty());
}

#[test]
fn resolves_neighbors_and_pings() {
    let mut bench = Bench::configured();
    let ours = bench.slaac();
    let ping = bench.stack.ping_open().unwrap();
    bench
        .stack
        .send_to(ping, host(), 0, b"hello", bench.now)
        .unwrap();
    let out = sent(&mut bench.stack);
    // fec0::2 is on-link (the prefix said so): resolve it directly.
    let ns = icmp_of(&out, 135);
    assert_eq!(ns.len(), 1);
    assert_eq!(ns[0].src, ours, "from the packet's source");
    assert_eq!(ns[0].dst, solicited_node(&host()));
    assert_eq!(ns[0].payload[8..24], host());
    assert_eq!(ns[0].payload[24..32], mac_option(1, OURS)[..]);
    assert!(icmp_of(&out, 128).is_empty(), "waiting for resolution");
    assert_eq!(
        bench.stack.neighbor_state(&host()),
        Some((NeighborState::Incomplete, None))
    );

    let out = bench.receive(&advertisement_na(
        ROUTER_MAC,
        host(),
        ours,
        host(),
        0x60,
        Some(ROUTER_MAC),
    ));
    let echo = icmp_of(&out, 128);
    assert_eq!(echo.len(), 1);
    assert_eq!(echo[0].dst_mac, ROUTER_MAC);
    assert_eq!((echo[0].src, echo[0].dst), (ours, host()));
    assert_eq!(echo[0].payload[8..], *b"hello");
    assert_eq!(
        bench.stack.neighbor_state(&host()),
        Some((NeighborState::Reachable, Some(ROUTER_MAC)))
    );

    let mut reply = echo[0].payload.clone();
    reply[0] = 129;
    bench.receive(&icmp_frame(OURS, ROUTER_MAC, host(), ours, 64, &reply));
    assert_eq!(
        bench.stack.recv(ping),
        Some(Datagram {
            from: IpAddr::V6(host()),
            port: 1,
            data: b"hello".to_vec(),
        })
    );

    // Off-link destinations go to the router (known from its
    // advertisement).
    bench
        .stack
        .send_to(ping, v6("2001:db8::1"), 0, b"far", bench.now)
        .unwrap();
    let out = sent(&mut bench.stack);
    let echo = icmp_of(&out, 128);
    assert_eq!(echo.len(), 1);
    assert_eq!(echo[0].dst_mac, ROUTER_MAC);
    assert_eq!(echo[0].dst, v6("2001:db8::1"));
    assert_eq!(
        bench.stack.neighbor_state(&router_ll()).unwrap().0,
        NeighborState::Delay,
        "used while stale"
    );
    // Without a router, nothing off-link can be reached.
    bench.receive(&advertisement(0, &[]));
    assert_eq!(
        bench
            .stack
            .send_to(ping, v6("2001:db8::1"), 0, b"far", bench.now),
        Err(NetError::NoRoute)
    );
}

#[test]
fn neighbor_unreachability_detection() {
    let mut bench = Bench::configured();
    let ours = bench.slaac();
    let udp = bench.stack.udp_bind(0).unwrap();
    bench
        .stack
        .send_to(udp, host(), 9, b"a", bench.now)
        .unwrap();
    bench.receive(&advertisement_na(
        ROUTER_MAC,
        host(),
        ours,
        host(),
        0x60,
        Some(ROUTER_MAC),
    ));
    sent(&mut bench.stack);
    // REACHABLE lasts ReachableTime (randomised, at most 1.5 × 30 s).
    bench.advance(46_000);
    assert_eq!(
        bench.stack.neighbor_state(&host()).unwrap().0,
        NeighborState::Stale
    );
    // Used while stale: sent at once, then DELAY, then PROBE.
    bench
        .stack
        .send_to(udp, host(), 9, b"b", bench.now)
        .unwrap();
    let out = sent(&mut bench.stack);
    assert_eq!(out.iter().filter(|s| s.next == 17).count(), 1);
    assert_eq!(
        bench.stack.neighbor_state(&host()).unwrap().0,
        NeighborState::Delay
    );
    let out = bench.advance(5_000);
    assert_eq!(
        bench.stack.neighbor_state(&host()).unwrap().0,
        NeighborState::Probe
    );
    let probe = icmp_of(&out, 135);
    assert_eq!(probe.len(), 1);
    assert_eq!(probe[0].dst, host(), "unicast probe");
    assert_eq!(probe[0].dst_mac, ROUTER_MAC);
    // An answer makes it reachable again...
    bench.receive(&advertisement_na(
        ROUTER_MAC,
        host(),
        ours,
        host(),
        0x40,
        None,
    ));
    assert_eq!(
        bench.stack.neighbor_state(&host()).unwrap().0,
        NeighborState::Reachable
    );
    // ...silence removes it after three probes.
    bench.advance(46_000);
    bench
        .stack
        .send_to(udp, host(), 9, b"c", bench.now)
        .unwrap();
    let probes = bench.advance(5_000 + 3_500);
    assert_eq!(icmp_of(&probes, 135).len(), 3);
    assert_eq!(bench.stack.neighbor_state(&host()), None);
}

#[test]
fn unanswered_resolution_gives_up() {
    let mut bench = Bench::configured();
    let udp = bench.stack.udp_bind(0).unwrap();
    let nobody = v6("fec0::99");
    let dropped = bench.stack.stats().dropped;
    for _ in 0..3 {
        bench
            .stack
            .send_to(udp, nobody, 9, b"x", bench.now)
            .unwrap();
    }
    let mut solicitations = icmp_of(&sent(&mut bench.stack), 135).len();
    solicitations += icmp_of(&bench.advance(3_100), 135).len();
    assert_eq!(solicitations, 3, "MAX_MULTICAST_SOLICIT");
    assert_eq!(bench.stack.neighbor_state(&nobody), None);
    assert_eq!(bench.stack.stats().dropped, dropped + 3, "queued packets");
    // At most a few packets wait per neighbor.
    for _ in 0..4 {
        bench
            .stack
            .send_to(udp, nobody, 9, b"x", bench.now)
            .unwrap();
    }
    assert_eq!(
        bench.stack.send_to(udp, nobody, 9, b"x", bench.now),
        Err(NetError::NoBuffers)
    );
}

#[test]
fn echo_requests_are_answered() {
    let mut bench = Bench::configured();
    let ours = bench.slaac();
    bench.know_host();
    let request = [128, 0, 0, 0, 0x12, 0x34, 0, 7, b'p', b'i', b'n', b'g'];
    let out = bench.receive(&icmp_frame(OURS, ROUTER_MAC, host(), ours, 64, &request));
    let reply = icmp_of(&out, 129);
    assert_eq!(reply.len(), 1);
    assert_eq!((reply[0].src, reply[0].dst), (ours, host()));
    assert_eq!(reply[0].payload[4..], request[4..]);
    // To all nodes: answered from the link-local address.
    let out = bench.receive(&icmp_frame(
        multicast_mac(&all_nodes()),
        ROUTER_MAC,
        router_ll(),
        all_nodes(),
        64,
        &request,
    ));
    let reply = icmp_of(&out, 129);
    assert_eq!(reply[0].src, bench.link_local());
    assert_eq!(reply[0].dst, router_ll());
    // Not to a tentative address.
    let mut fresh = Bench::new();
    let tentative = fresh.link_local();
    let out = fresh.receive(&icmp_frame(
        OURS,
        ROUTER_MAC,
        router_ll(),
        tentative,
        64,
        &request,
    ));
    assert!(out.is_empty());
}

#[test]
fn udp_over_ipv6() {
    let mut bench = Bench::configured();
    let ours = bench.slaac();
    bench.know_host();
    let socket = bench.stack.udp_bind(5353).unwrap();
    let segment = udp6(host(), ours, 53, 5353, b"answer");
    bench.receive(&eth(OURS, ROUTER_MAC, &ip6(host(), ours, 17, 64, &segment)));
    assert_eq!(
        bench.stack.recv(socket),
        Some(Datagram {
            from: IpAddr::V6(host()),
            port: 53,
            data: b"answer".to_vec(),
        })
    );
    // No checksum (mandatory in IPv6), or a wrong one: dropped.
    let mut unchecked = segment.clone();
    unchecked[6..8].copy_from_slice(&[0, 0]);
    let mut wrong = segment.clone();
    wrong[8] ^= 0xff;
    for bad in [unchecked, wrong] {
        bench.receive(&eth(OURS, ROUTER_MAC, &ip6(host(), ours, 17, 64, &bad)));
        assert_eq!(bench.stack.recv(socket), None);
    }
    // Sending: the checksum is checked by `parse`.
    bench
        .stack
        .send_to(socket, host(), 53, b"query", bench.now)
        .unwrap();
    let out = sent(&mut bench.stack);
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].src, out[0].dst, out[0].next), (ours, host(), 17));
    assert_eq!(out[0].payload[8..], *b"query");
    // To link-local destinations, from the link-local address.
    bench
        .stack
        .send_to(socket, router_ll(), 53, b"q", bench.now)
        .unwrap();
    let out = sent(&mut bench.stack);
    assert_eq!(out[0].src, bench.link_local());
    // A closed port: destination unreachable (port), quoting the packet.
    let closed = udp6(host(), ours, 53, 999, b"x");
    let packet = ip6(host(), ours, 17, 64, &closed);
    let out = bench.receive(&eth(OURS, ROUTER_MAC, &packet));
    let error = icmp_of(&out, 1);
    assert_eq!(error.len(), 1);
    assert_eq!(error[0].payload[1], 4);
    assert_eq!(error[0].payload[8..], packet[..]);
    // Too large for one datagram.
    assert_eq!(
        bench.stack.send_to(
            socket,
            host(),
            53,
            &vec![0; MAX_UDP6_PAYLOAD + 1],
            bench.now
        ),
        Err(NetError::TooLarge)
    );
}

#[test]
fn icmp_errors_are_rate_limited() {
    let mut bench = Bench::configured();
    let ours = bench.slaac();
    bench.know_host();
    let closed = udp6(host(), ours, 53, 999, b"x");
    let frame = eth(OURS, ROUTER_MAC, &ip6(host(), ours, 17, 64, &closed));
    let mut errors = 0;
    for _ in 0..50 {
        errors += icmp_of(&bench.receive(&frame), 1).len();
    }
    assert_eq!(errors, 10, "a burst of ten");
    bench.advance(1_000);
    let more: usize = (0..50)
        .map(|_| icmp_of(&bench.receive(&frame), 1).len())
        .sum();
    assert_eq!(more, 10, "refilled at ten per second");
}

#[test]
fn extension_headers() {
    let mut bench = Bench::configured();
    let ours = bench.slaac();
    bench.know_host();
    let socket = bench.stack.udp_bind(5353).unwrap();
    let segment = udp6(host(), ours, 53, 5353, b"through");
    let send = |bench: &mut Bench, next: u8, headers: &[u8]| {
        let mut payload = headers.to_vec();
        payload.extend_from_slice(&segment);
        bench.receive(&eth(
            OURS,
            ROUTER_MAC,
            &ip6(host(), ours, next, 64, &payload),
        ))
    };
    // Hop-by-hop (PadN), then destination options (Pad1s): skipped.
    send(
        &mut bench,
        0,
        &[60, 0, 1, 4, 0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(bench.stack.recv(socket).is_some());
    // An atomic fragment is a whole packet.
    send(&mut bench, 44, &[17, 0, 0, 0, 1, 2, 3, 4]);
    assert!(bench.stack.recv(socket).is_some());
    // A real fragment is not reassembled.
    assert!(send(&mut bench, 44, &[17, 0, 0, 1, 1, 2, 3, 4]).is_empty());
    assert!(send(&mut bench, 44, &[17, 0, 0, 8, 1, 2, 3, 4]).is_empty());
    assert!(bench.stack.recv(socket).is_none());
    // A routing header with segments left: parameter problem at its type.
    let out = send(&mut bench, 43, &[17, 0, 0, 1, 0, 0, 0, 0]);
    let problem = icmp_of(&out, 4);
    assert_eq!(problem.len(), 1);
    assert_eq!(
        (problem[0].payload[1], be32(&problem[0].payload, 4)),
        (0, 42)
    );
    // ...but one with none left is skipped.
    send(&mut bench, 43, &[17, 0, 0, 0, 0, 0, 0, 0]);
    assert!(bench.stack.recv(socket).is_some());
    // Unknown options: by their two high bits.
    assert!(send(&mut bench, 60, &[17, 0, 0x3e, 2, 0, 0, 1, 0]).is_empty());
    assert!(bench.stack.recv(socket).is_some(), "00: skipped");
    assert!(send(&mut bench, 60, &[17, 0, 0x7e, 2, 0, 0, 1, 0]).is_empty());
    assert!(bench.stack.recv(socket).is_none(), "01: dropped silently");
    let out = send(&mut bench, 60, &[17, 0, 0xbe, 2, 0, 0, 1, 0]);
    let problem = icmp_of(&out, 4);
    assert_eq!(
        (problem[0].payload[1], be32(&problem[0].payload, 4)),
        (2, 42)
    );
    // An unknown next header: parameter problem at the field naming it.
    let out = send(&mut bench, 253, &[]);
    let problem = icmp_of(&out, 4);
    assert_eq!(
        (problem[0].payload[1], be32(&problem[0].payload, 4)),
        (1, 6)
    );
    // Hop-by-hop anywhere but first.
    let out = send(
        &mut bench,
        60,
        &[0, 0, 1, 4, 0, 0, 0, 0, 17, 0, 1, 4, 0, 0, 0, 0],
    );
    let problem = icmp_of(&out, 4);
    assert_eq!(
        (problem[0].payload[1], be32(&problem[0].payload, 4)),
        (1, 40)
    );
    // A header running past the packet.
    assert!(send(&mut bench, 60, &[17, 9, 0, 0]).is_empty());
    assert!(bench.stack.recv(socket).is_none());
}

#[test]
fn answers_mld_queries() {
    let (mut bench, _) = Bench::up();
    bench.advance(3_000);
    // A general query (MLDv2 form), maximum response delay 1000 ms.
    let mut query = StdVec::from([130, 0, 0, 0, 0x03, 0xe8, 0, 0]);
    query.extend_from_slice(&[0; 16]);
    query.extend_from_slice(&[0, 0, 0, 0]);
    let query = checksummed(router_ll(), all_nodes(), 58, query, 2);
    let mut payload = StdVec::from([58, 0, 5, 2, 0, 0, 1, 0]);
    payload.extend_from_slice(&query);
    bench.receive(&eth(
        multicast_mac(&all_nodes()),
        ROUTER_MAC,
        &ip6(router_ll(), all_nodes(), 0, 1, &payload),
    ));
    let reports = icmp_of(&bench.advance(1_100), 143)
        .into_iter()
        .map(|r| r.payload.clone())
        .collect::<StdVec<_>>();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0][8], 2, "MODE_IS_EXCLUDE: current state");
    assert_eq!(reports[0][12..28], solicited_node(&bench.link_local()));
}

#[test]
fn hostile_frames_never_panic() {
    let bench = Bench::configured();
    let ours = bench.slaac();
    let samples = [
        slirp_advertisement(),
        solicitation(
            router_ll(),
            solicited_node(&ours),
            ours,
            Some(ROUTER_MAC),
            255,
        ),
        advertisement_na(ROUTER_MAC, host(), ours, host(), 0x60, Some(ROUTER_MAC)),
        eth(
            OURS,
            ROUTER_MAC,
            &ip6(host(), ours, 0, 64, &{
                let mut p = StdVec::from([60, 0, 1, 4, 0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0]);
                p.extend_from_slice(&udp6(host(), ours, 53, 5353, b"x"));
                p
            }),
        ),
    ];
    for sample in samples {
        // One stack per sample: whatever the mutations do to its state,
        // nothing may panic.
        let mut bench = Bench::configured();
        for at in 14..sample.len() {
            for value in [0u8, 1, 2, 0x3a, 0x80, 0xff] {
                let mut mutated = sample.clone();
                mutated[at] = value;
                bench.stack.receive(&mutated, bench.now);
                bench.advance(50);
            }
            bench.stack.receive(&sample[..at], bench.now);
        }
    }
}

/// Two stacks with IPv6 on one wire.
struct Wire {
    a: Stack,
    b: Stack,
    now: u64,
    /// IPv6 frames each way, sizes included.
    largest_from_a: usize,
}

impl Wire {
    fn new() -> Self {
        let mut a = Stack::new([2, 0, 0, 0, 0, 0xa]);
        let mut b = Stack::new([2, 0, 0, 0, 0, 0xb]);
        a.set_secret(1);
        b.set_secret(2);
        a.enable_ipv6([1; 16], 0);
        b.enable_ipv6([2; 16], 0);
        let mut wire = Self {
            a,
            b,
            now: 0,
            largest_from_a: 0,
        };
        wire.advance(2_500);
        wire
    }

    fn pump(&mut self) {
        for _ in 0..10_000 {
            let mut moved = false;
            while let Some(frame) = self.a.transmit() {
                moved = true;
                self.largest_from_a = self.largest_from_a.max(frame.len());
                self.b.receive(&frame, self.now);
            }
            while let Some(frame) = self.b.transmit() {
                moved = true;
                self.a.receive(&frame, self.now);
            }
            if !moved {
                return;
            }
        }
        panic!("the wire never went quiet");
    }

    fn advance(&mut self, ms: u64) {
        let end = self.now + ms;
        while self.now < end {
            self.now = (self.now + 10).min(end);
            self.a.poll(self.now);
            self.b.poll(self.now);
            self.pump();
        }
    }
}

fn link_local_of(stack: &Stack) -> Ipv6 {
    let address = stack.ipv6_config().unwrap().addresses[0];
    assert_eq!(address.state, AddressState::Preferred);
    address.address
}

#[test]
fn tcp_over_ipv6() {
    let mut wire = Wire::new();
    let (a, b) = (link_local_of(&wire.a), link_local_of(&wire.b));
    let listener = wire.b.tcp_listen(80).unwrap();
    let client = wire.a.tcp_connect(b, 80, wire.now).unwrap();
    wire.advance(100);
    assert_eq!(
        wire.a.tcp_status(client),
        Some((TcpState::Established, None))
    );
    let server = wire.b.tcp_accept(listener).unwrap().expect("accepted");
    let data: StdVec<u8> = (0..60_000u32).map(|i| (i % 253) as u8).collect();
    let (mut sent_bytes, mut received) = (0, StdVec::new());
    let mut buffer = [0u8; 4096];
    for _ in 0..10_000 {
        if sent_bytes < data.len() {
            sent_bytes += wire
                .a
                .tcp_send(client, &data[sent_bytes..], wire.now)
                .unwrap();
        }
        wire.advance(10);
        while let Recv::Data(n) = wire.b.tcp_recv(server, &mut buffer, wire.now).unwrap() {
            received.extend_from_slice(&buffer[..n]);
        }
        if received.len() == data.len() {
            break;
        }
    }
    assert_eq!(received, data);
    // Full-sized segments: 1500-byte packets (MSS 1440) in 1514-byte frames.
    assert_eq!(wire.largest_from_a, 1514);
    // The server saw the client's address.
    let remote = wire.b.tcp(server).unwrap().remote;
    assert_eq!(remote, IpAddr::V6(a));

    // A smaller path MTU (Packet Too Big) shrinks the segments.
    let mut quoted = ip6(a, b, 6, 64, &[0; 20]);
    quoted.truncate(48);
    let mut too_big = StdVec::from([2, 0, 0, 0, 0, 0, 0x05, 0x00]);
    too_big.extend_from_slice(&quoted);
    let router = v6("fe80::1");
    let frame = icmp_frame([2, 0, 0, 0, 0, 0xa], ROUTER_MAC, router, a, 64, &too_big);
    wire.a.receive(&frame, wire.now);
    wire.largest_from_a = 0;
    wire.a.tcp_send(client, &data[..10_000], wire.now).unwrap();
    wire.advance(500);
    assert_eq!(wire.largest_from_a, 14 + 1280);

    // Closing, both ways.
    wire.a.tcp_shutdown(client, wire.now).unwrap();
    wire.advance(100);
    let mut rest = [0u8; 16_384];
    while let Recv::Data(_) = wire.b.tcp_recv(server, &mut rest, wire.now).unwrap() {}
    assert_eq!(wire.b.tcp_recv(server, &mut rest, wire.now), Ok(Recv::Eof));
    wire.b.tcp_shutdown(server, wire.now).unwrap();
    wire.advance(100);
    assert_eq!(wire.a.tcp_recv(client, &mut rest, wire.now), Ok(Recv::Eof));
}

#[test]
fn tcp_to_a_closed_ipv6_port_is_refused() {
    let mut wire = Wire::new();
    let b = link_local_of(&wire.b);
    let client = wire.a.tcp_connect(b, 81, wire.now).unwrap();
    wire.advance(100);
    assert_eq!(
        wire.a.tcp_status(client),
        Some((TcpState::Closed, Some(TcpError::Refused)))
    );
    // And IPv6 without a route: refused at once.
    assert_eq!(
        wire.a.tcp_connect(v6("2001:db8::1"), 80, wire.now),
        Err(NetError::NotConfigured)
    );
}

#[test]
fn source_selection_prefers_matching_scope() {
    let mut bench = Bench::configured();
    // A second prefix: the longest match wins among global addresses.
    bench.receive(&advertisement(
        1800,
        &[prefix_option(v6("2001:db8::"), 64, 0xc0, 86_400, 14_400)],
    ));
    bench.advance(1_100);
    let addresses = bench.config().addresses;
    let global = |prefix: &str| {
        addresses
            .iter()
            .find(|a| a.address[..8] == v6(prefix)[..8])
            .unwrap()
            .address
    };
    let v6state = |bench: &Bench, dst: &str| bench.stack.route6(&v6(dst)).unwrap().0;
    assert_eq!(v6state(&bench, "2001:db8::99"), global("2001:db8::"));
    assert_eq!(v6state(&bench, "fec0::99"), global("fec0::"));
    assert_eq!(v6state(&bench, "fe80::99"), bench.link_local());
    assert_eq!(v6state(&bench, "ff02::1"), bench.link_local());
}
