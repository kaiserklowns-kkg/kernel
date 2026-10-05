# ADR-0058: The Oceans web experience: SvelteKit apps and the Go service bridge

- Status: Accepted (amended by ADR-0061: pairing may also propose installs, which the user confirms on the device)
- Date: 2026-10-04
- Depends on: ADR-0001 (apps reach the system through a local service
  bridge), ADR-0045 to ADR-0049 (Oceans Core, packages, permissions,
  delegation), ADR-0050 (Go on Oceans), ADR-0051 (the AI runtime),
  ADR-0056 (Phase 7 UI architecture)
- Phase 7: the system experiences (Control Center, Apps, AI Center,
  Settings)

## Context

The master spec builds the user-facing system apps (Settings, Store, AI
Center, Control Center) with SvelteKit, and Bun for their tooling, and
never makes the UI a dependency of the kernel or of the system's working
(§6, §29–32). ADR-0001 says such apps reach the system through a local
service bridge, never directly.

ADR-0056 splits Phase 7:
- on-device graphics, the desktop shell and permission dialogs are
  native Rust;
- the SvelteKit experience is a static bundle served by a Go **bridge**
  on Oceans, which exposes the System API as JSON.

Oceans has no HTML engine yet, so the browser is on another device for
now (on-device later). Everything the web pages do is also a shell
command: the UI is a convenience, never a requirement.

The hard part is authority. A web server reachable from the network is
the easiest thing on the system to attack. It must not become a standing
holder of power over apps, and a request must not count as the user just
because it arrived.

## Decision

### The bridge (`go/cmd/bridge`)

A Go service run by the Go host (ADR-0050), started by init.

**Its own authority is small:**

| Grant | For |
|---|---|
| `log` | its log lines |
| `provide = bridge` | the pairing endpoint (below) |
| `use = net` | listening on TCP port 8080 |
| `use = ai` | the AI runtime (ADR-0051) |
| `sysinfo` | memory, uptime, processes (read-only) |

**It holds no capability over apps of its own**: no Core capability, no
filesystem, no console. It stores nothing.

**What it serves:**
- the web app, embedded in the module with Go's `embed`;
- the System API as JSON.

**How it serves (HTTP/1.1, strict and bounded):**
- **One request per connection** (`Connection: close`), one connection
  at a time. The service is single-threaded (wasip1): IPC calls and
  connections are handled in turn.
  - Connections are signalled on a notification bound to its endpoint,
    so pairing calls and connections share one receive loop.
  - A request has 10 seconds in all (not per read): a client sending a
    byte at a time cannot hold it.
- **Bounds:** header 8 KiB and 64 fields, body 16 KiB, one Content-Length
  only.
- **Refused rather than guessed:** chunked request bodies, pipelined
  data, repeated security-relevant fields, obsolete line folding, escapes
  (`%`), dot segments and backslashes in paths, control characters.
