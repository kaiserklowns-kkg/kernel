//! Device interrupt vectors (ADR-0021): each allocated vector signals a
//! notification when its MSI-X message arrives.
//!
//! The routing table is a fixed array (no allocation in interrupt
//! context). The interrupt path only reads it and signals, which never
//! blocks; routes are dropped outside the lock, never in interrupt context.

use alloc::sync::Arc;

use spin::Mutex;

use crate::arch::{self, DEVICE_VECTORS};
use crate::ipc::Notification;

const COUNT: usize = (DEVICE_VECTORS.end - DEVICE_VECTORS.start) as usize;

struct Route {
    notification: Arc<Notification>,
    bits: u64,
}

static ROUTES: Mutex<[Option<Route>; COUNT]> = Mutex::new([const { None }; COUNT]);

fn slot(vector: u8) -> usize {
    usize::from(vector - DEVICE_VECTORS.start)
}

/// Interrupt context: a device vector fired.
pub fn dispatch(vector: u8) {
    if !DEVICE_VECTORS.contains(&vector) {
        return;
    }
    // Interrupts are disabled here, and every other holder of the lock
    // disables them, so it cannot be held by code this interrupted.
    if let Some(route) = &ROUTES.lock()[slot(vector)] {
        route.notification.signal(route.bits);
    }
    // A vector without a route (released, message still in flight) is
    // simply acknowledged.
}

/// A free vector that will signal `bits` on `notification`.
pub fn allocate(notification: Arc<Notification>, bits: u64) -> Option<u8> {
    arch::without_interrupts(|| {
        let mut routes = ROUTES.lock();
        let index = routes.iter().position(Option::is_none)?;
        routes[index] = Some(Route { notification, bits });
        Some(DEVICE_VECTORS.start + index as u8)
    })
}

/// Points an allocated vector at a new notification.
pub fn rebind(vector: u8, notification: Arc<Notification>, bits: u64) {
    let old = arch::without_interrupts(|| {
        ROUTES.lock()[slot(vector)].replace(Route { notification, bits })
    });
    drop(old);
}

/// Frees a vector. The caller has masked its source first.
pub fn release(vector: u8) {
    let old = arch::without_interrupts(|| ROUTES.lock()[slot(vector)].take());
    drop(old);
}
