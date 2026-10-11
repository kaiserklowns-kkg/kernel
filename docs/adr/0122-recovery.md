# ADR-0122: Recovery: safe mode, recovery mode, a forgotten password, a factory reset

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0071 (system updates), ADR-0074 (the log kept on disk),
  ADR-0091 and ADR-0092 (Secure Boot), ADR-0110 and ADR-0115 (passwords,
  several users), ADR-0018 (init's manifest)
- Part of Phase 10 (Alpha).

## Context

Phase 10 asks for recovery before an alpha. Until now, a system that did
not come up could only be helped by booting the previous release from
the boot menu (ADR-0071); a forgotten password locked the owner out for
good; and nothing gave a computer back as new.

**What other systems give:**
- **macOS:** Safe Mode (holding Shift) starts without login items and
  third-party extensions; macOS Recovery has a terminal, the password
  reset assistant, and Erase Mac.
- **Windows:** Safe Mode; the recovery environment opens by itself after
  two failed starts; Reset this PC.
- **Android, ChromeOS:** a recovery menu and a factory reset ("powerwash").

## Decision

### How a boot is started: ABI 17

- The kernel command line reaches userspace: `SYSTEM_INFO` kind `BOOT`
  (4) answers a `BootRecord { mode }`, read from `oceans.mode=safe` or
  `oceans.mode=recovery` (`BootRecord::from_cmdline`; `mode::NORMAL`, `SAFE`,
  `RECOVERY`). `oceans_rt::boot_mode` reads it. The kernel logs the mode.
- **The boot menu** has three more entries beside the release:
  "Oceans … (safe mode)" and "Oceans … (recovery)" with that `cmdline:`,
  then the previous release. The main entry stays first: `update` finds
  the slot in use from it. The menu waits 3 seconds (it booted at once).
  `update apply` and `update previous` write the same four entries; the
  development image has the three too.
- **Under Secure Boot** the configuration is enrolled with Limine
  (ADR-0091), so the entries are made at build time: each slot's
  configuration has the release, safe mode and recovery, with a 3-second
  menu. The slot selector's menu comes first, so under Secure Boot there
  are two menus one after the other.

### Safe mode: the usual system, its kept services off

- **Core** starts none of the services the user kept to start at boot
  (`app enable`) when the boot is in safe mode, says how many, and
  answers `MODE` (op 43, `query`): 0, 1 chosen at boot, 2 after boots that
  did not finish. The desktop shows why in a notice. Apps still open when
  the user opens them: what failed was what starts by itself.
- **After three boots in a row that did not finish, safe mode comes by
  itself.** Core counts each boot in `/system/boots` when it starts; the
  desktop says `BOOT_OK` (op 44, `query`) once it has run a minute, and a
  shutdown or restart through init counts as finished too (ADR-0086),
  however short. Either puts the count back to 0.

### Recovery mode: the disks, the network and a shell

- init reads the boot mode and, in recovery, starts
  `services-recovery.conf` instead of `services.conf`. xtask makes it from
  the image's own manifest (the hardware one for hardware images): only
  the disks (block, NVMe, SATA and their filesystems), USB with the stick
  and the keyboard, the network (virtio-net and e1000e), the filesystem,
  the kept logs and the shell; each `use` no kept service provides is
  left out, as init refuses one, and so are the test-mode checks. Core,
  the desktop, sound, the AI and the bridge do not start.
- The shell says it is recovery and what can be done there.
- **`recovery`** (`run recovery out use:fs -- …`):
  - no words: the accounts, the boots that did not finish, what can be
    done;
  - `password LOGIN NEW`: a new password for an account, Argon2id with a
    new salt as Core keeps one (ADR-0110), the account otherwise as it was;
  - `factory-reset --erase-everything`: `/system` but its logs, the
    installed apps and their data, and what is in each Home go; the next
    boot is a new computer's, the system's apps put back by Core from the
    image. Without the word it only says what it would do.
- **The previous release:** `update previous` puts the previous release
  first in the boot menu (it stays in the menu either way). The previous
  boot's log is `diag previous` (ADR-0074).

## Consequences

- A system that does not come up can be started without what was added
  to it, by choice or by itself; and repaired from a shell when even that
  fails.
- **Who can start recovery can set any password.** The disk is not
  encrypted, so whoever can choose recovery at the boot menu could read
  the files anyway; a password reset gives nothing more. That is the
  physical-access trade-off macOS without FileVault and Windows without
  BitLocker make too. With disk encryption (later), recovery would need
  the key, and this reset would go.
- **What comes by itself is safe mode, not recovery mode.** Init would
  have to read the count from the disk before it chooses what to start,
  and the disks are services init starts. So three unfinished boots bring
  safe mode, which helps when an added service is at fault. A system that
  fails before Core, or in safe mode too, needs the user to choose
  recovery or the previous release in the boot menu.
- **Under Secure Boot `update previous` is refused:** the selector's
  enrolled configuration chooses the slot (ADR-0092). The previous slot
  is still in the selector's menu.
- **The smoke test** has four more boots on the disk the first two left:
  recovery (the status, a refused short password and unknown account, a
  password set, three boots marked unfinished, the previous log, the
  network), the usual system in safe mode by itself and opened with the
  new password, recovery again for a factory reset, and the new
  computer's first boot.
