# ADR-0085: Switching off and restarting (ABI 16)

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0017 and ADR-0021 (ACPI tables), ADR-0016 (init),
  ADR-0044 (stopping processes), ADR-0022 (durable filesystem),
  ADR-0076 and ADR-0078 (the desktop)
- Adds: ABI 16 (`SYSTEM_POWER`), `grant = power`
- Part of Phase 10 (Alpha).

## Context

Oceans could not switch a machine off or restart it. People held the power
button, which cuts power in the middle of whatever is running. Files that
are committed survive that (ADR-0022), but anything not committed yet is
lost. An update installed with `update apply` (ADR-0071) needs a restart
too. Every tester on real hardware needs both, and expects them in the
desktop as well as in the shell.

Switching a PC off is ACPI's job:
- the FADT says where the PM1 control registers are;
- the `\_S5` object in the DSDT says which sleep type means "off".

Restarting has a reset register in the FADT, and older fallbacks. The
kernel already validated the RSDP, the MADT and the MCFG, but had no AML
interpreter, and adding one is a large project of its own.

## Decision

**The kernel (ABI 16).**
- `oceans-acpi` parses the FADT and finds the sleep types:
  - the FADT: the PM1a/PM1b control registers (64-bit `X_` fields win),
    `SMI_CMD`/`ACPI_ENABLE`, the reset register (only when
    `RESET_REG_SUP` is set), and, for hardware-reduced ACPI, the sleep
    control register;
  - `sleep_type(aml, "_S5_")` looks for `Name (_S5, Package () {…})`
    in the AML bytes without running any. Every byte read is
    bounds-checked, and the values are cut to their 3 bits.
- At boot, `acpi::discover` reads the FADT, then searches the DSDT, then
  each SSDT, for `\_S5`.
  - Up to 4 MiB is copied, for the search only.
  - A DSDT with a bad checksum is still searched (firmware ships them),
    with a warning.
  - `power::init` maps memory-space registers once and logs what it will
    use, or says plainly that this machine cannot be switched off.
- **`SYSTEM_POWER(system, action)`** (syscall 48):
  - `QUERY` returns `CAN_OFF | CAN_RESTART` and needs `READ` on the
    system information object;
  - `OFF` and `RESTART` need **`MANAGE`** on that object. `MANAGE` is now
    one of its default rights, init keeps it, and init only ever hands
    out `READ` (`grant = sysinfo`).
- **Off:**
  1. ACPI mode is entered through `SMI_CMD` if `SCI_EN` is clear, polling
     for up to 3 s.
  2. With interrupts off, SLP_TYP goes to PM1a (and PM1b) control.
  3. The caches are flushed (`WBINVD`).
  4. SLP_TYP | SLP_EN is written.

  On hardware-reduced ACPI the sleep control register gets the type and
  `SLP_EN` instead. If the machine is still running 3 s later, the kernel
  says it is safe to turn it off and halts. Without `\_S5`, `OFF` returns
  `NotFound` before anything happens.
- **Restart:** each way gets 500 ms before the next one is tried, and
  every fallback is logged:
  1. the ACPI reset register (an I/O port, memory, or bus-0 PCI
     configuration space);
  2. the chipset's reset control at 0xCF9;
  3. the 8042 keyboard controller (0xFE);
  4. a triple fault.

**init stops the system.**
- init makes a `power` endpoint and binds its service-exit notification
  to it, so one receive loop sees both exits and requests.
  `grant = power` hands a client end to a service.
- A request (label `OFF` or `RESTART`) is answered first with one byte:
  - `ACCEPTED`, only if `QUERY` says this machine can do it;
  - `NOT_POSSIBLE`;
  - `INVALID`.
- After `ACCEPTED`, init stops the system:
  1. It stops, in reverse start order, every service started after the
     one providing `fs` (`PROCESS_KILL`, then waits for each). Their open
     files close, which commits them.
  2. It sends `SYNC` to `fs`, which also reaches every mounted disk.
  3. It stops the remaining services (drivers, the other filesystems) in
     reverse order.
  4. It calls `SYSTEM_POWER`.
- In test mode init does not call `SYSTEM_POWER`: it checks the manifest's
  expectations and exits, so the kernel's smoke contract (init exits 0,
  then `isa-debug-exit`) is unchanged.

**Who may ask.**
- The shell (`shutdown`, `reboot`): the console is the machine's
  administrator.
