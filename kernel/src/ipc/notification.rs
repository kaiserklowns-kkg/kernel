//! Notifications: asynchronous signals as a latched 64-bit word.
//!
//! `signal(bits)` ORs bits in and wakes one waiter; `wait()` blocks until any
//! bit is set, then returns and clears them all. Signals are never lost:
//! bits set with nobody waiting stay set until the next `wait`. Signalling
//! never blocks, so it is safe from interrupt handlers (future IRQ
//! delivery to userspace drivers).

//!
//! A notification can be **bound** to one endpoint (ADR-0023): its signals
//! then also wake the endpoint's receiver, which gets them as an event, so
//! a single-threaded service waits for calls and events (IRQs, timers,
//! "data ready") at once. **Timers** signal a notification after a delay.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use super::endpoint::Endpoint;
use crate::sched::{self, Thread};
use crate::{arch, time};

struct State {
    bits: u64,
    waiters: VecDeque<Arc<Thread>>,
    /// The endpoint whose receiver this notification also wakes.
    bound: Option<Weak<Endpoint>>,
}

pub struct Notification {
    state: Mutex<State>,
}

impl core::fmt::Debug for Notification {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Notification")
    }
}

impl Notification {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                bits: 0,
                waiters: VecDeque::new(),
                bound: None,
            }),
        })
    }

    pub fn signal(&self, bits: u64) {
        if bits == 0 {
            return;
        }
        arch::without_interrupts(|| {
            let (waiter, endpoint) = {
                let mut state = self.state.lock();
                state.bits |= bits;
                (
                    state.waiters.pop_front(),
                    state.bound.as_ref().and_then(Weak::upgrade),
                )
            };
            // Our lock is released first: the endpoint takes its own lock
            // and then ours (`take_bits`), never the other way round.
            if let Some(waiter) = waiter {
                sched::wake(waiter);
            }
            if let Some(endpoint) = endpoint {
                endpoint.wake_receiver();
            }
        });
    }

    /// Takes (and clears) the pending bits without blocking.
    pub fn take_bits(&self) -> u64 {
        arch::without_interrupts(|| core::mem::take(&mut self.state.lock().bits))
    }

    /// Binds to `endpoint`; fails if bound to another live endpoint.
    pub fn bind(&self, endpoint: Weak<Endpoint>) -> Result<(), ()> {
        arch::without_interrupts(|| {
            let mut state = self.state.lock();
            match &state.bound {
                Some(current)
                    if current.strong_count() > 0 && !Weak::ptr_eq(current, &endpoint) =>
                {
                    Err(())
                }
                _ => {
                    state.bound = Some(endpoint);
                    Ok(())
                }
            }
        })
    }

    pub fn unbind(&self) {
        arch::without_interrupts(|| self.state.lock().bound = None);
    }

    /// Blocks until a signal arrives; returns (and clears) all pending bits
    /// (0 if the waiting thread was interrupted, ADR-0044).
    pub fn wait(&self) -> u64 {
        arch::without_interrupts(|| {
            loop {
                {
                    let mut state = self.state.lock();
                    if state.bits != 0 {
                        return core::mem::take(&mut state.bits);
                    }
                    if sched::interrupted() {
                        let me = sched::current();
                        state.waiters.retain(|waiter| !Arc::ptr_eq(waiter, &me));
                        return 0;
                    }
                    state.waiters.push_back(sched::current());
                }
                sched::block();
            }
        })
    }
}

/// A pending one-shot timer. Weak: a timer never keeps its notification
/// alive, so timers cannot outlive the capabilities that set them.
struct Timer {
    deadline: u64,
    notification: Weak<Notification>,
    bits: u64,
}

static TIMERS: Mutex<Vec<Timer>> = Mutex::new(Vec::new());
/// Earliest deadline (`u64::MAX`: none), so most ticks skip the lock.
static NEXT_DEADLINE: AtomicU64 = AtomicU64::new(u64::MAX);

