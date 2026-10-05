# ADR-0062: The Oceans SDK and developer tools

- Status: Accepted (amended by ADR-0064: SvelteKit/Bun apps run as web apps through the bridge)
- Date: 2026-10-05
- Depends on: ADR-0045 (API level 1), ADR-0046 (packages), ADR-0050 and
  ADR-0052 (Go apps), ADR-0059 to ADR-0061 (windows, the Store),
  ADR-0063 (developer keys)
- Starts Phase 8.

## Context

Phase 8 asks for an SDK, application templates, developer tools, and
support for Rust, Go, SvelteKit and Bun (master spec §40, §47). Its exit
criterion is a third-party app built with the SDK.

Until now, every app was built inside this repository:
- `cargo xtask` packaged the examples;
- they were signed with the development key;
- they used whichever crates they liked, including service protocols and
  `oceans-rt` internals.

An outside developer had no supported way to start, build, sign or ship
an app.

## Decision

### The SDK

The SDK is this repository, used from outside it:

- **The Rust API: the `oceans-sdk` crate** (`user/sdk`), API level 1 by
  area as §40 lists them:

  | Area | Module | What it covers |
  |---|---|---|
  | Application | `app` | `entry!`, `Directory`, identity, arguments, console |
  | Storage | `storage` | the app's data and the user's files |
  | Network | `network` | net-proto |
  | UI | `ui` | windows |
  | System | `system` | memory, uptime |
  | Permission | `permission` | the manifest's names |

  Apps depend on it alone. It is the stable surface: what is behind it may
  change.
- **The Go API: `go/oceans` and its packages**, as Go apps already use it.
  A project points its module at the SDK on disk (`replace`).
- **Not open to apps at API level 1:**
  - notifications: desktop toasts are the system's;
  - the AI runtime: there is no permission for it yet.

  These come with permissions designed for them.
- **SvelteKit and Bun:**
  - Bun builds the system's web experience (ADR-0058), but web apps cannot
    run on Oceans until the on-device web view exists (ADR-0056);
  - their SDK support waits for that decision. Until then, Rust and Go are
    the app languages.

### Templates (`sdk/templates`)

There are two templates, `rust` and `go`: a console app that says who it
is, counts its runs in its storage and shows the free memory. Each carries:
- a manifest asking for what it uses;
- for Rust: the build configuration (target, code model, soft crypto
  flags) and the pinned toolchain;
- for Go: the module's `replace`.

The window examples (Notes in Rust, Tiles in Go) show the UI API.

### The developer tool `oceans` (`tools/oceans`, Rust, host)

| Command | What it does |
|---|---|
| `oceans new rust\|go ID [DIR]` | a project from a template. Names are checked before they reach files: ids as Core checks them; names and publishers as plain text. |
| `oceans keygen PUBLISHER` | a developer key (OS randomness, ADR-0063), written to a file it never overwrites, and the console command that trusts it |
| `oceans build [DIR] --key FILE` | builds natively (`cargo`) or as wasm (`go`), checks the program fits the runtime and the manifest's publisher is the key's, and signs `dist/ID-VERSION.opk` |
| `oceans trust --key FILE` | the trust command again |
| `oceans serve [DIR]` | serves `dist/` as a store (ADR-0061): `index.json` with sizes and SHA-256, and the packages |

Its logic is a library with unit tests: templates, key files, packaging and
store indexes. The command line is a thin layer over it.

## Consequences

- **A developer outside this repository can:**
  1. start from a template;
  2. sign as themselves;
  3. ship through a file or a store.

  The user decides, at the console, whether their key is trusted
  (ADR-0063).
- **Phase 8's exit criterion passes in `cargo xtask smoke`.** A key, a Rust
  app and a Go app are made with the tool in a directory outside the
  repository, built and signed there. On Oceans:
  1. the Rust app is refused (untrusted key);
  2. the key is trusted at the console;
  3. both apps install and run;
  4. after a reboot, the Rust app still runs (its third run).
- **Not yet:**
  - SvelteKit/Bun apps, which wait for the web view;
  - an SDK release outside this repository (a versioned download);
  - windows in the templates, which the examples show;
  - API levels beyond 1.

## Alternatives considered

- **Writing the tool in TypeScript on Bun:** the package format, the
  archive and the signatures already exist in Rust (`libs/package`).
  Reimplementing them would make two signers to keep in step.
- **Templates the tool downloads:** they are embedded, so the tool works
  offline and matches its SDK.
- **Apps depending on the service protocol crates directly:** that couples
  third-party code to internals. The facade is the contract.

## Checklist (master spec §48)

- **Purpose:** the SDK, templates and developer tools of Phase 8.
- **Architecture:** the `oceans-sdk` facade; `go/oceans`; `sdk/templates`;
  the `oceans` tool (library and CLI).
- **API:** `oceans_sdk::{app, storage, network, ui, system, permission}`;
  the commands above.
- **Dependencies:** the tool uses `getrandom` 0.2 (OS randomness) and
  `sha2`, both already in the workspace.
- **Security:**
  - packages are signed by the developer's own key;
  - the tool refuses a publisher that is not the key's;
  - it never overwrites a key and never prints the seed (even in `Debug`);
  - it checks names before writing them into files;
  - the device trusts keys only from its console (ADR-0063).
- **Testing:** unit (templates filled completely, name checks, keys round
  trip, signing for the right publisher only, store index); smoke (the exit
  criterion above).
- **Failure behaviour:** clear messages for a missing manifest, a failed
  build, a wrong runtime, a publisher mismatch, an existing directory or
  key.
