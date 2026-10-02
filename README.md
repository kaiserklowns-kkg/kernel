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
| 2 — Kernel core | Memory, processes, scheduler, syscalls, IPC | **In progress** — frame allocator done (ADR-0008) |

Full roadmap: [docs/architecture/overview.md](docs/architecture/overview.md#roadmap).

## Quick start

Requires Rust (the pinned toolchain installs automatically), Git, QEMU and
x86_64 UEFI firmware (OVMF/edk2, bundled with QEMU on Windows and macOS).

```bash
cargo xtask limine   # fetch the Limine UEFI bootloader (once)
cargo xtask run      # build and boot in QEMU, serial log on this terminal
cargo xtask smoke    # headless boot test used by CI
cargo xtask check    # fmt + clippy + unit tests
```

Details: [docs/development/getting-started.md](docs/development/getting-started.md).

## Repository layout

```
kernel/            Oceans kernel (Rust, no_std)
  src/boot/          boot protocol adapters → BootInfo
  src/arch/x86_64/   CPU bring-up: serial, GDT/TSS, IDT, PIC
  src/memory/        physical memory discovery
libs/memory-map/   host-testable physical memory map model
libs/frame-allocator/ host-testable buddy allocator for physical frames
tools/xtask/       build, image and QEMU tooling (`cargo xtask`)
docs/              architecture, ADRs, hardware, development guides
```

Directories for services (Go), apps and UI (SvelteKit) are created when their
phase starts, per the "no empty directories" rule.
