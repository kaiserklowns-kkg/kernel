# ADR-0128: Oceans Setup: installing from the web over Windows

- Status: Accepted
- Date: 2026-10-12
- Depends on: ADR-0127 (the installer), ADR-0126 (the installed layout),
  ADR-0075 (signed release checksums)
- Part of Phase 10 (Alpha).

## Context

The user wants Oceans installed as Windows installs itself: downloaded
from the web and run on the PC, with no USB stick, after which the PC runs
Oceans instead of Windows. The user chose to replace Windows, not to sit
beside it.

Constraints:
- **Windows cannot erase the disk it runs from.** So the work is split:
  Windows prepares, Oceans' installer (ADR-0127) erases and installs.
- **Secure Boot:** PCs sold with Windows trust Microsoft's keys. Oceans is
  not signed by Microsoft (that takes Microsoft's review of a shim), so
  Secure Boot must be off.
- **Unsigned programs:** without a code-signing certificate (bought from a
  CA), Windows' SmartScreen warns about an unknown publisher.

## Decision

### `OceansSetup.exe`

A Windows program in Rust (`tools/setup`). It is a small window: what it
will do, a log, one button per step.

1. **Check this PC:**
   - it is UEFI;
   - Secure Boot is off (if on, it says how to turn it off);
   - Windows is on an NVMe SSD, GPT, with 512-byte sectors, big enough;
   - C: can give up 1 GiB.

   It asks for administrator rights, as installers do.
2. **Download:**
   - It fetches from the latest GitHub release only `SHA256SUMS`, its
     signature and the system package.
   - It checks the signature with the Oceans release key, built in from
     `tools/keys/oceans-release.pub`.
   - Then the package against its checksum, and the package's own
     signature.
3. **Confirm:** the user types ERASE. Everything on the SSD goes: Windows,
   files, apps.
4. **Prepare and restart:**
   - **Disk:** shrinks C: by 1 GiB (PowerShell's Storage module) and makes
     a FAT32 partition, "Oceans Setup", holding:
     - Limine;
     - a `limine.conf` that starts the installer;
     - the kernel and boot archive from the package;
     - the package itself;
     - `oceans/install`.
     It then hides the partition again.
   - **Boot entries:** through the firmware's variables:
     - "Oceans": the future ESP, its GUID chosen now; first in
       `BootOrder`;
     - "Oceans Setup": the staging partition, as `BootNext`, for the next
       start only.
   - **Restart.**

**Until the restart nothing is lost:** cancel, and Windows starts as
before. The "Oceans" entry points to a partition that does not exist yet,
so the firmware skips it. The small partition can be deleted.

### Tested without a Windows PC

`OceansSetup --dry-run OUT --from DIR [--key HEX PUBLISHER] [--esp GUID]`
runs
steps 2 and 4's staging on any system, writing the staging files into a
folder:
- `smoke-hw` uses it to make the staging partition of its Windows disk
  (ADR-0127);
- the host tests cover the boot entries' encoding (load options, device
  paths) and the PowerShell scripts and their answers.

### The release

`cargo xtask release` builds `OceansSetup.exe` for `x86_64-pc-windows-gnu`
and lists it in the signed `SHA256SUMS`. The "web page" is the GitHub
release, where it sits beside the USB image.

## Consequences

- **A PC goes from Windows to Oceans by running one program, with no
  stick:** the user turns Secure Boot off, runs Oceans Setup, types
  ERASE, and waits for a restart.
- **Not tested on a real PC yet.** The firmware variables, PowerShell's
  shrinking of C: and a real firmware's handling of `BootNext` are
  exercised only on Windows. Its first run should be on a spare machine.
- **Later:**
  - signing the program (SmartScreen);
  - a Microsoft-signed shim (Secure Boot on);
  - installing beside Windows;
  - SATA system disks;
  - a progress bar while downloading.
