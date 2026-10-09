//! The client side of the Oceans window protocol (ADR-0059): an app given
//! `window` opens windows through its `use windows` end, draws into their
//! shared pixels and takes their events, and copies to and pastes from the
//! clipboard (ADR-0095). The wire format is [`oceans_window::proto`].

#![no_std]

pub use oceans_window::proto::{self, Event, Status, kind};

use oceans_rt::{Error, Handle, prot, rights};
use proto::{MAX_EVENTS, OpenRequest, op};

/// Why a window request failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// The call itself failed (the display service is gone, …).
    Ipc(Error),
    Refused(Status),
}

/// A window of this app's, mapped for drawing.
pub struct Window {
    windows: Handle,
    pub id: u32,
    pub width: usize,
    pub height: usize,
    pixels: *mut u32,
}

impl Window {
    /// Opens a window of `width × height` pixels through `windows` (the
    /// `use windows` end); events for it signal `bits` on `notification`.
    pub fn open(
        windows: Handle,
        notification: Handle,
        bits: u64,
        width: u16,
        height: u16,
        title: &str,
    ) -> Result<Self, WindowError> {
        let (data, len) = OpenRequest {
            bits,
            width,
            height,
            title,
        }
        .encode()
        .ok_or(WindowError::Refused(Status::BadRequest))?;
        let shared = oceans_rt::duplicate(notification, rights::SIGNAL | rights::TRANSFER)
            .map_err(WindowError::Ipc)?;
        let mut reply = [0u8; 8];
        let mut handles = [Handle(0); 1];
        let got = oceans_rt::ipc_call_msg(
            windows,
            op::OPEN,
            &data[..len],
            &[shared],
            &mut reply,
            &mut handles,
        )
        .map_err(WindowError::Ipc)?;
        let status = Status::from_label(got.label);
        if status != Status::Ok || got.handles_len != 1 || got.data_len < 4 {
            for &handle in &handles[..got.handles_len] {
                let _ = oceans_rt::close(handle);
            }
            return Err(WindowError::Refused(if status == Status::Ok {
                Status::BadRequest
            } else {
                status
            }));
        }
        let memory = handles[0];
        let mapped = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let _ = oceans_rt::close(memory);
        Ok(Self {
            windows,
            id: u32::from_le_bytes([reply[0], reply[1], reply[2], reply[3]]),
            width: usize::from(width),
            height: usize::from(height),
            pixels: mapped.map_err(WindowError::Ipc)?.cast(),
        })
    }

    /// The pixels, row after row (`0x00RRGGBB`).
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: the display service made the object at least this large
        // (`proto::pixel_bytes`), and it stays mapped while `self` lives.
        unsafe { core::slice::from_raw_parts_mut(self.pixels, self.width * self.height) }
    }

    /// Lets the user resize and maximize the window (ADR-0097), down to
    /// `min_width × min_height`. The app then takes [`kind::RESIZE`]
    /// events with [`Window::resize`].
    pub fn set_resizable(&self, min_width: u16, min_height: u16) -> Result<(), WindowError> {
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&self.id.to_le_bytes());
        data[4..6].copy_from_slice(&min_width.to_le_bytes());
        data[6..].copy_from_slice(&min_height.to_le_bytes());
        let got =
            oceans_rt::ipc_call_msg(self.windows, op::RESIZABLE, &data, &[], &mut [], &mut [])
                .map_err(WindowError::Ipc)?;
        match Status::from_label(got.label) {
            Status::Ok => Ok(()),
            status => Err(WindowError::Refused(status)),
        }
    }

    /// After a [`kind::RESIZE`] event: pixels at the window's new size, in
    /// place of the old ones (`width` and `height` follow). Draw and
    /// present them.
    pub fn resize(&mut self) -> Result<(), WindowError> {
        let mut reply = [0u8; 4];
        let mut handles = [Handle(0); 1];
        let got = oceans_rt::ipc_call_msg(
            self.windows,
            op::RESIZE,
            &self.id.to_le_bytes(),
            &[],
            &mut reply,
            &mut handles,
        )
        .map_err(WindowError::Ipc)?;
        let status = Status::from_label(got.label);
        if status != Status::Ok || got.handles_len != 1 || got.data_len < 4 {
            for &handle in &handles[..got.handles_len] {
                let _ = oceans_rt::close(handle);
            }
            return Err(WindowError::Refused(if status == Status::Ok {
                Status::BadRequest
            } else {
                status
            }));
        }
        let width = u16::from_le_bytes([reply[0], reply[1]]);
        let height = u16::from_le_bytes([reply[2], reply[3]]);
        let memory = handles[0];
        let size = oceans_rt::memory_size(memory).unwrap_or(0) as usize;
        let mapped = if size >= proto::pixel_bytes(width, height) {
            oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
        } else {
            Err(oceans_rt::Error::InvalidArgument)
        };
        let _ = oceans_rt::close(memory);
        let address = mapped.map_err(WindowError::Ipc)?;
        let _ = oceans_rt::memory_unmap(self.pixels.cast());
        self.pixels = address.cast();
        self.width = usize::from(width);
        self.height = usize::from(height);
        Ok(())
    }

    /// Shows what was drawn.
    pub fn present(&self) -> Result<(), WindowError> {
        self.simple(op::PRESENT)
    }

    /// Closes the window.
    pub fn close(self) -> Result<(), WindowError> {
        let result = self.simple(op::CLOSE);
        let _ = oceans_rt::memory_unmap(self.pixels.cast());
        result
    }

    fn simple(&self, label: u64) -> Result<(), WindowError> {
        let got = oceans_rt::ipc_call_msg(
            self.windows,
            label,
            &self.id.to_le_bytes(),
            &[],
            &mut [],
            &mut [],
        )
        .map_err(WindowError::Ipc)?;
        match Status::from_label(got.label) {
            Status::Ok => Ok(()),
            status => Err(WindowError::Refused(status)),
        }
    }
}

