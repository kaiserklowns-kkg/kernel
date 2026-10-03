//! HTTP/1.1 client protocol (RFC 9110, 9112), ADR-0028: URLs, requests and
//! a streaming response parser. No allocation and no I/O: feed it bytes as
//! they arrive, in pieces of any size, and it reports the head and body.
//!
//! A response is untrusted input:
//! - the head is bounded ([`MAX_HEAD`]);
//! - lengths are checked for overflow, and conflicting `Content-Length`
//!   headers are rejected (a request-smuggling vector);
//! - chunk sizes are bounded hex;
//! - a body cut short is an error, not a short success.

#![no_std]

/// Largest response head (status line and headers) accepted.
pub const MAX_HEAD: usize = 8 * 1024;
/// Longest redirect target kept.
pub const MAX_LOCATION: usize = 512;
const MAX_CHUNK_LINE: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// Not an `http://` or `https://` URL we can use.
    BadUrl,
    Unsupported,
    /// The output buffer is too small.
    TooLarge,
    /// The response is not valid HTTP/1.x.
    Malformed,
    /// The connection ended before the body was complete.
    Truncated,
    /// A redirect from `https://` to `http://` (it would drop the
    /// protection the user asked for).
    Downgrade,
}

/// A parsed `http://` or `https://` URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Url<'a> {
    /// `https://`: the connection must use TLS (ADR-0031).
    pub secure: bool,
    pub host: &'a str,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub target: &'a str,
}

impl<'a> Url<'a> {
    pub fn parse(text: &'a str) -> Result<Self, HttpError> {
        let (secure, rest) = match (text.strip_prefix("https://"), text.strip_prefix("http://")) {
            (Some(rest), _) => (true, rest),
            (None, Some(rest)) => (false, rest),
            (None, None) => return Err(HttpError::BadUrl),
        };
        let (authority, target) = match rest.find(['/', '?']) {
            Some(at) if rest.as_bytes()[at] == b'/' => (&rest[..at], &rest[at..]),
            Some(_) => return Err(HttpError::BadUrl),
            None => (rest, "/"),
        };
        let target = target.split('#').next().unwrap_or("/");
        if authority.contains('@') {
            return Err(HttpError::Unsupported); // credentials in URLs
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().map_err(|_| HttpError::BadUrl)?),
            None => (authority, default_port(secure)),
        };
        let host_ok = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if !host_ok || port == 0 || !printable(target) {
            return Err(HttpError::BadUrl);
        }
        Ok(Self {
            secure,
            host,
            port,
            target,
        })
    }

    fn scheme(&self) -> &'static [u8] {
        if self.secure { b"https://" } else { b"http://" }
    }
}

fn default_port(secure: bool) -> u16 {
    if secure { 443 } else { 80 }
}

/// Writes `:port` unless it is the scheme's default.
fn put_port(writer: &mut Writer<'_>, url: &Url<'_>) -> Result<(), HttpError> {
    if url.port == default_port(url.secure) {
        return Ok(());
    }
    let mut digits = [0u8; 5];
    let mut port = url.port;
    let mut at = digits.len();
    while port > 0 {
        at -= 1;
        digits[at] = b'0' + (port % 10) as u8;
        port /= 10;
    }
    writer.put(b":")?;
    writer.put(&digits[at..])
}

fn printable(text: &str) -> bool {
    text.bytes().all(|b| (0x21..0x7f).contains(&b))
}

/// Writes a `GET` request for `url`; returns its length.
pub fn get_request(url: &Url<'_>, out: &mut [u8]) -> Result<usize, HttpError> {
    let mut writer = Writer { out, len: 0 };
    writer.put(b"GET ")?;
    writer.put(url.target.as_bytes())?;
    writer.put(b" HTTP/1.1\r\nHost: ")?;
    writer.put(url.host.as_bytes())?;
    put_port(&mut writer, url)?;
    writer.put(b"\r\nUser-Agent: Oceans/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n")?;
    Ok(writer.len)
}

struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), HttpError> {
        let end = self.len + bytes.len();
        self.out
            .get_mut(self.len..end)
            .ok_or(HttpError::TooLarge)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

/// The response's status and the headers the client needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    reason: [u8; 64],
    reason_len: usize,
    pub content_length: Option<u64>,
    pub chunked: bool,
    location: [u8; MAX_LOCATION],
    location_len: usize,
}

