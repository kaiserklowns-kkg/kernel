# ADR-0104: A kill is never lost

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0044 (stopping processes)
- Part of Phase 10 (Alpha: basic apps).

## Context

ADR-0044 kills a process by interrupting its thread: if it is blocked or
asleep it runs again, and it will not block or sleep again. The smoke
boots sometimes hung: a process killed at once after it was started
never ended, and whoever waited for it (the kernel's self-test, Core
stopping an app) waited for ever.

**Why:**
- `sleep_ms` and `block` looked at the thread's `interrupted` flag, then
  took the scheduler's lock to sleep or block.
- A kill on another CPU in between set the flag and, under the lock,
  found the thread neither blocked nor asleep: nothing to wake.
- The thread then slept (an hour, for the self-test's victim) or blocked,
  with nobody left to wake it.

**What users expect, on every system:** a process that is ended (Force
Quit on macOS, End task on Windows, `kill -9` on Linux) ends, whatever it
was doing at that moment.

## Decision

- **The scheduler looks again under its lock:** a thread that was
  interrupted does not sleep or block, even if it was not interrupted when
  `sleep_ms` or `block` first looked. `interrupt` sets the flag before it
  takes the lock, so either it finds the thread asleep or blocked and
  wakes it, or the thread sees the flag.
- **The self-test kills at once:** the parent of `ipc-test` kills sixteen
  victims just after starting them, as they go to sleep or to wait, and
  waits for each.
- **The smoke harness reports an early end:** the console reader says when
  QEMU closed its output, so a panic before the shell script began fails
  at once instead of after the 600 s timeout.

## Consequences

- Killing a process ends it, whenever the kill comes.
- The other wake-ups (`wake`) were not affected: `wake_pending` already
  covered a wake between the look and the lock.

## Alternatives considered

- **Setting `wake_pending` in `interrupt`:** it covers `block` but not
  `sleep_ms`, which does not look at it.
- **A timeout on every kernel wait:** hides the lost kill instead of
  removing it, and the process would end late.
