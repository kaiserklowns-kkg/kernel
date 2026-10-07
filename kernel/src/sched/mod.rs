//! Kernel threads and preemptive scheduling (ADR-0012, ADR-0089).
//!
//! Mechanism here, policy in `oceans-scheduler`: round-robin with a fixed
//! time slice, driven by each CPU's local APIC timer, plus timed sleep.
//!
//! **On every CPU** (ADR-0089). One ready queue and one lock for all CPUs;
//! each CPU has its own running thread, idle thread, time slice and exited
//! threads. Three rules make that safe:
//! - **Hand-over.** A thread switched out is put back (in the ready queue,
//!   or wherever it waits) before its registers are saved, so another CPU
//!   may pick it while it is still leaving. `on_cpu` says it is still on
//!   a CPU: set when a CPU takes it, cleared by that CPU only after the
//!   switch away from it is complete. A CPU that picks a thread waits until
//!   it is free before loading its registers.
//! - **Blocking.** Waiters register with an object, release its lock, then
//!   call [`block`]: a waker on another CPU may come in between. A wake of a
//!   thread that has not blocked yet leaves a token (`wake_pending`), and
//!   its [`block`] then returns at once. `blocked` (committed to block) and
//!   the token change only under the scheduler lock, so exactly one of the
//!   two happens.
//! - **Exits.** A thread that exits is freed (its stack with it) only by
//!   the next thread on the same CPU, after the switch away from it.
//!
//! A thread made ready while some CPU idles wakes that CPU with an
//! inter-processor interrupt. Time advances on the boot CPU's tick; every
//! CPU's tick counts its own slice.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use oceans_scheduler::{RunQueue, TimeSlice};
use spin::Mutex;

use crate::arch::MAX_CPUS;
use crate::ipc::endpoint::ReplyToken;
use crate::memory::paging::{self, KernelStack, MapError};
use crate::process::Process;
use crate::{arch, klog, time};

/// Ticks a thread may run before another ready thread gets the CPU
/// (20 ms at 100 Hz).
const QUANTUM_TICKS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(u64);

pub struct Thread {
    id: ThreadId,
    name: &'static str,
    /// Stack pointer while the thread is not running. Written by
    /// `switch_context` when the thread is switched out, read when it is
    /// switched in; `on_cpu` orders the two across CPUs.
    saved_rsp: UnsafeCell<u64>,
    /// Committed to wait in [`block`] for a [`wake`]: set and cleared under
    /// the scheduler lock. Guards against double wake-ups, which would
    /// queue a thread twice.
    blocked: AtomicBool,
    /// A [`wake`] came before the thread blocked: its next [`block`]
    /// returns at once. Changed under the scheduler lock.
    wake_pending: AtomicBool,
    /// Running on a CPU, or still being switched away from (see the module
    /// docs).
    on_cpu: AtomicBool,
    /// Set by [`interrupt`]: the thread must not block or sleep any more;
    /// its process is being killed (ADR-0044).
    interrupted: AtomicBool,
    /// Page-table root the thread runs in: the kernel's for kernel threads,
    /// its process's for user threads.
    root: u64,
    /// The process a user thread belongs to (keeps its address space and
    /// capabilities alive while the thread exists).
    process: Option<Arc<Process>>,
    /// Call received with `IPC_RECEIVE`, answered by `IPC_REPLY`.
    pending_call: Mutex<Option<ReplyToken>>,
    /// The thread's stack; freed when the thread is reaped.
    stack: KernelStack,
}

impl Thread {
    pub fn process(&self) -> Option<&Arc<Process>> {
        self.process.as_ref()
    }

    pub fn pending_call(&self) -> &Mutex<Option<ReplyToken>> {
        &self.pending_call
    }
}

// SAFETY: `saved_rsp` is written only by the CPU switching the thread out
// and read only by a CPU switching it in after `on_cpu` showed the switch
// out complete (Acquire/Release), never concurrently.
unsafe impl Sync for Thread {}
unsafe impl Send for Thread {}