impl Head {
    pub fn reason(&self) -> &str {
        core::str::from_utf8(&self.reason[..self.reason_len]).unwrap_or("")
    }

    /// The `Location` header, for redirects.
    pub fn location(&self) -> Option<&str> {
        (self.location_len > 0)
            .then(|| core::str::from_utf8(&self.location[..self.location_len]).ok())
            .flatten()
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308)
    }
}

/// What parsing produced.
#[derive(Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "passed by value to a callback once per response head; boxing needs an allocator"
)]
pub enum Event<'a> {
    Head(Head),
    Body(&'a [u8]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Head,
    /// Fixed length: bytes still to come.
    Length(u64),
    /// Until the connection closes.
    UntilClose,
    ChunkSize,
    ChunkData(u64),
    /// The CRLF after a chunk's data.
    ChunkEnd,
    Trailers,
    Done,
}

/// A streaming HTTP/1.x response parser.
pub struct Parser {
    state: State,
    head: [u8; MAX_HEAD],
    head_len: usize,
    line: [u8; MAX_CHUNK_LINE],
    line_len: usize,
    /// The response to a `HEAD` request has no body.
    no_body: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub const fn new() -> Self {
        Self {
            state: State::Head,
            head: [0; MAX_HEAD],
            head_len: 0,
            line: [0; MAX_CHUNK_LINE],
            line_len: 0,
            no_body: false,
        }
    }

    /// Whether the whole response has been received.
    pub fn done(&self) -> bool {
        self.state == State::Done
    }

    /// Feeds received bytes, reporting events to `sink`.
    pub fn push(
        &mut self,
        mut input: &[u8],
        sink: &mut impl FnMut(Event<'_>),
    ) -> Result<(), HttpError> {
        while !input.is_empty() {
            match self.state {
                State::Head => {
                    // Accumulate until the blank line that ends the head.
                    let room = MAX_HEAD - self.head_len;
                    let mut taken = 0;
                    let mut ended = None;
                    for &byte in input.iter().take(room) {
                        self.head[self.head_len] = byte;
                        self.head_len += 1;
                        taken += 1;
                        if self.head[..self.head_len].ends_with(b"\r\n\r\n") {
                            ended = Some(self.head_len);
                            break;
                        }
                    }
                    input = &input[taken..];
                    match ended {
                        Some(len) => {
                            let head = parse_head(&self.head[..len])?;
                            self.state = if self.no_body
                                || head.status == 204
                                || head.status == 304
                                || (100..200).contains(&head.status)
                            {
                                State::Done
                            } else if head.chunked {
                                State::ChunkSize
                            } else {
                                match head.content_length {
                                    Some(0) => State::Done,
                                    Some(len) => State::Length(len),
                                    None => State::UntilClose,
                                }
                            };
                            sink(Event::Head(head));
                        }
                        None if self.head_len == MAX_HEAD => return Err(HttpError::TooLarge),
                        None => {}
                    }
                }
                State::Length(remaining) => {
                    let take = (input.len() as u64).min(remaining) as usize;
                    sink(Event::Body(&input[..take]));
                    input = &input[take..];
                    self.state = match remaining - take as u64 {
                        0 => State::Done,
                        left => State::Length(left),
                    };
                }
                State::UntilClose => {
                    sink(Event::Body(input));
                    input = &[];
                }
                State::ChunkSize | State::ChunkEnd | State::Trailers => {
                    let Some((line, rest)) = self.take_line(input)? else {
                        return Ok(());
                    };
                    input = rest;
                    let mut line_copy = [0u8; MAX_CHUNK_LINE];
                    line_copy[..line].copy_from_slice(&self.line[..line]);
                    self.line_len = 0;
                    let line = &line_copy[..line];
                    self.state = match self.state {
                        State::ChunkSize => match chunk_size(line)? {
                            0 => State::Trailers,
                            size => State::ChunkData(size),
                        },
                        State::ChunkEnd if line.is_empty() => State::ChunkSize,
                        State::ChunkEnd => return Err(HttpError::Malformed),
                        // Trailers are ignored; a blank line ends them.
                        _ if line.is_empty() => State::Done,
                        _ => State::Trailers,
                    };
                }
                State::ChunkData(remaining) => {
                    let take = (input.len() as u64).min(remaining) as usize;
                    sink(Event::Body(&input[..take]));
                    input = &input[take..];
                    self.state = match remaining - take as u64 {
                        0 => State::ChunkEnd,
                        left => State::ChunkData(left),
                    };
                }
                State::Done => return Ok(()), // anything after the response is ignored
            }
        }
        Ok(())
    }

    /// Collects one CRLF-terminated line into `self.line`; returns its
    /// length (without CRLF) and the input after it, or `None` if more
    /// input is needed.
    fn take_line<'a>(&mut self, input: &'a [u8]) -> Result<Option<(usize, &'a [u8])>, HttpError> {
        for (index, &byte) in input.iter().enumerate() {
            if self.line_len == MAX_CHUNK_LINE {
                return Err(HttpError::Malformed);
            }
            self.line[self.line_len] = byte;
            self.line_len += 1;
            if self.line[..self.line_len].ends_with(b"\r\n") {
                return Ok(Some((self.line_len - 2, &input[index + 1..])));
            }
        }
        Ok(None)
    }

