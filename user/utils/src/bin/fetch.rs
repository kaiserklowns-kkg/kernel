//! `fetch URL [PATH]`: downloads an `http://` URL (needs `use:net`).
//! Without PATH the body is printed; with PATH it is saved as that file
//! (needs `use:fs` too). Redirects are followed, at most 5.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_fs_proto::{FsError, Node, flags};
use oceans_http::{Event, Head, Parser, Url, get_request, redirect};
use oceans_net_proto::{Read, TcpStream, resolve};
use oceans_rt::{Buffer, Out, Start};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:net\n");
oceans_rt::entry!(main);

const MAX_REDIRECTS: usize = 5;
const CONNECT_MS: u64 = 10_000;
const IDLE_MS: u64 = 15_000;
const MAX_URL: usize = 600;

/// Where the body goes.
enum Sink {
    Console,
    File { node: Node, written: u64 },
}

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let net = match require(&mut out, &directory, "fetch", "use", "net", "use:net") {
        Ok(net) => net,
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let Some(first) = words.next() else {
        let _ = writeln!(out, "usage: fetch URL [PATH]");
        return EXIT_USAGE;
    };
    let mut sink = match words.next() {
        None => Sink::Console,
        Some(path) => {
            let Ok(fs) = require(&mut out, &directory, "fetch", "use", "fs", "use:fs") else {
                return EXIT_FAILED;
            };
            match create(Node(fs), path) {
                Ok(node) => match node.truncate(0) {
                    Ok(()) => Sink::File { node, written: 0 },
                    Err(error) => return fail(&mut out, error.message()),
                },
                Err(error) => {
                    let _ = writeln!(out, "fetch: {path}: {}", error.message());
                    return EXIT_FAILED;
                }
            }
        }
    };

    let mut url_text = Buffer::<MAX_URL>::new();
    let _ = url_text.write_str(first);
    for _ in 0..=MAX_REDIRECTS {
        let url = match Url::parse(url_text.as_str()) {
            Ok(url) => url,
            Err(_) => return fail(&mut out, "only http:// URLs are supported"),
        };
        let head = match get(net, &url, &mut sink, &mut out) {
            Ok(head) => head,
            Err(problem) => return fail(&mut out, problem),
        };
        if head.is_redirect() {
            let Some(location) = head.location() else {
                return fail(&mut out, "redirect without a location");
            };
            let mut next = [0u8; MAX_URL];
            let Ok(len) = redirect(&url, location, &mut next) else {
                return fail(&mut out, "unsupported redirect target");
            };
            url_text = Buffer::new();
            let _ = url_text.write_str(core::str::from_utf8(&next[..len]).unwrap_or(""));
            continue;
        }
        if head.status != 200 {
            let _ = writeln!(out, "fetch: HTTP {} {}", head.status, head.reason());
            return EXIT_FAILED;
        }
        if let Sink::File { node, written } = &sink {
            // Durable before we say so: a close is committed only when the
            // filesystem gets to it, which may be after we have exited.
            if let Err(error) = node.sync() {
                return fail(&mut out, error.message());
            }
            let _ = writeln!(out, "fetch: saved {written} bytes");
        }
        return 0;
    }
    fail(&mut out, "too many redirects")
}

/// Opens (creating) `path` for writing: its directory is opened writable,
/// since only a writable directory handle may create in it.
fn create(root: Node, path: &str) -> Result<Node, FsError> {
    let path = path.trim_start_matches('/');
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    if parent.is_empty() {
        return root
            .open(name, flags::CREATE_FILE | flags::WRITE)
            .map(|(node, _)| node);
    }
    let (directory, _) = root.walk(parent, flags::WRITE)?;
    let file = directory.open(name, flags::CREATE_FILE | flags::WRITE);
    directory.close();
    file.map(|(node, _)| node)
}

fn fail(out: &mut Out, problem: &str) -> i64 {
    let _ = writeln!(out, "fetch: {problem}");
    EXIT_FAILED
}

/// One request: the response head, with the body (of a 200) delivered to
/// `sink` as it arrives.
fn get(
    net: oceans_rt::Handle,
    url: &Url<'_>,
    sink: &mut Sink,
    out: &mut Out,
) -> Result<Head, &'static str> {
    let address = resolve(net, url.host).map_err(|e| e.message())?;
    let stream = TcpStream::connect(net, address, url.port).map_err(|e| e.message())?;
    stream.wait_connected(CONNECT_MS).map_err(|e| e.message())?;
    let mut request = [0u8; 1024];
    let len = get_request(url, &mut request).map_err(|_| "URL too long")?;
    stream
        .send_all(&request[..len], IDLE_MS)
        .map_err(|e| e.message())?;

    let mut parser = Parser::new();
    let mut head: Option<Head> = None;
    let mut problem = None;
    let mut buffer = [0u8; 256];
    loop {
        let len = match stream.read_wait(&mut buffer, IDLE_MS) {
            Ok(Read::Data(len)) => len,
            Ok(Read::Eof) => break,
            Ok(Read::WouldBlock) => continue,
            Err(error) => return Err(error.message()),
        };
        parser
            .push(&buffer[..len], &mut |event| match event {
                Event::Head(h) => head = Some(h),
                // Only a successful response's body is the download.
                Event::Body(bytes) if head.is_some_and(|h| h.status == 200) => {
                    if let Err(error) = deliver(sink, &mut *out, bytes) {
                        problem = Some(error.message());
                    }
                }
                Event::Body(_) => {}
            })
            .map_err(|_| "invalid HTTP response")?;
        if let Some(problem) = problem {
            return Err(problem);
        }
        if parser.done() {
            break;
        }
    }
    parser
        .finish()
        .map_err(|_| "connection closed before the end of the response")?;
    head.ok_or("no HTTP response")
}

fn deliver(sink: &mut Sink, out: &mut Out, bytes: &[u8]) -> Result<(), FsError> {
    match sink {
        Sink::Console => {
            for (i, line) in bytes.split(|&b| b == b'\n').enumerate() {
                if i > 0 {
                    let _ = out.write_str("\n");
                }
                let _ = out.write_bytes(line);
            }
            Ok(())
        }
        Sink::File { node, written } => {
            node.write_all(*written, bytes)?;
            *written += bytes.len() as u64;
            Ok(())
        }
    }
}