impl Drop for Thread {
    fn drop(&mut self) {
        klog::debug!("thread {} ({}) reaped", self.id.0, self.name);
        // Processes have one thread for now, so its reaping ends the
        // process's use of its address space. Reaping runs after a switch,
        // so the space is no longer active.
        if let Some(process) = &self.process {
            process.release_address_space();
        }
        LIVE_THREADS.fetch_sub(1, Ordering::Relaxed);
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static LIVE_THREADS: AtomicUsize = AtomicUsize::new(0);

/// One CPU's part of the scheduler.
struct Cpu {
    current: Option<Arc<Thread>>,
    idle: Option<Arc<Thread>>,
    /// The thread this CPU just switched away from: its `on_cpu` is
    /// cleared once the switch is complete.
    previous: Option<Arc<Thread>>,
    /// Exited threads, freed by the next thread to run on this CPU.
    zombies: Vec<Arc<Thread>>,
    slice: TimeSlice,
}

impl Cpu {
    const fn new() -> Self {
        Self {
            current: None,
            idle: None,
            previous: None,
            zombies: Vec::new(),
            slice: TimeSlice::new(QUANTUM_TICKS),
        }
    }

    fn is_idle(&self) -> bool {
        match (&self.current, &self.idle) {
            (Some(current), Some(idle)) => Arc::ptr_eq(current, idle),
            _ => false,
        }
    }
}

struct Scheduler {
    queue: RunQueue<Arc<Thread>>,
    cpus: [Cpu; MAX_CPUS],
}

impl Scheduler {
    /// Makes `thread` ready, and wakes an idle CPU (other than the
    /// caller's) to run it.
    fn make_ready(&mut self, thread: Arc<Thread>) {
        self.queue.push_ready(thread);
        let me = arch::cpu_index();
        if let Some(idle) = (0..MAX_CPUS).find(|&cpu| cpu != me && self.cpus[cpu].is_idle()) {
            arch::send_ipi_to_cpu(idle);
        }
    }
}

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler {
    queue: RunQueue::new(),
    cpus: [const { Cpu::new() }; MAX_CPUS],
});

/// The scheduler runs on the boot CPU: the others may join.
static STARTED: AtomicBool = AtomicBool::new(false);

/// The stack the boot code switched to, adopted as the boot thread's.
static BOOT_STACK: Mutex<Option<KernelStack>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// No memory for the thread's stack.
    Stack(MapError),
}

/// Hands the scheduler the stack the boot code is about to run on.
pub fn set_boot_stack(stack: KernelStack) {
    *BOOT_STACK.lock() = Some(stack);
}

/// A thread for code already running on `stack` (it becomes the thread).
fn adopt(name: &'static str, stack: KernelStack) -> Arc<Thread> {
    Arc::new(Thread {
        id: next_id(),
        name,
        // Filled in by the first switch away from it.
        saved_rsp: UnsafeCell::new(0),
        blocked: AtomicBool::new(false),
        wake_pending: AtomicBool::new(false),
        on_cpu: AtomicBool::new(true),
        interrupted: AtomicBool::new(false),
        root: paging::kernel_root(),
        process: None,
        pending_call: Mutex::new(None),
        stack,
    })
}

/// Turns the running boot code into the first thread, creates the boot
/// CPU's idle thread and starts preemption.
pub fn init() {
    let stack = BOOT_STACK
        .lock()
        .take()
        .expect("boot code hands over its stack first");
    let boot = adopt("boot", stack);
    let idle = new_thread("idle", idle_main, 0, paging::kernel_root(), None)
        .unwrap_or_else(|err| panic!("cannot create the idle thread: {err:?}"));

    arch::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        let cpu = &mut scheduler.cpus[0];
        cpu.current = Some(boot);
        cpu.idle = Some(idle);
    });
    arch::set_after_device_interrupt(on_device_interrupt);
    arch::start_timer(time::HZ, on_timer_tick);
    STARTED.store(true, Ordering::Release);
    arch::enable_interrupts();
    klog::info!(
        "scheduler running: round-robin, {} ms slice",
        u64::from(QUANTUM_TICKS) * 1000 / u64::from(time::HZ)
    );
}

/// Joins the scheduler on another CPU (ADR-0089), once the boot CPU runs
/// it: the code running on `stack` becomes this CPU's idle thread. Never
/// returns.
pub fn enter_secondary(stack: KernelStack) -> ! {
    while !STARTED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    let idle = adopt("idle", stack);
    arch::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        let cpu = &mut scheduler.cpus[arch::cpu_index()];
        cpu.current = Some(idle.clone());
        cpu.idle = Some(idle);
    });
    arch::start_secondary_timer();
    idle_main(0);
    unreachable!("the idle loop returned")
}

/// Starts a kernel thread running `entry(arg)`. It exits when `entry` returns.
pub fn spawn(name: &'static str, entry: fn(usize), arg: usize) -> Result<ThreadId, SpawnError> {
    let thread = new_thread(name, entry, arg, paging::kernel_root(), None)?;
    let id = thread.id;
    arch::without_interrupts(|| SCHEDULER.lock().make_ready(thread));
    Ok(id)
}

