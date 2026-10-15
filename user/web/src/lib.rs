//! `GET` over `http://` and `https://` for apps (ADR-0135): the
//! connection (a name's addresses raced, ADR-0043), the request, redirects
//! (at most 5, never from `https` down to `http`), then the body as it
//! arrives: waiting for it, or, for an app's ticks, taking what is there.
//!
//! `https` certificates are checked against Mozilla's roots (ADR-0031).
//! Everything received is untrusted: `oceans-http` bounds the head and
//! checks the framing.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::time::Duration;

pub use oceans_http::Head;
use oceans_http::{Event, HttpError, Parser, Url, get_request, redirect};
use oceans_net_proto::{Read, TcpStream, connect_host};
use oceans_rt::Handle;
use oceans_tls::rustls::ClientConfig;
use oceans_tls::rustls::crypto::{GetRandomFailed, SecureRandom};
use oceans_tls::rustls::pki_types::{ServerName, UnixTime};
use oceans_tls::rustls::time_provider::TimeProvider;
use oceans_tls::{Client, ClientError, Transport};

const MAX_REDIRECTS: usize = 5;
/// Longest URL followed.
pub const MAX_URL: usize = 600;
const CONNECT_MS: u64 = 10_000;
/// How long the head, or a body read that waits, may take.
pub const IDLE_MS: u64 = 15_000;
/// What a transport read that must not wait reports.
const NOTHING_YET: &str = "nothing yet";

/// Why a request failed, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem(pub String);

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Problem {
    fn from(text: &str) -> Self {
        Self(String::from(text))
    }
}

impl From<ClientError<&'static str>> for Problem {
    fn from(error: ClientError<&'static str>) -> Self {
        Self(alloc::format!("{error}"))
    }
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

fn tls_config() -> Result<Arc<ClientConfig>, Problem> {
    let provider = Arc::new(oceans_tls::provider(&RANDOM));
    oceans_tls::client_config(provider, Arc::new(KernelTime), oceans_tls::web_roots())
        .map(Arc::new)
        .map_err(|_| Problem::from("TLS setup failed"))
}

/// A TCP connection whose reads wait up to `wait_ms` (0: not at all).
struct Tcp {
    stream: TcpStream,
    wait_ms: u64,
}

impl Transport for Tcp {
    type Error = &'static str;

    fn send(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        self.stream.send_all(data, IDLE_MS).map_err(|e| e.message())
    }

    fn recv(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let read = if self.wait_ms == 0 {
            self.stream.read(buffer)
        } else {
            self.stream.read_wait(buffer, self.wait_ms)
        };
        match read.map_err(|e| e.message())? {
            Read::Data(len) => Ok(len),
            Read::Eof => Ok(0),
            Read::WouldBlock if self.wait_ms == 0 => Err(NOTHING_YET),
            Read::WouldBlock => Err("timed out"),
        }
    }
}

enum Connection {
    Plain(Tcp),
    Tls(Box<Client<Tcp>>),
}

impl Connection {
    fn tcp(&mut self) -> &mut Tcp {
        match self {
            Self::Plain(tcp) => tcp,
            Self::Tls(client) => client.transport_mut(),
        }
    }

    /// Received bytes: `Some(0)` at the end, `None` when nothing came
    /// within `wait_ms`.
    fn recv(&mut self, buffer: &mut [u8], wait_ms: u64) -> Result<Option<usize>, Problem> {
        self.tcp().wait_ms = wait_ms;
        let result = match self {
            Self::Plain(tcp) => tcp.recv(buffer).map_err(ClientError::Transport),
            Self::Tls(client) => client.read(buffer),
        };
        match result {
            Ok(len) => Ok(Some(len)),
            Err(ClientError::Transport(NOTHING_YET)) => Ok(None),
            // A stream that just stops (many do) has ended.
            Err(ClientError::Truncated) => Ok(Some(0)),
            Err(error) => Err(error.into()),
        }
    }
}

/// A response: its head, then its body as it arrives.
pub struct Response {
    /// The address the response came from, after the redirects (what a
    /// page's relative links are relative to; ADR-0157).
    pub url: String,
    pub head: Head,
    connection: Connection,
    /// On the heap, like `raw`: a program's stack is 64 KiB.
    parser: Box<Parser>,
    /// Body bytes that came with the head, not taken yet.
    early: Vec<u8>,
    /// What a read receives, before the framing is taken off.
    raw: Vec<u8>,
    ended: bool,
}