- **Strict JSON:** a single object, no unknown fields, nothing after it.
- **Every answer** carries:
  - `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
    `Referrer-Policy: no-referrer`;
  - `Cross-Origin-Opener-Policy` and `Cross-Origin-Resource-Policy:
    same-origin`;
  - a Content-Security-Policy:
    - API answers: `default-src 'none'`;
    - the page: the policy SvelteKit writes into it (`default-src
      'self'`, plus the SHA-256 of its one inline bootstrap script), with
      `frame-ancestors 'none'`. A page without such a policy gets a
      strict one, under which its inline script does not run.
- **No CORS headers at all**, so no other origin can read an answer.

**The new binding:** `go/oceans/tcp/listen.go` adds `Listen` and
`Accept`, mirroring net-proto's `TCP_LISTEN` and `TCP_ACCEPT`:
- the listener signals the caller's notification;
- each accepted connection gets its own notification, like `Dial`.

### Pairing: the user's agent lends authority

The shell is the user's agent (ADR-0018, ADR-0047). `ui pair`:

1. **Mints** from its own Core capability a narrower one (ADR-0048):
   **query + run + audit**.
   - Never **decide**: consent and permission changes stay with the
     console (and the native desktop agent later).
   - Never **manage**: no installing or removing.
2. **Draws a code** of 128 bits from the kernel's random generator (32
   hex digits).
3. **Sends both** to the bridge (`PAIR`, the capability moving with the
   call).
4. **Prints** the address to open and the code.

`ui unpair`:
- the bridge **closes the capability** and **forgets the code**;
- every browser session ends at once.

A new `ui pair` replaces the old pairing the same way.

**The `bridge` protocol (`go/cmd/bridge/protocol.go`):**

| Request | Data | Reply |
|---|---|---|
| `PAIR` | the code, handles = [core] | `[port u16]` |
| `UNPAIR` | — | `NotPaired` if there was none |
| `STATUS` | — | `[paired u8][port u16]` |

Only the shell has `use = bridge`. A program the shell runs gets it only
if the user types it.

### Authenticating the browser

Every API but `/api/session` answers **401** without the current code.
The code is presented in either of two ways:
- **`Authorization: Bearer CODE`**, for scripts and tests;
- **the session cookie**:
  - the login page posts the code to `/api/session`;
  - the answer sets `oceans_session`, `HttpOnly` (page scripts cannot
    read it) and `SameSite=Strict` (other sites' requests do not carry
    it).

Codes are compared in constant time. Each refused login is logged.

**Requests that change something** are also refused when:
- they come from another site (`Origin` other than the bridge's own, or
  `Sec-Fetch-Site` other than `same-origin`);
- they are not `application/json` (which a cross-site form cannot send
  without a preflight the bridge never grants).

### The System API

| Route | Does | Through |
|---|---|---|
| `GET /api/session` | paired? signed in? | — |
| `POST`, `DELETE /api/session` | sign in with the code; sign out | — |
| `GET /api/system` | kernel, memory, uptime, processes | `sysinfo` |
| `GET /api/apps` | installed apps: version, kind, runtime, publisher, running | Core `query` |
| `POST /api/apps/{id}/start`, `/stop` | runs (detached) or stops an installed app | Core `run` |
| `GET /api/apps/{id}/permissions` | each permission, the system's words for it, the decision, the app's reason | Core `query` |
| `GET /api/audit` | the audit log | Core `audit` |
| `POST /api/ai/ask` | asks Oceans AI | `ai` |
| `POST /api/ai/continue` | the user's answer to an approval | `ai` |
| `GET /api/ai/activity` | the AI's activity log | `ai` |
| `POST /api/ai/model` | the model server (as `ai model`) | `ai` |

Core's statuses become HTTP statuses with the system's words, e.g.:
- 404: not installed;
- 409: already running, or **needs a permission decision** ("run it once
  on the Oceans console to answer");
- 403: pairing does not allow this.

An app needing an undecided permission therefore cannot be started from
the browser: the decision is the console's.

### AI approvals from the browser

- **`/api/ai/ask` delegates** what the browser's user may: a Core
  capability minted from the paired one, **query + run** only, as the
  shell does (ADR-0051). No folder is delegated: file tools stay with the
  console.
- **The approval question is the tool's wording**, shown as such: "Oceans
  AI wants to: start the app Hello (app.oceans.hello)".
- **Only the explicit Allow click** sends `approve: true`. Deny has the
  focus, so Enter never approves by accident.
- **The bridge accepts answers only for sessions it started**, each
  answered once. The AI service's session numbers are shared with the
  shell, and a browser must not answer the console's questions.
- **Each answer is logged as the user's**: "AI action approved by the
  user in the paired browser: start the app Hello". The AI service's
  activity log records "approved by the user" as for the console.

### The web app (`ui/`)

- **Stack:** SvelteKit 2 with Svelte 5 (runes), TypeScript strict
  (`noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`), built by
  Vite with `adapter-static` as a single-page app.
  - One script and one stylesheet (`bundleStrategy: 'single'`), because
    the bridge serves one request at a time.
  - Gzip copies, which the bridge serves to browsers that accept them.
  - The bundle: 120 KiB of script (44 KiB gzipped) and 17 KiB of styles
    (4 KiB).
- **Pages:**
  - **Control Center:** memory (with a meter), uptime, the processes;
    refreshed every 5 s while visible.
  - **Apps:** installed apps, their state, kind, runtime and publisher;
    Start and Stop; each app's permissions with their decisions; the
    audit log.
  - **AI Center:** questions, approval requests worded by the system with
    Allow and Deny, answers, the activity log.
  - **Settings:** the AI model server; About Oceans; sign out.
  - **Pairing screen:** shown until the browser is signed in, and again
    on any 401 (e.g. after `ui unpair`).
- **The design system** (master spec §29–32):
  - tokens as CSS custom properties: a deep dark background, neutral
    surfaces in small steps, one accent, semantic success, warning, error
    and info colours, radii, spacing, type;
  - components with their states: `Button` (default, hover, focus,
    active, disabled, loading), `TextField` (with hint and error), `Card`,
    `Badge`, `Meter`, `Notice`, `Spinner`, `PageHeader`;
  - keyboard and assistive technology: visible focus rings, a skip link,
    ARIA states (`aria-busy`, `aria-invalid`, `aria-current`,
    `aria-expanded`), meters with values, errors as alerts and the rest
    as polite status, reduced motion respected.
  - No inline styles of ours: dynamic widths go through the CSSOM, so
    the page needs no `'unsafe-inline'`. The one inline style is
    SvelteKit's route announcer for screen readers, allowed by its hash
    alone (`'unsafe-hashes'`), which `svelte.config.js` reads from the
    installed SvelteKit.
  - Checked in a browser against Oceans in QEMU (`cargo xtask run`): the
    pairing screen, every page, and no policy violations.
- **The API client** (`ui/src/lib/api.ts`):
  - typed;
  - checks the shape of every answer before the app sees it;
  - refuses non-app ids and over-long questions before sending.

### Build and tests

- **Bun is a build requirement**, like Go. xtask runs Bun (`OCEANS_BUN`
  overrides):
  - `bun install --frozen-lockfile` (versions pinned in `bun.lock`);
  - `bun run build`;
  - then copies the build (without Brotli copies) to
    `go/cmd/bridge/web`, **before** building the Go programs, so
    `bridge.wasm` embeds it.

  A committed `.gitkeep` keeps the embed valid without a build; the
  bridge then answers 503 and says the app was not built.
- **`cargo xtask check`** adds:
  - the license check;
  - `bun test` (the API client, presentation helpers, polling);
  - `svelte-check` with warnings as errors;
  - `go test` of the bridge (the HTTP parser, the routes and their
    refusals, the static files and policy, the System API parsing).
- **CI** installs Bun with `oven-sh/setup-bun`.
- **`cargo xtask run`** forwards host port 8080 (`OCEANS_BRIDGE_PORT`) to
  the bridge: type `ui pair`, then open `http://127.0.0.1:8080/`.