/// Signals `bits` on `notification` once `ms` milliseconds have passed
/// (rounded up to whole ticks), replacing the notification's earlier timer.
/// `ms` 0 only cancels.
pub fn set_timer(notification: &Arc<Notification>, bits: u64, ms: u64) {
    let deadline = time::ticks().saturating_add(time::ms_to_ticks(ms).max(1));
    let weak = Arc::downgrade(notification);
    arch::without_interrupts(|| {
        let mut timers = TIMERS.lock();
        timers
            .retain(|t| t.notification.strong_count() > 0 && !Weak::ptr_eq(&t.notification, &weak));
        if ms > 0 && bits != 0 {
            timers.push(Timer {
                deadline,
                notification: weak,
                bits,
            });
        }
        let next = timers.iter().map(|t| t.deadline).min().unwrap_or(u64::MAX);
        NEXT_DEADLINE.store(next, Ordering::Relaxed);
    });
}

/// Timer interrupt (interrupts disabled): signals every expired timer.
pub fn fire_timers(now: u64) {
    if now < NEXT_DEADLINE.load(Ordering::Relaxed) {
        return;
    }
    let mut timers = TIMERS.lock();
    let mut index = 0;
    while index < timers.len() {
        if timers[index].deadline <= now {
            let timer = timers.swap_remove(index);
            if let Some(notification) = timer.notification.upgrade() {
                notification.signal(timer.bits);
            }
        } else {
            index += 1;
        }
    }
    let next = timers.iter().map(|t| t.deadline).min().unwrap_or(u64::MAX);
    NEXT_DEADLINE.store(next, Ordering::Relaxed);
}

/// Notification self-test, for smoke-test boots.
pub fn self_test() {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicU64, Ordering};

    use oceans_capability::Rights;

    use crate::object::{self, CapTable, Capability, KernelObject, ObjectKind};
    use crate::{klog, time};

    static RECEIVED: AtomicU64 = AtomicU64::new(0);

    fn waiter(arg: usize) {
        // SAFETY: `arg` is the `Box<Arc<Notification>>` leaked by the spawner.
        let notification = unsafe { Box::from_raw(arg as *mut Arc<Notification>) };
        RECEIVED.store(notification.wait(), Ordering::Release);
    }

    // Blocked waiter woken by a signal; the signaller holds a SIGNAL-only
    // capability.
    let mut table = CapTable::new(object::DEFAULT_CAP_LIMIT);
    let full = table
        .insert(Capability::new(
            KernelObject::Notification(Notification::new()),
            object::default_rights(ObjectKind::Notification),
        ))
        .expect("insert notification");
    let signal_only = table
        .derive(full, Rights::SIGNAL)
        .expect("derive SIGNAL-only");
    assert!(object::notification(&mut table, signal_only, Rights::WAIT).is_err());
    let notification =
        object::notification(&mut table, signal_only, Rights::SIGNAL).expect("SIGNAL");
    let arg = Box::into_raw(Box::new(notification.clone())) as usize;
    sched::spawn("notify-waiter", waiter, arg).expect("spawn waiter");
    sched::sleep_ms(20); // let it block
    notification.signal(0b101);
    let deadline = time::ticks() + 2 * u64::from(time::HZ);
    while RECEIVED.load(Ordering::Acquire) == 0 {
        assert!(time::ticks() < deadline, "notification waiter never woke");
        sched::sleep_ms(5);
    }
    assert_eq!(RECEIVED.load(Ordering::Acquire), 0b101);

    // Latched: signals before the wait are delivered by it, merged.
    notification.signal(0b010);
    notification.signal(0b100);
    assert_eq!(notification.wait(), 0b110);

    // Timers: fire once after the delay; setting again replaces; 0 cancels.
    let start = time::ticks();
    set_timer(&notification, 0b1000, 500);
    set_timer(&notification, 0b1000, 30);
    assert_eq!(notification.wait(), 0b1000);
    let waited = time::ticks() - start;
    assert!(
        (time::ms_to_ticks(30)..time::ms_to_ticks(500)).contains(&waited),
        "timer fired after {waited} ticks"
    );
    set_timer(&notification, 0b1, 20);
    set_timer(&notification, 0b1, 0);
    sched::sleep_ms(60);
    assert_eq!(notification.take_bits(), 0, "cancelled timer fired");
    klog::info!("notification self-test passed");
}
