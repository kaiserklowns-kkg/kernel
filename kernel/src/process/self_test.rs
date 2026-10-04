//! User-process self-test, for smoke-test boots (`oceans.test=smoke`).
//!
//! Runs the `ipc-test` program of the boot archive in four processes:
//! - **server** + **client** (ABI 1): the Phase 2 criterion, 1000 verified
//!   IPC round trips between isolated processes;
//! - **intruder**: reads kernel memory and must be killed;
//! - **parent** (ABI 2): memory objects, mapping rules, W^X across mappings,
//!   endpoint creation, capability transfer in both directions, spawning a
//!   child process from an image it holds, and waiting for its exit.

use alloc::sync::Arc;
use alloc::vec;

use super::{Process, killed_by_exception, spawn};
use crate::boot::BootInfo;
use crate::ipc::endpoint::Endpoint;
use crate::object::{Capability, KernelObject, MemoryObject, ObjectKind, default_rights};
use crate::{klog, sched, time};

// Roles understood by the `ipc-test` program (user/ipc-test).
const SERVER: u64 = 1;
const CLIENT: u64 = 2;
const INTRUDER: u64 = 3;
const PARENT: u64 = 4;

pub fn self_test(boot: &BootInfo) {
    let image = super::init::archive(boot)
        .and_then(|archive| archive.find("ipc-test"))
        .expect("the smoke image's boot archive has ipc-test");
    let log = || Capability::new(KernelObject::Log, default_rights(ObjectKind::Log));

    let (server_end, client_end) = Endpoint::create();
    let server = spawn(
        "ipc-server",
        image,
        vec![
            log(),
            Capability::new(
                KernelObject::EndpointServer(server_end),
                default_rights(ObjectKind::EndpointServer),
            ),
        ],
        SERVER,
    )
    .expect("spawn server process");
    let client = spawn(
        "ipc-client",
        image,
        vec![
            log(),
            Capability::new(
                KernelObject::EndpointClient(client_end),
                default_rights(ObjectKind::EndpointClient),
            ),
        ],
        CLIENT,
    )
    .expect("spawn client process");
    let intruder = spawn("intruder", image, vec![log()], INTRUDER).expect("spawn intruder");

    // The parent gets its own program image as a read-only memory object,
    // from which it spawns a child.
    let image_object = MemoryObject::new(image.len() as u64).expect("image memory object");
    image_object.write(0, image).expect("copy image");
    let parent = spawn(
        "parent",
        image,
        vec![
            log(),
            Capability::new(
                KernelObject::Memory(image_object),
                oceans_capability::Rights::READ,
            ),
        ],
        PARENT,
    )
    .expect("spawn parent");

    let processes = [&server, &client, &intruder, &parent];
    wait_all(&processes, 20);
    assert_eq!(server.exit_status(), Some(0), "server process failed");
    assert_eq!(client.exit_status(), Some(0), "client process failed");
    assert_eq!(
        intruder.exit_status(),
        Some(killed_by_exception(14)),
        "intruder was not killed by its page fault"
    );
    assert_eq!(parent.exit_status(), Some(0), "parent process failed");

    // Everything is destroyed once threads are reaped and handles closed:
    // address spaces, page tables, memory objects, capability tables (the
    // parent's child included: its handle closed when the parent exited).
    let alive = processes.map(Arc::downgrade);
    drop((server, client, intruder, parent));
    let deadline = time::ticks() + 2 * u64::from(time::HZ);
    while alive.iter().any(|p| p.upgrade().is_some()) {
        assert!(
            time::ticks() < deadline,
            "exited processes were not destroyed"
        );
        sched::sleep_ms(10);
    }
    klog::info!(
        "user process self-test passed: IPC between processes, intruder killed, \
         parent spawned, waited for and killed children"
    );
}

fn wait_all(processes: &[&Arc<Process>], seconds: u64) {
    let deadline = time::ticks() + seconds * u64::from(time::HZ);
    while processes.iter().any(|p| p.exit_status().is_none()) {
        assert!(time::ticks() < deadline, "user processes did not finish");
        sched::sleep_ms(10);
    }
}

/// init with the test manifest: it starts the services, checks every
/// expectation in the manifest (exit codes, restart counts) and exits 0 only
/// if all hold.
pub fn init_self_test(boot: &BootInfo) {
    let init = super::init::start(boot, true).expect("smoke image ships init");
    // The whole scripted shell session runs inside this init; the host
    // side (xtask) allows it 180 s as well.
    wait_all(&[&init], 180);
    assert_eq!(init.exit_status(), Some(0), "init's service test failed");
    klog::info!("init self-test passed: services started, supervised and restarted per manifest");
}
