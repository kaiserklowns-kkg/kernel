//! clock: sets the wall clock from network time (ADR-0143).
//!
//! The real-time clock is read once at boot (ADR-0031); it drifts, and on
//! a machine that kept local time it is hours off. Once the network is up,
//! this service asks time servers (SNTP) and sets the clock, and the
//! real-time clock with it.
//!
//! - It asks the servers of `pool.ntp.org` (or the servers in its
//!   arguments, `HOST[:PORT] ...`, as the smoke test gives), each request
//!   with a random nonce the answer must carry.
//! - Two answers from different addresses must agree within 2 s: one
//!   server, or someone pretending to be one on the local network, cannot
//!   move the clock alone.
//! - Until it has set the clock it tries again every 30 s; then every
//!   6 hours.
//!
//! Manifest grants: `log`, `clock` (the system information object with
//! `WRITE`), `use = net`.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{IpAddr, ServerTime, lookup, parse_ip, split_host_port, time_via};
use oceans_rt::{Buffer, Directory, Handle, Start};

oceans_rt::entry!(main);

/// Where to ask, unless the arguments say otherwise.
const SERVERS: [&str; 4] = [
    "0.pool.ntp.org",
    "1.pool.ntp.org",
    "2.pool.ntp.org",
    "3.pool.ntp.org",
];
const RETRY_MS: u64 = 30_000;
const RESYNC_MS: u64 = 6 * 3600 * 1000;
/// A change smaller than this is not worth a log line (or a jump).
const QUIET_MS: u64 = 500;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// A server's address and port from `HOST[:PORT]`.
fn address(net: Handle, server: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = split_host_port(server)?;
    let port = port.unwrap_or(oceans_ntp::PORT);
    if let Some(ip) = parse_ip(host) {
        return Some((ip, port));
    }
    let found = lookup(net, host).ok()?;
    found
        .v4
        .map(IpAddr::V4)
        .or(found.v6.map(IpAddr::V6))
        .map(|ip| (ip, port))
}

/// Two agreeing answers from different addresses, the later one; `None`
/// if the servers could not be reached or did not agree.
fn ask(net: Handle, servers: &[&str]) -> Option<ServerTime> {
    let mut first: Option<(IpAddr, u16, ServerTime)> = None;
    for server in servers {
        let Some((ip, port)) = address(net, server) else {
            continue;
        };
        if first.is_some_and(|(seen, seen_port, _)| seen == ip && seen_port == port) {
            continue;
        }
        let Ok(time) = time_via(net, ip, port) else {
            continue;
        };
        match first {
            None => first = Some((ip, port, time)),
            Some((_, _, earlier)) => {
                if oceans_ntp::agree(earlier.unix_ms, earlier.at_ms, time.unix_ms, time.at_ms) {
                    return Some(time);
                }
                // They differ: the later one is kept, and another may
                // side with it.
                first = Some((ip, port, time));
            }
        }
    }
    None
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(clock), Some(net)) = (
        directory.find("log", "log"),
        directory.find("clock", "clock"),
        directory.find("use", "net"),
    ) else {
        return 3;
    };
    let args = directory.args();
    let mut given = [""; 8];
    let mut count = 0;
    for word in args.split_whitespace().take(given.len()) {
        given[count] = word;
        count += 1;
    }
    let servers: &[&str] = if count > 0 { &given[..count] } else { &SERVERS };
    let Ok(timer) = oceans_rt::notification_create() else {
        return 4;
    };
    let mut set_once = false;
    loop {
        match ask(net, servers) {
            Some(time) => {
                // Brought to now: asking took a moment.
                let now = time.unix_ms + (oceans_rt::clock_ms() - time.at_ms);
                let was = oceans_rt::unix_time_ms();
                let off = was.map_or(u64::MAX, |was| was.abs_diff(now));
                if off >= QUIET_MS {
                    match oceans_rt::clock_set(clock, now) {
                        Ok(()) => match was {
                            Some(was) => say(
                                log,
                                format_args!(
                                    "clock: set from network time, {} s {}",
                                    off / 1000,
                                    if now > was { "ahead" } else { "back" }
                                ),
                            ),
                            None => say(log, format_args!("clock: set from network time")),
                        },
                        Err(error) => {
                            say(log, format_args!("clock: cannot set the clock: {error:?}"));
                        }
                    }
                } else if !set_once {
                    say(log, format_args!("clock: already right"));
                }
                set_once = true;
            }
            None if !set_once => {}
            None => say(log, format_args!("clock: no time servers answered alike")),
        }
        let wait = if set_once { RESYNC_MS } else { RETRY_MS };
        let _ = oceans_rt::timer_set(timer, 1, wait);
        let _ = oceans_rt::notification_wait(timer);
    }
}
