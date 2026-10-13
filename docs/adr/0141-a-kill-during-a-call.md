# ADR-0141: A kill during a call no longer strands the server

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0013 (IPC), ADR-0089 (scheduling on every CPU),
  ADR-0104 (kills are never lost)
- Part of Phase 10 (Alpha).

## Context

Now and then the whole system stopped, most often while an app was being
stopped or the system was shutting down. Every CPU was idle and no thread
was ready. The kernel's diagnostics showed the filesystem service blocked
receiving on its endpoint, and Core and others blocked calling that same
endpoint.

The cause is a direct switch (ADR-0013) cut short by a kill:

1. A client's `call` queues its message and takes the waiting server off
   the endpoint's receivers, so as to switch straight to it.
2. Between that and the switch, another CPU kills the client
   (ADR-0104: `interrupted` is set).
3. `block_and_switch_to` saw the flag and returned. So did `schedule_to`,
   when the flag came in between its own check and its lock.
4. The server was neither run nor woken, and no longer registered. The
   client withdrew its message and exited. The server slept on in
   `receive`, out of every list, and every later call to its endpoint
   waited forever.

## Decision

When a direct switch is abandoned because the caller was interrupted,
the target is woken (`wake`, or `wake_locked` under the scheduler's
lock).

- A server woken this way looks again: it finds a queued call, or
  registers as a receiver anew.
- If it has not blocked yet (on its way on another CPU), the wake is kept
  as `wake_pending`, which its receive loop already handles.

## Consequences

- Stopping an app, or a process killed at any moment, no longer leaves a
  service unreachable.
- The window is a few instructions wide, so the smoke tests met it only
  now and then. They are the test: hundreds of calls with apps stopped
  under way, on four CPUs.
- Hardening left for later: a receiver re-registering after a spurious
  wake could be listed twice; it is harmless (the extra entry is skipped
  as not blocked) but untidy.

## Alternatives considered

- **Taking the server off the receivers only once the switch happens:**
  a bigger change to the call path, for the same result.

## Checklist (master spec §48)

- Purpose: no system-wide hangs.
- Architecture: the scheduler's direct switch.
- API: none.
- Dependencies: none.
- Security: none.
- Testing: the smoke tests.
- Failure: what was a hang is now an ordinary wake-up.
