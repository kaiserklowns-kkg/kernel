//! Processes (ADR-0014): an isolated address space, a capability table,
//! and the threads that run in them.
//!
//! - All user memory is mapped from memory objects (ADR-0011), so frames are
//!   owned by objects and freed when the last mapping or capability goes.
//! - The kernel never dereferences user pointers. It walks the process's
//!   page tables to the backing frames and copies through the direct map, so
//!   a bad pointer is an error (`BadAddress`), never a kernel fault, and
//!   SMAP stays enabled.
//! - A process holds only the capabilities it was given at start or receives
//!   over IPC. There is no ambient authority, not even for logging.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use oceans_abi::{Error, start::MAX_INITIAL_HANDLES};
use oceans_elf::{ElfError, Executable, Limits};
use oceans_memory_map::PAGE_SIZE;
use spin::Mutex;

use crate::arch::{self, AddressSpace, TrapFrame};
use crate::memory::layout::USER;
use crate::memory::paging::{self, Cache, MapError, MapFlags, PageSize};
use crate::memory::phys_to_virt;
use crate::object::{CapTable, Capability, DEFAULT_CAP_LIMIT, MemoryObject, ObjectError};
use crate::{klog, sched};

/// Top of the main thread's stack; the stack grows down from here.
pub const USER_STACK_TOP: u64 = 0x0000_7fff_f000_0000;
pub const USER_STACK_SIZE: u64 = 64 * 1024;

/// Where program images may be loaded: above the first 64 KiB (null
/// pointer guard) and well below the stack.
const IMAGE_LIMITS: Limits = Limits {
    lowest: 0x1_0000,
    highest: 0x0000_7000_0000_0000,
    max_total: 256 * 1024 * 1024,
};

/// Exit code of a process killed by CPU exception `vector`.
pub const fn killed_by_exception(vector: u64) -> i64 {
    -128 - vector as i64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnError {
    Elf(ElfError),
    Memory(ObjectError),
    Map(MapError),
    TooManyHandles,
    Thread(sched::SpawnError),
}

struct Mapping {
    #[expect(
        dead_code,
        reason = "kept for unmapping, which arrives with the memory syscalls"
    )]
    start: u64,
    /// Keeps the backing frames alive while mapped.
    _object: Arc<MemoryObject>,
}

struct UserSpace {
    tables: Option<AddressSpace>,
    mappings: Vec<Mapping>,
}

impl UserSpace {
    fn tables(&self) -> &AddressSpace {
        self.tables.as_ref().expect("live until drop")
    }

    /// Maps all of `object` at `start` for user mode.
    fn map(
        &mut self,
        start: u64,
        object: Arc<MemoryObject>,
        writable: bool,
        executable: bool,
    ) -> Result<(), MapError> {
        let flags = MapFlags {
            writable,
            executable,
            user: true,
            global: false,
            cache: Cache::WriteBack,
        };
        let tables = self.tables.as_mut().expect("live until drop");
        for (index, frame) in object.frames().iter().enumerate() {
            let virt = start + index as u64 * PAGE_SIZE;
            tables.map(virt, frame.addr(), PageSize::Size4KiB, flags)?;
        }
        self.mappings.push(Mapping {
            start,
            _object: object,
        });
        Ok(())
    }

    /// Physical address behind user address `virt`, if mapped for user mode
    /// (and writable, if `write`).
    fn translate(&self, virt: u64, write: bool) -> Option<u64> {
        let (phys, flags) = self.tables().translate(virt)?;
        (flags.user && (flags.writable || !write)).then_some(phys)
    }
}

impl Drop for UserSpace {
    fn drop(&mut self) {
        if let Some(tables) = self.tables.take() {
            assert_ne!(
                arch::active_root(),
                tables.root(),
                "destroying the active address space"
            );
            // SAFETY: not active (checked), and nothing uses it after this.
            unsafe { tables.destroy_user() };
        }
        // `mappings` drops next, releasing the memory objects.
    }
}

