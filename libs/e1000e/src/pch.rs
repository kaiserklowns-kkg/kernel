//! The Intel I217, I218 and I219 (ADR-0123): the Ethernet MAC inside Intel's
//! platform controller hub (PCH, "LAN on motherboard" on most Intel PCs
//! since 2013), with an external PHY on the PCH's own link. The receive
//! and transmit side is the 82574's (legacy descriptors, the same ring
//! registers); what differs is bringing it up:
//!
//! - **The PHY may be asleep:** in ultra low power (ULP) or SMBus mode,
//!   left so by the firmware or the management engine (ME). The driver
//!   takes it out (`H2ME` with an ME, the PHY's own registers without),
//!   and power-cycles it (`LANPHYPC`) if it does not answer.
//! - **A software flag** (`EXTCNF_CTRL.SWFLAG`) is held around every PHY
//!   access and the reset: the ME shares the PHY.
//! - **No `EERD`:** the NVM is the PCH's flash; the hardware loads the
//!   address into receive address 0 at reset, where the driver reads it.
//! - **The reset** resets MAC and PHY together, and must not be followed
//!   by a register read for 20 ms.
//! - **PHY registers are paged:** page 0 at MDIO address 2, pages from 768
//!   at address 1 after a page select.
//!
//! Facts from Intel's open drivers (FreeBSD's `e1000_ich8lan.c`, BSD
//! licence), written here anew; the spec's sections are cited as "PCH".

/// The PCH generation, from the device ID. Later ones need more of the
/// workarounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Generation {
    /// Lynx Point / Wildcat Point: I217, I218 (2013–2015).
    Lpt,
    /// Sunrise Point: I219 (Skylake, Kaby Lake; 2015–2017).
    Spt,
    /// Cannon Point, Ice Point, Comet Point (2018–2020).
    Cnp,
    /// Tiger Point (2020–2021).
    Tgp,
    /// Alder Point, Raptor Lake (2021–2023).
    Adp,
    /// Meteor Lake, Lunar Lake (2023–2024).
    Mtp,
    /// Arrow Lake, Panther Point (2024–).
    Ptp,
    /// Nova Lake.
    Nvp,
}

/// Every device ID this driver takes, with its generation.
pub const DEVICES: &[(u16, Generation)] = &[
    (0x153a, Generation::Lpt),
    (0x153b, Generation::Lpt),
    (0x155a, Generation::Lpt),
    (0x1559, Generation::Lpt),
    (0x15a0, Generation::Lpt),
    (0x15a1, Generation::Lpt),
    (0x15a2, Generation::Lpt),
    (0x15a3, Generation::Lpt),
    (0x156f, Generation::Spt),
    (0x1570, Generation::Spt),
    (0x15b7, Generation::Spt),
    (0x15b8, Generation::Spt),
    (0x15b9, Generation::Spt),
    (0x15d7, Generation::Spt),
    (0x15d8, Generation::Spt),
    (0x15e3, Generation::Spt),
    (0x15d6, Generation::Spt),
    (0x0d53, Generation::Spt),
    (0x0d55, Generation::Spt),
    (0x15bd, Generation::Cnp),
    (0x15be, Generation::Cnp),
    (0x15bb, Generation::Cnp),
    (0x15bc, Generation::Cnp),
    (0x15df, Generation::Cnp),
    (0x15e0, Generation::Cnp),
    (0x15e1, Generation::Cnp),
    (0x15e2, Generation::Cnp),
    (0x0d4e, Generation::Cnp),
    (0x0d4f, Generation::Cnp),
    (0x0d4c, Generation::Cnp),
    (0x0d4d, Generation::Cnp),
    (0x15fb, Generation::Tgp),
    (0x15fc, Generation::Tgp),
    (0x15f9, Generation::Tgp),
    (0x15fa, Generation::Tgp),
    (0x15f4, Generation::Tgp),
    (0x15f5, Generation::Tgp),
    (0x1a1e, Generation::Adp),
    (0x1a1f, Generation::Adp),
    (0x1a1c, Generation::Adp),
    (0x1a1d, Generation::Adp),
    (0x550c, Generation::Adp),
    (0x550d, Generation::Adp),
    (0x0dc7, Generation::Adp),
    (0x0dc8, Generation::Adp),
    (0x0dc5, Generation::Adp),
    (0x0dc6, Generation::Adp),
    (0x550a, Generation::Mtp),
    (0x550b, Generation::Mtp),
    (0x550e, Generation::Mtp),
    (0x550f, Generation::Mtp),
    (0x5510, Generation::Mtp),
    (0x5511, Generation::Mtp),
    (0x57a0, Generation::Ptp),
    (0x57a1, Generation::Ptp),
    (0x57b3, Generation::Ptp),
    (0x57b4, Generation::Ptp),
    (0x57b5, Generation::Ptp),
    (0x57b6, Generation::Ptp),
    (0x57b7, Generation::Ptp),
    (0x57b8, Generation::Ptp),
    (0x57b9, Generation::Nvp),
    (0x57ba, Generation::Nvp),
];

