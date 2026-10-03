//! `fetch [--ca PATH] URL [PATH]`: downloads an `http://` or `https://` URL
//! (needs `use:net`). Without PATH the body is printed; with PATH it is
//! saved as that file (needs `use:fs` too). Redirects are followed, at most
//! 5, never from `https` down to `http`. `https` certificates are checked
//! against Mozilla's roots, plus the certificates in `--ca PATH` (PEM or
//! DER; ADR-0031).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::time::Duration;

use oceans_fs_proto::{FsError, Kind, Node, Shared, flags};
use oceans_http::{Event, Head, HttpError, Parser, Url, get_request, redirect};
use oceans_net_proto::{Read, TcpStream, resolve};
use oceans_rt::{Buffer, Out, Start};
use oceans_tls::rustls::crypto::{GetRandomFailed, SecureRandom};
use oceans_tls::rustls::pki_types::{ServerName, UnixTime};
use oceans_tls::rustls::time_provider::TimeProvider;
use oceans_tls::rustls::{ClientConfig, RootCertStore};
use oceans_tls::{Client, ClientError, Transport};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:net\n");
oceans_rt::entry!(main);

const MAX_REDIRECTS: usize = 5;
const CONNECT_MS: u64 = 10_000;
const IDLE_MS: u64 = 15_000;
const MAX_URL: usize = 600;
/// Largest `--ca` file.
const MAX_CA_FILE: u64 = 1024 * 1024;

/// Where the body goes.
enum Sink {
    Console,
    File {
        node: Node,
        /// Bulk writes (ADR-0030), when the buffer could be set up.
        shared: Option<Shared>,
        written: u64,
    },
}

/// Why a request failed.
enum Problem {
    Text(&'static str),
    Tls(ClientError<&'static str>),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Tls(error) => error.fmt(f),
        }
    }
}

impl From<&'static str> for Problem {
    fn from(text: &'static str) -> Self {
        Self::Text(text)
    }
}

