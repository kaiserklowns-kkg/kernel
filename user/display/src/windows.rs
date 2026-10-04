//! App windows (ADR-0059): the window endpoint's requests, the apps behind
//! them, and the pixel memory each window shares with its app. Where
//! windows are, which has the focus and whose events are queued is
//! `oceans_window::Manager`'s.

use alloc::string::{String, ToString};

use oceans_core_proto::{op as core_op, parts};
use oceans_rt::{Handle, Received};
use oceans_window::proto::{Event, MAX_EVENTS, OpenRequest, Status, op, pixel_bytes};

use super::{Service, prot, rights, say};

/// An app with windows: its name as Core verified it, and the
/// notification its events are signalled on.
pub struct Owner {
    pub app: String,
    pub notification: Option<(Handle, u64)>,
}

/// A window's pixels: the memory shared with the app, mapped read-only
/// here.
pub struct Pixels {
    memory: Handle,
    address: *mut u8,
}

impl Pixels {
    pub fn pixels(&self) -> *const u32 {
        self.address.cast_const().cast()
    }

    fn release(self) {
        let _ = oceans_rt::memory_unmap(self.address);
        let _ = oceans_rt::close(self.memory);
    }
}

fn close_all(handles: &[Handle]) {
    for &handle in handles {
        let _ = oceans_rt::close(handle);
    }
}

fn window_of(data: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
}

impl Service {
    /// Hands Core the window endpoint (`WINDOWS`), as a server end it can
    /// only mint client ends of.
    pub(crate) fn register_windows(&self, server: Handle) {
        let Some(core) = self.core else {
            return say(
                self.log,
                format_args!("display: no Oceans Core; apps get no windows"),
            );
        };
        let registered = oceans_rt::duplicate(server, rights::MANAGE | rights::TRANSFER)
            .ok()
            .is_some_and(|end| core.call(core_op::WINDOWS, &[], &[end], &mut []).is_ok());
        if !registered {
            say(
                self.log,
                format_args!("display: Core did not take the window endpoint; apps get no windows"),
            );
        }
    }

    /// The app behind `badge`, asked of Core the first time.
    fn owner(&mut self, badge: u64) -> Option<&mut Owner> {
        if !self.owners.contains_key(&badge) {
            let core = self.core?;
            let mut reply = [0u8; 256];
            let got = core
                .call(core_op::WINDOW_OWNER, &badge.to_le_bytes(), &[], &mut reply)
                .ok()?;
            let mut fields = parts(&reply[..got.len]);
            let id = fields.next().unwrap_or("?");
            let _version = fields.next();
            let app = fields.next().filter(|name| !name.is_empty()).unwrap_or(id);
            say(self.log, format_args!("display: windows for {id} ({app})"));
            self.owners.insert(
                badge,
                Owner {
                    app: app.to_string(),
                    notification: None,
                },
            );
        }
        self.owners.get_mut(&badge)
    }

    /// One call on the window endpoint; answers it. `true` if the screen
    /// must change.
    pub(crate) fn window_request(
        &mut self,
        got: &Received,
        data: &[u8],
        handles: &[Handle],
    ) -> bool {
        let data = &data[..got.data_len.min(data.len())];
        // Only ends Core minted for apps are badged; our own is not used.
        let (status, dirty) = if got.badge == 0 {
            close_all(handles);
            (Status::BadRequest, false)
        } else {
            match got.label {
                op::OPEN => return self.open(got.badge, data, handles),
                op::EVENTS => {
                    close_all(handles);
                    let events = self.windows.take_events(got.badge, MAX_EVENTS);
                    let mut reply = [0u8; MAX_EVENTS * Event::SIZE];
                    for (slot, event) in reply
                        .as_chunks_mut::<{ Event::SIZE }>()
                        .0
                        .iter_mut()
                        .zip(&events)
                    {
                        slot.copy_from_slice(&event.encode());
                    }
                    let _ = oceans_rt::ipc_reply_msg(
                        Status::Ok as u64,
                        &reply[..events.len() * Event::SIZE],
                        &[],
                    );
                    return false;
                }
                op::PRESENT => {
                    close_all(handles);
                    match window_of(data).map(|id| self.windows.present(got.badge, id)) {
                        Some(Ok(())) => (Status::Ok, true),
                        Some(Err(status)) => (status, false),
                        None => (Status::BadRequest, false),
                    }
                }
                op::CLOSE => {
                    close_all(handles);
                    match window_of(data).map(|id| (id, self.windows.close(got.badge, id))) {
                        Some((id, Ok(()))) => {
                            if let Some(pixels) = self.pixels.remove(&id) {
                                pixels.release();
                            }
                            (Status::Ok, true)
                        }
                        Some((_, Err(status))) => (status, false),
                        None => (Status::BadRequest, false),
                    }
                }
                _ => {
                    close_all(handles);
                    (Status::BadRequest, false)
                }
            }
        };
        let _ = oceans_rt::ipc_reply_msg(status as u64, &[], &[]);
        dirty
    }

