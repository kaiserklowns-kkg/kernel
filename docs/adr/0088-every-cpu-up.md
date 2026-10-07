# ADR-0088: Every CPU up

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0003 (Limine), ADR-0009 (kernel address space),
  ADR-0012 (scheduling), ADR-0017 (local APIC)
- First of two steps to SMP; the second (ADR-0089) schedules threads on
  every CPU, and replaces the `cpu_index` and idle loop described here.

## Context

Oceans ran on one CPU, while every Tier 1 machine has several.

Running threads on all of them touches the deepest parts of the kernel:
- the scheduler's state;
- the IPC wake-up protocol, whose "check, register, block" is atomic only
  because interrupts are off on the only CPU;
- the syscall entry's scratch statics;
- kernel mappings that other CPUs may still cache in their TLBs.

Before any of that can be tested, the CPUs must be running kernel code at
all.

The bootloader (Limine, with its MP request) starts every CPU and parks
each in a loop. Each one is on Limine's stack, in Limine's page tables,
in **bootloader-reclaimable memory**, which the kernel frees for its own
use early in the boot.

## Decision

- **Limine's MP request** reports the boot CPU's local APIC ID and the
  others. `boot::secondary_cpus()` lists them (at most `arch::MAX_CPUS`,
  64, in all). `boot::start_secondary(i, entry, arg)` sends one on its
  way. The rest of the kernel never sees Limine's types.
- **Starting them, before bootloader memory is reclaimed:** `smp::start`
  takes one CPU at a time, and each gets a second for each of two steps.
  1. On Limine's stack the CPU:
     - turns on the boot CPU's protections (NX first: the kernel's page
       tables use it; then WP, PGE, SMEP, SMAP, UMIP);
     - activates the kernel's page tables (they map the direct map, so
       Limine's stack stays reachable until the next step);
     - switches to a kernel stack of its own.

     It is now off bootloader memory and says so.
  2. It then loads its own GDT and TSS (with its own double-fault stack
     from the kernel-stack area), the shared IDT and its local APIC, and
     reports in.

  A CPU that never leaves bootloader memory makes the kernel keep that
  memory: it may still be reading it. One that leaves but does not finish
  is left out.
- **Per CPU in the arch layer:**
  - a GDT and TSS per CPU (`gdt::init_cpu`; `set_kernel_stack` writes the
    calling CPU's TSS);
  - `register_cpu` and `cpu_index`, which looks the CPU up by its local
    APIC ID;
  - the local APIC mapping is shared (every CPU sees its own APIC at the
    same address), and `apic::init` enables the caller's.
- **Inter-processor interrupts:** vector 51 (`arch::send_ipi`,
  `set_ipi_handler`). Here they are only counted. ADR-0089 uses them to
  wake idle CPUs and to flush TLBs.
- **The started CPUs then wait for interrupts.** Threads are scheduled
  on the boot CPU only, as before.

## Consequences

- Every CPU of a Tier 1 machine runs kernel code, set up like the boot
  CPU.
- The boot log says how many CPUs are online.
- The smoke tests run QEMU with four CPUs:
  - the boot self-test interrupts each of the other three and checks
    that each one, and not the boot CPU, took it;
  - `smoke-hw` checks the count on the production configuration.
- **Not yet:**
  - no thread runs on the other CPUs, so the machine is no faster yet;
  - the syscall MSRs are set on the boot CPU only, since no user code
    runs elsewhere yet;
  - `cpu_index` reads the local APIC ID, which costs an MMIO read.
    ADR-0089 moves per-CPU state behind `GS` (with `swapgs` on every entry
    from ring 3), as the syscall path needs anyway.
- **Limits:** xAPIC only. A machine whose firmware leaves the CPUs in
  x2APIC mode (more than 255 CPUs) is not a Tier 1 machine.

## Alternatives considered

- **Starting the CPUs ourselves** (INIT, then SIPI, with a real-mode
  trampoline below 1 MiB). It is the textbook way and independent of the
  bootloader. But it needs a page of low memory kept free, 16- and 32-bit
  code, and careful timing. Limine already does it, correctly, on every
  machine it boots, and the boot protocol is already ours to depend on
  (ADR-0003).
- **Letting them run before the kernel's page tables exist:** impossible
  to do safely, because their stacks would be freed under them.
- **Scheduling on every CPU in the same step:** too much change at once to
  test well. This step can be verified on its own.

## Checklist (master spec §48)

- **Purpose:** every CPU running kernel code, ready for SMP scheduling.
- **Architecture:**
  - `boot` (Limine's MP request behind a neutral API);
  - `smp` (starting, waiting, the self-test);
  - `arch` (per-CPU GDT and TSS, the IDT load, the local APIC, IPIs, CPU
    indices).
- **API (kernel-internal):**
  - `boot::{secondary_cpus, start_secondary, bsp_lapic_id}`;
  - `arch::{MAX_CPUS, register_cpu, cpu_index, init_secondary,
    set_cpu_protections, send_ipi, set_ipi_handler}`;
  - `smp::{start, count, self_test}`.
- **Dependencies:** none new (Limine's MP request is in the crate the
  kernel already uses).
- **Security:**
  - every CPU has the boot CPU's protections before it runs any kernel
    page table;
  - each CPU has its own double-fault stack;
  - no CPU runs user code yet.
- **Testing:**
  - the boot self-test: an IPI answered by each other CPU, and only by
    it;
  - smoke and `smoke-hw` with four CPUs;
  - the count in the log.
- **Failure behaviour:**
  - a CPU that does not start is reported and left out;
  - one that may still be in bootloader memory makes the kernel keep
    that memory (logged);
  - no MP response means one CPU, as before.
