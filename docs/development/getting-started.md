# Getting started

## Prerequisites

| Tool | Why | Notes |
|---|---|---|
| Rust (rustup) | everything | `rust-toolchain.toml` installs the pinned stable toolchain and `x86_64-unknown-none` target |
| Git | fetching Limine | |
| QEMU ≥ 8 | running the kernel | Windows: install from qemu.org (includes UEFI firmware); Ubuntu: `qemu-system-x86 ovmf`; macOS: `brew install qemu` |

`cargo xtask` finds QEMU on `PATH` or in `C:\Program Files\qemu`, and the
firmware next to it or in the usual Linux locations. Override with
`OCEANS_QEMU` and `OCEANS_OVMF`.

## Commands

```bash
cargo xtask limine          # one-time: clone Limine v9.x binaries into build/limine
cargo xtask build           # build the kernel
cargo xtask image           # build + assemble build/esp (EFI system partition)
cargo xtask run             # boot in QEMU; serial console on this terminal (Ctrl+A X to quit)
cargo xtask smoke           # headless boot; passes when the kernel prints OCEANS KERNEL ONLINE
cargo xtask check           # rustfmt, clippy (host + kernel), unit tests
```

Add `--release` to `build`, `image`, `run` or `smoke` for an optimised kernel.

## Expected output

```
[INFO ] kernel: Oceans 0.1.0 on x86_64
[INFO ] kernel: command line: ""
[DEBUG] arch::x86_64::gdt: GDT and TSS loaded
[DEBUG] arch::x86_64::interrupts: IDT loaded with 32 exception handlers
[DEBUG] arch::x86_64::pic: legacy PIC remapped to vectors 32..48 and masked
[DEBUG] memory: 0x0000000000000000..0x000000000009f000      636 KiB usable
...
[INFO ] memory: 202 MiB usable (...), ... regions
[INFO ] memory: physical memory direct map at 0xffff800000000000
[INFO ] arch::x86_64::interrupts: breakpoint at 0x..., resuming
[INFO ] kernel: OCEANS KERNEL ONLINE
```

## Rules for every change

From the master spec §48–52: define purpose, API, security implications,
tests and failure behaviour before large features; record architectural
decisions as ADRs; nothing is done until it builds, passes `cargo xtask check`
and the QEMU smoke test.
