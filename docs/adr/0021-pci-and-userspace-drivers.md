# ADR-0021: PCI, device capabilities and userspace drivers

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0011 (capabilities), ADR-0016 (init), ADR-0017 (ACPI)
- Adds: ABI v7 (`DEVICE_*`, syscalls 28–34; errors `NotFound`, `Busy`); starts Phase 4
- Amended by: ADR-0022. The filesystem, not the shell, now holds
  `use = block`.

## Context

Phase 4 needs real hardware: storage first, because everything after it
(a filesystem that survives reboot, packages, models) needs a disk.
Drivers are the largest and least trustworthy part of any kernel, so the
master spec puts them in userspace where possible, and ADR-0011 already
planned capabilities for MMIO ranges, IRQs and DMA. This ADR makes them
real, with one complete driver to prove the design: virtio-blk on QEMU.

## Decision

### Enumeration (kernel)

- The ACPI MCFG is parsed (`oceans-acpi`, validated like every other
  table). Each PCI Express configuration (ECAM) bus is mapped once (1 MiB,
  uncached) and scanned. Bridges are followed with a depth limit, and each
  bus is visited once, so firmware loops cannot hang the boot.
- Configuration-space logic lives in a new host-tested crate,
  `oceans-pci`. It covers:
  - headers;
  - BAR sizing, done with decoding off and every register restored;
  - a bounded capability walk;
  - MSI-X parsing.
- Endpoints are left with **bus mastering off**: no device does DMA until
  a driver turns it on.

### Device capabilities

| Object | Rights | Allows |
|---|---|---|
| Device bus | `READ` | `DEVICE_LIST`: one 16-byte record per function |
| Device bus | `MANAGE` | `DEVICE_OPEN(vendor:device, index)`: open a function **exclusively** (`Busy` if already open) |
| Device | `READ` | `DEVICE_CONFIG_READ`: aligned 1/2/4-byte reads of configuration space |
| Device | `MANAGE` | `DEVICE_ENABLE`: turn on memory decoding and bus mastering, with legacy INTx off |
| Device | `MANAGE` | `DEVICE_BAR(n)`: a memory object for memory BAR *n* |
| Device | `MANAGE` | `DEVICE_DMA_CREATE(size)`: contiguous memory, plus the address the device uses for it |
| Device | `MANAGE` | `DEVICE_IRQ(entry, notification, bits)`: an MSI-X vector delivered as notification bits |

- **init holds the device bus (handle 4).** Boot modules now start at
  handle 5.
  - `grant = device:VVVV:DDDD` in `services.conf` opens that function and
    hands it to a service, which makes the service its driver.
  - `grant = devices` gives a read-only list of devices.
- **A device has exactly one capability:** its rights do not include
  `DUPLICATE`.
- **Drivers never write configuration space.** The kernel sets the
  command bits and programs MSI-X itself. So a driver cannot move a BAR
  over RAM or aim interrupts at another vector.
- **BAR memory objects:**
  - are mapped uncached and are never executable;
  - leave out the pages holding the MSI-X table and pending bits;
  - are refused if the BAR is smaller than a page (it would share a page
    with other registers) or overlaps RAM in the boot memory map;
  - cannot be read by the kernel: `copy_*_user` refuses non-RAM pages, so a
    user pointer into device memory is `BadAddress`, never a kernel fault.
- **DMA memory** is one physically contiguous, zeroed buddy block, up to
  4 MiB and never executable. Each open device gets at most 64 MiB.
- **BAR and DMA objects carry `READ | WRITE | MAP` only**, so they cannot
  leave the driver's process.
- **Closing a device**, or its driver dying, happens in this order:
  1. its MSI-X entries are masked and MSI-X is disabled;
  2. its vectors are freed;
  3. bus mastering and decoding are turned off;
  4. only then is its DMA memory released.

  A device can therefore never write into memory that has been reused.

### Interrupts

- Vectors 64–127 are for device interrupts (MSI-X). Each one signals a
  notification.
- The interrupt path only reads a fixed routing table and signals, which
  never blocks or allocates. It then sends EOI and lets the scheduler run
  the woken driver.
- MSI-X only for now. Devices without it can be polled. Legacy INTx would
  need ACPI `_PRT` (AML), which is not supported.

### The block protocol and the virtio-blk driver

- **`oceans-block-proto`.** A client opens a **session** by sending a
  memory object it has mapped, and gets back a badged session handle.
  `READ`/`WRITE` name sectors and an offset in that buffer. The driver
  copies between the buffer and its own DMA bounce buffer, so **clients
  never see or choose physical addresses**. `INFO` and `FLUSH` complete
  the protocol.
