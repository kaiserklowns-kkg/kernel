//! Signs `db`, `KEK` and `PK` with the development Secure Boot key, on the
//! host, for `oceans-enroll` to hand to the firmware as they are (ADR-0091).

use std::path::PathBuf;

use oceans_dev::secure_boot::SecureBootKey;

/// Non-volatile, boot and runtime access, time-based authenticated write
/// (the attributes `main.rs` sets them with).
const ATTRIBUTES: u32 = 0x01 | 0x02 | 0x04 | 0x20;
/// EFI_GLOBAL_VARIABLE and EFI_IMAGE_SECURITY_DATABASE_GUID, as stored.
const GLOBAL_VARIABLE: [u8; 16] = [
    0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c,
];
const IMAGE_SECURITY_DATABASE: [u8; 16] = [
    0xcb, 0xb2, 0x19, 0xd7, 0x3a, 0x3d, 0x96, 0x45, 0xa3, 0xbc, 0xda, 0xd0, 0x0e, 0x67, 0x65, 0x6f,
];
/// The signature owner recorded with the certificate (any GUID will do).
const OWNER: [u8; 16] = [
    0x45, 0x7e, 0xeb, 0x0c, 0xe4, 0x0c, 0x45, 0x4e, 0x9a, 0x2e, 0x0c, 0xea, 0x45, 0x05, 0x00, 0x91,
];

fn main() {
    let key_path = PathBuf::from("../keys/oceans-dev-secure-boot.key");
    println!("cargo:rerun-if-changed={}", key_path.display());
    let text = std::fs::read_to_string(&key_path).expect("the development Secure Boot key");
    let key = SecureBootKey::from_file(&text).expect("a valid Secure Boot key file");
    // EFI_TIME 2025-01-01 00:00:00.
    let mut time = [0u8; 16];
    time[..2].copy_from_slice(&2025u16.to_le_bytes());
    time[2] = 1;
    time[3] = 1;
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    for (file, name, guid) in [
        ("db.auth", "db", IMAGE_SECURITY_DATABASE),
        ("kek.auth", "KEK", GLOBAL_VARIABLE),
        ("pk.auth", "PK", GLOBAL_VARIABLE),
    ] {
        let variable =
            key.authenticated_certificate_variable(name, &guid, ATTRIBUTES, &time, &OWNER);
        std::fs::write(out.join(file), variable).expect("OUT_DIR is writable");
    }
}
