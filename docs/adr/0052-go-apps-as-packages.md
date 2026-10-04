# ADR-0052: Go apps as packages

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0045 (Oceans Core), ADR-0046 (packages), ADR-0047
  (permissions and consent), ADR-0049 (app services), ADR-0050 (Go on
  Oceans)

## Context

ADR-0050 made Go run on Oceans, but only from the boot image: init starts
`gohost` with `grant = module:NAME.wasm`. Apps that users install come
as signed packages (ADR-0046), and Oceans Core starts only native ELF
programs from them. ADR-0050 left this open: "Go programs installed as
packages ... come with a package runtime field".

So a Go app had none of what packages give:
- verification at install and at every start;
- permissions from its manifest and the user's consent (ADR-0047);
- updates, rollback and removal;
- services that restart and start at boot (ADR-0049);
- the audit log.

Go is the language the master spec puts above the Rust system (§5, §8).
Writing an app in Go should not mean giving any of this up.

## Decision

### The manifest's `runtime`

A new manifest key: **`runtime = native | wasm`**, `native` when absent.
Every existing package stays valid and means the same.

- **`native`:** the entry is an x86-64 ELF executable, spawned directly,
  as before.
- **`wasm`:** the entry is a WebAssembly module built for wasip1 (a Go
  program, ADR-0050). The Go host runs it.

Parsing stays strict: any other value is refused, and so is the key
twice.

**Verification** (`Package::open`, at install and at every start) now
also checks that the entry matches its runtime:
- `wasm`: the WebAssembly magic `\0asm` and binary format version 1;
- `native`: the ELF magic.

A mismatch is `WrongFormat`: "the program is not a WebAssembly module"
(or "... not an ELF executable"). The signature already covers the
entry; this check stops a package that was signed by mistake from
reaching the wrong loader. The loaders still check the rest: the kernel
the ELF, wasmi the module.

### Core runs `wasm` apps in the Go host

Core gets `grant = module:gohost` (both service manifests). It keeps
that read-only image for as long as it runs.

Starting a `wasm` app is the same path as a native one:
1. the same checks: installed, not running, no undecided permission;
2. the package read from disk and verified again;
3. the same handles, by the same rules: one per allowed permission, `log`
   for services, `app info`, `args`.

**Two things differ:**
- **The image spawned is gohost.** The process is still named after the
  entry (`greeter.wasm`).
- **The program is one more handle:** a read-only memory object (read,
  map, transfer; no write, no duplicate), listed first as
  `module ENTRY`. Gohost runs the first `module` of its directory.

So the program cannot be changed by the app it is, and the Go host gets
no authority beyond the app's. Without gohost, starting a `wasm` app
fails with "this system has no Go host (gohost)". Installing still works.

Everything else is untouched, and the same for both runtimes: consent,
revocation (which stops the process), `STOP`, exits and service
restarts, and the audit.

**Output** follows ADR-0050's rule: the console if the app was granted
it (`console out`), otherwise the log, a line at a time. Services have
`log log` and no console, so a Go service writes to the system log.

`INFO` gets a field, **`RUNTIME`** (`native` or `wasm`). The shell's
`app info` shows it as `runtime: wasm`. An older Core answers
`BadRequest` for the field, and the shell then prints nothing.

### A Go app's identity: `read_text`

Core gives every app its identity as a text object, `app info` (`ID
VERSION`). A Go module cannot map memory objects. The Go host gets one
more host function:

**`read_text(memory, buf, cap) -> len`**
- maps the object read-only;
- copies at most `cap` bytes (no more than 64 KiB) into a host buffer,
  then unmaps it;
- cuts the text at the first NUL;
- checks that it is UTF-8;
- copies it into module memory, with the usual range check.

Errors:
- `TooLarge` if the text does not fit (the caller may retry larger);
- `InvalidArgument` if it is not UTF-8;
- the kernel's own error for a handle that is not a mappable memory
  object.

The module never sees the mapping. The host copies rather than
borrowing the bytes, since another process may hold the object
writable.

**`go/oceans` gains:**

| Function | Purpose |
|---|---|
| `ReadText(h)` | wraps `read_text`; retries once at `MaxText` |
| `AppInfo()` | `Find("app", "info")`, read and parsed into `App{ID, Version}` |
| `ErrNotAnApp` | no `app info`: not started by Core |
| `Memory(sysinfo)` | the memory figures as `MemoryInfo` |
| `ParseAppInfo`, `ParseMemory` | the pure parsers, unit-tested on the host |

Arguments need nothing new. Core's `args` object already reaches Go as
`os.Args` (WASI) and as `oceans.Args()`.

### The example: `go/apps/greeter`

Package `app.oceans.greeter`, `kind = app`, `runtime = wasm`, entry
`greeter.wasm`. Permissions: `console` and `system-info`, both granted
without asking.

