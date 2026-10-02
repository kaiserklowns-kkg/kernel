//! Legacy 8259 PIC.
//!
//! Oceans will route device interrupts through the APIC (Phase 2). The PIC is
//! remapped away from the exception vectors so a stray or spurious IRQ cannot
//! masquerade as a CPU exception, then fully masked.

use ::x86_64::instructions::port::Port;

use crate::klog;

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xa0;
const PIC2_DATA: u16 = 0xa1;

/// First vector for each PIC after remapping; just above the 32 exceptions.
const PIC1_OFFSET: u8 = 32;
const PIC2_OFFSET: u8 = 40;

const ICW1_INIT_WITH_ICW4: u8 = 0x11;
const ICW4_8086: u8 = 0x01;

fn out(port: u16, value: u8) {
    // SAFETY: the PIC ports are owned by this module; interrupts are disabled
    // during initialisation so the sequence cannot be interleaved. Port 0x80
    // (POST diagnostics) is a conventional no-op write that gives old PICs
    // time to settle between commands.
    unsafe {
        Port::<u8>::new(port).write(value);
        Port::<u8>::new(0x80).write(0);
    }
}

pub fn remap_and_mask() {
    out(PIC1_COMMAND, ICW1_INIT_WITH_ICW4);
    out(PIC2_COMMAND, ICW1_INIT_WITH_ICW4);
    out(PIC1_DATA, PIC1_OFFSET);
    out(PIC2_DATA, PIC2_OFFSET);
    out(PIC1_DATA, 4); // secondary PIC on IRQ2
    out(PIC2_DATA, 2); // secondary cascade identity
    out(PIC1_DATA, ICW4_8086);
    out(PIC2_DATA, ICW4_8086);
    out(PIC1_DATA, 0xff);
    out(PIC2_DATA, 0xff);
    klog::debug!(
        "legacy PIC remapped to vectors {PIC1_OFFSET}..{} and masked",
        PIC2_OFFSET + 8
    );
}