/// Takes queued events for this app's windows into `out`; returns how
/// many (0: none).
pub fn events(windows: Handle, out: &mut [Event]) -> Result<usize, WindowError> {
    let mut data = [0u8; MAX_EVENTS * Event::SIZE];
    let got = oceans_rt::ipc_call_msg(windows, op::EVENTS, &[], &[], &mut data, &mut [])
        .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => {}
        status => return Err(WindowError::Refused(status)),
    }
    let (received, _) = data[..got.data_len.min(data.len())].as_chunks::<{ Event::SIZE }>();
    let mut count = 0;
    let decoded = received.iter().filter_map(|bytes| Event::decode(bytes));
    for (slot, event) in out.iter_mut().zip(decoded) {
        *slot = event;
        count += 1;
    }
    Ok(count)
}

/// Shows `text` as a notification on the desktop, after the app's name
/// (ADR-0065; needs `notifications`). One line, at most
/// `proto::MAX_NOTIFICATION` bytes; one per `proto::NOTIFY_INTERVAL_MS`.
pub fn notify(windows: Handle, text: &str) -> Result<(), WindowError> {
    let got = oceans_rt::ipc_call_msg(windows, op::NOTIFY, text.as_bytes(), &[], &mut [], &mut [])
        .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok(()),
        status => Err(WindowError::Refused(status)),
    }
}

/// Puts `text` on the clipboard (ADR-0095). The display takes it only
/// while this app's window has the focus and the user gave it a key or a
/// click since its last copy (`Refused(NotAllowed)` otherwise): copy in
/// answer to the user's Ctrl+C, never on your own.
pub fn copy(windows: Handle, text: &str) -> Result<(), WindowError> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > proto::MAX_CLIPBOARD {
        return Err(WindowError::Refused(Status::BadRequest));
    }
    let got = if bytes.len() <= oceans_abi::IPC_MAX_INLINE {
        oceans_rt::ipc_call_msg(windows, op::COPY, bytes, &[], &mut [], &mut [])
    } else {
        let memory = oceans_rt::memory_create(bytes.len() as u64).map_err(WindowError::Ipc)?;
        let filled = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE).map(|address| {
            // SAFETY: mapped, and at least `bytes.len()` bytes large.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), address, bytes.len()) };
            let _ = oceans_rt::memory_unmap(address);
        });
        let shared = filled.and_then(|()| {
            oceans_rt::duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER)
        });
        let _ = oceans_rt::close(memory);
        let shared = shared.map_err(WindowError::Ipc)?;
        let len = (bytes.len() as u32).to_le_bytes();
        oceans_rt::ipc_call_msg(windows, op::COPY, &len, &[shared], &mut [], &mut [])
    }
    .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok(()),
        status => Err(WindowError::Refused(status)),
    }
}

