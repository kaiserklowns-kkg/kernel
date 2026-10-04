# ADR-0045: The Oceans Runtime: apps, Oceans Core and API level 1

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0006 (security model), ADR-0016 (init), ADR-0019 (file
  protocol), ADR-0020 (program manifests), ADR-0044 (stopping processes)
- Starts Phase 5. Packages: ADR-0046. Permissions and consent: ADR-0047.

## Context

Phase 5 turns programs into **apps**. They are installed from packages,
run with the permissions the user granted, and stopped, updated and
removed. This is the layer the master spec puts between the kernel and
everything a user installs (§8, §19, §23, §27–28, §41). The AI runtime
(Phase 6) and the UI (Phase 7) are built on it: §55 forbids building UI
before the system API is defined.

**Until now:**
- Programs in `/bin` came with the boot image.
- The shell ran them with what their ELF manifest requested, low-risk
  grants only (ADR-0020), or with grants typed by the user (`run`).
- Nothing could install software, remember what a user allowed, or stop
  a program.

## Decision

### Oceans Core

A service, `core`, is the app manager, package manager and permission
broker in one process.

- **Why one process:**
  - all three hold the same authority: the capabilities apps receive;
  - all three need the same state: installed apps and the user's
    decisions;
  - splitting them would add IPC between parties that must trust each
    other anyway.
- **Its own authority** (`services.conf`), and nothing else:
  - the filesystem root;
  - what it may pass on: `use = net`, `use = input`, `sysinfo`;
  - the trusted publisher keys, `module:trust.keys` from the boot image.
- **Storage:**
  - `/apps/ID/package.opk`, the installed package;
  - `previous.opk`, for rollback;
  - `data/`, the app's private storage;
  - `/system/permissions`, the user's decisions;
  - `/system/audit.log`;
  - `/home`, the user's files.

### The `core` protocol (`oceans-core-proto`)

One endpoint, `core`, with these requests:

| Request | Does |
|---|---|
| `INSTALL` | carries the package as an open file handle, so Core reads exactly what the caller could read |
| `LIST`, `INFO`, `PERMISSION` | queries |
| `RUN` | foreground or detached; the caller passes the console the app may write to |
| `STOP`, `REMOVE`, `ROLLBACK` | lifecycle |
| `DECIDE` | a user's permission decision |
| `AUDIT` | recent entries |

- **Who holds it:** its unbadged client end is full authority over apps,
  so it goes to the user's agent (the shell today, the system UI later)
  and never to an app.
- **Narrower authority later:** e.g. "may start apps" for the AI runtime,
  as badged ends minted by Core.

### Lifecycle

- **Install:** verified (ADR-0046), then written beside any installed
  version and renamed into place (atomic on the Oceans volume). The data
  directory is created.
- **Update:**
  - only a newer version, signed with the same key as the installed one,
    is accepted (`NotNewer`, `KeyChanged`);
  - the replaced version is kept for `ROLLBACK`;
  - data is kept;
  - decisions about permissions the new version no longer asks for are
    dropped, and new requests are asked at the next start.
- **Run:**
  - Core reads the package again and **verifies it again**: what runs is
    what was signed, even if `/apps` was changed;
  - the app's program is spawned with one handle per granted permission
    (ADR-0047), its identity, its arguments and a handle directory;
  - one instance per app;
  - a foreground run gives the caller a wait-only process handle;
  - exits are noticed through a notification bound to Core's endpoint.
- **Stop:** `PROCESS_KILL` (ADR-0044), then Core waits for the exit.
- **Remove:** stops the app, deletes its directory (or keeps its data on
  request) and its decisions.
- **Audit:** every install, update, rollback, start, stop, decision and
  removal is recorded, with the time, in `/system/audit.log` and the
  kernel log.

### API level 1: what an app sees

An app is an ordinary Oceans process (ABI 12) whose handle directory
(ADR-0020) lists exactly:

| Directory line | Present when | Is |
|---|---|---|
| `console out` | `console` | output to the terminal that ran it |
| `use storage` | `storage` | its data directory (an fs node, read-write) |
| `sysinfo sysinfo` | `system-info` | read-only system information |
| `use net` | `network`, allowed | the network service (ADR-0023, ADR-0043) |
| `use files` | `files`, allowed | `/home` (an fs node, read-write) |
| `use input` | `pointer`, allowed | pointer events (ADR-0042) |
| `app info` | always | text `ID VERSION` |
| `args args` | arguments given | text |

