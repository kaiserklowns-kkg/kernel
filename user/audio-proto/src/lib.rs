//! The Oceans audio protocol (ADR-0079, ADR-0087), shared by the sound
//! driver (`hda`) and its clients.
//!
//! As for disks (ADR-0021), samples move through shared memory: a client
//! opens a **session** by sending a memory object it has mapped and gets
//! back a session handle (a badged client end of the driver's endpoint).
//! `PLAY` names bytes of that buffer; the driver copies them into its own
//! DMA memory, waiting while its buffer is full, so a client that keeps
//! playing is paced by the sound itself.
//!
//! **Recording** (ADR-0087) is the mirror image: a client opens a
//! **capture session** (`OPEN` with [`open_flags::CAPTURE`], sending a
//! buffer the driver may write) and `RECORD` fills bytes of it with what
//! came in, waiting until there is enough. The input runs from the first
//! `RECORD` until `STOP` or the session closes; one capture session at a
//! time.
//!
//! One format: 48 kHz, 16-bit little-endian, two channels interleaved
//! ([`FORMAT`]). Clients convert.

#![no_std]

use oceans_rt::{Error, Handle, prot, rights};

/// Operations (request labels).
pub mod op {
    /// On the driver endpoint or a session: → a line of text naming the
    /// device and the output ("Intel HDA, codec 1af4:0022, line out").
    pub const INFO: u64 = 1;
    /// On the driver endpoint, carrying one memory object (`READ`, `MAP`,
    /// `TRANSFER`): → a session handle. Data: nothing (playing), or one
    /// byte of [`open_flags`](super::open_flags) (a capture session's
    /// memory also needs `WRITE`).
    pub const OPEN: u64 = 2;
    /// On a session: data = `[offset u32][len u32]`, a whole number of
    /// frames (4 bytes). Answered once the bytes are queued.
    pub const PLAY: u64 = 3;
    /// On a session: answered once everything queued has been played; the
    /// output then rests.
    pub const DRAIN: u64 = 4;
    /// On a session: what is queued is dropped, the output rests now. On
    /// a capture session: the input stops.
    pub const STOP: u64 = 5;
    /// On the driver endpoint or a session: → a line of text naming the
    /// input ("line in (pin 0x5, converter 0x4)"), or `IoError` if there
    /// is none (ADR-0087).
    pub const INPUT_INFO: u64 = 6;
    /// On a capture session: data = `[offset u32][len u32]` (as `PLAY`),
    /// whole frames. Answered once those bytes of the buffer hold what
    /// came in next; the input starts if it was not running. Sound that
    /// came in and was not taken within a second or so is dropped.
    pub const RECORD: u64 = 7;
}

/// `OPEN` flags.
pub mod open_flags {
    /// A capture session (ADR-0087).
    pub const CAPTURE: u8 = 1;
}

/// What `PLAY` takes and `RECORD` gives.
pub const FORMAT: &str = "48 kHz, 16-bit little-endian, stereo";
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// Bytes per frame (one sample of each channel).
pub const FRAME: usize = 4;
/// Largest session buffer a driver accepts.
pub const MAX_BUFFER: usize = 256 * 1024;

/// Reply status (reply label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum Status {
    Ok = 0,
    /// Malformed request, or an operation on the wrong handle.
    BadRequest = 1,
    /// Bytes beyond the session buffer, or not whole frames.
    OutOfRange = 2,
    /// No sound device, or it failed.
    IoError = 4,
    /// No more sessions.
    NoSpace = 6,
    /// Another session is recording.
    Busy = 7,
}

impl Status {
    pub fn from_label(label: u64) -> Self {
        match label {
            0 => Self::Ok,
            2 => Self::OutOfRange,
            4 => Self::IoError,
            6 => Self::NoSpace,
            7 => Self::Busy,
            _ => Self::BadRequest,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::BadRequest => "bad request",
            Self::OutOfRange => "out of range",
            Self::IoError => "no sound device, or it failed",
            Self::NoSpace => "too many sessions",
            Self::Busy => "another program is recording",
        }
    }
}

/// A `PLAY` or `RECORD` request: bytes of the session buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Play {
    pub offset: u32,
    pub len: u32,
}

impl Play {
    pub const SIZE: usize = 8;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[..4].copy_from_slice(&self.offset.to_le_bytes());
        out[4..].copy_from_slice(&self.len.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        Some(Self {
            offset: u32::from_le_bytes(bytes[..4].try_into().ok()?),
            len: u32::from_le_bytes(bytes[4..].try_into().ok()?),
        })
    }

    /// The bytes, if they are whole frames inside a buffer of `buffer`
    /// bytes.
    pub fn checked(&self, buffer: usize) -> Option<core::ops::Range<usize>> {
        let start = usize::try_from(self.offset).ok()?;
        let end = start.checked_add(usize::try_from(self.len).ok()?)?;
        (self.len > 0 && (self.len as usize).is_multiple_of(FRAME) && end <= buffer)
            .then_some(start..end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioError {
    /// The driver answered with this status.
    Status(Status),
    /// The IPC itself failed (e.g. the driver is gone).
    Ipc(Error),
}

impl AudioError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Status(status) => status.message(),
            Self::Ipc(Error::PeerClosed) => "audio service unavailable",
            Self::Ipc(_) => "audio request failed",
        }
    }
}

