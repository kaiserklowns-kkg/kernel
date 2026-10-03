# ADR-0014: Processes, user mode and the system call ABI

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0009 (address spaces), ADR-0011 (capabilities),
  ADR-0012 (threads), ADR-0013 (IPC)
- Completes the Phase 2 exit criterion: *multiple isolated processes
  exchange IPC messages*.

## Context

Every Oceans service, driver and application runs in user mode (ADR-0002),
isolated from the kernel and from each other, with authority only from
capabilities (ADR-0006/0011). The system API must be Oceans-specific and
versioned (master spec §19), and must not expose kernel internals.

## Decision

### Process

A process = **own address space** (user half; kernel half shared) + **own
capability table** + threads (one for now).

- **User memory comes only from memory objects** mapped into the address
  space. Frames are owned by objects, never by page tables.
- **Program loading:** static ELF64 executables, parsed by `libs/elf` under
  strict rules:
  - bounds and overflow checks on every field;
  - `ET_EXEC` only, no interpreter;
  - segments inside `0x10000..0x7000_0000_0000` and not overlapping;
  - **no writable+executable segment (W^X for userspace)**;
  - the entry point must lie in an executable segment;
  - at most 256 MiB.

  Each segment becomes a zero-filled memory object.
- **Stack:** 64 KiB at `0x7fff_f000_0000` (top), unmapped below.
- **Start state:** `rdi` = number of initial handles, `rsi` = pointer to
  them (on the stack), `rdx` = an argument word. All other registers are
  zero (`oceans-abi::start`).
- **Boot modules:** the kernel receives program images as Limine modules,
  mapped read-only into the direct map.
- **Exit:**
  - `EXIT`, or a CPU exception in ring 3, ends the process. The exception
    exit code is `-128 - vector`.
  - **The capability table is closed at once**, so peers see the death
    immediately (`PeerClosed`).
  - The address space and page tables are freed when the last thread is
    reaped. The kernel checks they are not the active ones.
- **Faults never reach the kernel.** A fault in ring 3 kills the process
  with a log line (faulting address, error code) and nothing else.

### Isolation and kernel access to user memory

- User pages are `USER`. Kernel pages are supervisor-only and global, so a
  user access to kernel memory faults.
- **The kernel never dereferences user pointers.** `copy_from_user` and
  `copy_to_user` work as follows:
  1. check the range lies in the user half (with overflow checks);
  2. walk the process's own page tables, requiring user (and, for writes,
     writable) permission;
  3. validate the whole range first;
  4. copy through the direct map.

  A bad pointer is therefore `BadAddress`, never a kernel fault. **SMAP and
  SMEP stay enabled** with no `stac`/`clac` windows.

### CPU mechanics (x86_64)

- **GDT:** kernel code, kernel data, user data, user code, TSS. This is the
  order `syscall`/`sysret` require, and it is asserted at boot.
- **`syscall`/`sysret`:**
  - STAR/LSTAR/FMASK are set; FMASK clears IF, DF, TF and AC.
  - The entry stub saves the user RSP, switches to the current thread's
    kernel stack and builds a `SyscallFrame`.
  - Syscalls run **preemptible** (interrupts re-enabled).
  - Caller-saved argument registers are zeroed on return, so kernel values
    cannot leak through them.
- **TSS.RSP0** and the syscall stack are set to the next thread's kernel
  stack on every switch. CR3 is switched when the address space differs.
  The TSS is packed, so RSP0 is written as two aligned halves; the debug
  build's UB checks caught the unaligned write.
- **First entry to ring 3:** `iretq` with IF set and all registers zeroed.
- **Single CPU:** the syscall scratch slots are statics, safe because the
  entry stub runs with interrupts off until the user RSP is on the kernel
  stack. SMP moves them to per-CPU data with `swapgs`. The NMI IST needed
  around `sysret` comes with that work.

### System call ABI v1 (`libs/abi`, shared by kernel and userspace)

| # | Name | Arguments → results | Authority |
|---|---|---|---|
| 0 | `ABI_VERSION` | → 1 | none |
| 1 | `DEBUG_WRITE` | log, ptr, len (≤ 1024) | `WRITE` on a **log capability** |
| 2 | `EXIT` | code → never returns | none |
| 3 | `YIELD` | | none |
| 4 | `HANDLE_CLOSE` | handle | the handle |
| 5 | `IPC_CALL` | client, label, ptr, len (≤ 256), reply ptr, reply capacity → reply len, reply label | `SEND` |
| 6 | `IPC_RECEIVE` | server, ptr, capacity → len, label | `RECEIVE` |
| 7 | `IPC_REPLY` | label, ptr, len | the thread's pending call |

Errors are negative codes (`oceans_abi::Error`). For example, a capability
of the wrong type is reported as `WrongType` *before* any rights check.
Even logging needs a capability: there is no ambient authority. Capability
transfer through syscalls (attaching handles to messages), memory mapping,
and process creation from userspace come next (Phase 3), as ABI additions.

### Userspace

- `user/` is a separate Cargo workspace: same target as the kernel, small
  code model. Its own `.cargo/config.toml` overrides the kernel-only flags.
- `user/rt` (`oceans-rt`): entry macro, syscall wrappers, panic handler,
  formatting buffer.
- `user/ipc-test`: the Phase 2 criterion program (below). `cargo xtask`
  builds it and ships it as a boot module.
- Normal boots start no user process yet. The first real one (init and the
  service manager) is Phase 3.

### Lock discipline (now that kernel code runs preemptible in syscalls)

Every spinlock is taken with interrupts disabled. Otherwise a preempted
lock holder would deadlock the CPU. Locks are never held across blocking
IPC.

## Testing

- `cargo test -p oceans-elf` (7 tests): the typical layout, non-ELF input,
  ELF32, `ET_DYN`, `PT_INTERP`, file data out of bounds (including
  overflow), segments outside user space (including address overflow),
  W+X, overlap, entry in data, no segments, the size limit, header sanity,
  and empty segments ignored.
- `cargo test -p oceans-abi`: error codes round-trip.
- Smoke boot (`oceans.test=smoke`) starts three processes from the
  `ipc-test` module:
  - **server** (log + server end) and **client** (log + client end) do
    **1000 verified round trips**. The client then checks that a bad
    pointer gives `BadAddress`, the log handle used as an endpoint gives
    `WrongType`, a forged handle gives `InvalidHandle`, and a reply without
    a call gives `NoPendingCall`. The client exits 0, and the server sees
    `PeerClosed` and exits 0.
  - **intruder** (log only): a forged handle is rejected, then it reads
    `0xffff_ffff_8000_0000`. It is **killed by the page fault** (exit
    `-142`) while everything else runs on.
  - All three processes are then destroyed (checked with weak references):
    their address spaces, page tables, objects and tables are freed.
- Passes in debug and release, and with `-cpu max`, where SMEP, SMAP and
  UMIP are enforced.

## Alternatives considered

- **Kernel dereferencing user pointers with `stac`/`clac` and fault
  fix-ups (Linux):** faster for large copies, but needs exception tables
  and opens SMAP windows. Page-table-walk copies are simple, and they are
  correct by construction. Revisit if measurements show copies matter.
- **Position-independent executables:** needs relocation processing; static
  executables suffice until ASLR is designed.
- **`int 0x80`-style software interrupts:** slower than `syscall`, and no
  simpler once the entry stub exists.
