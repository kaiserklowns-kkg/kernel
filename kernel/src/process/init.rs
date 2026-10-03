//! Starting init, the first user process (ADR-0016).
//!
//! init receives exactly:
//!
//! | handle | capability |
//! |---|---|
//! | 0 | kernel log (`WRITE`, `DUPLICATE`, `TRANSFER`) |
//! | 1 | boot module table: memory object (`READ`, `MAP`) holding lines `<name> <handle index>` |
//! | 2 | system console (`READ`, `WRITE`, `DUPLICATE`, `TRANSFER`), ADR-0017 |
//! | 3… | each boot module as a memory object (`READ`, `MAP`) |
//!
//! and its argument word: 1 in smoke-test boots (run the test manifest and
//! report through the exit code), 0 otherwise. Everything else (service
//! images, manifest, endpoints) is init's to manage; the kernel holds no
//! policy about services.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_abi::start::MAX_INITIAL_HANDLES;
use oceans_capability::Rights;

use super::{Process, spawn};
use crate::boot::{BootInfo, BootModule};
use crate::klog;
use crate::memory::phys_to_virt;
use crate::object::{Capability, KernelObject, MemoryObject, ObjectKind, default_rights};

/// Boot module name of init's program image.
pub const INIT_MODULE: &str = "init";

/// Handles before the modules: log, module table, console.
const FIXED_HANDLES: usize = 3;

fn module_bytes(module: &BootModule) -> &'static [u8] {
    // SAFETY: boot modules stay mapped read-only in the direct map for the
    // kernel's lifetime (ADR-0009) and are never written.
    unsafe {
        core::slice::from_raw_parts(
            phys_to_virt(module.physical_base).cast_const(),
            module.size as usize,
        )
    }
}

fn read_only(bytes: &[u8]) -> Option<Capability> {
    let object = MemoryObject::new(bytes.len().max(1) as u64).ok()?;
    object.write(0, bytes).ok()?;
    Some(Capability::new(
        KernelObject::Memory(object),
        Rights::READ | Rights::MAP,
    ))
}

/// Starts init, or returns `None` (logged) if the boot image has none.
pub fn start(boot: &BootInfo, test_mode: bool) -> Option<Arc<Process>> {
    let Some(init) = boot.module(INIT_MODULE) else {
        klog::warn!("no `{INIT_MODULE}` boot module: no userspace will run");
        return None;
    };

    let mut table = String::new();
    let mut modules = Vec::new();
    for module in boot.modules() {
        if FIXED_HANDLES + modules.len() >= MAX_INITIAL_HANDLES {
            klog::warn!(
                "boot module {} not passed to init: handle limit",
                module.name()
            );
            continue;
        }
        let Some(capability) = read_only(module_bytes(module)) else {
            klog::error!("no memory to pass boot module {} to init", module.name());
            continue;
        };
        let _ = writeln!(table, "{} {}", module.name(), FIXED_HANDLES + modules.len());
        modules.push(capability);
    }

    let mut initial = Vec::with_capacity(FIXED_HANDLES + modules.len());
    initial.push(Capability::new(
        KernelObject::Log,
        default_rights(ObjectKind::Log),
    ));
    initial.push(read_only(table.as_bytes()).expect("memory for the module table"));
    initial.push(Capability::new(
        KernelObject::Console,
        default_rights(ObjectKind::Console),
    ));
    initial.extend(modules);

    match spawn("init", module_bytes(init), initial, u64::from(test_mode)) {
        Ok(process) => {
            klog::info!("init started with {} boot modules", boot.modules().len());
            Some(process)
        }
        Err(err) => {
            klog::error!("cannot start init: {err:?}");
            None
        }
    }
}