- **The protocols behind those handles** are the API: fs-proto,
  net-proto, input-proto, the console, sysinfo.
- **Manifests:** a manifest names the level it needs (`api = 1`). Core
  refuses packages needing a newer one.
- **Changes:** a later level only adds. Removing or changing anything
  apps rely on needs a new level and a period where both work.

### The shell as the user's agent

The shell's `app` commands:

| Command | Does |
|---|---|
| `app list`, `app info ID` | queries |
| `app install PATH` | installs or updates |
| `app run ID [ARGS]`, `app start ID [ARGS]` | runs (`start`: detached) |
| `app stop ID`, `app remove ID [--keep-data]` | lifecycle |
| `app grant ID PERM`, `app revoke ID PERM` | decisions |
| `app rollback ID`, `app audit` | rollback, audit trail |

The shell is also the consent agent (ADR-0047).

### Examples and tools

- **The example app:** `user/apps/hello` (Hello, `app.oceans.hello`). It
  greets, counts its runs in its storage, and talks to a server over the
  network when allowed.
- **Packages:** `cargo xtask` packages and signs it, with the development
  key, into `build/packages/` with every image build.

## Consequences

- **Done:** software can be installed, updated, rolled back and removed
  without rebuilding the image. Apps get least privilege by construction:
  nothing they did not ask for, nothing the user did not allow.
- **The foundation for later phases:**
  - **the Store (Phase 7):** a front end to `INSTALL` and the manifest
    data;
  - **the AI runtime (Phase 6):** apps whose permissions it mediates,
    with approvals rendered by the system (ADR-0007);
  - **the SDK (Phase 8):** API level 1 as its contract.
- **Not done yet:**
  - **dependencies between packages:** the manifest format reserves them,
    but none are accepted yet;
  - **several instances of one app;**
  - **background services with restart policies** (apps that run at
    boot);
  - **resource limits per app** (memory, CPU);
  - **a narrower Core capability** for other agents.

## Alternatives considered

- **Installing apps into `/bin` and keeping the ELF manifest grants:**
  - no signatures and no decisions per user;
  - a program could ask for anything at every run;
  - `/bin` comes from the boot image and is read-only.
- **The shell as app manager:**
  - the shell would have to hold every capability apps may get;
  - the UI could not reuse it.
- **One process per role (packages, permissions, launching):** more IPC
  and more trusted processes for no separation gained (see above).
- **Extracting packages to files on install:** verification then covers
  only the moment of installation. Keeping the signed package and
  verifying at every start costs a hash over the package, measured in
  milliseconds for typical apps.

## Checklist (master spec §48)

- **Purpose:** apps, installed and run with user-granted permissions.
- **Architecture:**
  - the `core` service;
  - `oceans-core-proto`;
  - `oceans-package` (ADR-0046);
  - the shell's `app` commands;
  - API level 1.
- **API:**
  - the `core` protocol (`INSTALL` … `AUDIT`);
  - API level 1 (the handle directory above);
  - manifests (ADR-0046).
- **Dependencies:** Ed25519 and SHA-512 from the RustCrypto crates
  already used by TLS (ADR-0031).
- **Security:**
  - apps get only granted capabilities;
  - packages are verified at install and at every start;
  - updates must keep the key;
  - the Core capability goes only to the user's agent;
  - revocation stops the app;
  - everything is audited.
- **Testing:**
  - **Host:** manifest, signature, trust and tampering tests
    (`oceans-package`).
  - **Smoke:**
    - packages fetched over HTTP;
    - an untrusted and a tampered package refused;
    - install, a consent prompt answered "no", then a grant;
    - storage persisting across runs and across a reboot;
    - the network used once allowed;
    - an update, a downgrade refused;
    - a revocation that stops the running app;
    - a rollback, a stop, the audit trail;
    - removal on the second boot.
  - **Host check afterwards:** the app is gone, and the audit log holds
    every step.
- **Failure behaviour:**
  - refused packages say why;
  - a damaged installed package is not loaded (logged), and does not
    start;
  - storage errors are reported, and the previous state stays (writes
    and renames are atomic);
  - an app that crashes is reaped, and its exit is logged.
