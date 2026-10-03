//! udp-echo: the UDP echo service (RFC 862) on port 7, for network
//! diagnostics (ADR-0023). Every datagram goes back to its sender.
//!
//! Manifest grants: `log`, `use = net`.

#![no_std]
#![no_main]

use oceans_net_proto::{MAX_DATA, READABLE, Socket};
use oceans_rt::{Directory, Start};

oceans_rt::entry!(main);

const PORT: u16 = 7;

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(net)) = (directory.find("log", "log"), directory.find("use", "net"))
    else {
        return 3;
    };
    let socket = match Socket::udp(net, PORT) {
        Ok((socket, _)) => socket,
        Err(error) => {
            let _ = oceans_rt::debug_write(log, "udp-echo: cannot open port 7");
            let _ = oceans_rt::debug_write(log, error.message());
            return 4;
        }
    };
    let _ = oceans_rt::debug_write(log, "udp-echo: listening on port 7");
    let mut buffer = [0u8; MAX_DATA];
    loop {
        match socket.wait() {
            Ok(bits) if bits & READABLE != 0 => {}
            Ok(_) => continue,
            Err(_) => return 5,
        }
        while let Ok(Some(received)) = socket.recv(&mut buffer) {
            // Cut datagrams are echoed as received: best effort.
            let _ = socket.send_to(received.from, received.port, &buffer[..received.len]);
        }
    }
}
