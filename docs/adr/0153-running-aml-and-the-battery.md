# ADR-0153: Running AML, and the battery read from the firmware

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0148 (the firmware's AML read into a namespace),
  ADR-0152 (a laptop or a desktop; `/system/battery/status`), ADR-0021
  (the device bus), ADR-0016 (init's grants)
- Part of Phase 10 (Alpha). ABI 28.

## Context

ADR-0148 read the DSDT and SSDTs into a namespace but ran nothing. A
laptop's battery is behind methods: `_BIX` or `_BIF` say what it is,
`_BST` how it is now, and they read the embedded controller's registers
through operation regions and fields. ADR-0152 put the battery on the
screen from a status file. This ADR is what writes that file: the
methods run, against the hardware.

## Decision

### The interpreter (`oceans_aml::eval`, host-tested)

- **A `Machine`** owns a loaded namespace and a `Host`. A method's body
  is read statement by statement when it runs: `If`/`Else`, `While`,
  `Break`, `Continue`, `Return`; `Name` and buffer fields a method
  defines go when it returns; other definitions inside a method
  (regions, fields) are defined as a block defines them.
- **Expressions**: integers, strings, buffers, packages (names in them
  are references), locals and arguments, references (`RefOf`, `Index`,
  `DerefOf`, `CondRefOf`), arithmetic and logic, comparisons by the
  first operand's type, the conversions (`ToBuffer`, `ToInteger`,
  `ToHexString`, `ToDecimalString`, `ToString`, `Mid`, `Concatenate`,
  `ConcatenateResTemplate`), `Match`, `SizeOf`, `ObjectType`,
  `CopyObject`, BCD, `Notify`, `Sleep`, `Stall`, `Timer`, mutexes and
  events (one thread runs AML: `Acquire` always has it, `Wait` times out
  unless signalled).
- **Stores** convert as ACPI says: into an integer an integer, a string a
  string, a buffer keeps its length; into an element of a named package
  in place.
- **Fields**: region fields, index fields and bank fields, by their
  access width, with the update rule (preserve, ones, zeros) for partial
  writes; values over 64 bits (32 where the DSDT's revision is below 2)
  are buffers. Buffer fields (`CreateDWordField` and the like) read and
  write their buffer.
- **The operating system**: `\_OSI` says yes to every Windows from 2000
  to 2022 and to the features ACPI names, no to the rest (firmware is
  written and tested for Windows); `\_OS` is "Microsoft Windows NT",
  `\_REV` 2.
- **Untrusted code**: calls nest at most 32 deep, one evaluation runs at
  most 4,000,000 steps (a loop that never ends is an error), `Sleep` and
  `Stall` are capped, buffers and packages are bounded, and every
  failure is an error, never a panic. Damaged methods are tested.
- **`oceans_aml::battery`**: `_STA`'s presence bit, `_BIX` (or `_BIF`),
  `_BST`, `_PSR`, read into a charge, a state and the minutes left.
- **`Machine::connect`** runs `_REG (space, 1)` for the devices with a
  region in a space, once its handler is ready.

### Reaching the hardware (ABI 28; system calls 53–55)

All three need `WRITE` on the device bus, which init gives with
`grant = firmware` (ADR-0148).

| Call | What | What it keeps out |
|---|---|---|
| `FIRMWARE_PORT` (53) | reads or writes 1, 2 or 4 bytes at an I/O port (`SystemIO`, the EC) | the ports the kernel keeps: DMA, the interrupt controllers, the timer, the keyboard controller, the clock, the serial ports, PCI configuration (`MissingRights`) |
| `FIRMWARE_MEMORY` (54) | a memory object for physical memory (`SystemMemory`) | anything but reserved memory, ACPI NVS and what the map does not list; any page a PCI device's BAR decodes |
| `FIRMWARE_PCI_READ` (55) | reads PCI configuration space (`PCI_Config`) | writes: a device's registers stay its driver's |

### The `acpi` service

- It loads the blocks (ADR-0148), then runs `\_SB._INI` and each present
  device's `_INI`, as other systems do at start, and each device's
  `_STA` and `_CRS` once (they only read; what the interpreter cannot
  run shows in the log).
- **The embedded controller** (`PNP0C09`): its data and command ports
  from `_CRS`; a byte is read with command 0x80 and written with 0x81,
  waiting on the input and output buffer bits, a millisecond's sleep
  between polls after the first few, given up after 2,000. Then `_REG`.
- **The battery**: while a battery device (`PNP0C0A`) is there, every
  5 seconds, and at once when it or the adapter notifies a change, the
  batteries are read together (their charge summed) with the adapter's
  `_PSR` into `/system/battery/status` (`grant = storage:/system/battery
  as battery`), the line ADR-0152's desktop reads. A battery taken out:
  the file goes. Each change is logged (`acpi: battery 75%, 2:00 left`).
- A machine without a battery: the service says so and ends.

## Consequences

- **The ZBook** will show its real charge once updated, if its battery
  methods run here. If not, the log names the method and the error.
- **The smoke tests**: QEMU's machine has no battery, but every device's
  `_STA` and `_CRS` run on QEMU's own AML (44 methods, among them the
  PCI root's `_CRS` with buffer fields and resource templates), and the
  test expects none to fail.
- **Host tests**: methods that compute (loops, locals, `_OSI`, buffer
  fields, `Notify`, `Sleep`, an endless loop stopped, a port refused), an
  embedded controller with `_REG` and a battery whose `_BIF` and `_BST`
  read it, the reading (charge, minutes to empty or to full), and
  damaged methods.
- Not yet: the lid, the buttons and the EC's events (`_Qxx` through the
  SCI), `_BCM` for brightness, tables loaded at run time (`Load`
  answers failure).

## Alternatives considered

- **Run AML in the kernel**, as most systems do: a fault in firmware's
  code would take the system, and its ports and memory would be the
  kernel's to keep safe in every line of the interpreter rather than in
  three system calls.
- **Write PCI configuration for AML**: some firmware does, but a
  driver's device could be reprogrammed under it.
- **Read the EC's battery registers directly**: their places differ
  from one model to the next; the firmware's methods are what knows
  them.

## Checklist (master spec §48)

- **Purpose.** The real battery on a laptop.
- **Architecture.** A host-tested interpreter in a service; three system
  calls that bound what it reaches; a status file the desktop reads.
- **API.** `FIRMWARE_PORT`, `FIRMWARE_MEMORY`, `FIRMWARE_PCI_READ`
  (ABI 28); `oceans_aml::eval::{Machine, Host}`, `oceans_aml::battery`.
- **Dependencies.** None.
- **Security implications.** Firmware's code runs in a service with one
  right, the device bus's `WRITE`; the kernel refuses its own ports, RAM
  and devices' BARs; PCI configuration is read only; the interpreter is
  bounded in depth, steps and sizes.
- **Testing strategy.** Host tests; QEMU's AML run in every smoke test.
- **Failure behaviour.** A method that fails is logged and the battery is
  tried again 5 seconds later; an EC that does not answer fails the read,
  not the service; with nothing read, the desktop shows the battery as
  not read yet (ADR-0152).
