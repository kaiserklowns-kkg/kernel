//! TCP between two stacks joined by a simulated wire that can lose frames.

extern crate std;

use super::*;
use std::vec::Vec as StdVec;

const MAC_A: Mac = [2, 0, 0, 0, 0, 0xa];
const MAC_B: Mac = [2, 0, 0, 0, 0, 0xb];
const A: Ipv4 = [10, 0, 2, 15];
const B: Ipv4 = [10, 0, 2, 2];
const PORT: u16 = 7000;

struct Wire {
    a: Stack,
    b: Stack,
    now: u64,
    /// Data-carrying TCP frames from A to drop.
    drop_data_from_a: usize,
    /// Everything is lost (the cable is pulled).
    unplugged: bool,
    /// Largest frame seen.
    largest: usize,
}

fn stack(mac: Mac, address: Ipv4, secret: u64) -> Stack {
    let mut stack = Stack::new(mac);
    stack.configure(Config {
        address,
        prefix: 24,
        gateway: None,
        dns: None,
    });
    stack.set_secret(secret);
    stack
}

/// Whether `frame` is a TCP segment with payload.
fn carries_tcp_data(frame: &[u8]) -> bool {
    if frame.len() < 14 + 20 + 20 || be16(frame, 12) != ETHERTYPE_IPV4 || frame[14 + 9] != 6 {
        return false;
    }
    let ip_len = usize::from(be16(frame, 16));
    let tcp_offset = usize::from(frame[14 + 20 + 12] >> 4) * 4;
    ip_len > 20 + tcp_offset
}

impl Wire {
    fn new() -> Self {
        Self {
            a: stack(MAC_A, A, 1),
            b: stack(MAC_B, B, 2),
            now: 0,
            drop_data_from_a: 0,
            unplugged: false,
            largest: 0,
        }
    }