    /// `OPEN`: a window for the app behind `badge`, if Core knows it.
    fn open(&mut self, badge: u64, data: &[u8], handles: &[Handle]) -> bool {
        let refuse = |status: Status| {
            close_all(handles);
            let _ = oceans_rt::ipc_reply_msg(status as u64, &[], &[]);
            false
        };
        let (Some(request), &[notification]) = (OpenRequest::decode(data), handles) else {
            return refuse(Status::BadRequest);
        };
        let Some(owner) = self.owner(badge) else {
            return refuse(Status::NotAllowed);
        };
        let app = owner.app.clone();
        let id = match self
            .windows
            .open(badge, &app, request.title, request.width, request.height)
        {
            Ok(id) => id,
            Err(status) => return refuse(status),
        };
        let size = pixel_bytes(request.width, request.height) as u64;
        let shared = oceans_rt::memory_create(size).ok().and_then(|memory| {
            let mapped = oceans_rt::memory_map(memory, 0, prot::READ);
            let theirs = oceans_rt::duplicate(
                memory,
                rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
            );
            match (mapped, theirs) {
                (Ok(address), Ok(theirs)) => Some((Pixels { memory, address }, theirs)),
                (mapped, theirs) => {
                    if let Ok(address) = mapped {
                        let _ = oceans_rt::memory_unmap(address);
                    }
                    if let Ok(theirs) = theirs {
                        let _ = oceans_rt::close(theirs);
                    }
                    let _ = oceans_rt::close(memory);
                    None
                }
            }
        });
        let Some((pixels, theirs)) = shared else {
            let _ = self.windows.close(badge, id);
            return refuse(Status::NoMemory);
        };
        if oceans_rt::ipc_reply_msg(Status::Ok as u64, &id.to_le_bytes(), &[theirs]).is_err() {
            let _ = oceans_rt::close(theirs);
            let _ = self.windows.close(badge, id);
            pixels.release();
            let _ = oceans_rt::close(notification);
            return false;
        }
        self.pixels.insert(id, pixels);
        if let Some(owner) = self.owners.get_mut(&badge)
            && let Some((old, _)) = owner.notification.replace((notification, request.bits))
        {
            let _ = oceans_rt::close(old);
        }
        true
    }

    /// The app behind `badge` is gone: its windows close. `true` if it had
    /// any.
    pub(crate) fn forget_owner(&mut self, badge: u64) -> bool {
        let closed = self.windows.close_owner(badge);
        for id in &closed {
            if let Some(pixels) = self.pixels.remove(id) {
                pixels.release();
            }
        }
        if let Some(owner) = self.owners.remove(&badge)
            && let Some((notification, _)) = owner.notification
        {
            let _ = oceans_rt::close(notification);
        }
        !closed.is_empty()
    }

    /// Signals the apps that have new events.
    pub(crate) fn signal_owners(&mut self) {
        for badge in self.windows.take_signals() {
            if let Some((notification, bits)) =
                self.owners.get(&badge).and_then(|owner| owner.notification)
            {
                let _ = oceans_rt::notification_signal(notification, bits);
            }
        }
    }
}
