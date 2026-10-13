//! Bringing up the I217, I218 and I219 (ADR-0123): the PHY woken and
//! reachable, MAC and PHY reset together, the address taken from receive
//! address 0. The register facts are in `oceans_e1000e::pch`; the steps
//! follow Intel's own order (FreeBSD's `e1000_ich8lan.c`).

use oceans_e1000e::pch::{
    self, FLUSH_DESC_REQUIRED, Generation, PAGE_SELECT, PAGE_SELECT_ADDRESS, PCI_DESC_RING_STATUS,
    PhyReg, phy,
};
use oceans_e1000e::{Mac, reg};
use oceans_nic::{Registers, wait_for};
use oceans_rt::Handle;
use oceans_virtio::Dma;

use crate::say;

/// Holding the software flag: up to 100 ms for the ME to let go, 1 s for
/// ours to take.
const SWFLAG_FREE_MS: u64 = 100;
const SWFLAG_TAKE_MS: u64 = 1000;
/// The ME's ULP exit: up to 2.5 s.
const ULP_DONE_MS: u64 = 2500;
/// `LAN_INIT_DONE` after a reset: up to 150 ms.
const LAN_INIT_MS: u64 = 150;
/// One PHY access: up to ~100 ms.
const MDIC_MS: u64 = 100;
const MASTER_DISABLE_MS: u64 = 100;

pub struct Pch {
    pub regs: Registers,
    device: Handle,
    pub generation: Generation,
    id: u16,
    log: Handle,
    /// The ME may block resetting the PHY.
    reset_blocked: bool,
}

/// The software flag, released when dropped.
struct Flag(Registers);

impl Drop for Flag {
    fn drop(&mut self) {
        self.0
            .update(pch::reg::EXTCNF_CTRL, 0, pch::reg::extcnf::SWFLAG);
    }
}

impl Pch {
    pub fn new(
        regs: Registers,
        device: Handle,
        id: u16,
        generation: Generation,
        log: Handle,
    ) -> Self {
        Self {
            regs,
            device,
            generation,
            id,
            log,
            reset_blocked: false,
        }
    }

