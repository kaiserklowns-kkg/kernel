//! Inter-process communication (ADR-0013).
//!
//! - [`endpoint`]: synchronous call/reply with inline data and capability
//!   transfer, and a direct switch from caller to waiting server.
//! - [`notification`]: a latched word of signal bits, for asynchronous
//!   events (IRQs, timeouts, "data ready").
//!
//! Blocking protocol: every operation disables interrupts, checks its
//! condition and registers the waiting thread under the object's lock, then
//! calls `sched::block`. A wake from another CPU between the registration
//! and the block is not lost: the block then returns at once (ADR-0089,
//! see `sched`).

pub mod endpoint;
mod message;
pub mod notification;

pub use endpoint::{ClientEnd, ServerEnd};
pub use message::{MAX_INLINE_BYTES, Message};
pub use notification::Notification;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpcError {
    /// Every end on the other side has been closed.
    PeerClosed,
    /// The server dropped the call without replying.
    NoReply,
    /// Inline data larger than [`MAX_INLINE_BYTES`].
    MessageTooLarge,
    /// More than [`message::MAX_CAPABILITIES`] capabilities attached.
    TooManyCapabilities,
    /// No memory for the call's bookkeeping.
    OutOfMemory,
    /// The notification is bound to another endpoint.
    Busy,
    /// The waiting thread was interrupted: its process is being killed.
    Interrupted,
}

/// IPC self-tests and round-trip benchmark, for smoke-test boots.
pub fn self_test() {
    endpoint::self_test();
    notification::self_test();
}
