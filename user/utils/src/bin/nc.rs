//! `nc HOST PORT [TEXT...]`: opens a TCP connection, sends TEXT and a line
//! end (then closes its side), and prints everything the peer sends until
//! it closes (needs `use:net`). HOST may be a name (resolved with DNS).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::{Read, TcpStream, resolve};
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
    let address = match resolve(net, host) {
        Ok(address) => address,
        Err(error) => {
            let _ = writeln!(out, "nc: {host}: {}", error.message());
            return EXIT_FAILED;
        }
    };
    let fail = |out: &mut Out, problem: &str| {
        let _ = writeln!(out, "nc: {problem}");
        EXIT_FAILED
    };
    let stream = match TcpStream::connect(net, address, port) {
        Ok(stream) => stream,
        Err(error) => return fail(&mut out, error.message()),
    };
    if let Err(error) = stream.wait_connected(CONNECT_MS) {
        return fail(&mut out, error.message());
    }
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
            Ok(Read::Data(len)) => print_bytes(&out, &buffer[..len]),
            Ok(Read::Eof) => return 0,
            Ok(Read::WouldBlock) => {}
            Err(error) => return fail(&mut out, error.message()),
        }
    }
}

/// Prints received bytes, line ends as CR LF and other control bytes as
/// `.`.
fn print_bytes(out: &Out, bytes: &[u8]) {
    let mut line = [0u8; 256];
    let mut len = 0;
    for &byte in bytes {
        match byte {
            b'\n' => {
                let _ = oceans_rt::console_write(out.0, &line[..len]);
                let _ = oceans_rt::console_write(out.0, b"\r\n");
                len = 0;
            }
            b'\r' => {}
            byte => {
                line[len] = if byte.is_ascii_graphic() || byte == b' ' || byte == b'\t' {
                    byte
                } else {
                    b'.'
                };
                len += 1;
            }
        }
    }
    let _ = oceans_rt::console_write(out.0, &line[..len]);
}
