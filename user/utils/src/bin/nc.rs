//! `nc HOST PORT [TEXT...]`: opens a TCP connection, sends TEXT and a line
//! end (then closes its side), and prints everything the peer sends until
//! it closes (needs `use:net`). HOST may be an IPv4 or IPv6 address
//! (`fec0::2` or `[fec0::2]`, ADR-0043) or a name (resolved with DNS; its
//! IPv6 and IPv4 addresses race, the first connection wins).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{Read, ResolveError, connect_host};
use oceans_rt::{Buffer, Out, Start};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:net\n");
oceans_rt::entry!(main);

const CONNECT_MS: u64 = 10_000;
const IDLE_MS: u64 = 10_000;

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let net = match require(&mut out, &directory, "nc", "use", "net", "use:net") {
        Ok(net) => net,
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let (Some(host), Some(port)) = (words.next(), words.next().and_then(|p| p.parse().ok())) else {
        let _ = writeln!(out, "usage: nc HOST PORT [TEXT...]");
        return EXIT_USAGE;
    };
    let fail = |out: &mut Out, problem: &str| {
        let _ = writeln!(out, "nc: {problem}");
        EXIT_FAILED
    };
    let stream = match connect_host(net, host, port, CONNECT_MS) {
        Ok(stream) => stream,
        Err(ResolveError::Net(error)) => return fail(&mut out, error.message()),
        Err(error) => {
            let _ = writeln!(out, "nc: {host}: {}", error.message());
            return EXIT_FAILED;
        }
    };
    let mut text = Buffer::<512>::new();
    for (i, word) in words.enumerate() {
        let _ = write!(text, "{}{word}", if i > 0 { " " } else { "" });
    }
    if !text.as_bytes().is_empty() {
        let _ = text.write_str("\n");
        if let Err(error) = stream.send_all(text.as_bytes(), IDLE_MS) {
            return fail(&mut out, error.message());
        }
        let _ = stream.shutdown();
    }
    let mut buffer = [0u8; 256];
    loop {
        match stream.read_wait(&mut buffer, IDLE_MS) {
            Ok(Read::Data(len)) => print_bytes(&mut out, &buffer[..len]),
            Ok(Read::Eof) => return 0,
            Ok(Read::WouldBlock) => {}
            Err(error) => return fail(&mut out, error.message()),
        }
    }
}

/// Prints received bytes, line ends as CR LF and other control bytes as
/// `.`.
fn print_bytes(out: &mut Out, bytes: &[u8]) {
    for &byte in bytes {
        let _ = match byte {
            b'\n' => out.write_str("\n").map_err(drop),
            b'\r' => Ok(()),
            byte if byte.is_ascii_graphic() || byte == b' ' || byte == b'\t' => {
                out.write_bytes(&[byte]).map_err(drop)
            }
            _ => out.write_bytes(b".").map_err(drop),
        };
    }
}
