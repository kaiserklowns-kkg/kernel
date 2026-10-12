# ADR-0126: Oceans installed on the internal disk (ABI 20)

- Status: Accepted
- Date: 2026-10-12
- Depends on: ADR-0068 (the hardware profile, the USB image), ADR-0071
  and ADR-0092 (updates, slots), ADR-0022 (the filesystem on disk),
  ADR-0036 and ADR-0037 (FAT)
- Part of Phase 10 (Alpha). First step of installing from the web, on a PC
  that had Windows (ADR-0127, ADR-0128).

## Context

Oceans boots from a USB stick (ADR-0068). The NVMe SSD is its root only
if it is blank, as a whole disk without a partition table. The user wants
Oceans installed from the web, replacing Windows, with no stick, as
Windows itself installs. An installed system needs:

- **its boot files on the SSD:** an EFI system partition (ESP), where the
  firmware finds the boot loader;
- **its root beside it:** a partition, not the whole disk;
- **updates written there:** not to a stick;
- **no confusion while a stick is plugged in:** a USB boot on a PC whose
  SSD holds another system (Windows, or an installed Oceans) must leave
  the SSD's ESP alone.

## Decision

### The layout

An installed disk is GPT:
1. **The EFI system partition:** FAT32, holding what the USB image's ESP
   holds:
   - Limine at the fallback path (`\EFI\BOOT\BOOTX64.EFI`);
   - `limine.conf` in both places Limine looks;
   - the slots `boot/a` and `boot/b` (ADR-0071).
2. **The Oceans root:** a partition of its own type,
   `0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093` (`oceans_gpt::OCEANS_ROOT`).
   It starts blank and the filesystem formats it at first boot.

### The kernel says where it came from (ABI 20)

- Limine's executable-file response gives the GPT disk and partition the
  kernel was loaded from.
- The `BOOT` record of `SYSTEM_INFO` grows from 8 to 40 bytes: the mode,
  the disk GUID and the partition GUID, as stored on disk. An ABI 17
  reader still sees the mode.
- `oceans_rt::boot_partition` reads it, and the kernel logs
  `boot partition: <guid>`.

### The filesystem

- **The root (`use = X as block`):**
  - a GPT disk with an Oceans root partition: that partition;
  - a disk without a partition table: the whole disk, as before;
  - a GPT disk without one: someone else's disk, left untouched, as
    before.
- **`/boot` (`use = X as boot`, with `grant = sysinfo`):**
  - It serves only the ESP whose partition GUID is the one the kernel was
    loaded from: its FAT volume, read-write, crash-safe (ADR-0037).
  - It never formats anything.
  - Started from a stick, it serves nothing, so Windows' ESP, or another
    disk's, is never written.
- **The hardware profile:**
  - The NVMe instance that served `/nvme` (a second Oceans volume on the
    SSD) becomes `nvmeboot`, the SSD's ESP, mounted at `/boot`.
  - The SSD is the root, so `/nvme` showed the same disk twice and is
    gone.
  - Recovery mode keeps `/boot`.

### Updates

`update` writes to `/boot` when the system was started from the SSD (its
`limine.conf` is there), else to the stick at `/usb`, as before.

### Images

- `oceans-gpt` has the root type.
- xtask's `gpt_image` leaves an Oceans root blank.
- `write_installed_image` lays a disk out as an installed system, with
  GUIDs of its own: never the USB image's, which `/boot` must not mistake
  for it.

## Consequences

- **`smoke-hw` has two more boots**, from the NVMe SSD laid out as
  installed, with no stick to boot from. The stick stays plugged in, as a
  source of an update.
  - **Boot 3:**
    - the kernel names its partition, and `/boot` serves that ESP;
    - the root is the Oceans partition, formatted;
    - `update` installs 0.1.1 into slot b on the SSD, and a file is
      written.
  - **Boot 4:** the update runs from the SSD, the previous release stays
    in the menu, and the file is still there.

  In boots 1 and 2, started from the stick, `/boot` serves nothing.
- **What this does not do yet:**
  - **Install:** nothing yet turns a disk with Windows into this layout.
    That is ADR-0127 (the installer inside Oceans) and ADR-0128 (the
    Windows program that starts it).
  - **SATA as the installed disk:** only the NVMe SSD, the hardware
    profile's root (ADR-0068). A machine whose system disk is SATA comes
    later.
  - **A UEFI boot entry:** Oceans boots from the fallback path. Firmware
    that boots only named entries needs one, made by the Windows program
    before it hands over (ADR-0128).
- ADR-0068's rule stands for every disk Oceans did not lay out: it never
  formats a disk holding anything.
