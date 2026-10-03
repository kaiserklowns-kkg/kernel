//! net-echo: the echo service (RFC 862) on UDP and TCP port 7, for network
//! diagnostics (ADR-0023, ADR-0024), over IPv4 and IPv6 (ADR-0043).
//! Datagrams go back to their sender; bytes on a connection go back on it
//! until the peer closes.
//!
//! One thread serves everything: every socket signals the same
//! notification, and each wake-up services them all.
//!
//! Manifest grants: `log`, `use = net`.

#![no_std]
#![no_main]

use oceans_net_proto::{MAX_DATA6, MAX_STREAM, Read, Socket, TcpListener, TcpStream};
use oceans_rt::{Directory, Handle, Start};

oceans_rt::entry!(main);

const PORT: u16 = 7;
const EVENT: u64 = 1;
const MAX_CONNECTIONS: usize = 16;

/// A connection and the bytes it still has to send back.
struct Connection {
    stream: TcpStream,
    pending: [u8; MAX_STREAM],
    start: usize,
    end: usize,
}

impl Connection {
    /// Echoes what it can; `false` once the connection is finished.
    fn service(&mut self) -> bool {
        loop {
            if self.start < self.end {
                match self.stream.send(&self.pending[self.start..self.end]) {
                    Ok(0) => return true, // full: wait for space
                    Ok(sent) => self.start += sent,
                    Err(_) => return false,
                }
                continue;
            }
            match self.stream.read(&mut self.pending) {
                Ok(Read::Data(len)) => {
                    self.start = 0;
                    self.end = len;
                }
                Ok(Read::WouldBlock) => return true,
                Ok(Read::Eof) => {
                    let _ = self.stream.shutdown();
                    return false; // dropping it finishes the close
                }
                Err(_) => return false,
            }
        }
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(net)) = (directory.find("log", "log"), directory.find("use", "net"))
    else {
        return 3;
    };
    let Ok(events) = oceans_rt::notification_create() else {
        return 4;
    };
    let (udp, listener) = match (
        Socket::udp_on(net, PORT, events, EVENT),
        TcpListener::listen_on(net, PORT, events, EVENT),
    ) {
        (Ok(udp), Ok(listener)) => (udp, listener),
        (Err(error), _) | (_, Err(error)) => {
            let _ = oceans_rt::debug_write(log, "net-echo: cannot open port 7:");
            let _ = oceans_rt::debug_write(log, error.message());
            return 5;
        }
    };
    let _ = oceans_rt::debug_write(log, "net-echo: listening on UDP and TCP port 7");
    let mut connections: [Option<Connection>; MAX_CONNECTIONS] = [const { None }; MAX_CONNECTIONS];
    loop {
        serve_udp(&udp);
        accept(&listener, events, &mut connections);
        for slot in &mut connections {
            if let Some(connection) = slot
                && !connection.service()
            {
                *slot = None;
            }
        }
        if oceans_rt::notification_wait(events).is_err() {
            return 6;
        }
    }
}

fn serve_udp(udp: &Socket) {
    let mut buffer = [0u8; MAX_DATA6];
    while let Ok(Some(received)) = udp.recv_ip(&mut buffer) {
        // Cut datagrams are echoed as received: best effort.
        let _ = udp.send_to_ip(received.from, received.port, &buffer[..received.len]);
    }
}

fn accept(listener: &TcpListener, events: Handle, connections: &mut [Option<Connection>]) {
    while let Ok(Some(stream)) = listener.accept_on(events, EVENT) {
        match connections.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => {
                *slot = Some(Connection {
                    stream,
                    pending: [0; MAX_STREAM],
                    start: 0,
                    end: 0,
                });
            }
            // Full: dropping the stream closes it.
            None => drop(stream),
        }
    }
}
