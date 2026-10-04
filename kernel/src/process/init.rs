//! Starting init, the first user process (ADR-0016, ADR-0025).
//!
//! The boot loader loads one **boot archive** (`initrd`, `oceans-archive`):
//! every program and configuration file of the system. The kernel validates
//! it (structure and per-file checksums), starts the `init` it contains,
//! and gives init exactly:
//!
//! | handle | capability |
//! |---|---|
//! | 0 | kernel log (`WRITE`, `DUPLICATE`, `TRANSFER`) |
//! | 1 | the boot archive, as a memory object (`READ`, `MAP`) |
//! | 2 | system console (`READ`, `WRITE`, `DUPLICATE`, `TRANSFER`), ADR-0017 |
//! | 3 | system information (`READ`, `DUPLICATE`, `TRANSFER`), ADR-0020 |
//! | 4 | the PCI device bus (`READ`, `MANAGE`, `DUPLICATE`, `TRANSFER`), ADR-0021 |
//!
//! and its argument word: 1 in smoke-test boots (run the test manifest and
//! report through the exit code), 0 otherwise. Everything else (service
//! images, manifest, endpoints) is init's to manage; the kernel holds no
//! policy about services.

use alloc::sync::Arc;
use alloc::vec;

use oceans_archive::Archive;
use oceans_capability::Rights;

use super::{Process, spawn};
use crate::boot::BootInfo;
use crate::klog;
use crate::memory::phys_to_virt;
use crate::object::{Capability, KernelObject, MemoryObject, ObjectKind, default_rights};

/// Boot module holding the archive.
pub const ARCHIVE_MODULE: &str = "initrd";
/// The archive file that is init's program image.
pub const INIT_FILE: &str = "init";

/// The raw bytes of the boot archive module.
fn archive_bytes(boot: &BootInfo) -> Option<&'static [u8]> {
    let module = boot.module(ARCHIVE_MODULE)?;
    // SAFETY: boot modules stay mapped read-only in the direct map for the
    // kernel's lifetime (ADR-0009) and are never written.
    Some(unsafe {
        core::slice::from_raw_parts(
            phys_to_virt(module.physical_base).cast_const(),
            module.size as usize,
        )
    })
}

/// The validated boot archive, or `None` (logged) if there is none or it
/// is damaged.
pub fn archive(boot: &BootInfo) -> Option<Archive<'static>> {
    let Some(bytes) = archive_bytes(boot) else {
        klog::warn!("no `{ARCHIVE_MODULE}` boot module: no userspace will run");
        return None;
    };
    match Archive::parse(bytes) {
        Ok(archive) => Some(archive),
        Err(error) => {
            klog::error!("boot archive rejected: {error:?}");
            None
        }
    }
}

/// Starts init, or returns `None` (logged) if the boot archive is missing,
/// damaged or has no init.
pub fn start(boot: &BootInfo, test_mode: bool) -> Option<Arc<Process>> {
    let archive = archive(boot)?;
    let Some(image) = archive.find(INIT_FILE) else {
        klog::warn!("the boot archive has no `{INIT_FILE}`: no userspace will run");
        return None;
    };
    let bytes = archive_bytes(boot)?;
    let Ok(copy) = MemoryObject::new(bytes.len().max(1) as u64) else {
        klog::error!("no memory to pass the boot archive to init");
        return None;
    };
    copy.write(0, bytes).expect("sized for the archive");

    let mut initial = vec![
        Capability::new(KernelObject::Log, default_rights(ObjectKind::Log)),
        // Read-only: init unpacks what it needs from it.
        Capability::new(KernelObject::Memory(copy), Rights::READ | Rights::MAP),
        Capability::new(KernelObject::Console, default_rights(ObjectKind::Console)),
        Capability::new(
            KernelObject::SystemInfo,
            default_rights(ObjectKind::SystemInfo),
        ),
        Capability::new(
            KernelObject::DeviceBus,
            default_rights(ObjectKind::DeviceBus),
        ),
    ];
    // Handle 5, when there is a screen: the display (ADR-0057).
    if crate::display::info().is_some() {
        initial.push(Capability::new(
            KernelObject::Display,
            default_rights(ObjectKind::Display),
        ));
    }
    match spawn("init", image, initial, u64::from(test_mode)) {
        Ok(process) => {
            klog::info!(
                "init started from a boot archive of {} files ({} KiB)",
                archive.len(),
                bytes.len() / 1024
            );
            Some(process)
        }
        Err(err) => {
            klog::error!("cannot start init: {err:?}");
            None
        }
    }
}
