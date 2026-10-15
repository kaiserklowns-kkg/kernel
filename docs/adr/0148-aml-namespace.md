# ADR-0148: Reading the firmware's AML into a namespace

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0017 and ADR-0085 (ACPI tables in the kernel),
  ADR-0021 (the device bus), ADR-0016 (init's grants)
- Part of Phase 10 (Alpha). ABI 26.

## Context

On a laptop, the battery, the AC adapter, the lid, the power button and
the portable way to set the brightness are all behind ACPI's AML. This is
the code the firmware ships in its DSDT and SSDTs, which the operating
system runs. Oceans read AML only to find `\_S5`.

No Rust AML interpreter builds on the stable compiler. The `acpi` crate
(6.x) needs `allocator_api`, and `aml` (0.16) needs `decl_macro`, both
nightly-only. ADR-0001 rules out copying their source in. So Oceans has
its own, in steps:

1. read the definition blocks into a namespace (this ADR);
2. run methods: fields, operation regions, the embedded controller;
3. the battery, the AC adapter, the lid and the buttons, with their
   events.

## Decision

- **`oceans-aml`** (host-tested, no I/O):
  - **The reader** knows every opcode's encoding: package lengths, names
    (`\`, `^`, dual and multi segments, the null name), data (integers,
    strings, buffers, packages), locals and arguments, and the operands
    of every Type 1 and Type 2 opcode. Expressions become an AST that the
    next step will run.
  - **Loading** reads each block's objects at the level they are defined:
    `Scope`, `Device`, `Processor`, `PowerResource`, `ThermalZone`,
    `Method`, `Name`, `Alias`, `External`, `Mutex`, `Event`,
    `OperationRegion`, `DataTableRegion`, and `Field`, `IndexField` and
    `BankField` with each unit's bit offset, length and access.
  - **Methods** keep their bodies, to be read when they run. AML does not
    mark calls, so a call's arguments can be told apart only once every
    block has defined its methods.
  - **Code run at load time** (`If`, `Store` and the like outside
    methods) is read but not run. Both arms of an `If` define their
    objects, and the first definition of a name is kept.
  - **Lookups** follow the namespace's search rules (a lone segment is
    looked for up the scopes) and aliases. A device's constant `_HID`
    reads as text (EISA IDs decoded).
  - **Untrusted input:** every length must fit its container, names must
    be valid, and nesting is bounded (64). Anything wrong is an error,
    never a panic. A test cuts and mutates a table at every byte.
- **`FIRMWARE_TABLE` (system call 52, ABI 26):**
  - It copies definition block `index` (0 the DSDT, then the SSDTs) whole,
    so the kernel keeps their addresses from discovery.
  - It needs `WRITE` on the device bus, which init holds and gives on
    request (`grant = firmware`).
- **The `acpi` service** (`grant = log`, `grant = firmware`, run once):
  - It checks each block's signature, length and checksum (a bad checksum
    is only reported, as firmware often has one) and loads it.
  - It logs the totals, any block not read and why, and the devices the
    next steps need: battery, AC adapter, lid, power and sleep buttons,
    embedded controller, light sensor.
  - AML runs in a service, never in the kernel: firmware's code is
    untrusted.

## Consequences

- QEMU's DSDT and SSDTs are read whole in every smoke test
  (`definition block(s) read, 0 not`). The ZBook's next update will log
  its own, and show where its battery and embedded controller are.
- Host tests:
  - a DSDT built in the test: a PCI root, a region and fields, a
    battery, an `If`/`Else`, an `External`, a load-time call and an
    alias;
  - package lengths and names;
  - damaged input.
- Not yet: running anything. Step 2 adds port I/O, firmware memory and
  PCI configuration access for the service (system calls 53–55).

## Alternatives considered

- **The rust-osdev crates on nightly:** the project builds on stable.
- **ACPICA through C:** a large C dependency, built separately for
  every target.
- **In the kernel:** a fault in firmware's code would take the system.

## Checklist (master spec §48)

- Purpose: the base for battery, lid, buttons and brightness on laptops.
- Architecture: a library, a system call, a service.
- API: `FIRMWARE_TABLE`; `oceans_aml::Namespace`.
- Dependencies: none.
- Security: read-only tables. The right to them is the device bus's
  `WRITE`, granted to one service. The parser is bounded.
- Testing: host tests; QEMU's tables in every smoke test.
- Failure: a block that cannot be read is reported and skipped, and the
  others still load.
