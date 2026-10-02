//! Scheduling policy (ADR-0012): which thread runs next.
//!
//! Round-robin over a FIFO ready queue with a fixed time slice, plus a sleep
//! queue ordered by wake-up tick. Generic over the thread handle type and
//! free of any mechanism (context switching, timers, locking), so it is
//! fully testable on the host.

#![no_std]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};

/// Ready and sleeping threads.
pub struct RunQueue<T> {
    ready: VecDeque<T>,
    /// Keyed by (wake tick, insertion sequence): equal deadlines wake in
    /// the order they went to sleep.
    sleeping: BTreeMap<(u64, u64), T>,
    sequence: u64,
}

impl<T> Default for RunQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> RunQueue<T> {
    pub const fn new() -> Self {
        Self {
            ready: VecDeque::new(),
            sleeping: BTreeMap::new(),
            sequence: 0,
        }
    }

    /// Makes `thread` runnable, behind every thread already waiting.
    pub fn push_ready(&mut self, thread: T) {
        self.ready.push_back(thread);
    }

    /// The thread that has waited longest.
    pub fn pop_ready(&mut self) -> Option<T> {
        self.ready.pop_front()
    }

    pub fn has_ready(&self) -> bool {
        !self.ready.is_empty()
    }

    pub fn ready_len(&self) -> usize {
        self.ready.len()
    }

    pub fn sleeping_len(&self) -> usize {
        self.sleeping.len()
    }

    /// Parks `thread` until tick `deadline`.
    pub fn sleep_until(&mut self, thread: T, deadline: u64) {
        self.sequence += 1;
        self.sleeping.insert((deadline, self.sequence), thread);
    }

    /// Moves every sleeper whose deadline is `<= now` to the ready queue, in
    /// deadline order. Returns how many woke.
    pub fn wake_due(&mut self, now: u64) -> usize {
        let mut woken = 0;
        while let Some(entry) = self.sleeping.first_entry() {
            if entry.key().0 > now {
                break;
            }
            self.ready.push_back(entry.remove());
            woken += 1;
        }
        woken
    }

    /// The earliest wake-up tick, if anything sleeps (for tickless idle).
    pub fn next_deadline(&self) -> Option<u64> {
        self.sleeping.keys().next().map(|&(deadline, _)| deadline)
    }
}

/// Remaining time of the running thread's slice, in ticks.
#[derive(Clone, Copy, Debug)]
pub struct TimeSlice {
    quantum: u32,
    left: u32,
}

impl TimeSlice {
    /// A slice of `quantum` ticks (at least 1).
    pub const fn new(quantum: u32) -> Self {
        let quantum = if quantum == 0 { 1 } else { quantum };
        Self {
            quantum,
            left: quantum,
        }
    }

    /// Starts a fresh slice (on every switch).
    pub fn reset(&mut self) {
        self.left = self.quantum;
    }

    /// Accounts one tick. Returns true when the slice is used up.
    pub fn tick(&mut self) -> bool {
        self.left = self.left.saturating_sub(1);
        self.left == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn ready_queue_is_fifo_round_robin() {
        let mut q = RunQueue::new();
        for t in [1, 2, 3] {
            q.push_ready(t);
        }
        let mut order = Vec::new();
        for _ in 0..6 {
            let t = q.pop_ready().unwrap();
            order.push(t);
            q.push_ready(t); // preempted: back of the queue
        }
        assert_eq!(order, [1, 2, 3, 1, 2, 3]);
    }

    #[test]
    fn sleepers_wake_in_deadline_then_arrival_order() {
        let mut q = RunQueue::new();
        q.sleep_until("late", 30);
        q.sleep_until("first", 10);
        q.sleep_until("second", 10);
        assert_eq!(q.next_deadline(), Some(10));
        assert_eq!(q.wake_due(9), 0);
        assert_eq!(q.wake_due(10), 2);
        assert_eq!(q.pop_ready(), Some("first"));
        assert_eq!(q.pop_ready(), Some("second"));
        assert_eq!(q.sleeping_len(), 1);
        assert_eq!(q.wake_due(1000), 1);
        assert_eq!(q.pop_ready(), Some("late"));
        assert_eq!(q.next_deadline(), None);
    }

    #[test]
    fn woken_threads_queue_behind_ready_ones() {
        let mut q = RunQueue::new();
        q.push_ready("running-before");
        q.sleep_until("sleeper", 5);
        q.wake_due(5);
        assert_eq!(q.pop_ready(), Some("running-before"));
        assert_eq!(q.pop_ready(), Some("sleeper"));
    }

    #[test]
    fn time_slice_expires_and_resets() {
        let mut slice = TimeSlice::new(3);
        assert!(!slice.tick());
        assert!(!slice.tick());
        assert!(slice.tick());
        assert!(slice.tick(), "stays expired until reset");
        slice.reset();
        assert!(!slice.tick());
        assert!(TimeSlice::new(0).tick(), "zero quantum is one tick");
    }
}
