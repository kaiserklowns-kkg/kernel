//! usb-hid: the USB pointer class driver (HID mice and tablets; ADR-0042).
//!
//! An ordinary service holding what `services.conf` grants: the USB
//! service's endpoint (`use = usb`), the endpoint it serves (`provide =
//! input`) and a log. Like usb-storage (ADR-0034) it holds no device
//! capability: the xhci driver hands it pointer interfaces (`CLAIM` with
//! [`service::POINTER`]) and queues their interrupt reports, which this
//! driver fetches (`REPORTS`) when xhci signals.
//!
//! For each pointer it reads the report descriptor and finds the buttons,
//! X, Y and wheels in it (`oceans_usb::pointer`); a boot mouse is switched
//! to the boot protocol and decoded as such. Reports become input events
//! (`oceans_input::Tracker`), which go to every subscriber of the input
//! protocol (`oceans_input_proto`) on `input`. Reading pointer input
//! therefore takes that endpoint: a capability init hands out explicitly.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_input::{Axes, EVENT_SIZE, Event, Tracker};
use oceans_input_proto::{MAX_EVENTS, MAX_SUBSCRIPTIONS, QUEUE, READ_REPLY, Status, op};
use oceans_rt::{Buffer, Directory, Handle, Start, prot, rights};
use oceans_usb::pointer::{self, Layout};
use oceans_usb::request::Setup;
use oceans_usb::service::{self, Claim, Reports};

oceans_rt::entry!(main);

/// The buffer shared with xhci (one memory object for every claim): it
/// receives report descriptors.
const SHARED: usize = service::MIN_POINTER_BUFFER;
/// Notification bit: xhci has reports, or a pointer came or went.
const USB_EVENT: u64 = 1;
const MAX_POINTERS: usize = 4;

const EXIT_BAD_START: i64 = 2;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<200>::new();
    let _ = line.write_str("usb-hid: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// How a pointer's reports are read: the boot protocol's fixed report
/// (HID 1.11 appendix B.2), or the layout its report descriptor gives.
/// With neither, the claim is kept (so it is not claimed over and over)
/// and its reports are dropped.
struct Decoder {
    boot: bool,
    layout: Option<Layout>,
}

impl Decoder {
    fn decode(&self, report: &[u8]) -> Option<oceans_input::Sample> {
        match &self.layout {
            _ if self.boot => pointer::boot_report(report),
            Some(layout) => layout.decode(report),
            None => None,
        }
    }

    fn axes(&self) -> Axes {
        match &self.layout {
            Some(layout) if !self.boot => layout.axes(),
            _ => Axes::Relative,
        }
    }
}

/// A claimed pointer.
struct Pointer {
    session: Handle,
    claim: Claim,
    /// Its number in events.
    device: u8,
    decoder: Decoder,
    tracker: Tracker,
}

/// A client reading events.
struct Subscriber {
    badge: u64,
    notification: Handle,
    bits: u64,
    queue: [Event; QUEUE],
    head: usize,
    len: usize,
    lost: u32,
}

impl Subscriber {
    fn push(&mut self, event: Event) {
        if self.len == QUEUE {
            self.head = (self.head + 1) % QUEUE;
            self.len -= 1;
            self.lost = self.lost.saturating_add(1);
        }
        self.queue[(self.head + self.len) % QUEUE] = event;
        self.len += 1;
    }

    /// Moves up to [`MAX_EVENTS`] events into a `READ` reply.
    fn read(&mut self, reply: &mut [u8; READ_REPLY]) -> usize {
        reply[..4].copy_from_slice(&self.lost.to_le_bytes());
        self.lost = 0;
        let mut len = 4;
        for _ in 0..MAX_EVENTS {
            if self.len == 0 {
                break;
            }
            reply[len..len + EVENT_SIZE].copy_from_slice(&self.queue[self.head].encode());
            len += EVENT_SIZE;
            self.head = (self.head + 1) % QUEUE;
            self.len -= 1;
        }
        len
    }
}

struct Driver {
    log: Handle,
    usb: Handle,
    notification: Handle,
    memory: Handle,
    shared: *mut u8,
    pointers: [Option<Pointer>; MAX_POINTERS],
    next_device: u8,
    subscribers: [Option<Subscriber>; MAX_SUBSCRIPTIONS],
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
        pointers: [const { None }; MAX_POINTERS],
        next_device: 1,
        subscribers: [const { None }; MAX_SUBSCRIPTIONS],
    };
    driver.find_pointers();
    driver.serve(server)
}

