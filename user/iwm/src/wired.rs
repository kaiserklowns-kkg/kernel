//! The wired network device, when the machine has one: iwm stands between
//! it and the stack (ADR-0129). Until Wi-Fi joins a network, the stack's
//! frames go to the wire through it, as `net` would send them; frames from
//! the wire come back the same way.

use oceans_net_proto::Status;
use oceans_net_proto::netdev::{self, BUFFER_SIZE, MAX_FRAME, RX_AREA, TX_AREA};
use oceans_rt::{Handle, prot, rights};

pub struct Wired {
    session: Handle,
    buffer: *mut u8,
    pub mac: [u8; 6],
}

impl Wired {
    /// A session with the wired driver at `driver`, which signals `bits` on
    /// `events` when frames wait.
    pub fn connect(driver: Handle, events: Handle, bits: u64) -> Result<Self, &'static str> {
        let mut info = [0u8; 8];
        let got = oceans_rt::ipc_call_msg(driver, netdev::op::INFO, &[], &[], &mut info, &mut [])
            .map_err(|_| "no wired driver running")?;
        if got.label != Status::Ok as u64 || got.data_len < 6 {
            return Err("the wired driver gave no address");
        }
        let mac: [u8; 6] = info[..6].try_into().map_err(|_| "bad address")?;
        let memory = oceans_rt::memory_create(BUFFER_SIZE as u64).map_err(|_| "no memory")?;
        let buffer = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let shared = oceans_rt::duplicate(
            memory,
            rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
        );
        let _ = oceans_rt::close(memory);
        let buffer = buffer.map_err(|_| "cannot map the frame buffer")?;
        let shared = shared.map_err(|_| "cannot share the frame buffer")?;
        let signal = oceans_rt::duplicate(events, rights::SIGNAL | rights::TRANSFER)
            .map_err(|_| "cannot share the notification")?;
        let mut session = [Handle(0); 1];
        let got = oceans_rt::ipc_call_msg(
            driver,
            netdev::op::OPEN,
            &bits.to_le_bytes(),
            &[shared, signal],
            &mut [],
            &mut session,
        )
        .map_err(|_| "the wired driver refused a session")?;
        if got.label != Status::Ok as u64 || got.handles_len != 1 {
            return Err("the wired driver refused a session");
        }
        Ok(Self {
            session: session[0],
            buffer,
            mac,
        })
    }

    /// Sends one Ethernet frame on the wire.
    pub fn send(&self, frame: &[u8]) -> Status {
        if frame.len() > TX_AREA.end - TX_AREA.start || frame.len() > MAX_FRAME {
            return Status::BadRequest;
        }
        // SAFETY: the transmit area of the mapped session buffer, which
        // only this thread writes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                self.buffer.add(TX_AREA.start),
                frame.len(),
            );
        }
        let mut request = [0u8; 6];
        request[..4].copy_from_slice(&(TX_AREA.start as u32).to_le_bytes());
        request[4..].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        match oceans_rt::ipc_call_msg(
            self.session,
            netdev::op::SEND,
            &request,
            &[],
            &mut [],
            &mut [],
        ) {
            Ok(got) if got.label == Status::Ok as u64 => Status::Ok,
            Ok(_) => Status::NoBuffers,
            Err(_) => Status::NoDevice,
        }
    }

    /// Hands each frame waiting on the wire to `each`.
    pub fn receive(&self, mut each: impl FnMut(&[u8])) {
        loop {
            let mut reply = [0u8; 2];
            let Ok(got) = oceans_rt::ipc_call_msg(
                self.session,
                netdev::op::RECV,
                &[],
                &[],
                &mut reply,
                &mut [],
            ) else {
                return;
            };
            let count = u16::from_le_bytes(reply);
            if got.label != Status::Ok as u64 || count == 0 {
                return;
            }
            // SAFETY: the receive area of the mapped session buffer, which
            // the driver wrote during the call that just returned.
            let area = unsafe {
                core::slice::from_raw_parts(
                    self.buffer.add(RX_AREA.start),
                    RX_AREA.end - RX_AREA.start,
                )
            };
            let mut at = 0;
            for _ in 0..count {
                let Some(len) = area
                    .get(at..at + 2)
                    .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
                else {
                    return;
                };
                let Some(frame) = area.get(at + 2..at + 2 + len) else {
                    return;
                };
                each(frame);
                at += 2 + len.next_multiple_of(2);
            }
        }
    }
}
