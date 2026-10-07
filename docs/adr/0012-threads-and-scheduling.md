# ADR-0012: Kernel threads and preemptive round-robin scheduling

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0009 (guarded kernel stacks, MMIO mapping), ADR-0010 (heap)
- Enables: ADR-0013 (IPC: blocking call/reply, direct switch)

## Context

IPC needs threads that block and wake. Userspace needs something to run
its processes on. Master spec §17: start simple, prioritising correctness,
stability, fairness and predictable latency, and optimise only after
measuring.

## Decision

### Threads

- A **kernel thread** = id, name, saved stack pointer, and its own 64 KiB
  guarded kernel stack (ADR-0009). User threads (Phase 2, with processes and
  syscalls) are kernel threads that additionally return to ring 3.
- **Context switch** (`arch/x86_64/context.rs`): a naked function that saves
  the six callee-saved registers on the outgoing stack, stores RSP, loads
  the incoming RSP and returns into it. The rest of the state is
  caller-saved, and the kernel is soft-float, so there is no FPU state to
  save; user threads add XSAVE state.
- **New threads** start through a trampoline that calls
  `thread_start(entry, arg)`: reap zombies, enable interrupts, run `entry`,
  then exit.
- **Exit and reaping:** an exiting thread becomes a zombie. The next thread
  to run (the code after every switch) drops zombies, which frees their
  stacks (unmap, invalidate the TLB entries, return the frames, recycle the
  stack slot). No thread ever frees the stack it is running on.
- **Boot thread:** the boot code's kernel stack is adopted as thread 0.
  After boot it exits like any other thread, and its stack is reclaimed.
- **Idle thread:** runs when nothing else is ready; `sti; hlt` atomically.

### Scheduling

- **Policy** (`libs/scheduler`, host-tested): FIFO ready queue, round-robin
  with a **20 ms slice** (2 ticks at 100 Hz). Timed sleeps are ordered by
  wake tick, with FIFO among equal deadlines; woken threads queue behind
  already-ready ones.
- **Preemption:** on each timer tick, sleepers whose deadline passed become
  ready. The running thread is preempted if its slice expired, or if it is
  the idle thread, and anything is ready. A thread that yields while nothing
  else is ready keeps running.
- **API:** `sched::spawn(name, fn(usize), arg)`, `yield_now`,
  `sleep_ms(ms)` (rounded up: never short), `exit`.
- Priorities, CPU affinity, SMP run queues, real-time classes and tickless
  idle are deliberately deferred until measured needs exist (§17, §51).
  `RunQueue::next_deadline` already exists for tickless idle.

### Timer

- **Local APIC timer**, periodic, vector 48, xAPIC registers mapped uncached
  via `memory::paging::map_mmio`. The legacy PIC stays masked.
- **Calibration:** one 10 ms measurement against PIT channel 2 at boot. The
  PIT is otherwise unused; this is the one sanctioned legacy-device use
  (ADR-0005), because Tier 1 x86 PCs and QEMU all provide it and CPUID
  frequency leaves are often missing. TSC-deadline mode and HPET can replace
  it later. A PIT that never counts down is a boot panic, not a hang.
- Spurious interrupts (vector 255) are ignored, as the APIC specification
  requires.

### Concurrency protocol (single CPU; on every CPU since ADR-0089)

All scheduler state is touched with interrupts disabled. `schedule` decides
under the scheduler lock, **releases the lock, then switches**. The outgoing
thread stays referenced by the ready queue, sleep queue or zombie list, so
the slot its RSP is saved into stays valid. Every kernel lock is taken with
interrupts disabled, so the timer can never preempt a lock holder into a
deadlock. SMP needs per-CPU state and a lock hand-off; that is a separate
ADR.

## Consequences

- A thread that never yields still only delays others by one slice per
  round.
- Tick granularity is 10 ms: sleeps are accurate to +1 tick.
- Thread count is bounded by memory (64 KiB + 4 KiB of page tables at most
  per thread) and by the stack region's 512 Ki slots.

## Testing

- `cargo test -p oceans-scheduler` (4 tests): round-robin order, sleep
  ordering (deadline, then arrival), woken threads queuing behind ready
  ones, and slice expiry and reset.
- Smoke boot (`oceans.test=smoke`):
  - 3 yielding workers (100 rounds each) run alongside a **spinner that never
    yields** and a sleeper. They all finish only if preemption works.
  - The sleeper's 3 × 50 ms sleeps must take at least 150 ms.
  - All test threads must be reaped (live thread count returns to its
    baseline).
- Normal boot: the boot thread exits, the idle thread reaps it and idles
  with the timer running. Checked in QEMU, debug and release, with default
  and `-cpu max` CPUs.

## Failure behaviour

Spawn returns `SpawnError::Stack` when no stack can be allocated (frames or
stack slots exhausted). A partly mapped stack is rolled back.
Missing APIC mapping or PIT calibration failure panic at boot.