impl From<ClientError<&'static str>> for Problem {
    fn from(error: ClientError<&'static str>) -> Self {
        Self::Tls(error)
    }
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
    let mut words = args.split_whitespace().peekable();
    let mut fs = None;
    let mut fs_root = |out: &mut Out| -> Result<Node, i64> {
        if fs.is_none() {
            let root = require(out, &directory, "fetch", "use", "fs", "use:fs")
                .map_err(|_| EXIT_FAILED)?;
            fs = Some(root);
        }
        Ok(Node(fs.expect("set above")))
    };
    let mut ca = None;
    if words.peek() == Some(&"--ca") {
        words.next();
        let Some(path) = words.next() else {
            let _ = writeln!(out, "usage: fetch [--ca PATH] URL [PATH]");
            return EXIT_USAGE;
        };
        let root = match fs_root(&mut out) {
            Ok(root) => root,
            Err(code) => return code,
        };
        match read_file(&root, path) {
            Ok(bytes) => ca = Some(bytes),
            Err(error) => {
                let _ = writeln!(out, "fetch: {path}: {}", error.message());
                return EXIT_FAILED;
            }
        }
    }
    let Some(first) = words.next() else {
        let _ = writeln!(out, "usage: fetch [--ca PATH] URL [PATH]");
        return EXIT_USAGE;
    };
    let mut sink = match words.next() {
        None => Sink::Console,
        Some(path) => {
            let root = match fs_root(&mut out) {
                Ok(root) => root,
                Err(code) => return code,
            };
            match create(root, path) {
                Ok(node) => match node.truncate(0) {
                    Ok(()) => {
                        let shared = node.attach(16 * 1024).ok();
                        Sink::File {
                            node,
                            shared,
                            written: 0,
                        }
                    }
                    Err(error) => return fail(&mut out, error.message()),
                },
                Err(error) => {
                    let _ = writeln!(out, "fetch: {path}: {}", error.message());
                    return EXIT_FAILED;
                }
            }
        }
    };

    let started = oceans_rt::clock_ms();
    let mut tls: Option<Arc<ClientConfig>> = None;
    let mut url_text = Buffer::<MAX_URL>::new();
    let _ = url_text.write_str(first);
    for _ in 0..=MAX_REDIRECTS {
        let url = match Url::parse(url_text.as_str()) {
            Ok(url) => url,
            Err(_) => return fail(&mut out, "only http:// and https:// URLs are supported"),
        };
        let config = if url.secure {
            if tls.is_none() {
                match tls_config(ca.as_deref()) {
                    Ok(config) => tls = Some(config),
                    Err(problem) => return fail(&mut out, problem),
                }
            }
            tls.clone()
        } else {
            None
        };
        let (head, security) = match get(net, &url, config, &mut sink, &mut out) {
            Ok(result) => result,
            Err(problem) => return fail(&mut out, problem),
        };
        if head.is_redirect() {
            let Some(location) = head.location() else {
                return fail(&mut out, "redirect without a location");
            };
            let mut next = [0u8; MAX_URL];
            let len = match redirect(&url, location, &mut next) {
                Ok(len) => len,
                Err(HttpError::Downgrade) => {
                    return fail(&mut out, "refusing a redirect from https to http");
                }
                Err(_) => return fail(&mut out, "unsupported redirect target"),
            };
            url_text = Buffer::new();
            let _ = url_text.write_str(core::str::from_utf8(&next[..len]).unwrap_or(""));
            continue;
        }
        if head.status != 200 {
            let _ = writeln!(out, "fetch: HTTP {} {}", head.status, head.reason());
            return EXIT_FAILED;
        }
        if let Sink::File { node, written, .. } = &sink {
            // Durable before we say so: a close is committed only when the
            // filesystem gets to it, which may be after we have exited.
            if let Err(error) = node.sync() {
                return fail(&mut out, error.message());
            }
            if let Some((version, suite)) = security {
                let _ = writeln!(out, "fetch: {version:?} {suite:?}");
            }
            let ms = oceans_rt::clock_ms() - started;
            let _ = writeln!(out, "fetch: saved {written} bytes ({ms} ms)");
        }
        return 0;
    }
    fail(&mut out, "too many redirects")
}

/// Randomness for TLS: the kernel's generator (ADR-0026).
#[derive(Debug)]
struct KernelRandom;

impl SecureRandom for KernelRandom {
    fn fill(&self, out: &mut [u8]) -> Result<(), GetRandomFailed> {
        oceans_rt::random(out).map_err(|_| GetRandomFailed)
    }
}

static RANDOM: KernelRandom = KernelRandom;

/// Wall time for certificate validity (ADR-0031).
#[derive(Debug)]
struct KernelTime;

impl TimeProvider for KernelTime {
    fn current_time(&self) -> Option<UnixTime> {
        oceans_rt::unix_time_ms().map(|ms| UnixTime::since_unix_epoch(Duration::from_millis(ms)))
    }
}

fn tls_config(ca: Option<&[u8]>) -> Result<Arc<ClientConfig>, &'static str> {
    let mut roots: RootCertStore = oceans_tls::web_roots();
    if let Some(bytes) = ca {
        let certificates = oceans_tls::parse_certificates(bytes);
        if certificates.is_empty() {
            return Err("--ca: no certificates in that file");
        }
        for certificate in certificates {
            roots
                .add(certificate)
                .map_err(|_| "--ca: not a usable CA certificate")?;
        }
    }
    let provider = Arc::new(oceans_tls::provider(&RANDOM));
    oceans_tls::client_config(provider, Arc::new(KernelTime), roots)
        .map(Arc::new)
        .map_err(|_| "TLS setup failed")
}

