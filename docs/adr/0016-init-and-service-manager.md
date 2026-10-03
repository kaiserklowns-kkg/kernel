# ADR-0016: init and the service manager

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0011 (capabilities), ADR-0014/0015 (processes, ABI v2)
- Adds: ABI v3 (syscalls 18–22)
- Inputs: [reference-systems.md](../architecture/reference-systems.md) §2
  (Redox init.d units and per-user scheme lists)

## Context

Every boot needs a first user process that brings up the system's services
and keeps them running (master spec Phase 3). In Oceans it must also be the
**root of capability distribution**: services receive authority only from
it, as declared, with no ambient access (ADR-0006).

## Decision

### Boot contract (kernel → init)

The kernel starts the boot module `init` on every boot. It owns no service
policy. init receives exactly:

| Handle | Capability |
|---|---|
| 0 | kernel log (`WRITE`, `DUPLICATE`, `TRANSFER`) |
| 1 | module table: lines `<name> <handle index>` (memory, `READ`, `MAP`) |
| 2… | every boot module as a memory object (`READ`, `MAP`), at most 14 |

The argument word is 1 in smoke-test boots and 0 otherwise. If init exits,
the boot thread logs it as an error: on a normal boot it should never exit.

### Manifest: `services.conf` (a boot module)

```
service echo
    image = echo-service      # boot module holding the program
    restart = always          # always | on-failure | never
    max-restarts = 5          # default 5
    grant = log               # a log capability
    provide = echo            # server end of a new endpoint "echo"

service hello
    image = hello-client
    restart = never
    grant = log
    use = echo                # a client end of endpoint "echo"
```

- Services start in file order. A service receives **exactly** its
  `grant`/`provide`/`use` capabilities, as handles 0, 1, … **in the order
  written**.
- `use` must name an endpoint provided by an earlier service; otherwise
  init refuses the whole manifest.
- Errors are reported as `services.conf:<line>: <problem>`. Unknown
  settings are errors, not ignored.
- Test-only keys `expect-exit = N` and `expect-runs = N` are checked in
  smoke mode.
- Endpoints: init keeps each client end in a registry and hands out
  duplicates with only `SEND` and `TRANSFER`. A restarted provider gets a
  new endpoint; existing users see `PeerClosed`. Reconnection through a
  name service is future work.

### Supervision

- One notification, one bit per service. `PROCESS_WATCH` makes the kernel
  set the bit when the service exits. init sleeps in `NOTIFICATION_WAIT`,
  so supervision is event-driven, with no polling.
- On exit:
  - collect the code with `PROCESS_WAIT`, which does not block;
  - apply the policy: `always` restarts, `on-failure` restarts on a
    non-zero exit, `never` lets the service end;
  - back off before restarting: 100 ms × 2^n, capped at 2 s;
  - stop after `max-restarts` and log that it gave up.
- A service that cannot be started (bad image, no memory) is logged and
  settled. Grants that were not handed over are closed.
- init needs no allocator. Everything is fixed-size (32 services,
  8 grants, 16 endpoints), and strings borrow the mapped manifest. Text
  modules must fit in one page; a larger one is refused, never truncated.

### ABI v3 (additions only)

| # | Call | Notes |
|---|---|---|
| 18 | `NOTIFICATION_CREATE` → handle | `SIGNAL`, `WAIT`, `DUPLICATE`, `TRANSFER` |
| 19 | `NOTIFICATION_SIGNAL`(n, bits) | needs `SIGNAL`, never blocks |
| 20 | `NOTIFICATION_WAIT`(n) → bits | needs `WAIT`; returns and clears |
| 21 | `PROCESS_WATCH`(process, n, bits) | `WAIT` on the process, `SIGNAL` on n; fires at once if the process already exited |
| 22 | `SLEEP`(ms) | |

`PROCESS_SPAWN` also gains an optional name. Its sixth argument was always
0 for ABI 2 callers; it can now point to a 32-byte buffer, and the child is
then named `<parent>/<name>` in logs (`[init/echo] echo: ready`).

### Programs

| Program | Role |
|---|---|
| `user/init` | init and the service manager |
| `user/echo-service` | provides `echo`: answers with the label + 1 and the data in upper case |
| `user/hello-client` | uses `echo` once and logs the reply |
| `user/crasher` | faults on purpose (smoke manifest only) |

Manifests live in `config/`: `services.conf` for normal boots and
`services-smoke.conf` for smoke tests. `cargo xtask` packages the right one
as `boot/services.conf`.

> **Extended by [ADR-0018](0018-shell.md):** every service also receives a
> handle directory as its last handle; `grant = module:NAME` grants a
> program image; granted capabilities include `DUPLICATE`.
> **Extended by [ADR-0017](0017-console-input.md) and
> [ADR-0020](0020-system-information-and-utilities.md):** init also receives
> the console (handle 2) and system information (handle 3); boot modules
> start at handle 4; `grant = console`, `grant = sysinfo`.

## Consequences

- Authority in the running system is now visible in one file: what each
  service can do is exactly what `services.conf` grants it.
- init is a single point of failure. A crashing init leaves services
  running unsupervised (the kernel logs it). Restarting init needs state
  hand-over and is future work.
- Restarting a provider breaks its clients' endpoints until there is a
  name service with reconnection.

## Testing

Smoke boot with `services-smoke.conf` (init in test mode, checked by the
kernel through init's exit code):
- `echo` starts; `hello` calls it and exits 0 after exactly 1 run;
- `crasher` faults on every run. It is restarted after 100 ms and 200 ms
  backoff, and then given up: exit −142 after exactly 3 runs;
- init verifies all expectations and exits 0.

Normal boot, release build: init starts `echo` and `hello`, `hello`
receives "HELLO, OCEANS", and init keeps supervising `echo`. Also passes in
debug and with `-cpu max`.