/// The generation of Intel device `device`, if it is a PCH LAN this
/// driver takes.
pub fn generation(device: u16) -> Option<Generation> {
    DEVICES
        .iter()
        .find(|&&(id, _)| id == device)
        .map(|&(_, generation)| generation)
}

/// Whether the device leaves ULP alone: the early I217 and I218 parts
/// that have none (PCH §3 step 2).
pub fn without_ulp(device: u16) -> bool {
    matches!(device, 0x153a | 0x153b | 0x15a0 | 0x15a1)
}

/// The register BAR must reach the last PCH register used (`FFLT_DBG`).
pub const SPAN: usize = reg::FFLT_DBG + 4;

/// PCH registers beyond the 82574's (PCH §2).
pub mod reg {
    pub const STRAP: usize = 0x000c;
    /// The 82574's `EECD`; here an NVM-loaded K1 setting, never written.
    pub const FEXTNVM6: usize = 0x0010;
    pub const FEXTNVM4: usize = 0x0024;
    pub const FEXTNVM: usize = 0x0028;
    pub const FEXTNVM3: usize = 0x003c;
    /// The 82574's `IVAR`.
    pub const FEXTNVM7: usize = 0x00e4;
    pub const EXTCNF_CTRL: usize = 0x0f00;
    pub const EXTCNF_SIZE: usize = 0x0f08;
    pub const IOSFPC: usize = 0x0f28;
    pub const PBA: usize = 0x1000;
    pub const PBECCSTS: usize = 0x100c;
    pub const RXDCTL0: usize = 0x2828;
    pub const KABGTXD: usize = 0x3004;
    pub const TXDCTL1: usize = 0x3928;
    pub const TARC1: usize = 0x3940;
    pub const RFCTL: usize = 0x5008;
    pub const H2ME: usize = 0x5b50;
    pub const FWSM: usize = 0x5b54;
    pub const FEXTNVM11: usize = 0x5bbc;
    pub const FEXTNVM12: usize = 0x5bc0;
    pub const FFLT_DBG: usize = 0x5f04;

    pub mod ctrl {
        pub const LANPHYPC_OVERRIDE: u32 = 1 << 16;
        pub const LANPHYPC_VALUE: u32 = 1 << 17;
        pub const MEHE: u32 = 1 << 19;
    }

    pub mod status {
        pub const LAN_INIT_DONE: u32 = 1 << 9;
        pub const PHY_RESET_ASSERTED: u32 = 1 << 10;
    }

    pub mod ctrl_ext {
        pub const LPCD: u32 = 1 << 2;
        pub const DPG_EN: u32 = 1 << 3;
        pub const FORCE_SMBUS: u32 = 1 << 11;
        pub const RO_DIS: u32 = 1 << 17;
        pub const PHYPDEN: u32 = 1 << 20;
        pub const BIT_22: u32 = 1 << 22;
    }

    pub mod extcnf {
        pub const SWFLAG: u32 = 1 << 5;
        pub const GATE_PHY_CFG: u32 = 1 << 7;
    }

    pub mod fwsm {
        /// Clear while the ME blocks resetting the PHY.
        pub const RSPCIPHY: u32 = 1 << 6;
        pub const ULP_CFG_DONE: u32 = 1 << 10;
        /// The ME's firmware is running.
        pub const FW_VALID: u32 = 1 << 15;
    }

