//! The network device the stack sees (ADR-0129): Wi-Fi once joined, the
//! wired NIC until then; and joining the network set in `/system/wifi`.
//!
//! The network is set with two files the shell can write:
//!
//! ```text
//! write /system/wifi/ssid MyNetwork
//! write /system/wifi/password the passphrase
//! ```
//!
//! (no `password` for an open network), or with Control Center's Wi-Fi
//! panel (ADR-0130). They are read every 2 seconds, joined or not, to
//! follow a change. The driver writes `status` (what it does) and
//! `networks` (the last scan), and scans when `scan` appears
//! (`oceans_wifi::state` has the formats).
//!
//! Joining: a scan probing for the network, the MAC context, binding and
//! station, open-system authentication, association, then for WPA2 the
//! 4-way handshake over EAPOL. Data frames are then encrypted and checked
//! here (`oceans_wpa::Session`, CCMP in software); the chip carries them
//! as they are. The join blocks the device's service for its few seconds.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use oceans_fs_proto::{Kind, Node, flags};
use oceans_iwm::join::{self, Frame};
use oceans_net_proto::Status;
use oceans_net_proto::netdev::MAX_FRAME;
use oceans_nic::{Frames, Mac, Nic, ShowMac};
use oceans_rt::Handle;
use oceans_wifi::{self as wifi, Answer, Security, state};
use oceans_wpa::{Action, Session, Supplicant, eapol};

use crate::chip::{Chip, Network};
use crate::say;
use crate::wired::Wired;

/// How often the files are read (the panel waits on them).
const CHECK_MS: u64 = 2000;
/// After a failed join, the next attempt.
const RETRY_MS: u64 = 20_000;
/// Waiting for the access point: each answer, and the whole handshake.
const ANSWER_MS: u64 = 700;
const HANDSHAKE_MS: u64 = 6000;
/// Frames waiting for the stack, at most.
const QUEUE: usize = 256;
const MTU: u16 = 1500;
/// Room for an 802.11 frame with its CCMP header and MIC.
const AIR: usize = 2400;

/// The network the files set.
#[derive(Clone, PartialEq, Eq)]
struct Wanted {
    ssid: Vec<u8>,
    password: Option<String>,
}

/// A joined network.
struct Joined {
    network: Network,
    wanted: Wanted,
    supplicant: Option<Supplicant>,
    session: Session,
}

pub struct Driver<'a> {
    log: Handle,
    mac: Mac,
    chip: Option<Chip<'a>>,
    wired: Option<Wired>,
    storage: Option<Node>,
    /// Ethernet frames for the stack.
    queue: VecDeque<Vec<u8>>,
    joined: Option<Joined>,
    next_check: u64,
    /// The network last tried, its PMK (PBKDF2 is slow: computed once).
    pmk: Option<(Wanted, [u8; 32])>,
    /// What was last logged about the files, so it is said once.
    said: Option<Wanted>,
    /// The network whose join last failed, and when.
    failed: Option<(Wanted, u64)>,
    /// The `status` line last written.
    status: Option<Vec<u8>>,
}

impl<'a> Driver<'a> {
    pub fn new(
        log: Handle,
        mac: Mac,
        chip: Option<Chip<'a>>,
        wired: Option<Wired>,
        storage: Option<Node>,
    ) -> Self {
        Self {
            log,
            mac,
            chip,
            wired,
            storage,
            queue: VecDeque::new(),
            joined: None,
            next_check: 0,
            pmk: None,
            said: None,
            failed: None,
            status: None,
        }
    }

    fn say(&self, args: core::fmt::Arguments<'_>) {
        say(self.log, args);
    }

    fn enqueue(&mut self, frame: &[u8]) {
        if self.queue.len() < QUEUE && frame.len() <= MAX_FRAME {
            self.queue.push_back(frame.to_vec());
        }
    }

    // ---- The network's files ----------------------------------------------

    fn read_file(&self, name: &str) -> Option<Vec<u8>> {
        let storage = self.storage.as_ref()?;
        let (node, kind) = storage.open(name, 0).ok()?;
        let mut data = alloc::vec![0u8; 128];
        let len = if kind == Kind::File {
            node.read(0, &mut data).ok()
        } else {
            None
        };
        node.close();
        data.truncate(len?);
        // Written by hand: no line ending.
        while data.last().is_some_and(|b| matches!(b, b'\r' | b'\n')) {
            data.pop();
        }
        Some(data)
    }