**The smoke test** forwards a host port to the bridge and, from the host:
1. `ui status`: not paired, listening.
2. `ui pair`; the harness reads the code from the console. Then:
   - **The page:** `GET /` has the title and its policy; an app route
     gets the page; the script comes gzipped.
   - **Refusals:** the API without the code, with a wrong one and a wrong
     login: 401.
   - **The System API:** `/api/system` with memory figures and processes;
     `/api/apps` lists Hello; its permissions.
   - **Apps:** start Hello (it runs), stop it, stop again (409); a
     cross-origin start (403).
   - **The login cookie:** `HttpOnly`, `SameSite=Strict`, and it works.
   - **Oceans AI:** a read-only answer; "start the hello app" asks for
     approval in the system's words; the browser's Allow starts it; the
     activity says "approved by the user".
3. `ui unpair`: the code is refused, `/api/session` says not paired, and
   a second `ui unpair` says no browser is paired.

### Dependencies and licenses

Pinned in `ui/bun.lock` and checked by `bun run licenses` in
`cargo xtask check` (MIT, Apache-2.0 and BSD only):

| Package | Version | License |
|---|---|---|
| svelte | 5.57.1 | MIT |
| @sveltejs/kit | 2.70.3 | MIT |
| @sveltejs/adapter-static | 3.0.10 | MIT |
| @sveltejs/vite-plugin-svelte | 6.2.4 | MIT |
| vite | 7.3.6 | MIT |
| typescript | 6.0.3 | Apache-2.0 |
| svelte-check | 4.7.6 | MIT |
| @types/bun | 1.3.14 | MIT |

59 packages in all: MIT 54, Apache-2.0 3, BSD-3-Clause 1, and ISC 1
(`picocolors`, through PostCSS; ISC is the BSD family's simplest form and
is accepted as BSD).

- **Vite 7, not 8:** Vite 8 depends on `lightningcss` (MPL-2.0).
- **SvelteKit 2.70, not 3.0:** 3.0 is days old.

All of these are build tools. What ships is the bundle built from
Svelte's and SvelteKit's runtime (MIT) and our code.

### Measurements

In QEMU (TCG, `-m 256M`, release kernel), from the smoke test's host,
round trip including QEMU's port forwarding:

