//! Hello: the example Oceans app (ADR-0045).
//!
//! It uses only what Oceans Core grants, each found by name in its handle
//! directory, and works with whatever it was not given:
//! - `console out`: where it writes (started without one, e.g. by a
//!   program holding only `core:run`, it works silently);
//! - `app info`: its id and version;
//! - `use storage`: its private data directory, where it counts its runs;
//! - `use net`: with `HOST PORT` arguments it says hello to that TCP
//!   server and prints the answer;
//! - argument `wait`: it then waits until stopped (for `app stop`).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_fs_proto::{Kind, Node, flags};
use oceans_net_proto::{Read, connect_host};
use oceans_rt::{Directory, Out, Start};

oceans_rt::entry!(main);

const CONNECT_MS: u64 = 10_000;

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let mut out = Talk(directory.find("console", "out").map(Out::new));
    let info = directory
        .find("app", "info")
        .and_then(oceans_rt::map_text)
        .unwrap_or("? ?");
    let mut words = info.split_whitespace();
    let (id, version) = (words.next().unwrap_or("?"), words.next().unwrap_or("?"));
    let _ = writeln!(out, "Hello from {id} {version}");

    match directory.find("use", "storage") {
        Some(storage) => match count_run(&Node(storage)) {
            Ok(runs) => {
                let _ = writeln!(out, "hello: run {runs} (counted in my storage)");
            }
            Err(problem) => {
                let _ = writeln!(out, "hello: my storage failed: {problem}");
            }
        },
        None => {
            let _ = writeln!(out, "hello: no storage permission; not counting runs");
        }
    }

    let args = directory.args();
    let mut words = args.split_whitespace();
    match words.next() {
        Some("wait") => {
            let _ = writeln!(out, "hello: waiting until stopped");
            drop(out);
            loop {
                oceans_rt::sleep_ms(60_000);
            }
        }
        Some(host) => {
            let port = words.next().and_then(|p| p.parse().ok()).unwrap_or(7);
            match directory.find("use", "net") {
                Some(net) => greet(&mut out, net, host, port),
                None => {
                    let _ = writeln!(out, "hello: no network permission; not connecting");
                }
            }
        }
        None => {}
    }
    0
}

/// Reads, increments and writes back the `runs` file.
fn count_run(storage: &Node) -> Result<u64, &'static str> {
    let (file, kind) = storage
        .open("runs", flags::CREATE_FILE | flags::WRITE)
        .map_err(|e| e.message())?;
    let result = (|| {
        if kind != Kind::File {
            return Err("runs is not a file");
        }
        let mut text = [0u8; 20];
        let len = file.read(0, &mut text).map_err(|e| e.message())?;
        let runs = core::str::from_utf8(&text[..len])
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        let mut line = oceans_rt::Buffer::<24>::new();
        let _ = writeln!(line, "{runs}");
        // Written over the old count, then cut to length: stopped midway
        // (an app can be stopped at any moment), the file still holds a
        // count, never nothing. The new count is never shorter.
        file.write_all(0, line.as_bytes())
            .and_then(|()| file.truncate(line.as_bytes().len() as u64))
            .and_then(|()| file.sync())
            .map_err(|e| e.message())?;
        Ok(runs)
    })();
    file.close();
    result
}

/// Says hello to `host:port` and prints the answer.
/// The console, if the app was given one; otherwise output goes nowhere.
struct Talk(Option<Out>);

impl Write for Talk {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        match &mut self.0 {
            Some(out) => out.write_str(s),
            None => Ok(()),
        }
    }
}

fn greet(out: &mut Talk, net: oceans_rt::Handle, host: &str, port: u16) {
    let stream = match connect_host(net, host, port, CONNECT_MS) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = writeln!(out, "hello: {host}: {}", error.message());
            return;
        }
    };
    if stream.send_all(b"hello from an app\n", CONNECT_MS).is_err() {
        let _ = writeln!(out, "hello: the connection failed");
        return;
    }
    let _ = stream.shutdown();
    let mut buffer = [0u8; 128];
    loop {
        match stream.read_wait(&mut buffer, CONNECT_MS) {
            Ok(Read::Data(len)) => {
                let text = core::str::from_utf8(&buffer[..len]).unwrap_or("?");
                let _ = out.write_str(text);
            }
            Ok(Read::WouldBlock) => {}
            Ok(Read::Eof) | Err(_) => return,
        }
    }
}
