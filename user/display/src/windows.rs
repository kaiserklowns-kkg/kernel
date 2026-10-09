//! App windows (ADR-0059): the window endpoint's requests, the apps behind
//! them, and the pixel memory each window shares with its app. Where
//! windows are, which has the focus and whose events are queued is
//! `oceans_window::Manager`'s, and so is who may copy and paste (the
//! clipboard, ADR-0095); the text moves here.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use oceans_core_proto::{display_grant, field, op as core_op, parts};
use oceans_rt::{Handle, Received};
use oceans_window::proto::{
    Event, MAX_CLIPBOARD, MAX_EVENTS, NOTIFY_INTERVAL_MS, OpenRequest, Status, notification_text,
    op, open_name, pixel_bytes,
};

use super::{Service, prot, rights, say};

/// What new pixels show until the app draws: the toolkit's window colour
/// (ADR-0078).
pub const WINDOW_COLOUR: u32 = 0x00_f6_f6_f8;

/// An `OPENERS` reply: what one inline message carries.
const OPENERS_REPLY: usize = 256;
/// The ids of the apps that come with the system: a file opens in one of
/// them before any other (ADR-0099).
const SYSTEM_APPS: &str = "app.oceans.";

/// An app with windows: its name as Core verified it, and the
/// notification its events are signalled on.
pub struct Owner {
    pub app: String,
    /// `display_grant` bits: windows, notifications.
    pub grants: u8,
    pub notification: Option<(Handle, u64)>,
    /// When it last notified (ms since boot).
    pub notified_ms: Option<u64>,
}

