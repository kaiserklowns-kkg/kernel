//! Notifications: asynchronous signals as a latched 64-bit word.
//!
//! `signal(bits)` ORs bits in and wakes one waiter; `wait()` blocks until any
//! bit is set, then returns and clears them all. Signals are never lost:
//! bits set with nobody waiting stay set until the next `wait`. Signalling
//! never blocks, so it is safe from interrupt handlers (future IRQ
//! delivery to userspace drivers).

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use spin::Mutex;

use crate::arch;
use crate::sched::{self, Thread};

struct State {
    bits: u64,
    waiters: VecDeque<Arc<Thread>>,
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
            }),
        })
    }

    pub fn signal(&self, bits: u64) {
        if bits == 0 {
            return;
        }
        arch::without_interrupts(|| {
            let waiter = {
                let mut state = self.state.lock();
                state.bits |= bits;
                state.waiters.pop_front()
            };
            if let Some(waiter) = waiter {
                sched::wake(waiter);
            }
        });
    }

    /// Blocks until a signal arrives; returns (and clears) all pending bits.
    pub fn wait(&self) -> u64 {
        arch::without_interrupts(|| {
            loop {
                {
                    let mut state = self.state.lock();
                    if state.bits != 0 {
                        return core::mem::take(&mut state.bits);
                    }
                    state.waiters.push_back(sched::current());
                }
                sched::block();
            }
        })
    }
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
    klog::info!("notification self-test passed");
}
