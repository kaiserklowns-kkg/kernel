//! Processes (ADR-0014, ADR-0015): an isolated address space, a capability
//! table, and the threads that run in them.
//!
//! - All user memory is mapped from memory objects (ADR-0011), so frames are
//!   owned by objects and freed when the last mapping or capability goes.
//! - The kernel never dereferences user pointers. It walks the process's
//!   page tables to the backing frames and copies through the direct map, so
//!   a bad pointer is an error (`BadAddress`), never a kernel fault, and
//!   SMAP stays enabled.
//! - A process holds only the capabilities it was given at start, received
//!   over IPC or created itself. There is no ambient authority, not even for
//!   logging.
//! - Exit closes the capabilities at once; the address space is freed when
//!   the last thread is reaped. The process record (exit code) lives on while
//!   anyone holds a capability to it.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use oceans_abi::{Error, prot, start::MAX_INITIAL_HANDLES};
use oceans_elf::{ElfError, Executable, Limits};
use oceans_memory_map::PAGE_SIZE;
use spin::Mutex;

use crate::arch::{self, AddressSpace, TrapFrame};
use crate::ipc::Notification;
use crate::klog;
use crate::memory::layout::USER;
use crate::memory::paging::{self, Cache, MapError, MapFlags, PageSize};
use crate::memory::phys_to_virt;
use crate::object::{CapTable, Capability, DEFAULT_CAP_LIMIT, MemoryObject, ObjectError};
use crate::sched::{self, Thread};

pub mod init;
mod self_test;
pub use self_test::{init_self_test, self_test};

/// Top of the main thread's stack; the stack grows down from here.
pub const USER_STACK_TOP: u64 = 0x0000_7fff_f000_0000;
pub const USER_STACK_SIZE: u64 = 64 * 1024;

/// Where program images may be loaded: above the first 64 KiB (null
/// pointer guard), below the mapping region.
const IMAGE_LIMITS: Limits = Limits {
    lowest: 0x1_0000,
    highest: MAP_REGION_START,
    max_total: 256 * 1024 * 1024,
};

/// Where `MEMORY_MAP` places objects when the caller lets the kernel choose.
const MAP_REGION_START: u64 = 0x0000_1000_0000_0000;
const MAP_REGION_END: u64 = 0x0000_7000_0000_0000;

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

impl From<SpawnError> for Error {
    fn from(error: SpawnError) -> Self {
        match error {
            SpawnError::Elf(_) => Error::InvalidImage,
            SpawnError::TooManyHandles => Error::TooLarge,
            SpawnError::Memory(_) | SpawnError::Map(_) | SpawnError::Thread(_) => {
                Error::OutOfMemory
            }
        }
    }
}

struct Mapping {
    start: u64,
    len: u64,
    /// Keeps the backing frames alive while mapped.
    object: Arc<MemoryObject>,
}

impl Mapping {
    fn overlaps(&self, start: u64, len: u64) -> bool {
        start < self.start + self.len && self.start < start + len
    }
}