impl Driver {
    // ---- Pointers --------------------------------------------------------

    /// Claims pointers while xhci has unclaimed ones and slots are free
    /// (when it has none, it will signal).
    fn find_pointers(&mut self) {
        while let Some(slot) = self.pointers.iter().position(Option::is_none) {
            let Some((claim, session)) = self.claim() else {
                return;
            };
            let pointer = self.start_pointer(claim, session);
            self.pointers[slot] = Some(pointer);
            // Reports may already be waiting; this also starts polling.
            self.fetch(slot);
        }
    }

    /// One `CLAIM` of a pointer.
    fn claim(&self) -> Option<(Claim, Handle)> {
        let memory = oceans_rt::duplicate(
            self.memory,
            rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER,
        )
        .ok()?;
        let notification =
            oceans_rt::duplicate(self.notification, rights::SIGNAL | rights::TRANSFER).ok()?;
        let (class, subclass, protocol) = service::POINTER;
        let mut data = [0u8; 12];
        data[..3].copy_from_slice(&[class, subclass, protocol]);
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
        match (got.label, got.handles_len) {
            (service::OK, 1) => match Claim::decode(&reply[..got.data_len]) {
                Some(claim) => Some((claim, handles[0])),
                None => {
                    let _ = oceans_rt::close(handles[0]);
                    None
                }
            },
            (service::NOT_FOUND, _) => None,
            (label, _) => {
                say(
                    self.log,
                    format_args!("claiming a pointer failed ({label})"),
                );
                None
            }
        }
    }

    /// Readies a claimed pointer: the boot protocol for a boot mouse,
    /// otherwise its report descriptor; reports only on change.
    fn start_pointer(&mut self, claim: Claim, session: Handle) -> Pointer {
        let device = self.next_device;
        self.next_device = self.next_device.checked_add(1).unwrap_or(1);
        let path = claim.path();
        let mut decoder = Decoder {
            boot: false,
            layout: None,
        };
        if claim.is_boot_mouse() {
            match control(session, Setup::hid_set_boot_protocol(claim.interface)) {
                Ok(_) => decoder.boot = true,
                Err(label) => say(
                    self.log,
                    format_args!("port {path}: the boot protocol was refused ({label})"),
                ),
            }
        }
        if !decoder.boot {
            match self.report_layout(session, &claim) {
                Ok(layout) => decoder.layout = Some(layout),
                Err(problem) => say(self.log, format_args!("port {path}: {problem}")),
            }
        }
        // Optional (HID 1.11 §7.2.4): some devices stall it.
        let _ = control(session, Setup::hid_set_idle(claim.interface));
        match &decoder.layout {
            _ if decoder.boot => say(
                self.log,
                format_args!("port {path}: mouse, boot protocol, pointer {device}"),
            ),
            Some(layout) => {
                let mut text = Buffer::<80>::new();
                let _ = write!(
                    text,
                    "{} buttons{}{}",
                    layout.button_count(),
                    if layout.wheel.is_some() {
                        ", wheel"
                    } else {
                        ""
                    },
                    if layout.pan.is_some() { ", pan" } else { "" },
                );
                match layout.axes() {
                    Axes::Relative => say(
                        self.log,
                        format_args!("port {path}: mouse, {}, pointer {device}", text.as_str()),
                    ),
                    Axes::Absolute { x_max, y_max } => say(
                        self.log,
                        format_args!(
                            "port {path}: tablet, {}, 0..{x_max} x 0..{y_max}, pointer {device}",
                            text.as_str()
                        ),
                    ),
                }
            }
            None => say(
                self.log,
                format_args!("port {path}: not usable; its reports are ignored"),
            ),
        }
        Pointer {
            session,
            claim,
            device,
            decoder,
            tracker: Tracker::new(),
        }
    }

