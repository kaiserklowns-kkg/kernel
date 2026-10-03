//! `ifconfig`: the network configuration, IPv4 and IPv6 (ADR-0043)
//! (needs `use:net`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{Colons, Dotted, NetError, NetInfo6, Status, address_state, info, info6};
use oceans_rt::Start;
use utils::{EXIT_FAILED, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:net\n");
oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let net = match require(&mut out, &directory, "ifconfig", "use", "net", "use:net") {
        Ok(net) => net,
        Err(code) => return code,
    };
    let info = match info(net) {
        Ok(info) => info,
        Err(error) => {
            let _ = writeln!(out, "ifconfig: {}", error.message());
            return EXIT_FAILED;
        }
    };
    // A stack without IPv6 does not know the request.
    let info6 = match info6(net) {
        Ok(info6) => info6,
        Err(NetError::Status(Status::BadRequest)) => NetInfo6::disabled(),
        Err(error) => {
            let _ = writeln!(out, "ifconfig: {}", error.message());
            return EXIT_FAILED;
        }
    };
    if info.configured {
        let _ = write!(out, "net0: {}/{}", Dotted(info.address), info.prefix);
        if info.gateway != [0; 4] {
            let _ = write!(out, " gateway {}", Dotted(info.gateway));
        }
        if info.dns != [0; 4] {
            let _ = write!(out, " dns {}", Dotted(info.dns));
        }
        let _ = writeln!(out);
    } else {
        let _ = writeln!(out, "net0: no address yet (DHCP in progress)");
    }
    for address in info6.addresses() {
        let scope = if address.link_local {
            "link-local"
        } else {
            "autoconf"
        };
        let state = match address.state {
            address_state::TENTATIVE => " tentative",
            address_state::DEPRECATED => " deprecated",
            address_state::DUPLICATE => " duplicate",
            _ => "",
        };
        let _ = writeln!(
            out,
            "      inet6 {}/{} {scope}{state}",
            Colons(address.address),
            address.prefix
        );
    }
    if info6.enabled && (info6.router.is_some() || info6.dns.is_some()) {
        let _ = write!(out, "      inet6");
        if let Some(router) = info6.router {
            let _ = write!(out, " router {}", Colons(router));
        }
        if let Some(dns) = info6.dns {
            let _ = write!(out, " dns {}", Colons(dns));
        }
        let _ = writeln!(out);
    }
    let [a, b, c, d, e, f] = info.mac;
    let _ = writeln!(
        out,
        "      mac {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}"
    );
    0
}
