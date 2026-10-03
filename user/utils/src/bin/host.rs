//! `host NAME [SERVER[:PORT]]`: looks up a host name's IPv4 (A) and IPv6
//! (AAAA, ADR-0043) addresses with DNS (needs `use:net`). Without SERVER,
//! asks the configured DNS server. SERVER is an IPv4 address or an IPv6
//! one (in brackets to give a port: `[fec0::3]:53`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{lookup, lookup_via, parse_ip, split_host_port};
use oceans_rt::Start;
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:net\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let net = match require(&mut out, &directory, "host", "use", "net", "use:net") {
        Ok(net) => net,
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let Some(name) = words.next() else {
        let _ = writeln!(out, "usage: host NAME [SERVER[:PORT]]");
        return EXIT_USAGE;
    };
    let result = match words.next() {
        None => lookup(net, name),
        Some(server) => {
            let parsed = split_host_port(server)
                .and_then(|(address, port)| Some((parse_ip(address)?, port.unwrap_or(53))));
            match parsed {
                Some((address, port)) => lookup_via(net, name, address, port, (true, true)),
                None => {
                    let _ = writeln!(out, "host: bad server {server}");
                    return EXIT_USAGE;
                }
            }
        }
    };
    match result {
        Ok(addresses) => {
            if let Some(v4) = addresses.v4 {
                let _ = writeln!(out, "{name} has address {}", oceans_net_proto::Dotted(v4));
            }
            if let Some(v6) = addresses.v6 {
                let _ = writeln!(
                    out,
                    "{name} has IPv6 address {}",
                    oceans_net_proto::Colons(v6)
                );
            }
            0
        }
        Err(error) => {
            let _ = writeln!(out, "host: {name}: {}", error.message());
            EXIT_FAILED
        }
    }
}