    /// Reads the pointer's report descriptor through the shared buffer.
    fn report_layout(&self, session: Handle, claim: &Claim) -> Result<Layout, &'static str> {
        let length = usize::from(claim.report_length).min(SHARED);
        if length == 0 {
            return Err("no report descriptor");
        }
        let got = control(
            session,
            Setup::hid_get_report_descriptor(claim.interface, length as u16),
        )
        .map_err(|_| "the report descriptor could not be read")?;
        // SAFETY: `shared` maps SHARED bytes; xhci wrote `got` ≤ `length`
        // ≤ SHARED of them during the call, and touches them only then.
        let descriptor = unsafe { core::slice::from_raw_parts(self.shared, got.min(length)) };
        Layout::parse(descriptor).map_err(|error| match error {
            pointer::Error::Malformed => "malformed report descriptor",
            pointer::Error::NotAPointer => "the report descriptor has no pointer",
        })
    }

    /// Fetches and decodes everything xhci queued for pointer `slot`;
    /// forgets the pointer if it was unplugged.
    fn fetch(&mut self, slot: usize) {
        loop {
            let Some(pointer) = self.pointers[slot].as_mut() else {
                return;
            };
            let mut reply = [0u8; service::MAX_REPORTS_REPLY];
            let got = oceans_rt::ipc_call_msg(
                pointer.session,
                service::REPORTS,
                &[],
                &[],
                &mut reply,
                &mut [],
            );
            let len = match got {
                Ok(got) if got.label == service::OK => got.data_len,
                Ok(got) if got.label != service::GONE => {
                    say(
                        self.log,
                        format_args!(
                            "port {}: reports failed ({})",
                            pointer.claim.path(),
                            got.label
                        ),
                    );
                    return;
                }
                // GONE, or xhci itself went away: the pointer is gone.
                _ => {
                    say(
                        self.log,
                        format_args!(
                            "port {}: pointer {} removed",
                            pointer.claim.path(),
                            pointer.device
                        ),
                    );
                    let _ = oceans_rt::close(pointer.session);
                    self.pointers[slot] = None;
                    return;
                }
            };
            let reports = Reports::new(&reply[..len]);
            if reports.lost() > 0 {
                say(
                    self.log,
                    format_args!(
                        "port {}: {} reports lost",
                        pointer.claim.path(),
                        reports.lost()
                    ),
                );
            }
            let mut any = false;
            for (time_ms, report) in reports {
                any = true;
                // Reports that do not decode (another report ID, too
                // short) carry nothing for us.
                let Some(sample) = pointer.decoder.decode(report) else {
                    continue;
                };
                let axes = pointer.decoder.axes();
                let device = pointer.device;
                let subscribers = &mut self.subscribers;
                pointer.tracker.update(&sample, axes, |kind| {
                    let event = Event {
                        time_ms,
                        device,
                        kind,
                    };
                    for subscriber in subscribers.iter_mut().flatten() {
                        subscriber.push(event);
                    }
                });
            }
            self.notify();
            if !any {
                return;
            }
        }
    }

    /// Signals every subscriber with events waiting.
    fn notify(&self) {
        for subscriber in self.subscribers.iter().flatten() {
            if subscriber.len > 0 {
                let _ = oceans_rt::notification_signal(subscriber.notification, subscriber.bits);
            }
        }
    }

    /// xhci signalled: reports arrived, or a pointer came or went.
    fn usb_event(&mut self) {
        for slot in 0..MAX_POINTERS {
            self.fetch(slot);
        }
        self.find_pointers();
    }

    // ---- Clients ---------------------------------------------------------

    fn serve(&mut self, server: Handle) -> i64 {
        let mut next_badge = 1;
        let mut data = [0u8; 16];
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
                self.usb_event();
                continue;
            }
            let find = |subscribers: &[Option<Subscriber>], badge| {
                subscribers
                    .iter()
                    .position(|s| s.as_ref().is_some_and(|s| s.badge == badge))
            };
            if got.closed {
                if let Some(index) = find(&self.subscribers, got.badge)
                    && let Some(subscriber) = self.subscribers[index].take()
                {
                    let _ = oceans_rt::close(subscriber.notification);
                }
                continue;
            }
            let received = &handles[..got.handles_len];
            let mut kept = false;
            let mut reply = [0u8; READ_REPLY];
            let mut reply_len = 0;
            let mut reply_handle = None;
            let subscriber = find(&self.subscribers, got.badge).filter(|_| got.badge != 0);
            let status = match (got.label, subscriber) {
                (op::SUBSCRIBE, None) if got.badge == 0 && received.len() == 1 => {
                    let bits = data[..got.data_len]
                        .try_into()
                        .map(u64::from_le_bytes)
                        .unwrap_or(0);
                    match self.subscribe(server, received[0], bits, next_badge) {
                        Ok(handle) => {
                            next_badge += 1;
                            reply_handle = Some(handle);
                            kept = true;
                            Status::Ok
                        }
                        Err(status) => status,
                    }
                }
                (op::READ, Some(index)) => match self.subscribers[index].as_mut() {
                    Some(subscriber) => {
                        reply_len = subscriber.read(&mut reply);
                        Status::Ok
                    }
                    None => Status::BadRequest,
                },
                _ => Status::BadRequest,
            };
            if !kept {
                for &handle in received {
                    let _ = oceans_rt::close(handle);
                }
            }
            let handles_out: &[Handle] = match &reply_handle {
                Some(handle) => core::slice::from_ref(handle),
                None => &[],
            };
            if oceans_rt::ipc_reply_msg(status as u64, &reply[..reply_len], handles_out).is_err()
                && let Some(handle) = reply_handle
            {
                let _ = oceans_rt::close(handle);
            }
        }
    }

    /// `SUBSCRIBE`: keeps the client's notification and mints its
    /// subscription handle.
    fn subscribe(
        &mut self,
        server: Handle,
        notification: Handle,
        bits: u64,
        badge: u64,
    ) -> Result<Handle, Status> {
        if bits == 0 {
            return Err(Status::BadRequest);
        }
        let slot = self
            .subscribers
            .iter()
            .position(Option::is_none)
            .ok_or(Status::NoSpace)?;
        let handle = oceans_rt::endpoint_mint(server, badge).map_err(|_| Status::BadRequest)?;
        self.subscribers[slot] = Some(Subscriber {
            badge,
            notification,
            bits,
            queue: [Event {
                time_ms: 0,
                device: 0,
                kind: oceans_input::Kind::Wheel {
                    vertical: 0,
                    horizontal: 0,
                },
            }; QUEUE],
            head: 0,
            len: 0,
            lost: 0,
        });
        Ok(handle)
    }
}

/// A control request on a claim's session (no data stage, or into the
/// shared buffer at 0) → the bytes transferred, or the reply label.
fn control(session: Handle, setup: Setup) -> Result<usize, u64> {
    let mut data = [0u8; 12];
    data[..8].copy_from_slice(&setup.to_bytes());
    let mut reply = [0u8; 4];
    match oceans_rt::ipc_call_msg(session, service::CONTROL, &data, &[], &mut reply, &mut []) {
        Ok(got) if got.label == service::OK => Ok(u32::from_le_bytes(reply) as usize),
        Ok(got) => Err(got.label),
        Err(_) => Err(service::GONE),
    }
}