/// A window's pixels: the memory shared with the app, mapped read-only
/// here.
pub struct Pixels {
    memory: Handle,
    address: *mut u8,
    /// Their size, which a window being resized may no longer have.
    pub width: usize,
    pub height: usize,
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

/// Pixel memory for a `width × height` window: mapped here to be read,
/// and an end for the app to draw through. `fill` paints it first.
fn share_pixels(width: u16, height: u16, fill: Option<u32>) -> Option<(Pixels, Handle)> {
    let size = pixel_bytes(width, height);
    let memory = oceans_rt::memory_create(size as u64).ok()?;
    if let Some(colour) = fill
        && let Ok(address) = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
    {
        // SAFETY: mapped, and `size` bytes large: `size / 4` pixels.
        unsafe { core::slice::from_raw_parts_mut(address.cast::<u32>(), size / 4) }.fill(colour);
        let _ = oceans_rt::memory_unmap(address);
    }
    let mapped = oceans_rt::memory_map(memory, 0, prot::READ);
    let theirs = oceans_rt::duplicate(
        memory,
        rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
    );
    match (mapped, theirs) {
        (Ok(address), Ok(theirs)) => Some((
            Pixels {
                memory,
                address,
                width: usize::from(width),
                height: usize::from(height),
            },
            theirs,
        )),
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
}

fn close_all(handles: &[Handle]) {
    for &handle in handles {
        let _ = oceans_rt::close(handle);
    }
}

/// The first `len` bytes of an app's memory object, copied here; `None`
/// if it is smaller or `len` passes the clipboard's limit.
fn read_shared(memory: Handle, len: usize) -> Option<Vec<u8>> {
    let size = oceans_rt::memory_size(memory).ok()? as usize;
    if len > size || len > MAX_CLIPBOARD {
        return None;
    }
    let address = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: the object is mapped, and at least `len` bytes large.
    let bytes = unsafe { core::slice::from_raw_parts(address, len) }.to_vec();
    let _ = oceans_rt::memory_unmap(address);
    Some(bytes)
}

/// `text` in a new memory object, as an end that can only be read and
/// mapped.
fn share_text(text: &str) -> Option<Handle> {
    let memory = oceans_rt::memory_create(text.len().max(1) as u64).ok()?;
    let theirs = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
        .ok()
        .and_then(|address| {
            // SAFETY: mapped, and at least `text.len()` bytes large.
            unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), address, text.len()) };
            let _ = oceans_rt::memory_unmap(address);
            oceans_rt::duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER).ok()
        });
    let _ = oceans_rt::close(memory);
    theirs
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
            let (&grants, rest) = reply[..got.len].split_first()?;
            let mut fields = parts(rest);
            let id = fields.next().unwrap_or("?");
            let _version = fields.next();
            let app = fields.next().filter(|name| !name.is_empty()).unwrap_or(id);
            say(self.log, format_args!("display: windows for {id} ({app})"));
            self.owners.insert(
                badge,
                Owner {
                    app: app.to_string(),
                    grants,
                    notification: None,
                    notified_ms: None,
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
                op::NOTIFY => {
                    close_all(handles);
                    self.notify(got.badge, data)
                }
                op::COPY => (self.copy(got.badge, data, handles), false),
                op::RESIZABLE => {
                    close_all(handles);
                    let parsed = window_of(data).zip(data.get(4..8));
                    match parsed {
                        Some((id, size)) => {
                            let w = u16::from_le_bytes([size[0], size[1]]);
                            let h = u16::from_le_bytes([size[2], size[3]]);
                            match self.windows.set_resizable(got.badge, id, w, h) {
                                // The zoom button lights up.
                                Ok(()) => (Status::Ok, true),
                                Err(status) => (status, false),
                            }
                        }
                        None => (Status::BadRequest, false),
                    }
                }
                op::RESIZE => {
                    close_all(handles);
                    return self.resize(got.badge, data);
                }
                op::NOW_PLAYING => {
                    close_all(handles);
                    match window_of(data)
                        .map(|id| self.windows.set_now_playing(got.badge, id, &data[4..]))
                    {
                        // The sound panel shows it.
                        Some(Ok(())) => (Status::Ok, true),
                        Some(Err(status)) => (status, false),
                        None => (Status::BadRequest, false),
                    }
                }
                op::MEDIA_KEYS => {
                    close_all(handles);
                    match window_of(data).map(|id| self.windows.want_media_keys(got.badge, id)) {
                        Some(Ok(())) => {
                            let app = self.owners.get(&got.badge).map_or("?", |o| o.app.as_str());
                            say(self.log, format_args!("display: media keys go to {app}"));
                            (Status::Ok, false)
                        }
                        Some(Err(status)) => (status, false),
                        None => (Status::BadRequest, false),
                    }
                }
                op::OPEN_FILE => {
                    close_all(handles);
                    self.open_file(got.badge, data)
                }
                op::OPENERS => {
                    close_all(handles);
                    let Some(name) = open_name(data) else {
                        let _ = oceans_rt::ipc_reply_msg(Status::BadRequest as u64, &[], &[]);
                        return false;
                    };
                    let mut reply = Vec::new();
                    for (id, app) in self.openers(name) {
                        let entry = alloc::format!("{id}\0{app}\0");
                        if reply.len() + entry.len() > OPENERS_REPLY {
                            break;
                        }
                        reply.extend_from_slice(entry.as_bytes());
                    }
                    let _ = oceans_rt::ipc_reply_msg(Status::Ok as u64, &reply, &[]);
                    return false;
                }
                op::PASTE => {
                    close_all(handles);
                    self.paste(got.badge);
                    return false;
                }
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
        let Some(owner) = self
            .owner(badge)
            .filter(|o| o.grants & display_grant::WINDOW != 0)
        else {
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
        let Some((pixels, theirs)) = share_pixels(request.width, request.height, None) else {
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

    /// The installed apps that open files like `name` (ADR-0099), as id and
    /// name: the system's own first, then the others in Core's order. The
    /// first is the one a file opens in unless the user chooses.
    pub(crate) fn openers(&self, name: &str) -> Vec<(String, String)> {
        let (Some(core), Some(kind)) = (self.core, oceans_package::extension(name)) else {
            return Vec::new();
        };
        let mut found: Vec<(String, String)> = Vec::new();
        let mut reply = [0u8; 256];
        for app in &self.desktop.apps {
            let Ok(got) = core.about(core_op::INFO, &[field::OPENS], &app.id, &mut reply) else {
                continue;
            };
            let kinds = core::str::from_utf8(&reply[..got.len]).unwrap_or("");
            if kinds
                .split_ascii_whitespace()
                .any(|k| k.eq_ignore_ascii_case(kind))
            {
                found.push((app.id.clone(), app.name.clone()));
            }
        }
        // Stable: the system's own apps keep Core's order among them.
        found.sort_by_key(|(id, _)| !id.starts_with(SYSTEM_APPS));
        found
    }

    /// `OPEN_FILE` (ADR-0099): the file in the app chosen, or the first
    /// that opens it, started with its name; once per key or click the
    /// user gave the caller.
    fn open_file(&mut self, badge: u64, data: &[u8]) -> (Status, bool) {
        let Some((&len, rest)) = data.split_first() else {
            return (Status::BadRequest, false);
        };
        let len = usize::from(len);
        let (Some(chosen), Some(name)) = (
            rest.get(..len).and_then(|id| core::str::from_utf8(id).ok()),
            rest.get(len..).and_then(open_name),
        ) else {
            return (Status::BadRequest, false);
        };
        if let Err(status) = self.windows.take_open(badge) {
            return (status, false);
        }
        let openers = self.openers(name);
        let app = if chosen.is_empty() {
            openers.first()
        } else {
            openers.iter().find(|(id, _)| id == chosen)
        };
        let Some((id, _)) = app.cloned() else {
            return (Status::NotFound, false);
        };
        let from = self.owners.get(&badge).map_or("?", |o| o.app.as_str());
        say(
            self.log,
            format_args!(
                "desktop: opening {name} with {id} ({}), for {from}",
                if chosen.is_empty() {
                    "the default"
                } else {
                    "chosen"
                }
            ),
        );
        self.launch(&id, name);
        (Status::Ok, true)
    }

    /// `RESIZE` (ADR-0097): new pixels at the window's size now, painted in
    /// the windows' colour until the app draws; the old ones go. `true`
    /// if the screen must change.
    fn resize(&mut self, badge: u64, data: &[u8]) -> bool {
        let reply = |status: Status| {
            let _ = oceans_rt::ipc_reply_msg(status as u64, &[], &[]);
            false
        };
        let Some(id) = window_of(data) else {
            return reply(Status::BadRequest);
        };
        let (width, height) = match self.windows.size(badge, id) {
            Ok(size) => size,
            Err(status) => return reply(status),
        };
        let Some((pixels, theirs)) = share_pixels(width, height, Some(WINDOW_COLOUR)) else {
            return reply(Status::NoMemory);
        };
        let mut size = [0u8; 4];
        size[..2].copy_from_slice(&width.to_le_bytes());
        size[2..].copy_from_slice(&height.to_le_bytes());
        if oceans_rt::ipc_reply_msg(Status::Ok as u64, &size, &[theirs]).is_err() {
            let _ = oceans_rt::close(theirs);
            pixels.release();
            return false;
        }
        if let Some(old) = self.pixels.insert(id, pixels) {
            old.release();
        }
        let app = self.owners.get(&badge).map_or("?", |o| o.app.as_str());
        say(
            self.log,
            format_args!("display: {app}'s window resized to {width}x{height}"),
        );
        true
    }

    /// `NOTIFY` (ADR-0065): a notification, after the app's verified name,
    /// at most one per `NOTIFY_INTERVAL_MS`.
    fn notify(&mut self, badge: u64, data: &[u8]) -> (Status, bool) {
        let Some(text) = notification_text(data) else {
            return (Status::BadRequest, false);
        };
        let now = oceans_rt::clock_ms();
        let log = self.log;
        let Some(owner) = self
            .owner(badge)
            .filter(|o| o.grants & display_grant::NOTIFICATIONS != 0)
        else {
            return (Status::NotAllowed, false);
        };
        if owner
            .notified_ms
            .is_some_and(|then| now.saturating_sub(then) < NOTIFY_INTERVAL_MS)
        {
            return (Status::TooMany, false);
        }
        owner.notified_ms = Some(now);
        let shown = alloc::format!("{}: {text}", owner.app);
        say(log, format_args!("desktop: notification: {shown}"));
        self.toast(shown, false);
        (Status::Ok, true)
    }

    /// `COPY` (ADR-0095): text from the app behind `badge` onto the
    /// clipboard, inline or in a memory object of its own, which is copied
    /// before it is looked at (the app could change it meanwhile). The
    /// text is never logged; who copied and how much is.
    fn copy(&mut self, badge: u64, data: &[u8], handles: &[Handle]) -> Status {
        let text = match handles {
            [] => Some(data.to_vec()),
            &[memory] => {
                let text = data
                    .get(..4)
                    .and_then(|len| len.try_into().ok())
                    .map(|len| u32::from_le_bytes(len) as usize)
                    .and_then(|len| read_shared(memory, len));
                let _ = oceans_rt::close(memory);
                text
            }
            _ => {
                close_all(handles);
                None
            }
        };
        let Some(text) = text else {
            return Status::BadRequest;
        };
        match self.windows.copy(badge, &text) {
            Ok(bytes) => {
                let app = self.owners.get(&badge).map_or("?", |o| o.app.as_str());
                say(
                    self.log,
                    format_args!("display: clipboard: {bytes} bytes copied from {app}"),
                );
                Status::Ok
            }
            Err(status) => status,
        }
    }

    /// `PASTE`: the clipboard's text, in a memory object only the app
    /// pasted into gets, once per paste.
    fn paste(&mut self, badge: u64) {
        let text = match self.windows.take_paste(badge) {
            Ok(text) => String::from(text),
            Err(status) => {
                let _ = oceans_rt::ipc_reply_msg(status as u64, &[], &[]);
                return;
            }
        };
        let Some(memory) = share_text(&text) else {
            let _ = oceans_rt::ipc_reply_msg(Status::NoMemory as u64, &[], &[]);
            return;
        };
        let len = (text.len() as u32).to_le_bytes();
        if oceans_rt::ipc_reply_msg(Status::Ok as u64, &len, &[memory]).is_err() {
            let _ = oceans_rt::close(memory);
            return;
        }
        let app = self.owners.get(&badge).map_or("?", |o| o.app.as_str());
        say(
            self.log,
            format_args!("display: clipboard: pasted into {app}"),
        );
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
