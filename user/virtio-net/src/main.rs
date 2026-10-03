//! virtio-net: the driver for virtio network devices (virtio 1.x, PCI
//! transport, ADR-0023).
//!
//! An ordinary service with exactly what `services.conf` grants: one device
//! capability (`grant = device:1af4:1041`), the endpoint it serves
//! (`provide = netdev`) and a log. It moves Ethernet frames, nothing more:
//! the network stack is the `net` service, its one client.
//!
//! - Receive: 32 DMA buffers stay posted to the receive queue. Completed
//!   ones wait until the stack asks (`RECV`); only then are they posted
//!   again, so a slow stack makes the device drop frames instead of the
//!   driver queueing without bound.
//! - Transmit: 32 DMA buffers; a frame is copied in and queued, and its
//!   buffer is reclaimed when the device is done.
//! - The device's MSI-X interrupt is a notification bound to the driver's
//!   endpoint, so one thread serves the stack and the device. When frames
//!   arrive the driver signals the stack's notification, never calling it:
//!   neither side can block the other.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_net_proto::netdev::{BUFFER_SIZE, MAX_FRAME, RX_AREA, TX_AREA, op};
use oceans_net_proto::{Mac, Status};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_virtio::{Buffer as Chain, Dma, Queue, Transport};

oceans_rt::entry!(main);

const F_MAC: u64 = 1 << 5;
/// `virtio_net_hdr_v1` (virtio 1.x always includes `num_buffers`).
const HEADER: usize = 12;
const SLOTS: usize = 32;
const SLOT_SIZE: usize = 2048;
const RECEIVE_QUEUE: u16 = 0;
const TRANSMIT_QUEUE: u16 = 1;
/// Notification bits: device interrupt, polling tick.
const IRQ: u64 = 1;
/// Polling interval without MSI-X.
const POLL_MS: u64 = 10;
const MTU: u16 = 1500;

const EXIT_BAD_START: i64 = 2;
const EXIT_DEVICE: i64 = 3;

struct Session {
    badge: u64,
    buffer: *mut u8,
    notification: Handle,
    bits: u64,
}

struct Driver {
    log: Handle,
    mac: Mac,
    rx: Queue,
    tx: Queue,
    rx_buffers: Dma,
    tx_buffers: Dma,
    /// Receive slot of each descriptor head.
    rx_slot: [u8; 128],
    tx_slot: [u8; 128],
    /// Completed receive slots and frame lengths, in arrival order.
    pending: [(u8, u16); SLOTS],
    pending_len: usize,
    free_tx: [u8; SLOTS],
    free_tx_len: usize,
    interrupts: bool,
    events: Handle,
    session: Option<Session>,
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_str("virtio-net: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(device), Some(server)) = (
        directory.find("log", "log"),
        directory.find_kind("device"),
        directory.find_kind("provide"),
    ) else {
        return EXIT_BAD_START;
    };
    let mut driver = match Driver::start(log, device, server) {
        Ok(driver) => driver,
        Err(problem) => {
            say(log, format_args!("{problem}"));
            return EXIT_DEVICE;
        }
    };
    let [a, b, c, d, e, f] = driver.mac;
    say(
        log,
        format_args!(
            "MAC {a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}, {}",
            if driver.interrupts {
                "MSI-X"
            } else {
                "polling"
            }
        ),
    );
    driver.serve(server)
}

