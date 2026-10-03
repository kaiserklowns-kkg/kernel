# ADR-0018: The shell: a capability-explicit command line

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0016 (init), ADR-0017 (console)
- Meets the Phase 3 exit criterion: an interactive shell in QEMU.

## Context

Oceans needs an interactive command line. In a capability system the shell
must not be a back door: it may hold only what init grants it, and anything
it runs should get only the authority the user deliberately gives it.

## Decision

### Shell (`user/shell`, a service in `services.conf`)

- **Terminal handling in userspace** (the kernel delivers raw bytes,
  ADR-0017):
  - echo; Backspace/DEL; Ctrl-C cancels the line; Ctrl-U clears it;
  - CR, LF and CR LF all mean Enter;
  - ANSI escape sequences (arrow keys) are swallowed, not inserted.
- **Type-ahead is kept.** Input is buffered across lines, so bytes typed
  while a command runs belong to the next line.
- **Commands:**

  | Command | Meaning |
  |---|---|
  | `help` | list commands |
  | `echo TEXT` | print TEXT |
  | `grants` | list the capabilities this shell holds (its handle directory) |
  | `call ENDPOINT TEXT` | IPC call to an endpoint the shell `use`s; print the reply |
  | `run PROGRAM [GRANT...]` | run a program with **only** the listed authority, wait, report its exit |
  | `clear`, `exit` | clear the screen; leave the shell (init restarts it if `restart = always`) |
- **`run` is capability-explicit.**
  - Grants are `log`, `console` and `use:ENDPOINT`. Each becomes a
    *narrowed* duplicate (for example, `use:` gives only `SEND` and
    `TRANSFER`), placed in the child's table in the order typed.
  - With no grants, the program gets nothing.
  - The shell can grant only what it holds (`this shell does not hold it`)
    and only with rights it may pass on.
  - A child killed by a fault is reported as `killed by CPU exception N`.

### init contract changes (extends ADR-0016)

- **Handle directory.** Every service gets one extra, last handle: a
  read-only memory object listing `<index> <kind> <name>` for each handle
  (kinds `log`, `console`, `provide`, `use`, `module`, `directory`).
  Programs find capabilities by name instead of by position. Existing
  positional services are unaffected.
- **`grant = module:NAME`** gives a service a read-only copy of a boot
  module (a program it may run).
- Granted capabilities now include `DUPLICATE`. A service can pass narrower
  copies on (the shell to its children); rights are never widened. The
  kernel's module capabilities to init gained `DUPLICATE` and `TRANSFER` for
  the same reason.

### Kernel change: console output no longer disables interrupts

Kernel log lines and console output used to be written with interrupts
disabled. Serial output is slow, so the UART's 16-byte receive FIFO
overflowed whenever input arrived during output, and keystrokes were lost.
The console lock is now held with **preemption** disabled (a new
`sched::NoPreempt` section) but **interrupts enabled**, so receive
interrupts keep draining the UART:
- a preemption that falls due inside the section is deferred to its end;
- no interrupt handler takes the console lock, and holders never block, so
  this cannot deadlock.

## Consequences

- What a program can do is visible on the command line that started it:
  the opposite of an ambient-authority shell.
- There is no filesystem yet, so programs are boot modules granted to the
  shell; the filesystem service (next) will provide them.
- There is no job control: `run` waits for the child, and a child given
  `console` reads the same input stream.
- Serial transmission is still synchronous (byte by byte, with preemption
  off). Interrupt-driven transmit is future work.

## Testing

Smoke boot: the smoke manifest runs the shell with `log`, `console`,
`use = echo`, `module:hello-client` and `module:crasher`, and expects exit
0. When the shell logs `shell: ready`, `cargo xtask smoke` **types a
script** at a typing pace (2 ms per byte):
1. `help`
2. `echo hello from the shell`
3. `echo abc⌫d`
4. `grants`
5. `call echo ping`
6. `run hello-client log use:echo`
7. `run crasher log`
8. `run hello-client use:nothing`
9. `run nosuch`
10. `frobnicate`
11. `exit`

xtask then requires each expected output on the console:
- the help text;
- `hello from the shell` and `abd` (Backspace works), as whole lines;
- `module hello-client` in the `grants` listing;
- `PING`;
- `hello-client exited with 0`;
- `crasher was killed by CPU exception 14`;
- `does not hold it`, `no program named nosuch` and `unknown command`.

init checks that the shell exited 0. This passes in debug, release and with
`-cpu max`. A normal boot ends at an interactive `oceans>` prompt
(`cargo xtask run`).

This replaces ADR-0017's single-line `console-test` service: two console
readers in one boot would compete for input.

While building the test, it found two input-loss bugs, both fixed above:
- interrupts were disabled during console output, which overflowed the
  UART FIFO (kernel);
- the shell dropped type-ahead after Enter (shell).
