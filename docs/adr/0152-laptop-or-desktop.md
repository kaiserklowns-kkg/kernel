# ADR-0152: A laptop or a desktop, and the battery

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0020 (system information), ADR-0085 (the FADT and the
  DSDT's `\_S5`), ADR-0130 (Wi-Fi's files), ADR-0136 (status icons),
  ADR-0151 (Control Center you arrange)
- Part of Phase 10 (the desktop experience).

## Context

The user asked for two kinds of desktop: on a desktop computer, as it is;
on a laptop, the battery too, as macOS and Windows show it in the menu
bar and in Control Center.

Reading a battery's charge on a PC goes through ACPI: the battery is a
device in the firmware's AML (`PNP0C0A`) whose `_BIF`/`_BIX` and `_BST`
methods read the embedded controller. Oceans interprets no AML yet, so
this ADR does what does not need it — knowing the kind of machine and
showing the battery — and leaves the reading to the next ones.

## Decision

### The kind of machine, from ACPI (ABI 27)

- **The FADT's `Preferred_PM_Profile`** (byte 45): 1 desktop, 2 mobile
  (a laptop), 3 workstation, 8 tablet, 0 unspecified.
  `oceans_acpi::Fadt::pm_profile`.
- **The devices the AML names.** The DSDT and SSDTs, already searched for
  `\_S5` (ADR-0085), are searched for a battery (`PNP0C0A`) and a lid
  (`PNP0C0D`), as `EisaId` constants or strings
  (`oceans_acpi::declares_device`). This is a search, not an
  interpretation: a device whose `_STA` says absent still counts.
- **Portable** (`PowerRecord::portable`): the profile says mobile or
  tablet; or it says nothing and both a battery and a lid are named.
  A desktop's firmware often names a battery it does not have, so a
  battery alone is not enough.
- `SYSTEM_INFO` kind `POWER` (5) gives a `PowerRecord` (8 bytes: the
  profile, battery, lid); `oceans_rt::power_record`. The kernel logs
  `ACPI: power profile P, battery …, lid …: portable` at boot. ABI 27
  (25 and 26 are other changes' numbers).

### The battery's status, as Wi-Fi's

`/system/battery/status` holds one line, `PERCENT STATE [MINUTES]`
(`charging`, `discharging` or `full`; minutes to empty or to full):
`oceans_window::battery`. What reads the battery writes it; the desktop
reads it every few seconds, as it reads Wi-Fi's files (ADR-0130). init
learns `grant = storage:/PATH as NAME`, so the display service has both
`/system/wifi` and `/system/battery` (as `battery`).

### What the desktop shows

The battery shows on a portable machine, and wherever its status is
known:

- **The top bar**, left of the keyboard layout: the charge in percent
  and a battery icon (`oceans_draw::icon::battery`), filled to the
  charge, white; green with a bolt while charging; red at 20 % or less
  while discharging. A click opens Control Center. Until its status is
  read, the icon is empty and dimmed.
- **Control Center's Battery** (`controls::BATTERY`, four cells wide,
  after Settings by default): its charge and what it is doing ("2:30
  left", "1:05 until full", "Fully charged") and a gauge, green while
  charging, red when low. On a desktop it is neither shown nor offered.
- **Warnings.** Discharging down to 20 %, 10 % and 5 %, the desktop says
  "Battery low: N% left. Plug in the charger." once each.

A desktop computer shows none of it: the top bar and Control Center are
as before.

## Consequences

- On the HP ZBook the top bar shows the battery icon, empty, until the
  battery is read (the next ADRs): an AML interpreter for `_BST` and
  `_BIX`, and the embedded controller they read, which also gives the
  lid, the power button and `_BCM`.
- **The smoke tests** expect the kernel's line for QEMU (profile 0, no
  battery, no lid: not portable), and, with the shell writing the
  status file in the place of the battery's reader, the battery's card in
  Control Center with its gauge at 64 %. Host tests cover the id
  encoding and search, the record, the status line and the warnings.

## Alternatives considered

- **SMBIOS's chassis type.** Also says laptop or desktop, but needs
  another table found and parsed; ACPI's profile is already at hand, and
  the battery needs ACPI anyway.
- **Show the battery wherever the AML names one.** Wrong on many
  desktops.
- **Wait for the AML interpreter.** The kind of machine and the
  battery's place on the screen do not need it, and the status file
  is what the interpreter's service will write.

## Checklist (master spec §48)

- **Purpose.** A laptop shows its battery; a desktop does not.
- **Architecture.** The kernel classifies the machine from ACPI at boot;
  the display service shows the battery from its status file.
- **API.** ABI 27: `SYSTEM_INFO` kind `POWER`, `PowerRecord`;
  `oceans_window::battery`; control id `BATTERY` (12); init's
  `storage:/PATH as NAME`.
- **Dependencies.** None.
- **Security implications.** The record tells what kind of machine it is,
  as `MEMORY` tells its memory: `sysinfo`'s holders already learn more.
  The status file is in its own directory, granted only to the display
  service (and, later, to the battery's reader).
- **Testing strategy.** Host tests; the smoke tests, as above.
- **Failure behaviour.** No ACPI: not portable. A status line that does
  not parse is as no status.
