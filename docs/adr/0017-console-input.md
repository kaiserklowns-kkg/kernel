# ADR-0017: Console input — ACPI, I/O APIC and a console capability

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0012 (scheduler), ADR-0014–0016 (processes, ABI, init)
- Adds: ABI v4 (syscalls 23–24)

## Context

Phase 3's exit criterion is an interactive shell, so keyboard input must
reach userspace. On the Tier 0 reference machine (QEMU, `-serial stdio`) and
on headless servers, the console is the 16550 serial port. USB HID
keyboards and a framebuffer console come with Phase 4 device support.

## Decision

### Ownership

The serial port is the kernel's early console (ADR-0002 keeps boot-critical
console code in the kernel), so the kernel owns both directions. It adds
**no policy**:
- no echo;
- no line editing;
- no CR/LF translation.

Raw bytes go to whoever reads, and terminal behaviour belongs to that
program (the shell, later a terminal service).

### Interrupt routing (no polling)

- **ACPI** (`libs/acpi`, host-tested): the RSDP from Limine, then
  RSDT/XSDT, then the **MADT**. Every table is checksum- and
  length-validated, and malformed entries stop the iteration. Tables are
  read through the direct map when they lie in RAM; otherwise through a
  read-only, uncached mapping (`memory::read_physical`). Only I/O APICs and
  ISA interrupt overrides are used; the rest of ACPI is userspace's later.
- **I/O APIC** (`arch/x86_64/ioapic.rs`): every line is masked at init and
  only explicitly routed lines are delivered. COM1's ISA IRQ 4 is resolved
  through the MADT overrides (polarity and trigger included) to a GSI and
  routed to vector 49 on the boot CPU's local APIC.
- **UART:** the receive-data interrupt is enabled, along with OUT2, which
  gates the IRQ line on PC boards. The handler drains the FIFO (bounded),
  then acknowledges, so a level-triggered line cannot re-fire for handled
  data.
- **Latency:** if the CPU was idle and the interrupt woke a reader, the
  scheduler switches to it right after EOI rather than at the next tick
  (`set_after_device_interrupt`).

### Buffering

A fixed 4 KiB ring with no allocation in interrupt context. When it is
full, bytes are dropped, counted, and reported as a warning on the next
read. Readers block FIFO and are woken one per interrupt.

### Capability and ABI v4

- `KernelObject::Console`: `READ` for input, `WRITE` for output, plus
  `DUPLICATE` and `TRANSFER`.
- `CONSOLE_READ(console, ptr, cap) → count` blocks until input exists. The
  destination is validated **before** consuming input, so a bad pointer
  loses no keystrokes.
- `CONSOLE_WRITE(console, ptr, len)` writes raw bytes in 64-byte chunks.
  Each chunk is atomic relative to kernel log lines, and the chunking keeps
  output from holding interrupts off for long.
- Limit: 4096 bytes per call.
- **init gets the console as handle 2**; boot modules now start at handle 3.
  Services get it only through `grant = console` in `services.conf`.
  **Keyboard input is an explicitly granted capability**, so a service
  without it cannot read keystrokes.

## Consequences

- `DEBUG_WRITE` (prefixed log lines) and `CONSOLE_WRITE` (raw terminal
  output) are now separate: logs stay attributable, and terminals stay
  clean.
- Several readers share one input stream; arbitration (a foreground
  terminal) is a terminal service's job.
- Serial output is synchronous byte-by-byte transmission. Fine for QEMU;
  on real hardware a TX interrupt queue will be needed.

## Testing

- `cargo test -p oceans-acpi` (5 tests): RSDP v1 and v2; bad signatures and
  both checksums; truncation; table length and checksum validation;
  RSDT/XSDT entries; MADT I/O APIC and override parsing (polarity and
  trigger); malformed and zero-length entries.
- *Superseded by the ADR-0018 shell test, which types a whole script.* The
  original test:
- Smoke boot: the `console-test` service (granted `log` and `console`)
  announces it is waiting. `cargo xtask smoke` then **types
  `hello oceans⏎` into QEMU's serial input**. The service echoes the bytes,
  reads up to Enter, and must receive exactly that line (init checks
  `expect-exit = 0`). This passes in debug, release and with `-cpu max`
  (`OCEANS_QEMU_EXTRA="-cpu max"`).
- Found while testing: QEMU's Windows stdio backend drops a lone CR from
  piped input. The harness sends CR LF, and the guest accepts either as
  Enter.