- **`virtio-blk`** is an ordinary service (`user/virtio-blk`):
  - modern virtio 1.x over PCI, finding its structures through the vendor
    capabilities;
  - feature negotiation (`VERSION_1`, plus read-only and flush when
    offered);
  - one split virtqueue in one DMA page;
  - completions via MSI-X, with a polling fallback;
  - one request in flight at a time;
  - checked ranges (`OutOfRange`) and a read-only refusal.
- **Utilities:**
  - `lspci` (`run lspci out devices`);
  - `disk` (`run disk out use:block -- info | read N | dump N | write N TEXT`).
- `services.conf` lists `provide = block` before the device grant. If no
  disk is attached, the driver does not start, and clients get "block
  service unavailable" instead of the shell failing.

## Consequences

- A driver bug kills a process, not the kernel. init restarts the driver
  and reopens the device, because the old capability closes at exit.
- **No IOMMU yet.** A driver that programs DMA can reach all of physical
  memory through its device. Drivers are therefore trusted services, and
  the device capability is exactly that grant of trust: visible, single,
  and given by `services.conf`. IOMMU support will turn device addresses
  into isolated I/O virtual addresses without changing the ABI, which is
  why it speaks of a "device address", not a "physical address".
- ECAM mappings and the MSI-X table mappings are permanent. They are
  bounded by the number of buses and functions.
- The filesystem is still in memory. Putting it on this disk comes next.

## Alternatives considered

- **Drivers in the kernel.** Simpler to write, but every driver bug becomes
  a kernel bug. That contradicts the capability model and the reliability
  goal.
- **Let drivers write configuration space.** That would let a driver
  remap BARs over RAM or retarget MSI. A narrow `DEVICE_ENABLE` and
  kernel-programmed MSI-X cover what drivers need.
- **Clients DMA straight into their own buffers (zero-copy).** That would
  need pinned, contiguous client memory and would expose physical
  addresses. The bounce copy costs little next to disk latency and keeps
  clients unprivileged.
- **Legacy INTx.** It needs AML to route on real hardware. MSI-X is
  universal on the device classes we target (NVMe, virtio, xHCI, modern
  NICs).

## Testing

- `cargo test -p oceans-acpi`: MCFG parsing; skipping entries with a zero
  base or inverted buses; ECAM address computation and bounds.
- `cargo test -p oceans-pci` (6 tests) runs against a simulated function
  with hardware-like BAR decoders. It checks:
  - BAR sizing (I/O, 32-bit and 64-bit memory) with decoding off during
    probes and every register restored;
  - BAR masks that are not powers of two are rejected;
  - capability walks over loops, pointers into the header and a missing
    list;
  - MSI-X naming a nonexistent BAR;
  - checked configuration reads.
- `cargo test -p oceans-abi`: device records round-trip; error codes
  -1…-16.
- **Kernel self-test** (smoke boots only):
  - devices open exclusively (`Busy`);
  - configuration reads are bounds-checked;
  - the kernel refuses to read device memory;
  - device memory is never executable;
  - the MSI-X table is a hole in its BAR;
  - DMA memory is contiguous and survives its handle while the device is
    open;
  - DMA memory is released when the device closes;
  - reopen works; an unknown ID gives `NotFound`.
- **Smoke script.** QEMU gets a fresh 8 MiB disk whose sector 0 holds
  `OCEANS TEST DISK`. The script checks:
  - bare `lspci` is refused (it asks for `devices`);
  - `run lspci out devices` shows the host bridge and
    `1af4:1042  mass storage  (driver attached)`;
  - `disk` without `use:block` explains what it needs;
  - `info` reports 16384 sectors;
  - `read 0` shows the label;
  - `write 1 written by oceans` succeeds and `read 1` returns the text;
  - `read 99999999` reports out of range;
  - **after QEMU exits, xtask reads the image file on the host** and
    requires sector 1 to hold exactly the text, so the write really went
    through the driver's DMA to the disk.
- Passes in debug, release and with `-cpu max`.
- Checked by hand:
  - a normal boot (`cargo xtask run`) keeps `build/disk.img`, and data
    written in one boot is read back in the next;
  - booting without a disk logs `cannot start block: NotFound`, and the
    shell keeps working.

## Checklist (master spec §48)

- **Purpose:** hardware access for userspace drivers; persistent storage.
- **API:** ABI v7 above, plus `oceans-block-proto`.
- **Dependencies:** none new; `oceans-pci` is in-tree and `no_std`.
- **Security:** exclusive device capabilities, no config-space writes,
  kernel-programmed MSI-X, MSI-X pages unmapped, RAM-overlap refusal,
  driver memory that cannot be transferred, and DMA stopped before its
  memory is freed. The no-IOMMU trust is stated above.
- **Failure behaviour:**
  - no MCFG: no devices, and the boot continues;
  - no disk: the service does not start and clients get an error;
  - device errors: `IoError` to the client;
  - driver crash: init restarts the driver.
