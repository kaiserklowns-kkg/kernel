# ADR-0131: Oceans Setup on Linux

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0127 (the installer), ADR-0128 (Oceans Setup on
  Windows), ADR-0126 (installed on the internal disk)
- Part of Phase 10 (Alpha).

## Context

Oceans Setup installs Oceans from the web over a Windows PC, with no USB
stick (ADR-0128). Plenty of the PCs people would give Oceans run Linux
instead, among them the first real machine Oceans is to be installed on (an
HP ZBook with Debian 12). The installer itself (ADR-0127) does not care
what the disk held: it installs from whatever FAT partition of the NVMe SSD
Oceans was started from.

## Decision

`OceansSetup-linux`, the same program built for Linux (one static
`x86_64-unknown-linux-musl` binary, linked by Rust's `rust-lld`; in the
release beside `OceansSetup.exe`). It runs in a terminal as root:

```
sudo ./OceansSetup-linux [--from DIR] [--key HEX PUBLISHER] [--no-restart]
```

1. **Checks:** root; UEFI (efivarfs); Secure Boot off (the `SecureBoot`
   variable); the EFI system partition mounted (`/boot/efi`, `/efi` or
   `/boot`, vfat, from `/proc/mounts`); its disk an NVMe SSD, GPT,
   512-byte sectors (sysfs, the GPT header); room in the ESP for the
   installer (`df`).
2. **The release:** the latest one's checksums, signature and package,
   downloaded with `curl` (else `wget`), or from a folder (`--from`);
   checked as on Windows (`release::check`).
3. **ERASE**, typed.
4. **Staging, without making a partition:** Linux's own EFI system
   partition already is a FAT partition on the SSD. The installer's files
   go into it, beside Linux's loader, at the same paths as the Windows
   program's staging partition (`EFI/oceans/BOOTX64.EFI`, `limine.conf`,
   `oceans/…`). The fallback loader `EFI/BOOT/BOOTX64.EFI` is written only
   where there is none.
5. **Boot entries** through efivarfs (`Boot####`, `BootOrder`,
   `BootNext`; `chattr -i` first, as efivarfs files are immutable):
   "Oceans Setup" (this ESP, for the next start only) and "Oceans" (the ESP
   the installer will make, its GUID chosen now, first). The encoding is
   the Windows program's (`efi`).
6. **Restart** (`systemctl reboot`), after Enter; `--no-restart` leaves it
   to the user.

The installer then works as after Windows: the partition it started from
is Linux's ESP; it reads the release from it and lays the whole SSD out.

What is parsed and built is in `linux_plan`, tested on any host; `linux`
holds the system calls.

### Trying it before a release

`cargo xtask setup-linux` writes `build/setup-linux`: the program, and a
release of the tree signed with the test key (its seed public: an install
for trying, not for keeping), with a README and the command line
(`--from . --key …`).

### Tested

`smoke-hw`'s seventh boot is a Debian PC as Setup leaves it: Debian's
ESP with its loader and the installer staged beside it (Setup's dry run,
which is shared), and a Linux root. The installer starts from that ESP,
checks the release and installs over the whole SSD. The binary runs on
Linux and refuses on a PC that did not start through UEFI, changing
nothing.

## Consequences

- **A Linux PC goes to Oceans** with one program in a terminal, and no
  stick.
- **Until the restart, nothing of Linux is lost** but the room the files
  take in the ESP. The boot entries remain (`efibootmgr -B -b ####`
  removes them).
- **The ESP must have room** for the installer (about 90 MB for the alpha);
  a small one is refused with what is free and what is needed.
- **Not run on a real PC yet:** efivarfs, real firmware's `BootNext` and a
  real ESP are exercised first on the ZBook.
- **Later:** a window for it (GTK or the web), SATA disks, installing
  beside Linux.

## Alternatives considered

- **Shrinking a Linux partition for staging, as on Windows:** ext4 cannot
  shrink while mounted, and Btrfs, XFS and LVM each differ; the ESP is
  already a FAT partition on the same disk.
- **`efibootmgr`:** not on every Linux, and it builds device paths only
  for partitions that exist; "Oceans" names the ESP the installer will
  make.
- **A shell script:** checking signatures and building device paths would
  need tools a PC may not have; the program is the Windows one's code.

## Checklist (master spec §48)

- Purpose: installing Oceans over Linux from the web.
- Architecture: the same program, a Linux front end over shared checks,
  staging and boot entries.
- API: none new; the release gains `OceansSetup-linux`.
- Dependencies: none new (the musl target and `rust-lld` to build it);
  on the PC `df`, `chattr`, `curl` or `wget`, `systemctl`.
- Security: the release checked against the release key before anything
  is written; root only; ERASE typed; Secure Boot must be off (as on
  Windows), said plainly.
- Testing: host tests of everything parsed and built; `smoke-hw` installs
  from a Debian ESP; the binary run on Linux.
- Failure: any refusal or error before step 4 changes nothing; a failure
  in step 4 or 5 leaves Linux booting (no `BootNext` yet, or one the
  firmware skips if its loader is missing).