- The desktop: **Restart** and **Shut Down** in the apps panel's footer.
  Each is confirmed in a dialog the system draws ("Shut down Oceans?
  Every app and service stops first…", Cancel / Shut Down).
- No app has `grant = power`, and there is no app permission for it.
- `oceans_rt::request_power` is the client call.

## Consequences

- A real machine can be switched off and restarted cleanly. Files are
  committed and the disks synced before the power goes, and the steps
  are in the log.
- Restarting into an update no longer needs the power button.
- Every smoke boot now ends as a user would end it:
  - The main smoke's first boot ends with `reboot`, the second with
    `shutdown`. init stops the system in test mode, and the shell's
    expected exit is now `-127` (stopped).
  - `smoke-hw` ends its first boot with a real ACPI reset and its second
    with a real ACPI S5. QEMU (`-no-reboot`) must exit by itself, with
    status 0, and with no fallback logged.
- **Limits:**
  - **No AML is run.** `\_PTS`, `\_TTS` and a `\_S5` defined as a method
    are not supported; Oceans then says it cannot switch the machine off.
    Real AML is for an interpreter later, alongside suspend (S3).
  - **Stopping is a kill, not a request.** A service gets no chance to
    finish. What it had not written or committed is lost, as in a crash,
    but the volume stays consistent. For the same reason `logkeep` may
    miss the last couple of seconds, which are the shutdown's own lines.
  - **Orphans:** apps started by Core, and programs started by the shell,
    are not stopped when their parent is (ADR-0044). They keep running
    until the power goes, after `SYNC`, so their later writes are lost.
  - **NVMe** is not told about the shutdown (no `CC.SHN`), so an SSD
    counts an unsafe shutdown. That needs a stop request to the driver.
  - **No timeouts:** a filesystem that never answers `SYNC` holds the
    shutdown.

## Alternatives considered

- **A userspace ACPI service.** This is where AML belongs eventually.
  But switching off is the last thing that runs: everything else has
  already been stopped, the service included. The writes are a handful
  of registers the kernel already knows how to reach. For now the kernel
  keeps the mechanism and init the policy.
- **An AML interpreter now (ACPICA, or a Rust one).** It would be
  correct for `\_PTS` and method-defined `\_S5`, but it is a large body
  of firmware-facing code for a feature that works on PCs without it.
  Licensing and the attack surface also need their own decision.
- **A new kernel object for power.** It would need another boot handle,
  and init's handle layout already has an optional slot. `MANAGE` on the
  system object says "manage the system", and init already holds that
  object.
- **Asking services to stop politely first.** There is no stop protocol
  yet. Inventing one for every service was out of scope, and storage is
  crash-consistent. It is the natural next step: a grace period before
  the kill.
- **An app permission for power.** Not needed by any app now, and a
  denial-of-service lever. Left out.

## Checklist (master spec §48)

- **Purpose:** switch off and restart cleanly from the shell and the
  desktop.
- **Architecture:**
  - `oceans-acpi`: parsing (FADT, `\_S5`);
  - the kernel's `power`: the mechanism, behind `SYSTEM_POWER`;
  - init: the policy (who may ask, the stop order, the sync);
  - the shell and the desktop: the clients.
- **API:**
  - `SYSTEM_POWER` (48; `power::QUERY/OFF/RESTART`, `CAN_*`);
  - init's `power` endpoint (answers `ACCEPTED/NOT_POSSIBLE/INVALID`);
  - `oceans_rt::{power_query, system_power, request_power}`;
  - `grant = power`.
- **Dependencies:** none new.
- **Security:**
  - only init holds `MANAGE` on the system object;
  - asking init needs `grant = power`, given to the shell and the desktop
    only;
  - the desktop confirms in a system-drawn dialog;
  - firmware tables are untrusted: lengths and bounds are checked, sleep
    values are cut to 3 bits, and registers outside the I/O port range or
    in unknown spaces are refused.
- **Testing:**
  - unit tests: the FADT (q35's layout, ACPI 1.0 length, no
    `RESET_REG_SUP`, 64-bit fields, hardware-reduced) and `\_S5` (QEMU's
    and a PC's encodings, long PkgLength, one-element packages, a method,
    truncated and malformed AML);
  - the kernel self-test: QEMU describes both ways;
  - smoke: `reboot` and `shutdown` from the shell, init's stop order and
    sync;
  - `smoke-hw`: the real ACPI reset and S5 on the production manifest.
- **Failure behaviour:**
  - cannot switch off: nothing is stopped, and the shell or the desktop
    says so;
  - the machine does not switch off: the kernel says it is safe to turn
    it off and halts;
  - the reset register fails: the next way is tried, each one logged;
  - `SYNC` fails: logged, and the shutdown continues;
  - the kernel refuses: init logs it and exits (the kernel logs that no
    userspace runs).
