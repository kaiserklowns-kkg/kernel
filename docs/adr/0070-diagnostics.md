# ADR-0070: Diagnostics: the kept log and `diag` (ABI 15)

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0014 (logging), ADR-0016 (init grants), ADR-0068
  (hardware validation)
- Adds: ABI 15 (`LOG_READ`)
- Part of Phase 10 (Alpha: diagnostics).

## Context

Phase 10 asks for diagnostics. The master spec (§42) asks for:
- kernel and service logs, crash reports and diagnostics;
- diagnostics that never depend only on a GUI;
- CLI tools.

Every log line went to the serial port, and lines at INFO and above to the
screen as well. On a real machine there is rarely a serial cable, and the
desktop covers the console. When something failed in the field there was
nothing to read and nothing to send.

## Decision

### The kernel keeps the log (ABI 15)

- **The last 64 KiB** of lines at INFO and above, services' lines
  included, stay in a ring in the kernel. DEBUG stays on serial only.
- **`LOG_READ (log, from, ptr, capacity)`** copies kept text from byte
  position `from`, or from the oldest still kept. It returns how much was
  copied and where it starts, so a reader continues without gaps or
  repeats. At most 16 KiB per call.
- **It needs `READ` on the Log object.** init holds it; services' `log`
  grant stays write-only. Logs can hold what users and agents did, so
  reading is a grant of its own: **`grant = log-read`** (directory
  `logs`). Today only the shell has it.
- **The ring's lock:** taken only with preemption disabled (under the
  console lock, or by `LOG_READ`), never by an interrupt handler. A panic
  writes to it only if the lock is free.

### `diag`

```text
run diag out logs -- log [LINES]   the last lines (40)
run diag out logs -- crashes       what went wrong
run diag out logs sysinfo use:fs -- save /usb/diag.txt
```

- **What `crashes` finds:** processes killed by faults, panics, failures,
  restarts, warnings and errors.
- **What `save` writes:** a report (system, memory, uptime, time, what went
  wrong, then the whole kept log) to a file, for instance on the USB stick,
  to send with a hardware report (ADR-0068).
- The shell grants `logs` only when asked (`run diag out logs …`).

## Consequences

- A machine without a serial port can still say what went wrong.
- **Limits:**
  - 64 KiB is a few minutes of a busy boot: the oldest lines give way;
  - `diag save` writes what is kept at that moment;
  - persistent logs, structured fields, metrics and tracing are later
    work.
- **The Control Center** could show the log through the bridge. It does
  not, because pairing does not grant `logs`, and logs are sensitive.

## Alternatives considered

- **Reading the log through `sysinfo`:** apps get `system-info`
  automatically, so they would read what users and agents did.
- **A log service in userspace:** the kernel's own lines (faults, panics)
  are exactly what must survive a broken userspace.
- **Persisting the log to disk now:** it needs a policy (size, rotation,
  privacy) of its own.

## Checklist (master spec §48)

- **Purpose:** diagnostics without a cable or a screen.
- **Architecture:**
  - the kernel log ring;
  - `LOG_READ`;
  - init's `log-read` grant;
  - the shell's `logs` grant word;
  - `diag`.
- **API:**
  - `LOG_READ` (47), `LOG_RING`, `LOG_READ_MAX`;
  - `oceans_rt::log_read`;
  - `grant = log-read`;
  - `diag log | crashes | save`.
- **Dependencies:** none.
- **Security:**
  - reading the log is its own right, granted on request;
  - services and apps cannot read it;
  - the kernel copies only within the caller's buffer.
- **Testing:** smoke:
  - `diag` without `logs` is refused;
  - with it, `crashes` finds the crasher test service's faults;
  - `save` writes a report to a file;
  - ABI 15 everywhere (both smoke tests).
- **Failure behaviour:**
  - nothing kept: an empty answer;
  - a reader overtaken by new lines starts again from the oldest kept;
  - an unwritable path: `diag` says why.