    /// Delivers frames both ways until the wire is quiet.
    fn pump(&mut self) {
        for _ in 0..10_000 {
            let mut moved = false;
            while let Some(frame) = self.a.transmit() {
                moved = true;
                self.largest = self.largest.max(frame.len());
                if self.unplugged {
                    continue;
                }
                if self.drop_data_from_a > 0 && carries_tcp_data(&frame) {
                    self.drop_data_from_a -= 1;
                    continue;
                }
                self.b.receive(&frame, self.now);
            }
            while let Some(frame) = self.b.transmit() {
                moved = true;
                self.largest = self.largest.max(frame.len());
                if !self.unplugged {
                    self.a.receive(&frame, self.now);
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the wire never went quiet");
    }

    fn advance(&mut self, ms: u64) {
        self.now += ms;
        self.a.poll(self.now);
        self.b.poll(self.now);
        self.pump();
    }

    /// A connection from A to a listener on B: (A's socket, B's accepted
    /// socket, B's listener).
    fn connect(&mut self) -> (SocketId, SocketId, SocketId) {
        let listener = self.b.tcp_listen(PORT).unwrap();
        let client = self.a.tcp_connect(B, PORT, self.now).unwrap();
        assert_eq!(self.a.tcp_status(client).unwrap().0, TcpState::SynSent);
        self.pump();
        assert_eq!(
            self.a.tcp_status(client),
            Some((TcpState::Established, None))
        );
        assert!(
            self.b.take_ready().contains(&listener),
            "listener signalled"
        );
        let server = self.b.tcp_accept(listener).unwrap().expect("accepted");
        assert_eq!(
            self.b.tcp_status(server),
            Some((TcpState::Established, None))
        );
        (client, server, listener)
    }

    /// Moves `data` from A's `from` to B's `to`, reading as it arrives.
    fn transfer(&mut self, from: SocketId, to: SocketId, data: &[u8]) -> StdVec<u8> {
        let mut sent = 0;
        let mut received = StdVec::new();
        let mut buffer = [0u8; 1000];
        for _ in 0..100_000 {
            if sent < data.len() {
                sent += self.a.tcp_send(from, &data[sent..], self.now).unwrap();
            }
            self.pump();
            while let Recv::Data(n) = self.b.tcp_recv(to, &mut buffer, self.now).unwrap() {
                received.extend_from_slice(&buffer[..n]);
            }
            self.pump();
            if received.len() == data.len() {
                return received;
            }
            self.advance(10);
        }
        panic!(
            "transfer stalled at {} of {} bytes",
            received.len(),
            data.len()
        );
    }
}

fn pattern(len: usize) -> StdVec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

#[test]
fn connects_transfers_both_ways_and_closes() {
    let mut wire = Wire::new();
    let (client, server, listener) = wire.connect();

    let data = pattern(100_000);
    assert_eq!(wire.transfer(client, server, &data), data);
    assert!(wire.largest <= ETH_HEADER + MTU, "segments respect the MSS");

    // B answers on the same connection.
    let reply = b"thanks";
    assert_eq!(
        wire.b.tcp_send(server, reply, wire.now).unwrap(),
        reply.len()
    );
    wire.pump();
    let mut buffer = [0u8; 16];
    assert_eq!(
        wire.a.tcp_recv(client, &mut buffer, wire.now).unwrap(),
        Recv::Data(6)
    );
    assert_eq!(&buffer[..6], reply);

    // A closes first: B sees end of stream, then closes too.
    wire.a.tcp_shutdown(client, wire.now).unwrap();
    wire.pump();
    assert_eq!(
        wire.b.tcp_recv(server, &mut buffer, wire.now).unwrap(),
        Recv::Eof
    );
    assert_eq!(wire.b.tcp_status(server).unwrap().0, TcpState::CloseWait);
    assert_eq!(
        wire.a.tcp_send(client, b"x", wire.now),
        Err(NetError::Closed)
    );
    wire.b.tcp_shutdown(server, wire.now).unwrap();
    wire.pump();
    assert_eq!(wire.b.tcp_status(server).unwrap().0, TcpState::Closed);
    assert_eq!(wire.a.tcp_status(client).unwrap().0, TcpState::TimeWait);
    assert_eq!(
        wire.a.tcp_recv(client, &mut buffer, wire.now).unwrap(),
        Recv::Eof
    );
    wire.advance(11_000);
    assert_eq!(wire.a.tcp_status(client).unwrap().0, TcpState::Closed);

    for (stack, ids) in [
        (&mut wire.a, [client].as_slice()),
        (&mut wire.b, &[server, listener]),
    ] {
        for &id in ids {
            stack.release(id, 0);
        }
        assert!(
            stack.sockets.iter().all(Option::is_none),
            "everything freed"
        );
    }
}

#[test]
fn connecting_to_a_closed_port_is_refused() {
    let mut wire = Wire::new();
    let client = wire.a.tcp_connect(B, 9, wire.now).unwrap();
    wire.pump();
    assert_eq!(
        wire.a.tcp_status(client),
        Some((TcpState::Closed, Some(TcpError::Refused)))
    );
    assert_eq!(
        wire.a.tcp_send(client, b"x", wire.now),
        Err(NetError::Refused)
    );
}

#[test]
fn lost_segments_are_retransmitted() {
    let mut wire = Wire::new();
    let (client, server, _) = wire.connect();
    wire.drop_data_from_a = 3;
    let data = pattern(5_000);
    assert_eq!(wire.transfer(client, server, &data), data);
    assert_eq!(wire.drop_data_from_a, 0, "the losses happened");
}

#[test]
fn a_silent_peer_times_out() {
    let mut wire = Wire::new();
    let (client, _, _) = wire.connect();
    wire.unplugged = true;
    wire.a.tcp_send(client, b"anyone there?", wire.now).unwrap();
    for _ in 0..400 {
        wire.advance(1_000);
    }
    assert_eq!(
        wire.a.tcp_status(client),
        Some((TcpState::Closed, Some(TcpError::TimedOut)))
    );
    let mut buffer = [0u8; 4];
    assert_eq!(
        wire.a.tcp_recv(client, &mut buffer, wire.now),
        Err(NetError::TimedOut)
    );
}

#[test]
fn an_unanswered_syn_times_out() {
    let mut wire = Wire::new();
    wire.unplugged = true;
    let client = wire.a.tcp_connect(B, PORT, wire.now).unwrap();
    for _ in 0..200 {
        wire.advance(1_000);
    }
    assert_eq!(
        wire.a.tcp_status(client),
        Some((TcpState::Closed, Some(TcpError::TimedOut)))
    );
}

#[test]
fn a_full_receiver_stalls_the_sender_until_it_reads() {
    let mut wire = Wire::new();
    let (client, server, _) = wire.connect();
    let data = pattern(3 * TCP_BUFFER);
    let mut sent = 0;
    for _ in 0..50 {
        sent += wire.a.tcp_send(client, &data[sent..], wire.now).unwrap();
        wire.advance(10);
    }
    // B never read: its window closed after one buffer, A's buffer is full.
    assert_eq!(sent, 2 * TCP_BUFFER, "one buffer in flight, one queued");
    assert_eq!(wire.b.tcp(server).unwrap().recv.len(), TCP_BUFFER);
    // Probes keep the connection alive while the window is closed.
    for _ in 0..60 {
        wire.advance(1_000);
    }
    assert_eq!(
        wire.a.tcp_status(client),
        Some((TcpState::Established, None))
    );
    // Reading reopens the window and the rest flows.
    let mut received = StdVec::new();
    let mut buffer = [0u8; 4096];
    for _ in 0..10_000 {
        if sent < data.len() {
            sent += wire.a.tcp_send(client, &data[sent..], wire.now).unwrap();
        }
        while let Recv::Data(n) = wire.b.tcp_recv(server, &mut buffer, wire.now).unwrap() {
            received.extend_from_slice(&buffer[..n]);
        }
        if received.len() == data.len() {
            break;
        }
        wire.advance(50);
    }
    assert_eq!(received, data);
}

#[test]
fn releasing_with_unread_data_resets_the_peer() {
    let mut wire = Wire::new();
    let (client, server, _) = wire.connect();
    wire.a.tcp_send(client, b"never read", wire.now).unwrap();
    wire.pump();
    wire.b.release(server, wire.now);
    wire.pump();
    let mut buffer = [0u8; 4];
    assert_eq!(
        wire.a.tcp_recv(client, &mut buffer, wire.now),
        Err(NetError::Reset)
    );
    assert!(wire.b.tcp(server).is_none(), "freed at once");
}

#[test]
fn graceful_release_finishes_in_the_background() {
    let mut wire = Wire::new();
    let (client, server, listener) = wire.connect();
    wire.a.tcp_send(client, b"last words", wire.now).unwrap();
    wire.a.release(client, wire.now); // queued data still goes, then FIN
    wire.pump();
    let mut buffer = [0u8; 32];
    assert_eq!(
        wire.b.tcp_recv(server, &mut buffer, wire.now).unwrap(),
        Recv::Data(10)
    );
    assert_eq!(
        wire.b.tcp_recv(server, &mut buffer, wire.now).unwrap(),
        Recv::Eof
    );
    wire.b.release(server, wire.now);
    wire.b.release(listener, wire.now);
    wire.advance(11_000);
    assert!(
        wire.a.sockets.iter().all(Option::is_none),
        "orphan freed after TIME_WAIT"
    );
    assert!(wire.b.sockets.iter().all(Option::is_none));
}

#[test]
fn the_backlog_is_bounded_and_listeners_reset_what_they_drop() {
    let mut wire = Wire::new();
    let listener = wire.b.tcp_listen(PORT).unwrap();
    let clients: StdVec<SocketId> = (0..BACKLOG + 2)
        .map(|_| wire.a.tcp_connect(B, PORT, wire.now).unwrap())
        .collect();
    wire.pump();
    let established = clients
        .iter()
        .filter(|&&c| wire.a.tcp_status(c).unwrap().0 == TcpState::Established)
        .count();
    assert_eq!(
        established, BACKLOG,
        "SYNs beyond the backlog are not answered"
    );
    // Releasing the listener resets the connections nobody accepted.
    wire.b.release(listener, wire.now);
    wire.pump();
    let reset = clients
        .iter()
        .filter(|&&c| wire.a.tcp_status(c).unwrap().1 == Some(TcpError::Reset))
        .count();
    assert_eq!(reset, BACKLOG);
    assert_eq!(
        wire.b.tcp_listen(PORT).err(),
        None,
        "the port is free again"
    );
}

#[test]
fn ports_are_exclusive_and_sequence_numbers_differ() {
    let mut wire = Wire::new();
    let listener = wire.b.tcp_listen(PORT).unwrap();
    assert_eq!(wire.b.tcp_listen(PORT), Err(NetError::AddressInUse));
    assert_eq!(wire.b.tcp_listen(0), Err(NetError::BadSocket));
    let one = wire.a.tcp_connect(B, PORT, 0).unwrap();
    let two = wire.a.tcp_connect(B, PORT, 0).unwrap();
    let (one, two) = (wire.a.tcp(one).unwrap(), wire.a.tcp(two).unwrap());
    assert_ne!(one.local_port, two.local_port);
    assert_ne!(one.iss, two.iss, "keyed by the connection's ports");
    let _ = listener;
    assert_eq!(
        Stack::new(MAC_A).tcp_connect(B, PORT, 0),
        Err(NetError::NotConfigured)
    );
}

#[test]
fn mangled_segments_never_panic() {
    // Record a real exchange, then replay every single-byte mutation of
    // each TCP frame into a fresh pair.
    let mut wire = Wire::new();
    let listener = wire.b.tcp_listen(PORT).unwrap();
    let client = wire.a.tcp_connect(B, PORT, 0).unwrap();
    let mut frames = StdVec::new();
    for _ in 0..20 {
        while let Some(frame) = wire.a.transmit() {
            frames.push(frame.clone());
            wire.b.receive(&frame, 0);
        }
        while let Some(frame) = wire.b.transmit() {
            frames.push(frame.clone());
            wire.a.receive(&frame, 0);
        }
        let _ = wire.a.tcp_send(client, b"payload", 0);
        let _ = wire.b.tcp_accept(listener);
    }
    let tcp_frames: StdVec<_> = frames
        .into_iter()
        .filter(|f| f.len() > 34 && f[23] == 6)
        .collect();
    assert!(!tcp_frames.is_empty());
    let mut target = Wire::new();
    target.b.tcp_listen(PORT).unwrap();
    target.a.tcp_listen(PORT).unwrap();
    for frame in &tcp_frames {
        for at in 14..frame.len() {
            for value in [0u8, 0xff, 0x12, 0x50] {
                let mut mutated = frame.clone();
                mutated[at] = value;
                target.a.receive(&mutated, 1);
                target.b.receive(&mutated, 1);
            }
        }
    }
    target.advance(100_000);
}