impl Driver {
    fn start(log: Handle, device: Handle, server: Handle) -> Result<Self, &'static str> {
        let mut transport = Transport::open(device)?;
        let setup = (|| {
            let accepted = transport.negotiate(F_MAC)?;
            let mut mac = [0x02, 0, 0, 0, 0, 0x01]; // locally administered
            if accepted & F_MAC != 0 {
                transport
                    .config_read(0, &mut mac)
                    .ok_or("no MAC in the device configuration")?;
            }
            let events = oceans_rt::notification_create().map_err(|_| "no notification")?;
            oceans_rt::endpoint_bind(server, events).map_err(|_| "cannot bind events")?;
            let irq = transport.bind_interrupts(events, IRQ);
            let (rx, rx_irq) = transport.queue(RECEIVE_QUEUE, SLOTS as u16, irq)?;
            let (tx, tx_irq) = transport.queue(TRANSMIT_QUEUE, SLOTS as u16, irq)?;
            let rx_buffers = Dma::new(device, SLOTS * SLOT_SIZE)?;
            let tx_buffers = Dma::new(device, SLOTS * SLOT_SIZE)?;
            let mut driver = Self {
                log,
                mac,
                rx,
                tx,
                rx_buffers,
                tx_buffers,
                rx_slot: [0; 128],
                tx_slot: [0; 128],
                pending: [(0, 0); SLOTS],
                pending_len: 0,
                free_tx: core::array::from_fn(|i| i as u8),
                free_tx_len: SLOTS,
                interrupts: irq && rx_irq && tx_irq,
                events,
                session: None,
            };
            for slot in 0..SLOTS as u8 {
                driver.post_receive(slot)?;
            }
            transport.driver_ok();
            driver.rx.kick();
            Ok(driver)
        })();
        if setup.is_err() {
            transport.fail();
        }
        setup
    }

    fn post_receive(&mut self, slot: u8) -> Result<(), &'static str> {
        let head = self
            .rx
            .add(&[Chain {
                address: self.rx_buffers.device + (usize::from(slot) * SLOT_SIZE) as u64,
                len: SLOT_SIZE as u32,
                device_writes: true,
            }])
            .ok_or("receive queue full")?;
        self.rx_slot[usize::from(head)] = slot;
        Ok(())
    }

    /// Collects completed receive and transmit buffers.
    fn service_queues(&mut self) {
        while let Some((head, len)) = self.rx.pop_used() {
            let slot = self.rx_slot[usize::from(head)];
            let len = len as usize;
            if (HEADER + 14..=HEADER + MAX_FRAME).contains(&len) && self.pending_len < SLOTS {
                self.pending[self.pending_len] = (slot, (len - HEADER) as u16);
                self.pending_len += 1;
            } else {
                // Runt or oversized: drop it and give the buffer back.
                let _ = self.post_receive(slot);
                self.rx.kick();
            }
        }
        while let Some((head, _)) = self.tx.pop_used() {
            if self.free_tx_len < SLOTS {
                self.free_tx[self.free_tx_len] = self.tx_slot[usize::from(head)];
                self.free_tx_len += 1;
            }
        }
    }

    fn signal_session(&self) {
        if self.pending_len > 0
            && let Some(session) = &self.session
        {
            let _ = oceans_rt::notification_signal(session.notification, session.bits);
        }
    }

    fn serve(&mut self, server: Handle) -> i64 {
        let mut data = [0u8; 64];
        let mut handles = [Handle(0); 4];
        let mut next_badge = 1;
        loop {
            if !self.interrupts {
                let _ = oceans_rt::timer_set(self.events, IRQ, POLL_MS);
            }
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                Err(error) => {
                    say(self.log, format_args!("receive failed: {error:?}"));
                    return 4;
                }
            };
            if got.signals != 0 {
                self.service_queues();
                self.signal_session();
                continue;
            }
            if got.closed {
                if self.session.as_ref().is_some_and(|s| s.badge == got.badge)
                    && let Some(session) = self.session.take()
                {
                    let _ = oceans_rt::memory_unmap(session.buffer);
                    let _ = oceans_rt::close(session.notification);
                }
                continue;
            }
            let received = &handles[..got.handles_len];
            let in_session =
                got.badge != 0 && self.session.as_ref().is_some_and(|s| s.badge == got.badge);
            let mut reply = [0u8; 8];
            let mut reply_len = 0;
            let mut reply_handle = None;
            let mut kept = false;
            let status = match got.label {
                op::INFO => {
                    reply[..6].copy_from_slice(&self.mac);
                    reply[6..8].copy_from_slice(&MTU.to_le_bytes());
                    reply_len = 8;
                    Status::Ok
                }
                op::OPEN if got.badge == 0 && received.len() == 2 && got.data_len == 8 => {
                    let bits = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
                    match self.open(server, received, bits, next_badge) {
                        Ok(handle) => {
                            next_badge += 1;
                            kept = true;
                            reply_handle = Some(handle);
                            Status::Ok
                        }
                        Err(status) => status,
                    }
                }
                op::SEND if in_session => self.send(&data[..got.data_len]),
                op::RECV if in_session => {
                    let count = self.receive();
                    reply[..2].copy_from_slice(&count.to_le_bytes());
                    reply_len = 2;
                    Status::Ok
                }
                _ => Status::BadRequest,
            };
            if !kept {
                for &handle in received {
                    let _ = oceans_rt::close(handle);
                }
            }
            let reply_handles: &[Handle] = match &reply_handle {
                Some(handle) => core::slice::from_ref(handle),
                None => &[],
            };
            if oceans_rt::ipc_reply_msg(status as u64, &reply[..reply_len], reply_handles).is_err()
                && let Some(handle) = reply_handle
            {
                let _ = oceans_rt::close(handle);
            }
        }
    }

    fn open(
        &mut self,
        server: Handle,
        received: &[Handle],
        bits: u64,
        badge: u64,
    ) -> Result<Handle, Status> {
        if self.session.is_some() {
            return Err(Status::NoBuffers);
        }
        let (memory, notification) = (received[0], received[1]);
        let size = oceans_rt::memory_size(memory).map_err(|_| Status::BadRequest)? as usize;
        if size < BUFFER_SIZE || bits == 0 {
            return Err(Status::BadRequest);
        }
        let buffer = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
            .map_err(|_| Status::BadRequest)?;
        let handle = match oceans_rt::endpoint_mint(server, badge) {
            Ok(handle) => handle,
            Err(_) => {
                let _ = oceans_rt::memory_unmap(buffer);
                return Err(Status::NoBuffers);
            }
        };
        // The mapping keeps the memory; the notification handle is kept.
        let _ = oceans_rt::close(memory);
        self.session = Some(Session {
            badge,
            buffer,
            notification,
            bits,
        });
        // Frames that arrived before the stack connected.
        self.signal_session();
        Ok(handle)
    }

    /// Queues the frame at `[offset u32][len u16]` of the transmit area.
    fn send(&mut self, data: &[u8]) -> Status {
        let Some(session) = &self.session else {
            return Status::BadRequest;
        };
        if data.len() != 6 {
            return Status::BadRequest;
        }
        let offset = u32::from_le_bytes(data[..4].try_into().expect("4 bytes")) as usize;
        let len = usize::from(u16::from_le_bytes([data[4], data[5]]));
        let fits = offset
            .checked_add(len)
            .is_some_and(|end| offset >= TX_AREA.start && end <= TX_AREA.end);
        if !fits || !(14..=MAX_FRAME).contains(&len) {
            return Status::BadRequest;
        }
        let buffer = session.buffer;
        self.service_queues();
        if self.free_tx_len == 0 {
            return Status::NoBuffers;
        }
        self.free_tx_len -= 1;
        let slot = self.free_tx[self.free_tx_len];
        let at = usize::from(slot) * SLOT_SIZE;
        self.tx_buffers.copy_in(at, &[0u8; HEADER]);
        // SAFETY: `offset..offset + len` lies in the transmit area of the
        // session buffer (checked), mapped read-write while it lives.
        let frame = unsafe { core::slice::from_raw_parts(buffer.add(offset), len) };
        self.tx_buffers.copy_in(at + HEADER, frame);
        let head = self.tx.add(&[Chain {
            address: self.tx_buffers.device + at as u64,
            len: (HEADER + len) as u32,
            device_writes: false,
        }]);
        match head {
            Some(head) => {
                self.tx_slot[usize::from(head)] = slot;
                self.tx.kick();
                Status::Ok
            }
            None => {
                self.free_tx[self.free_tx_len] = slot;
                self.free_tx_len += 1;
                Status::NoBuffers
            }
        }
    }

    /// Copies waiting frames into the receive area as `[len u16][frame]`
    /// records and gives their buffers back to the device.
    fn receive(&mut self) -> u16 {
        self.service_queues();
        let Some(session) = &self.session else {
            return 0;
        };
        let buffer = session.buffer;
        let mut at = RX_AREA.start;
        let mut count = 0usize;
        while count < self.pending_len {
            let (slot, len) = self.pending[count];
            let len = usize::from(len);
            let end = at + 2 + len.next_multiple_of(2);
            if end > RX_AREA.end {
                break;
            }
            // SAFETY: `at..end` lies in the receive area of the session
            // buffer (checked), mapped read-write while it lives.
            let record = unsafe { core::slice::from_raw_parts_mut(buffer.add(at), 2 + len) };
            record[..2].copy_from_slice(&(len as u16).to_le_bytes());
            self.rx_buffers
                .copy_out(usize::from(slot) * SLOT_SIZE + HEADER, &mut record[2..]);
            at = end;
            count += 1;
        }
        for index in 0..count {
            let _ = self.post_receive(self.pending[index].0);
        }
        self.pending.copy_within(count..self.pending_len, 0);
        self.pending_len -= count;
        if count > 0 {
            self.rx.kick();
        }
        count as u16
    }
}
