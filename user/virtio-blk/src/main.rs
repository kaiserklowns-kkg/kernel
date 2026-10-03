//! virtio-blk: the block device driver for virtio disks (virtio 1.x, PCI
//! transport, ADR-0021).
//!
//! An ordinary service. Its authority is exactly what init grants in
//! `services.conf`: one device capability (`grant = device:1af4:1042`),
//! the endpoint it serves (`provide = block`) and a log. Through the device
//! capability it maps the virtio register BAR, allocates DMA memory for the
//! virtqueue and a bounce buffer, and receives completions as MSI-X
//! interrupts on a notification (`oceans-virtio` does the transport). It
//! speaks the block protocol (`oceans-block-proto`) to clients, copying
//! between their session buffers and its DMA buffer, so no client ever
//! sees a physical address.
//!
//! One request is in flight at a time: simple, and plenty for a disk that
//! serves one filesystem. Without MSI-X it falls back to polling.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;

use oceans_block_proto::{Info, SECTOR_SIZE, Status, Transfer, info_flags, op};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_virtio::{Buffer as Chain, Dma, Queue, Transport};

oceans_rt::entry!(main);

mod feature {
    pub const BLK_RO: u64 = 1 << 5;
    pub const BLK_FLUSH: u64 = 1 << 9;
}

/// Request types.
const REQUEST_IN: u32 = 0;
const REQUEST_OUT: u32 = 1;
const REQUEST_FLUSH: u32 = 4;

/// Queue entries used (one request needs three descriptors).
const QUEUE_SIZE: u16 = 16;
/// The request page: header, then status byte.
const HEADER_OFFSET: usize = 0;
const STATUS_OFFSET: usize = 16;
/// Bounce buffer: the most one device request moves.
const BOUNCE_SIZE: usize = 64 * 1024;
const MAX_SESSIONS: usize = 16;

/// Exit codes.
const EXIT_BAD_START: i64 = 2;
const EXIT_DEVICE: i64 = 3;

struct Driver {
    log: Handle,
    queue: Queue,
    request: Dma,
    bounce: Dma,
    /// `None`: polling.
    irq: Option<Handle>,
    info: Info,
    flush: bool,
}

#[derive(Clone, Copy)]
struct Session {
    badge: u64,
    buffer: *mut u8,
    size: usize,
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
    let mut driver = match Driver::start(log, device) {
        Ok(driver) => driver,
        Err(problem) => {
            say(log, format_args!("virtio-blk: {problem}"));
            return EXIT_DEVICE;
        }
    };
    let info = driver.info;
    say(
        log,
        format_args!(
            "virtio-blk: {} sectors ({} MiB){}, {}",
            info.sectors,
            info.bytes() >> 20,
            if info.read_only() { ", read-only" } else { "" },
            if driver.irq.is_some() {
                "MSI-X"
            } else {
                "polling"
            }
        ),
    );
    driver.serve(server)
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

impl Driver {
    fn start(log: Handle, device: Handle) -> Result<Self, &'static str> {
        let mut transport = Transport::open(device)?;
        let setup = (|| {
            let accepted = transport.negotiate(feature::BLK_RO | feature::BLK_FLUSH)?;
            let irq = oceans_rt::notification_create()
                .ok()
                .filter(|&n| transport.bind_interrupts(n, 1));
            let (queue, interrupts) = transport.queue(0, QUEUE_SIZE, irq.is_some())?;
            let request = Dma::new(device, 4096)?;
            let bounce = Dma::new(device, BOUNCE_SIZE)?;
            let mut capacity = [0u8; 8];
            transport
                .config_read(0, &mut capacity)
                .ok_or("no capacity in the device configuration")?;
            transport.driver_ok();
            Ok(Self {
                log,
                queue,
                request,
                bounce,
                irq: irq.filter(|_| interrupts),
                info: Info {
                    sectors: u64::from_le_bytes(capacity),
                    sector_size: SECTOR_SIZE as u32,
                    flags: if accepted & feature::BLK_RO != 0 {
                        info_flags::READ_ONLY
                    } else {
                        0
                    },
                },
                flush: accepted & feature::BLK_FLUSH != 0,
            })
        })();
        if setup.is_err() {
            transport.fail();
        }
        setup
    }

    /// Runs one device request on the bounce buffer and waits for it.
    fn request(&mut self, kind: u32, sector: u64, len: usize) -> Result<(), Status> {
        self.request.write(HEADER_OFFSET, kind);
        self.request.write(HEADER_OFFSET + 4, 0u32);
        self.request.write(HEADER_OFFSET + 8, sector);
        self.request.write(STATUS_OFFSET, 0xffu8);
        let header = Chain {
            address: self.request.device + HEADER_OFFSET as u64,
            len: 16,
            device_writes: false,
        };
        let data = Chain {
            address: self.bounce.device,
            len: len as u32,
            device_writes: kind == REQUEST_IN,
        };
        let status = Chain {
            address: self.request.device + STATUS_OFFSET as u64,
            len: 1,
            device_writes: true,
        };
        let head = if len == 0 {
            self.queue.add(&[header, status])
        } else {
            self.queue.add(&[header, data, status])
        }
        .ok_or(Status::IoError)?;
        self.queue.kick();

        let mut spins = 0u32;
        loop {
            match self.queue.pop_used() {
                Some((used, _)) if used == head => break,
                Some(_) => continue, // nothing else is in flight
                None => {}
            }
            match self.irq {
                // Latched: a completion before the wait is not lost.
                Some(irq) => {
                    let _ = oceans_rt::notification_wait(irq);
                }
                None => {
                    spins += 1;
                    if spins.is_multiple_of(64) {
                        oceans_rt::sleep_ms(1);
                    } else {
                        oceans_rt::yield_now();
                    }
                }
            }
        }
        match self.request.read::<u8>(STATUS_OFFSET) {
            0 => Ok(()),
            2 => Err(Status::Unsupported),
            _ => Err(Status::IoError),
        }
    }

