# ADR-0068: Hardware validation and the Tier 1 list

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0005 (hardware tiers), ADR-0022 (the filesystem on
  disk), ADR-0040 (NVMe), ADR-0041 (Intel Ethernet), ADR-0032 to ADR-0035
  (USB)
- Starts Phase 9.

## Context

Phase 9 asks for testing on Tier 1 hardware and a hardware compatibility
matrix (master spec §47). Its exit criterion is a published Tier 1 list.

Until now Oceans ran only in QEMU with virtio devices. A real PC has no
virtio: its system disk is NVMe or SATA, its network an Intel or Realtek
chip, its input USB. There was:
- no image to boot a real machine from;
- no profile that put the root filesystem on real storage;
- no way for a machine to say what Oceans found on it.

## Decision

### The hardware profile

A real machine runs the services of `config/services.conf`, with **the
root filesystem on the NVMe SSD** (`use = nvme as block`); the virtio-blk
service and the separate `/nvme` mount are dropped.
- **Generated, never a copy:** `cargo xtask` builds it from
  `services.conf`, and every change must find exactly what it changes, or
  the build stops.
- **The user's SSD is safe:** the filesystem formats a disk only if its
  start is blank (ADR-0022). An SSD holding anything (a partition table,
  Windows, Linux) is left untouched, and files stay in memory.

### The USB image

`cargo xtask usb` writes `build/oceans-usb.img`, to be written to a USB
stick:
- a GPT disk (protective MBR, primary and backup headers and entries,
  CRC-32 checked);
- one EFI system partition, FAT32 (mkfs.fat and mtools), holding Limine,
  the kernel and the boot archive of the hardware profile;
- GUIDs derived from the kernel, so a build is reproducible.

On the machine, the stick also appears as removable media (`/usb`).

### `sysreport`

A program for compatibility reports:

```text
run sysreport out devices sysinfo use:net use:usb
```

It reports:
- the system: kernel version and ABI;
- the CPU (CPUID: vendor, brand, family/model/stepping) against **the
  Tier 1 baseline**: x86-64-v2 (SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT,
  CMPXCHG16B, LAHF), NX, an APIC; x2APIC is noted;
- memory;
- every PCI function with what Oceans gives it (a driver, attached or
  not; the firmware; the platform; or **NO DRIVER**);
- USB devices and the network;
- a summary: baseline met or not, and how many devices have no driver.

The judgements live in **`libs/hardware`** (host-tested): the baseline and
the device table. A test checks that the published matrix states every row
of that table, so the system and the documentation cannot disagree.

### The Tier 1 list and the matrix

`docs/hardware/compatibility.md` publishes:
- **the Tier 1 list:** the platform requirements (x86-64-v2 with NX and an
  APIC, UEFI 2.x with ACPI and a 32-bpp GOP framebuffer, 256 MiB, NVMe or
  USB storage, xHCI or PS/2 input, Intel 82574L for networking);
- **the device matrix,** with how each row is validated;
- **the candidates** not yet driven: Realtek, other Intel NICs, Wi-Fi,
  AHCI, HDA audio, GPUs;
- **Tier 3;**
- **the machines table** (QEMU Tier 0 validated in CI; real machines from
  their reports);
- **the validation procedure:** build the image, write the stick, boot,
  `sysreport`, the checks, send the report.

### A real PC, in CI

`cargo xtask smoke-hw` boots the USB image in QEMU as a real PC: q35,
`-cpu max` (a Tier 1 CPU), **no virtio at all**, an NVMe SSD, an Intel
82574L, xHCI with a keyboard, and the stick on USB as the only boot
device.

- **Boot 1:** a blank SSD is formatted as the root; the shell, Core, the
  stick as `/usb` and DHCP over e1000e work; `sysreport` shows the baseline
  met and NVMe, xHCI and e1000e driven.
- **Boot 2:** a file written in boot 1 is still on the SSD.

It runs in CI after the regular smoke test.

## Consequences

- **The Tier 1 list is published**, which is Phase 9's exit criterion, and
  the image and tools to validate machines exist.
- **What it does not include:** validation on real machines. That needs
  machines and their reports (`sysreport` plus the procedure). The machines
  table grows from them, and a candidate becomes Tier 1 only then.
- **Gaps** that keep common PCs out of Tier 1 until their drivers exist:
  - Realtek Ethernet;
  - Intel I219, I225 and I226;
  - AHCI, for machines whose system disk is SATA.

  These are the next drivers.

## Alternatives considered

- **Shipping an installer that partitions the SSD:** this risks the user's
  data before Oceans is validated on that machine. Blank-only formatting,
  with memory otherwise, is safe for a live stick.
- **A hand-kept hardware services file:** it would drift from
  `services.conf`.
- **An image via a tool such as `sgdisk`:** another host dependency, for a
  format small enough to write and test here.

## Checklist (master spec §48)

- **Purpose:** hardware validation; the Tier 1 list.
- **Architecture:**
  - the hardware profile (generated);
  - the USB image (GPT + ESP);
  - `sysreport` on `libs/hardware`;
  - the compatibility document;
  - the hardware smoke.
- **API:**
  - `cargo xtask usb`, `cargo xtask smoke-hw`;
  - `sysreport`;
  - `oceans_hardware::{Cpuid, MATRIX, lookup, support}`.
- **Dependencies:** none new (xtask uses `sha2`, already present).
- **Security:**
  - a disk is formatted only if blank;
  - the image is reproducible;
  - `sysreport` only reads (the device list, sysinfo, the network
    service's status, the USB list);
  - Secure Boot is off for now: signing the boot chain is later work.
- **Testing:**
  - unit: the baseline, CPU signatures, the device table, the published
    matrix, GPT (headers, CRCs, backup), CRC-32, the hardware profile;
  - smoke: `smoke-hw` (above), in CI.
- **Failure behaviour:**
  - no FAT tools: `usb` and `smoke-hw` say how to get them;
  - a non-blank SSD: files in memory, said in the log;
  - devices without drivers: listed as such.
