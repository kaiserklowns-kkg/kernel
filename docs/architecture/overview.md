# Oceans architecture overview

Status: living document. Decisions referenced here are recorded in
[ADRs](../adr/README.md); this page summarises how they fit together.

## Layers

```
 Applications (native, SvelteKit)            ─┐
 Oceans SDK / versioned Oceans APIs           │  Oceans Experience
 Oceans services (Go)  ·  AI Runtime (Go)     │  (userspace, unprivileged)
 Oceans Runtime (Rust): service manager,      │
   permission broker, IPC client libraries   ─┘
 ─────────────────────── syscall boundary (versioned) ───────────────────────
 Oceans Kernel (Rust): memory, processes, scheduler, IPC, capabilities,
   interrupt routing, minimal boot-critical drivers
 Oceans HAL: arch layer (x86_64, aarch64) + platform abstraction
 Hardware (Tier 1 targets, ADR-0005)
```

Rules that follow from this picture:

1. **Nothing above the syscall boundary sees kernel internals.** Applications
   and services use versioned Oceans APIs (ADR-0002).
2. **Every privileged operation is a capability check.** There is no ambient
   authority, for apps *or* AI agents (ADR-0006, ADR-0007).
3. **Go, JavaScript and SvelteKit never run in the kernel**, and the system
   must boot and be administrable without the web UI layer (ADR-0001).

## Kernel shape

Microkernel-leaning hybrid (ADR-0002): the kernel owns address spaces,
scheduling, IPC, capabilities and interrupt routing. Drivers, filesystems and
the network stack are userspace services by default. A small set of
boot-critical paths (early console, timer, interrupt controller) stays in the
kernel. Any further in-kernel driver needs an ADR with measurements.

### Current boot flow

```
UEFI firmware → Limine → kernel_entry (boot/limine.rs)
  → arch::early_init      serial console
  → BootInfo              protocol-neutral memory map, direct-map offset, cmdline
  → kernel_main
      → arch::init        GDT + TSS (IST for #DF), IDT (32 exception stubs), PIC masked
      → memory::init      memory map → frame allocator → own page tables (W^X, NX, SMEP…)
      → switch_stack      onto a guarded kernel stack
  → kernel_main_on_kernel_stack
      → reclaim bootloader memory
      → sched::init       boot code becomes thread 0; idle thread; APIC timer (100 Hz)
      → process::init     syscall/sysret, ring-3 fault handling
      → acpi + console    MADT → I/O APIC routes COM1 IRQ → console input (ADR-0017)
      → self-tests        only with oceans.test=smoke (memory, heap, capabilities, scheduler, IPC,
                          user processes from the ipc-test boot module)
      → "OCEANS KERNEL ONLINE"
      → init (first user process, ADR-0016): reads services.conf,
        starts and supervises services with only their declared capabilities
      → boot thread waits; reports if init ever exits
```

Source map: `kernel/src/boot` is the only code that knows Limine;
`kernel/src/arch/x86_64` is the only code that knows x86. `kernel_main` and
`memory` are architecture- and protocol-neutral.

## Language boundaries (ADR-0001)

| Layer | Language | Talks to the layer below via |
|---|---|---|
| Kernel, HAL, drivers, Oceans Runtime | Rust | — / syscalls |
| System services, AI Runtime | Go | Oceans System API (IPC), generated bindings |
| The desktop and every app on the device (ADR-0105) | Rust (`oceans-draw`, `oceans-ui`), or Go windows | Oceans Core and the window protocol (IPC) |
| The web experience, from a paired browser | SvelteKit + TypeScript (Bun) | Oceans APIs via the bridge |

## AI in the system (ADR-0007)

The AI Runtime is an ordinary userspace service. Agents act only through tools,
every tool call is mediated by the permission broker, and sensitive actions
require explicit user approval. All agent activity is logged and surfaced in
AI Center.

```
Agent → tool call → Permission broker ─(needs approval)→ User prompt
                          │ granted capability
                          ▼
                    Oceans System API → service / kernel
```

## Roadmap

| Phase | Deliverable | Exit criterion |
|---|---|---|
| 0 Architecture | ADRs 0001–0007, this document, repo layout | ADRs accepted or explicitly left Proposed with owners |
| 1 Boot | Limine boot, serial log, GDT/IDT, memory discovery, panic path | `cargo xtask smoke` passes in CI |
| 2 Kernel core | frame allocator, paging, kernel heap, processes/threads, scheduler, syscalls, IPC | multiple isolated processes exchange IPC messages |
| 3 Userspace | init, service manager, filesystem, shell, utilities | interactive shell in QEMU |
| 4 Hardware | NVMe, USB (xHCI), Ethernet, input, display, audio | Tier 1 QEMU devices + first real machine |
| 5 Oceans Runtime | system API, permissions, app lifecycle, package model | sandboxed app with declared permissions |
| 6 AI Runtime | Go AI service, model gateway, agent runtime, tools | agent completes a permission-gated task |
| 7 UI | desktop, launcher, Settings, Store, AI Center, permission dialogs | daily-usable desktop session |
| 8 Developer platform | SDK, templates, toolchains | third-party app built with the SDK |
| 9 Hardware validation | compatibility matrix | Tier 1 list published |
| 10 Alpha | all of the above, updates, diagnostics | alpha release |

Phases 1 to 9 are done and Phase 10 is in progress: see the phase table
in the [README](../../README.md#status).
