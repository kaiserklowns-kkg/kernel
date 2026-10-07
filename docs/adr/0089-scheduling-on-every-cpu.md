# ADR-0089: Scheduling on every CPU

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0088 (every CPU up), ADR-0012 (threads and
  scheduling), ADR-0013 (IPC), ADR-0014 (user mode), ADR-0044 (stopping
  processes)
- Second of two steps to SMP.

## Context

After ADR-0088 every CPU runs kernel code, but threads run on the boot
CPU only. Running them everywhere breaks four things the kernel relied
on, all of them true only on one CPU:

1. **The hand-over in a context switch.** A thread switched out goes back
   in the ready queue *before* its registers are saved. On one CPU nobody
   could pick it in between; another CPU now can.
2. **Check, register, block.** A waiter registers with an object, drops
   the object's lock and calls `block`. A waker on another CPU can come in
   between, and `wake` then found a thread "woken while not blocked" (a
   panic). The IPC direct switch had the same problem.
3. **The syscall entry.** It kept the user stack pointer and the kernel
   stack in two statics. Two CPUs entering at once would overwrite each
   other's.
4. **Kernel mappings in other TLBs.** Kernel pages are global. A freed
   kernel stack's slot, mapped again for a new thread, could still be
   translated to the old frames by a CPU that ran the old thread.

## Decision

### Per-CPU data behind `GS`

- Each CPU has a block (`arch::percpu`): its index, the running thread's
  kernel stack, and the `syscall` scratch.
- Kernel code always runs with `GS` at its CPU's block:
  - **every entry from ring 3** starts with `swapgs`: `syscall`, and
    interrupts or exceptions whose saved CS is a user one;
  - **every return to ring 3** ends with one: `sysret`, `iretq` to a user
    CS, and the first entry. Interrupts are off between the `swapgs` and
    the return.
- `cpu_index()` is a single load. `syscall` is enabled on every CPU.
- User code cannot set `GS`'s base: `WRGSBASE` is off. Loading a selector
  only changes the user-side value that `swapgs` sets aside.

### The scheduler

- **One ready queue and one lock**, with per-CPU state for each CPU: its
  running thread, its idle thread, its slice, the thread it last switched
  away from, and its exited threads.
- **Hand-over:** each thread has `on_cpu`.
  - A CPU sets it when it takes the thread, after waiting until it is
    clear.
  - The thread's last CPU clears it only once the switch away is
    complete. That is when its saved stack pointer is valid.
  - No cycle of CPUs can wait on one another. Each CPU decides its next
    thread and puts its old one back in the same locked step, so a thread
    can only be picked after its CPU has chosen what to run instead.
- **Blocking:** `blocked` (committed to wait) and a new `wake_pending`
  token change only under the scheduler lock.
  - `wake` makes a blocked thread ready. For a thread not blocked yet, it
    leaves the token.
  - `block` with the token returns at once.
  - The IPC direct switch goes straight to a server that has blocked. A
    server still on its way, on another CPU, gets the token and is woken
    the ordinary way.

  Every caller registers before it blocks and is woken once per
  registration, so a token can only belong to the block that follows.
- **Exits:** a thread that exits goes on its CPU's list. It is freed by
  the next thread on that CPU, after the switch, never on its own stack.
- **Joining:** each started CPU waits until the boot CPU runs the
  scheduler. The code on its kernel stack then becomes its idle thread.
  Its local APIC timer starts at the boot CPU's calibration, since one
  machine's APIC timers run at one rate.
- **Waking idle CPUs:** a thread made ready while another CPU idles sends
  that CPU an inter-processor interrupt, so does a sleeper woken by the
  tick, and the interrupt makes the idle CPU look at the queue.
- **Ticks:** time advances, sleepers wake and timers fire on the boot
  CPU's tick. Every CPU's own tick counts its slice.
- **Non-preemptible sections** (console output) are counted per CPU. A
  section is never preempted, so it never moves.
- **Killing** (ADR-0044) is unchanged. A target running on another CPU
  sees the mark at its next entry into the kernel, at the latest that
  CPU's next tick.

### Kernel stack slots and TLBs

- A freed kernel stack is unmapped and its frames freed at once. Its
  **slot** is reused only after every running CPU has flushed its whole
  TLB (`flush_tlb_all`, globals included) since the free.
  - Each free advances a generation.
  - Each CPU flushes on its next tick if it is behind, and records the
    generation it reached.
  - A slot is reused once the oldest of those has passed its generation.
    Until then new slots are taken: the region has room for millions.
