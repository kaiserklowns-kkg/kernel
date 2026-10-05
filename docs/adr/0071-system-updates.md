# ADR-0071: System updates: signed, two slots, the previous release kept

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0046 (signed packages), ADR-0035/0037/0038 (the USB
  stick at `/usb`, FAT writes, rename), ADR-0068 (the USB image)
- Part of Phase 10 (Alpha: updates and recovery).

## Context

Phase 10 asks for updates and recovery. The master spec asks for:
- signed updates (§12);
- a way back when an update fails.

Until now, a new release meant writing a new USB image, which also lost
whatever was on the stick. The boot partition is FAT. Replacing a file
there by rename is not atomic (ADR-0038): the old entry goes before the
new one is in place.

## Decision

### An update is a signed package

- **The package:** an `.opk` (ADR-0046) with:
  - the id `system.oceans`;
  - a version and a channel;
  - the kernel (`oceans-kernel`, the entry) and the boot archive
    (`initrd`).
- **Trust:** it is verified against the system's **update keys**:
  - `/bin/update.keys`, from the boot image;
  - a key that publishes apps does not, by being trusted for apps, publish
    systems.
- **No downgrades:** it must be newer than the running release,
  `/bin/release` (`VERSION CHANNEL`, written into every boot archive).
- **Nothing unverified is written.**

### Two slots on the boot partition

The hardware image's boot partition (ADR-0068) has:

```text
EFI/BOOT/BOOTX64.EFI
limine.conf                    the configuration Limine reads first
boot/limine/limine.conf        the same; read when /limine.conf is missing
boot/a/{oceans-kernel,initrd,release}
boot/b/...                     the other slot, empty until an update
```

### Applying an update

`run update out use:fs -- apply /usb/oceans-0.1.1.opk`:

1. **Verify:** the signature, the update key, the id, the files, and that
   the version is newer than the running release.
2. **Write the new release into the slot not running,** then sync it. The
   running slot is the one holding `/bin/release`'s release: someone may
   have chosen "previous" in the boot menu.
3. **Switch the boot configuration:**
   - the new slot starts first, after a 3-second menu;
   - the running release stays as the second entry, "previous".
   - Each configuration file is written beside it, synced, and renamed over
     the old one: first `boot/limine/limine.conf`, then `/limine.conf`.
   - Whatever moment power fails, Limine finds one complete configuration.
     Either `/limine.conf` is there (the old or the new), or it is missing
     and `boot/limine/limine.conf` is already the new one.

### Recovery

- **If the new release does not start,** choose "previous" in the boot
  menu.
- **`update status`** shows:
  - the running release;
  - the slot that starts first;
  - the previous one.
- The next update overwrites the slot not in use: the release that last
  started is never touched.

## Consequences

- **Updates without rewriting the stick,** signed, with a way back. The
  root filesystem (on NVMe) and the apps are untouched.
- **FAT writes in runs:** an update is about 26 MB. FAT wrote it one
  cluster per request (512 bytes on the stick's FAT32), and applying it
  took minutes in QEMU. New clusters next to each other on the disk now
  go out together, up to 64 KiB per request, and reads of clusters that
  follow each other are joined the same way (`libs/fat`, with tests). The
  crash-safe order of ADR-0037 is unchanged: the data still goes before
  the chain and the entry.
- **Room on the stick:** the USB image's partition is three times its
  files plus 48 MiB: room for the second slot and an update beside it.
- **The test:** `cargo xtask smoke-hw` applies an update and boots into it.
  - Boot 1 applies a 0.1.1 update (and refuses one signed with an
    untrusted key).
  - Boot 2 starts slot b, reads `0.1.1 alpha` in `/bin/release`, lists
    slot a as previous, and refuses to install the same release again.
- **Limits:**
  - **Going back is manual** (the boot menu). There is no automatic
    fallback after a failed boot yet: Limine has no boot counter. A
    "boot succeeded" mark checked by the next start is later work.
  - **Updates come from a file** (`/usb`, or any path). Downloading them
    (the Store's fetch, a channel feed) is later work.
  - **The kernel's version** (`uname`) is the crate's. The release is
    the image's, in `/bin/release`.
  - **QEMU's development images** (`cargo xtask run`) keep one slot; only
    the hardware image has two.
  - **Keys:** the development image trusts the development key for
    updates, as for apps. A release image must carry its own update key.
  - Core does not reserve the `system.` ids: a system package installed
    as an app would only be refused or fail to run, since it holds a
    kernel.

## Alternatives considered

- **Replacing the files in place:** a power cut halfway leaves nothing
  that boots.
- **Limine's `remember_last_entry` or a boot counter:** neither knows
  whether Oceans started well. It needs a mark Oceans writes once it is
  up, read before the menu, which Limine has no way to do.
- **Updating apps' way (through Core):** Core installs apps on the root
  filesystem. The system starts from the boot partition, before Core.

## Checklist (master spec §48)

- **Purpose:** signed system updates with a way back.
- **Architecture:**
  - the hardware image's A/B layout (`slot_layout`);
  - `/bin/release` and `/bin/update.keys`;
  - `update`.
- **API:** `update status | apply PATH`; the system package (id
  `system.oceans`, files `oceans-kernel` and `initrd`).
- **Dependencies:** none.
- **Security:**
  - update keys apart from app keys;
  - signature and trust checked before anything is read or written;
  - no downgrades;
  - the running slot is never written.
- **Testing:**
  - smoke-hw: an update applied and booted; an untrusted one and a
    reinstall refused;
  - unit: the boot configuration lists the slots in order.
- **Failure behaviour:**
  - an unverified, older or broken update: refused, nothing written;
  - a failure while writing the slot: the configuration is unchanged;
  - a power cut while switching: one complete configuration remains;
  - a new release that does not start: "previous" in the boot menu.