    fn wanted(&self) -> Option<Wanted> {
        let ssid = self
            .read_file("ssid")
            .filter(|s| !s.is_empty() && s.len() <= 32)?;
        let password = self
            .read_file("password")
            .and_then(|p| String::from_utf8(p).ok())
            .filter(|p| !p.is_empty());
        Some(Wanted { ssid, password })
    }

    fn write_file(&self, name: &str, bytes: &[u8]) {
        let Some(storage) = self.storage.as_ref() else {
            return;
        };
        if let Ok((node, kind)) = storage.open(name, flags::CREATE_FILE | flags::WRITE) {
            if kind == Kind::File {
                let _ = node.truncate(0);
                let _ = node.write_all(0, bytes);
            }
            node.close();
        }
    }

    /// `status`, rewritten when it changes (ADR-0130).
    fn set_status(&mut self, status: state::Status<'_>) {
        let mut line = [0u8; state::MAX_LINE];
        let Some(len) = state::status_line(&status, &mut line) else {
            return;
        };
        if self.status.as_deref() == Some(&line[..len]) {
            return;
        }
        self.write_file(state::STATUS, &line[..len]);
        self.status = Some(line[..len].to_vec());
    }

    /// The status of being joined (or not), as it stands.
    fn settled_status(&mut self) {
        let status = match (&self.chip, &self.joined) {
            (None, _) => state::Status::Off,
            (Some(_), Some(joined)) => {
                let bss = joined.network.bss;
                match state::Ssid::new(bss.ssid()) {
                    Some(ssid) => state::Status::Joined {
                        ssid,
                        rssi: bss.rssi,
                        channel: bss.channel,
                    },
                    None => state::Status::Idle,
                }
            }
            (Some(_), None) => state::Status::Idle,
        };
        self.set_status(status);
    }

    /// `networks`: the last scan's networks by name (the strongest access
    /// point of each), strongest first; hidden ones left out.
    fn write_networks(&self) {
        let Some(chip) = self.chip.as_ref() else {
            return;
        };
        let mut best: Vec<wifi::Bss> = Vec::new();
        for bss in chip.networks.iter().map(|n| n.bss) {
            if bss.ssid().is_empty() {
                continue;
            }
            match best.iter_mut().find(|b| b.ssid() == bss.ssid()) {
                Some(b) if b.rssi < bss.rssi => *b = bss,
                Some(_) => {}
                None => best.push(bss),
            }
        }
        best.sort_by_key(|b| core::cmp::Reverse(b.rssi));
        let mut file = Vec::new();
        let mut line = [0u8; state::MAX_LINE];
        for bss in best.iter().take(32) {
            if let Some(len) = state::network_line(bss, &mut line) {
                file.extend_from_slice(&line[..len]);
            }
        }
        self.write_file(state::NETWORKS, &file);
    }

    /// The first files: the boot scan's networks and the status.
    pub fn start(&mut self) {
        self.write_networks();
        self.settled_status();
    }

    // ---- Joining ----------------------------------------------------------

    /// Reads the files when due: a scan asked for; joins, leaves or
    /// rejoins as they say.
    fn follow_files(&mut self, now: u64) {
        if now < self.next_check || self.chip.is_none() {
            return;
        }
        self.next_check = now + CHECK_MS;
        if self.read_file(state::SCAN).is_some() {
            if let Some(storage) = self.storage.as_ref() {
                let _ = storage.remove(state::SCAN);
            }
            self.set_status(state::Status::Scanning);
            let scanned = self.chip.as_mut().map(|chip| chip.scan(None));
            match scanned {
                Some(Ok(())) => self.write_networks(),
                Some(Err(problem)) => self.say(format_args!("scan failed: {problem}")),
                None => {}
            }
            self.settled_status();
        }
        let wanted = self.wanted();
        let joined_to = self.joined.as_ref().map(|j| j.wanted.clone());
        if joined_to.is_some() && joined_to == wanted {
            return;
        }
        if joined_to.is_some() {
            self.say(format_args!("the network setting changed: leaving"));
            self.leave();
        }
        let Some(wanted) = wanted else {
            if self.said.take().is_some() {
                self.say(format_args!("no network set"));
            }
            self.failed = None;
            self.settled_status();
            return;
        };
        // The network that failed: again after a while, unless the setting
        // changed (a password being typed again, say).
        if let Some((failed, at)) = &self.failed
            && *failed == wanted
            && now < at + RETRY_MS
        {
            return;
        }
        if self.said.as_ref() != Some(&wanted) {
            self.say(format_args!(
                "network set: {:?}{}",
                String::from_utf8_lossy(&wanted.ssid),
                if wanted.password.is_some() {
                    " with a password"
                } else {
                    ", open"
                }
            ));
            self.said = Some(wanted.clone());
        }
        let ssid = state::Ssid::new(&wanted.ssid);
        if let Some(ssid) = ssid {
            self.set_status(state::Status::Joining(ssid));
        }
        match self.join(&wanted) {
            Ok(()) => {
                self.failed = None;
                self.settled_status();
            }
            Err(why) => {
                self.say(format_args!(
                    "joining failed: {why}; trying again in {} s",
                    RETRY_MS / 1000
                ));
                self.leave();
                if let Some(ssid) = ssid {
                    self.set_status(state::Status::Failed { ssid, reason: &why });
                }
                self.failed = Some((wanted, oceans_rt::clock_ms()));
            }
        }
        self.next_check = oceans_rt::clock_ms() + CHECK_MS;
    }