    /// The connection closed: fine only where the body ends with it.
    pub fn finish(&mut self) -> Result<(), HttpError> {
        match self.state {
            State::Done => Ok(()),
            State::UntilClose => {
                self.state = State::Done;
                Ok(())
            }
            _ => Err(HttpError::Truncated),
        }
    }
}

fn chunk_size(line: &[u8]) -> Result<u64, HttpError> {
    // Chunk extensions (`;name=value`) are ignored.
    let digits = line.split(|&b| b == b';').next().unwrap_or(&[]);
    let digits = trim(digits);
    if digits.is_empty() || digits.len() > 15 {
        return Err(HttpError::Malformed);
    }
    let mut size = 0u64;
    for &digit in digits {
        let value = (digit as char).to_digit(16).ok_or(HttpError::Malformed)?;
        size = size * 16 + u64::from(value);
    }
    Ok(size)
}

fn trim(mut bytes: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = bytes {
        bytes = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = bytes {
        bytes = rest;
    }
    bytes
}

fn parse_head(bytes: &[u8]) -> Result<Head, HttpError> {
    let text = core::str::from_utf8(bytes).map_err(|_| HttpError::Malformed)?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or(HttpError::Malformed)?;
    let rest = status_line
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| status_line.strip_prefix("HTTP/1.0 "))
        .ok_or(HttpError::Malformed)?;
    let (code, reason) = rest.split_once(' ').unwrap_or((rest, ""));
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(HttpError::Malformed);
    }
    let mut head = Head {
        status: code.parse().map_err(|_| HttpError::Malformed)?,
        reason: [0; 64],
        reason_len: 0,
        content_length: None,
        chunked: false,
        location: [0; MAX_LOCATION],
        location_len: 0,
    };
    let reason = &reason.as_bytes()[..reason.len().min(64)];
    head.reason[..reason.len()].copy_from_slice(reason);
    head.reason_len = reason.len();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(HttpError::Malformed)?;
        if name.is_empty() || name.contains([' ', '\t']) {
            return Err(HttpError::Malformed);
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("content-length") {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(HttpError::Malformed);
            }
            let length: u64 = value.parse().map_err(|_| HttpError::Malformed)?;
            if head.content_length.is_some_and(|l| l != length) {
                return Err(HttpError::Malformed); // conflicting lengths
            }
            head.content_length = Some(length);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            let last = value.rsplit(',').next().unwrap_or("").trim();
            if last.eq_ignore_ascii_case("chunked") {
                head.chunked = true;
            } else {
                return Err(HttpError::Unsupported); // e.g. gzip without chunked
            }
        } else if name.eq_ignore_ascii_case("location") {
            let value = &value.as_bytes()[..value.len().min(MAX_LOCATION)];
            head.location[..value.len()].copy_from_slice(value);
            head.location_len = value.len();
        }
    }
    if head.chunked {
        // Chunked wins; a length beside it is ignored (RFC 9112 §6.3).
        head.content_length = None;
    }
    Ok(head)
}