/// Starts the first thread of `process`, entering user mode at `entry` with
/// stack `user_rsp` and `args` in the first three argument registers.
pub fn spawn_user(
    name: &'static str,
    process: Arc<Process>,
    entry: u64,
    user_rsp: u64,
    args: [u64; 3],
) -> Result<ThreadId, SpawnError> {
    struct UserStart {
        entry: u64,
        user_rsp: u64,
        args: [u64; 3],
    }
    fn user_thread_main(arg: usize) {
        // SAFETY: `arg` is the `Box<UserStart>` leaked below, used once.
        let start = unsafe { alloc::boxed::Box::from_raw(arg as *mut UserStart) };
        let UserStart {
            entry,
            user_rsp,
            args,
        } = *start;
        // SAFETY: the scheduler switched to this thread's process address
        // space (which maps `entry` and the stack for user mode, see
        // `process::spawn`) and set this thread's kernel stack.
        unsafe { arch::enter_user(entry, user_rsp, args) }
    }

    let root = process.root();
    let start = alloc::boxed::Box::new(UserStart {
        entry,
        user_rsp,
        args,
    });
    let arg = alloc::boxed::Box::into_raw(start) as usize;
    let thread = match new_thread(name, user_thread_main, arg, root, Some(process)) {
        Ok(thread) => thread,
        Err(err) => {
            // SAFETY: the thread was not created, so we still own `arg`.
            drop(unsafe { alloc::boxed::Box::from_raw(arg as *mut UserStart) });
            return Err(err);
        }
    };
    let id = thread.id;
    if let Some(process) = &thread.process {
        process.attach_thread(Arc::downgrade(&thread));
    }
    arch::without_interrupts(|| SCHEDULER.lock().make_ready(thread));
    Ok(id)
}

/// Gives the CPU to the next ready thread, if any.
pub fn yield_now() {
    arch::without_interrupts(|| schedule(Reason::Yield));
}

/// Blocks the current thread for at least `ms` milliseconds, or until it
/// is interrupted.
pub fn sleep_ms(ms: u64) {
    let ticks = time::ms_to_ticks(ms).max(1);
    arch::without_interrupts(|| {
        if !interrupted() {
            schedule(Reason::Sleep(time::ticks() + ticks));
        }
    });
}

/// Ends the current thread.
pub fn exit() -> ! {
    arch::disable_interrupts();
    schedule(Reason::Exit);
    unreachable!("an exited thread was resumed");
}

/// The running thread.
pub fn current() -> Arc<Thread> {
    arch::without_interrupts(|| {
        SCHEDULER.lock().cpus[arch::cpu_index()]
            .current
            .clone()
            .expect("scheduler running")
    })
}

/// Blocks the current thread until [`wake`] is called for it.
///
/// The caller registers the thread with the object that will wake it (the
/// registration keeps it alive: it holds its `Arc`), with interrupts
/// disabled, then calls this. A wake that comes in between, from another
/// CPU, is not lost: this then returns at once (module docs).
///
/// An [`interrupted`] thread does not block (or returns as soon as it is
/// interrupted): the caller then removes its registration and gives up.
pub fn block() {
    if !interrupted() {
        schedule_to(None, Reason::Block);
    }
}

/// Like [`block`], but runs `next` (which must be registered somewhere to
/// be woken) immediately instead of the next ready thread: the IPC direct
/// switch. If `next` has not blocked yet (it is on its way, on another
/// CPU) or was interrupted, it is woken the ordinary way instead.
pub fn block_and_switch_to(next: Arc<Thread>) {
    if interrupted() {
        // `next` stays registered; give the CPU to whoever is ready.
        return;
    }
    schedule_to(Some(next), Reason::Block);
}

/// Makes a thread waiting in [`block`] runnable again, or, if it has not
/// blocked yet, makes its coming [`block`] return at once. An interrupted
/// thread has already been made runnable: waking it does nothing more.
pub fn wake(thread: Arc<Thread>) {
    arch::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        if thread.blocked.swap(false, Ordering::AcqRel) {
            scheduler.make_ready(thread);
        } else if !thread.interrupted.load(Ordering::Acquire) {
            thread.wake_pending.store(true, Ordering::Release);
        }
    });
}