It prints:
- its id and version, from `app info`;
- its arguments;
- the free and total memory, through `sysinfo`.

**`cargo xtask`:**
- builds it like the boot Go programs, into `build/go`, but **not** into
  the boot archive (`GO_APPS`);
- signs it with the development key as `greeter-1.0.0.opk`.

The same program is also packaged as a **service**,
`app.oceans.greeter-service` (`greeter-service-1.0.0.opk`, only the id,
name and `kind` differ). It has no console, so its output goes to the
log. That covers the Go host's second output path, and a `wasm` service
started at boot.

**The smoke disk grows from 8 to 32 MiB.** Go packages are 2.6 MB each,
and the first boot keeps both the downloaded and the installed copy.

## Consequences

- **A Go app is an app.** It is signed, verified at every start, and
  runs with the permissions its manifest asks for and the user allows.
  It can be updated, rolled back and removed, run as a service, and
  audited. The rules are exactly those for native apps.
- **A `wasm` package is portable in principle:** the module does not
  depend on the CPU. `architecture` stays `x86_64` for now, since that
  is the only system that runs one. A portable value is for when there
  is a second architecture.
- **Cost:**
  - packages are a few MB (the Go runtime);
  - every start verifies the whole package again: SHA-512 over the
    module, then an Ed25519 signature check;
  - each start then interprets the module (ADR-0050). That suits apps
    that mostly wait.
- **The Go host is now part of the trusted base for apps.** A flaw in
  wasmi or in a host function would let a module act with the Go host's
  authority. That is the app's own authority, nothing more, because
  Core spawns a fresh gohost process with exactly the app's handles.
- **Not yet:**
  - files from Go (`storage`, `files`): the Go `fs` binding is separate
    work;
  - WASI preopens for an app's storage;
  - modules shared between apps (every start copies the program into a
    fresh memory object);
  - a portable `architecture`.

## Alternatives considered

- **Deciding by the entry's magic bytes alone, no manifest key:** the
  runtime would be implicit. The manifest says what an app is, and the
  user and tools read it; a declared runtime that must also match the
  bytes catches both mistakes.
- **Shipping gohost inside each package:** every Go app would carry
  (and sign) its own copy of the host. The host is part of the system,
  updated with it; packages carry only the program.
- **Core interpreting the module itself:** Core would hold every app's
  authority while running untrusted code. A separate gohost process per
  app keeps each app in its own process, with only its own capabilities.
- **A memory-mapping host function** (handing the module a view of the
  object): wasm modules address only their linear memory, and a
  shared mapping would let another writer change bytes the module is
  reading. Copying bounded text is simple and safe.

## Checklist (master spec §48)

- **Purpose:** Go programs installed, verified, permitted and managed as
  apps and services, like native ones.
- **Architecture:**
  - the manifest's `runtime`;
  - Core spawning gohost with the program as `module ENTRY`;
  - gohost's `read_text`;
  - `go/oceans` app helpers;
  - the example `go/apps/greeter`.
- **API:**
  - manifest key `runtime = native | wasm`;
  - `PackageError::WrongFormat`;
  - Core `INFO` field `RUNTIME` (10);
  - host function `oceans.read_text`;
  - Go `ReadText`, `AppInfo`, `Memory`, `MaxText`, `ErrNotAnApp`;
  - Core's new grant `module:gohost`.
- **Dependencies:** none new (wasmi, Go 1.26 as in ADR-0050).
- **Security:**
  - the same verification, permission, consent and audit path for both
    runtimes;
  - the program is handed over read-only;
  - the Go host gets exactly the app's capabilities;
  - `read_text` copies bounded, checked text, never a mapping;
  - the entry must match its declared runtime.
- **Testing:**
  - **Host (`oceans-package`):**
    - `runtime` parsing (default, both values, bad values, duplicate);
    - `Runtime::accepts`;
    - packages whose entry does not match their runtime are refused.
  - **Go:** `ParseAppInfo`, `ParseMemory`, `describeArgs`; host-build
    stubs.
  - **Smoke, boot 1:**
    - Greeter fetched over HTTP and installed;
    - `app info` shows `runtime: wasm`;
    - `app run app.oceans.greeter alpha beta` prints its identity, both
      arguments and the memory figures (console path), exits 0;
    - the audit names its permissions;
    - Greeter Service is installed and enabled; it prints to the log
      (log path) and exits 0.
  - **Smoke, boot 2:**
    - four apps load;
    - Greeter Service starts at boot and logs;
    - Greeter runs again with new arguments.
- **Failure behaviour:**
  - a wrong-format entry is refused at install (and at start);
  - no gohost: the start fails with a reason;
  - module errors, traps and exit codes are as in ADR-0050, reported
    through Core's exit handling (and restarts for services);
  - `read_text` returns explicit errors (`TooLarge`, `InvalidArgument`,
    the kernel's for bad handles).