    /// The controller, for the log.
    pub fn name(&self) -> &'static str {
        match self.generation {
            Generation::Lpt => "I217/I218",
            _ => "I219",
        }
    }

    fn flag(&self) -> Result<Flag, &'static str> {
        let regs = self.regs;
        let free = || regs.read(pch::reg::EXTCNF_CTRL) & pch::reg::extcnf::SWFLAG == 0;
        if !wait_for(SWFLAG_FREE_MS, free) {
            return Err("the PHY's software flag stays taken (the ME holds it)");
        }
        regs.update(pch::reg::EXTCNF_CTRL, pch::reg::extcnf::SWFLAG, 0);
        let taken = || regs.read(pch::reg::EXTCNF_CTRL) & pch::reg::extcnf::SWFLAG != 0;
        if !wait_for(SWFLAG_TAKE_MS, taken) {
            regs.update(pch::reg::EXTCNF_CTRL, 0, pch::reg::extcnf::SWFLAG);
            return Err("the PHY's software flag cannot be taken");
        }
        Ok(Flag(regs))
    }

    fn mdic(&self, command: u32, register: u8) -> Result<u16, &'static str> {
        let regs = self.regs;
        regs.write(reg::MDIC, command);
        let mut value = 0;
        let done = wait_for(MDIC_MS, || {
            value = regs.read(reg::MDIC);
            value & (1 << 28) != 0
        });
        if !done {
            return Err("PHY access timed out");
        }
        if value & (1 << 30) != 0 || !pch::mdic_answers(value, register) {
            return Err("PHY access failed");
        }
        Ok(value as u16)
    }

    fn select(&self, register: PhyReg) -> Result<(), &'static str> {
        if let Some(page) = register.page_select() {
            self.mdic(
                pch::mdic_write(PAGE_SELECT_ADDRESS, PAGE_SELECT, page),
                PAGE_SELECT,
            )?;
        }
        Ok(())
    }

    /// Reads a PHY register; the software flag must be held.
    fn read_locked(&self, register: PhyReg) -> Result<u16, &'static str> {
        self.select(register)?;
        self.mdic(
            pch::mdic_read(register.address(), register.reg),
            register.reg,
        )
    }

    /// Writes a PHY register; the software flag must be held.
    fn write_locked(&self, register: PhyReg, value: u16) -> Result<(), &'static str> {
        self.select(register)?;
        self.mdic(
            pch::mdic_write(register.address(), register.reg, value),
            register.reg,
        )
        .map(drop)
    }

    fn update_locked(&self, register: PhyReg, set: u16, clear: u16) -> Result<(), &'static str> {
        let value = self.read_locked(register)?;
        self.write_locked(register, (value | set) & !clear)
    }

    fn me_present(&self) -> bool {
        self.regs.read(pch::reg::FWSM) & pch::reg::fwsm::FW_VALID != 0
    }

    /// Whether the ME lets the driver reset the PHY: up to ~300 ms.
    fn check_reset_blocked(&mut self) {
        let regs = self.regs;
        self.reset_blocked = !wait_for(310, || {
            regs.read(pch::reg::FWSM) & pch::reg::fwsm::RSPCIPHY != 0
        });
    }

    /// Whether the PHY answers (its ID); without an ME, SMBus mode is then
    /// unforced on both sides.
    fn phy_accessible(&self) -> bool {
        let id = |reg| self.read_locked(reg).ok().filter(|&v| v != 0xffff);
        let answers = (0..2).any(|_| id(phy::ID1).is_some() && id(phy::ID2).is_some());
        if answers && !self.me_present() {
            // This write may report an error; it takes effect regardless.
            let _ = self.update_locked(phy::SMB_CTRL, 0, phy::SMB_FORCE);
            self.regs
                .update(reg::CTRL_EXT, 0, pch::reg::ctrl_ext::FORCE_SMBUS);
        }
        answers
    }

    /// Power-cycles the PHY (`LANPHYPC`), PCH §3 step 3.
    fn toggle_lanphypc(&self) {
        let regs = self.regs;
        regs.replace(
            pch::reg::FEXTNVM3,
            pch::reg::PHY_CFG_COUNTER_MASK,
            pch::reg::PHY_CFG_COUNTER_50MS,
        );
        regs.update(
            reg::CTRL,
            pch::reg::ctrl::LANPHYPC_OVERRIDE,
            pch::reg::ctrl::LANPHYPC_VALUE,
        );
        let _ = regs.read(reg::STATUS);
        oceans_rt::sleep_ms(1);
        regs.update(reg::CTRL, 0, pch::reg::ctrl::LANPHYPC_OVERRIDE);
        let _ = regs.read(reg::STATUS);
        let mut polls = 0;
        while regs.read(reg::CTRL_EXT) & pch::reg::ctrl_ext::LPCD == 0 && polls < 20 {
            oceans_rt::sleep_ms(5);
            polls += 1;
        }
        oceans_rt::sleep_ms(30);
    }

    /// Takes the PHY out of ultra low power, PCH §3 step 2.
    fn disable_ulp(&self) -> Result<(), &'static str> {
        if pch::without_ulp(self.id) {
            return Ok(());
        }
        let regs = self.regs;
        if self.me_present() {
            regs.update(
                pch::reg::H2ME,
                pch::reg::h2me::ENFORCE_SETTINGS,
                pch::reg::h2me::ULP,
            );
            let done = wait_for(ULP_DONE_MS, || {
                regs.read(pch::reg::FWSM) & pch::reg::fwsm::ULP_CFG_DONE == 0
            });
            regs.update(pch::reg::H2ME, 0, pch::reg::h2me::ENFORCE_SETTINGS);
            return if done {
                Ok(())
            } else {
                Err("the ME did not take the PHY out of low power")
            };
        }
        let flag = self.flag()?;
        self.toggle_lanphypc();
        if self.read_locked(phy::SMB_CTRL).is_err() {
            regs.update(reg::CTRL_EXT, pch::reg::ctrl_ext::FORCE_SMBUS, 0);
            oceans_rt::sleep_ms(50);
        }
        self.update_locked(phy::SMB_CTRL, 0, phy::SMB_FORCE)?;
        regs.update(reg::CTRL_EXT, 0, pch::reg::ctrl_ext::FORCE_SMBUS);
        self.update_locked(phy::PM_CTRL, phy::K1_ENABLE, 0)?;
        let config = self.read_locked(phy::ULP_CONFIG1)? & !phy::ULP_CLEARED;
        self.write_locked(phy::ULP_CONFIG1, config)?;
        self.write_locked(phy::ULP_CONFIG1, config | phy::ULP_START)?;
        regs.update(pch::reg::FEXTNVM7, 0, pch::reg::DISABLE_SMB_PERST);
        drop(flag);
        self.phy_reset()?;
        oceans_rt::sleep_ms(50);
        Ok(())
    }

    /// Resets the PHY alone, PCH §3 step 1.7.
    fn phy_reset(&self) -> Result<(), &'static str> {
        if self.reset_blocked {
            return Ok(());
        }
        let regs = self.regs;
        {
            let _flag = self.flag()?;
            regs.update(reg::CTRL, reg::ctrl::PHY_RESET, 0);
            let _ = regs.read(reg::STATUS);
            oceans_rt::sleep_ms(1);
            regs.update(reg::CTRL, 0, reg::ctrl::PHY_RESET);
            let _ = regs.read(reg::STATUS);
            oceans_rt::sleep_ms(1);
        }
        self.configuration_done();
        Ok(())
    }

    /// After a PHY reset: the LAN's configuration loaded (PCH §3 step 4.8).
    fn configuration_done(&self) {
        let regs = self.regs;
        oceans_rt::sleep_ms(10);
        if !wait_for(LAN_INIT_MS, || {
            regs.read(reg::STATUS) & pch::reg::status::LAN_INIT_DONE != 0
        }) {
            say(
                self.log,
                format_args!("the LAN's configuration did not report done"),
            );
        }
        regs.update(reg::STATUS, 0, pch::reg::status::LAN_INIT_DONE);
        if regs.read(reg::STATUS) & pch::reg::status::PHY_RESET_ASSERTED != 0 {
            regs.update(reg::STATUS, 0, pch::reg::status::PHY_RESET_ASSERTED);
        }
    }

    /// The K1 exit timeout (Meteor Lake on), PCH §3 step 5; the flag held.
    fn k1_exit_timeout(&self) -> Result<(), &'static str> {
        self.regs.replace(
            pch::reg::FEXTNVM12,
            pch::reg::PHYPD_CTRL_MASK,
            pch::reg::PHYPD_CTRL_P1,
        );
        oceans_rt::sleep_ms(1);
        let value = self.read_locked(phy::TIMEOUTS)?;
        self.write_locked(
            phy::TIMEOUTS,
            (value & !phy::K1_EXIT_TIMEOUT_MASK) | phy::K1_EXIT_TIMEOUT,
        )
    }

    /// The PHY woken and reachable, PCH §3 step 1.
    fn wake_phy(&mut self) -> Result<(), &'static str> {
        let regs = self.regs;
        regs.update(pch::reg::EXTCNF_CTRL, pch::reg::extcnf::GATE_PHY_CFG, 0);
        if let Err(problem) = self.disable_ulp() {
            // Intel's drivers go on: the PHY may answer anyway.
            say(self.log, format_args!("{problem}; going on"));
        }
        self.check_reset_blocked();
        {
            let _flag = self.flag()?;
            if self.generation >= Generation::Mtp {
                let _ = self.k1_exit_timeout();
            }
            if !self.phy_accessible() {
                regs.update(reg::CTRL_EXT, pch::reg::ctrl_ext::FORCE_SMBUS, 0);
                oceans_rt::sleep_ms(50);
                if !self.phy_accessible() {
                    if self.reset_blocked {
                        return Err("the PHY does not answer, and the ME blocks power-cycling it");
                    }
                    self.toggle_lanphypc();
                    if !self.phy_accessible() {
                        regs.update(reg::CTRL_EXT, 0, pch::reg::ctrl_ext::FORCE_SMBUS);
                        if !self.phy_accessible() {
                            return Err("the PHY does not answer");
                        }
                    }
                }
            }
        }
        self.phy_reset()?;
        self.check_reset_blocked();
        if self.generation >= Generation::Mtp {
            let _flag = self.flag()?;
            self.k1_exit_timeout()?;
        }
        Ok(())
    }

    /// Drains the descriptor rings if the PCH asks to (Sunrise Point on,
    /// PCH §4.1), through `scratch`: a page of the driver's own DMA memory,
    /// used as a one-descriptor ring.
    fn flush_rings(&self, scratch: &Dma) {
        let regs = self.regs;
        regs.update(pch::reg::FEXTNVM11, pch::reg::DISABLE_MULR_FIX, 0);
        let required = || {
            oceans_rt::device_config_read(self.device, PCI_DESC_RING_STATUS, 2)
                .is_ok_and(|status| status & FLUSH_DESC_REQUIRED != 0)
        };
        if !required() {
            return;
        }
        say(
            self.log,
            format_args!("draining the descriptor rings before the reset"),
        );
        // Transmit: a ring of our own, one 512-byte frame of its own bytes.
        let split = |address: u64| (address as u32, (address >> 32) as u32);
        let (low, high) = split(scratch.device);
        regs.write(reg::TDBAL, low);
        regs.write(reg::TDBAH, high);
        regs.write(reg::TDLEN, 128);
        regs.write(reg::TDH, 0);
        regs.write(reg::TDT, 0);
        let [address, command] = pch::flush_descriptor(scratch.device);
        scratch.write(0, address);
        scratch.write(8, command);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        regs.update(reg::TCTL, reg::tctl::ENABLE, 0);
        regs.write(reg::TDT, 1);
        let _ = regs.read(reg::STATUS);
        oceans_rt::sleep_ms(1);
        if !required() {
            return;
        }
        // Receive: thresholds that let the ring empty, briefly enabled.
        regs.update(reg::RCTL, 0, reg::rctl::ENABLE);
        let _ = regs.read(reg::STATUS);
        oceans_rt::sleep_ms(1);
        let rxdctl = (regs.read(pch::reg::RXDCTL0) & !0x3fff) | 0x1f | (1 << 8) | (1 << 24);
        regs.write(pch::reg::RXDCTL0, rxdctl);
        regs.update(reg::RCTL, reg::rctl::ENABLE, 0);
        let _ = regs.read(reg::STATUS);
        oceans_rt::sleep_ms(1);
        regs.update(reg::RCTL, 0, reg::rctl::ENABLE);
    }

    /// The hang workaround around the reset: a configuration read, its
    /// value written to the read-only `STRAP` (PCH §3 step 4.5).
    fn strap_delay(&self) {
        if let Ok(id) = oceans_rt::device_config_read(self.device, 0, 4) {
            self.regs.write(pch::reg::STRAP, id);
        }
    }

    /// Resets MAC and PHY, PCH §3 step 4.
    fn reset(&mut self, scratch: &Dma) -> Result<(), &'static str> {
        let regs = self.regs;
        if self.generation >= Generation::Spt {
            self.flush_rings(scratch);
        }
        regs.update(reg::CTRL, reg::ctrl::GIO_MASTER_DISABLE, 0);
        if !wait_for(MASTER_DISABLE_MS, || {
            regs.read(reg::STATUS) & reg::status::GIO_MASTER_ENABLE == 0
        }) {
            say(
                self.log,
                format_args!("bus-master requests still pending; resetting anyway"),
            );
        }
        regs.write(reg::IMC, reg::int::ALL);
        regs.write(reg::RCTL, 0);
        regs.write(reg::TCTL, reg::tctl::PAD_SHORT_PACKETS);
        let _ = regs.read(reg::STATUS);
        oceans_rt::sleep_ms(10);
        let mut ctrl = regs.read(reg::CTRL);
        if !self.reset_blocked {
            ctrl |= reg::ctrl::PHY_RESET;
        }
        // Released by the reset itself.
        core::mem::forget(self.flag()?);
        self.strap_delay();
        // No register read now: it can hang the PCH. The reset takes 20 ms.
        regs.write(reg::CTRL, ctrl | reg::ctrl::RESET);
        oceans_rt::sleep_ms(20);
        self.strap_delay();
        if !self.reset_blocked {
            self.configuration_done();
            self.after_phy_reset()?;
        }
        regs.write(reg::IMC, reg::int::ALL);
        let _ = regs.read(reg::ICR);
        regs.update(pch::reg::KABGTXD, pch::reg::KABGTXD_BGSQLBIAS, 0);
        if self.generation >= Generation::Ptp {
            regs.update(reg::CTRL_EXT, 0, pch::reg::ctrl_ext::DPG_EN);
        }
        Ok(())
    }

    /// After the PHY's reset (PCH §3 step 4.9): no host wake-up. The NVM's
    /// extended PHY settings are not replayed (they are in the PCH's
    /// flash); that is said when the NVM has them.
    fn after_phy_reset(&self) -> Result<(), &'static str> {
        oceans_rt::sleep_ms(10);
        let _flag = self.flag()?;
        self.update_locked(phy::PORT_GEN_CFG, 0, phy::HOST_WAKE_UP)?;
        if self.regs.read(pch::reg::FEXTNVM) & pch::reg::SW_CONFIG != 0
            && self.regs.read(pch::reg::EXTCNF_SIZE) & 0x00ff_0000 != 0
        {
            say(
                self.log,
                format_args!("the NVM's extended PHY settings are not applied (not supported)"),
            );
        }
        Ok(())
    }

    /// Settings Intel's drivers apply after the reset (PCH §3 step 7, §3
    /// step 8).
    fn initialize_hardware_bits(&self) {
        let regs = self.regs;
        regs.update(
            reg::CTRL_EXT,
            pch::reg::ctrl_ext::BIT_22 | pch::reg::ctrl_ext::PHYPDEN | pch::reg::ctrl_ext::RO_DIS,
            0,
        );
        regs.update(reg::TXDCTL, 1 << 22, 0);
        regs.update(pch::reg::TXDCTL1, 1 << 22, 0);
        regs.update(reg::TARC0, (1 << 23) | (1 << 24) | (1 << 26) | (1 << 27), 0);
        // TCTL.MULR is not set: bit 28 set.
        regs.update(
            pch::reg::TARC1,
            (1 << 24) | (1 << 26) | (1 << 30) | (1 << 28),
            0,
        );
        regs.update(pch::reg::RFCTL, pch::reg::RFCTL_NFS_DISABLE, 0);
        regs.update(pch::reg::PBECCSTS, pch::reg::PBECCSTS_ECC_ENABLE, 0);
        regs.update(reg::CTRL, pch::reg::ctrl::MEHE, 0);
        if self.generation >= Generation::Tgp {
            regs.update(pch::reg::FFLT_DBG, pch::reg::FFLT_DBG_DMA_CLOCK, 0);
        }
        if self.generation == Generation::Spt {
            regs.update(pch::reg::IOSFPC, pch::reg::IOSFPC_RDMTS_HEX, 0);
            regs.replace(
                reg::TARC0,
                pch::reg::TARC0_MULTIQ_MASK,
                pch::reg::TARC0_MULTIQ_2,
            );
        }
        regs.write(pch::reg::PBA, pch::reg::PBA_26K);
        regs.replace(
            pch::reg::FEXTNVM4,
            pch::reg::BEACON_DURATION_MASK,
            pch::reg::BEACON_DURATION_8US,
        );
    }

    /// The whole bring-up: the station address, or why not. `scratch` is
    /// the driver's ring memory, not yet in use.
    pub fn bring_up(&mut self, scratch: &Dma) -> Result<Mac, &'static str> {
        let regs = self.regs;
        regs.write(reg::IMC, reg::int::ALL);
        self.wake_phy()?;
        self.reset(scratch)?;
        // Loaded from the NVM by the reset.
        let mac =
            oceans_e1000e::mac_from_receive_address(regs.read(reg::RAL0), regs.read(reg::RAH0))
                .ok_or("no valid MAC address in receive address 0")?;
        self.initialize_hardware_bits();
        Ok(mac)
    }

    /// Full speed while running: low-power link-up and gigabit-off cleared
    /// in `PHY_CTRL` and in the PHY's OEM bits, and autonegotiation
    /// restarted to apply them. A system before may have left them set for
    /// Wake on LAN (Linux does on power-down); the PHY keeps them while
    /// standby power lasts. Returns what they were (`PHY_CTRL`, OEM bits).
    pub fn full_speed(&self) -> Result<(u32, u16), &'static str> {
        let regs = self.regs;
        let control = regs.read(pch::reg::PHY_CTRL);
        regs.update(
            pch::reg::PHY_CTRL,
            0,
            pch::reg::D0A_LPLU | pch::reg::GBE_DISABLE,
        );
        let _flag = self.flag()?;
        let oem = self.read_locked(phy::OEM_BITS)?;
        let mut value = oem & !(phy::OEM_LPLU | phy::OEM_GBE_DISABLE);
        if !self.reset_blocked {
            value |= phy::OEM_RESTART_AUTONEG;
        }
        self.write_locked(phy::OEM_BITS, value)?;
        Ok((control, oem))
    }

    /// Autonegotiation, powered up, from the start (PCH §5).
    pub fn restart_autonegotiation(&self) -> Result<(), &'static str> {
        let _flag = self.flag()?;
        self.update_locked(
            phy::CONTROL,
            phy::AUTONEG_ENABLE | phy::RESTART_AUTONEG,
            phy::POWER_DOWN,
        )
    }

    /// What follows the link once it is up (PCH §4.3).
    pub fn link_up(&self, mbps: u16, full_duplex: bool) -> Result<(), &'static str> {
        let tuning = pch::link_tuning(self.generation, mbps, full_duplex);
        self.regs.replace(reg::TIPG, 0x3ff, tuning.ipgt);
        let _flag = self.flag()?;
        let gate = self.read_locked(phy::PLL_CLOCK_GATE)?;
        self.write_locked(
            phy::PLL_CLOCK_GATE,
            (gate & !phy::PLL_GATE_MASK) | tuning.pll_gate,
        )?;
        if tuning.k1_clock_request {
            self.update_locked(phy::PM_CTRL, phy::K1_CLK_REQ, 0)?;
        }
        if self.generation >= Generation::Spt {
            let current = self.read_locked(phy::SPEED_TUNING)?;
            self.write_locked(phy::SPEED_TUNING, pch::speed_tuning(current, mbps))?;
        }
        Ok(())
    }
}
