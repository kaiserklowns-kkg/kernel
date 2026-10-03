//! usb-storage: the USB mass storage class driver (Bulk-Only Transport,
//! SCSI; ADR-0034).
//!
//! An ordinary service holding what `services.conf` grants: the USB
//! service's endpoint (`use = usb`), the endpoint it serves (`provide =
//! usbdisk`) and a log. It holds no device capability: the xhci driver
//! hands it one mass storage interface (`CLAIM`) and moves its bulk
//! transfers through a shared buffer, so this driver can reach that
//! interface and nothing else.
//!
//! It serves the block protocol (ADR-0021) on `usbdisk`, like virtio-blk on
//! `block`, so the same clients (the filesystem, `disk`) work with a USB
//! stick. With no stick present requests fail with an I/O error; a stick
//! plugged in later is picked up when xhci signals.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;

use oceans_block_proto::{Info, SECTOR_SIZE, Status, Transfer, op};
use oceans_rt::{Buffer, Directory, Handle, Start, prot, rights};
use oceans_usb::service::{self, Claim};
use oceans_usb::storage::{self, Capacity, Command, Direction, Inquiry, Sense};

oceans_rt::entry!(main);

/// The buffer shared with xhci: data, then the command and status
/// wrappers.
const DATA: usize = service::MAX_TRANSFER;
const CBW_AT: usize = DATA;
const CSW_AT: usize = DATA + 64;
const SHARED: usize = DATA + 4096;
/// Notification bit: xhci says a stick came or went.
const USB_EVENT: u64 = 1;
const MAX_SESSIONS: usize = 4;
/// Attempts at TEST UNIT READY while a device spins up.
const READY_ATTEMPTS: u32 = 20;

const EXIT_BAD_START: i64 = 2;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<200>::new();
    let _ = line.write_str("usb-storage: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

#[derive(Clone, Copy)]
struct Session {
    badge: u64,
    buffer: *mut u8,
    size: usize,
}

/// A claimed stick.
struct Disk {
    session: Handle,
    claim: Claim,
    capacity: Capacity,
}

/// Why a command failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    /// The device reported CHECK CONDITION (see REQUEST SENSE).
    Check,
    /// Transport trouble; the device was reset.
    Transport,
    /// The stick was unplugged.
    Gone,
}

struct Driver {
    log: Handle,
    usb: Handle,
    notification: Handle,
    memory: Handle,
    shared: *mut u8,
    disk: Option<Disk>,
    tag: u32,
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(usb), Some(server)) = (
        directory.find("log", "log"),
        directory.find("use", "usb"),
        directory.find_kind("provide"),
    ) else {
        return EXIT_BAD_START;
    };
    let setup = (|| {
        let memory = oceans_rt::memory_create(SHARED as u64).ok()?;
        let shared = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE).ok()?;
        let notification = oceans_rt::notification_create().ok()?;
        oceans_rt::endpoint_bind(server, notification).ok()?;
        Some((memory, shared, notification))
    })();
    let Some((memory, shared, notification)) = setup else {
        say(log, format_args!("cannot set up"));
        return EXIT_BAD_START;
    };
    let mut driver = Driver {
        log,
        usb,
        notification,
        memory,
        shared,
        disk: None,
        tag: 0,
    };
    driver.find_disk();
    driver.serve(server)
}

impl Driver {
    // ---- The stick -------------------------------------------------------

    /// Claims a stick if xhci has one (otherwise xhci will signal).
    fn find_disk(&mut self) {
        let claimed = (|| {
            let memory = oceans_rt::duplicate(
                self.memory,
                rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
            )
            .ok()?;
            let notification =
                oceans_rt::duplicate(self.notification, rights::SIGNAL | rights::TRANSFER).ok()?;
            let mut data = [0u8; 12];
            data[..3].copy_from_slice(&[
                storage::CLASS,
                storage::SUBCLASS_SCSI,
                storage::PROTOCOL_BOT,
            ]);
            data[4..].copy_from_slice(&USB_EVENT.to_le_bytes());
            let mut reply = [0u8; Claim::SIZE];
            let mut handles = [Handle(0); 1];
            let got = oceans_rt::ipc_call_msg(
                self.usb,
                service::CLAIM,
                &data,
                &[memory, notification],
                &mut reply,
                &mut handles,
            )
            .ok()?;
            (got.label == service::OK && got.handles_len == 1)
                .then(|| Claim::decode(&reply[..got.data_len]).map(|claim| (claim, handles[0])))
                .flatten()
        })();
        let Some((claim, session)) = claimed else {
            return;
        };
        match self.start_disk(claim, session) {
            Ok(disk) => self.disk = Some(disk),
            Err(problem) => {
                say(self.log, format_args!("port {}: {problem}", claim.path()));
                let _ = oceans_rt::close(session);
            }
        }
    }

