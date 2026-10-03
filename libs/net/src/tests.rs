extern crate std;

use super::*;
use std::vec::Vec as StdVec;

const OURS: Mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
const PEER: Mac = [0x52, 0x55, 10, 0, 2, 2];
const ADDRESS: Ipv4 = [10, 0, 2, 15];
const GATEWAY: Ipv4 = [10, 0, 2, 2];
const DNS: Ipv4 = [10, 0, 2, 3];

fn configured() -> Stack {
    let mut stack = Stack::new(OURS);
    stack.configure(Config {
        address: ADDRESS,
        prefix: 24,
        gateway: Some(GATEWAY),
        dns: Some(DNS),
    });
    stack
}

fn frame(dst: Mac, src: Mac, ethertype: u16, payload: &[u8]) -> StdVec<u8> {
    let mut frame = StdVec::new();
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn ip(src: Ipv4, dst: Ipv4, protocol: u8, payload: &[u8]) -> StdVec<u8> {
    let mut packet = StdVec::new();
    packet.extend_from_slice(&[0x45, 0]);
    packet.extend_from_slice(&((20 + payload.len()) as u16).to_be_bytes());
    packet.extend_from_slice(&[0, 1, 0x40, 0, 64, protocol, 0, 0]);
    packet.extend_from_slice(&src);
    packet.extend_from_slice(&dst);
    let sum = checksum(&packet);
    packet[10..12].copy_from_slice(&sum.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

fn udp_frame(
    src: Ipv4,
    dst: Ipv4,
    sport: u16,
    dport: u16,
    data: &[u8],
    dst_mac: Mac,
) -> StdVec<u8> {
    let segment = udp_segment(src, dst, sport, dport, data);
    frame(
        dst_mac,
        PEER,
        ETHERTYPE_IPV4,
        &ip(src, dst, PROTOCOL_UDP, &segment),
    )
}

fn arp(operation: u16, sender: (Mac, Ipv4), target: (Mac, Ipv4)) -> StdVec<u8> {
    let mut arp = StdVec::from([0, 1, 8, 0, 6, 4]);
    arp.extend_from_slice(&operation.to_be_bytes());
    arp.extend_from_slice(&sender.0);
    arp.extend_from_slice(&sender.1);
    arp.extend_from_slice(&target.0);
    arp.extend_from_slice(&target.1);
    let dst = if operation == 1 {
        BROADCAST_MAC
    } else {
        target.0
    };
    frame(dst, sender.0, ETHERTYPE_ARP, &arp)
}

/// An IP frame the stack sent, checked and taken apart.
struct Sent {
    dst_mac: Mac,
    src: Ipv4,
    dst: Ipv4,
    protocol: u8,
    payload: StdVec<u8>,
}

fn sent_ip(frame: &[u8]) -> Sent {
    assert_eq!(&frame[6..12], &OURS, "source MAC");
    assert_eq!(be16(frame, 12), ETHERTYPE_IPV4);
    let packet = &frame[14..];
    assert_eq!(checksum(&packet[..20]), 0, "IP header checksum");
    let total = usize::from(be16(packet, 2));
    assert_eq!(total, packet.len());
    let sent = Sent {
        dst_mac: frame[..6].try_into().unwrap(),
        src: ip_at(packet, 12),
        dst: ip_at(packet, 16),
        protocol: packet[9],
        payload: packet[20..].to_vec(),
    };
    match sent.protocol {
        PROTOCOL_UDP => {
            let mut sum = checksum_add(0, &sent.src);
            sum = checksum_add(sum, &sent.dst);
            sum += u32::from(PROTOCOL_UDP) + sent.payload.len() as u32;
            assert_eq!(
                checksum_finish(checksum_add(sum, &sent.payload)),
                0,
                "UDP checksum"
            );
        }
        PROTOCOL_ICMP => assert_eq!(checksum(&sent.payload), 0, "ICMP checksum"),
        _ => {}
    }
    sent
}

/// The options of a DHCP message the stack sent.
fn dhcp_option(message: &[u8], code: u8) -> Option<StdVec<u8>> {
    let mut at = 240;
    while at < message.len() && message[at] != 255 {
        if message[at] == 0 {
            at += 1;
            continue;
        }
        let len = usize::from(message[at + 1]);
        if message[at] == code {
            return Some(message[at + 2..at + 2 + len].to_vec());
        }
        at += 2 + len;
    }
    None
}

fn dhcp_reply(kind: u8, xid: u32, chaddr: Mac) -> StdVec<u8> {
    let mut message = vec![0u8; 240];
    message[0] = 2;
    message[1] = 1;
    message[2] = 6;
    message[4..8].copy_from_slice(&xid.to_be_bytes());
    message[16..20].copy_from_slice(&ADDRESS);
    message[20..24].copy_from_slice(&GATEWAY);
    message[28..34].copy_from_slice(&chaddr);
    message[236..240].copy_from_slice(&DHCP_MAGIC);
    message.extend_from_slice(&[53, 1, kind, 54, 4]);
    message.extend_from_slice(&GATEWAY);
    message.extend_from_slice(&[1, 4, 255, 255, 255, 0, 3, 4]);
    message.extend_from_slice(&GATEWAY);
    message.extend_from_slice(&[6, 4]);
    message.extend_from_slice(&DNS);
    message.extend_from_slice(&[51, 4]);
    message.extend_from_slice(&86_400u32.to_be_bytes());
    message.push(255);
    udp_frame(GATEWAY, BROADCAST, 67, 68, &message, BROADCAST_MAC)
}

/// The DHCP message (UDP payload) of a frame the stack sent.
fn sent_dhcp(frame: &[u8]) -> StdVec<u8> {
    let sent = sent_ip(frame);
    assert_eq!(sent.dst_mac, BROADCAST_MAC);
    assert_eq!(sent.dst, BROADCAST);
    assert_eq!(sent.protocol, PROTOCOL_UDP);
    assert_eq!((be16(&sent.payload, 0), be16(&sent.payload, 2)), (68, 67));
    sent.payload[8..].to_vec()
}

#[test]
fn checksum_matches_rfc_1071() {
    assert_eq!(
        checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7]),
        0x220d
    );
    assert_eq!(checksum(&[0xff]), 0x00ff, "odd length pads with zero");
}

#[test]
fn dhcp_discovers_requests_and_configures() {
    let mut stack = Stack::new(OURS);
    assert_eq!(stack.config(), None);
    stack.poll(0);
    let discover = sent_dhcp(&stack.transmit().expect("DISCOVER"));
    assert_eq!(dhcp_option(&discover, 53), Some(vec![DHCP_DISCOVER]));
    assert_eq!(&discover[28..34], &OURS);
    assert!(discover.len() >= 300);
    let xid = u32::from_be_bytes(discover[4..8].try_into().unwrap());

    // Replies for someone else, or another transaction, are ignored.
    stack.receive(&dhcp_reply(DHCP_OFFER, xid ^ 1, OURS), 10);
    stack.receive(&dhcp_reply(DHCP_OFFER, xid, PEER), 10);
    assert!(stack.transmit().is_none());

    stack.receive(&dhcp_reply(DHCP_OFFER, xid, OURS), 10);
    let request = sent_dhcp(&stack.transmit().expect("REQUEST"));
    assert_eq!(dhcp_option(&request, 53), Some(vec![DHCP_REQUEST]));
    assert_eq!(dhcp_option(&request, 50), Some(ADDRESS.to_vec()));
    assert_eq!(dhcp_option(&request, 54), Some(GATEWAY.to_vec()));

    stack.receive(&dhcp_reply(DHCP_ACK, xid, OURS), 20);
    assert_eq!(
        stack.config(),
        Some(Config {
            address: ADDRESS,
            prefix: 24,
            gateway: Some(GATEWAY),
            dns: Some(DNS),
        })
    );
    assert!(stack.dhcp_bound());
    assert_eq!(
        stack.next_deadline(),
        Some(20 + 43_200_000),
        "renew at half the lease"
    );

    // Renewal: unicast-style REQUEST from our address, then a new ACK.
    stack.poll(20 + 43_200_000);
    let renew = sent_dhcp(&stack.transmit().expect("renewal"));
    assert_eq!(&renew[12..16], &ADDRESS, "ciaddr");
    stack.receive(&dhcp_reply(DHCP_ACK, xid, OURS), 43_200_100);
    assert_eq!(stack.next_deadline(), Some(43_200_100 + 43_200_000));

    // A NAK drops the configuration and starts over.
    stack.receive(&dhcp_reply(DHCP_NAK, xid, OURS), 43_200_200);
    assert_eq!(stack.config(), None);
    stack.poll(43_200_200);
    let again = sent_dhcp(&stack.transmit().expect("DISCOVER again"));
    assert_eq!(dhcp_option(&again, 53), Some(vec![DHCP_DISCOVER]));
}

/// The next IPv4 frame the stack sent (IPv6 ones skipped).
fn next_ipv4(stack: &mut Stack) -> Option<StdVec<u8>> {
    while let Some(frame) = stack.transmit() {
        if be16(&frame, 12) != ipv6::ETHERTYPE_IPV6 {
            return Some(frame);
        }
    }
    None
}

#[test]
fn dhcp_configures_with_ipv6_enabled() {
    let mut stack = Stack::new(OURS);
    stack.enable_ipv6([7; 16], 0);
    stack.poll(0);
    let discover = sent_dhcp(&next_ipv4(&mut stack).expect("DISCOVER"));
    let xid = u32::from_be_bytes(discover[4..8].try_into().unwrap());
    stack.receive(&dhcp_reply(DHCP_OFFER, xid, OURS), 10);
    let request = sent_dhcp(&next_ipv4(&mut stack).expect("REQUEST"));
    assert_eq!(dhcp_option(&request, 53), Some(vec![DHCP_REQUEST]));
    stack.receive(&dhcp_reply(DHCP_ACK, xid, OURS), 20);
    assert!(stack.dhcp_bound());
}

#[test]
fn dhcp_retransmits_with_backoff() {
    let mut stack = Stack::new(OURS);
    let mut sent_at = StdVec::new();
    for now in (0..=40_000).step_by(100) {
        stack.poll(now);
        while stack.transmit().is_some() {
            sent_at.push(now);
        }
    }
    assert_eq!(sent_at, [0, 2_000, 6_000, 14_000, 30_000]);
}

#[test]
fn answers_arp_for_its_address_only() {
    let mut stack = configured();
    stack.receive(&arp(1, (PEER, GATEWAY), ([0; 6], [10, 0, 2, 99])), 0);
    assert!(stack.transmit().is_none(), "not our address");
    stack.receive(&arp(1, (PEER, GATEWAY), ([0; 6], ADDRESS)), 0);
    let reply = stack.transmit().expect("ARP reply");
    assert_eq!(&reply[..6], &PEER);
    assert_eq!(be16(&reply, 12), ETHERTYPE_ARP);
    assert_eq!(be16(&reply, 20), 2, "reply");
    assert_eq!(&reply[22..28], &OURS);
    assert_eq!(ip_at(&reply, 28), ADDRESS);
    assert_eq!(ip_at(&reply, 38), GATEWAY);
}

#[test]
fn udp_waits_for_arp_then_goes_to_the_resolved_mac() {
    let mut stack = configured();
    let socket = stack.udp_bind(0).unwrap();
    let port = stack.local_port(socket).unwrap();
    assert!(EPHEMERAL_PORTS.contains(&port));
    stack
        .send_to(socket, [10, 0, 2, 3], 53, b"query", 0)
        .unwrap();
    let request = stack.transmit().expect("ARP request");
    assert_eq!(&request[..6], &BROADCAST_MAC);
    assert_eq!(ip_at(&request, 38), [10, 0, 2, 3]);
    assert!(stack.transmit().is_none(), "the datagram waits");

    let dns_mac = [2, 0, 0, 0, 0, 3];
    stack.receive(&arp(2, (dns_mac, [10, 0, 2, 3]), (OURS, ADDRESS)), 5);
    let sent = sent_ip(&stack.transmit().expect("the datagram"));
    assert_eq!(sent.dst_mac, dns_mac);
    assert_eq!((sent.src, sent.dst), (ADDRESS, [10, 0, 2, 3]));
    assert_eq!(be16(&sent.payload, 0), port);
    assert_eq!(be16(&sent.payload, 2), 53);
    assert_eq!(&sent.payload[8..], b"query");

    // Cached now: the next datagram goes straight out.
    stack
        .send_to(socket, [10, 0, 2, 3], 53, b"again", 10)
        .unwrap();
    assert_eq!(sent_ip(&stack.transmit().unwrap()).dst_mac, dns_mac);
}

#[test]
fn off_link_traffic_goes_through_the_gateway() {
    let mut stack = configured();
    let socket = stack.udp_bind(5000).unwrap();
    stack.send_to(socket, [8, 8, 8, 8], 53, b"x", 0).unwrap();
    let request = stack.transmit().expect("ARP for the gateway");
    assert_eq!(ip_at(&request, 38), GATEWAY);

    let mut isolated = Stack::new(OURS);
    isolated.configure(Config {
        address: ADDRESS,
        prefix: 24,
        gateway: None,
        dns: None,
    });
    let socket = isolated.udp_bind(5000).unwrap();
    assert_eq!(
        isolated.send_to(socket, [8, 8, 8, 8], 53, b"x", 0),
        Err(NetError::NoRoute)
    );
    assert_eq!(
        Stack::new(OURS).send_to(0, GATEWAY, 1, b"", 0),
        Err(NetError::NotConfigured)
    );
}

#[test]
fn unanswered_arp_is_retried_then_given_up() {
    let mut stack = configured();
    let socket = stack.udp_bind(5000).unwrap();
    stack.send_to(socket, [10, 0, 2, 77], 9, b"x", 0).unwrap();
    let mut requests = 0;
    for now in (0..=5_000).step_by(250) {
        stack.poll(now);
        while stack.transmit().is_some() {
            requests += 1;
        }
    }
    assert_eq!(requests, ARP_TRIES);
    assert_eq!(stack.stats().dropped, 1, "the datagram was dropped");
    assert_eq!(stack.next_deadline(), None);
}

#[test]
fn delivers_udp_and_refuses_closed_ports() {
    let mut stack = configured();
    let socket = stack.udp_bind(7).unwrap();
    // Learn the peer first so replies need no ARP.
    stack.receive(&arp(1, (PEER, GATEWAY), ([0; 6], ADDRESS)), 0);
    let _ = stack.transmit();

    stack.receive(&udp_frame(GATEWAY, ADDRESS, 4000, 7, b"echo me", OURS), 1);
    assert_eq!(stack.take_ready(), [socket]);
    assert_eq!(
        stack.recv(socket),
        Some(Datagram {
            from: IpAddr::V4(GATEWAY),
            port: 4000,
            data: b"echo me".to_vec()
        })
    );
    assert_eq!(stack.recv(socket), None);

    stack.receive(&udp_frame(GATEWAY, ADDRESS, 4000, 9, b"nobody", OURS), 2);
    let sent = sent_ip(&stack.transmit().expect("port unreachable"));
    assert_eq!(sent.protocol, PROTOCOL_ICMP);
    assert_eq!(
        &sent.payload[..2],
        &[ICMP_UNREACHABLE, ICMP_PORT_UNREACHABLE]
    );
    // Not for a broadcast.
    stack.receive(
        &udp_frame(GATEWAY, BROADCAST, 4000, 9, b"all", BROADCAST_MAC),
        3,
    );
    assert!(stack.transmit().is_none());
}

#[test]
fn answers_pings_and_sends_its_own() {
    let mut stack = configured();
    stack.receive(&arp(1, (PEER, GATEWAY), ([0; 6], ADDRESS)), 0);
    let _ = stack.transmit();

    let request = icmp_echo(ICMP_ECHO_REQUEST, 0x1234, 7, b"payload");
    stack.receive(
        &frame(
            OURS,
            PEER,
            ETHERTYPE_IPV4,
            &ip(GATEWAY, ADDRESS, PROTOCOL_ICMP, &request),
        ),
        1,
    );
    let reply = sent_ip(&stack.transmit().expect("echo reply"));
    assert_eq!(reply.dst, GATEWAY);
    assert_eq!(reply.payload[0], ICMP_ECHO_REPLY);
    assert_eq!(
        &reply.payload[4..],
        &request[4..],
        "same identifier, sequence and data"
    );

    let ping = stack.ping_open().unwrap();
    let other = stack.ping_open().unwrap();
    stack.send_to(ping, GATEWAY, 0, b"abc", 2).unwrap();
    let echo = sent_ip(&stack.transmit().expect("echo request"));
    assert_eq!(echo.payload[0], ICMP_ECHO_REQUEST);
    let ident = be16(&echo.payload, 4);
    assert_eq!(be16(&echo.payload, 6), 1, "first sequence number");
    assert_eq!(stack.last_sequence(ping), Some(1));

    let answer = icmp_echo(ICMP_ECHO_REPLY, ident, 1, b"abc");
    stack.receive(
        &frame(
            OURS,
            PEER,
            ETHERTYPE_IPV4,
            &ip(GATEWAY, ADDRESS, PROTOCOL_ICMP, &answer),
        ),
        3,
    );
    assert_eq!(stack.take_ready(), [ping], "only the matching socket");
    assert_eq!(stack.recv(ping).unwrap().port, 1);
    assert_eq!(stack.recv(other), None);
}

#[test]
fn malformed_frames_are_dropped_without_effect() {
    let mut stack = configured();
    let socket = stack.udp_bind(7).unwrap();
    let good = udp_frame(GATEWAY, ADDRESS, 4000, 7, b"data", OURS);

    let mut bad_ip_sum = good.clone();
    bad_ip_sum[14 + 10] ^= 1;
    let mut bad_udp_sum = good.clone();
    let last = bad_udp_sum.len() - 1;
    bad_udp_sum[last] ^= 1;
    let mut fragment = good.clone();
    fragment[14 + 6] |= 0x20; // more fragments
    let mut other_mac = good.clone();
    other_mac[0] = 0x02;
    let mut long_udp = good.clone();
    long_udp[14 + 20 + 4] = 0xff;
    let truncated = good[..30].to_vec();
    for frame in [
        &bad_ip_sum,
        &bad_udp_sum,
        &fragment,
        &other_mac,
        &long_udp,
        &truncated,
    ] {
        stack.receive(frame, 0);
    }
    assert_eq!(stack.recv(socket), None);
    assert!(stack.transmit().is_none());

    // Every single-byte mutation of real traffic is handled without panic.
    let mut corpus = StdVec::from([
        good,
        arp(1, (PEER, GATEWAY), ([0; 6], ADDRESS)),
        dhcp_reply(DHCP_OFFER, 1, OURS),
        frame(
            OURS,
            PEER,
            ETHERTYPE_IPV4,
            &ip(GATEWAY, ADDRESS, PROTOCOL_ICMP, &icmp_echo(8, 1, 1, b"x")),
        ),
    ]);
    corpus.push(StdVec::new());
    for sample in &corpus {
        for at in 0..sample.len() {
            for value in [0u8, 1, 0x45, 0x7f, 0x80, 0xff] {
                let mut mutated = sample.clone();
                mutated[at] = value;
                stack.receive(&mutated, 1);
                stack.receive(&mutated[..at], 1);
            }
        }
    }
    while stack.transmit().is_some() {}
}

#[test]
fn ports_and_limits() {
    let mut stack = configured();
    let first = stack.udp_bind(53).unwrap();
    assert_eq!(stack.udp_bind(53), Err(NetError::AddressInUse));
    assert_eq!(
        stack.udp_bind(68),
        Err(NetError::AddressInUse),
        "the DHCP client's"
    );
    stack.close(first);
    assert!(stack.udp_bind(53).is_ok(), "closed ports are free again");
    let a = stack.udp_bind(0).unwrap();
    let b = stack.udp_bind(0).unwrap();
    assert_ne!(stack.local_port(a), stack.local_port(b));
    while stack.udp_bind(0).is_ok() {}
    assert_eq!(
        stack.ping_open(),
        Err(NetError::NoBuffers),
        "socket table full"
    );

    let mut stack = configured();
    let socket = stack.udp_bind(7).unwrap();
    for _ in 0..SOCKET_QUEUE + 8 {
        stack.receive(&udp_frame(GATEWAY, ADDRESS, 1, 7, b"x", OURS), 0);
    }
    let mut queued = 0;
    while stack.recv(socket).is_some() {
        queued += 1;
    }
    assert_eq!(queued, SOCKET_QUEUE);
    assert!(stack.stats().dropped >= 8);
    assert_eq!(
        stack.send_to(socket, GATEWAY, 1, &vec![0u8; MAX_UDP_PAYLOAD + 1], 0),
        Err(NetError::TooLarge)
    );
}

#[test]
fn parses_and_prints_addresses() {
    assert_eq!(parse_ipv4("10.0.2.2"), Some([10, 0, 2, 2]));
    assert_eq!(parse_ipv4("255.255.255.255"), Some(BROADCAST));
    for bad in [
        "",
        "1.2.3",
        "1.2.3.4.5",
        "256.0.0.1",
        "1..2.3",
        "a.b.c.d",
        "1.2.3.+4",
        "0001.2.3.4",
    ] {
        assert_eq!(parse_ipv4(bad), None, "{bad}");
    }
    assert_eq!(std::format!("{}", Dotted(ADDRESS)), "10.0.2.15");
    let config = configured().config().unwrap();
    assert!(config.on_link(GATEWAY) && !config.on_link([10, 0, 3, 1]));
    assert_eq!(config.subnet_broadcast(), [10, 0, 2, 255]);
}
