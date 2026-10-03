//! `host NAME [SERVER[:PORT]]`: looks up a host name's IPv4 address with
//! DNS (needs `use:net`). Without SERVER, asks the configured DNS server.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{Dotted, parse_ipv4, resolve, resolve_via};
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
        None => resolve(net, name),
        Some(server) => {
            let (address, port) = server.split_once(':').unwrap_or((server, "53"));
            match (parse_ipv4(address), port.parse()) {
                (Some(address), Ok(port)) => resolve_via(net, name, address, port),
                _ => {
                    let _ = writeln!(out, "host: bad server {server}");
                    return EXIT_USAGE;
                }
            }
        }
    };
    match result {
        Ok(address) => {
            let _ = writeln!(out, "{name} has address {}", Dotted(address));
            0
        }
        Err(error) => {
            let _ = writeln!(out, "host: {name}: {}", error.message());
            EXIT_FAILED
        }
    }
}