- Nothing waits on another CPU, so a CPU spinning on a lock with
  interrupts off can never deadlock a flush.
- Nothing else in the kernel half is ever unmapped: the heap only grows,
  and MMIO and firmware mappings are permanent.
- User address spaces need nothing. A process has one thread, so it runs
  on one CPU at a time. A CPU that ran it has since loaded another page
  table, and without PCIDs that drops the non-global entries.

## Consequences

- Threads run on every CPU: user processes, drivers and the AI runtime
  use the whole machine.
- **The smoke tests** (four CPUs):
  - the IPI test;
  - four threads that never yield ran on several CPUs at once and all
    finished;
  - the whole system runs on four CPUs: every service, IPC between
    processes on different CPUs, the shell session, both boots and the
    hardware boots.
- **Limits:**
  - **One lock for the scheduler.** It is correct and simple, and fine
    for a few CPUs. Per-CPU run queues and work stealing come when they
    are measured to matter.
  - **No affinity:** a thread runs wherever a CPU is free.
  - **NMIs** have no handler (none is enabled). An NMI between an entry
    and its `swapgs` would need the usual paranoid entry.
  - **One thread per process** remains. Several would need TLB shootdown
    for user mappings too.

## Addendum: diagnosing a hang (2026-10-07)

Once, among some fifteen smoke runs since this change, a run hung: `app
install` never answered, while the rest of the system ran on. It did not
come back in nine runs after it, six of them with the CPU loaded on
purpose, and reading every wait and wake path found no way to lose a
wake-up. So that the next one can be found:

- **The diagnostic key:** Ctrl+\\ (0x1C) on the serial line makes the
  kernel log, instead of passing the byte on:
  - what each CPU runs, and how many threads are ready or asleep;
  - every live process's thread: on a CPU, blocked, holding a wake
    token, killed, answering a call.

  It logs through `klog::emergency`, so it never waits for the console
  from the interrupt handler.
- **A smoke test that times out** presses it, prints what follows, then
  prints every CPU's registers from QEMU's monitor (`info registers -a`:
  the instruction pointer, the flags, whether it halted).
- The smoke test presses the key once on every boot, to keep it working.

## Alternatives considered

- **Per-CPU run queues now:** more scalable, but load balancing and
  stealing are a design of their own. Correctness first, with one lock.
- **IPI-based TLB shootdown on every kernel unmap:** the textbook way,
  but the CPU that frees a stack often holds locks with interrupts off,
  and waiting for acknowledgements there can deadlock. Deferring reuse of
  the slot needs no waiting at all, and kernel stacks are the only kernel
  unmapping.
- **The local APIC ID as the CPU's index** (ADR-0088): an MMIO read on
  every context switch. It cannot serve the syscall entry either, which
  has no free register before the stack switch.
- **Holding the scheduler lock across the switch** (released by the
  incoming thread): it serialises every switch on the lock and is harder
  to reason about with interrupts. `on_cpu` keeps the lock's hold short.

## Checklist (master spec §48)

- **Purpose:** use every CPU.
- **Architecture:**
  - `arch::percpu` (GS blocks, `swapgs` in the entry stubs);
  - `sched` (per-CPU state, hand-over, the wake protocol, idle wake-ups);
  - `memory::paging` (stack slots that wait for flushes);
  - `smp` (CPUs join the scheduler).
- **API (kernel-internal):**
  - `sched::{enter_secondary, on_reschedule_ipi}`;
  - `arch::{send_ipi_to_cpu, start_secondary_timer, flush_tlb_all}`;
  - `paging::{tlb_tick, cpu_online}`.

  The syscall ABI is unchanged.
- **Dependencies:** none new.
- **Security:**
  - user code never sees a kernel `GS` (the swap on every boundary, with
    interrupts off where it matters);
  - a freed stack slot is never reachable through a stale translation;
  - each CPU keeps its own double-fault stack and TSS.
- **Testing:**
  - the boot self-tests: scheduler, IPIs, and threads on several CPUs at
    once;
  - the full smoke, in all modes, and `smoke-hw`, with four CPUs.
- **Failure behaviour:**
  - a CPU that does not start stays out (ADR-0088), and the others
    schedule without it;
  - a thread can never run on two CPUs at once (`on_cpu`), or be freed
    while on a CPU.
