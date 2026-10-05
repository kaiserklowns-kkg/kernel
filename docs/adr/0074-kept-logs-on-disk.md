# ADR-0074: The log kept on disk across reboots

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0070 (the kept log, `diag`), ADR-0016 (services),
  ADR-0022 (the root filesystem)
- Part of Phase 10 (Alpha: diagnostics and recovery).

## Context

The kernel keeps the last 64 KiB of the log in memory (ADR-0070). A machine
that hangs or reboots loses it, and those are the failures an alpha on
real hardware will have. The tester then has nothing to send.

## Decision

### `logkeep`, a service

- **Capabilities:** `log`, `log-read` and `use = fs`, granted in the
  service manifest, nothing else.
- **At start:**
  - it renames `/system/logs/boot.log` (the last boot's) to
    `previous-boot.log`, replacing the one before;
  - it creates an empty `boot.log`.
- **Every 2 seconds:** it copies the new text of the kernel's log
  (`LOG_READ` from where it stopped) into `boot.log`, and syncs when
  something was copied.
  - If text gave way before it was copied, it notes how much was lost.
- **At most 512 KiB per boot:** the start of a boot is what explains it.
  After that, a note marks where the file stops.
- **It is a `restart = always` service:** it never exits. Without a disk
  it can use, it logs why once and waits rather than exit, since a
  restart would not help.

### `diag previous`

- **What it shows:** `run diag out logs use:fs -- previous [LINES]` prints
  the previous boot's trouble (the same markers as `diag crashes`), then
  its last lines (20 by default).
- **It needs `logs`,** like the rest of `diag`: an old log is as sensitive
  as the current one.
- **`diag save` adds the previous boot** (its trouble and its log) to the
  report when there is one.

## Consequences

- After a hang or a reset, the next boot has the log up to the last copy.
  The tester can read it or save it to the stick (`diag save /usb/diag.txt`)
  and send it.
- **Limits:**
  - **The last 2 seconds** before a hard hang or a power cut may be
    missing. A kernel panic is not written by the kernel itself (no disk
    from inside a panic).
  - **Two boots only:** the current and the previous. Rotation by count or
    age is later work.
  - **Writes:** one sync every 2 seconds while the log grows, nothing when
    it is quiet. The copy-on-write root (ADR-0022) makes each sync one
    commit.
- **Privacy:** the logs stay in `/system/logs`, which apps' storage
  grants (`storage`, `storage:/PATH`) never reach. Reading them takes
  `logs`.

## Alternatives considered

- **The kernel writing the log to disk:** the kernel has no filesystem, by
  design (ADR-0015).
- **Persisting from the shell or `diag`:** only when someone runs it, and
  never for a boot that hung.
- **Writing every line as it comes:** a sync per line on a busy boot is a
  commit storm, for little more than 2 seconds of extra coverage.

## Checklist (master spec §48)

- **Purpose:** what happened in a boot that crashed or hung, on the next
  boot.
- **Architecture:**
  - `logkeep` (a service, in `utils`) copying `LOG_READ` into
    `/system/logs`;
  - `diag previous`, and `diag save` including the previous boot.
- **API:** `/system/logs/boot.log`, `/system/logs/previous-boot.log`;
  `diag previous [LINES]`.
- **Dependencies:** none.
- **Security:**
  - least capabilities for the service;
  - logs outside apps' reach;
  - reading them takes `logs`.
- **Testing:** smoke. The second boot's `diag previous` shows the first
  boot's crasher faults.
- **Failure behaviour:**
  - no disk: logged once, nothing kept;
  - a full file: noted and stopped;
  - lost text: noted.
