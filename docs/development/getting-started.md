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

## Expected output (normal boot)

```
[INFO ] kernel: Oceans 0.1.0 on x86_64
[INFO ] kernel: command line: ""
[INFO ] memory: 201 MiB usable (51630 pages of 4 KiB), 47 MiB reclaimable, ...
[INFO ] memory: physical memory direct map at 0xffff800000000000
[INFO ] memory::frames: frame allocator: 200 MiB free in 51278 frames, metadata 768 KiB at 0x1780000
[INFO ] arch::x86_64::cpu: protections: NX WP PGE
[INFO ] memory::paging: kernel address space active: root 0x..., direct map 251 MiB using pages up to 2 MiB
[INFO ] memory::frames: reclaimed 47 MiB of bootloader memory; 248 MiB free
[INFO ] kernel: OCEANS KERNEL ONLINE
[INFO ] process::init: init started with 6 boot modules
[INFO ] syscall: [init] init: 2 services in services.conf
[INFO ] syscall: [init/echo] echo: ready
[INFO ] syscall: [init] init: started echo
[INFO ] syscall: [init] init: started hello
[INFO ] syscall: [init/hello] hello: echo replied "HELLO, OCEANS"
[INFO ] syscall: [init] init: hello exited with 0
```

Services are configured in `config/services.conf` (ADR-0016). Try `ps`, `mem`,
`uptime` and `uname` (ADR-0020).
The last service is the shell: type `help` at the `oceans>` prompt. Programs
get only the authority you list, e.g. `run hello-client log use:echo`
(ADR-0018). Files: `ls /bin`, `mkdir /docs`, `write /docs/a hi`, `cat /docs/a`
(ADR-0019). Files live on disk (ADR-0022): `cargo xtask run` attaches
`build/disk.img` (8 MiB, blank on first run, formatted by the filesystem and
kept across boots; delete it to start over). File contents are durable once
the command that wrote them finishes; `sync` forces a commit. Devices:
`run lspci out devices` (ADR-0021). Network (ADR-0023): QEMU's user
network gives the guest 10.0.2.15 by DHCP; try `run ifconfig out use:net` and
`run ping out use:net -- 10.0.2.2`; TCP and DNS (ADR-0024):
`run host out use:net -- example.com` and `run nc out use:net -- example.com 80`; HTTP (ADR-0028):
`run fetch out use:net -- http://example.com/`. The QEMU window shows the same console on the framebuffer and takes keyboard
input (ADR-0029). Quit QEMU with Ctrl+A, X in the terminal, or close the window.
`cargo xtask smoke` adds `oceans.test=smoke` to the command line: the kernel
then also runs its self-tests and exits QEMU with the result. Normal boots run
no tests. Pass `-cpu max` to QEMU to exercise SMEP/SMAP/UMIP and 1 GiB pages.

## Rules for every change

From the master spec §48–52: define purpose, API, security implications,
tests and failure behaviour before large features; record architectural
decisions as ADRs; nothing is done until it builds, passes `cargo xtask check`
and the QEMU smoke test.
