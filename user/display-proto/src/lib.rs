//! The client side of the Oceans window protocol (ADR-0059): an app given
//! `window` opens windows through its `use windows` end, draws into their
//! shared pixels and takes their events. The wire format is
//! [`oceans_window::proto`].

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