    /// Readies a claimed stick: LUN 0, identity, ready, capacity.
    fn start_disk(&mut self, claim: Claim, session: Handle) -> Result<Disk, &'static str> {
        let mut disk = Disk {
            session,
            claim,
            capacity: Capacity {
                blocks: 0,
                block_size: 0,
            },
        };
        // Multiple LUNs (card readers) are not used: LUN 0 only. Devices
        // with one LUN may stall the request.
        let _ = self.control(&disk, storage::get_max_lun(claim.interface));
        let inquiry = self
            .run(&disk, &Command::inquiry())
            .ok()
            .and_then(|_| Inquiry::parse(self.data(36)))
            .ok_or("INQUIRY failed")?;
        if inquiry.device_type != 0 {
            return Err("not a block device");
        }
        let mut ready = false;
        for _ in 0..READY_ATTEMPTS {
            match self.run(&disk, &Command::test_unit_ready()) {
                Ok(_) => {
                    ready = true;
                    break;
                }
                Err(Failure::Gone) => return Err("unplugged"),
                // Unit attention after reset, or still becoming ready.
                Err(_) => {
                    let sense = self.sense(&disk);
                    if sense.is_some_and(|s| {
                        s.key == storage::SENSE_NOT_READY && s.code == storage::ASC_NO_MEDIUM
                    }) {
                        return Err("no medium");
                    }
                    oceans_rt::sleep_ms(100);
                }
            }
        }
        if !ready {
            return Err("the device never became ready");
        }
        let capacity = match self.run(&disk, &Command::read_capacity_10()) {
            Ok(_) => storage::capacity_10(self.data(8)),
            Err(_) => None,
        };
        let capacity = match capacity {
            Some(capacity) => capacity,
            None => {
                self.run(&disk, &Command::read_capacity_16())
                    .map_err(|_| "READ CAPACITY failed")?;
                storage::capacity_16(self.data(32)).ok_or("bad capacity")?
            }
        };
        disk.capacity = capacity;
        let mib = (capacity.blocks * u64::from(capacity.block_size)) >> 20;
        say(
            self.log,
            format_args!(
                "port {}: {} {}, {mib} MiB ({} blocks of {} bytes){}",
                claim.path(),
                inquiry.vendor(),
                inquiry.product(),
                capacity.blocks,
                capacity.block_size,
                if capacity.block_size as usize == SECTOR_SIZE {
                    ""
                } else {
                    "; only 512-byte blocks are served"
                }
            ),
        );
        Ok(disk)
    }

    fn sense(&mut self, disk: &Disk) -> Option<Sense> {
        self.run(disk, &Command::request_sense()).ok()?;
        Sense::parse(self.data(18))
    }

    fn data(&self, len: usize) -> &[u8] {
        // SAFETY: `shared` maps SHARED bytes; DATA ≥ len for every caller.
        unsafe { core::slice::from_raw_parts(self.shared, len.min(DATA)) }
    }

    fn shared_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        assert!(offset + len <= SHARED);
        // SAFETY: inside the shared mapping (checked); xhci touches it only
        // during our calls.
        unsafe { core::slice::from_raw_parts_mut(self.shared.add(offset), len) }
    }

    // ---- Transport -------------------------------------------------------

    fn call(&self, disk: &Disk, label: u64, data: &[u8]) -> Result<usize, u64> {
        let mut reply = [0u8; 4];
        match oceans_rt::ipc_call_msg(disk.session, label, data, &[], &mut reply, &mut []) {
            Ok(got) if got.label == service::OK => Ok(u32::from_le_bytes(reply) as usize),
            Ok(got) => Err(got.label),
            Err(_) => Err(service::GONE),
        }
    }

    fn bulk(&self, disk: &Disk, address: u8, offset: usize, len: usize) -> Result<usize, u64> {
        let mut data = [0u8; 12];
        data[0] = address;
        data[4..8].copy_from_slice(&(offset as u32).to_le_bytes());
        data[8..].copy_from_slice(&(len as u32).to_le_bytes());
        self.call(disk, service::BULK, &data)
    }

    fn clear_halt(&self, disk: &Disk, address: u8) -> Result<usize, u64> {
        self.call(disk, service::CLEAR_HALT, &[address])
    }

    fn control(&self, disk: &Disk, setup: oceans_usb::request::Setup) -> Result<usize, u64> {
        let mut data = [0u8; 12];
        data[..8].copy_from_slice(&setup.to_bytes());
        self.call(disk, service::CONTROL, &data)
    }

    /// Reset recovery (BOT §5.3.4): reset, then clear both halts.
    fn reset_recovery(&self, disk: &Disk) {
        let _ = self.control(disk, storage::reset(disk.claim.interface));
        let _ = self.clear_halt(disk, disk.claim.bulk_in);
        let _ = self.clear_halt(disk, disk.claim.bulk_out);
    }

    /// Runs one command: CBW, data (in the shared buffer's data area),
    /// CSW. Returns the bytes the data stage moved.
    fn run(&mut self, disk: &Disk, command: &Command) -> Result<usize, Failure> {
        let gone = |label: u64| {
            if label == service::GONE {
                Failure::Gone
            } else {
                Failure::Transport
            }
        };
        self.tag = self.tag.wrapping_add(1);
        let tag = self.tag;
        let wrapper = storage::cbw(tag, 0, command);
        self.shared_mut(CBW_AT, storage::CBW_SIZE)
            .copy_from_slice(&wrapper);
        if let Err(label) = self.bulk(disk, disk.claim.bulk_out, CBW_AT, storage::CBW_SIZE) {
            if label != service::GONE {
                self.reset_recovery(disk);
            }
            return Err(gone(label));
        }
        let len = command.transfer as usize;
        let mut moved = 0;
        if len > 0 {
            let endpoint = match command.direction {
                Direction::In => disk.claim.bulk_in,
                _ => disk.claim.bulk_out,
            };
            match self.bulk(disk, endpoint, 0, len) {
                Ok(count) => moved = count,
                // A stalled data stage still ends with a status wrapper.
                Err(service::STALL) => {
                    let _ = self.clear_halt(disk, endpoint);
                }
                Err(label) => {
                    if label != service::GONE {
                        self.reset_recovery(disk);
                    }
                    return Err(gone(label));
                }
            }
        }
        let mut status = self.bulk(disk, disk.claim.bulk_in, CSW_AT, storage::CSW_SIZE);
        if status == Err(service::STALL) {
            let _ = self.clear_halt(disk, disk.claim.bulk_in);
            status = self.bulk(disk, disk.claim.bulk_in, CSW_AT, storage::CSW_SIZE);
        }
        let got = match status {
            Ok(got) => got,
            Err(label) => {
                if label != service::GONE {
                    self.reset_recovery(disk);
                }
                return Err(gone(label));
            }
        };
        let mut csw = [0u8; storage::CSW_SIZE];
        let got = got.min(storage::CSW_SIZE);
        csw[..got].copy_from_slice(self.shared_mut(CSW_AT, got));
        match storage::csw(&csw[..got], tag, command.transfer) {
            Some(storage::Status::Passed { residue }) => Ok(moved.min(len - residue as usize)),
            Some(storage::Status::Failed { .. }) => Err(Failure::Check),
            Some(storage::Status::PhaseError) | None => {
                self.reset_recovery(disk);
                Err(Failure::Transport)
            }
        }
    }

    /// Reads or writes `count` 512-byte blocks at `block`, between the
    /// client's buffer and the stick, 64 KiB at a time.
    fn transfer(&mut self, client: &Session, write: bool, request: &Transfer) -> Status {
        let Some(disk) = self.disk.take() else {
            return Status::IoError;
        };
        let result = (|| {
            if disk.capacity.block_size as usize != SECTOR_SIZE {
                return Err(Status::Unsupported);
            }
            let len = request
                .checked_len(disk.capacity.blocks, client.size)
                .ok_or(Status::OutOfRange)?;
            let per_command = DATA / SECTOR_SIZE;
            let mut done = 0usize;
            while done < len {
                let blocks = ((len - done) / SECTOR_SIZE).min(per_command);
                let bytes = blocks * SECTOR_SIZE;
                let block = request.sector + (done / SECTOR_SIZE) as u64;
                let at = request.offset as usize + done;
                let command = if write {
                    // SAFETY: `at..at + bytes` is inside the client's
                    // mapped buffer (checked_len).
                    let from = unsafe { core::slice::from_raw_parts(client.buffer.add(at), bytes) };
                    self.shared_mut(0, bytes).copy_from_slice(from);
                    Command::write(block, blocks as u32, SECTOR_SIZE as u32)
                } else {
                    Command::read(block, blocks as u32, SECTOR_SIZE as u32)
                };
                match self.run(&disk, &command) {
                    Ok(moved) if moved == bytes => {}
                    Err(Failure::Gone) => return Err(Status::IoError),
                    _ => {
                        let _ = self.sense(&disk);
                        return Err(Status::IoError);
                    }
                }
                if !write {
                    // SAFETY: as above.
                    unsafe { ptr::copy_nonoverlapping(self.shared, client.buffer.add(at), bytes) };
                }
                done += bytes;
            }
            Ok(())
        })();
        self.disk = Some(disk);
        match result {
            Ok(()) => Status::Ok,
            Err(status) => status,
        }
    }

    fn flush(&mut self) -> Status {
        let Some(disk) = self.disk.take() else {
            return Status::IoError;
        };
        let result = self.run(&disk, &Command::synchronize_cache());
        self.disk = Some(disk);
        match result {
            // Sticks without a cache may refuse the command: nothing to
            // flush then.
            Ok(_) | Err(Failure::Check) => Status::Ok,
            Err(_) => Status::IoError,
        }
    }

    /// xhci signalled: a stick came or went. A gone stick's session
    /// answers GONE. Returns whether a stick went away.
    fn usb_event(&mut self) -> bool {
        let mut removed = false;
        if let Some(disk) = self.disk.take() {
            match self.run(&disk, &Command::test_unit_ready()) {
                Err(Failure::Gone) => {
                    say(
                        self.log,
                        format_args!("port {}: disk removed", disk.claim.path()),
                    );
                    let _ = oceans_rt::close(disk.session);
                    removed = true;
                }
                _ => {
                    self.disk = Some(disk);
                    return false;
                }
            }
        }
        self.find_disk();
        removed
    }

    fn info(&self) -> Option<Info> {
        let disk = self.disk.as_ref()?;
        Some(Info {
            sectors: disk.capacity.blocks,
            sector_size: disk.capacity.block_size,
            flags: 0,
        })
    }

    // ---- Clients ---------------------------------------------------------

    fn serve(&mut self, server: Handle) -> i64 {
        let mut sessions: [Option<Session>; MAX_SESSIONS] = [None; MAX_SESSIONS];
        let mut next_badge = 1;
        let mut data = [0u8; 64];
        let mut handles = [Handle(0); 4];
        loop {
            let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
                Ok(got) => got,
                Err(oceans_rt::Error::PeerClosed) => return 0,
                Err(error) => {
                    say(self.log, format_args!("receive failed: {error:?}"));
                    return 4;
                }
            };
            if got.signals != 0 {
                // Sessions belong to one stick: when it goes, they end, so
                // a client never writes a volume onto the next stick.
                if self.usb_event() {
                    for session in sessions.iter_mut().filter_map(Option::take) {
                        let _ = oceans_rt::memory_unmap(session.buffer);
                    }
                }
                continue;
            }
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
            let mut kept = false;
            let status = match (got.label, session) {
                // A session of a removed stick learns it here.
                (op::INFO, None) if got.badge != 0 => Status::BadRequest,
                (op::INFO, _) => match self.info() {
                    Some(info) => {
                        reply_data = info.encode();
                        reply_len = Info::SIZE;
                        Status::Ok
                    }
                    None => Status::IoError,
                },
                (op::OPEN, None) if got.badge == 0 && received.len() == 1 => {
                    match open_session(server, received[0], next_badge, &mut sessions) {
                        Ok(handle) => {
                            next_badge += 1;
                            reply_handle = Some(handle);
                            kept = true;
                            Status::Ok
                        }
                        Err(status) => status,
                    }
                }
                (op::READ | op::WRITE, Some(session)) => {
                    match Transfer::decode(&data[..got.data_len]) {
                        Some(request) => self.transfer(&session, got.label == op::WRITE, &request),
                        None => Status::BadRequest,
                    }
                }
                (op::FLUSH, Some(_)) => self.flush(),
                _ => Status::BadRequest,
            };
            if !kept {
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
            Err(Status::BadRequest)
        }
    }
}
