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
const LINE_STATUS_THR_EMPTY: u8 = 1 << 5;
const IER_RECEIVED_DATA: u8 = 1 << 0;
const MCR_DTR_RTS: u8 = 0x03;
/// OUT2 gates the UART's interrupt line on PC-compatible boards.
const MCR_OUT2: u8 = 1 << 3;
/// Bytes drained per interrupt at most (the FIFO holds 16).
const DRAIN_LIMIT: usize = 64;

/// How long to wait for the transmitter before dropping a byte. Keeps the
/// kernel from hanging on machines without a working UART.
const TX_SPIN_LIMIT: u32 = 100_000;

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
    outb(INTERRUPT_ENABLE, 0x00); // polled mode
    outb(LINE_CONTROL, 0x80); // DLAB on: next two writes set the divisor
    outb(DATA, 0x01); // divisor 1 → 115200 baud
    outb(INTERRUPT_ENABLE, 0x00);
    outb(LINE_CONTROL, 0x03); // 8 data bits, no parity, 1 stop bit
    outb(FIFO_CONTROL, 0xc7); // enable + clear FIFOs, 14-byte threshold
    outb(MODEM_CONTROL, MCR_DTR_RTS);
}

fn write_byte(byte: u8) {
    for _ in 0..TX_SPIN_LIMIT {
        if inb(LINE_STATUS) & LINE_STATUS_THR_EMPTY != 0 {
            outb(DATA, byte);
            return;
        }
        core::hint::spin_loop();
    }
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

/// Raises the UART interrupt whenever received data is available.
pub fn enable_receive_interrupt() {
    outb(MODEM_CONTROL, MCR_DTR_RTS | MCR_OUT2);
    outb(INTERRUPT_ENABLE, IER_RECEIVED_DATA);
}

/// Passes every received byte to `sink` (from the interrupt handler).
pub fn drain_input(mut sink: impl FnMut(u8)) {
    for _ in 0..DRAIN_LIMIT {
        let status = inb(LINE_STATUS);
        // 0xff: no UART present (floating bus).
        if status == 0xff || status & LINE_STATUS_DATA_READY == 0 {
            return;
        }
        sink(inb(DATA));
    }
}