struct UserSpace {
    tables: Option<AddressSpace>,
    mappings: Vec<Mapping>,
    /// Next candidate address for kernel-chosen mappings.
    next_map: u64,
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
    ) -> Result<(), Error> {
        let len = object.size();
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(Error::InvalidArgument);
        }
        let end = start.checked_add(len).ok_or(Error::BadAddress)?;
        if start < USER.start || end > USER.end {
            return Err(Error::BadAddress);
        }
        if self.mappings.iter().any(|m| m.overlaps(start, len)) {
            return Err(Error::AddressInUse);
        }
        object
            .claim_mapping(writable, executable)
            .map_err(|_| Error::InvalidArgument)?;

        let flags = MapFlags {
            writable,
            executable,
            user: true,
            global: false,
            cache: object.cache(),
        };
        let tables = self.tables.as_mut().expect("live until drop");
        for index in 0..len / PAGE_SIZE {
            let virt = start + index * PAGE_SIZE;
            // Holes (device pages only the kernel may touch) stay unmapped.
            let Some(phys) = object.page(index) else {
                continue;
            };
            if let Err(err) = tables.map(virt, phys, PageSize::Size4KiB, flags) {
                // Roll back the pages mapped so far (holes report NotMapped).
                for undo in (start..virt).step_by(PAGE_SIZE as usize) {
                    let _ = tables.unmap(undo);
                }
                return Err(match err {
                    MapError::OutOfMemory => Error::OutOfMemory,
                    _ => Error::AddressInUse,
                });
            }
        }
        self.mappings.push(Mapping { start, len, object });
        Ok(())
    }

    /// A free, page-aligned range of `len` bytes in the mapping region,
    /// leaving an unmapped guard page after the previous kernel-chosen one.
    fn choose_address(&mut self, len: u64) -> Result<u64, Error> {
        let mut candidate = self.next_map;
        loop {
            let end = candidate.checked_add(len).ok_or(Error::OutOfMemory)?;
            if end > MAP_REGION_END {
                return Err(Error::OutOfMemory);
            }
            match self.mappings.iter().find(|m| m.overlaps(candidate, len)) {
                None => {
                    self.next_map = end + PAGE_SIZE;
                    return Ok(candidate);
                }
                Some(m) => candidate = m.start + m.len + PAGE_SIZE,
            }
        }
    }

    /// Removes the mapping starting at `start`; its object is released.
    fn unmap(&mut self, start: u64) -> Result<Arc<MemoryObject>, Error> {
        let index = self
            .mappings
            .iter()
            .position(|m| m.start == start)
            .ok_or(Error::InvalidArgument)?;
        let mapping = self.mappings.swap_remove(index);
        let tables = self.tables.as_mut().expect("live until drop");
        for (index, page) in (mapping.start..mapping.start + mapping.len)
            .step_by(PAGE_SIZE as usize)
            .enumerate()
        {
            if mapping.object.page(index as u64).is_none() {
                continue; // a hole: never mapped
            }
            // Flushes this CPU's TLB entry; the space is active (we are its
            // only thread). SMP will need a shootdown here.
            tables
                .unmap(page)
                .unwrap_or_else(|err| panic!("user mapping page {page:#x}: {err:?}"));
        }
        Ok(mapping.object)
    }

    /// Physical address behind user address `virt`, if mapped for user mode
    /// (and writable, if `write`) and RAM: the kernel never copies through
    /// device mappings, which are not in the direct map.
    fn translate(&self, virt: u64, write: bool) -> Option<u64> {
        let (phys, flags) = self.tables().translate(virt)?;
        (flags.user && (flags.writable || !write) && flags.cache == Cache::WriteBack)
            .then_some(phys)
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
    /// The process that spawned this one (`None` for kernel-started ones).
    parent: Option<ProcessId>,
    name: String,
    root: u64,
    /// `None` once the last thread has been reaped.
    space: Mutex<Option<UserSpace>>,
    capabilities: Mutex<CapTable>,
    exited: AtomicBool,
    exit_code: AtomicI64,
    /// Threads blocked in `wait_exit`.
    exit_waiters: Mutex<VecDeque<Arc<Thread>>>,
    /// Notifications signalled at exit (`PROCESS_WATCH`); guarded by
    /// `exit_waiters`' lock ordering: always taken after it.
    watchers: Mutex<Vec<(Arc<Notification>, u64)>>,
}