    fn leave(&mut self) {
        if let Some(joined) = self.joined.take() {
            self.say(format_args!(
                "left {:?}; frames go by wire again",
                String::from_utf8_lossy(joined.network.bss.ssid())
            ));
        }
        if let Some(chip) = self.chip.as_mut() {
            chip.leave();
            chip.inbox.clear();
        }
    }

    fn join(&mut self, wanted: &Wanted) -> Result<(), String> {
        let chip = self.chip.as_mut().ok_or("no Wi-Fi")?;
        chip.scan(Some(&wanted.ssid))
            .map_err(|e| alloc::format!("scan: {e}"))?;
        let network = chip
            .find(&wanted.ssid)
            .ok_or_else(|| String::from("the network was not found"))?;
        let bss = network.bss;
        let pmk = match (bss.security, &wanted.password) {
            (Security::Open, _) => None,
            (Security::Wpa2Personal, Some(password)) => Some(self.pmk_for(wanted, password)?),
            (Security::Wpa2Personal, None) => return Err("the network needs a password".into()),
            (Security::Unsupported, _) => {
                return Err(
                    "the network's security is not supported (WPA2-Personal or open only)".into(),
                );
            }
        };
        self.say(format_args!(
            "joining {:?} on channel {} ({} dBm, {})",
            String::from_utf8_lossy(bss.ssid()),
            bss.channel,
            bss.rssi,
            ShowMac(bss.bssid)
        ));
        let chip = self.chip.as_mut().ok_or("no Wi-Fi")?;
        chip.inbox.clear();
        chip.prepare_join(&network)?;

        // Authentication (open system), then association.
        let mut frame = [0u8; 512];
        let len = wifi::auth_request(&mut frame, &self.mac, &bss.bssid).ok_or("auth frame")?;
        let answer = self.exchange(&network, &frame[..len], false)?;
        if answer != Answer::Authenticated {
            return Err(alloc::format!("authentication: {}", describe(answer)));
        }
        self.say(format_args!("authenticated"));
        let rsn = pmk.map(|_| &oceans_wpa::RSN_ELEMENT[..]);
        let len = wifi::assoc_request(&mut frame, &self.mac, &bss, rsn).ok_or("assoc frame")?;
        let aid = match self.exchange(&network, &frame[..len], true)? {
            Answer::Associated(aid) => aid,
            other => return Err(alloc::format!("association: {}", describe(other))),
        };
        self.say(format_args!("associated (AID {aid})"));
        let chip = self.chip.as_mut().ok_or("no Wi-Fi")?;
        chip.associated(&network, aid)?;

        let supplicant = match pmk {
            Some(pmk) => {
                let mut snonce = [0u8; 32];
                oceans_rt::random(&mut snonce).map_err(|_| "no randomness for the handshake")?;
                Some(Supplicant::new(pmk, bss.bssid, self.mac, snonce))
            }
            None => None,
        };
        let wpa = supplicant.is_some();
        self.joined = Some(Joined {
            network,
            wanted: wanted.clone(),
            supplicant,
            session: Session::default(),
        });
        if wpa {
            // The 4-way handshake: EAPOL frames through the data path.
            let deadline = oceans_rt::clock_ms() + HANDSHAKE_MS;
            loop {
                self.poll_chip();
                self.drain_inbox();
                let protected = self.joined.as_ref().is_some_and(|j| j.session.protected());
                if protected {
                    break;
                }
                if self.joined.is_none() {
                    return Err("dropped by the access point during the handshake".into());
                }
                if oceans_rt::clock_ms() > deadline {
                    return Err("no WPA2 handshake (a wrong password?)".into());
                }
                oceans_rt::sleep_ms(2);
            }
        }
        // What the wire held for the stack is stale now.
        self.queue.clear();
        self.say(format_args!(
            "joined {:?}{}: the network goes by Wi-Fi",
            String::from_utf8_lossy(bss.ssid()),
            if wpa { " (WPA2)" } else { " (open)" }
        ));
        Ok(())
    }