    pub mod h2me {
        pub const ULP: u32 = 1 << 11;
        pub const ENFORCE_SETTINGS: u32 = 1 << 12;
    }

    /// `FEXTNVM3`: how long the PHY's configuration takes after a power
    /// cycle; 50 ms.
    pub const PHY_CFG_COUNTER_MASK: u32 = 3 << 26;
    pub const PHY_CFG_COUNTER_50MS: u32 = 2 << 26;
    /// `FEXTNVM7`.
    pub const DISABLE_SMB_PERST: u32 = 1 << 5;
    /// `FEXTNVM11`.
    pub const DISABLE_MULR_FIX: u32 = 1 << 13;
    /// `FEXTNVM12`: PHY power-down control, P1 (Meteor Lake on).
    pub const PHYPD_CTRL_MASK: u32 = 3 << 22;
    pub const PHYPD_CTRL_P1: u32 = 1 << 23;
    /// `FEXTNVM`: the NVM holds PHY settings for software to replay.
    pub const SW_CONFIG: u32 = 1 << 27;
    /// `FEXTNVM4`: beacon duration 8 µs.
    pub const BEACON_DURATION_MASK: u32 = 7;
    pub const BEACON_DURATION_8US: u32 = 7;
    pub const KABGTXD_BGSQLBIAS: u32 = 0x0005_0000;
    pub const PBECCSTS_ECC_ENABLE: u32 = 1 << 16;
    pub const RFCTL_NFS_DISABLE: u32 = (1 << 6) | (1 << 7);
    pub const IOSFPC_RDMTS_HEX: u32 = 1 << 16;
    pub const FFLT_DBG_DMA_CLOCK: u32 = 1 << 12;
    /// `TARC0` on Sunrise Point: multi-queue request 2, not 3.
    pub const TARC0_MULTIQ_MASK: u32 = 3 << 28;
    pub const TARC0_MULTIQ_2: u32 = 2 << 28;
    /// The packet buffer: 26 KiB for receive.
    pub const PBA_26K: u32 = 0x1a;
}

/// PCI configuration space: whether the descriptor rings must be flushed
/// before a reset (Sunrise Point on, PCH §4.1).
pub const PCI_DESC_RING_STATUS: u16 = 0xe4;
pub const FLUSH_DESC_REQUIRED: u32 = 0x100;

/// The PHY's registers (PCH §4.2): `(page, register)`. Page 0 is the
/// standard MII set at MDIO address 2; pages from 768 are at address 1,
/// selected through register 31 first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhyReg {
    pub page: u16,
    pub reg: u8,
}

/// Pages from here on are at MDIO address 1.
pub const HIGH_PAGES: u16 = 768;

impl PhyReg {
    pub const fn new(page: u16, reg: u8) -> Self {
        Self { page, reg }
    }

    /// The MDIO address the register is at.
    pub const fn address(self) -> u32 {
        if self.page >= HIGH_PAGES { 1 } else { 2 }
    }

    /// The page select to write first (at address 1, register 31), if the
    /// register needs one: registers above 15 of the high pages. Page 768
    /// is selected as 0.
    pub const fn page_select(self) -> Option<u16> {
        if self.page >= HIGH_PAGES && self.reg > 15 {
            let page = if self.page == HIGH_PAGES {
                0
            } else {
                self.page
            };
            Some(page << 5)
        } else {
            None
        }
    }
}

/// The page select register, at MDIO address 1.
pub const PAGE_SELECT: u8 = 31;
pub const PAGE_SELECT_ADDRESS: u32 = 1;

/// PHY registers used (PCH §3, §4.3).
pub mod phy {
    use super::PhyReg;

