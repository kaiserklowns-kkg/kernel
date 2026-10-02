# Reference systems: Redox and Linux compared with Oceans

Status: research note, 2026-10-03. Input for the Phase 2 ADRs. References are
studied, not copied (master spec §3, ADR-0002).

Sources (shallow clones outside this repo, in `C:/src/refs/`):

| Repo | Revision | License | What we may do |
|---|---|---|---|
| redox-kernel | `b68957d` (2026-09-27) | MIT | study; borrow code only with attribution |
| redox (build system) | `b3f371c` (2026-09-26) | MIT | study; borrow code only with attribution |
| linux | 7.3-rc5, `5d144c294` | GPL-2.0 | **study only; never copy code into Oceans** |

## 1. Kernel side by side

| Topic | Redox kernel | Oceans (current / planned) | Verdict for Oceans |
|---|---|---|---|
| Toolchain | pinned nightly, custom target JSONs, needs `nasm` | stable Rust, built-in `x86_64-unknown-none`, no external assembler (ADR-0004) | keep ours |
| Boot | own bootloader; `KernelArgs` struct must stay in sync with it | Limine behind `BootInfo` (ADR-0003) | keep ours; Redox shows the cost of a private boot ABI |
| Architectures | x86_64, i586, aarch64, riscv64 | x86_64, then aarch64 (ADR-0005) | keep the narrow scope |
| Frame allocator | buddy allocator, 11 orders, per-NUMA free lists, a `PageInfo` per frame (refcount with CoW/shared bits) | none yet (Phase 2) | **adopt the idea:** buddy allocator + per-frame metadata. Start without NUMA |
| Kernel heap | `linked_list_allocator` in a fixed 1 MiB region | none yet | build on the frame allocator so the heap can grow; no fixed cap |
| Address-space layout | kernel at −2 GiB, physmap at `0xffff800000000000`, heap just below the kernel | kernel at −2 GiB, direct map from Limine (`0xffff800000000000` in QEMU) | already similar; fix the full layout in the Phase 2 ADR |
| Mappings | `AddrSpace` + "grants", each with a `Provider` (Allocated, Shared, PhysBorrowed, External, FmapBorrowed) | — | **adopt:** typed mapping records. Here each record is backed by a capability |
| Processes | `Context` struct; process management through the `proc:` scheme; no fork/exec syscall | — | **adopt:** no fork. Spawn by building a new address space through explicit APIs |
| Scheduler | weighted virtual-time, 40 priority levels, per-CPU run queues, work stealing | — | start with single-core round-robin (§17); this is the target design for later |
| Syscalls | ~31 syscalls, file-shaped (read/write/openat/fmap…) | — | keep the count small, but object-shaped: syscalls take capability handles, not paths |
| Resource naming | "schemes": every resource is an fd; `openat` relative to an fd; no global namespace in the kernel | no global namespace in the kernel (ADR-0006) | **agree** on no global namespace. Kernel objects are typed, not file-like |
| IPC | io_uring-style SQE/CQE packets; direct switch to the server; zero-copy borrowing of user buffers; fd passing | message passing + shared memory + notifications (ADR-0002) | **adopt:** direct switch + zero-copy + handle passing. Benchmark IPC from day one (Redox found a HashMap lookup cost >3%) |
| Security | fds work like capabilities, but `uid == 0` checks still guard irq/memory/scheme creation; bootstrap gets root fds | capabilities only, no ambient root (ADR-0006) | **diverge:** this hybrid is exactly what Oceans avoids |
| Driver hardware access | `memory:physical`, `irq:` and `memory:zeroed?phys_contiguous` paths; port I/O via a per-process IOPL flag | — | same idea expressed as capabilities: MmioRange, Irq, DmaBuffer, IoPortRange (individual ports, not IOPL) |
| Locking | compile-time lock ordering (levels L0–L5, `CleanLockToken`) | `spin` locks, no ordering yet | **adopt** before SMP; cheap and catches deadlocks at compile time |
| Lint policy | clippy warns on `indexing_slicing`, `unwrap_used`, `arithmetic_side_effects` | `-D warnings` | **adopt** these three lints for kernel code (see §43, no panics in production paths) |
| Logging | macros → serial + in-memory ring buffer readable via `sys:log` | macros → serial | **adopt** a ring buffer once a heap exists, readable through a CLI tool (§42) |
| Tests | ~49 unit tests; QEMU boot tests | host unit tests + QEMU smoke test in CI | keep ours; Redox's CI only builds and lints the kernel |
| Code health | 2–3k-line files, stale comments ("round-robin"), two buddy allocators side by side, `static_mut_refs` FIXMEs | small modules | lesson: keep files focused and docs synchronised with code |

