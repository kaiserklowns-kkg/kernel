//! Kernel threads and preemptive scheduling (ADR-0012).
//!
//! Mechanism here, policy in `oceans-scheduler`: round-robin with a fixed
//! time slice, driven by the local APIC timer, plus timed sleep. A thread
//! that exits becomes a zombie until the next thread to run reaps it, which
//! frees its stack: a thread can never free the stack it is running on.
//!
//! Single CPU for now: all scheduler state is touched only with interrupts
//! disabled, which is what makes the context-switch protocol below sound.
//! SMP replaces this with per-CPU run queues (separate ADR).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use oceans_scheduler::{RunQueue, TimeSlice};
use spin::Mutex;

use crate::memory::paging::{self, KernelStack, MapError};
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
    /// switched in, both with interrupts disabled on the only CPU.
    saved_rsp: UnsafeCell<u64>,
    /// The thread's stack; freed when the thread is reaped.
    _stack: KernelStack,
}

// SAFETY: `saved_rsp` is only accessed by the scheduler with interrupts
// disabled on a single CPU (see module docs), never concurrently.
unsafe impl Sync for Thread {}
unsafe impl Send for Thread {}

impl Drop for Thread {
    fn drop(&mut self) {
        klog::debug!("thread {} ({}) reaped", self.id.0, self.name);
        LIVE_THREADS.fetch_sub(1, Ordering::Relaxed);
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static LIVE_THREADS: AtomicUsize = AtomicUsize::new(0);

struct Scheduler {
    queue: RunQueue<Arc<Thread>>,
    current: Option<Arc<Thread>>,
    idle: Option<Arc<Thread>>,
    /// Exited threads whose stacks are freed by the next thread to run.
    zombies: Vec<Arc<Thread>>,
    slice: TimeSlice,
}

impl Scheduler {
    fn current_is_idle(&self) -> bool {
        match (&self.current, &self.idle) {
            (Some(current), Some(idle)) => Arc::ptr_eq(current, idle),
            _ => false,
        }
    }
}

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler {
    queue: RunQueue::new(),
    current: None,
    idle: None,
    zombies: Vec::new(),
    slice: TimeSlice::new(QUANTUM_TICKS),
});

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

/// Turns the running boot code into the first thread, creates the idle
/// thread and starts preemption.
pub fn init() {
    let stack = BOOT_STACK
        .lock()
        .take()
        .expect("boot code hands over its stack first");
    let boot = Arc::new(Thread {
        id: next_id(),
        name: "boot",
        // Filled in by the first switch away from it.
        saved_rsp: UnsafeCell::new(0),
        _stack: stack,
    });
    let idle = new_thread("idle", idle_main, 0)
        .unwrap_or_else(|err| panic!("cannot create the idle thread: {err:?}"));

    arch::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        scheduler.current = Some(boot);
        scheduler.idle = Some(idle);
    });
    arch::start_timer(time::HZ, on_timer_tick);
    arch::enable_interrupts();
    klog::info!(
        "scheduler running: round-robin, {} ms slice",
        u64::from(QUANTUM_TICKS) * 1000 / u64::from(time::HZ)
    );
}

/// Starts a kernel thread running `entry(arg)`. It exits when `entry` returns.
pub fn spawn(name: &'static str, entry: fn(usize), arg: usize) -> Result<ThreadId, SpawnError> {
    let thread = new_thread(name, entry, arg)?;
    let id = thread.id;
    arch::without_interrupts(|| SCHEDULER.lock().queue.push_ready(thread));
    Ok(id)
}

/// Gives the CPU to the next ready thread, if any.
pub fn yield_now() {
    arch::without_interrupts(|| schedule(Reason::Yield));
}

/// Blocks the current thread for at least `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    let ticks = time::ms_to_ticks(ms).max(1);
    arch::without_interrupts(|| schedule(Reason::Sleep(time::ticks() + ticks)));
}

/// Ends the current thread.
pub fn exit() -> ! {
    arch::disable_interrupts();
    schedule(Reason::Exit);
    unreachable!("an exited thread was resumed");
}

/// Threads that exist (including the idle thread and unreaped zombies).
pub fn live_threads() -> usize {
    LIVE_THREADS.load(Ordering::Relaxed)
}

fn next_id() -> ThreadId {
    LIVE_THREADS.fetch_add(1, Ordering::Relaxed);
    ThreadId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

fn new_thread(name: &'static str, entry: fn(usize), arg: usize) -> Result<Arc<Thread>, SpawnError> {
    let stack = paging::allocate_kernel_stack().map_err(SpawnError::Stack)?;
    // SAFETY: the stack was just mapped for this thread alone.
    let rsp = unsafe { arch::prepare_stack(stack.top(), thread_start, entry as usize, arg) };
    Ok(Arc::new(Thread {
        id: next_id(),
        name,
        saved_rsp: UnsafeCell::new(rsp),
        _stack: stack,
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
    Exit,
}

/// Switches away from the current thread. Interrupts must be disabled.
///
/// Protocol: decide under the scheduler lock, release it, then switch. The
/// outgoing thread stays referenced by the queue, sleep queue or zombie
/// list, so the `saved_rsp` slot written by the switch remains valid.
fn schedule(reason: Reason) {
    let (save, load) = {
        let mut scheduler = SCHEDULER.lock();
        let next = match scheduler.queue.pop_ready() {
            Some(next) => next,
            // Nothing else to run: a yielding thread just continues.
            None if matches!(reason, Reason::Yield) => {
                scheduler.slice.reset();
                return;
            }
            None => scheduler.idle.clone().expect("idle thread exists"),
        };
        let was_idle = scheduler.current_is_idle();
        let previous = scheduler.current.take().expect("scheduler running");
        let save = previous.saved_rsp.get();
        match reason {
            Reason::Yield if was_idle => {}
            Reason::Yield => scheduler.queue.push_ready(previous),
            Reason::Sleep(deadline) => scheduler.queue.sleep_until(previous, deadline),
            Reason::Exit => scheduler.zombies.push(previous),
        }
        // SAFETY: `next` is not running, so its saved stack pointer is stable.
        let load = unsafe { *next.saved_rsp.get() };
        scheduler.current = Some(next);
        scheduler.slice.reset();
        (save, load)
    };
    // SAFETY: interrupts are disabled (caller contract); `save` belongs to the
    // outgoing thread, kept alive as described above; `load` was saved by a
    // previous switch or prepared by `new_thread`.
    unsafe { arch::switch_context(save, load) };
    after_switch();
}

/// Runs on the incoming thread after every switch: frees exited threads.
fn after_switch() {
    let zombies = core::mem::take(&mut SCHEDULER.lock().zombies);
    // Dropped outside the lock: freeing stacks takes the paging and frame
    // locks.
    drop(zombies);
}

/// Timer interrupt (interrupts disabled): wakes sleepers and preempts the
/// current thread when its slice is used up and someone else can run.
fn on_timer_tick() {
    let now = time::advance();
    let preempt = {
        let mut scheduler = SCHEDULER.lock();
        scheduler.queue.wake_due(now);
        let expired = scheduler.slice.tick();
        scheduler.queue.has_ready() && (expired || scheduler.current_is_idle())
    };
    if preempt {
        schedule(Reason::Yield);
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
