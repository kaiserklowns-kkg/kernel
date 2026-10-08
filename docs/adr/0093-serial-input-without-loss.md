# ADR-0093: Serial input without loss: flow-controlled test consoles, a lower FIFO trigger, overruns reported

- Status: Accepted
- Date: 2026-10-08
- Depends on: ADR-0017 (the console), ADR-0018 (serial input by
  interrupt), ADR-0089 (the diagnostic key)
- Part of Phase 10 (Alpha: diagnostics).

## Context

Local smoke tests sometimes hung. The shell had echoed part of a typed
command and then waited for the rest, which never came:
- the release smoke at `app install`;
- smoke-hw at `write /hw-note.txt …`, where the shell received exactly 14
  bytes of 42.

CI on Linux never hung this way.

To find out why, the kernel counted how many bytes each serial interrupt
found waiting, and smoke-hw typed 60 long lines at 1 ms per byte while
the system was booting.
- **Some interrupts came late:** up to 14 bytes waited for one interrupt,
  which is the FIFO's trigger level.
- **Lines arrived with bytes missing**, for example
  `…typed-to-stree-serial-console-inpth`.

QEMU's 16550 stops accepting bytes once its FIFO holds as many as the
trigger level, until the guest reads them. What happens to a byte the
UART cannot take depends on QEMU's serial backend:
- **Linux stdio and every socket:** the byte waits on the host.
- **Windows stdio:** the byte is **dropped**.

So on Windows, a serial interrupt served a little late (load on the host
or the guest) lost keystrokes. A lost Enter left the shell waiting
forever, and the test with it.

## Decision

- **Tests talk to the serial line over TCP.** `smoke`, `smoke-hw` and
  `smoke-secure-boot` start QEMU with
  `-serial tcp:127.0.0.1:PORT,server=on,wait=on` and connect to it
  (`serial_args`, `connect_serial`). A socket is read only when the UART
  can take a byte, so input waits instead of being lost, on every host.
  `cargo xtask run` keeps stdio, for a person at the terminal.
- **The kernel interrupts at 8 bytes, not 14.** On a real 16550 at 115200
  baud this leaves 8 bytes (about 0.7 ms) for the interrupt to be served
  before the FIFO overruns, instead of 2 bytes (0.17 ms). Fewer waiting
  bytes still raise an interrupt after four characters' time.
- **Overruns are reported.** The interrupt handler notes the UART's
  overrun flag (LSR bit 1). The next console read logs
  "the serial line overran N time(s); typed input was lost", from the
  reader's context: interrupt handlers never log.
- **A hung hardware smoke leaves its state:**
  - on timeout, `smoke-hw` presses the diagnostic key (ADR-0089);
  - it prints where each CPU is, through QEMU's monitor, as `smoke`
    already did.

## Consequences

- **The local smoke tests no longer lose typed input.** The same stress,
  60 long lines at 1 ms per byte while booting, delivered every line
  intact over TCP. Before, several lines lost bytes.
- **Late interrupts still happen under emulation.** Up to about 20 ms
  were seen while booting under stress. With flow control they cost time,
  not input.
- **Real serial consoles** get more headroom, and a loss is now visible in
  the log instead of looking like a hang.
- **Limits:**
  - `cargo xtask run` on Windows can still drop input pasted faster than
    the guest reads it. Typing by hand is far slower than that.
  - A late interrupt's cause under QEMU is not measured further: the
    host's scheduling of QEMU's threads and the guest's load cannot be
    told apart from inside the guest.

## Alternatives considered

- **Waiting for each typed byte's echo in the tests:** the echo is mixed
  with log lines from other CPUs, so matching it is fragile. It would also
  slow every test, and it fixes only what the tests type.
- **Typing through the USB keyboard (QEMU's monitor):** once the desktop
  takes the keyboard (ADR-0059), keys go to it, not to the shell.
- **A trigger level of 1:** an interrupt for every byte. QEMU would then
  hold only one byte at a time, which makes losing input on Windows stdio
  more likely, not less.

## Checklist (master spec §48)

- **Purpose:** serial input that is not lost, and that says so when it is.
- **Architecture:**
  - `arch::serial` (FIFO trigger, overrun flag);
  - `console::read` (the report);
  - xtask's `serial_args` and `connect_serial`, used by every smoke test.
- **API:** none changed. A new log warning.
- **Dependencies:** none.
- **Security:** none changed. Overruns are counted in an atomic, and
  nothing is logged from interrupt context.
- **Testing:**
  - the stress described above, run before and after the change;
  - every smoke test now runs over TCP.
- **Failure behaviour:** lost serial input is logged. A hung smoke prints
  each CPU's position and the scheduler's state.