    /// Moves sectors between the disk and a session buffer, through the
    /// bounce buffer, in chunks.
    fn transfer(&mut self, session: &Session, write: bool, data: &[u8]) -> Status {
        let Some(request) = Transfer::decode(data) else {
            return Status::BadRequest;
        };
        let Some(len) = request.checked_len(self.info.sectors, session.size) else {
            return Status::OutOfRange;
        };
        if write && self.info.read_only() {
            return Status::ReadOnly;
        }
        let mut done = 0;
        while done < len {
            let chunk = (len - done).min(BOUNCE_SIZE);
            let sector = request.sector + (done / SECTOR_SIZE) as u64;
            // SAFETY: `offset + len` lies inside the session buffer
            // (`checked_len`), mapped read-write while the session lives;
            // the bounce buffer holds BOUNCE_SIZE bytes.
            let client = unsafe { session.buffer.add(request.offset as usize + done) };
            let result = if write {
                unsafe { ptr::copy_nonoverlapping(client, self.bounce.virt, chunk) };
                self.request(REQUEST_OUT, sector, chunk)
            } else {
                self.request(REQUEST_IN, sector, chunk)
                    .inspect(|()| unsafe {
                        ptr::copy_nonoverlapping(self.bounce.virt, client, chunk);
                    })
            };
            if let Err(status) = result {
                return status;
            }
            done += chunk;
        }
        Status::Ok
    }

    fn serve(&mut self, server: Handle) -> i64 {
        let mut sessions: [Option<Session>; MAX_SESSIONS] = [None; MAX_SESSIONS];
        let mut next_badge = 1;
        let mut data = [0u8; 64];
        let mut handles = [Handle(0); 4];
        loop {
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                Err(error) => {
                    say(
                        self.log,
                        format_args!("virtio-blk: receive failed: {error:?}"),
                    );
                    return 4;
                }
            };
            let find = |badge| {
                sessions
                    .iter()
                    .position(|s| s.is_some_and(|s: Session| s.badge == badge))
            };
            if got.closed {
                if let Some(index) = find(got.badge)
                    && let Some(session) = sessions[index].take()
                {
                    let _ = oceans_rt::memory_unmap(session.buffer);
                }
                continue;
            }
            let received = &handles[..got.handles_len];
            let session = find(got.badge).and_then(|i| sessions[i]);
            let mut reply_handle = None;
            let mut reply_data = [0u8; Info::SIZE];
            let mut reply_len = 0;
            let status = match (got.label, session) {
                (op::INFO, _) => {
                    reply_data = self.info.encode();
                    reply_len = Info::SIZE;
                    Status::Ok
                }
                (op::OPEN, None) if got.badge == 0 && received.len() == 1 => {
                    match open_session(server, received[0], next_badge, &mut sessions) {
                        Ok(handle) => {
                            next_badge += 1;
                            reply_handle = Some(handle);
                            Status::Ok
                        }
                        Err(status) => status,
                    }
                }
                (op::READ, Some(session)) => self.transfer(&session, false, &data[..got.data_len]),
                (op::WRITE, Some(session)) => self.transfer(&session, true, &data[..got.data_len]),
                (op::FLUSH, Some(_)) if self.flush => self
                    .request(REQUEST_FLUSH, 0, 0)
                    .err()
                    .unwrap_or(Status::Ok),
                // Without the flush feature, writes are durable on completion.
                (op::FLUSH, Some(_)) => Status::Ok,
                _ => Status::BadRequest,
            };
            // Capabilities sent with a request we did not take are closed.
            if !matches!(got.label, op::OPEN) || status != Status::Ok {
                for &handle in received {
                    let _ = oceans_rt::close(handle);
                }
            }
            let reply: &[Handle] = match &reply_handle {
                Some(handle) => core::slice::from_ref(handle),
                None => &[],
            };
            if oceans_rt::ipc_reply_msg(status as u64, &reply_data[..reply_len], reply).is_err()
                && let Some(handle) = reply_handle
            {
                let _ = oceans_rt::close(handle);
            }
        }
    }
}

/// Maps a client's buffer and mints its session handle.
fn open_session(
    server: Handle,
    memory: Handle,
    badge: u64,
    sessions: &mut [Option<Session>; MAX_SESSIONS],
) -> Result<Handle, Status> {
    let slot = sessions
        .iter()
        .position(Option::is_none)
        .ok_or(Status::NoSpace)?;
    let size = oceans_rt::memory_size(memory).map_err(|_| Status::BadRequest)? as usize;
    if size == 0 || size > oceans_block_proto::MAX_BUFFER || !size.is_multiple_of(SECTOR_SIZE) {
        return Err(Status::BadRequest);
    }
    let buffer = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
        .map_err(|_| Status::BadRequest)?;
    // The mapping keeps the memory; the handle is not needed.
    let _ = oceans_rt::close(memory);
    match oceans_rt::endpoint_mint(server, badge) {
        Ok(handle) => {
            sessions[slot] = Some(Session {
                badge,
                buffer,
                size,
            });
            Ok(handle)
        }
        Err(_) => {
            let _ = oceans_rt::memory_unmap(buffer);
            Err(Status::NoSpace)
        }
    }
}
