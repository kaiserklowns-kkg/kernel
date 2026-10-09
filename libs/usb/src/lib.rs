//! USB for Oceans (ADR-0032), host-tested and without allocation:
//!
//! - [`descriptor`]: device, configuration, interface, endpoint and string
//!   descriptors, parsed defensively (devices are untrusted input).
//! - [`request`]: standard and HID control requests (setup packets).
//! - [`hid`]: the HID boot keyboard protocol, as console bytes.
//! - [`pointer`]: HID mice and tablets: report descriptors, boot mouse
//!   reports, decoded into input samples (ADR-0042).
//! - [`hub`]: hub descriptors, port status and requests; route strings.
//! - [`service`]: the protocol of the driver's `usb` endpoint (`lsusb`,
//!   class drivers).
//! - [`storage`]: mass storage: Bulk-Only Transport and SCSI commands.
//! - [`xhci`]: the xHCI controller's data structures: TRBs, rings and
//!   device contexts (xHCI 1.2).

#![no_std]

pub mod consumer;
pub mod descriptor;
pub mod hid;
pub mod hub;
pub mod pointer;
pub mod request;
pub mod service;
pub mod storage;
pub mod xhci;

/// Bus speeds, as xHCI port speed IDs (xHCI 1.2 §7.2.2.1.1, default
/// mapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Full,
    Low,
    High,
    Super,
    SuperPlus,
}

impl Speed {
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            1 => Self::Full,
            2 => Self::Low,
            3 => Self::High,
            4 => Self::Super,
            5 => Self::SuperPlus,
            _ => return None,
        })
    }

    pub fn id(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::Low => 2,
            Self::High => 3,
            Self::Super => 4,
            Self::SuperPlus => 5,
        }
    }

    /// Endpoint 0's packet size before the device descriptor says (USB 2.0
    /// §5.5.3; full speed may be 8–64, so 8 is safe and corrected later).
    pub fn default_max_packet0(self) -> u16 {
        match self {
            Self::Low | Self::Full => 8,
            Self::High => 64,
            Self::Super | Self::SuperPlus => 512,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "1.5 Mb/s",
            Self::Full => "12 Mb/s",
            Self::High => "480 Mb/s",
            Self::Super => "5 Gb/s",
            Self::SuperPlus => "10 Gb/s",
        }
    }
}
