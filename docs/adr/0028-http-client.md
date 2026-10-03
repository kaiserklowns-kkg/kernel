# ADR-0028: HTTP client

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0024 (TCP, DNS), ADR-0022 (files)

## Context

Packages, models and updates will be downloaded. With TCP and DNS in
place (ADR-0024), the remaining piece is HTTP. HTTPS needs TLS, which
builds on the entropy of ADR-0026; that is the next step.

## Decision

- **Protocol (`libs/http`, `oceans-http`, host-tested, no allocation):**
  - `http://` URL parsing. URLs with credentials are refused; `https://`
    is refused as not yet supported.
  - `GET` requests with `Host`, `User-Agent: Oceans/0.1` and
    `Connection: close`.
  - **A streaming response parser.** Bytes can be fed in pieces of any
    size; it reports the head once and the body as it arrives, and
    supports Content-Length, chunked (with extensions and trailers) and
    close-delimited bodies.
  - **Redirect resolution:** absolute `http://` URLs, and absolute paths
    on the same host.
- **Hostile servers:**
  - the head is limited to 8 KiB;
  - lengths must be decimal without overflow;
  - **conflicting `Content-Length` headers are rejected** (a smuggling
    vector), and `chunked` overrides a length;
  - chunk sizes are bounded hex;
  - a body that ends early is `Truncated`, not a short success;
  - unknown transfer codings are refused.
- **`fetch URL [PATH]` (`user/utils`):**
  - resolves the host and connects;
  - follows up to 5 redirects;
  - prints the body, or saves it to `PATH` through the filesystem.
    Saving needs `use:fs` in addition to `use:net`, so a program cannot
    write files just because it can download.
  - Non-200 statuses are reported (`fetch: HTTP 404 Not Found`).
- **`Out` is now line-buffered** (`oceans_rt`). A line reaches the
  console in one write, so kernel log lines no longer land inside a
  program's output line. That interleaving made a smoke expectation
  flaky.

## Found on the way

CI's second boot found the downloaded file empty, while local runs
passed. `fetch` reported success and exited; the filesystem commits a
writer's data when it processes the handle's close, which is
asynchronous, and the test machine shut down before that. `fetch` now
calls `sync` before reporting a file saved. The fs protocol
documentation states the rule: a program that must know its data is on
disk calls `sync`.

## Consequences

- Programs can fetch over HTTP. Throughput is bounded by the 248-byte
  inline stream calls (ADR-0024); bulk shared buffers will lift that.
- No HTTPS, keep-alive, compression or caching yet.

## Testing

- **`cargo test -p oceans-http`** (7 tests):
  - URLs, and the exact request bytes;
  - Content-Length, chunked, close-delimited and bodyless responses,
    **each fed whole and in 1, 2, 3, 7 and 64-byte pieces**, all giving
    identical results;
  - truncation, bad chunk framing and oversized chunk sizes;
  - redirect resolution, refusing `//host` and relative targets;
  - hostile heads (conflicting or negative lengths, oversized heads, bad
    status lines), plus every single-byte mutation of a chunked response
    without panic.
- **Smoke test** against an HTTP server xtask runs on the host:
  - a plain file;
  - a 302 redirect;
  - a chunked response;
  - a 404;
  - a 20000-byte download saved to `/keep/big.bin`. **After the boots,
    xtask mounts the disk image on the host and compares that file byte
    for byte with what it served.**
  - Passes in debug, release and with `-cpu max`.
