//! The chip: its registers, DMA rings and firmware, the commands it takes,
//! and what it receives (ADR-0129). Everything it does is logged: it runs
//! only on real hardware, and the log is how it is debugged.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use oceans_iwm::fw::{self, Firmware, Image};
use oceans_iwm::join::{self, Association, Frame, MacContext};
use oceans_iwm::mvm::{self, Station, code, ids};
use oceans_iwm::{alive, cmd, nvm, paging, reg, rx};
use oceans_nic::{Registers, ShowMac};
use oceans_rt::Handle;
use oceans_virtio::Dma;
use oceans_wifi::{Bss, Security};

use crate::say;

const RX_BUFFERS: usize = 256;
const RX_BUFFER: usize = 4096;
const CHUNK: usize = 0x2_0000;
const TIMEOUT_MS: u64 = 1000;
const CALIBRATION_MS: u64 = 2000;
/// A passive scan of every channel: about 110 ms each.
const SCAN_MS: u64 = 15_000;
/// TX queues besides the command queue: auxiliary (1), data (5..8).
const TX_QUEUES: [u8; 5] = [1, 5, 6, 7, 8];
/// Room for one frame on the data queue: the command, the TX command, the
/// 802.11 header and the frame's body.
const TX_SLOT: usize = 2048;
/// Frames received and not yet looked at, at most.
const INBOX: usize = 256;
/// REMOVE_STA: `[sta_id, reserved 3]`.
const REMOVE_STA: u8 = 0x19;

pub type Result<T, E = String> = core::result::Result<T, E>;

/// The chip and its DMA memory.
pub struct Chip<'a> {
    log: Handle,
    device: Handle,
    regs: Registers,
    firmware: &'a Firmware<'a>,
    hw_rev: u32,
    /// Firmware chunks on their way in.
    chunk: Dma,
    /// The command queue (0): TFDs, command buffers, byte counts.
    tfds: Dma,
    commands: Dma,
    byte_counts: Dma,
    keep_warm: Dma,
    cmd_next: usize,
    /// Commands too long for a slot (the scan request).
    big: Dma,
    /// The other TX queues' TFD rings (`TX_QUEUES`).
    rings: Vec<Dma>,
    /// RX: descriptors, buffers, the status the chip writes.
    rbds: Dma,
    rx_buffers: Dma,
    rx_status: Dma,
    rx_next: usize,
    /// What the RX ring delivered: a response, notifications seen.
    response: Option<(u8, bool, Vec<u8>)>,
    alive: Option<alive::Alive>,
    init_complete: bool,
    phy_db: Vec<Vec<u8>>,
    /// Paging blocks (kept: the chip reads them while it runs).
    paging: Vec<Dma>,
    mac: Option<[u8; 6]>,
    /// From the NVM: antennas, channels, regulatory.
    tx_ant: u8,
    rx_ant: u8,
    band_5ghz: bool,
    lar: bool,
    channels: Vec<u8>,
    /// The scan: the last RX_PHY (for the frame after it), what was heard.
    phy: Option<mvm::Phy>,
    pub networks: Vec<Network>,
    scan_done: Option<u8>,
    frames: usize,
    /// The address used on the air: the wired NIC's when there is one (the
    /// stack has one address whichever way frames go), else the NVM's.
    pub address: Option<[u8; 6]>,
    /// The data queue (5): frame slots, and the ring's next and oldest
    /// unfinished entries.
    tx_slots: Dma,
    tx_cur: usize,
    tx_tail: usize,
    /// How the last frame sent went (sent, retries), and counts.
    pub last_tx: Option<(bool, u8)>,
    pub tx_failed: u64,
    /// Frames received other than beacons and probe responses.
    pub inbox: VecDeque<Vec<u8>>,
    /// The session protection's start and end, as notified.
    time_event: Option<u32>,
    /// What the join set up, to take down before the next.
    joined: bool,
}

/// A network heard, and when its frame arrived (the chip's clock).
#[derive(Clone, Copy, Debug)]
pub struct Network {
    pub bss: Bss,
    pub received: u32,
}

