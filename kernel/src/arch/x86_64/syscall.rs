//! `syscall`/`sysret` entry and the first entry into ring 3.
//!
//! On `syscall` the CPU loads kernel CS/SS from STAR, masks RFLAGS with
//! SFMASK (interrupts off) and jumps to [`syscall_entry`], still on the user
//! stack. The stub saves the user RSP, switches to the current thread's
//! kernel stack, saves the user registers in a [`SyscallFrame`] and calls the
//! registered handler.
//!
//! Single CPU: the kernel stack and user-RSP scratch are plain statics,
//! valid because the stub runs with interrupts disabled until the user RSP
//! is safely on the kernel stack. SMP moves them to per-CPU data (`swapgs`).

use core::arch::{asm, naked_asm};
use core::cell::UnsafeCell;

use ::x86_64::registers::model_specific::{Efer, EferFlags, Msr};
use spin::Once;

use super::gdt::{KERNEL_CODE, KERNEL_DATA, USER_CODE, USER_DATA};

const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;

/// RFLAGS bits cleared on syscall entry: TF, IF, DF, AC.
const SYSCALL_RFLAGS_MASK: u64 = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 18);
/// RFLAGS for a thread's first entry into user mode: IF, plus reserved bit 1.
const USER_INITIAL_RFLAGS: u64 = (1 << 9) | (1 << 1);

/// User registers at `syscall`, in stack order. The handler writes results
/// into `rax` (and `rdx`); everything else is restored as the user left it.
#[repr(C)]
#[derive(Debug)]
pub struct SyscallFrame {
    /// Syscall number in, primary result out.
    pub rax: u64,
    pub rdi: u64,
    pub rsi: u64,
    /// Third argument in, secondary result out.
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
    /// User return address (set by `syscall`).
    pub rcx: u64,
    /// User RFLAGS (set by `syscall`).
    pub r11: u64,
    pub user_rsp: u64,
}

struct Scratch(UnsafeCell<u64>);

// SAFETY: accessed only by the entry stub (interrupts disabled) and by
// `set_kernel_stack` (interrupts disabled), on one CPU.
unsafe impl Sync for Scratch {}

static KERNEL_RSP: Scratch = Scratch(UnsafeCell::new(0));
static USER_RSP: Scratch = Scratch(UnsafeCell::new(0));

static HANDLER: Once<fn(&mut SyscallFrame)> = Once::new();

/// Enables `syscall` with `handler` as the dispatcher.
pub fn init(handler: fn(&mut SyscallFrame)) {
    HANDLER.call_once(|| handler);
    // SAFETY: the MSRs exist on every x86_64 CPU; the selectors match the
    // GDT layout (asserted in `gdt::init`): sysret uses STAR[63:48] + 8 for
    // SS and + 16 for CS, syscall uses STAR[47:32] and + 8.
    unsafe {
        let star = (u64::from(KERNEL_DATA) << 48) | (u64::from(KERNEL_CODE) << 32);
        Msr::new(IA32_STAR).write(star);
        Msr::new(IA32_LSTAR).write(syscall_entry as *const () as u64);
        Msr::new(IA32_FMASK).write(SYSCALL_RFLAGS_MASK);
        Efer::update(|flags| flags.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
    }
    debug_assert_eq!(USER_DATA, (KERNEL_DATA + 8) | 3);
    debug_assert_eq!(USER_CODE, (KERNEL_DATA + 16) | 3);
}

/// Stack used for syscalls and ring-3 interrupts: the running thread's
/// kernel stack. Interrupts must be disabled.
pub fn set_kernel_stack(top: u64) {
    // SAFETY: see `Scratch`.
    unsafe { KERNEL_RSP.0.get().write(top) };
    super::gdt::set_kernel_stack(super::cpu_index(), top);
}

#[unsafe(naked)]
unsafe extern "C" fn syscall_entry() {
    naked_asm!(
        "mov [rip + {user_rsp}], rsp",
        "mov rsp, [rip + {kernel_rsp}]",
        "push qword ptr [rip + {user_rsp}]",
        "push r11",
        "push rcx",
        "push r9",
        "push r8",
        "push r10",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rax",
        // 10 words on a 16-byte aligned stack top: aligned for the call.
        "mov rdi, rsp",
        "cld",
        "call {dispatch}",
        "cli",
        "pop rax",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop r10",
        "pop r8",
        "pop r9",
        "pop rcx",
        "pop r11",
        "pop rsp",
        "sysretq",
        user_rsp = sym USER_RSP,
        kernel_rsp = sym KERNEL_RSP,
        dispatch = sym dispatch,
    );
}

extern "C" fn dispatch(frame: &mut SyscallFrame) {
    let handler = HANDLER.get().expect("syscall::init registers a handler");
    handler(frame);
    // A non-canonical return address would make `sysret` fault in ring 0.
    // Return addresses come from the `syscall` instruction itself and are
    // never rewritten, but keep the invariant explicit.
    assert!(
        frame.rcx < 0x0000_8000_0000_0000,
        "non-canonical sysret target"
    );
}

/// Enters ring 3 at `entry` with stack `user_rsp` and the first three
/// argument registers set; all other registers are zeroed so no kernel
/// values leak.
///
/// # Safety
///
/// The current address space must map `entry` executable and `user_rsp`
/// writable for user mode, and the kernel stack must be set
/// ([`set_kernel_stack`]) to this thread's.
pub unsafe fn enter_user(entry: u64, user_rsp: u64, args: [u64; 3]) -> ! {
    // SAFETY: caller contract; the iretq frame selects ring 3 code and stack.
    unsafe {
        asm!(
            "push {ss}",
            "push {user_stack}",
            "push {rflags}",
            "push {cs}",
            "push {target}",
            "xor eax, eax",
            "xor ebx, ebx",
            "xor ecx, ecx",
            "xor ebp, ebp",
            "xor r8d, r8d",
            "xor r9d, r9d",
            "xor r10d, r10d",
            "xor r11d, r11d",
            "xor r12d, r12d",
            "xor r13d, r13d",
            "xor r14d, r14d",
            "xor r15d, r15d",
            "iretq",
            ss = const USER_DATA as u64,
            cs = const USER_CODE as u64,
            rflags = const USER_INITIAL_RFLAGS,
            user_stack = in(reg) user_rsp,
            target = in(reg) entry,
            in("rdi") args[0],
            in("rsi") args[1],
            in("rdx") args[2],
            options(noreturn),
        )
    }
}
