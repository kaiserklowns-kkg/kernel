# ADR-0025: The boot archive (initrd)

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0016 (init), ADR-0020
- Changes: init's start contract (handle 1)

## Context

Every program was its own boot module, and init received each one as a
handle. That ties the number of programs to init's 32 initial handles
(27 once the 5 fixed ones are counted). ADR-0023 already ran into it: the
kernel's module limit silently dropped `ipc-test`. The system will keep
gaining programs, so the limit must go, not move.

## Decision

### Format (`libs/archive`, `oceans-archive`)

- **Layout:**
  - a header: magic `OCEANSAR`, version, file count;
  - a table of 64-byte entries: name (≤ 40 bytes), offset, size,
    **CRC-32C**;
  - the files, 16-byte aligned.
- **The reader** validates everything before handing out a byte:
  - the table lies inside the archive;
  - every file lies inside it and not over the table, with no overflow;
  - names are valid and unique;
  - every checksum matches.

  It does not allocate, so both the kernel and init use it.
- **The writer** is the same crate, used by xtask.

### Boot

- Limine loads the kernel and **one module, `initrd`**. Configuration is
  in the archive too (`services.conf`).
- The kernel validates the archive and starts the `init` it contains. A
  damaged archive is rejected with a log, and no userspace starts: the
  boot never runs unverified code.
- **init's start contract** (handle 1 changes):

  | Handle | Capability |
  |---|---|
  | 0 | log |
  | 1 | **the boot archive**, as a read-only memory object (was: the module table) |
  | 2 | console |
  | 3 | sysinfo |
  | 4 | device bus |

  Modules no longer follow as extra handles.
- **init** maps the archive for its lifetime, validates it again (it does
  not rely on the kernel), reads `services.conf` straight from it, and
  unpacks a program into a read-only memory object **only when a service
  needs it**, once (`image =`, `grant = module:NAME`).
- The kernel keeps at most 8 boot modules again (it needs one).

## Consequences

- The number of programs is bounded by the archive (256 files), not by
  handles.
- One file to build, ship, and later sign and update: package and update
  work (Phase 5) can build on it.
- The kernel copies the archive once into the memory object it gives init
  (763 KiB today).

## Alternatives considered

- **cpio or tar.** These are standard, but have more format than we need
  and no integrity check.
- **The kernel unpacks every file into its own object.** That keeps the
  handle limit, which is the problem.

## Testing

- `cargo test -p oceans-archive` (3 tests):
  - round trip, including an empty file and an empty archive;
  - the writer refuses duplicate or invalid names and wrong sizes;
  - the reader refuses bad magic and version, a flipped data byte
    (checksum), truncation, an absurd file count, a file over the table,
    offset overflow and duplicate names;
  - every single-byte mutation either fails cleanly or yields a usable
    archive.
- Smoke (all existing steps run from the archive): the kernel logs `init
  started from a boot archive of 22 files (763 KiB)`, the kernel's own
  `ipc-test` self-test comes from the archive, and fs publishes 12
  programs in `/bin`.
- Passes in debug, release and with `-cpu max`.