impl<'a> Chip<'a> {
    pub fn new(log: Handle, device: Handle, firmware: &'a Firmware<'a>) -> Result<Self> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let regs = Registers::map(device, 0, reg::SPAN)?;
        let dma = |size: usize| Dma::new(device, size).map_err(String::from);
        let chip = Self {
            log,
            device,
            regs,
            firmware,
            hw_rev: 0,
            chunk: dma(CHUNK)?,
            tfds: dma(cmd::RING * cmd::TFD_SIZE)?,
            commands: dma(cmd::RING * cmd::SLOT)?,
            byte_counts: dma(cmd::QUEUES * cmd::BC_TABLE)?,
            keep_warm: dma(4096)?,
            cmd_next: 0,
            big: dma(cmd::MAX)?,
            rings: TX_QUEUES
                .iter()
                .map(|_| dma(cmd::RING * cmd::TFD_SIZE))
                .collect::<Result<_>>()?,
            rbds: dma(4096)?,
            rx_buffers: dma(RX_BUFFERS * RX_BUFFER)?,
            rx_status: dma(4096)?,
            rx_next: 0,
            response: None,
            alive: None,
            init_complete: false,
            phy_db: Vec::new(),
            paging: Vec::new(),
            mac: None,
            tx_ant: ((firmware.phy_sku >> 16) & 0xf) as u8,
            rx_ant: ((firmware.phy_sku >> 20) & 0xf) as u8,
            band_5ghz: false,
            lar: false,
            channels: Vec::new(),
            phy: None,
            networks: Vec::new(),
            scan_done: None,
            frames: 0,
            address: None,
            tx_slots: dma(cmd::RING * TX_SLOT)?,
            tx_cur: 0,
            tx_tail: 0,
            last_tx: None,
            tx_failed: 0,
            inbox: VecDeque::new(),
            time_event: None,
            joined: false,
        };
        for (name, dma) in [
            ("chunk", &chip.chunk),
            ("tfds", &chip.tfds),
            ("rx buffers", &chip.rx_buffers),
            ("big commands", &chip.big),
            ("byte counts", &chip.byte_counts),
            ("frames", &chip.tx_slots),
        ] {
            if dma.device >> 36 != 0 {
                return Err(alloc::format!(
                    "{name} DMA above 64 GiB: the chip cannot reach it"
                ));
            }
        }
        Ok(chip)
    }

    fn step(&self, args: core::fmt::Arguments<'_>) {
        say(self.log, args);
    }

    // ---- Register access ----------------------------------------------------

    fn read(&self, at: usize) -> u32 {
        self.regs.read(at)
    }

    fn write(&self, at: usize, value: u32) {
        self.regs.write(at, value);
    }

    fn set(&self, at: usize, bits: u32) {
        self.regs.update(at, bits, 0);
    }

    fn clear(&self, at: usize, bits: u32) {
        self.regs.update(at, 0, bits);
    }

    fn poll(&self, at: usize, mask: u32, want: u32, ms: u64) -> bool {
        oceans_nic::wait_for(ms, || self.read(at) & mask == want)
    }

    fn lock(&self) -> Result<()> {
        self.set(reg::GP_CNTRL, reg::gp::MAC_ACCESS_REQ);
        let want = reg::gp::MAC_CLOCK_READY;
        if self.poll(
            reg::GP_CNTRL,
            reg::gp::MAC_CLOCK_READY | reg::gp::GOING_TO_SLEEP,
            want,
            150,
        ) {
            Ok(())
        } else {
            Err(alloc::format!(
                "the chip did not grant access (GP_CNTRL {:#x})",
                self.read(reg::GP_CNTRL)
            ))
        }
    }

    fn unlock(&self) {
        self.clear(reg::GP_CNTRL, reg::gp::MAC_ACCESS_REQ);
    }

    fn prph_read(&self, at: u32) -> u32 {
        self.write(reg::HBUS_TARG_PRPH_RADDR, (at & 0xf_ffff) | (3 << 24));
        self.read(reg::HBUS_TARG_PRPH_RDAT)
    }

    fn prph_write(&self, at: u32, value: u32) {
        self.write(reg::HBUS_TARG_PRPH_WADDR, (at & 0xf_ffff) | (3 << 24));
        self.write(reg::HBUS_TARG_PRPH_WDAT, value);
    }

    fn prph_set(&self, at: u32, bits: u32) {
        self.prph_write(at, self.prph_read(at) | bits);
    }

    fn mem_write(&self, at: u32, words: &[u32]) {
        self.write(reg::HBUS_TARG_MEM_WADDR, at);
        for &word in words {
            self.write(reg::HBUS_TARG_MEM_WDAT, word);
        }
    }

    // ---- Waking the chip ------------------------------------------------------

    fn set_hw_ready(&self) -> bool {
        self.set(reg::HW_IF_CONFIG, reg::hw_if::NIC_READY);
        let ready = self.poll(
            reg::HW_IF_CONFIG,
            reg::hw_if::NIC_READY,
            reg::hw_if::NIC_READY,
            1,
        );
        if ready {
            self.set(reg::MBOX_SET, reg::MBOX_OS_ALIVE);
        }
        ready
    }

    fn prepare_card_hw(&self) -> Result<()> {
        if self.set_hw_ready() {
            return Ok(());
        }
        self.set(reg::DBG_LINK_PWR_MGMT, 0x8000_0000);
        oceans_rt::sleep_ms(1);
        for _ in 0..10 {
            self.set(reg::HW_IF_CONFIG, reg::hw_if::PREPARE);
            for _ in 0..150 {
                if self.set_hw_ready() {
                    return Ok(());
                }
                oceans_rt::sleep_ms(1);
            }
            oceans_rt::sleep_ms(25);
        }
        Err("the chip did not become ready (NIC_READY)".into())
    }

    fn apm_init(&self) -> Result<()> {
        self.set(reg::GIO_CHICKEN_BITS, reg::GIO_CHICKEN_L1A_NO_L0S_RX);
        self.set(reg::DBG_HPET_MEM, 0xffff_0000);
        self.set(reg::HW_IF_CONFIG, reg::hw_if::HAP_WAKE_L1A);
        // L0s off unless the link has ASPM L1 on: left as the firmware has
        // it (the driver cannot read every field it would want).
        self.clear(reg::GIO, reg::GIO_L0S_ENABLED);
        self.set(reg::GP_CNTRL, reg::gp::INIT_DONE);
        if !self.poll(
            reg::GP_CNTRL,
            reg::gp::MAC_CLOCK_READY,
            reg::gp::MAC_CLOCK_READY,
            25,
        ) {
            return Err("the chip's clock did not start (MAC_CLOCK_READY)".into());
        }
        Ok(())
    }

    fn start_hw(&self) -> Result<()> {
        self.prepare_card_hw()?;
        self.write(reg::RESET, reg::RESET_SW);
        oceans_rt::sleep_ms(5);
        self.apm_init()?;
        self.write(reg::INT_MASK, reg::int::RF_KILL);
        if self.read(reg::GP_CNTRL) & reg::gp::HW_RF_KILL_SW == 0 {
            self.step(format_args!(
                "the radio is switched off (RF-kill): calibration needs it on"
            ));
        }
        Ok(())
    }

    fn attach(&mut self) -> Result<()> {
        let raw = self.read(reg::HW_REV);
        self.hw_rev = (raw & 0xfff0) | ((raw & 0x3) << 2);
        self.prepare_card_hw()?;
        self.set(reg::GP_CNTRL, reg::gp::INIT_DONE);
        if !self.poll(
            reg::GP_CNTRL,
            reg::gp::MAC_CLOCK_READY,
            reg::gp::MAC_CLOCK_READY,
            25,
        ) {
            return Err("the chip's clock did not start at attach".into());
        }
        self.lock()?;
        self.prph_set(reg::prph::WFPM_CTRL, reg::prph::ENABLE_WFPM);
        let step = (self.prph_read(reg::prph::AUX_MISC) >> 24) & 0xf;
        self.unlock();
        if step == 3 {
            self.hw_rev = (self.hw_rev & !0xc) | (2 << 2);
        }
        self.write(reg::INT, !0);
        self.step(format_args!(
            "chip awake: hardware revision {raw:#x} (step {})",
            (self.hw_rev >> 2) & 3
        ));
        Ok(())
    }

    // ---- RX and TX rings ----------------------------------------------------

    fn rx_init(&mut self) -> Result<()> {
        self.write(reg::RCSR_CONFIG, 0);
        self.poll(reg::RSSR_STATUS, 1 << 24, 1 << 24, 10);
        for i in 0..RX_BUFFERS {
            let address = self.rx_buffers.device + (i * RX_BUFFER) as u64;
            self.rbds.write(4 * i, (address >> 8) as u32);
        }
        self.rx_status.copy_in(0, &[0; 16]);
        self.rx_next = 0;
        fence(Ordering::SeqCst);
        self.lock()?;
        self.write(reg::RCSR_RBDCB_WPTR, 0);
        self.write(reg::RCSR_FLUSH_RB_REQ, 0);
        self.write(reg::RSCSR_RDPTR, 0);
        self.write(reg::RSCSR_WPTR, 0);
        self.write(reg::RSCSR_RBDCB_BASE, (self.rbds.device >> 8) as u32);
        self.write(reg::RSCSR_STTS_WPTR, (self.rx_status.device >> 4) as u32);
        self.write(reg::RCSR_CONFIG, reg::RCSR_ENABLE);
        self.regs.write8(reg::INT_COALESCING, 0x40);
        self.unlock();
        self.write(reg::RSCSR_WPTR, 8);
        Ok(())
    }

    fn tx_init(&mut self) -> Result<()> {
        let empty = alloc::vec![0u8; cmd::RING * cmd::TFD_SIZE];
        self.tfds.copy_in(0, &empty);
        for ring in &self.rings {
            ring.copy_in(0, &empty);
        }
        self.byte_counts
            .copy_in(0, &alloc::vec![0u8; cmd::QUEUES * cmd::BC_TABLE]);
        self.cmd_next = 0;
        self.tx_cur = 0;
        self.tx_tail = 0;
        fence(Ordering::SeqCst);
        self.lock()?;
        self.prph_write(reg::prph::SCD_TXFACT, 0);
        self.write(reg::FH_KW_MEM_ADDR, (self.keep_warm.device >> 4) as u32);
        self.write(reg::cbbc_queue(0), (self.tfds.device >> 8) as u32);
        for (&q, ring) in TX_QUEUES.iter().zip(&self.rings) {
            self.write(reg::cbbc_queue(usize::from(q)), (ring.device >> 8) as u32);
        }
        self.prph_set(
            reg::prph::SCD_GP_CTRL,
            reg::prph::SCD_GP_AUTO_ACTIVE | reg::prph::SCD_GP_ENABLE_31_QUEUES,
        );
        self.unlock();
        Ok(())
    }

    fn nic_init(&mut self) -> Result<()> {
        self.apm_init()?;
        let sku = self.firmware.phy_sku;
        let (phy_type, phy_step, phy_dash) = (sku & 3, (sku >> 2) & 3, (sku >> 4) & 3);
        use reg::hw_if::*;
        let value = (self.read(reg::HW_IF_CONFIG)
            & !(MAC_DASH | MAC_STEP | PHY_STEP | PHY_DASH | PHY_TYPE | RADIO_SI | MAC_SI))
            | (((self.hw_rev >> 2) & 3) << 2)
            | (self.hw_rev & 3)
            | (phy_type << 10)
            | (phy_step << 14)
            | (phy_dash << 12);
        self.write(reg::HW_IF_CONFIG, value);
        self.rx_init()?;
        self.tx_init()?;
        self.set(reg::MAC_SHADOW_REG_CTRL, 0x800f_ffff);
        Ok(())
    }

    // ---- Interrupt causes (polled) ------------------------------------------

    /// Reads and acknowledges the cause registers; fails on a crash.
    fn causes(&self) -> Result<(u32, u32)> {
        let int = self.read(reg::INT);
        let fh = self.read(reg::FH_INT_STATUS);
        if int == u32::MAX || int & 0xffff_fff0 == 0xa5a5_a5a0 {
            return Err("the chip stopped answering".into());
        }
        if int != 0 {
            self.write(reg::INT, int);
        }
        if fh != 0 {
            self.write(reg::FH_INT_STATUS, fh);
        }
        if int & reg::int::SW_ERR != 0 {
            return Err("the firmware crashed (SW_ERR)".into());
        }
        if int & reg::int::HW_ERR != 0 {
            return Err("the chip reported a DMA error (HW_ERR)".into());
        }
        Ok((int, fh))
    }

    // ---- Firmware -------------------------------------------------------------

    fn load_chunk(&self, devoff: u32, bytes: &[u8]) -> Result<()> {
        self.chunk.copy_in(0, bytes);
        fence(Ordering::SeqCst);
        let ch = reg::SRVC_CHANNEL;
        let address = self.chunk.device;
        self.lock()?;
        self.write(reg::tcsr_config(ch), 0);
        self.write(reg::SRVC_SRAM_ADDR, devoff);
        self.write(reg::tfdib_ctrl0(ch), address as u32);
        self.write(
            reg::tfdib_ctrl1(ch),
            (((address >> 32) & 0xf) as u32) << 28 | bytes.len() as u32,
        );
        self.write(reg::tcsr_buf_sts(ch), 0x0010_1003);
        self.write(reg::tcsr_config(ch), 0x8010_0000);
        self.unlock();
        let deadline = oceans_rt::clock_ms() + TIMEOUT_MS;
        loop {
            let (int, fh) = self.causes()?;
            if int & reg::int::FH_TX != 0 || fh & reg::int::FH_STATUS_TX != 0 {
                return Ok(());
            }
            if oceans_rt::clock_ms() > deadline {
                return Err(alloc::format!(
                    "a firmware chunk to {devoff:#x} did not finish loading (INT {int:#x})"
                ));
            }
            oceans_rt::sleep_ms(1);
        }
    }

    fn load_sections(&self, sections: &[fw::Section<'_>], shift: u32) -> Result<()> {
        let mut progress = 0u32;
        for section in sections {
            for (i, chunk) in section.data.chunks(CHUNK).enumerate() {
                self.load_chunk(section.devoff + (i * CHUNK) as u32, chunk)?;
            }
            progress = (progress << 1) | 1;
            self.lock()?;
            let status = self.read(reg::FH_UCODE_LOAD_STATUS);
            self.write(reg::FH_UCODE_LOAD_STATUS, status | (progress << shift));
            self.unlock();
        }
        self.lock()?;
        self.write(
            reg::FH_UCODE_LOAD_STATUS,
            if shift == 0 { 0xffff } else { 0xffff_ffff },
        );
        self.unlock();
        Ok(())
    }

    fn load_image(&mut self, name: &str, image: &Image<'_>) -> Result<()> {
        self.alive = None;
        self.write(reg::INT, !0);
        self.nic_init()?;
        self.write(reg::UCODE_DRV_GP1_CLR, reg::GP1_SW_BIT_RFKILL);
        self.write(reg::UCODE_DRV_GP1_CLR, reg::GP1_CMD_BLOCKED);
        self.write(reg::INT, !0);
        self.write(reg::INT_MASK, reg::int::FH_TX);
        self.write(reg::UCODE_DRV_GP1_CLR, reg::GP1_SW_BIT_RFKILL);
        self.lock()?;
        self.prph_write(
            reg::prph::RELEASE_CPU_RESET,
            reg::prph::RELEASE_CPU_RESET_BIT,
        );
        self.unlock();
        self.load_sections(&image.cpu1, 0)?;
        self.load_sections(&image.cpu2, 16)?;
        self.write(reg::INT_MASK, reg::int::INI_SET_MASK);
        self.step(format_args!("{name} image loaded; waiting for ALIVE"));
        let deadline = oceans_rt::clock_ms() + TIMEOUT_MS;
        while self.alive.is_none() {
            self.service_rx()?;
            if oceans_rt::clock_ms() > deadline {
                return Err(alloc::format!(
                    "{name}: no ALIVE (INT {:#x}, GP_CNTRL {:#x}, load status {:#x})",
                    self.read(reg::INT),
                    self.read(reg::GP_CNTRL),
                    self.read(reg::FH_UCODE_LOAD_STATUS)
                ));
            }
            oceans_rt::sleep_ms(1);
        }
        let alive = self.alive.expect("alive");
        if alive.status != alive::OK {
            return Err(alloc::format!(
                "{name}: ALIVE with status {:#x}, not ok",
                alive.status
            ));
        }
        self.step(format_args!(
            "{name} firmware ALIVE (scheduler at {:#x})",
            alive.scd_base
        ));
        self.post_alive(alive.scd_base)
    }

    fn post_alive(&mut self, scd_base: u32) -> Result<()> {
        self.lock()?;
        let _ = self.prph_read(reg::prph::SCD_SRAM_BASE);
        // The scheduler's context in SRAM, zeroed.
        self.mem_write(scd_base + 0x600, &[0; 130]);
        self.prph_write(
            reg::prph::SCD_DRAM_BASE,
            (self.byte_counts.device >> 10) as u32,
        );
        self.prph_write(reg::prph::SCD_CHAINEXT_EN, 0);
        // The command queue (0) on FIFO 7.
        let q = 0u32;
        self.write(reg::HBUS_TARG_WRPTR, q << 8);
        self.prph_write(reg::prph::scd_queue_status(q), 1 << 19);
        let aggr = self.prph_read(reg::prph::SCD_AGGR_SEL);
        self.prph_write(reg::prph::SCD_AGGR_SEL, aggr & !(1 << q));
        self.prph_write(reg::prph::scd_queue_rdptr(q), 0);
        self.mem_write(scd_base + 0x600 + 8 * q, &[0, 0x0040_0040]);
        self.prph_write(reg::prph::scd_queue_status(q), 0x017f_001f);
        self.prph_set(reg::prph::SCD_EN_CTRL, 1 << q);
        self.prph_write(reg::prph::SCD_TXFACT, 0xff);
        for ch in 0..8 {
            self.write(reg::tcsr_config(ch), 0x8000_0008);
        }
        self.set(reg::FH_TX_CHICKEN_BITS, 0x2);
        self.unlock();
        self.cmd_next = 0;
        Ok(())
    }

    // ---- Commands and the RX ring --------------------------------------------

    /// Drains the RX ring: answers kept, notifications acted on.
    pub fn service_rx(&mut self) -> Result<()> {
        let _ = self.causes()?;
        let closed = (self.rx_status.read::<u16>(0) & 0xfff) as usize % RX_BUFFERS;
        if closed == self.rx_next {
            return Ok(());
        }
        let mut buffer = [0u8; RX_BUFFER];
        while self.rx_next != closed {
            fence(Ordering::SeqCst);
            self.rx_buffers
                .copy_out(self.rx_next * RX_BUFFER, &mut buffer);
            for packet in rx::packets(&buffer) {
                self.handle(&packet);
            }
            self.rx_next = (self.rx_next + 1) % RX_BUFFERS;
        }
        let wptr = if closed == 0 {
            RX_BUFFERS - 1
        } else {
            closed - 1
        };
        self.write(reg::RSCSR_WPTR, (wptr & !7) as u32);
        Ok(())
    }

    fn handle(&mut self, packet: &rx::Packet<'_>) {
        // Received frames and scan notifications, whatever the queue bits.
        match packet.code {
            code::RX_PHY => {
                self.phy = mvm::rx_phy(packet.payload);
                return;
            }
            code::RX_MPDU => {
                if let (Some(phy), Some(frame)) = (self.phy.take(), mvm::rx_mpdu(packet.payload)) {
                    self.frames += 1;
                    let kind = frame[0];
                    if kind == oceans_wifi::kind::BEACON
                        || kind == oceans_wifi::kind::PROBE_RESPONSE
                    {
                        self.heard(frame, phy);
                    } else if self.inbox.len() < INBOX {
                        self.inbox.push_back(frame.to_vec());
                    }
                }
                return;
            }
            join::code::TX_CMD
                if !packet.notification() && packet.qid & 0x1f == ids::DATA_QUEUE =>
            {
                self.tx_done(packet.payload);
                return;
            }
            join::code::TIME_EVENT_NOTIFICATION => {
                if let Some((action, _)) = join::time_event_notification(packet.payload) {
                    self.time_event = Some(action);
                }
                return;
            }
            code::SCAN_COMPLETE_UMAC if packet.notification() => {
                self.scan_done = Some(mvm::scan_complete(packet.payload).unwrap_or(0xff));
                return;
            }
            code::SCAN_ITERATION_COMPLETE_UMAC => {
                self.step(format_args!(
                    "scan pass done: {} channels",
                    mvm::scan_iteration(packet.payload).unwrap_or(0)
                ));
                return;
            }
            _ => {}
        }
        if !packet.notification() {
            // Only the command queue's answers; TX queues report elsewhere.
            if packet.qid & 0x1f == 0 {
                self.response = Some((packet.idx, packet.failed(), packet.payload.to_vec()));
            }
            return;
        }
        match packet.code {
            cmd::ALIVE => self.alive = alive::parse(packet.payload),
            cmd::INIT_COMPLETE => self.init_complete = true,
            cmd::CALIB_RES_PHY_DB => self.phy_db.push(packet.payload.to_vec()),
            cmd::REPLY_ERROR => {
                let error = packet
                    .payload
                    .get(..4)
                    .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                let command = packet.payload.get(4).copied().unwrap_or(0);
                self.step(format_args!(
                    "the firmware refused command {command:#x} (error {error:#x})"
                ));
            }
            _ => {}
        }
    }

    /// Sends a command on queue 0; with `wait`, its answer's payload.
    fn command(
        &mut self,
        opcode: u8,
        group: u8,
        version: u8,
        payload: &[u8],
        wait: bool,
    ) -> Result<Vec<u8>> {
        let idx = self.cmd_next;
        let mut header = [0u8; 8];
        let header_len = cmd::header(
            &mut header,
            opcode,
            group,
            version,
            idx as u8,
            0,
            payload.len() as u16,
        );
        let len = header_len + payload.len();
        let address = if len <= cmd::SLOT {
            let at = idx * cmd::SLOT;
            self.commands.copy_in(at, &header[..header_len]);
            self.commands.copy_in(at + header_len, payload);
            self.commands.device + at as u64
        } else if len <= cmd::MAX {
            // One long command at a time: every caller waits for its answer.
            if !wait {
                return Err(alloc::format!(
                    "command {opcode:#x} is long: send it waiting"
                ));
            }
            self.big.copy_in(0, &header[..header_len]);
            self.big.copy_in(header_len, payload);
            self.big.device
        } else {
            return Err(alloc::format!("command {opcode:#x} too long ({len} bytes)"));
        };
        let tfd = cmd::tfd(address, len as u16);
        self.tfds.copy_in(idx * cmd::TFD_SIZE, &tfd);
        let count = cmd::byte_count(0, self.firmware.dword_byte_counts());
        self.byte_counts.write::<u16>(2 * idx, count);
        if idx < 64 {
            self.byte_counts.write::<u16>(2 * (256 + idx), count);
        }
        fence(Ordering::SeqCst);
        self.response = None;
        self.cmd_next = (idx + 1) % cmd::RING;
        self.write(reg::HBUS_TARG_WRPTR, self.cmd_next as u32);
        if !wait {
            return Ok(Vec::new());
        }
        let deadline = oceans_rt::clock_ms() + TIMEOUT_MS;
        loop {
            self.service_rx()?;
            if let Some((answered, failed, payload)) = self.response.take()
                && answered as usize == idx
            {
                if failed {
                    return Err(alloc::format!("the firmware failed command {opcode:#x}"));
                }
                return Ok(payload);
            }
            if oceans_rt::clock_ms() > deadline {
                return Err(alloc::format!("no answer to command {opcode:#x}"));
            }
            oceans_rt::sleep_ms(1);
        }
    }

    // ---- The NVM and calibration ---------------------------------------------

    fn read_section(&mut self, section: u16) -> Option<Vec<u8>> {
        let mut data = Vec::new();
        loop {
            let request = nvm::read(section, data.len() as u16);
            let answer = self.command(cmd::NVM_ACCESS, 0, 0, &request, true).ok()?;
            let chunk = nvm::answer(&answer)?;
            data.extend_from_slice(chunk);
            if chunk.len() < usize::from(nvm::CHUNK) || data.len() >= 32 * 1024 {
                return Some(data);
            }
        }
    }

    fn read_nvm(&mut self) -> Result<()> {
        let sw = self.read_section(nvm::SW);
        let mac_override = self.read_section(nvm::MAC_OVERRIDE);
        let hw = self.read_section(nvm::HW_8000);
        let regulatory = self.read_section(nvm::REGULATORY);
        let phy_sku = self.read_section(nvm::PHY_SKU);
        let size = |s: &Option<Vec<u8>>| s.as_ref().map_or(0, Vec::len);
        self.step(format_args!(
            "NVM: software {}, MAC override {}, hardware {}, regulatory {}, PHY SKU {} bytes",
            size(&sw),
            size(&mac_override),
            size(&hw),
            size(&regulatory),
            size(&phy_sku)
        ));
        let (Some(sw), Some(regulatory)) = (sw, regulatory) else {
            return Err("the NVM lacks its software or regulatory section".into());
        };
        let version = nvm::version(&sw).unwrap_or(0);
        if let Some(sku) = phy_sku.as_deref().and_then(nvm::sku) {
            if sku.tx_ant != 0 {
                self.tx_ant &= sku.tx_ant;
            }
            if sku.rx_ant != 0 {
                self.rx_ant &= sku.rx_ant;
            }
            self.band_5ghz = sku.band_5ghz;
        }
        self.lar =
            nvm::lar_enabled(&regulatory, version) && self.firmware.has_capa(fw::capa::LAR_SUPPORT);
        self.channels = nvm::channels(&regulatory, self.band_5ghz)
            .take(self.firmware.n_scan_channels as usize)
            .collect();
        self.step(format_args!(
            "NVM {version:#x}: antennas TX {:#x} RX {:#x}, 5 GHz {}, LAR {}, {} channels",
            self.tx_ant,
            self.rx_ant,
            self.band_5ghz,
            self.lar,
            self.channels.len()
        ));
        if self.tx_ant == 0 || self.rx_ant == 0 || self.channels.is_empty() {
            return Err("the NVM leaves no antenna or no channel".into());
        }
        self.mac = mac_override.as_deref().and_then(nvm::mac_override);
        if self.mac.is_none() && hw.is_some() {
            self.lock()?;
            let a = self.prph_read(reg::prph::WFMP_MAC_ADDR_0);
            let b = self.prph_read(reg::prph::WFMP_MAC_ADDR_1);
            self.unlock();
            self.mac = nvm::mac_from_registers(a, b);
        }
        match self.mac {
            Some(mac) => {
                self.step(format_args!("MAC address {}", ShowMac(mac)));
                Ok(())
            }
            None => Err("no MAC address in the NVM".into()),
        }
    }

    fn calibrate(&mut self) -> Result<()> {
        self.init_complete = false;
        self.phy_db.clear();
        self.command(cmd::SF_CFG, 0, 0, &oceans_iwm::sf_config_init(), false)?;
        let antennas = u32::from(self.tx_ant);
        self.command(
            cmd::TX_ANT_CONFIGURATION,
            0,
            0,
            &antennas.to_le_bytes(),
            true,
        )?;
        let calib = self.firmware.calib_init;
        let mut phy = [0u8; 12];
        phy[..4].copy_from_slice(&self.firmware.phy_sku.to_le_bytes());
        phy[4..8].copy_from_slice(&calib.flow.to_le_bytes());
        phy[8..].copy_from_slice(&calib.event.to_le_bytes());
        self.command(cmd::PHY_CONFIGURATION, 0, 0, &phy, true)?;
        let deadline = oceans_rt::clock_ms() + CALIBRATION_MS;
        while !(self.init_complete && !self.phy_db.is_empty()) {
            self.service_rx()?;
            if oceans_rt::clock_ms() > deadline {
                return Err(alloc::format!(
                    "calibration did not complete (INIT_COMPLETE {}, {} PHY results)",
                    self.init_complete,
                    self.phy_db.len()
                ));
            }
            oceans_rt::sleep_ms(1);
        }
        self.step(format_args!(
            "calibrated: {} PHY results kept",
            self.phy_db.len()
        ));
        Ok(())
    }

    fn stop_device(&mut self) -> Result<()> {
        self.write(reg::INT_MASK, 0);
        self.write(reg::INT, !0);
        self.lock()?;
        self.prph_write(reg::prph::SCD_TXFACT, 0);
        for ch in 0..8 {
            self.write(reg::tcsr_config(ch), 0);
        }
        self.write(reg::RCSR_CONFIG, 0);
        self.unlock();
        self.poll(reg::RSSR_STATUS, 1 << 24, 1 << 24, 10);
        self.clear(reg::GP_CNTRL, reg::gp::MAC_ACCESS_REQ);
        // apm_stop.
        self.set(reg::DBG_LINK_PWR_MGMT, 0x8000_0000);
        self.set(
            reg::HW_IF_CONFIG,
            reg::hw_if::PREPARE | reg::hw_if::ENABLE_PME,
        );
        oceans_rt::sleep_ms(1);
        self.clear(reg::DBG_LINK_PWR_MGMT, 0x8000_0000);
        oceans_rt::sleep_ms(5);
        self.set(reg::RESET, reg::RESET_STOP_MASTER);
        self.poll(
            reg::RESET,
            reg::RESET_MASTER_DISABLED,
            reg::RESET_MASTER_DISABLED,
            1,
        );
        self.clear(reg::GP_CNTRL, reg::gp::INIT_DONE);
        self.write(reg::RESET, reg::RESET_SW);
        oceans_rt::sleep_ms(5);
        self.write(reg::INT_MASK, reg::int::RF_KILL);
        self.prepare_card_hw()
    }

    // ---- REGULAR --------------------------------------------------------------

    fn load_paging(&mut self) -> Result<()> {
        let image = &self.firmware.regular;
        let (Some(css), Some(code)) = (image.paging_css, image.paging) else {
            return Ok(());
        };
        let (_, blocks, last) = paging::layout(code.len());
        // The CSS block: 4 KiB, the CSS section zero-padded.
        let css_block = Dma::new(self.device, paging::PAGE)?;
        css_block.copy_in(0, &[0; paging::PAGE]);
        css_block.copy_in(0, &css[..css.len().min(paging::PAGE)]);
        let mut addresses = alloc::vec![css_block.device];
        self.paging.push(css_block);
        for chunk in code.chunks(paging::BLOCK).take(blocks) {
            let block = Dma::new(self.device, paging::BLOCK)?;
            block.copy_in(0, &[0; paging::BLOCK]);
            block.copy_in(0, chunk);
            addresses.push(block.device);
            self.paging.push(block);
        }
        fence(Ordering::SeqCst);
        let payload = paging::command(&addresses, last);
        self.command(cmd::FW_PAGING_BLOCK, cmd::LONG_GROUP, 0, &payload, true)?;
        self.step(format_args!(
            "paging: {} blocks handed over",
            addresses.len() - 1
        ));
        Ok(())
    }

    fn runtime(&mut self) -> Result<()> {
        let antennas = u32::from(self.tx_ant);
        self.command(
            cmd::TX_ANT_CONFIGURATION,
            0,
            0,
            &antennas.to_le_bytes(),
            true,
        )?;
        for result in core::mem::take(&mut self.phy_db) {
            self.command(cmd::PHY_DB, 0, 0, &result, false)?;
            oceans_rt::sleep_ms(1);
        }
        let calib = self.firmware.calib_regular;
        let mut phy = [0u8; 12];
        phy[..4].copy_from_slice(&self.firmware.phy_sku.to_le_bytes());
        phy[4..8].copy_from_slice(&calib.flow.to_le_bytes());
        phy[8..].copy_from_slice(&calib.event.to_le_bytes());
        self.command(cmd::PHY_CONFIGURATION, 0, 0, &phy, true)?;
        let mut bt = [0u8; 8];
        bt[..4].copy_from_slice(&3u32.to_le_bytes());
        bt[4..].copy_from_slice(&0x10u32.to_le_bytes());
        self.command(cmd::BT_CONFIG, 0, 0, &bt, true)?;
        if self.firmware.has_capa(fw::capa::DQA_SUPPORT) {
            self.command(
                cmd::DQA_ENABLE,
                cmd::DATA_PATH_GROUP,
                0,
                &0u32.to_le_bytes(),
                true,
            )?;
        }
        self.step(format_args!(
            "REGULAR configured (antennas, calibration, PHY, BT, DQA)"
        ));
        self.client_mode()
    }

    /// A TX queue for station `sta` and `tid` on `fifo`, starting at 0 (the
    /// write pointer there first, then the scheduler told).
    fn enable_queue(&mut self, sta: u8, queue: u8, tid: u8, fifo: u8) -> Result<()> {
        self.lock()?;
        self.write(reg::HBUS_TARG_WRPTR, u32::from(queue) << 8);
        let config = mvm::scd_queue_cfg(sta, queue, tid, fifo, 0);
        let sent = self.command(code::SCD_QUEUE_CFG, 0, 0, &config, true);
        self.unlock();
        sent.map(drop)
            .map_err(|e| alloc::format!("queue {queue}: {e}"))
    }

    /// Everything a client needs before scanning (iwm's init_hw, after
    /// DQA), in the firmware's order.
    fn client_mode(&mut self) -> Result<()> {
        // The auxiliary station scans with, and its queue.
        self.enable_queue(ids::AUX_STA, ids::AUX_QUEUE, ids::MGMT_TID, ids::MCAST_FIFO)?;
        let answer = self.command(code::ADD_STA, 0, 0, &Station::AUX.command(), true)?;
        if !mvm::add_sta_ok(&answer) {
            return Err(alloc::format!(
                "the auxiliary station was refused (status {:#x})",
                answer.first().copied().unwrap_or(0)
            ));
        }
        // Three PHY contexts on the first channel.
        let first = self.channels[0];
        for id in 0..ids::PHY_CONTEXTS {
            let phy = mvm::phy_context(id, mvm::action::ADD, first, self.tx_ant, self.rx_ant);
            self.command(code::PHY_CONTEXT, 0, 0, &phy, true)?;
        }
        if self.firmware.has_capa(fw::capa::CT_KILL_BY_FW) {
            self.command(
                code::TEMP_REPORTING_THRESHOLDS,
                code::PHY_OPS_GROUP,
                0,
                &mvm::TEMP_THRESHOLDS_NONE,
                true,
            )?;
        }
        self.command(code::POWER_TABLE, 0, 0, &mvm::POWER_TABLE_DEVICE, true)?;
        if self.lar {
            let answer = self.command(code::MCC_UPDATE, 0, 0, &mvm::mcc_update(*b"ZZ"), true)?;
            match mvm::mcc_answer(&answer) {
                Some((status, [a, b])) => self.step(format_args!(
                    "regulatory: country {}{} (status {status})",
                    char::from(a),
                    char::from(b)
                )),
                None => return Err("MCC_UPDATE answered too short".into()),
            }
        }
        let mac = self.our_address()?;
        let slots = self.firmware.n_scan_channels as usize;
        let config = mvm::scan_config(self.tx_ant, self.rx_ant, mac, &self.channels, slots);
        self.command(code::SCAN_CFG, cmd::LONG_GROUP, 0, &config, true)?;
        // The data queues (5..8, one per AC) for the access point's
        // station, before it exists.
        for (ac, &fifo) in ids::AC_FIFOS.iter().enumerate() {
            self.enable_queue(ids::AP_STA, ids::DATA_QUEUE + ac as u8, 0, fifo)?;
        }
        self.command(code::BEACON_FILTERING, 0, 0, &mvm::BEACON_FILTER_OFF, true)?;
        self.step(format_args!(
            "client mode: auxiliary station, PHY contexts, scan configuration, data queues"
        ));
        Ok(())
    }

    /// One passive scan of every channel, until the firmware says done.
    /// One scan of every channel, until the firmware says done: passive,
    /// or probing for `ssid` (which finds a hidden network too).
    pub fn scan(&mut self, ssid: Option<&[u8]>) -> Result<()> {
        let mac = self.our_address()?;
        self.networks.clear();
        self.scan_done = None;
        self.frames = 0;
        let request = mvm::scan_request(
            &self.channels,
            ssid,
            mac,
            self.band_5ghz,
            self.firmware.n_scan_channels as usize,
        );
        self.step(format_args!(
            "scanning {} channels{}",
            self.channels.len(),
            if ssid.is_some() {
                ", probing for the network set"
            } else {
                ""
            }
        ));
        self.command(code::SCAN_REQ_UMAC, cmd::LONG_GROUP, 0, &request, true)?;
        let deadline = oceans_rt::clock_ms() + SCAN_MS;
        while self.scan_done.is_none() {
            self.service_rx()?;
            if oceans_rt::clock_ms() > deadline {
                let abort = [0u8; 8];
                let _ = self.command(code::SCAN_ABORT_UMAC, cmd::LONG_GROUP, 0, &abort, true);
                return Err(alloc::format!(
                    "the scan did not complete ({} frames, {} networks heard)",
                    self.frames,
                    self.networks.len()
                ));
            }
            oceans_rt::sleep_ms(5);
        }
        self.step(format_args!(
            "scan complete (status {}): {} frames, {} networks",
            self.scan_done.unwrap_or(0),
            self.frames,
            self.networks.len()
        ));
        Ok(())
    }

    /// A beacon or probe response heard: its network, as last heard (a
    /// beacon's DTIM kept when a probe response, which has none, follows).
    fn heard(&mut self, frame: &[u8], phy: mvm::Phy) {
        let Some(mut bss) = Bss::parse(frame, phy.channel, phy.rssi) else {
            return;
        };
        let network = |bss| Network {
            bss,
            received: phy.system_time,
        };
        match self.networks.iter().position(|n| n.bss.bssid == bss.bssid) {
            Some(i) => {
                let old = self.networks[i].bss;
                if bss.dtim_period == 0 {
                    bss.dtim_period = old.dtim_period;
                    bss.dtim_count = old.dtim_count;
                }
                self.networks[i] = network(bss);
            }
            None if self.networks.len() < 64 => self.networks.push(network(bss)),
            None => {}
        }
    }

    /// The strongest access point of the network `ssid`.
    pub fn find(&self, ssid: &[u8]) -> Option<Network> {
        self.networks
            .iter()
            .filter(|n| n.bss.ssid() == ssid)
            .max_by_key(|n| n.bss.rssi)
            .copied()
    }

    pub fn show_networks(&mut self) {
        self.networks
            .sort_by_key(|n| core::cmp::Reverse(n.bss.rssi));
        for bss in self.networks.iter().map(|n| &n.bss) {
            let ssid = core::str::from_utf8(bss.ssid()).unwrap_or("(not UTF-8)");
            let ssid = if ssid.is_empty() { "(hidden)" } else { ssid };
            let security = match bss.security {
                Security::Open => "open",
                Security::Wpa2Personal => "WPA2",
                Security::Unsupported => "unsupported security",
            };
            say(
                self.log,
                format_args!(
                    "network {ssid:?}: channel {}, {} dBm, {security}, {}",
                    bss.channel,
                    bss.rssi,
                    ShowMac(bss.bssid)
                ),
            );
        }
    }

    /// The address used on the air.
    pub fn our_address(&self) -> Result<[u8; 6]> {
        self.address
            .or(self.mac)
            .ok_or_else(|| String::from("no MAC address"))
    }

    // ---- Sending frames (queue 5) ------------------------------------------

    /// Queues one 802.11 frame described by `meta` on the data queue: the
    /// TX command and the frame's header in the first buffers, its body
    /// after them.
    pub fn send_frame(&mut self, frame: &[u8], meta: Frame) -> Result<(), &'static str> {
        let q = usize::from(ids::DATA_QUEUE);
        let slot = self.tx_cur;
        if (slot + 1) % cmd::RING == self.tx_tail {
            return Err("the data queue is full");
        }
        let header_len = meta.header_len;
        let pad = (4 - header_len % 4) % 4;
        let first = 4 + join::TX_CMD_LEN + header_len + pad;
        let body = frame
            .get(header_len..)
            .ok_or("frame shorter than its header")?;
        if first + body.len() > TX_SLOT || first < 16 {
            return Err("frame too long");
        }
        let base = slot * TX_SLOT;
        let address = self.tx_slots.device + base as u64;
        let scratch = address + 4 + join::TX_SCRATCH as u64;
        let mut head = [0u8; 4 + join::TX_CMD_LEN + 32 + 3];
        head[..4].copy_from_slice(&[join::code::TX_CMD, 0, slot as u8, ids::DATA_QUEUE]);
        head[4..4 + join::TX_CMD_LEN].copy_from_slice(&join::tx_command(&meta, scratch));
        let at = 4 + join::TX_CMD_LEN;
        head.get_mut(at..at + header_len)
            .ok_or("header too long")?
            .copy_from_slice(&frame[..header_len]);
        self.tx_slots.copy_in(base, &head[..first]);
        self.tx_slots.copy_in(base + first, body);
        let mut buffers = alloc::vec![(address, 16u16), (address + 16, (first - 16) as u16)];
        if !body.is_empty() {
            buffers.push((address + first as u64, body.len() as u16));
        }
        let ring = &self.rings[1];
        ring.copy_in(slot * cmd::TFD_SIZE, &join::tfd(&buffers));
        let count =
            join::frame_byte_count(ids::AP_STA, meta.len, self.firmware.dword_byte_counts());
        let table = q * cmd::BC_TABLE;
        self.byte_counts.write::<u16>(table + 2 * slot, count);
        if slot < 64 {
            self.byte_counts
                .write::<u16>(table + 2 * (cmd::RING + slot), count);
        }
        fence(Ordering::SeqCst);
        self.tx_cur = (slot + 1) % cmd::RING;
        self.write(reg::HBUS_TARG_WRPTR, (q << 8 | self.tx_cur) as u32);
        Ok(())
    }

    /// A TX response: the slots up to its sequence number are free again.
    fn tx_done(&mut self, payload: &[u8]) {
        let Some((sent, retries, ssn)) = join::tx_response(payload) else {
            return;
        };
        self.last_tx = Some((sent, retries));
        if !sent {
            self.tx_failed += 1;
        }
        let q = usize::from(ids::DATA_QUEUE);
        let end = usize::from(ssn) % cmd::RING;
        let table = q * cmd::BC_TABLE;
        let mut freed = 0;
        while self.tx_tail != end && freed < cmd::RING {
            let slot = self.tx_tail;
            let idle = 1 | (u16::from(ids::AP_STA) << 12);
            self.byte_counts.write::<u16>(table + 2 * slot, idle);
            if slot < 64 {
                self.byte_counts
                    .write::<u16>(table + 2 * (cmd::RING + slot), idle);
            }
            self.tx_tail = (slot + 1) % cmd::RING;
            freed += 1;
        }
    }

    // ---- Joining a network (iwm's auth and run steps) ---------------------

    /// Everything before the authentication frame: the PHY context on the
    /// network's channel, rates, the MAC context, its binding, the access
    /// point's station, and time on the channel.
    pub fn prepare_join(&mut self, network: &Network) -> Result<()> {
        if self.joined {
            self.leave();
        }
        let bss = network.bss;
        let us = self.our_address()?;
        let band_24ghz = bss.channel <= 14;
        let phy = mvm::phy_context(
            0,
            mvm::action::MODIFY,
            bss.channel,
            self.tx_ant,
            self.rx_ant,
        );
        self.command(code::PHY_CONTEXT, 0, 0, &phy, true)?;
        let lq = join::link_quality(ids::AP_STA, band_24ghz, bss.basic_rate);
        self.command(join::code::LQ, 0, 0, &lq, true)?;
        let ctx = MacContext {
            action: mvm::action::ADD,
            us,
            bssid: bss.bssid,
            band_24ghz,
            assoc: None,
        };
        self.command(
            join::code::MAC_CONTEXT,
            0,
            0,
            &join::mac_context(&ctx),
            true,
        )?;
        self.joined = true;
        let answer = self.command(
            join::code::BINDING_CONTEXT,
            0,
            0,
            &join::binding(mvm::action::ADD),
            true,
        )?;
        if !join::status_ok(&answer) {
            return Err("the binding was refused".into());
        }
        let answer = self.command(
            code::ADD_STA,
            0,
            0,
            &join::ap_station(bss.bssid).command(),
            true,
        )?;
        if !mvm::add_sta_ok(&answer) {
            return Err("the access point's station was refused".into());
        }
        let interval = u32::from(bss.beacon_interval);
        // Two beacon intervals in iwm; at least 600 TU here, so retried
        // authentication and association still find the radio on the channel
        // (the time quota that gives it the air for good comes after).
        let duration = if interval == 0 {
            1024
        } else {
            (2 * interval).max(600)
        };
        self.time_event = None;
        let answer = self.command(
            join::code::TIME_EVENT,
            0,
            0,
            &join::time_event(duration, interval.max(2) / 2),
            true,
        )?;
        if !join::status_ok(&answer) {
            return Err("time on the channel was refused".into());
        }
        // The firmware moves to the channel when the event starts.
        let deadline = oceans_rt::clock_ms() + 500;
        while self.time_event.is_none() && oceans_rt::clock_ms() < deadline {
            self.service_rx()?;
            oceans_rt::sleep_ms(1);
        }
        self.step(format_args!(
            "on channel {}: {}",
            bss.channel,
            match self.time_event {
                Some(_) => "time on the channel started",
                None => "no word of the time on the channel yet",
            }
        ));
        Ok(())
    }

    /// After the association response: the station updated, the MAC
    /// context told the association, and the commands of iwm's RUN.
    pub fn associated(&mut self, network: &Network, aid: u16) -> Result<()> {
        let bss = network.bss;
        let answer = self.command(
            code::ADD_STA,
            0,
            0,
            &join::ap_station_associated(aid).command(),
            true,
        )?;
        if !mvm::add_sta_ok(&answer) {
            return Err("the station update was refused".into());
        }
        let ctx = MacContext {
            action: mvm::action::MODIFY,
            us: self.our_address()?,
            bssid: bss.bssid,
            band_24ghz: bss.channel <= 14,
            assoc: Some(Association {
                aid,
                beacon_interval: bss.beacon_interval,
                dtim_count: bss.dtim_count,
                dtim_period: bss.dtim_period,
                tsf: bss.tsf,
                received: network.received,
            }),
        };
        self.command(
            join::code::MAC_CONTEXT,
            0,
            0,
            &join::mac_context(&ctx),
            true,
        )?;
        self.command(join::code::SF_CFG, 0, 0, &join::sf_config_full_on(), false)?;
        self.command(
            join::code::MCAST_FILTER,
            0,
            0,
            &join::mcast_filter(bss.bssid),
            true,
        )?;
        self.command(code::POWER_TABLE, 0, 0, &mvm::POWER_TABLE_DEVICE, true)?;
        let power = join::mac_power(bss.beacon_interval, bss.dtim_period);
        self.command(join::code::MAC_PM_POWER_TABLE, 0, 0, &power, true)?;
        self.command(join::code::TIME_QUOTA, 0, 0, &join::time_quota(), true)?;
        let lq = join::link_quality(ids::AP_STA, bss.channel <= 14, bss.basic_rate);
        self.command(join::code::LQ, 0, 0, &lq, true)?;
        Ok(())
    }

    /// Takes down what a join set up (best effort): the station, the
    /// binding, the MAC context.
    pub fn leave(&mut self) {
        if !self.joined {
            return;
        }
        self.joined = false;
        let _ = self.command(REMOVE_STA, 0, 0, &[ids::AP_STA, 0, 0, 0], true);
        let _ = self.command(
            join::code::BINDING_CONTEXT,
            0,
            0,
            &join::binding(mvm::action::REMOVE),
            true,
        );
        let ctx = MacContext {
            action: mvm::action::REMOVE,
            us: self.our_address().unwrap_or([0; 6]),
            bssid: [0; 6],
            band_24ghz: true,
            assoc: None,
        };
        let _ = self.command(
            join::code::MAC_CONTEXT,
            0,
            0,
            &join::mac_context(&ctx),
            true,
        );
    }

    pub fn bring_up(&mut self) -> Result<()> {
        self.attach()?;
        self.start_hw()?;
        let init = self.firmware.init.clone();
        self.load_image("INIT", &init)?;
        self.read_nvm()?;
        self.calibrate()?;
        self.stop_device()?;
        self.start_hw()?;
        let regular = self.firmware.regular.clone();
        self.load_image("REGULAR", &regular)?;
        self.load_paging()?;
        self.runtime()
    }

    /// What tells why a step failed.
    pub fn report(&self) {
        self.step(format_args!(
            "registers: INT {:#x}, INT_MASK {:#x}, GP_CNTRL {:#x}, HW_IF {:#x}, load status {:#x}, RX closed {}",
            self.read(reg::INT),
            self.read(reg::INT_MASK),
            self.read(reg::GP_CNTRL),
            self.read(reg::HW_IF_CONFIG),
            self.read(reg::FH_UCODE_LOAD_STATUS),
            self.rx_status.read::<u16>(0) & 0xfff
        ));
    }
}
