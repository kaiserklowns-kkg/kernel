//! 16550-compatible UART on COM1, the early kernel console.

use ::x86_64::instructions::port::Port;

const COM1: u16 = 0x3f8;

// Register offsets from the base port.
const DATA: u16 = 0;
const INTERRUPT_ENABLE: u16 = 1;
const FIFO_CONTROL: u16 = 2;
const LINE_CONTROL: u16 = 3;
const MODEM_CONTROL: u16 = 4;
const LINE_STATUS: u16 = 5;

const LINE_STATUS_DATA_READY: u8 = 1 << 0;
/// A byte arrived with the FIFO full: input was lost.
const LINE_STATUS_OVERRUN: u8 = 1 << 1;
const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;
/// The transmitter holds nothing more: every byte has left.
const LINE_STATUS_TRANSMITTER_EMPTY: u8 = 1 << 6;
const IER_RECEIVED_DATA: u8 = 1 << 0;
const MCR_DTR_RTS: u8 = 0x03;
/// OUT2 gates the UART's interrupt line on PC-compatible boards.
const MCR_OUT2: u8 = 1 << 3;
/// Bytes drained per interrupt at most (the FIFO holds 16).
const DRAIN_LIMIT: usize = 64;

const SCRATCH: u16 = 7;

/// Whether a UART answered at boot. Without one, output is discarded
/// instead of waiting forever for a transmitter that does not exist; with
/// one, output waits for the transmitter and is never dropped (a real UART
/// always drains at the line rate).
static PRESENT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn outb(offset: u16, value: u8) {
    // SAFETY: COM1 ports are owned exclusively by this driver; writes to an
    // absent UART are ignored by the chipset.
    unsafe { Port::<u8>::new(COM1 + offset).write(value) }
}

fn inb(offset: u16) -> u8 {
    // SAFETY: as for `outb`. Reading DATA consumes a received byte, which
    // only `drain_input` does.
    unsafe { Port::<u8>::new(COM1 + offset).read() }
}

pub fn init() {
    // Presence: the scratch register reads back what was written.
    outb(SCRATCH, 0xa5);
    let present = inb(SCRATCH) == 0xa5;
    PRESENT.store(present, core::sync::atomic::Ordering::Relaxed);
    if !present {
        return;
    }
    outb(INTERRUPT_ENABLE, 0x00); // polled mode
    outb(LINE_CONTROL, 0x80); // DLAB on: next two writes set the divisor
    outb(DATA, 0x01); // divisor 1 → 115200 baud
    outb(INTERRUPT_ENABLE, 0x00);
    outb(LINE_CONTROL, 0x03); // 8 data bits, no parity, 1 stop bit
    // Enable and clear the FIFOs; interrupt at 8 bytes (ADR-0093): 8 bytes
    // of the 16 are left for the interrupt's latency, about 0.7 ms at
    // 115200 baud (at 14, two bytes: 0.17 ms). Fewer bytes waiting also
    // raise an interrupt, after four characters' time.
    outb(FIFO_CONTROL, 0x87);
    outb(MODEM_CONTROL, MCR_DTR_RTS);
}

fn write_byte(byte: u8) {
    if !PRESENT.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    while inb(LINE_STATUS) & LINE_STATUS_THR_EMPTY == 0 {
        core::hint::spin_loop();
    }
    outb(DATA, byte);
}

pub fn write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        write_byte(byte);
    }
}

pub fn write_str(s: &str) {
    for byte in s.bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
}

/// Waits until every byte written has left the UART.
pub fn drain() {
    if !PRESENT.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    while inb(LINE_STATUS) & LINE_STATUS_TRANSMITTER_EMPTY == 0 {
        core::hint::spin_loop();
    }
}

/// Raises the UART interrupt whenever received data is available.
pub fn enable_receive_interrupt() {
    outb(MODEM_CONTROL, MCR_DTR_RTS | MCR_OUT2);
    outb(INTERRUPT_ENABLE, IER_RECEIVED_DATA);
}

/// Passes every received byte to `sink` (from the interrupt handler);
/// returns whether the UART reported an overrun (input lost before the
/// kernel could read it).
pub fn drain_input(mut sink: impl FnMut(u8)) -> bool {
    let mut overrun = false;
    for _ in 0..DRAIN_LIMIT {
        let status = inb(LINE_STATUS);
        // 0xff: no UART present (floating bus).
        if status == 0xff {
            return false;
        }
        // Reading the status clears the overrun flag.
        overrun |= status & LINE_STATUS_OVERRUN != 0;
        if status & LINE_STATUS_DATA_READY == 0 {
            break;
        }
        sink(inb(DATA));
    }
    overrun
}