/// After a [`kind::PASTE`] event: the clipboard's text, handed to `take`
/// (once per paste; `Refused(NotFound)` for a second try).
pub fn paste<T>(windows: Handle, take: impl FnOnce(&str) -> T) -> Result<T, WindowError> {
    let mut reply = [0u8; 4];
    let mut handles = [Handle(0); 1];
    let got = oceans_rt::ipc_call_msg(windows, op::PASTE, &[], &[], &mut reply, &mut handles)
        .map_err(WindowError::Ipc)?;
    let status = Status::from_label(got.label);
    if status != Status::Ok || got.handles_len != 1 || got.data_len < 4 {
        for &handle in &handles[..got.handles_len] {
            let _ = oceans_rt::close(handle);
        }
        return Err(WindowError::Refused(if status == Status::Ok {
            Status::BadRequest
        } else {
            status
        }));
    }
    let memory = handles[0];
    let len = u32::from_le_bytes(reply) as usize;
    let size = oceans_rt::memory_size(memory).unwrap_or(0) as usize;
    let mapped = oceans_rt::memory_map(memory, 0, prot::READ);
    let _ = oceans_rt::close(memory);
    let address = mapped.map_err(WindowError::Ipc)?;
    // SAFETY: mapped, and `len` is checked against the object's size.
    let bytes = unsafe { core::slice::from_raw_parts(address, len.min(size)) };
    let result = core::str::from_utf8(bytes)
        .map(take)
        .map_err(|_| WindowError::Refused(Status::BadRequest));
    let _ = oceans_rt::memory_unmap(address);
    result
}

/// Opens the file `name` (in Home: `folder/file.txt`) in the app with id
/// `app`, or in the one that opens its kind first (ADR-0099). Only in
/// answer to the user's key or click (`Refused(NotAllowed)` otherwise);
/// `Refused(NotFound)` when no app (or not that one) opens it.
pub fn open_file(windows: Handle, app: Option<&str>, name: &str) -> Result<(), WindowError> {
    let app = app.unwrap_or("");
    let mut data = [0u8; 1 + 64 + proto::MAX_OPEN_NAME];
    if app.len() > 64 || name.len() > proto::MAX_OPEN_NAME {
        return Err(WindowError::Refused(Status::BadRequest));
    }
    data[0] = app.len() as u8;
    data[1..1 + app.len()].copy_from_slice(app.as_bytes());
    let end = 1 + app.len() + name.len();
    data[1 + app.len()..end].copy_from_slice(name.as_bytes());
    let got = oceans_rt::ipc_call_msg(windows, op::OPEN_FILE, &data[..end], &[], &mut [], &mut [])
        .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok(()),
        status => Err(WindowError::Refused(status)),
    }
}

/// The apps that open files like `name` (ADR-0099): `each(id, name)` for
/// each, the one [`open_file`] would choose first.
pub fn openers(
    windows: Handle,
    name: &str,
    mut each: impl FnMut(&str, &str),
) -> Result<(), WindowError> {
    let mut reply = [0u8; 256];
    let got = oceans_rt::ipc_call_msg(
        windows,
        op::OPENERS,
        name.as_bytes(),
        &[],
        &mut reply,
        &mut [],
    )
    .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => {}
        status => return Err(WindowError::Refused(status)),
    }
    let text = core::str::from_utf8(&reply[..got.data_len.min(reply.len())]).unwrap_or("");
    let mut parts = text.split('\0');
    while let (Some(id), Some(app)) = (parts.next(), parts.next()) {
        if !id.is_empty() {
            each(id, app);
        }
    }
    Ok(())
}

/// The media keys (Play/Pause, Stop, Previous, Next; ADR-0102) come to
/// window `id` as key events from now on, whatever has the focus, until
/// another app asks for them. For players.
pub fn want_media_keys(windows: Handle, id: u32) -> Result<(), WindowError> {
    let got = oceans_rt::ipc_call_msg(windows, op::MEDIA_KEYS, &id.to_le_bytes(), &[], &mut [], &mut [])
        .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok(()),
        status => Err(WindowError::Refused(status)),
    }
}

/// Says what window `id` plays (ADR-0103), for the desktop's sound panel:
/// `state` from [`proto::playing`] and the title (at most
/// [`proto::MAX_NOW_PLAYING`] bytes). Only after [`want_media_keys`].
pub fn now_playing(windows: Handle, id: u32, state: u8, title: &str) -> Result<(), WindowError> {
    let title = title.as_bytes();
    let mut data = [0u8; 5 + proto::MAX_NOW_PLAYING];
    if title.len() > proto::MAX_NOW_PLAYING {
        return Err(WindowError::Refused(Status::BadRequest));
    }
    data[..4].copy_from_slice(&id.to_le_bytes());
    data[4] = state;
    data[5..5 + title.len()].copy_from_slice(title);
    let got = oceans_rt::ipc_call_msg(windows, op::NOW_PLAYING, &data[..5 + title.len()], &[], &mut [], &mut [])
        .map_err(WindowError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok(()),
        status => Err(WindowError::Refused(status)),
    }
}