    fn pmk_for(&mut self, wanted: &Wanted, password: &str) -> Result<[u8; 32], String> {
        if let Some((cached, pmk)) = &self.pmk
            && cached == wanted
        {
            return Ok(*pmk);
        }
        let pmk = oceans_wpa::psk(password, &wanted.ssid)
            .ok_or("the password must be 8 to 63 printable characters")?;
        self.pmk = Some((wanted.clone(), pmk));
        Ok(pmk)
    }

    /// Sends a management frame and waits for the access point's answer,
    /// three tries.
    fn exchange(&mut self, network: &Network, frame: &[u8], assoc: bool) -> Result<Answer, String> {
        let bss = network.bss;
        let meta = Frame {
            len: frame.len() as u16,
            header_len: wifi::MGMT_HEADER,
            data: false,
            unicast: true,
            assoc_request: assoc,
            rate: join::management_rate(bss.basic_rate, bss.channel <= 14),
        };
        for _ in 0..3 {
            let chip = self.chip.as_mut().ok_or("no Wi-Fi")?;
            chip.last_tx = None;
            chip.send_frame(frame, meta)?;
            let deadline = oceans_rt::clock_ms() + ANSWER_MS;
            while oceans_rt::clock_ms() < deadline {
                let chip = self.chip.as_mut().ok_or("no Wi-Fi")?;
                chip.service_rx()?;
                while let Some(got) = chip.inbox.pop_front() {
                    if let Some(answer) = wifi::answer(&got, &self.mac, &bss.bssid) {
                        return Ok(answer);
                    }
                }
                oceans_rt::sleep_ms(2);
            }
            let chip = self.chip.as_ref().ok_or("no Wi-Fi")?;
            self.say(format_args!(
                "no answer from the access point (the frame {})",
                match chip.last_tx {
                    Some((true, retries)) => alloc::format!("went, {retries} retries"),
                    Some((false, retries)) => alloc::format!("failed after {retries} retries"),
                    None => String::from("was not reported sent"),
                }
            ));
        }
        Err("the access point did not answer".into())
    }

    // ---- The data path ----------------------------------------------------

    fn poll_chip(&mut self) {
        let Some(chip) = self.chip.as_mut() else {
            return;
        };
        if let Err(problem) = chip.service_rx() {
            chip.report();
            say(self.log, format_args!("Wi-Fi stopped: {problem}"));
            self.chip = None;
            self.joined = None;
            self.settled_status();
        }
    }

    /// Frames the chip received: data for the stack, EAPOL for the
    /// handshakes, the access point dropping us.
    fn drain_inbox(&mut self) {
        loop {
            let Some(frame) = self.chip.as_mut().and_then(|c| c.inbox.pop_front()) else {
                return;
            };
            if self.joined.is_none() {
                continue;
            }
            self.received(&frame);
        }
    }

    fn received(&mut self, frame: &[u8]) {
        let Some(joined) = self.joined.as_mut() else {
            return;
        };
        let bssid = joined.network.bss.bssid;
        let Some((header_len, protected)) = wifi::from_access_point(frame, &bssid) else {
            if let Some(Answer::Dropped(reason)) = wifi::answer(frame, &self.mac, &bssid) {
                self.say(format_args!(
                    "the access point dropped us (reason {reason})"
                ));
                self.leave();
                self.settled_status();
                self.next_check = oceans_rt::clock_ms() + CHECK_MS;
            }
            return;
        };
        let mut plain = [0u8; AIR];
        let frame = if protected {
            match joined.session.unprotect(frame, header_len, &mut plain) {
                Some(len) => &plain[..len],
                None => return,
            }
        } else {
            frame
        };
        let mut ethernet = [0u8; AIR];
        let Some(len) = wifi::to_ethernet(frame, &mut ethernet) else {
            return;
        };
        let ethernet = &ethernet[..len];
        let ethertype = u16::from_be_bytes([ethernet[12], ethernet[13]]);
        if ethertype == eapol::ETHERTYPE {
            self.handshake(&ethernet[14..]);
            return;
        }
        // Once keys exist, only protected frames are data.
        let wpa = joined.supplicant.is_some();
        if (wpa && !protected) || ethernet[6..12] == self.mac {
            return;
        }
        self.enqueue(ethernet);
    }