/// Interrupts `thread` for good (its process is being killed, ADR-0044):
/// if it is blocked or asleep it runs again now, and it will not block or
/// sleep again. Whatever it waited on sees it gone when it deregisters. A
/// thread running on another CPU notices at its next entry into the
/// kernel (at the latest, that CPU's next tick).
pub fn interrupt(thread: &Arc<Thread>) {
    arch::without_interrupts(|| {
        thread.interrupted.store(true, Ordering::Release);
        let mut scheduler = SCHEDULER.lock();
        if thread.blocked.swap(false, Ordering::AcqRel) {
            scheduler.make_ready(thread.clone());
        } else {
            scheduler
                .queue
                .wake_sleeper(|sleeper| Arc::ptr_eq(sleeper, thread));
        }
    });
}

/// Whether the running thread has been interrupted.
pub fn interrupted() -> bool {
    current().interrupted.load(Ordering::Acquire)
}

/// Threads that exist (including idle threads and unreaped zombies).
pub fn live_threads() -> usize {
    LIVE_THREADS.load(Ordering::Relaxed)
}

fn next_id() -> ThreadId {
    LIVE_THREADS.fetch_add(1, Ordering::Relaxed);
    ThreadId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_thread(
    name: &'static str,
    entry: fn(usize),
    arg: usize,
    root: u64,
    process: Option<Arc<Process>>,
) -> Result<Arc<Thread>, SpawnError> {
    let stack = paging::allocate_kernel_stack().map_err(SpawnError::Stack)?;
    // SAFETY: the stack was just mapped for this thread alone.
    let rsp = unsafe { arch::prepare_stack(stack.top(), thread_start, entry as usize, arg) };
    Ok(Arc::new(Thread {
        id: next_id(),
        name,
        saved_rsp: UnsafeCell::new(rsp),
        blocked: AtomicBool::new(false),
        wake_pending: AtomicBool::new(false),
        on_cpu: AtomicBool::new(false),
        interrupted: AtomicBool::new(false),
        root,
        process,
        pending_call: Mutex::new(None),
        stack,
    }))
}

/// First Rust code of every spawned thread (see `arch::prepare_stack`).
extern "C" fn thread_start(entry: usize, arg: usize) -> ! {
    after_switch();
    arch::enable_interrupts();
    // SAFETY: `entry` was produced from a `fn(usize)` in `new_thread`.
    let entry: fn(usize) = unsafe { core::mem::transmute::<usize, fn(usize)>(entry) };
    entry(arg);
    exit()
}

fn idle_main(_: usize) {
    loop {
        arch::wait_for_interrupt();
    }
}

enum Reason {
    /// Still runnable: back of the ready queue (or keep running if alone).
    Yield,
    /// Runnable again at this tick.
    Sleep(u64),
    /// Waiting for [`wake`]; owned by whatever registered it.
    Block,
    Exit,
}

/// Switches away from the current thread. Interrupts must be disabled.
fn schedule(reason: Reason) {
    schedule_to(None, reason);
}

/// [`schedule`], optionally running `target` next instead of the head of
/// the ready queue (`target` must not be in the ready queue; it is used
/// only if it has blocked, see [`block_and_switch_to`]).
///
/// Protocol: decide under the scheduler lock, release it, wait until the
/// next thread is off its last CPU, then switch (module docs).
fn schedule_to(target: Option<Arc<Thread>>, reason: Reason) {
    let me = arch::cpu_index();
    let (save, next) = {
        let mut scheduler = SCHEDULER.lock();
        let scheduler = &mut *scheduler;
        let cpu = &mut scheduler.cpus[me];
        if matches!(reason, Reason::Block) {
            assert!(!cpu.is_idle(), "an idle thread must never block");
            let current = cpu.current.as_ref().expect("scheduler running");
            // Woken already, from another CPU: nothing to wait for.
            if current.wake_pending.swap(false, Ordering::AcqRel) {
                if let Some(target) = target {
                    wake_locked(scheduler, target);
                }
                return;
            }
            current.blocked.store(true, Ordering::Release);
        }
        // The direct switch only to a thread that has blocked; one still on
        // its way is woken the ordinary way.
        let target = target.and_then(|target| {
            if target.blocked.swap(false, Ordering::AcqRel) {
                Some(target)
            } else {
                if !target.interrupted.load(Ordering::Acquire) {
                    target.wake_pending.store(true, Ordering::Release);
                }
                None
            }
        });
        let cpu = &mut scheduler.cpus[me];
        let next = match target.or_else(|| scheduler.queue.pop_ready()) {
            Some(next) => next,
            // Nothing else to run: a yielding thread just continues.
            None if matches!(reason, Reason::Yield) => {
                cpu.slice.reset();
                return;
            }
            None => cpu.idle.clone().expect("idle thread exists"),
        };
        let was_idle = cpu.is_idle();
        let previous = cpu.current.take().expect("scheduler running");
        let save = previous.saved_rsp.get();
        cpu.previous = Some(previous.clone());
        match reason {
            Reason::Yield if was_idle => {}
            Reason::Yield => scheduler.queue.push_ready(previous),
            Reason::Sleep(deadline) => scheduler.queue.sleep_until(previous, deadline),
            // Kept alive by the object it waits on (see `block`).
            Reason::Block => drop(previous),
            Reason::Exit => scheduler.cpus[me].zombies.push(previous),
        }
        let cpu = &mut scheduler.cpus[me];
        cpu.current = Some(next.clone());
        cpu.slice.reset();
        (save, next)
    };
    // The next thread may still be leaving another CPU: its registers are
    // saved once that CPU clears `on_cpu`.
    while next
        .on_cpu
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    // SAFETY: `next` is not running anywhere (claimed above), so its saved
    // stack pointer is stable.
    let load = unsafe { *next.saved_rsp.get() };
    // Ring-3 entries (syscalls, interrupts) of the next thread land on its
    // own kernel stack.
    arch::set_kernel_stack(next.stack.top());
    if next.root != arch::active_root() {
        // SAFETY: `root` is the kernel's address space or that of the next
        // thread's process, which the thread keeps alive; both contain the
        // shared kernel half, so this code and stack stay mapped.
        unsafe { arch::activate_root(next.root) };
    }
    drop(next);
    // SAFETY: interrupts are disabled (caller contract); `save` belongs to
    // the outgoing thread, kept alive by this CPU's `previous`; `load` was
    // saved by a previous switch or prepared by `new_thread`.
    unsafe { arch::switch_context(save, load) };
    after_switch();
}

/// [`wake`] with the scheduler lock held.
fn wake_locked(scheduler: &mut Scheduler, thread: Arc<Thread>) {
    if thread.blocked.swap(false, Ordering::AcqRel) {
        scheduler.make_ready(thread);
    } else if !thread.interrupted.load(Ordering::Acquire) {
        thread.wake_pending.store(true, Ordering::Release);
    }
}

/// Runs on the incoming thread after every switch: the thread switched
/// away from is free for other CPUs, and exited threads are freed.
fn after_switch() {
    let (previous, zombies) = {
        let mut scheduler = SCHEDULER.lock();
        let cpu = &mut scheduler.cpus[arch::cpu_index()];
        (cpu.previous.take(), core::mem::take(&mut cpu.zombies))
    };
    if let Some(previous) = previous {
        previous.on_cpu.store(false, Ordering::Release);
    }
    // Dropped outside the lock: freeing stacks takes the paging and frame
    // locks.
    drop(zombies);
}

/// After a device interrupt or an inter-processor interrupt (interrupts
/// disabled, already acknowledged): if this CPU was idle and someone is
/// ready, run it now rather than at the next tick.
fn on_device_interrupt() {
    let run_now = {
        let scheduler = SCHEDULER.lock();
        scheduler.cpus[arch::cpu_index()].is_idle() && scheduler.queue.has_ready()
    };
    if run_now {
        preempt_or_defer();
    }
}

/// An inter-processor interrupt asked this CPU to look at the ready queue.
pub fn on_reschedule_ipi() {
    if STARTED.load(Ordering::Acquire) {
        on_device_interrupt();
    }
}

/// Sections that must not be preempted but must keep interrupts enabled
/// (console output: receive interrupts must still drain the UART). Nested
/// counts, per CPU: a section never moves, as it is not preempted.
static PREEMPT_DISABLED: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
/// A preemption fell due inside such a section.
static PREEMPT_PENDING: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// While alive, the current thread is not preempted (interrupts still run).
/// It must not block.
pub struct NoPreempt(());

impl NoPreempt {
    pub fn new() -> Self {
        arch::without_interrupts(|| {
            PREEMPT_DISABLED[arch::cpu_index()].fetch_add(1, Ordering::Acquire);
        });
        Self(())
    }
}

impl Drop for NoPreempt {
    fn drop(&mut self) {
        let yield_due = arch::without_interrupts(|| {
            let cpu = arch::cpu_index();
            PREEMPT_DISABLED[cpu].fetch_sub(1, Ordering::Release) == 1
                && PREEMPT_PENDING[cpu].swap(false, Ordering::AcqRel)
        });
        if yield_due {
            // Only reached from thread context (interrupt handlers never
            // hold a NoPreempt across returning).
            yield_now();
        }
    }
}

/// From interrupt context: switch now, or when the current section ends.
fn preempt_or_defer() {
    let cpu = arch::cpu_index();
    if PREEMPT_DISABLED[cpu].load(Ordering::Acquire) > 0 {
        PREEMPT_PENDING[cpu].store(true, Ordering::Release);
    } else {
        schedule(Reason::Yield);
    }
}

/// Timer interrupt (interrupts disabled), on every CPU: the boot CPU
/// advances time and wakes sleepers; each CPU counts its slice and
/// preempts its thread when the slice is used up and someone else can
/// run.
fn on_timer_tick() {
    let me = arch::cpu_index();
    let now = if me == 0 {
        let now = time::advance();
        crate::random::sample();
        // Before taking the scheduler lock: signalling wakes threads.
        crate::ipc::notification::fire_timers(now);
        Some(now)
    } else {
        None
    };
    paging::tlb_tick();
    let preempt = {
        let mut scheduler = SCHEDULER.lock();
        if let Some(now) = now
            && scheduler.queue.wake_due(now) > 0
        {
            // Woken sleepers may be for idle CPUs.
            if let Some(idle) = (1..MAX_CPUS).find(|&cpu| scheduler.cpus[cpu].is_idle()) {
                arch::send_ipi_to_cpu(idle);
            }
        }
        let cpu = &mut scheduler.cpus[me];
        let expired = cpu.slice.tick();
        let idle = cpu.is_idle();
        scheduler.queue.has_ready() && (expired || idle)
    };
    if preempt {
        preempt_or_defer();
    }
}

/// Scheduler self-test, for smoke-test boots.
pub fn self_test() {
    use core::sync::atomic::AtomicBool;

    const WORKERS: usize = 3;
    // Each round costs up to one full slice while the spinner holds the CPU.
    const ROUNDS: u64 = 100;
    static COUNTS: [AtomicU64; WORKERS] = [const { AtomicU64::new(0) }; WORKERS];
    static STOP_SPINNER: AtomicBool = AtomicBool::new(false);
    static SPINS: AtomicU64 = AtomicU64::new(0);
    static SLEPT_TICKS: AtomicU64 = AtomicU64::new(0);

    fn worker(index: usize) {
        for _ in 0..ROUNDS {
            COUNTS[index].fetch_add(1, Ordering::Relaxed);
            yield_now();
        }
    }
    // Never yields: other threads only progress if preemption works.
    fn spinner(_: usize) {
        while !STOP_SPINNER.load(Ordering::Relaxed) {
            SPINS.fetch_add(1, Ordering::Relaxed);
            core::hint::spin_loop();
        }
    }
    fn sleeper(_: usize) {
        let start = time::ticks();
        for _ in 0..3 {
            sleep_ms(50);
        }
        SLEPT_TICKS.store((time::ticks() - start).max(1), Ordering::Relaxed);
    }

    let baseline = live_threads();
    spawn("test-spinner", spinner, 0).expect("spawn spinner");
    for index in 0..WORKERS {
        spawn("test-worker", worker, index).expect("spawn worker");
    }
    spawn("test-sleeper", sleeper, 0).expect("spawn sleeper");

    let deadline = time::ticks() + 10 * u64::from(time::HZ);
    let done = || {
        COUNTS.iter().all(|c| c.load(Ordering::Relaxed) == ROUNDS)
            && SLEPT_TICKS.load(Ordering::Relaxed) != 0
    };
    while !done() {
        assert!(time::ticks() < deadline, "scheduler self-test timed out");
        sleep_ms(10);
    }
    let slept = SLEPT_TICKS.load(Ordering::Relaxed);
    assert!(
        slept >= time::ms_to_ticks(150),
        "3 × 50 ms sleeps took only {slept} ticks"
    );
    assert!(SPINS.load(Ordering::Relaxed) > 0, "spinner never ran");

    STOP_SPINNER.store(true, Ordering::Relaxed);
    let deadline = time::ticks() + 2 * u64::from(time::HZ);
    while live_threads() != baseline {
        assert!(time::ticks() < deadline, "exited threads were not reaped");
        sleep_ms(10);
    }
    klog::info!(
        "scheduler self-test passed: {} worker rounds, sleeper {} ticks, spinner preempted",
        WORKERS as u64 * ROUNDS,
        slept
    );
}
