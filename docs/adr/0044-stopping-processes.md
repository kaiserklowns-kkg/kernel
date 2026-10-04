# ADR-0044: Stopping processes (ABI 12)

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0012 (scheduling), ADR-0013 (IPC), ADR-0014 (processes),
  ADR-0015 (process capabilities)
- Adds: ABI 12 (`PROCESS_KILL`)

## Context

A process could end only by itself: by exiting, or by a CPU exception.
Nothing could stop one that hangs, loops, or misbehaves.

Phase 5 needs exactly that:
- the app manager stops apps;
- withdrawing a permission must end the app holding it;
- a user must be able to stop a stuck program.

The process capability already carries `MANAGE` (ADR-0015), reserved for
this.

What makes it hard is that the target's thread is usually *blocked*:
- in an IPC call or receive;
- in a notification or process wait;
- in console input;
- asleep.

Each wait registers the thread with an object that will wake it later. A
kill must get the thread out of any of them without:
- losing another thread's wake-up;
- leaving a stale registration behind (a dead receiver handed a call, a
  withdrawn call left queued);
- waking a thread twice.

## Decision

- **`PROCESS_KILL(process)`** (syscall 41) needs `MANAGE` on the process
  capability.
  - The process ends with exit code **`EXIT_KILLED` (-127)**, before it
    runs any more of its own code.
  - Killing a process that has already exited does nothing. A process may
    kill itself.
  - Exception exit codes are `-128 - vector`, so the two never collide.
  - Its capabilities are closed as at any exit: peers see `PeerClosed`,
    exit watchers are signalled, waiters woken.
- **Mechanism: interruption.**
  - A process records its thread at spawn.
  - `kill` marks the process, then **interrupts** the thread (it is
    marked, so it never blocks or sleeps again):
    - if it is blocked, it is made runnable;
    - if it is asleep, it leaves the sleep queue
      (`RunQueue::wake_sleeper`).
  - **Every wait** checks the mark, removes its own registration, and
    returns at once:
    - endpoint receive: from the receivers;
    - endpoint call: its call, if not yet received (a server holding it
      may still answer; nobody listens);
    - notification wait and process wait: from the waiters;
    - console read: from the readers.
  - **The thread then exits**, before user mode:
    - **after the syscall:** the syscall return path checks the mark and
      exits with `EXIT_KILLED`;
    - **after an interrupt:** a thread preempted in user mode is caught on
      the return to user mode (a hook on the interrupt path).
  - **Waking an interrupted thread is a no-op.** A wake-up still in flight
    for it (an object that popped it before it deregistered) is harmless.
    Waking a thread that is neither blocked nor interrupted is still a
    kernel bug and panics.
  - **The IPC direct switch** (`block_and_switch_to`) falls back to a
    plain block when its target was interrupted meanwhile.
- **Single CPU:** a kill takes effect when the target next runs, at most
  one scheduling decision later. Interruption is safe there because every
  check-register-block sequence runs with interrupts disabled (ADR-0012).
  SMP will need the same marks with an inter-processor interrupt.
- **Userspace:** `oceans_rt::process_kill` and `oceans_rt::EXIT_KILLED`.

## Consequences

- **What it enables:** stopping apps, ending the holder of a revoked
  permission, and stopping runaway programs. The app manager and
  permission broker (ADR-0045 onwards) build on it.
- **What a killed process loses:**
  - no cleanup code of its own runs, as on any crash;
  - services see its handles close and release what they held (the fs
    commits, drivers end sessions).
- **What it does not give:** a kill does not reach a process's
  descendants. A supervisor that needs that keeps their handles; process
  groups can come later if needed.

## Alternatives considered

- **Kill flag checked only at syscall return:** blocked processes, the
  usual case, would never die.
- **Closing the victim's capabilities to wake it:** objects it waits on
  outlive its handles (an endpoint shared with others, its own
  notification), so this neither wakes it reliably nor removes stale
  registrations.
- **Asking the process to exit (a signal it handles):** a cooperative
  stop is useful, but it cannot be the only one; a stuck or hostile
  program ignores it. It can be added on top (an event before the kill).

## Checklist (master spec §48)

- **Purpose:** ending another process, for supervisors, the app manager
  and the user.
- **Architecture:**
  - a kill mark on the process;
  - interruption of its thread in the scheduler;
  - deregistration in every wait;
  - exit at the next return to user mode.
- **API:**
  - `PROCESS_KILL` (41, ABI 12);
  - `EXIT_KILLED` (-127);
  - `oceans_rt::process_kill`.
- **Dependencies:** none.
- **Security:**
  - needs `MANAGE` on the process capability; init and spawners hold it,
    and a duplicate can drop it (a watch-only handle cannot kill);
  - the victim runs no more user code once killed.
- **Testing:**
  - **Host:** `wake_sleeper` takes out only the matching sleeper; the
    others still wake on time.
  - **Smoke (every boot), the parent self-test kills a child in each kind
    of wait:**
    - receiving;
    - calling (its call must not stay queued);
    - waiting on a notification;
    - asleep for an hour;
    - running a busy loop that never enters the kernel.
  - **Checked for each:** it exits with `EXIT_KILLED`; its endpoint ends
    are closed (`PeerClosed` both ways); a second kill is harmless; a
    handle without `MANAGE` is refused.
- **Failure behaviour:**
  - a kill always ends the process;
  - waits interrupted mid-way leave no state behind;
  - unknown handles or missing rights are refused with the usual errors.
