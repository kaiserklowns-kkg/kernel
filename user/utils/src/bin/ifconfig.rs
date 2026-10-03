//! `ifconfig`: the network configuration (needs `use:net`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{Dotted, info};
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
    let [a, b, c, d, e, f] = info.mac;
    let _ = writeln!(
        out,
        "      mac {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}"
    );
    0
}
