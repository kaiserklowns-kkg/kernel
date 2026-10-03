//! `ping HOST [COUNT]`: ICMP echo (or ICMPv6 echo, ADR-0043) round trips
//! (needs `use:net`). HOST is an IPv4 or IPv6 address (`fec0::2`,
//! `[fec0::2]`) or a name.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{READABLE, Socket, resolve_ip};
use oceans_rt::Start;
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:net\n");
oceans_rt::entry!(main);

const TIMEOUT: u64 = 1 << 1;
const TIMEOUT_MS: u64 = 1000;
const INTERVAL_MS: u64 = 1000;
const MAX_COUNT: u32 = 100;
const PAYLOAD: &[u8] = b"Oceans ping payload: 0123456789abcdefghijklmnopqrstuvwxyz";

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let net = match require(&mut out, &directory, "ping", "use", "net", "use:net") {
        Ok(net) => net,
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let host = words.next();
    let count = match words.next() {
        None => Some(3),
        Some(word) => word.parse().ok().filter(|&n| (1..=MAX_COUNT).contains(&n)),
    };
    let (Some(host), Some(count)) = (host, count) else {
        let _ = writeln!(out, "usage: ping HOST [COUNT]   (COUNT 1-{MAX_COUNT})");
        return EXIT_USAGE;
    };
    let address = match resolve_ip(net, host) {
        Ok(address) => address,
        Err(error) => {
            let _ = writeln!(out, "ping: {host}: {}", error.message());
            return EXIT_FAILED;
        }
    };
    let socket = match Socket::ping(net) {
        Ok(socket) => socket,
        Err(error) => {
            let _ = writeln!(out, "ping: {}", error.message());
            return EXIT_FAILED;
        }
    };
    let _ = writeln!(out, "PING {address} with {} bytes", PAYLOAD.len());
    let mut received = 0;
    for sequence in 1..=count {
        let sent_at = oceans_rt::clock_ms();
        if let Err(error) = socket.send_to_ip(address, 0, PAYLOAD) {
            let _ = writeln!(out, "ping: {}", error.message());
            return EXIT_FAILED;
        }
        let _ = oceans_rt::timer_set(socket.notification(), TIMEOUT, TIMEOUT_MS);
        let mut answered = false;
        'wait: while let Ok(bits) = socket.wait() {
            if bits & READABLE != 0 {
                let mut buffer = [0u8; 64];
                while let Ok(Some(reply)) = socket.recv_ip(&mut buffer) {
                    // Late replies to earlier requests are skipped.
                    if reply.from == address && u32::from(reply.port) == sequence {
                        let _ = writeln!(
                            out,
                            "reply from {}: seq={sequence} time={} ms",
                            reply.from,
                            oceans_rt::clock_ms() - sent_at
                        );
                        answered = true;
                        break 'wait;
                    }
                }
            }
            if bits & TIMEOUT != 0 {
                break;
            }
        }
        let _ = oceans_rt::timer_set(socket.notification(), TIMEOUT, 0);
        if answered {
            received += 1;
        } else {
            let _ = writeln!(out, "timeout: seq={sequence}");
        }
        if sequence < count {
            let elapsed = oceans_rt::clock_ms() - sent_at;
            oceans_rt::sleep_ms(INTERVAL_MS.saturating_sub(elapsed));
        }
    }
    let _ = writeln!(out, "{count} sent, {received} received");
    if received > 0 { 0 } else { EXIT_FAILED }
}