fn request(
    handle: Handle,
    op: u64,
    data: &[u8],
    send: &[Handle],
    reply: &mut [u8],
    reply_handles: &mut [Handle],
) -> Result<(usize, usize), AudioError> {
    let got = oceans_rt::ipc_call_msg(handle, op, data, send, reply, reply_handles)
        .map_err(AudioError::Ipc)?;
    match Status::from_label(got.label) {
        Status::Ok => Ok((got.data_len, got.handles_len)),
        status => Err(AudioError::Status(status)),
    }
}

/// The device behind `handle` (driver endpoint or session), in words.
pub fn info(handle: Handle, out: &mut [u8]) -> Result<usize, AudioError> {
    request(handle, op::INFO, &[], &[], out, &mut []).map(|(len, _)| len)
}

/// The input behind `handle` (driver endpoint or session), in words.
pub fn input_info(handle: Handle, out: &mut [u8]) -> Result<usize, AudioError> {
    request(handle, op::INPUT_INFO, &[], &[], out, &mut []).map(|(len, _)| len)
}

/// A capture session (ADR-0087): a buffer the driver writes what came in
/// into.
pub struct Input {
    session: Handle,
    buffer: *mut u8,
    size: usize,
}

impl Input {
    /// Opens a capture session with a `buffer_size`-byte buffer (at most
    /// [`MAX_BUFFER`]). `Busy` if another session is recording.
    pub fn open(driver: Handle, buffer_size: usize) -> Result<Self, AudioError> {
        let ipc = AudioError::Ipc;
        let memory = oceans_rt::memory_create(buffer_size as u64).map_err(ipc)?;
        let mapped = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let shared = oceans_rt::duplicate(
            memory,
            rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
        );
        let _ = oceans_rt::close(memory);
        let buffer = mapped.map_err(ipc)?;
        let fail = |error| {
            let _ = oceans_rt::memory_unmap(buffer);
            error
        };
        let shared = shared.map_err(ipc).map_err(fail)?;
        let mut session = [Handle(0); 1];
        let (_, count) = request(
            driver,
            op::OPEN,
            &[open_flags::CAPTURE],
            &[shared],
            &mut [],
            &mut session,
        )
        .map_err(fail)?;
        if count != 1 {
            return Err(fail(AudioError::Status(Status::BadRequest)));
        }
        Ok(Self {
            session: session[0],
            buffer,
            size: buffer_size,
        })
    }

    /// Fills `len` bytes of the buffer from `offset` with what comes in
    /// next: returns once they are there.
    pub fn record(&self, offset: u32, len: u32) -> Result<(), AudioError> {
        let data = Play { offset, len }.encode();
        request(self.session, op::RECORD, &data, &[], &mut [], &mut []).map(drop)
    }

    /// The buffer, as the last `record` left it.
    pub fn buffer(&self) -> &[u8] {
        // SAFETY: `buffer` maps `size` bytes for as long as `self` lives;
        // the driver only writes it during our calls.
        unsafe { core::slice::from_raw_parts(self.buffer, self.size) }
    }

    /// Stops the input.
    pub fn stop(&self) -> Result<(), AudioError> {
        request(self.session, op::STOP, &[], &[], &mut [], &mut []).map(drop)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.session);
        let _ = oceans_rt::memory_unmap(self.buffer);
    }
}

/// A session: a buffer shared with the driver.
pub struct Output {
    session: Handle,
    buffer: *mut u8,
    size: usize,
}

impl Output {
    /// Opens a session with a `buffer_size`-byte buffer (at most
    /// [`MAX_BUFFER`]).
    pub fn open(driver: Handle, buffer_size: usize) -> Result<Self, AudioError> {
        let ipc = AudioError::Ipc;
        let memory = oceans_rt::memory_create(buffer_size as u64).map_err(ipc)?;
        let mapped = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let shared = oceans_rt::duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER);
        // The mapping keeps the object alive; the driver gets `shared`.
        let _ = oceans_rt::close(memory);
        let buffer = mapped.map_err(ipc)?;
        let fail = |error| {
            let _ = oceans_rt::memory_unmap(buffer);
            error
        };
        let shared = shared.map_err(ipc).map_err(fail)?;
        let mut session = [Handle(0); 1];
        let (_, count) =
            request(driver, op::OPEN, &[], &[shared], &mut [], &mut session).map_err(fail)?;
        if count != 1 {
            return Err(fail(AudioError::Status(Status::BadRequest)));
        }
        Ok(Self {
            session: session[0],
            buffer,
            size: buffer_size,
        })
    }

    /// The shared buffer.
    pub fn buffer(&mut self) -> &mut [u8] {
        // SAFETY: `buffer` maps `size` bytes read-write for as long as
        // `self` lives; the driver only reads it during our calls.
        unsafe { core::slice::from_raw_parts_mut(self.buffer, self.size) }
    }

    /// Plays `len` bytes of the buffer from `offset`: returns once they are
    /// queued.
    pub fn play(&self, offset: u32, len: u32) -> Result<(), AudioError> {
        let data = Play { offset, len }.encode();
        request(self.session, op::PLAY, &data, &[], &mut [], &mut []).map(drop)
    }

    /// Waits until everything queued has been played.
    pub fn drain(&self) -> Result<(), AudioError> {
        request(self.session, op::DRAIN, &[], &[], &mut [], &mut []).map(drop)
    }

    pub fn stop(&self) -> Result<(), AudioError> {
        request(self.session, op::STOP, &[], &[], &mut [], &mut []).map(drop)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = oceans_rt::close(self.session);
        let _ = oceans_rt::memory_unmap(self.buffer);
    }
}
