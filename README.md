# Oceans OS

**Intelligence, Engineered.**

Oceans is an AI-native operating system built on a Rust kernel, with Go system
and AI services and a SvelteKit/Bun experience layer. It targets a deliberately
controlled set of modern hardware instead of maximum compatibility.

> Rust owns the machine. Go powers the intelligent services. SvelteKit builds
> the modern experience. Bun powers the TypeScript ecosystem. Oceans owns the
> architecture.

## Status

| Phase | Goal | State |
|---|---|---|
| 0 — Architecture | ADRs, system architecture, repo layout | **In progress** — see [docs/adr](docs/adr) |
| 1 — Boot | Boot in QEMU, `OCEANS KERNEL ONLINE` | **Done** — `cargo xtask smoke` passes |
| 2 — Kernel core | Memory, processes, scheduler, syscalls, IPC | **Done** — ADRs 0008–0014; exit criterion (isolated processes exchanging IPC) passes in `cargo xtask smoke` |
| 3 — Userspace | init, service manager, filesystem, shell, utilities | **Done** — ADRs 0015–0020; boots to an interactive shell with ps, mem, uptime, uname |
| 4 — Hardware | PCI, storage, USB, network, display | **In progress** — ADR-0021: PCI, device capabilities, a userspace virtio-blk driver; ADR-0022: crash-consistent filesystem on disk; ADR-0023: virtio-net, IPv4/UDP/ICMP stack with DHCP, sockets; ADR-0024: TCP, DNS (`nc`, `host`); ADR-0025: boot archive; ADR-0026: kernel entropy; ADR-0027: data checksums |

Full roadmap: [docs/architecture/overview.md](docs/architecture/overview.md#roadmap).

## Quick start

Requires Rust (the pinned toolchain installs automatically), Git, QEMU and
x86_64 UEFI firmware (OVMF/edk2, bundled with QEMU on Windows and macOS).

```bash
cargo xtask limine   # fetch the Limine UEFI bootloader (once)
cargo xtask run      # boot in QEMU; ends at the interactive `oceans>` shell
cargo xtask smoke    # headless boot test used by CI
cargo xtask check    # fmt + clippy + unit tests
```

Details: [docs/development/getting-started.md](docs/development/getting-started.md).

## Repository layout

```
kernel/            Oceans kernel (Rust, no_std)
  src/boot/          boot protocol adapters → BootInfo
  src/arch/x86_64/   CPU bring-up: serial, GDT/TSS, IDT, PIC
  src/memory/        frames, page tables, heap
  src/object/        kernel objects reachable by capability
  src/sched/         kernel threads, context switch, preemptive scheduler
  src/ipc/           endpoints (call/reply + capability transfer), notifications
  src/process/       processes: ELF loading, user address spaces, user faults
  src/syscall.rs     system call dispatch (ABI v1)
libs/memory-map/   host-testable physical memory map model
libs/frame-allocator/ host-testable buddy allocator for physical frames
libs/heap/          host-testable slab + page-block kernel heap
libs/capability/    host-testable capability tables, rights, revocation
libs/scheduler/     host-testable scheduling policy (run queue, sleep, slices)
libs/abi/           system call ABI (v6) shared by kernel and userspace
libs/acpi/          validating ACPI parser (RSDP, RSDT/XSDT, MADT)
libs/elf/           strict ELF64 executable parser
user/               userspace: oceans-rt runtime (+ heap), init, fs (+ fs-proto), shell, utils, services, tests
config/             services.conf (normal boots), services-smoke.conf (smoke tests)
tools/xtask/       build, image and QEMU tooling (`cargo xtask`)
docs/              architecture, ADRs, hardware, development guides
```

Directories for services (Go), apps and UI (SvelteKit) are created when their
phase starts, per the "no empty directories" rule.