/// Reads a whole (small) file.
fn read_file(root: &Node, path: &str) -> Result<Vec<u8>, FsError> {
    let (file, kind) = root.walk(path, 0)?;
    let result = (|| {
        if kind != Kind::File {
            return Err(FsError::Status(oceans_fs_proto::Status::IsADirectory));
        }
        let size = file.stat()?.size;
        if size > MAX_CA_FILE {
            return Err(FsError::Status(oceans_fs_proto::Status::NoSpace));
        }
        let mut bytes = alloc::vec![0u8; size as usize];
        let mut done = 0;
        while done < bytes.len() {
            match file.read(done as u64, &mut bytes[done..])? {
                0 => break,
                n => done += n,
            }
        }
        bytes.truncate(done);
        Ok(bytes)
    })();
    file.close();
    result
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

fn fail(out: &mut Out, problem: impl fmt::Display) -> i64 {
    let _ = writeln!(out, "fetch: {problem}");
    EXIT_FAILED
}

/// A TCP connection as a TLS transport.
struct Tcp(TcpStream);

impl Transport for Tcp {
    type Error = &'static str;

    fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.0.send_all(data, IDLE_MS).map_err(|e| e.message())
    }

    fn recv(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            match self.0.read_wait(buffer, IDLE_MS).map_err(|e| e.message())? {
                Read::Data(len) => return Ok(len),
                Read::Eof => return Ok(0),
                Read::WouldBlock => {}
            }
        }
    }
}

/// The connection a request runs over.
enum Connection {
    Plain(Tcp),
    Tls(Box<Client<Tcp>>),
}

impl Connection {
    fn send_all(&mut self, data: &[u8]) -> Result<(), Problem> {
        match self {
            Self::Plain(tcp) => tcp.send(data).map_err(Problem::Text),
            Self::Tls(client) => client.write_all(data).map_err(Problem::Tls),
        }
    }

    /// Received bytes; 0 at the end of the stream.
    fn recv(&mut self, buffer: &mut [u8]) -> Result<usize, Problem> {
        match self {
            Self::Plain(tcp) => tcp.recv(buffer).map_err(Problem::Text),
            Self::Tls(client) => client.read(buffer).map_err(Problem::Tls),
        }
    }
}

type Security = Option<(
    oceans_tls::rustls::ProtocolVersion,
    oceans_tls::rustls::CipherSuite,
)>;

/// One request: the response head (and, for `https`, the protocol and
/// suite), with the body (of a 200) delivered to `sink` as it arrives.
fn get(
    net: oceans_rt::Handle,
    url: &Url<'_>,
    tls: Option<Arc<ClientConfig>>,
    sink: &mut Sink,
    out: &mut Out,
) -> Result<(Head, Security), Problem> {
    let address = resolve(net, url.host).map_err(|e| e.message())?;
    let stream = TcpStream::connect(net, address, url.port).map_err(|e| e.message())?;
    stream.wait_connected(CONNECT_MS).map_err(|e| e.message())?;
    let mut connection = match tls {
        None => Connection::Plain(Tcp(stream)),
        Some(config) => {
            let name = ServerName::try_from(url.host)
                .map_err(|_| "not a valid server name")?
                .to_owned();
            Connection::Tls(Box::new(Client::connect(config, name, Tcp(stream))?))
        }
    };
    let security = match &connection {
        Connection::Tls(client) => client.describe(),
        Connection::Plain(_) => None,
    };
    let mut request = [0u8; 1024];
    let len = get_request(url, &mut request).map_err(|_| "URL too long")?;
    connection.send_all(&request[..len])?;

    let mut parser = Parser::new();
    let mut head: Option<Head> = None;
    let mut problem = None;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let len = match connection.recv(&mut buffer) {
            Ok(0) => break,
            Ok(len) => len,
            Err(problem) => return Err(problem),
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
            return Err(problem.into());
        }
        if parser.done() {
            break;
        }
    }
    parser
        .finish()
        .map_err(|_| "connection closed before the end of the response")?;
    Ok((head.ok_or("no HTTP response")?, security))
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
        Sink::File {
            node,
            shared,
            written,
        } => {
            match shared {
                Some(shared) => node.write_shared(shared, *written, bytes)?,
                None => node.write_all(*written, bytes)?,
            }
            *written += bytes.len() as u64;
            Ok(())
        }
    }
}
