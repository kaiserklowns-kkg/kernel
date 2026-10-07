# Hardware compatibility

What Oceans runs on, and how well we know it (ADR-0005, ADR-0068). The
tiers are defined in [support-tiers.md](support-tiers.md).

The device rows below are checked against what the system itself reports
(`libs/hardware`, the table `sysreport` uses), so this page and the system
cannot disagree.

## The Tier 1 list

A machine is a **Tier 1 candidate** when it has:

| Requirement | Why |
|---|---|
| x86-64 CPU meeting **x86-64-v2** (SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, CMPXCHG16B, LAHF in long mode), with **NX** and an **APIC** | the kernel's assumptions (ADR-0005) |
| **UEFI 2.x** firmware with **ACPI** (RSDP, MADT, MCFG) and a **GOP** framebuffer of 32 bits per pixel | boot (Limine), interrupts, PCIe, the screen |
| At least **256 MiB** of RAM | the system and its services |
| An **NVMe SSD**, a **SATA disk (AHCI)**, or a USB stick | storage: the root filesystem on NVMe, a SATA disk at `/sata` |
| A **USB 3 (xHCI)** controller, or a PS/2 keyboard | input |
| An **Intel 82574L** network adapter (for networking) | the one Ethernet family driven today |

It becomes **Tier 1 (validated)** when a `sysreport` from it shows the
baseline met and every device it needs driven, and the validation
procedure below passes on it.

## Devices

| Device | Oceans | Validation |
|---|---|---|
| NVMe SSD (any vendor) | nvme (ADR-0040) | Tier 0 (QEMU `nvme`) in CI; real SSDs: awaiting reports |
| SATA AHCI controller (any vendor) | ahci (ADR-0069) | Tier 0 (QEMU q35's ICH9 AHCI with `ide-hd` disks) in CI; real controllers and disks: awaiting reports |
| Intel High Definition Audio controller (any vendor) | hda (ADR-0079, ADR-0087): output and input, 48 kHz stereo | Tier 0 (QEMU `intel-hda` with `hda-output`, recorded and checked) in CI; real codecs: awaiting reports |
| USB 3 xHCI controller (any vendor) | xhci (ADR-0032): keyboards, mice, tablets, hubs, mass storage | Tier 0 (QEMU `qemu-xhci`) in CI; real controllers: awaiting reports |
| Intel 82574L Ethernet | e1000e (ADR-0041) | Tier 0 (QEMU `e1000e`) in CI; real adapters: awaiting reports |
| virtio-net (modern) | virtio-net (ADR-0023) | Tier 0 (virtual machines) |
| virtio-blk (modern) | virtio-blk (ADR-0021) | Tier 0 (virtual machines) |
| Display controller | UEFI GOP framebuffer, 32 bpp (ADR-0029, ADR-0057) | Tier 0 (OVMF); no GPU driver |
| Host bridge, ISA/LPC bridge, PCI bridge | platform | — |

Also driven, outside PCI: the 16550 UART (COM1), the PS/2 keyboard
(i8042), and USB HID boot keyboards, mice and tablets on xHCI.

**Not driven yet (Tier 1 candidates):**
- Realtek Ethernet;
- other Intel Ethernet (I219, I225, I226);
- Wi-Fi;
- audio input (microphones) and HDMI audio;
- GPUs;
- USB 2-only controllers.

**Not supported (Tier 3):** legacy BIOS boot, 32-bit CPUs, ISA, IDE/PATA,
floppy.

## Machines

| Machine | CPU | Result | Report |
|---|---|---|---|
| QEMU `q35` + OVMF (Tier 0) | any x86-64-v2 model (`-cpu max` too) | validated in CI: `cargo xtask smoke` and `cargo xtask smoke-hw` | — |

Real machines are added here from their reports.

## Validating a machine

1. **Build the USB image:**

   ```bash
   cargo xtask usb
   ```

   This writes `build/oceans-usb.img` (GPT, one EFI system partition). It
   is the hardware profile: the root filesystem goes on the NVMe SSD, and
   the first SATA disk (AHCI mode) is shown as `/sata`.
2. **Write the image to a USB stick** (Rufus, `dd`, balenaEtcher).
3. **Boot the machine from the stick** (UEFI; SATA in AHCI mode, not RAID
   or legacy IDE). Secure Boot off, or on with an image signed for it
   once its certificate (`oceans-secure-boot.cer`, at the stick's root)
   is enrolled in the firmware's `db` (ADR-0091).
4. **What happens to the NVMe SSD and the SATA disk:** Oceans formats a
   disk **only if its start is blank**. A disk holding anything (a
   partition table, Windows, Linux) is left untouched, and files stay in
   memory. To keep files across reboots, use a blank disk.
5. **In the shell**, run:

   ```text
   run sysreport out devices sysinfo use:net use:usb
   ```

   It reports the CPU against the baseline, memory, every PCI device with
   what Oceans gives it, USB devices and the network.
6. **Check:**
   - the desktop (mouse, keyboard);
   - `ls /`;
   - `run ifconfig out use:net`;
   - `run ping out use:net -- 1.1.1.1 3`;
   - `app list`.
7. **Send the report and results** to be added to the table above.

## Updating the stick

The stick keeps two releases (ADR-0071). Copy an update (`.opk`) to the
stick, then, from the shell:

```text
run update out use:fs -- apply /usb/oceans-VERSION.opk
run update out use:fs -- status
```

The update is checked against the update keys of the release that is
running, and it starts at the next boot. If it does not start, choose the
"(previous)" entry in the boot menu. `run diag out logs sysinfo use:fs --
save /usb/diag.txt` saves what went wrong, to attach to a report (ADR-0070).
After a hang or a reset, `run diag out logs use:fs -- previous` shows the
boot before it, and `diag save` includes it (ADR-0074).
