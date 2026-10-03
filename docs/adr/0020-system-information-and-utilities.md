# ADR-0020: System information, program manifests and basic utilities

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0016 (init), ADR-0018 (shell), ADR-0019 (filesystem)
- Adds: ABI v6 (`SYSTEM_INFO`); completes Phase 3

## Context

Phase 3 ends with basic utilities. Tools like `ps` need information about
the whole system, which must still be authority someone grants, not
ambient. And typing `run ps out sysinfo` for every command is not usable,
yet a shell that grants commands whatever they like would undo the
capability model (master spec §23: apps do not get access automatically;
§41: apps declare what they need).

## Decision

### System information is a capability (ABI v6)

- A `SystemInfo` kernel object with `READ` (plus `DUPLICATE` and
  `TRANSFER`). **init receives it as handle 3**; boot modules now start at
  handle 4. Services get it through `grant = sysinfo`.
- `SYSTEM_INFO(sysinfo, kind, ptr, capacity) → len` returns fixed-size,
  little-endian records, defined and round-trip-tested in
  `oceans_abi::sysinfo`:

  | Kind | Record |
  |---|---|
  | `KERNEL` | ABI version, kernel version, architecture |
  | `MEMORY` | page size, managed and free frames, kernel heap in use |
  | `UPTIME` | ticks, tick rate |
  | `PROCESSES` | per process: id, **parent id**, exit code or running, user memory mapped, name |
- The kernel keeps a registry of live process records (weak references,
  pruned on read) and the parent of each process.
- Read-only by design: nothing in it lets a holder affect the system.
  Killing processes or changing settings would be separate capabilities.

### Program manifests: declared needs, low-risk grants only

- A program declares what it needs in an `.oceans.manifest` ELF section,
  via `oceans_rt::manifest!(b"grant out\ngrant sysinfo\n")`. The section is
  non-allocated, so it is never loaded into the program's memory.
  `oceans-elf` gained bounds-checked section lookup.
- **Bare commands:** typing `ps` makes the shell run `/bin/ps`, granting
  what the manifest requests **only if every request is low-risk**: `out`
  (console *output* only, no keystrokes) and `sysinfo` (read-only).
  - A program requesting anything else is not run implicitly; the shell
    says which grant it wants and to use `run` explicitly.
  - A program without a manifest is not run as a bare command at all.
- `run PROGRAM GRANT... [-- ARGS...]` stays the way to grant anything
  explicitly.

### Program conventions (shell → child, like init → service)

- Every child gets a **handle directory** as its last handle (`<index>
  <kind> <name>` lines), so programs find capabilities by name.
- **Arguments** travel as a read-only text object, listed in the directory
  as `args`.
- `oceans_rt` now provides `Directory`, `publish_text`, `map_text`,
  `system_info` and `Out` (console output with LF → CR LF).

### Utilities (`user/utils`, shipped in `/bin` by the fs service)

| Command | Output |
|---|---|
| `uname` | `Oceans 0.1.0 x86_64 (ABI 6)` |
| `uptime` | `up 22.32 seconds` |
| `mem` | free/total MiB and frames, kernel heap in use |
| `ps` | PID, PPID, memory, state, name: the process tree |

Each declares `grant out` and `grant sysinfo`. Without `sysinfo` it says
so (`ps: needs the sysinfo capability`) and exits with code 2.

### Limits raised

- Initial handles per process: 16 → **32**. This is backward compatible,
  because receivers use the count they are given; init needed 4 fixed
  handles plus 12 boot modules.
- Grants per service in init: 8 → 12.

### Found while testing

`run ps out` seemed to hang. The traces showed tens of thousands of
248-byte filesystem reads: the user programs carried full debug info
(`/bin/ps` was 832 KB, the shell 923 KB), and smoke builds used unstripped
dev builds. Shipped user programs are now **always release builds with
`strip = "debuginfo"`** (`ps` 24 KB, shell 59 KB, fs memory 12 MB →
352 KB). The underlying cost, per-call inline reads, remains; bulk reads
through memory objects are the planned protocol addition. Also fixed: a
leaked mapping when a program load failed mid-read, and `ps` column
alignment.

## Consequences

- Least privilege by default for commands, without ceremony for the
  common, harmless case. The automatic set is deliberately tiny and is a
  constant in the shell (`AUTOMATIC_GRANTS`).
- Manifests are declarations, not proof: a program can only *receive*
  what the shell decides. Signed manifests come with the package model
  (Phase 5).
- `ps` shows names and memory, but not CPU time (no accounting yet).

## Testing

- `cargo test -p oceans-abi`: all sysinfo records round-trip. Names are
  cut at 32 bytes, and short input is rejected.
- `cargo test -p oceans-elf` (8 tests): section lookup found, missing,
  prefix-only, no section table, and out-of-bounds section.
- Smoke script additions, with each output checked:
  - `uname`;
  - `uptime` (`… seconds`);
  - `mem` (`MiB free of`);
  - `ps` (header, and `init/shell/ps` itself in the tree);
  - `run ps out` → `ps: needs the sysinfo capability`;
  - `hello-client` (no manifest) → `has no manifest; use run`.
- Passes in debug, release and with `-cpu max`. On a normal boot, `/bin`
  holds 5 programs.

## Phase 3 exit

| Phase 3 item | Delivered by |
|---|---|
| init | ADR-0016 |
| service manager | ADR-0016 |
| filesystem | ADR-0019 |
| shell | ADR-0018 |
| basic utilities | ADR-0020 |

Exit criterion: an interactive shell in QEMU (`cargo xtask run`).