pub struct Process {
    id: ProcessId,
    name: &'static str,
    root: u64,
    space: Mutex<UserSpace>,
    capabilities: Mutex<CapTable>,
    exited: AtomicBool,
    exit_code: AtomicI64,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Process {
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Page-table root of the process's address space.
    pub fn root(&self) -> u64 {
        self.root
    }

    pub fn capabilities(&self) -> &Mutex<CapTable> {
        &self.capabilities
    }

    /// The exit code, once the process has exited.
    pub fn exit_status(&self) -> Option<i64> {
        self.exited
            .load(Ordering::Acquire)
            .then(|| self.exit_code.load(Ordering::Relaxed))
    }

    fn set_exit(&self, code: i64) {
        self.exit_code.store(code, Ordering::Relaxed);
        self.exited.store(true, Ordering::Release);
    }

    /// Copies `out.len()` bytes from user address `addr`.
    pub fn copy_from_user(&self, addr: u64, out: &mut [u8]) -> Result<(), Error> {
        self.for_each_user_chunk(addr, out.len(), false, |kernel, at, len| {
            // SAFETY: `kernel..+len` is a direct-map view of a frame mapped
            // in this process (checked under the space lock, which is held).
            unsafe { core::ptr::copy_nonoverlapping(kernel, out.as_mut_ptr().add(at), len) };
        })
    }

    /// Copies `data` to user address `addr` (which must be writable).
    pub fn copy_to_user(&self, addr: u64, data: &[u8]) -> Result<(), Error> {
        self.for_each_user_chunk(addr, data.len(), true, |kernel, at, len| {
            // SAFETY: as in `copy_from_user`, for a user-writable frame.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(at), kernel, len) };
        })
    }

    fn for_each_user_chunk(
        &self,
        addr: u64,
        len: usize,
        write: bool,
        mut f: impl FnMut(*mut u8, usize, usize),
    ) -> Result<(), Error> {
        if len == 0 {
            return Ok(());
        }
        let end = addr
            .checked_add(len as u64)
            .filter(|&end| addr >= USER.start && end <= USER.end)
            .ok_or(Error::BadAddress)?;
        arch::without_interrupts(|| {
            let space = self.space.lock();
            // Validate the whole range first, so a failed copy changes nothing.
            let mut page = addr - addr % PAGE_SIZE;
            while page < end {
                space.translate(page, write).ok_or(Error::BadAddress)?;
                page += PAGE_SIZE;
            }
            let mut position = addr;
            while position < end {
                let in_page = position % PAGE_SIZE;
                let chunk = (PAGE_SIZE - in_page).min(end - position);
                let phys = space.translate(position, write).ok_or(Error::BadAddress)?;
                f(
                    phys_to_virt(phys),
                    (position - addr) as usize,
                    chunk as usize,
                );
                position += chunk;
            }
            Ok(())
        })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        klog::debug!("process {} ({}) destroyed", self.id.0, self.name);
    }
}

/// Starts a process from the ELF `image` with `initial` capabilities and an
/// argument word. Its first thread starts with `rdi` = handle count, `rsi` =
/// pointer to the handles (on its stack), `rdx` = `arg` (ABI `start`).
pub fn spawn(
    name: &'static str,
    image: &[u8],
    initial: Vec<Capability>,
    arg: u64,
) -> Result<Arc<Process>, SpawnError> {
    if initial.len() > MAX_INITIAL_HANDLES {
        return Err(SpawnError::TooManyHandles);
    }
    let executable = Executable::parse(image, IMAGE_LIMITS).map_err(SpawnError::Elf)?;
    let tables = paging::new_user_address_space().map_err(SpawnError::Map)?;
    let root = tables.root();
    let mut space = UserSpace {
        tables: Some(tables),
        mappings: Vec::new(),
    };

    for segment in executable.segments() {
        let start = segment.page_start();
        let object = MemoryObject::new(segment.page_end() - start).map_err(SpawnError::Memory)?;
        object
            .write(segment.vaddr - start, segment.data)
            .map_err(SpawnError::Memory)?;
        space
            .map(start, object, segment.writable, segment.executable)
            .map_err(SpawnError::Map)?;
    }

    let mut capabilities = CapTable::new(DEFAULT_CAP_LIMIT);
    let mut handles = Vec::with_capacity(initial.len());
    for capability in initial {
        let handle = capabilities
            .insert(capability)
            .map_err(|_| SpawnError::TooManyHandles)?;
        handles.push(handle.raw());
    }

    // Stack: initial handles at the top, then a zero "return address" slot,
    // so the entry point sees the stack alignment of a normal call.
    let stack = MemoryObject::new(USER_STACK_SIZE).map_err(SpawnError::Memory)?;
    let stack_base = USER_STACK_TOP - USER_STACK_SIZE;
    let handles_addr = (USER_STACK_TOP - 8 * handles.len() as u64) & !15;
    for (index, handle) in handles.iter().enumerate() {
        let offset = handles_addr - stack_base + 8 * index as u64;
        stack
            .write(offset, &handle.to_le_bytes())
            .map_err(SpawnError::Memory)?;
    }
    let user_rsp = handles_addr - 8;
    space
        .map(stack_base, stack, true, false)
        .map_err(SpawnError::Map)?;

    let process = Arc::new(Process {
        id: ProcessId(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
        name,
        root,
        space: Mutex::new(space),
        capabilities: Mutex::new(capabilities),
        exited: AtomicBool::new(false),
        exit_code: AtomicI64::new(0),
    });
    let args = [handles.len() as u64, handles_addr, arg];
    sched::spawn_user(name, process.clone(), executable.entry(), user_rsp, args)
        .map_err(SpawnError::Thread)?;
    klog::debug!(
        "process {} ({name}) started at {:#x}",
        process.id.0,
        executable.entry()
    );
    Ok(process)
}

/// Enables user mode: syscalls and the ring-3 fault handler.
pub fn init() {
    arch::set_user_fault_handler(on_user_fault);
    arch::init_syscalls(crate::syscall::dispatch);
}

/// Ends the calling process with `code`.
///
/// Its capabilities are closed immediately, so peers see the exit at once
/// (e.g. `PeerClosed` on endpoints) even if something still references the
/// process record. The address space goes when the last thread is reaped
/// (it cannot be freed while this thread still runs on it).
pub fn exit_current(code: i64) -> ! {
    let thread = sched::current();
    if let Some(process) = thread.process() {
        process.set_exit(code);
        let capabilities = arch::without_interrupts(|| {
            core::mem::replace(&mut *process.capabilities.lock(), CapTable::new(0))
        });
        // Dropped outside the lock: closing endpoint ends wakes other threads.
        drop(capabilities);
    }
    drop(thread);
    sched::exit()
}

/// A CPU exception in user mode kills the process, never the kernel.
fn on_user_fault(frame: &TrapFrame) -> ! {
    let thread = sched::current();
    let name = thread.process().map_or("?", |p| p.name());
    if frame.vector == 14 {
        klog::warn!(
            "process {name} killed: page fault at {:#x} accessing {:#x} (error {:#x})",
            frame.rip,
            arch::fault_address(),
            frame.error_code
        );
    } else {
        klog::warn!(
            "process {name} killed: {} at {:#x}",
            arch::exception_name(frame.vector),
            frame.rip
        );
    }
    drop(thread);
    exit_current(killed_by_exception(frame.vector))
}

/// Phase 2 exit criterion, for smoke-test boots: isolated user processes
/// exchange IPC messages, bad requests are rejected, and a process touching
/// kernel memory is killed without harming anyone else.
pub fn self_test(boot: &crate::boot::BootInfo) {
    use alloc::vec;

    use crate::ipc::endpoint::Endpoint;
    use crate::object::{KernelObject, ObjectKind, default_rights};
    use crate::time;

    // Roles understood by the `ipc-test` program (user/ipc-test).
    const SERVER: u64 = 1;
    const CLIENT: u64 = 2;
    const INTRUDER: u64 = 3;

    let module = boot
        .module("ipc-test")
        .expect("smoke image ships the ipc-test module");
    // SAFETY: boot modules stay mapped read-only in the direct map for the
    // kernel's lifetime (ADR-0009) and are never written.
    let image = unsafe {
        core::slice::from_raw_parts(
            phys_to_virt(module.physical_base).cast_const(),
            module.size as usize,
        )
    };
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

    let processes = [&server, &client, &intruder];
    let deadline = time::ticks() + 10 * u64::from(time::HZ);
    while processes.iter().any(|p| p.exit_status().is_none()) {
        assert!(time::ticks() < deadline, "user processes did not finish");
        sched::sleep_ms(10);
    }
    assert_eq!(server.exit_status(), Some(0), "server process failed");
    assert_eq!(client.exit_status(), Some(0), "client process failed");
    assert_eq!(
        intruder.exit_status(),
        Some(killed_by_exception(14)),
        "intruder was not killed by its page fault"
    );

    // All three are destroyed once their threads are reaped: address
    // spaces, page tables, memory objects and capability tables freed.
    let alive = [
        Arc::downgrade(&server),
        Arc::downgrade(&client),
        Arc::downgrade(&intruder),
    ];
    drop((server, client, intruder));
    let deadline = time::ticks() + 2 * u64::from(time::HZ);
    while alive.iter().any(|p| p.upgrade().is_some()) {
        assert!(
            time::ticks() < deadline,
            "exited processes were not destroyed"
        );
        sched::sleep_ms(10);
    }
    klog::info!("user process self-test passed: 2 processes exchanged IPC, intruder killed");
}