/// Resolves a redirect target against the URL that produced it: absolute
/// URLs as they are (but never from `https://` down to `http://`),
/// absolute paths on the same origin. Writes the resulting URL into `out`
/// and returns its length.
pub fn redirect(base: &Url<'_>, location: &str, out: &mut [u8]) -> Result<usize, HttpError> {
    let mut writer = Writer { out, len: 0 };
    if location.starts_with("http://") || location.starts_with("https://") {
        if base.secure && location.starts_with("http://") {
            return Err(HttpError::Downgrade);
        }
        writer.put(location.as_bytes())?;
    } else if location.starts_with('/') && !location.starts_with("//") {
        writer.put(base.scheme())?;
        writer.put(base.host.as_bytes())?;
        put_port(&mut writer, base)?;
        writer.put(location.as_bytes())?;
    } else {
        return Err(HttpError::Unsupported);
    }
    Ok(writer.len)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// Parses `response` fed in pieces of `step` bytes.
    fn run(response: &[u8], step: usize) -> Result<(Head, Vec<u8>), HttpError> {
        let mut parser = Parser::new();
        let mut head = None;
        let mut body = Vec::new();
        for piece in response.chunks(step.max(1)) {
            parser.push(piece, &mut |event| match event {
                Event::Head(h) => head = Some(h),
                Event::Body(bytes) => body.extend_from_slice(bytes),
            })?;
        }
        parser.finish()?;
        Ok((head.ok_or(HttpError::Truncated)?, body))
    }

    fn every_split(response: &[u8]) -> (Head, Vec<u8>) {
        let whole = run(response, response.len()).unwrap();
        for step in [1, 2, 3, 7, 64] {
            let split = run(response, step).unwrap();
            assert_eq!(split.0, whole.0);
            assert_eq!(split.1, whole.1, "step {step}");
        }
        whole
    }

    #[test]
    fn parses_urls() {
        assert_eq!(
            Url::parse("http://example.com/a/b?c=d#frag"),
            Ok(Url {
                secure: false,
                host: "example.com",
                port: 80,
                target: "/a/b?c=d"
            })
        );
        assert_eq!(
            Url::parse("http://10.0.2.2:8080"),
            Ok(Url {
                secure: false,
                host: "10.0.2.2",
                port: 8080,
                target: "/"
            })
        );
        assert_eq!(
            Url::parse("https://example.com/x"),
            Ok(Url {
                secure: true,
                host: "example.com",
                port: 443,
                target: "/x"
            })
        );
        assert_eq!(
            Url::parse("http://user:pw@host/"),
            Err(HttpError::Unsupported)
        );
        for bad in [
            "ftp://x/",
            "http://",
            "http://host:0/",
            "http://host:99999/",
            "http://ho st/",
            "http://host?q",
            "http://host/a b",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn builds_requests() {
        let mut out = [0u8; 256];
        let url = Url::parse("http://10.0.2.2:8080/hello.txt").unwrap();
        let len = get_request(&url, &mut out).unwrap();
        assert_eq!(
            &out[..len],
            b"GET /hello.txt HTTP/1.1\r\nHost: 10.0.2.2:8080\r\nUser-Agent: Oceans/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
        let url = Url::parse("http://example.com/").unwrap();
        let len = get_request(&url, &mut out).unwrap();
        assert!(
            std::str::from_utf8(&out[..len])
                .unwrap()
                .contains("Host: example.com\r\n")
        );
        assert_eq!(get_request(&url, &mut [0u8; 10]), Err(HttpError::TooLarge));
        // The port is omitted only when it is the scheme's default.
        for (text, host) in [
            ("https://example.com/", "Host: example.com\r\n"),
            ("https://example.com:80/", "Host: example.com:80\r\n"),
            ("http://example.com:443/", "Host: example.com:443\r\n"),
        ] {
            let len = get_request(&Url::parse(text).unwrap(), &mut out).unwrap();
            let request = std::str::from_utf8(&out[..len]).unwrap();
            assert!(request.contains(host), "{text}: {request}");
        }
    }

    #[test]
    fn content_length_bodies() {
        let (head, body) =
            every_split(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Other: y\r\n\r\nhelloEXTRA");
        assert_eq!((head.status, head.reason()), (200, "OK"));
        assert_eq!(body, b"hello", "bytes after the body are ignored");
        let (head, body) = every_split(b"HTTP/1.0 404 Not Found\r\ncontent-length: 0\r\n\r\n");
        assert_eq!((head.status, body.len()), (404, 0));
        assert_eq!(
            run(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort", 3).err(),
            Some(HttpError::Truncated)
        );
    }

    #[test]
    fn chunked_bodies() {
        let response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 999\r\n\r\n\
            5;ext=1\r\nhello\r\n1\r\n \r\nA\r\n0123456789\r\n0\r\nTrailer: x\r\n\r\n";
        let (head, body) = every_split(response);
        assert!(head.chunked && head.content_length.is_none());
        assert_eq!(body, b"hello 0123456789");
        for bad in [
            &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"[..],
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcX\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nfffffffffffffffff\r\n",
        ] {
            assert_eq!(run(bad, 4).err(), Some(HttpError::Malformed));
        }
        assert_eq!(
            run(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel",
                2
            )
            .err(),
            Some(HttpError::Truncated)
        );
        assert_eq!(
            run(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", 5).err(),
            Some(HttpError::Unsupported)
        );
    }

    #[test]
    fn close_delimited_and_bodyless_responses() {
        let (_, body) = every_split(b"HTTP/1.0 200 OK\r\n\r\nall of it until close");
        assert_eq!(body, b"all of it until close");
        let (head, body) = every_split(b"HTTP/1.1 204 No Content\r\n\r\n");
        assert_eq!((head.status, body.len()), (204, 0));
    }

    #[test]
    fn redirects() {
        let (head, _) =
            every_split(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n");
        assert!(head.is_redirect());
        let base = Url::parse("http://10.0.2.2:8080/start").unwrap();
        let mut out = [0u8; 128];
        let len = redirect(&base, head.location().unwrap(), &mut out).unwrap();
        assert_eq!(&out[..len], b"http://10.0.2.2:8080/next");
        let len = redirect(&base, "http://other/x", &mut out).unwrap();
        assert_eq!(&out[..len], b"http://other/x");
        assert_eq!(
            redirect(&base, "relative", &mut out),
            Err(HttpError::Unsupported)
        );
        assert_eq!(
            redirect(&base, "//evil/x", &mut out),
            Err(HttpError::Unsupported)
        );
        // Secure origins keep their scheme and refuse to downgrade.
        let base = Url::parse("https://example.com/start").unwrap();
        let len = redirect(&base, "/next", &mut out).unwrap();
        assert_eq!(&out[..len], b"https://example.com/next");
        let len = redirect(&base, "https://other:8443/x", &mut out).unwrap();
        assert_eq!(&out[..len], b"https://other:8443/x");
        assert_eq!(
            redirect(&base, "http://example.com/x", &mut out),
            Err(HttpError::Downgrade)
        );
        let base = Url::parse("https://example.com:8443/").unwrap();
        let len = redirect(&base, "/y", &mut out).unwrap();
        assert_eq!(&out[..len], b"https://example.com:8443/y");
    }

    #[test]
    fn hostile_responses_are_refused() {
        for bad in [
            &b"HTTP/2 200 OK\r\n\r\n"[..],
            b"HTTP/1.1 20 OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nBad Name: x\r\n\r\n",
        ] {
            assert!(run(bad, 5).is_err(), "{:?}", std::str::from_utf8(bad));
        }
        let mut huge = Vec::from(&b"HTTP/1.1 200 OK\r\n"[..]);
        while huge.len() < MAX_HEAD + 10 {
            huge.extend_from_slice(b"X-Filler: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        }
        assert_eq!(run(&huge, 100).err(), Some(HttpError::TooLarge));
        // Every single-byte mutation: no panic.
        let good = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        for at in 0..good.len() {
            for value in [0u8, b'\r', b'\n', b'0', b'f', 0xff] {
                let mut mutated = good.to_vec();
                mutated[at] = value;
                let _ = run(&mutated, 3);
            }
        }
    }
}
