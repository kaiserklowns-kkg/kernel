# ADR-0092: Updates under Secure Boot: a slot per partition, switched in the partition table

- Status: Accepted
- Date: 2026-10-08
- Depends on: ADR-0071 (system updates, two slots), ADR-0091 (Secure
  Boot), ADR-0036 and ADR-0037 (FAT), ADR-0068 (the USB image)
- Part of Phase 10 (Alpha: updates and security).

## Context

ADR-0091 made a stick that boots with Secure Boot, but `update` refused
to touch it. Its boot configuration is pinned by a hash enrolled in the
signed Limine, and the device cannot sign: the key stays with the
release.

ADR-0071's way, two slots and a configuration rewritten to start the new
one first, does not fit:
- **The configuration lists both releases by hash.** The build host knows
  the new release's hashes, not the ones of whatever release the stick
  holds now.
- **Limine finds its configuration at fixed paths on its partition.**
  Two Limines on one partition read the same file.
- **Something must still choose the first release.** That choice cannot
  live in anything hash-pinned or signed, or the device could not make
  it.

## Decision

### The stick: a selector and two slot partitions

```text
GPT entry 1  EFI system partition (FAT32, "OCEANS", /usb)
               EFI/BOOT/BOOTX64.EFI   the selector: Limine, signed
               limine.conf            its configuration, never changed
               boot/secure-boot, oceans-secure-boot.cer, room for updates
GPT entry 2  Oceans slot (FAT32, "OCEANS-A" or "-B"): the release first
               EFI/BOOT/BOOTX64.EFI   this release's Limine, signed
               limine.conf            its kernel and boot archive, by hash
               boot/oceans-kernel, boot/initrd, release
GPT entry 3  Oceans slot: the previous release, or empty
```

- **The selector's configuration** is the same on every stick, for good:
  - "Oceans": `protocol: efi`, `path: boot(2):/EFI/BOOT/BOOTX64.EFI`;
  - "Oceans (previous)": the same at `boot(3)`;
  - a 3-second menu.
  It names partitions, never files by hash, so its enrolled hash never
  changes.
- **Each slot's Limine is checked by the firmware** when the selector
  starts it (`protocol: efi` goes through the firmware's `LoadImage`).
  It then reads its own partition's configuration (`boot()` is the
  partition it started from). That configuration starts the slot's
  kernel at once (`timeout: 0`), checking the kernel and boot archive
  against their hashes.
- **The slot partitions have a type of their own**, "Oceans slot"
  (`0e5ea0b5-…`), so other systems do not mount them. The system's `/usb`
  is still the first FAT partition, the EFI system partition.

### Updates carry their slot's boot files