| What | Size | Time |
|---|---|---|
| `GET /` | 1 KiB | 6–44 ms |
| `GET` the script, gzipped | 44 KiB | 64 ms |
| `GET /api/system` | 1.2 KiB | 53 ms |
| `GET /api/apps` (4 Core calls per app) | 0.6 KiB | 16–22 ms |
| `POST /api/apps/{id}/start` | — | 20 ms |
| a refused request (401, 403, 409) | — | 6 ms |
| `POST /api/ai/ask` (read-only, the host's scripted model) | — | 53 ms |

With the debug kernel, the same requests take 2–4 times as long (the
gzipped script 248 ms, an AI question about 150–200 ms).

- `bridge.wasm` is 4.2 MB (the Go runtime 2.7 MB, `encoding/json`, and
  the app's 183 KiB with its gzip copies); `ai.wasm` is 3.6 MB.
- The bridge process holds about 30 MiB (the Go host, the interpreted
  module and its heap), the largest process after boot; the AI service
  23 MiB.
- It is listening before the shell's first prompt.

The bridge is interpreted Go (wasmi), and TCP moves 248 bytes per IPC
call: both are why the bundle is one gzipped script.

## Consequences

- **The SvelteKit experience exists without on-device graphics:** any
  browser on the network uses it.
- **The bridge adds no authority to the system.**
  - Unpaired, it can read memory and processes, which any program with
    `sysinfo` can, and reach the AI runtime, which delegates nothing
    without a paired capability.
  - Paired, it can do what the user lent it, and only until `ui unpair`.
- **Permission decisions remain the console's.** A web Store that
  installs and decides needs the native permission dialogs of ADR-0056
  (a trusted path the page cannot fake), not more authority for the
  bridge.
- **Not yet:**
  - **TLS:** the code and the cookie cross the network in clear.
    Pairing is for a trusted network until the bridge serves HTTPS (the
    TLS library exists for clients, ADR-0029/0031; a server certificate
    and its trust are still to decide).
  - **Concurrency:** one request at a time. A long AI question (up to the
    gateway's 120 s) makes other requests wait; requests are not
    pipelined and connections are not kept alive.
  - **Live updates** (server-sent events) for activity and approvals.
  - **Reading the model setting back:** the AI service has no request for
    it, so Settings sets it but does not show the current one.
  - **The on-device HTML engine**, and a light theme.

## Alternatives considered

- **The bridge holding its own Core capability:** any compromise of the
  network-facing service would be standing control of apps.
  Pairing-time delegation ties its authority to a user action, revocable
  by another.
- **Letting the browser decide permissions:** the browser's page is
  whatever the network delivered. A permission prompt must be a trusted
  path (ADR-0047), which the native desktop will provide.
- **A login password stored on the system:** a long-lived secret to
  manage and leak. A one-time random code, shown only on the console and
  revoked with the pairing, needs no storage.
- **The code in the URL:** URLs are kept in history and logs. The code
  is typed once and becomes an HttpOnly cookie.
- **Go's `net/http`:** larger, and it assumes a socket API wasip1 does
  not have. A small, strict parser serves exactly what the bridge needs
  and refuses the rest.
- **Server-side rendering on Oceans:** it needs a JavaScript runtime on
  the device. Static files keep Oceans' side to Go and the System API.

## Checklist (master spec §48)

- **Purpose:** the Phase 7 system experiences (Control Center, Apps, AI
  Center, Settings) in SvelteKit, reaching the system only through a
  bridge.
- **Architecture:**
  - `ui/`, a static SvelteKit app built by Bun;
  - `go/cmd/bridge`, a Go service on Oceans that serves it and the
    System API as JSON;
  - authority lent by the shell at pairing.
- **API:**
  - the HTTP API above;
  - the `bridge` IPC protocol;
  - `tcp.Listen` and `Accept` in the Go binding;
  - the shell's `ui pair`, `ui unpair` and `ui status`;
  - `ui/src/lib/api.ts`.
- **Dependencies:**
  - Bun 1.3 (build);
  - the pinned, license-checked npm packages above (build);
  - Go's standard library (`encoding/json`, `embed`).
- **Security:**
  - no standing authority over apps;
  - pairing lends query + run + audit, never decide or manage, revoked by
    `ui unpair`;
  - a 128-bit random code, compared in constant time; an HttpOnly,
    SameSite=Strict cookie;
  - same-origin JSON only for changes, no CORS;
  - bounded, strict parsing;
  - CSP, nosniff and frame denial;
  - AI approvals only by the browser user's click, for the browser's own
    sessions, logged as the user's;
  - clear HTTP for now: a trusted network only (see Consequences).
- **Testing:**
  - **Go unit tests:** parser, routes, authentication, refusals, static
    files and policy, System API parsing.
  - **`bun test`** and **`svelte-check`**.
  - **The smoke test:** pairing, the page, the API, apps, AI approval,
    unpairing, from the host.
- **Failure behaviour:**
  - no network: the bridge logs it and serves nothing;
  - the network device not ready: it retries every second for a minute;
  - malformed or slow requests are answered with an error or dropped;
  - Core or AI errors become HTTP errors with the system's words;
  - unpaired or revoked: 401, and the app returns to its pairing screen;
  - the bundle not built: 503 with the reason.