    /// An EAPOL-Key frame: the supplicant's answer sent, keys installed.
    fn handshake(&mut self, eapol_frame: &[u8]) {
        let Some(joined) = self.joined.as_mut() else {
            return;
        };
        let Some(supplicant) = joined.supplicant.as_mut() else {
            return;
        };
        let mut reply = [0u8; 512];
        let (len, keys) = match supplicant.receive(eapol_frame, &mut reply) {
            Action::Ignore => return,
            Action::Send(len) => (len, None),
            Action::SendAndInstall(len, keys) => (len, Some(keys)),
        };
        let bssid = joined.network.bss.bssid;
        let mut ethernet = [0u8; 14 + 512];
        ethernet[..6].copy_from_slice(&bssid);
        ethernet[6..12].copy_from_slice(&self.mac);
        ethernet[12..14].copy_from_slice(&eapol::ETHERTYPE.to_be_bytes());
        ethernet[14..14 + len].copy_from_slice(&reply[..len]);
        // The reply goes under the keys in use (none for the 4-way's).
        let _ = self.send_ethernet(&ethernet[..14 + len]);
        if let Some(keys) = keys {
            let first = !joined_protected(&self.joined);
            if let Some(joined) = self.joined.as_mut() {
                joined.session.install(keys);
            }
            self.say(format_args!(
                "{}",
                if first {
                    "WPA2 keys installed"
                } else {
                    "group key renewed"
                }
            ));
        }
    }

    /// An Ethernet frame from the stack (or the handshake) onto the air.
    fn send_ethernet(&mut self, ethernet: &[u8]) -> Status {
        let Some(joined) = self.joined.as_mut() else {
            return Status::NoDevice;
        };
        let bss = joined.network.bss;
        let mut air = [0u8; AIR];
        let Some(len) = wifi::from_ethernet(ethernet, &bss.bssid, 0, false, &mut air) else {
            return Status::BadRequest;
        };
        let mut sealed = [0u8; AIR];
        let frame = if joined.session.protected() {
            match joined
                .session
                .protect(&air[..len], wifi::DATA_HEADER, &mut sealed)
            {
                Some(len) => &sealed[..len],
                None => return Status::BadRequest,
            }
        } else {
            &air[..len]
        };
        let meta = Frame {
            len: frame.len() as u16,
            header_len: wifi::DATA_HEADER,
            data: true,
            // To the access point: always unicast on the air.
            unicast: true,
            assoc_request: false,
            rate: join::management_rate(bss.basic_rate, bss.channel <= 14),
        };
        match self.chip.as_mut().map(|c| c.send_frame(frame, meta)) {
            Some(Ok(())) => Status::Ok,
            _ => Status::NoBuffers,
        }
    }
}

fn joined_protected(joined: &Option<Joined>) -> bool {
    joined.as_ref().is_some_and(|j| j.session.protected())
}

fn describe(answer: Answer) -> String {
    match answer {
        Answer::Authenticated => "authenticated".into(),
        Answer::Associated(aid) => alloc::format!("associated ({aid})"),
        Answer::Refused(status) => alloc::format!("refused (status {status})"),
        Answer::Dropped(reason) => alloc::format!("dropped (reason {reason})"),
    }
}

impl Nic for Driver<'_> {
    fn info(&self) -> (Mac, u16) {
        (self.mac, MTU)
    }

    fn service(&mut self) -> Result<(), &'static str> {
        let wifi = self.joined.is_some();
        if let Some(wired) = &self.wired {
            let mut frames = Vec::new();
            wired.receive(|frame| {
                // While joined, the wire is not the stack's network.
                if !wifi {
                    frames.push(frame.to_vec());
                }
            });
            for frame in frames {
                self.enqueue(&frame);
            }
        }
        self.poll_chip();
        self.drain_inbox();
        self.follow_files(oceans_rt::clock_ms());
        Ok(())
    }

    fn frames_waiting(&self) -> bool {
        !self.queue.is_empty()
    }

    fn transmit(&mut self, frame: &[u8]) -> Status {
        if self.joined_ready() {
            return self.send_ethernet(frame);
        }
        match &self.wired {
            Some(wired) => wired.send(frame),
            None => Status::NoDevice,
        }
    }

    fn receive(&mut self, frames: &mut Frames) {
        while let Some(frame) = self.queue.front() {
            if !frames.fits(frame.len()) {
                return;
            }
            let frame = self.queue.pop_front().expect("a frame");
            frames.push(frame.len(), |out| out.copy_from_slice(&frame));
        }
    }
}

impl Driver<'_> {
    /// Joined, and (for WPA2) the keys in place: the stack's frames go by
    /// Wi-Fi.
    fn joined_ready(&self) -> bool {
        self.joined
            .as_ref()
            .is_some_and(|j| j.supplicant.is_none() || j.session.protected())
    }
}