- An update package for Secure Boot systems also holds `BOOTX64.EFI` (the
  slot's Limine, enrolled and signed with the Secure Boot key) and
  `limine.conf`. `cargo xtask release` adds them when
  `OCEANS_SECURE_BOOT_KEY` is set: one package for every system.
- **Nothing that would not boot is written.** Before writing, `update`
  checks what Limine will check:
  - the configuration names this kernel and this boot archive by BLAKE2B;
  - Limine carries the configuration's hash.
  A package without these files is refused on a Secure Boot stick.
- The package's own signature and update key are checked first, as in
  ADR-0071. The Authenticode signature is the firmware's to check.

### Applying: write the other partition, then swap two entries

`run update out use:fs use:usbdisk -- apply /usb/oceans-X.opk`. `update`
reaches the stick itself through `use:usbdisk`.
1. **Repair the table:** finish or undo a switch a power cut interrupted.
2. **Find the running slot:** the one whose `release` matches
   `/bin/release`. If it is the previous one (chosen in the menu), the
   entries are swapped first, so the running release is never written.
3. **Write the slot at entry 3 from nothing:**
   - format FAT32 again, keeping its label;
   - write Limine, the configuration, the kernel and the boot archive,
     then sync;
   - write `release` last, then sync again.
4. **Swap GPT entries 2 and 3** (`oceans-gpt`). Each write is followed by
   a barrier, in this order:
   1. the backup copy's entries;
   2. the backup copy's header;
   3. the primary copy's entries: **the switch**, a single sector;
   4. the primary copy's header.

   Before write 3, every reader sees the old order. From write 3 on,
   every reader sees the new one:
   - readers that check CRCs find the primary damaged and use the backup,
     which is already new (edk2 then restores the primary from it);
   - readers that do not check read the new entries.

   The test cuts the power after every write.

`update status` names the running slot, the one that starts first and
the previous one.

### Images made in Rust

The images now come from Rust: three partitions, and a slot partition
made again on the device.
- **`oceans-fat` makes FAT32 volumes** (`format`, as `mkfs.fat -F 32`
  does).
- **`Window`** makes one partition of a disk a disk of its own.
- **`xtask` builds every image itself:**
  - the plain USB image;
  - the Secure Boot image;
  - the QEMU ESP image;
  - files copied onto a stick image.

  dosfstools and mtools are no longer needed. CI still runs `fsck.fat`
  on volumes the tests make.

## Consequences

- **A Secure Boot stick updates in place, signed, with a way back.** The
  previous release stays in the selector's menu. A power cut at any
  moment leaves a stick that boots one complete, signed release.
- **Firmware may start a slot directly.** If the selector is refused
  (unsigned or damaged), OVMF tries the stick's other partitions and
  starts a slot's own Limine. That Limine is signed and starts only its
  own hashed release, so the chain stays checked; only the menu is lost.
- **The plain image is unchanged** (ADR-0071's slots on one partition).
- **Building no longer needs dosfstools or mtools,** WSL included. The
  local hardware smoke runs on Windows again.
- **The test:** `cargo xtask smoke-secure-boot` boots six times on OVMF's
  Secure Boot firmware:
  1. enrol the certificate;
  2. boot slot A; `update status`; a package without boot files refused;
     a signed 0.1.1 installed into slot B;
  3. 0.1.1 starts from slot B, slot A is the previous;
  4. an unsigned selector is refused (and its menu never appears);
  5. an unsigned Limine in the first slot is refused when the selector
     starts it;
  6. a changed kernel in the first slot is refused by its Limine.
- **Limits:**
  - **Slot size:** each slot holds twice the first release plus 32 MiB
    (at least 64 MiB). A release that outgrows it needs a new image.
  - **Two Limine menus are not shown:** the selector's has the choice;
    the slot's starts at once. A release's own name appears only in the
    log and in `update status`.
  - **Going back is still manual** (ADR-0071).
  - **`update` needs `use:usbdisk`** on a Secure Boot stick. The shell
    holds it, and apps do not.

## Alternatives considered

- **A configuration naming both releases by hash, made by the host:** the
  host does not know which release the stick holds; one signed Limine per
  pair of releases does not scale.
- **Choosing by firmware boot entries (`BootOrder`):** needs UEFI runtime
  services in the kernel, and removable media boots without entries.
- **One EFI system partition per slot, switched by partition type:** edk2
  boots the first file system that has `EFI/BOOT/BOOTX64.EFI`, whatever
  its type, and other firmware differs.
- **Volume labels as the switch (`fslabel()`):** two partitions change,
  so it takes two writes; a cut between them leaves two slots with one
  name.
- **Renaming files on one partition:** FAT renames are not atomic
  (ADR-0038). Limine also reads one configuration per partition.

## Checklist (master spec §48)

- **Purpose:** signed, in-place updates with a way back on Secure Boot
  sticks.
- **Architecture:**
  - `oceans-gpt` (reading, making, repairing, swapping);
  - `oceans-fat::format` and `Window`;
  - `xtask::disk_image`;
  - `xtask::secure_boot` (the selector, slot boot files, the image, the
    test);
  - `update`'s `secure.rs`.
- **API:**
  - `update status | apply` with `use:usbdisk` on a Secure Boot stick;
  - update packages may carry `BOOTX64.EFI` and `limine.conf`;
  - GPT type "Oceans slot".
- **Dependencies:** `blake2` 0.10 in `update` (no_std; already used by
  xtask).
- **Security:**
  - every Limine signed, every configuration enrolled, every kernel and
    boot archive hashed;
  - the switch is outside the signed chain but only chooses between two
    signed releases, as the menu does;
  - the running release is never written;
  - an update that would not boot is refused before anything is written.
- **Testing:**
  - unit: GPT creation, swap, every power cut during a swap, repair of a
    damaged copy;
  - FAT32 formatting checked by our reader and by `fsck.fat` and mtools
    where installed;
  - windows; the xtask image builder;
  - firmware: the six boots of `smoke-secure-boot`.
- **Failure behaviour:**
  - a power cut while writing a slot leaves the old order;
  - a power cut while switching leaves one order, which the next
    `update` finishes or undoes;
  - an unsigned or changed Limine or kernel does not start.
