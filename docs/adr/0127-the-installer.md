# ADR-0127: The installer: Oceans over the disk it was started from (ABI 21)

- Status: Accepted
- Date: 2026-10-12
- Depends on: ADR-0126 (the installed layout), ADR-0122 (boot modes),
  ADR-0071 (signed system packages), ADR-0037 (FAT writes)
- Part of Phase 10 (Alpha). Installing from the web: the Oceans half.
  ADR-0128 is the Windows half.

## Context

ADR-0126 defined an installed disk. Something has to turn a PC's SSD,
which holds Windows, into one. It cannot happen while Windows runs from
that disk, so Oceans does it: started once in a mode of its own, from a
partition the Windows program (ADR-0128) prepared.

## Decision

### Install mode (ABI 21)

- `oceans.mode=install` is a fourth boot mode, `mode::INSTALL`.
- init starts `services-install.conf`, which xtask generates from the
  image's manifest. It holds:
  - the NVMe driver;
  - `installer`, with `grant = log`, `sysinfo`, `power`,
    `module:update.keys` and `use = nvme`.

  There is no shell, no desktop and no network: the user confirmed on
  Windows.

### What the installer does

1. **The disk:** the NVMe SSD whose GPT holds the partition the kernel
   was loaded from (ABI 20). Started from anywhere else, it refuses, and
   no other disk is ever written.
2. **What it reads**, all into memory first, because the staging
   partition is about to be overwritten:
   - `oceans/system.opk`, the signed release;
   - `oceans/install`: `esp = GUID`, the ESP GUID the Windows program put
     in the "Oceans" boot entry.
3. **The check:** the package must be signed by a key in the image's own
   `update.keys`, as `update` requires, and its id must be
   `system.oceans`.

   **Everything written comes from that package:**
   - the kernel;
   - the boot archive;
   - Limine (`limine.efi`, now in every system package).

   The loose files that started the installer are only used to start it.
4. **The layout** (`oceans-install`, host-tested), written in this order:
   1. a new GPT: the ESP (512 MiB, at 1 MiB, with that GUID) and the
      Oceans root (the rest, at least 1 GiB);
   2. the ESP formatted FAT32:
      - Limine at the fallback path;
      - the release in slot `a`;
      - `boot/limine/limine.conf`;
      - last, the root `limine.conf`;
   3. the root's first MiB zeroed.
5. **Done:** it says so and asks init to restart. The firmware's
   "Oceans" entry, or the fallback path, starts the installed system,
   which formats its root (ADR-0126).

### Failure

- Anything wrong before the table is written: the installer says what and
  writes nothing. The next start goes to Windows, whose boot entry is
  still there.
- A power cut while writing: neither system starts, and a USB stick is
  the way back. The table goes first and the configuration Limine reads
  first goes last, so a half-written disk never half-boots.

## Consequences

- `smoke-hw` boots twice more:
  - **Boot 5, the installer:** from a disk as Oceans Setup leaves a
    Windows PC: Windows' ESP, the Microsoft reserved partition, C:, and
    the staging partition. It checks the package and installs.
  - **Boot 6, the installed system:**
    - it starts from the ESP the boot entry names;
    - it formats its root;
    - Windows' boot manager is gone;
    - `update` sees slot `a`.

  The staging partition is written by Oceans Setup's own dry run
  (ADR-0128), not by a copy of its logic.
- Install mode needs Secure Boot off (ADR-0128). It installs on an NVMe
  SSD only, as ADR-0126 does.
