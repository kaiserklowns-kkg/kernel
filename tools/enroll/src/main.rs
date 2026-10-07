//! `oceans-enroll`: enrols the development Secure Boot certificate in UEFI
//! firmware that is in **Setup Mode** (no platform key yet), as `db`, `KEK`
//! and finally `PK`. Writing `PK` takes the firmware to User Mode, where it
//! checks every executable it runs (ADR-0091).
//!
//! For `cargo xtask smoke-secure-boot`, on QEMU's OVMF firmware with a
//! fresh variable store. The three variables are time-based authenticated
//! variables signed with the development key, made on the host (`build.rs`):
//! firmware that wants even the first `PK` self-signed gets that. On a
//! real machine, enrol the certificate from the firmware's own setup
//! screens instead.
//!
//! It says what it did on the console and switches the machine off.

#![no_std]
#![no_main]

use core::ffi::c_void;

static DB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/db.auth"));
static KEK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kek.auth"));
static PK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pk.auth"));

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

const GLOBAL_VARIABLE: Guid = Guid(
    0x8be4_df61,
    0x93ca,
    0x11d2,
    [0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c],
);
const IMAGE_SECURITY_DATABASE: Guid = Guid(
    0xd719_b2cb,
    0x3d3a,
    0x4596,
    [0xa3, 0xbc, 0xda, 0xd0, 0x0e, 0x67, 0x65, 0x6f],
);

/// Non-volatile, boot and runtime access, time-based authenticated write
/// (what `build.rs` signed them for).
const ATTRIBUTES: u32 = 0x01 | 0x02 | 0x04 | 0x20;

#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

#[repr(C)]
struct TextOutput {
    reset: usize,
    output_string: unsafe extern "efiapi" fn(*mut TextOutput, *const u16) -> usize,
}

#[repr(C)]
struct RuntimeServices {
    header: TableHeader,
    get_time: usize,
    set_time: usize,
    get_wakeup_time: usize,
    set_wakeup_time: usize,
    set_virtual_address_map: usize,
    convert_pointer: usize,
    get_variable: unsafe extern "efiapi" fn(
        *const u16,
        *const Guid,
        *mut u32,
        *mut usize,
        *mut c_void,
    ) -> usize,
    get_next_variable_name: usize,
    set_variable:
        unsafe extern "efiapi" fn(*const u16, *const Guid, u32, usize, *const c_void) -> usize,
    get_next_high_monotonic_count: usize,
    reset_system: unsafe extern "efiapi" fn(u32, usize, usize, *const c_void) -> !,
}

#[repr(C)]
struct SystemTable {
    header: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: *mut c_void,
    console_in: *mut c_void,
    console_out_handle: *mut c_void,
    console_out: *mut TextOutput,
    standard_error_handle: *mut c_void,
    standard_error: *mut c_void,
    runtime_services: *mut RuntimeServices,
}

/// An ASCII text as UTF-16 with a terminator, in a fixed buffer.
struct Wide([u16; 96]);

impl Wide {
    fn new(text: &str) -> Self {
        let mut wide = [0u16; 96];
        for (slot, byte) in wide.iter_mut().zip(text.bytes().take(95)) {
            *slot = u16::from(byte);
        }
        Self(wide)
    }
}

struct Firmware<'a> {
    system: &'a SystemTable,
}

impl Firmware<'_> {
    fn say(&self, text: &str) {
        let line = Wide::new(text);
        // SAFETY: the firmware's console, a valid NUL-terminated string.
        unsafe {
            ((*self.system.console_out).output_string)(self.system.console_out, line.0.as_ptr())
        };
    }

    fn runtime(&self) -> &RuntimeServices {
        // SAFETY: the system table's runtime services, valid while booting.
        unsafe { &*self.system.runtime_services }
    }

    /// Whether the firmware is in Setup Mode (the `SetupMode` variable is 1).
    fn setup_mode(&self) -> bool {
        let name = Wide::new("SetupMode");
        let (mut value, mut size, mut attributes) = (0u8, 1usize, 0u32);
        // SAFETY: valid name, GUID and a one-byte buffer.
        let status = unsafe {
            (self.runtime().get_variable)(
                name.0.as_ptr(),
                &GLOBAL_VARIABLE,
                &mut attributes,
                &mut size,
                (&raw mut value).cast(),
            )
        };
        status == 0 && value == 1
    }

    /// Sets authenticated variable `name` to `data` (made by `build.rs`).
    fn set(&self, name: &str, guid: &Guid, data: &[u8]) -> usize {
        let wide = Wide::new(name);
        // SAFETY: valid name, GUID and `data`.
        unsafe {
            (self.runtime().set_variable)(
                wide.0.as_ptr(),
                guid,
                ATTRIBUTES,
                data.len(),
                data.as_ptr().cast(),
            )
        }
    }

    fn off(&self) -> ! {
        // SAFETY: EfiResetShutdown with a success status.
        unsafe { (self.runtime().reset_system)(2, 0, 0, core::ptr::null()) }
    }
}

#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(_image: *mut c_void, system: *const SystemTable) -> usize {
    // SAFETY: the firmware passes a valid system table.
    let firmware = Firmware {
        system: unsafe { &*system },
    };
    if !firmware.setup_mode() {
        firmware.say("oceans-enroll: the firmware is not in Setup Mode; nothing enrolled\r\n");
        firmware.off();
    }
    for (name, guid, data) in [
        ("db", &IMAGE_SECURITY_DATABASE, DB),
        ("KEK", &GLOBAL_VARIABLE, KEK),
        ("PK", &GLOBAL_VARIABLE, PK),
    ] {
        let status = firmware.set(name, guid, data);
        if status != 0 {
            // "...refused NAME (status 0x8000000000000002)"
            let mut text = [0u8; 80];
            let mut at = 0;
            let mut put = |bytes: &[u8]| {
                text[at..at + bytes.len()].copy_from_slice(bytes);
                at += bytes.len();
            };
            put(b"oceans-enroll: the firmware refused ");
            put(name.as_bytes());
            put(b" (status 0x");
            for shift in (0..16).rev() {
                put(&[b"0123456789abcdef"[(status >> (shift * 4)) & 0xf]]);
            }
            put(b")\r\n");
            firmware.say(core::str::from_utf8(&text[..at]).unwrap_or("?"));
            firmware.off();
        }
    }
    firmware
        .say("oceans-enroll: enrolled the Oceans development certificate in db, KEK and PK\r\n");
    firmware.off()
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