## 2. Build system and userspace assembly

| Topic | Redox | Oceans takeaway |
|---|---|---|
| Image definition | declarative TOML: `include` layering, `[packages]`, inline `[[files]]`, users and groups | **adopt** for the Phase 3 image config (`config/*.toml` → `cargo xtask image`) |
| Installer | one tool formats the filesystem and installs packages | adopt later; for now `xtask` copies files into an ESP |
| Host requirements | Podman + large Debian package list + FUSE + cross gcc/rust prefix | **avoid.** Oceans must build with only Rust + QEMU on Windows, Linux and macOS |
| Service startup | `/usr/lib/init.d/NN_name` (numeric order) + TOML units with `requires_weak` and `.target` sync points | adopt the declarative TOML units, using dependencies instead of numeric ordering. Services also declare their capabilities |
| Early drivers | `pcid` and others shipped in an initfs | same approach: a minimal initfs with init, PCI and storage drivers |
| Packages | recipe TOML (source: git/tar+blake3/path; build templates); `pkgar` archives signed with ed25519 | **adopt:** content hashes (blake3) + ed25519 signatures. Add declared permissions (§28, §41) |
| Per-user access | `login_schemes.toml` lists which schemes each user may open | the Oceans equivalent is permission-broker policy (ADR-0006) |
| QEMU matrix | make variables: `disk=nvme|virtio|…`, `net=…`, `gpu=…`, `gdb=yes` | **adopt** as `cargo xtask run --disk nvme --net virtio --gdb` |
| CI | boots a test image, then reads test results back off the disk | adopt in Phase 3; the smoke test covers Phase 1–2 |

## 3. Linux: what to use it for

Linux is the hardware knowledge base, not an architecture model. Relevant
areas for the Tier 1 device classes (ADR-0005):

| Need | Where in Linux 7.3 |
|---|---|
| NVMe | `drivers/nvme/host` (23 files) |
| USB xHCI | `drivers/usb/host/xhci*` (36 files) |
| virtio (net, blk, gpu) | `drivers/virtio`, `drivers/net/virtio_net.c`, `drivers/block/virtio_blk.c` |
| Ethernet | `drivers/net/ethernet/intel/e1000e`, `drivers/net/ethernet/realtek` |
| APIC / x2APIC | `arch/x86/kernel/apic` |
| ACPI, UEFI | `drivers/acpi`, `drivers/firmware/efi` |
| Wi-Fi (future research) | `drivers/net/wireless/intel/iwlwifi` (294 files) |
| GPU (future research) | `drivers/gpu/drm/{i915,xe,amd}` (thousands of files: a deliberate Tier decision) |
| Sandboxing ideas | `security/landlock` |
| Rust-in-kernel patterns | `rust/kernel` |

Rule: read Linux for *device behaviour and quirks* (register sequences, errata,
timeouts). Implement from the public spec (NVMe, xHCI and virtio are all public
specs). Code-review checklist: no GPL-derived code.

Note: the Linux tree has case-colliding paths (e.g. `xt_DSCP.c` vs
`xt_dscp.c`) that cannot coexist on Windows' case-insensitive filesystem. This
does not matter for reading drivers.

## 4. Decisions this feeds into (Phase 2 ADRs)

1. **ADR-0008 Physical memory** (done): buddy allocator + per-frame metadata
   (refcount, state); built over `libs/memory-map`; host-tested.
2. **ADR-0009 Address-space layout** (done; heap: ADR-0010): lower half user, −2 GiB kernel, direct
   map, growable heap region, guard pages.
3. **ADR-0011 Kernel objects & capabilities:** typed objects (AddressSpace,
   Thread, Endpoint, Notification, MmioRange, Irq, DmaBuffer), per-process
   capability table, rights bits, delegation over IPC. No uid in the kernel.
4. **ADR-0012 IPC:** synchronous call/reply on endpoints with direct switch,
   capability transfer, shared-memory channels for bulk data; latency
   benchmarks in CI.
5. **Kernel lint policy:** enable `clippy::indexing_slicing`, `unwrap_used`,
   `arithmetic_side_effects` for `kernel/`; add compile-time lock levels
   before SMP.
