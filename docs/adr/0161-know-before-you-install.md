# ADR-0161: Know before you install: Oceans Setup judges the hardware first

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0068 (hardware validation and `libs/hardware`), ADR-0128
  (Oceans Setup on Windows), ADR-0131 (on Linux), ADR-0146 (the promises)
- Part of Phase 10 (Alpha). No ABI change; nothing on the device changes.
- Plan: WP-3 of [development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md).

## Context

Promise P8 (ADR-0146): know before you install.

Linux's hardest lesson is that its desktop is fine, and the wrong laptop
is not. Someone installs it, and discovers on the first day that the Wi-Fi,
the sleep or the disk does not work. By then the old system is gone.

Oceans supports a deliberately small set of hardware (ADR-0005), so the
risk is larger. Oceans Setup checked UEFI, Secure Boot and the disk
(ADR-0128, ADR-0131), but not whether Oceans has drivers for the machine.
`sysreport` could say so, but only after Oceans was running.

## Decision

### `oceans_hardware::assess` (host-tested)

The same table `sysreport` and `compatibility.md` use (ADR-0068), so the
three cannot disagree.

- **Each PCI function, by what it is for.** Its class gives storage,
  wired network, Wi-Fi (a "network, other" or wireless class), display,
  sound, USB, platform (bridges, SMBus) or other.
- **Its verdict:**
  - **works** (a driver, named with its ADR);
  - **works through the firmware** (the display: no driver of its own);
  - **platform** (nothing to drive; not listed);
  - **not supported**.
- **The machine:**
  - whether a storage controller works, and whether wired or Wi-Fi
    networking works;
  - how many noticeable parts do not work.
- **The rules:**
  - Setup **refuses** only when no storage controller works: Oceans would
    have nowhere to start from;
  - it **warns** when no network works (no internet, and updates only
    from a stick), when only Wi-Fi works (Oceans' Wi-Fi is new,
    ADR-0129), and when some parts will not work.

### Oceans Setup asks the system it runs on

- **Windows** (PnP, no driver needed): every present `PCI\…` device's
  compatible IDs give the vendor, the device and the class code (`CC_`
  with six digits). This is a PowerShell line, `WINDOWS_PCI_SCRIPT`.
- **Linux:** `/sys/bus/pci/devices/*/{vendor,device,class}`.
- **The report:**
  - a line per noticeable function ("works storage: NVMe SSD (any vendor)
    (144d:a802), nvme (ADR-0040)"; "not supported other (10ec:522a, class
    ff0000)"), then the verdict and the warnings;
  - shown after the disk checks and before the download, so long before
    ERASE;
  - saved as `oceans-hardware.txt` beside Oceans Setup (Windows) or in
    the current folder (Linux), for the user to keep or to send.
- **A refusal ends Setup, as a disk refusal does:** nothing has been
  changed.

## What the user sees change

Oceans Setup lists the PC's hardware, with what works and what will not,
before it downloads anything. It refuses a PC whose disk Oceans cannot
drive.

## Consequences

- Nobody erases Windows or Linux for a machine Oceans cannot start on.
- The report is a ready-made hardware report for the compatibility list.
- USB devices are not judged: they are not on the PCI bus, and Oceans'
  USB classes (keyboards, mice, storage) are generic.
- Since the table is the system's own, a driver added to Oceans is
  reflected in Setup with the next release of Setup.

## Alternatives considered

- **Refuse whenever the network does not work.** It lost: a desktop with
  an unsupported network card is still a working Oceans with a USB stick,
  and the user is told plainly.
- **Ask Windows for driver or device names.** It lost: the names are
  localized and vendor-written, while the IDs and the class are exact and
  match the table.
- **Judge on the device after installing** (`sysreport`). It lost because
  that is too late: the point is to know before anything is erased.

## Checklist (master spec §48)

- **Purpose:** know before you install.
- **Architecture:**
  - `oceans_hardware::assess`;
  - Setup's `hardware` module (the report, and the Windows and Linux
    parsers);
  - the check step on each system.
- **API:** none on the device. `assess::{Function, Kind, Verdict,
  Assessment, assess, verdict, kind, split_class}`.
- **Dependencies:** `oceans-hardware` in Setup (already in the tree).
- **Security:**
  - read-only enumeration, with nothing sent anywhere;
  - the file is written beside Setup.
- **Testing:**
  - `assess`: the ZBook (its real IDs) needs nothing, QEMU q35 is the
    reference, a laptop with RAID storage and Broadcom Wi-Fi is refused
    and told why, and Wi-Fi alone is a warning;
  - Setup: Windows' compatible IDs and Linux's sysfs files parsed, and the
    report's words and verdict;
  - clippy for Windows and Linux (musl) builds.
- **Failure behaviour:** if Windows lists nothing (PowerShell fails), the
  report says no devices were listed and Setup refuses, as it cannot know
  the storage. That is the safe side: it never erases blind.
- **Promises touched:** P8 (kept), P7 (Setup gains a step).
