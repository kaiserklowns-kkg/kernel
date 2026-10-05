# ADR-0073: Alpha hardening: small BARs, reserved system ids

- Status: Accepted
- Date: 2026-10-05
- Amends: ADR-0021 (BARs smaller than a page), ADR-0045/0071 (what Core
  installs)
- Part of Phase 10 (Alpha).

## Context

Two gaps from the last milestones stand between the alpha and real
machines:

- **Small BARs.** ADR-0021 refuses BARs smaller than a page, because the
  page would be shared with other devices' registers. Some Intel chipsets
  give the AHCI controller a 2 KiB `ABAR`. On those machines the SATA
  driver (ADR-0069) could not start. This was the main risk for the first
  real machine.
- **System packages installed as apps.** ADR-0071 left open that Core
  would install a system update (`system.oceans`) as an app. It would only
  fail to run (it holds a kernel), but it should never be accepted.

## Decision

### A small BAR maps its pages when it has them to itself

- **The rule:** a memory BAR that does not fill whole pages (smaller than a
  page, or not page-aligned) is still handed out, as the whole pages around
  it, when **no other BAR of any function decodes in those pages**.
  Otherwise it is refused as before, with a log line.
- **Where the registers are:** the memory object starts at the page. The
  driver finds its registers at the BAR's offset in that page, read from
  the BAR register in configuration space.
  - The AHCI driver does so for the `ABAR`.
  - MSI-X holes are shifted by the same offset.
- **The arithmetic** is `oceans_pci::page_span`, host-tested.

### `system.` ids are not apps

- **The check:** `oceans_package::system_id` names ids whose first label is
  `system`.
  - Core refuses to install such a package: "a system update, not an app:
    apply it with `update`".
  - The `oceans` tool refuses such an id for a project.

## Consequences

- **AHCI on chipsets with a 2 KiB `ABAR`** starts, as long as nothing else
  sits in its page. Firmware gives the `ABAR` a region of its own on the
  machines we know of.
- **The isolation of ADR-0021 holds:** a driver still maps only memory that
  no other device decodes.
- **Testing limits:**
  - QEMU's AHCI has a page-sized `ABAR`, so the sub-page path is covered
    by the `page_span` tests and the shared-page check, not by a boot.
  - The first real machine with a small `ABAR` is the full test.
- **The smoke test:** `smoke-hw` checks that installing the update package
  as an app is refused.

## Alternatives considered

- **Mapping a small BAR's page whatever shares it:** a driver could reach
  another device's registers, which ADR-0021 exists to prevent.
- **A kernel-side register window (syscalls per access):** slow for a
  storage driver, and unnecessary when the page is free.
- **Reserving ids in `valid_id`:** `valid_id` is about the shape of names,
  and system packages need their id to be valid.

## Checklist (master spec §48)

- **Purpose:** SATA on more real machines; system packages never become
  apps.
- **Architecture:**
  - the kernel's `Device::bar` with `shares_pages`;
  - the AHCI driver's register offset;
  - `system_id` in `oceans-package`, used by Core and the `oceans` tool.
- **API:**
  - no ABI change (BAR objects were already page-sized);
  - `oceans_pci::page_span`;
  - `oceans_package::system_id`.
- **Dependencies:** none.
- **Security:**
  - drivers still map only pages no other device uses;
  - Core refuses system packages.
- **Testing:**
  - unit: `page_span`, `system_id`, the `oceans` tool's id check;
  - smoke-hw: installing `update.opk` as an app is refused.
- **Failure behaviour:**
  - a shared page: the BAR is refused and logged, and the driver answers
    `IoError` (ADR-0069);
  - a system package given to `app install`: refused with a message.
