//! The Oceans input protocol (ADR-0042), served by input drivers
//! (`usb-hid` on `input`) to their clients (a compositor, `mouse`).
//!
//! Reading input is a capability: only a holder of the service's endpoint
//! can subscribe. A **subscription** is a badged client end of that
//! endpoint, opened by sending a notification. The service queues every
//! event for every subscription and signals the notification when events
//! are waiting; the client then reads them, several per call, in the order
//! they happened. Nothing blocks the service: a client that stops reading
//! loses its oldest events, and is told how many.
//!
//! Events are [`oceans_input::Event`]s in their wire format
//! ([`oceans_input::EVENT_SIZE`] bytes each). Requests are IPC calls;
//! replies carry a [`Status`] label.

#![no_std]

use oceans_rt::{Error, Handle, rights};

use oceans_input::EVENT_SIZE;
pub use oceans_input::{Event, Kind};

/// Operations (request labels).
pub mod op {
    /// On the service endpoint: data = `[bits u64]`, handle = a
    /// notification (`SIGNAL`, `TRANSFER`) → a subscription handle. The
    /// service signals `bits` when events are waiting.
    pub const SUBSCRIBE: u64 = 1;
    /// On a subscription: → `[lost u32]` and up to [`super::MAX_EVENTS`]
    /// events, oldest first; `lost` counts events dropped since the last
    /// read because the queue was full. No events: an empty list.
    pub const READ: u64 = 2;
}

/// Events per `READ` reply.
pub const MAX_EVENTS: usize = 7;
/// Events a service keeps per subscription before dropping the oldest.
pub const QUEUE: usize = 64;
/// Subscriptions a service serves at once.
pub const MAX_SUBSCRIPTIONS: usize = 4;
/// The largest `READ` reply.
pub const READ_REPLY: usize = 4 + MAX_EVENTS * EVENT_SIZE;

/// Reply status (reply label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    /// Malformed request, or an operation on the wrong handle.
    BadRequest = 1,
    /// No more subscriptions.
    NoSpace = 2,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            2 => Self::NoSpace,
            _ => Self::BadRequest,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::BadRequest => "bad request",
            Self::NoSpace => "too many subscriptions",
        }
    }
}

/// Why a client call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    Ipc(Error),
    Status(Status),
}

impl InputError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Ipc(Error::PeerClosed) => "the input service has gone",
            Self::Ipc(_) => "the input service did not answer",
            Self::Status(status) => status.message(),
        }
    }
}

/// Events read by one call.
pub struct Batch {
    events: [Option<Event>; MAX_EVENTS],
    /// Events dropped before these because the client read too slowly.
    pub lost: u32,
}

impl Batch {
    /// The events, oldest first.
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.iter().flatten()
    }

    pub fn is_empty(&self) -> bool {
        self.events[0].is_none()
    }

    /// Decodes a `READ` reply; `None` if it is malformed.
    pub fn decode(reply: &[u8]) -> Option<Self> {
        let lost = u32::from_le_bytes(reply.get(..4)?.try_into().ok()?);
        let (records, rest) = reply[4..].as_chunks::<EVENT_SIZE>();
        if !rest.is_empty() || records.len() > MAX_EVENTS {
            return None;
        }
        let mut events = [None; MAX_EVENTS];
        for (slot, record) in events.iter_mut().zip(records) {
            *slot = Some(Event::decode(record)?);
        }
        Some(Self { events, lost })
    }
}

/// A subscription to an input service.
pub struct Subscription {
    handle: Handle,
}

impl Subscription {
    /// Subscribes to the service behind `input`; it will signal `bits` on
    /// `notification` (which the caller keeps, and may share with timers
    /// on other bits).
    pub fn new(input: Handle, notification: Handle, bits: u64) -> Result<Self, InputError> {
        let shared = oceans_rt::duplicate(notification, rights::SIGNAL | rights::TRANSFER)
            .map_err(InputError::Ipc)?;
        let mut handles = [Handle(0); 1];
        let got = oceans_rt::ipc_call_msg(
            input,
            op::SUBSCRIBE,
            &bits.to_le_bytes(),
            &[shared],
            &mut [],
            &mut handles,
        )
        .map_err(InputError::Ipc)?;
        match Status::from_label(got.label) {
            Status::Ok if got.handles_len == 1 => Ok(Self { handle: handles[0] }),
            Status::Ok => Err(InputError::Status(Status::BadRequest)),
            status => Err(InputError::Status(status)),
        }
    }

    /// The events waiting now (none is not an error).
    pub fn read(&self) -> Result<Batch, InputError> {
        let mut reply = [0u8; READ_REPLY];
        let got = oceans_rt::ipc_call_msg(self.handle, op::READ, &[], &[], &mut reply, &mut [])
            .map_err(InputError::Ipc)?;
        match Status::from_label(got.label) {
            Status::Ok => {
                Batch::decode(&reply[..got.data_len]).ok_or(InputError::Status(Status::BadRequest))
            }
            status => Err(InputError::Status(status)),
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.handle);
    }
}