impl fmt::Debug for Process {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Process({} {})", self.id.0, self.name)
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Every process ever started that still exists (records are dropped when
/// their last reference goes; dead entries are pruned on snapshot).
static PROCESSES: Mutex<Vec<alloc::sync::Weak<Process>>> = Mutex::new(Vec::new());

/// A process as reported by `SYSTEM_INFO` (ADR-0020).
pub struct Summary {
    pub id: u64,
    pub parent: u64,
    pub name: String,
    pub exit_code: Option<i64>,
    /// Bytes of user memory mapped.
    pub memory: u64,
}

/// All live process records, in start order.
pub fn snapshot() -> Vec<Summary> {
    let processes: Vec<Arc<Process>> = arch::without_interrupts(|| {
        let mut list = PROCESSES.lock();
        list.retain(|weak| weak.strong_count() > 0);
        list.iter().filter_map(alloc::sync::Weak::upgrade).collect()
    });
    processes
        .iter()
        .map(|p| Summary {
            id: p.id.0,
            parent: p.parent.map_or(0, |id| id.0),
            name: p.name.clone(),
            exit_code: p.exit_status(),
            memory: arch::without_interrupts(|| {
                p.space
                    .lock()
                    .as_ref()
                    .map_or(0, |space| space.mappings.iter().map(|m| m.len).sum())
            }),
        })
        .collect()
}

impl Process {
    pub fn name(&self) -> &str {
        &self.name
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

    /// Blocks until the process exits; returns its exit code.
    pub fn wait_exit(&self) -> i64 {
        arch::without_interrupts(|| {
            loop {
                {
                    let mut waiters = self.exit_waiters.lock();
                    if let Some(code) = self.exit_status() {
                        return code;
                    }
                    waiters.push_back(sched::current());
                }
                sched::block();
            }
        })
    }

    /// Signals `bits` on `notification` when the process exits, or now if
    /// it already has.
    pub fn watch_exit(&self, notification: Arc<Notification>, bits: u64) {
        let now = arch::without_interrupts(|| {
            let _waiters = self.exit_waiters.lock();
            if self.exit_status().is_some() {
                return true;
            }
            self.watchers.lock().push((notification.clone(), bits));
            false
        });
        if now {
            notification.signal(bits);
        }
    }

    fn set_exit(&self, code: i64) {
        let (waiters, watchers) = arch::without_interrupts(|| {
            let mut waiters = self.exit_waiters.lock();
            self.exit_code.store(code, Ordering::Relaxed);
            self.exited.store(true, Ordering::Release);
            (
                core::mem::take(&mut *waiters),
                core::mem::take(&mut *self.watchers.lock()),
            )
        });
        for waiter in waiters {
            sched::wake(waiter);
        }
        for (notification, bits) in watchers {
            notification.signal(bits);
        }
    }

    /// Called when the process's last thread has been reaped (never while
    /// its address space is active): frees the address space.
    pub fn release_address_space(&self) {
        let space = arch::without_interrupts(|| self.space.lock().take());
        drop(space);
    }

    fn with_space<R>(
        &self,
        f: impl FnOnce(&mut UserSpace) -> Result<R, Error>,
    ) -> Result<R, Error> {
        arch::without_interrupts(|| match self.space.lock().as_mut() {
            Some(space) => f(space),
            None => Err(Error::BadAddress),
        })
    }

    /// Maps `object` at `addr` (0: the kernel chooses) with `prot` bits.
    pub fn map_memory(
        &self,
        object: Arc<MemoryObject>,
        addr: u64,
        prot_bits: u64,
    ) -> Result<u64, Error> {
        let writable = prot_bits & prot::WRITE != 0;
        let executable = prot_bits & prot::EXECUTE != 0;
        self.with_space(|space| {
            let start = if addr == 0 {
                space.choose_address(object.size())?
            } else {
                addr
            };
            space.map(start, object, writable, executable)?;
            Ok(start)
        })
    }

    pub fn unmap_memory(&self, addr: u64) -> Result<(), Error> {
        let object = self.with_space(|space| space.unmap(addr))?;
        // Released outside the lock: the last reference frees frames.
        drop(object);
        Ok(())
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
        self.with_space(|space| {
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
    name: &str,
    image: &[u8],
    initial: Vec<Capability>,
    arg: u64,
) -> Result<Arc<Process>, SpawnError> {
    spawn_with_parent(None, name, image, initial, arg)
}

/// Like [`spawn`], recording `parent` as the spawning process.
pub fn spawn_child(
    parent: &Process,
    name: &str,
    image: &[u8],
    initial: Vec<Capability>,
    arg: u64,
) -> Result<Arc<Process>, SpawnError> {
    spawn_with_parent(Some(parent.id), name, image, initial, arg)
}

fn spawn_with_parent(
    parent: Option<ProcessId>,
    name: &str,
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
        next_map: MAP_REGION_START,
    };
    let map_error = |_| SpawnError::Map(MapError::OutOfMemory);

    for segment in executable.segments() {
        let start = segment.page_start();
        let object = MemoryObject::new(segment.page_end() - start).map_err(SpawnError::Memory)?;
        object
            .write(segment.vaddr - start, segment.data)
            .map_err(SpawnError::Memory)?;
        space
            .map(start, object, segment.writable, segment.executable)
            .map_err(map_error)?;
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
        .map_err(map_error)?;

    let process = Arc::new(Process {
        id: ProcessId(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
        parent,
        name: String::from(name),
        root,
        space: Mutex::new(Some(space)),
        capabilities: Mutex::new(capabilities),
        exited: AtomicBool::new(false),
        exit_code: AtomicI64::new(0),
        exit_waiters: Mutex::new(VecDeque::new()),
        watchers: Mutex::new(Vec::new()),
    });
    let args = [handles.len() as u64, handles_addr, arg];
    sched::spawn_user(
        "user-main",
        process.clone(),
        executable.entry(),
        user_rsp,
        args,
    )
    .map_err(SpawnError::Thread)?;
    arch::without_interrupts(|| PROCESSES.lock().push(Arc::downgrade(&process)));
    klog::debug!(
        "process {} ({name}) started at {:#x}",
        process.id.0,
        executable.entry()
    );
    Ok(process)
}

/// Checks that `image` is a loadable executable, without loading it.
pub fn validate_image(image: &[u8]) -> Result<(), Error> {
    Executable::parse(image, IMAGE_LIMITS)
        .map(drop)
        .map_err(|_| Error::InvalidImage)
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
        let capabilities = arch::without_interrupts(|| {
            core::mem::replace(&mut *process.capabilities.lock(), CapTable::new(0))
        });
        // Dropped outside the lock: closing endpoint ends wakes other threads.
        drop(capabilities);
        process.set_exit(code);
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