/// Requests `url`, following redirects; the final response's head has
/// come (whatever its status), its body not yet.
pub fn get(net: Handle, url: &str) -> Result<Response, Problem> {
    let mut tls = None;
    let mut current = String::from(url);
    for _ in 0..=MAX_REDIRECTS {
        let parsed = Url::parse(&current)
            .map_err(|_| Problem::from("only http:// and https:// addresses work"))?;
        let config = match (parsed.secure, &tls) {
            (false, _) => None,
            (true, Some(config)) => Some(Arc::clone(config)),
            (true, None) => {
                let config = tls_config()?;
                tls = Some(Arc::clone(&config));
                Some(config)
            }
        };
        let response = request(net, &parsed, config)?;
        if !response.head.is_redirect() {
            let mut response = response;
            response.url = current;
            return Ok(response);
        }
        let location = response
            .head
            .location()
            .ok_or(Problem::from("a redirect without a location"))?;
        let mut next = [0u8; MAX_URL];
        let len = match redirect(&parsed, location, &mut next) {
            Ok(len) => len,
            Err(HttpError::Downgrade) => {
                return Err("refusing a redirect from https to http".into());
            }
            Err(_) => return Err("an unsupported redirect".into()),
        };
        current = String::from(core::str::from_utf8(&next[..len]).unwrap_or(""));
    }
    Err("too many redirects".into())
}

fn request(
    net: Handle,
    url: &Url<'_>,
    tls: Option<Arc<ClientConfig>>,
) -> Result<Response, Problem> {
    let stream = connect_host(net, url.host, url.port, CONNECT_MS).map_err(|e| e.message())?;
    let tcp = Tcp {
        stream,
        wait_ms: IDLE_MS,
    };
    let mut connection = match tls {
        None => Connection::Plain(tcp),
        Some(config) => {
            // Certificates name IP addresses without brackets.
            let name = ServerName::try_from(url.hostname())
                .map_err(|_| Problem::from("not a valid server name"))?
                .to_owned();
            Connection::Tls(Box::new(Client::connect(config, name, tcp)?))
        }
    };
    let mut request = [0u8; 1024];
    let len =
        get_request(url, &mut request).map_err(|_| Problem::from("the address is too long"))?;
    match &mut connection {
        Connection::Plain(tcp) => tcp.send(&request[..len]).map_err(Problem::from)?,
        Connection::Tls(client) => client.write_all(&request[..len])?,
    }

    let mut parser = Box::new(Parser::new());
    let mut head = None;
    let mut early = Vec::new();
    let mut buffer = [0u8; 4096];
    while head.is_none() {
        let len = match connection.recv(&mut buffer, IDLE_MS)? {
            Some(0) => return Err("the connection closed before a response".into()),
            Some(len) => len,
            None => return Err("timed out".into()),
        };
        parser
            .push(&buffer[..len], &mut |event| match event {
                Event::Head(h) => head = Some(h),
                Event::Body(bytes) => early.extend_from_slice(bytes),
            })
            .map_err(|_| Problem::from("not a valid HTTP response"))?;
    }
    let ended = parser.done();
    Ok(Response {
        url: String::new(),
        head: head.expect("the loop ends with a head"),
        connection,
        parser,
        early,
        raw: alloc::vec![0; 16 * 1024],
        ended,
    })
}

impl Response {
    /// Whether the whole body has been taken.
    pub fn ended(&self) -> bool {
        self.ended && self.early.is_empty()
    }

    /// Body bytes into `out`, waiting up to `wait_ms` for some (0: not at
    /// all). Returns how many; 0 when none came yet, or at the end
    /// ([`Response::ended`]).
    pub fn read(&mut self, out: &mut [u8], wait_ms: u64) -> Result<usize, Problem> {
        if !self.early.is_empty() {
            let len = out.len().min(self.early.len());
            out[..len].copy_from_slice(&self.early[..len]);
            self.early.drain(..len);
            return Ok(len);
        }
        if self.ended || out.is_empty() {
            return Ok(0);
        }
        // The body is never longer than what carries it, so what is read
        // fits `out`.
        let take = out.len().min(self.raw.len());
        let len = match self.connection.recv(&mut self.raw[..take], wait_ms)? {
            None => return Ok(0),
            Some(0) => {
                self.ended = true;
                self.parser
                    .finish()
                    .map_err(|_| Problem::from("the connection closed early"))?;
                return Ok(0);
            }
            Some(len) => len,
        };
        let mut filled = 0;
        self.parser
            .push(&self.raw[..len], &mut |event| {
                if let Event::Body(bytes) = event {
                    out[filled..filled + bytes.len()].copy_from_slice(bytes);
                    filled += bytes.len();
                }
            })
            .map_err(|_| Problem::from("not a valid HTTP response"))?;
        self.ended = self.parser.done();
        Ok(filled)
    }
}
