# ADR-0064: Web apps (SvelteKit and Bun), and the end of Phase 8

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0046 (packages), ADR-0056 (UI architecture: no HTML
  engine on the device yet), ADR-0058 (the bridge), ADR-0062 (the SDK),
  ADR-0063 (developer keys)

## Context

Phase 8 asks for SvelteKit and Bun support for apps (master spec §6–7,
§47). Oceans has no HTML engine (ADR-0056), so a web app cannot run on the
device yet. The system's own web experience (ADR-0058) runs in the paired
browser on another device, through the bridge.

Developers' web apps can take the same path now, and the same apps can run
in an on-device web view later. Three things must hold:
- they must not reach the system's web experience or its API;
- they must not reach each other;
- they must reach only what their manifest allows.

## Decision

### Packaging

- **A new runtime: `runtime = web`.** The app's package carries one file,
  `web.bundle`: the files of a static SvelteKit build.
  - **Why a bundle:** package file names are flat and at most 40 bytes;
    a build has paths.
  - **Format:** `OCEANSWB`, version, count, then each file's path and
    data.
  - **Paths:** only safe segments (no `..`, no dot-files, no empty
    segments).
  - **Checked by:** `libs/package` (`web::read`/`write`) and the bridge,
    each in full.
- **Manifest rules:**
  - a web app cannot be a service;
  - it may ask only for `storage` (its own data). It has nothing else to
    use.
- **Core:**
  - `RUN` refuses a web app ("open it from Apps in the Oceans web
    experience");
  - **`WEB_BUNDLE`** (needs `query`) hands the bridge the bundle, verified
    exactly as a start verifies a program: signature, trusted key,
    installed version.

### Serving (the bridge)

- **A port of its own, 8081:** web apps are served at `/ID/`, another
  origin than the system's web experience (8080).
- **The page:**
  - served only to the paired browser (its session cookie, sent with the
    navigation);
  - with the page's own hash-based policy plus **`sandbox allow-scripts
    allow-forms`**: an opaque origin, with no cookies, no storage, no
    `allow-same-origin`;
  - so an app page cannot read another app's page or token, the system's
    pages, or call the system's API with the user's session;
  - each page carries a random **app token**, per app and per pairing,
    forgotten on `ui unpair`.
- **The files** (scripts, styles) are as public as the package. They are
  served with `Access-Control-Allow-Origin: *` and
  `Cross-Origin-Resource-Policy: cross-origin`, because module scripts of
  an opaque-origin page need CORS.
- **The app API:**
  - `GET/PUT /ID/api/data/NAME`, with CORS for the sandboxed page;
  - only with that app's token;
  - only if the app asked for `storage`;
  - names are `a-z 0-9 . _ -`; values are up to 16 KiB;
  - kept under the bridge's storage, `apps/ID/`.
- **Opening:** the Apps page shows **Open** for web apps (a new tab on the
  app port) instead of Start/Stop.

### Developing (the SDK)

- **`oceans new sveltekit ID`:**
  - a SvelteKit app with its base path, a single bundle, and the same
    hash-based policy as the system's;
  - `src/lib/oceans.js`, the app API client (`load`, `save`);
  - **a pinned `bun.lock`**, the same versions as the system's UI, so
    builds never resolve dependencies anew.
- **`oceans build`:** `bun install --frozen-lockfile`, `bun run build`,
  then the bundle (Brotli copies dropped; the bridge serves gzip), signed
  like any package.

### Phase 8 is complete

**Its parts:**
- SDK (ADR-0062: `oceans-sdk`, `go/oceans`);
- templates: `rust`, `go`, `sveltekit`;
- developer tools: `oceans new | keygen | build | trust | serve`;
- developer keys (ADR-0063);
- support for Rust (native), Go (`wasm`), SvelteKit with Bun (`web`).

**Its exit criterion** (a third-party app built with the SDK) passes in
`cargo xtask smoke`. Three apps (Rust, Go, SvelteKit) are made with the
tool outside the repository, signed with a new developer key, refused
until the key is trusted, then installed. The Rust and Go apps run. The
web app:
- is refused as a program;
- is listed as `web`;
- has its page refused to an unpaired request, then served sandboxed
  with its token;
- serves its script with CORS;
- stores and reads a note with its token;
- refuses the system's code as a token, and another app's data path.

## Consequences

- **Web apps install, update and are trusted like any app**, and run in
  the paired browser. When Oceans has a web view, it can load the same
  bundles from the same bridge.
- **What web apps cannot do yet:**
  - use the network beyond the bridge (their policy is `connect-src
    'self'`);
  - read the user's files;
  - show windows on the device;
  - receive notifications.

  Each would need a permission designed for web apps.
- **Browsers and embedded views:** a sandboxed page works in standard
  browsers (checked in Chromium: its module scripts load, and its API calls
  pass CORS preflights). Some embedded views block requests from
  opaque-origin pages outright; those cannot show web apps.
- **Cost:** a bundle is read from Core once per installed version, and kept
  in the bridge's memory.
- **Smoke time:** boot 1 now walks every phase's path, so a boot may take
  480 s (xtask and the kernel's smoke-mode limit, kept equal).

## Alternatives considered

- **Serving web apps under the system's origin (8080):** they would then
  ride the user's session cookie into the System API.
- **One port per app:** this needs as many forwarded ports and listeners
  as apps. The sandbox gives each page its own (opaque) origin on one
  port.
- **`allow-same-origin` in the sandbox:** apps would share the app port's
  origin, and each other's tokens and data.
- **Letting web apps use the network or files now:** that needs
  permissions that make sense for code running in a browser on another
  device. Data of their own is the safe start.
- **Waiting for the on-device web view:** Phase 8 would then have no
  SvelteKit or Bun story for developers. This path works today and
  carries over.

## Checklist (master spec §48)

- **Purpose:** SvelteKit and Bun apps, completing Phase 8.
- **Architecture:**
  - `runtime = web`;
  - web bundles;
  - Core `WEB_BUNDLE`;
  - the bridge's app port (sandboxed pages, public files, per-app token
    API);
  - the `sveltekit` template and its build.
- **API:**
  - `oceans_package::web`, `Runtime::Web`;
  - Core `WEB_BUNDLE` (`[length u64]` + version, and the bundle);
  - `/ID/`, `/ID/api/data/NAME`;
  - `$lib/oceans.js` (`load`, `save`).
- **Dependencies:** the template's packages are pinned by its `bun.lock`:
  SvelteKit, its static adapter, Svelte, Vite, the versions the system UI
  already uses.
- **Security:**
  - signed and verified like every package;
  - separate origin and sandbox;
  - pages only for the paired browser;
  - tokens per app and per pairing;
  - only the app's own data, only with `storage`;
  - bundles and paths checked twice;
  - the manifest allows nothing else.
- **Testing:**
  - unit: bundle formats in Rust and Go (paths, duplicates, truncation,
    trailing bytes), manifest rules, the bridge's pages, files and API
    (pairing, sandbox, tokens, other apps, unpairing), the tool's bundle
    and template, the UI's link;
  - smoke: as above;
  - a manual check of the sandbox in Chromium.
- **Failure behaviour:**
  - not paired: 401;
  - a bad bundle: 502;
  - a native app's path: 404;
  - a wrong token: 403;
  - no `storage`: 403;
  - a value too large: 413;
  - nothing stored: 404.