    pub const CONTROL: PhyReg = PhyReg::new(0, 0);
    pub const ID1: PhyReg = PhyReg::new(0, 2);
    pub const ID2: PhyReg = PhyReg::new(0, 3);
    /// `CV_SMB_CTRL`: bit 0 forces SMBus mode.
    pub const SMB_CTRL: PhyReg = PhyReg::new(769, 23);
    pub const SMB_FORCE: u16 = 1 << 0;
    /// `BM_PORT_GEN_CFG`: bit 4 host wake-up.
    pub const PORT_GEN_CFG: PhyReg = PhyReg::new(769, 17);
    pub const HOST_WAKE_UP: u16 = 1 << 4;
    /// `HV_PM_CTRL`.
    pub const PM_CTRL: PhyReg = PhyReg::new(770, 17);
    pub const K1_ENABLE: u16 = 0x4000;
    pub const K1_CLK_REQ: u16 = 0x0200;
    /// `PHY_TIMEOUTS`: the K1 exit timeout (Meteor Lake on).
    pub const TIMEOUTS: PhyReg = PhyReg::new(770, 21);
    pub const K1_EXIT_TIMEOUT_MASK: u16 = 0x0fc0;
    pub const K1_EXIT_TIMEOUT: u16 = 0x0f00;
    /// `I217_PLL_CLOCK_GATE`.
    pub const PLL_CLOCK_GATE: PhyReg = PhyReg::new(772, 28);
    pub const PLL_GATE_MASK: u16 = 0x07ff;
    /// `I218_ULP_CONFIG1` and the bits cleared to leave ULP.
    pub const ULP_CONFIG1: PhyReg = PhyReg::new(779, 16);
    pub const ULP_START: u16 = 0x0001;
    pub const ULP_CLEARED: u16 =
        0x0004 | 0x0010 | 0x0100 | 0x0040 | 0x0020 | 0x0400 | 0x0800 | 0x1000;
    /// Sunrise Point on: a link-speed setting (PCH §4.3).
    pub const SPEED_TUNING: PhyReg = PhyReg::new(776, 20);
    /// Basic mode control.
    pub const POWER_DOWN: u16 = 1 << 11;
    pub const AUTONEG_ENABLE: u16 = 1 << 12;
    pub const RESTART_AUTONEG: u16 = 1 << 9;
}

/// The MDIC command reading `register` at MDIO `address`.
pub fn mdic_read(address: u32, register: u8) -> u32 {
    (u32::from(register & 0x1f) << 16) | ((address & 0x1f) << 21) | (2 << 26)
}

/// The MDIC command writing `value` to `register` at MDIO `address`.
pub fn mdic_write(address: u32, register: u8, value: u16) -> u32 {
    (u32::from(register & 0x1f) << 16) | ((address & 0x1f) << 21) | (1 << 26) | u32::from(value)
}

/// Whether a finished MDIC value answers `register` (the PCH's PHY may
/// answer another, which is then a failed access).
pub fn mdic_answers(mdic: u32, register: u8) -> bool {
    (mdic >> 16) & 0x1f == u32::from(register & 0x1f)
}

/// Settings that follow the link once it is up (PCH §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkTuning {
    /// `TIPG.IPGT`.
    pub ipgt: u32,
    /// `PLL_CLOCK_GATE` (the masked field).
    pub pll_gate: u16,
    /// `K1_CLK_REQ` in `PM_CTRL`.
    pub k1_clock_request: bool,
}

pub fn link_tuning(generation: Generation, mbps: u16, full_duplex: bool) -> LinkTuning {
    let ipgt = if mbps == 10 && !full_duplex {
        0xff
    } else if generation >= Generation::Spt && full_duplex && mbps != 1000 {
        0x0c
    } else {
        0x08
    };
    let pll_gate = match mbps {
        1000 if generation >= Generation::Mtp => 0x1d5,
        1000 => 0x0fa,
        _ => 0x3e8,
    };
    LinkTuning {
        ipgt,
        pll_gate,
        k1_clock_request: mbps == 1000,
    }
}

/// `SPEED_TUNING` for the link (Sunrise Point on): at 1000 Mb/s its field
/// in bits 11:2 at least 0x18; otherwise 0xc023.
pub fn speed_tuning(current: u16, mbps: u16) -> u16 {
    if mbps == 1000 {
        let field = (current >> 2) & 0x3ff;
        if field < 0x18 {
            (current & !(0x3ff << 2)) | (0x18 << 2)
        } else {
            current
        }
    } else {
        0xc023
    }
}

/// The dummy transmit descriptor that drains a ring before a reset (PCH
/// §4.1): 512 bytes from `address`, the FCS inserted.
pub fn flush_descriptor(address: u64) -> [u64; 2] {
    [address, 512 | (0x02 << 24)]
}
