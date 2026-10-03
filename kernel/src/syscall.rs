//! System call dispatch, ABI version 9 (`oceans-abi`, ADR-0014 to ADR-0026).
//!
//! Every argument is untrusted: handles are looked up with the required
//! rights in the caller's own capability table, and buffers are copied
//! through the process's page tables (`Process::copy_*_user`). Locks are
//! never held across blocking IPC, and, because syscalls run with interrupts
//! enabled, every spinlock is taken with interrupts disabled (a preempted
//! lock holder would otherwise deadlock the CPU).

use alloc::sync::Arc;
use alloc::vec::Vec;

use oceans_abi::{
    CONSOLE_IO_MAX, DEBUG_WRITE_MAX, EVENT_CALL, EVENT_CLOSED, EVENT_NOTIFICATION, Error,
    IPC_MAX_HANDLES, IPC_MAX_INLINE, MessageDesc, PROCESS_NAME_MAX, SPAWN_MAX_IMAGE, nr, prot,
    start::MAX_INITIAL_HANDLES,
};
use oceans_capability::{CapError, Handle, Rights};

use crate::arch::{self, SyscallFrame};
use crate::ipc::endpoint::{Endpoint, Event};
use crate::ipc::{IpcError, Message, Notification};
use crate::object::{
    self, Capability, KernelObject, MemoryObject, ObjectError, ObjectKind, default_rights,
};
use crate::process::{self, Process};
use crate::{console, klog, sched};

type SyscallResult = Result<(u64, u64), Error>;

pub fn dispatch(frame: &mut SyscallFrame) {
    // Syscalls are preemptible: the timer may switch threads while a thread
    // is in the kernel (each has its own kernel stack).
    arch::enable_interrupts();
    let thread = sched::current();
    let Some(process) = thread.process().cloned() else {
        panic!("syscall from a kernel thread");
    };
    drop(thread);

    let [a0, a1, a2, a3, a4, a5] = [
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    ];
    let result = match frame.rax {
        nr::ABI_VERSION => Ok((oceans_abi::ABI_VERSION, 0)),
        nr::DEBUG_WRITE => debug_write(&process, a0, a1, a2),
        nr::EXIT => {
            drop(process);
            process::exit_current(a0 as i64)
        }
        nr::YIELD => {
            sched::yield_now();
            Ok((0, 0))
        }
        nr::HANDLE_CLOSE => close(&process, a0),
        nr::IPC_CALL => ipc_call(&process, a0, a1, a2, a3, a4, a5),
        nr::IPC_RECEIVE => ipc_receive(&process, a0, a1, a2),
        nr::IPC_REPLY => ipc_reply(&process, a0, a1, a2),
        nr::HANDLE_DUPLICATE => duplicate(&process, a0, a1),
        nr::ENDPOINT_CREATE => endpoint_create(&process),
        nr::IPC_CALL_MSG => ipc_call_msg(&process, a0, a1, a2),
        nr::IPC_RECEIVE_MSG => ipc_receive_msg(&process, a0, a1),
        nr::IPC_REPLY_MSG => ipc_reply_msg(&process, a0),
        nr::MEMORY_CREATE => memory_create(&process, a0),
        nr::MEMORY_MAP => memory_map(&process, a0, a1, a2),
        nr::MEMORY_UNMAP => process.unmap_memory(a0).map(|()| (0, 0)),
        nr::PROCESS_SPAWN => process_spawn(&process, a0, a1, a2, a3, a4, a5),
        nr::PROCESS_WAIT => process_wait(&process, a0),
        nr::NOTIFICATION_CREATE => notification_create(&process),
        nr::NOTIFICATION_SIGNAL => notification_signal(&process, a0, a1),
        nr::NOTIFICATION_WAIT => notification_wait(&process, a0),
        nr::PROCESS_WATCH => process_watch(&process, a0, a1, a2),
        nr::SLEEP => sleep(a0),
        nr::CONSOLE_READ => console_read(&process, a0, a1, a2),
        nr::CONSOLE_WRITE => console_write(&process, a0, a1, a2),
        nr::ENDPOINT_MINT => endpoint_mint(&process, a0, a1),
        nr::MEMORY_SIZE => memory_size(&process, a0),
        nr::SYSTEM_INFO => system_info(&process, a0, a1, a2, a3),
        nr::DEVICE_LIST => device_list(&process, a0, a1, a2),
        nr::DEVICE_OPEN => device_open(&process, a0, a1, a2),
        nr::DEVICE_CONFIG_READ => device_config_read(&process, a0, a1, a2),
        nr::DEVICE_ENABLE => device_enable(&process, a0),
        nr::DEVICE_BAR => device_bar(&process, a0, a1),
        nr::DEVICE_DMA_CREATE => device_dma_create(&process, a0, a1),
        nr::DEVICE_IRQ => device_irq(&process, a0, a1, a2, a3),
        nr::ENDPOINT_BIND => endpoint_bind(&process, a0, a1),
        nr::TIMER_SET => timer_set(&process, a0, a1, a2),
        nr::RANDOM => random(&process, a0, a1),
        nr::CLOCK => Ok((crate::time::uptime_ms(), 0)),
        nr::TIME => crate::time::unix_ms()
            .map(|ms| (ms, 0))
            .ok_or(Error::NotFound),
        _ => Err(Error::UnknownSyscall),
    };
    match result {
        Ok((primary, secondary)) => {
            frame.rax = primary;
            frame.rdx = secondary;
        }
        Err(error) => {
            frame.rax = error.code() as u64;
            frame.rdx = 0;
        }
    }
    // Callee-saved user registers are preserved by Rust; scrub the
    // caller-saved argument registers that are not results so no kernel
    // values can leak through them.
    frame.rdi = 0;
    frame.rsi = 0;
    frame.r10 = 0;
    frame.r8 = 0;
    frame.r9 = 0;
}

fn handle(raw: u64) -> Handle {
    Handle::from_raw(raw)
}

fn len_arg(len: u64, max: usize) -> Result<usize, Error> {
    usize::try_from(len)
        .ok()
        .filter(|&len| len <= max)
        .ok_or(Error::TooLarge)
}

fn debug_write(process: &Process, log: u64, ptr: u64, len: u64) -> SyscallResult {
    let len = len_arg(len, DEBUG_WRITE_MAX)?;
    arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        match table
            .get(handle(log), Rights::WRITE)
            .map_err(cap_error)?
            .object()
        {
            KernelObject::Log => Ok(()),
            _ => Err(Error::WrongType),
        }
    })?;
    let mut buffer = [0u8; DEBUG_WRITE_MAX];
    process.copy_from_user(ptr, &mut buffer[..len])?;
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("<invalid UTF-8>");
    klog::info!("[{}] {}", process.name(), text.trim_end_matches('\n'));
    Ok((0, 0))
}

fn close(process: &Process, raw: u64) -> SyscallResult {
    // The capability is dropped after the lock is released: closing an
    // endpoint end may wake other threads.
    let closed = arch::without_interrupts(|| process.capabilities().lock().remove(handle(raw)));
    drop(closed.map_err(cap_error)?);
    Ok((0, 0))
}

fn ipc_call(
    process: &Process,
    client: u64,
    label: u64,
    ptr: u64,
    len: u64,
    reply_ptr: u64,
    reply_capacity: u64,
) -> SyscallResult {
    let len = len_arg(len, IPC_MAX_INLINE)?;
    let reply_capacity = usize::try_from(reply_capacity).unwrap_or(usize::MAX);
    let end = arch::without_interrupts(|| {
        object::client_end(
            &mut process.capabilities().lock(),
            handle(client),
            Rights::SEND,
        )
    })
    .map_err(object_error)?;

    let mut data = [0u8; IPC_MAX_INLINE];
    process.copy_from_user(ptr, &mut data[..len])?;
    let request = Message::new(label, &data[..len]).map_err(ipc_error)?;
    let reply = end.call(request).map_err(ipc_error)?;
    drop(end);

    if reply.data().len() > reply_capacity {
        return Err(Error::TooLarge);
    }
    process.copy_to_user(reply_ptr, reply.data())?;
    Ok((reply.data().len() as u64, reply.label))
}

fn ipc_receive(process: &Process, server: u64, ptr: u64, capacity: u64) -> SyscallResult {
    let capacity = usize::try_from(capacity).unwrap_or(usize::MAX);
    let end: Arc<_> = arch::without_interrupts(|| {
        object::server_end(
            &mut process.capabilities().lock(),
            handle(server),
            Rights::RECEIVE,
        )
    })
    .map_err(object_error)?;
    let (request, token) = end.receive().map_err(ipc_error)?;
    drop(end);

    // An earlier call still pending on this thread is answered with
    // NoReply (dropping its token) when the new one replaces it.
    let thread = sched::current();
    let previous = arch::without_interrupts(|| thread.pending_call().lock().replace(token));
    drop(previous);

    if request.data().len() > capacity {
        return Err(Error::TooLarge);
    }
    process.copy_to_user(ptr, request.data())?;
    Ok((request.data().len() as u64, request.label))
}

fn ipc_reply(process: &Process, label: u64, ptr: u64, len: u64) -> SyscallResult {
    let len = len_arg(len, IPC_MAX_INLINE)?;
    let mut data = [0u8; IPC_MAX_INLINE];
    process.copy_from_user(ptr, &mut data[..len])?;
    let reply = Message::new(label, &data[..len]).map_err(ipc_error)?;
    let thread = sched::current();
    let token = arch::without_interrupts(|| thread.pending_call().lock().take())
        .ok_or(Error::NoPendingCall)?;
    token.reply(reply);
    Ok((0, 0))
}

fn cap_error(error: CapError) -> Error {
    match error {
        CapError::InvalidHandle(_) => Error::InvalidHandle,
        CapError::Revoked(_) => Error::Revoked,
        CapError::MissingRights { .. } | CapError::RightsEscalation { .. } => Error::MissingRights,
        CapError::TableFull => Error::OutOfMemory,
    }
}

fn object_error(error: ObjectError) -> Error {
    match error {
        ObjectError::Capability(error) => cap_error(error),
        ObjectError::WrongType { .. } => Error::WrongType,
        ObjectError::OutOfMemory => Error::OutOfMemory,
        ObjectError::OutOfBounds => Error::BadAddress,
        ObjectError::WriteExecute | ObjectError::NotRam => Error::InvalidArgument,
    }
}

fn ipc_error(error: IpcError) -> Error {
    match error {
        IpcError::PeerClosed => Error::PeerClosed,
        IpcError::NoReply => Error::NoReply,
        IpcError::MessageTooLarge | IpcError::TooManyCapabilities => Error::TooLarge,
        IpcError::OutOfMemory => Error::OutOfMemory,
        IpcError::Busy => Error::Busy,
    }
}

// ---- ABI 2 -----------------------------------------------------------------

/// Inserts `capability` into the caller's table.
fn insert(process: &Process, capability: Capability) -> Result<u64, Error> {
    arch::without_interrupts(|| process.capabilities().lock().insert(capability))
        .map(Handle::raw)
        .map_err(cap_error)
}

fn duplicate(process: &Process, raw: u64, rights: u64) -> SyscallResult {
    let rights = u32::try_from(rights)
        .ok()
        .and_then(Rights::from_bits)
        .ok_or(Error::InvalidArgument)?;
    let derived =
        arch::without_interrupts(|| process.capabilities().lock().derive(handle(raw), rights));
    Ok((derived.map_err(cap_error)?.raw(), 0))
}

fn endpoint_create(process: &Process) -> SyscallResult {
    let (server, client) = Endpoint::create();
    let server = insert(
        process,
        Capability::new(
            KernelObject::EndpointServer(server),
            default_rights(ObjectKind::EndpointServer),
        ),
    )?;
    let client = insert(
        process,
        Capability::new(
            KernelObject::EndpointClient(client),
            default_rights(ObjectKind::EndpointClient),
        ),
    );
    match client {
        Ok(client) => Ok((server, client)),
        Err(error) => {
            // Do not leave half an endpoint behind.
            let _ = close(process, server);
            Err(error)
        }
    }
}

const DESC_WORDS: usize = size_of::<MessageDesc>() / 8;

fn read_desc(process: &Process, ptr: u64) -> Result<MessageDesc, Error> {
    let mut bytes = [0u8; size_of::<MessageDesc>()];
    process.copy_from_user(ptr, &mut bytes)?;
    let mut words = [0u64; DESC_WORDS];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let [label, data, data_len, handles, handles_len] = words;
    Ok(MessageDesc {
        label,
        data,
        data_len,
        handles,
        handles_len,
    })
}

fn write_desc(process: &Process, ptr: u64, desc: &MessageDesc) -> Result<(), Error> {
    let words = [
        desc.label,
        desc.data,
        desc.data_len,
        desc.handles,
        desc.handles_len,
    ];
    let mut bytes = [0u8; size_of::<MessageDesc>()];
    for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    process.copy_to_user(ptr, &bytes)
}

/// Reads `count` (at most `max`) distinct handles from user memory.
fn read_handles(
    process: &Process,
    ptr: u64,
    count: usize,
    max: usize,
) -> Result<Vec<Handle>, Error> {
    if count > max || count > MAX_INITIAL_HANDLES {
        return Err(Error::TooLarge);
    }
    let mut bytes = [0u8; 8 * MAX_INITIAL_HANDLES];
    process.copy_from_user(ptr, &mut bytes[..8 * count])?;
    let handles: Vec<Handle> = bytes[..8 * count]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| Handle::from_raw(u64::from_le_bytes(*chunk)))
        .collect();
    // A handle listed twice would be moved twice.
    for (i, a) in handles.iter().enumerate() {
        if handles[..i].contains(a) {
            return Err(Error::InvalidArgument);
        }
    }
    Ok(handles)
}

/// Removes `handles` from the caller's table for transfer: all or nothing,
/// each needing `TRANSFER`.
fn take_for_transfer(process: &Process, handles: &[Handle]) -> Result<Vec<Capability>, Error> {
    arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        for &h in handles {
            table.get(h, Rights::TRANSFER).map_err(cap_error)?;
        }
        Ok(handles
            .iter()
            .map(|&h| table.remove(h).expect("validated above"))
            .collect())
    })
}

/// Builds a message from a user `MessageDesc`, moving its handles out of
/// the caller's table as the last step (nothing can fail afterwards).
fn take_message(process: &Process, desc: &MessageDesc) -> Result<Message, Error> {
    let len = len_arg(desc.data_len, IPC_MAX_INLINE)?;
    let count = len_arg(desc.handles_len, IPC_MAX_HANDLES)?;
    let mut data = [0u8; IPC_MAX_INLINE];
    process.copy_from_user(desc.data, &mut data[..len])?;
    let handles = read_handles(process, desc.handles, count, IPC_MAX_HANDLES)?;
    let mut message = Message::new(desc.label, &data[..len]).map_err(ipc_error)?;
    for capability in take_for_transfer(process, &handles)? {
        message.attach(capability).map_err(ipc_error)?;
    }
    Ok(message)
}

/// Delivers `message` into the buffers described by the user `MessageDesc`
/// at `desc_ptr`, inserting its capabilities into the caller's table. On
/// failure the message's capabilities are closed, never leaked.
fn deliver_message(process: &Process, mut message: Message, desc_ptr: u64) -> Result<(), Error> {
    let mut desc = read_desc(process, desc_ptr)?;
    let capabilities = message.take_capabilities();
    let capacity = usize::try_from(desc.data_len).unwrap_or(usize::MAX);
    let handle_capacity = usize::try_from(desc.handles_len).unwrap_or(usize::MAX);
    if message.data().len() > capacity || capabilities.len() > handle_capacity {
        return Err(Error::TooLarge);
    }
    process.copy_to_user(desc.data, message.data())?;

    let mut inserted = Vec::new();
    for capability in capabilities {
        match insert(process, capability) {
            Ok(raw) => inserted.push(raw),
            Err(error) => {
                undo_inserts(process, &inserted);
                return Err(error);
            }
        }
    }
    let mut raw = [0u8; 8 * IPC_MAX_HANDLES];
    for (chunk, h) in raw.as_chunks_mut::<8>().0.iter_mut().zip(&inserted) {
        chunk.copy_from_slice(&h.to_le_bytes());
    }
    let written = process
        .copy_to_user(desc.handles, &raw[..8 * inserted.len()])
        .and_then(|()| {
            desc.label = message.label;
            desc.data_len = message.data().len() as u64;
            desc.handles_len = inserted.len() as u64;
            write_desc(process, desc_ptr, &desc)
        });
    if written.is_err() {
        undo_inserts(process, &inserted);
    }
    written
}

fn undo_inserts(process: &Process, handles: &[u64]) {
    for &h in handles {
        let _ = close(process, h);
    }
}

fn ipc_call_msg(process: &Process, client: u64, request_ptr: u64, reply_ptr: u64) -> SyscallResult {
    let end = arch::without_interrupts(|| {
        object::client_end(
            &mut process.capabilities().lock(),
            handle(client),
            Rights::SEND,
        )
    })
    .map_err(object_error)?;
    // Check the reply descriptor is readable before handles leave the table.
    read_desc(process, reply_ptr)?;
    let request = take_message(process, &read_desc(process, request_ptr)?)?;
    let reply = end.call(request).map_err(ipc_error)?;
    drop(end);
    deliver_message(process, reply, reply_ptr).map(|()| (0, 0))
}

fn ipc_receive_msg(process: &Process, server: u64, desc_ptr: u64) -> SyscallResult {
    read_desc(process, desc_ptr)?;
    let end = arch::without_interrupts(|| {
        object::server_end(
            &mut process.capabilities().lock(),
            handle(server),
            Rights::RECEIVE,
        )
    })
    .map_err(object_error)?;
    let event = end.receive_event(true).map_err(ipc_error)?;
    drop(end);
    let (request, badge, token) = match event {
        Event::Call {
            request,
            badge,
            token,
        } => (request, badge, token),
        Event::Closed { badge } => return Ok((EVENT_CLOSED, badge)),
        Event::Notification { bits } => return Ok((EVENT_NOTIFICATION, bits)),
    };
    let thread = sched::current();
    let previous = arch::without_interrupts(|| thread.pending_call().lock().replace(token));
    drop(previous);
    deliver_message(process, request, desc_ptr).map(|()| (EVENT_CALL, badge))
}

fn ipc_reply_msg(process: &Process, desc_ptr: u64) -> SyscallResult {
    let thread = sched::current();
    let has_pending = arch::without_interrupts(|| thread.pending_call().lock().is_some());
    if !has_pending {
        return Err(Error::NoPendingCall);
    }
    let reply = take_message(process, &read_desc(process, desc_ptr)?)?;
    let token = arch::without_interrupts(|| thread.pending_call().lock().take())
        .ok_or(Error::NoPendingCall)?;
    token.reply(reply);
    Ok((0, 0))
}

fn memory_create(process: &Process, size: u64) -> SyscallResult {
    let object = MemoryObject::new(size).map_err(|error| match error {
        ObjectError::OutOfBounds => Error::InvalidArgument,
        other => object_error(other),
    })?;
    let raw = insert(
        process,
        Capability::new(
            KernelObject::Memory(object),
            default_rights(ObjectKind::Memory),
        ),
    )?;
    Ok((raw, 0))
}

fn memory_map(process: &Process, raw: u64, addr: u64, prot_bits: u64) -> SyscallResult {
    let known = prot::READ | prot::WRITE | prot::EXECUTE;
    let writable = prot_bits & prot::WRITE != 0;
    let executable = prot_bits & prot::EXECUTE != 0;
    if prot_bits & !known != 0 || (writable && executable) {
        return Err(Error::InvalidArgument);
    }
    let mut required = Rights::MAP | Rights::READ;
    if writable {
        required = required | Rights::WRITE;
    }
    if executable {
        required = required | Rights::EXECUTE;
    }
    let object = arch::without_interrupts(|| {
        object::memory(&mut process.capabilities().lock(), handle(raw), required)
    })
    .map_err(object_error)?;
    Ok((process.map_memory(object, addr, prot_bits)?, 0))
}

fn process_spawn(
    process: &Process,
    image: u64,
    image_len: u64,
    handles_ptr: u64,
    handles_len: u64,
    arg: u64,
    name_ptr: u64,
) -> SyscallResult {
    let mut name_bytes = [0u8; PROCESS_NAME_MAX];
    if name_ptr != 0 {
        process.copy_from_user(name_ptr, &mut name_bytes)?;
    }
    let name_len = name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(PROCESS_NAME_MAX);
    let child_name =
        core::str::from_utf8(&name_bytes[..name_len]).map_err(|_| Error::InvalidArgument)?;
    let child_name = if child_name.is_empty() {
        "child"
    } else {
        child_name
    };
    let object = arch::without_interrupts(|| {
        object::memory(
            &mut process.capabilities().lock(),
            handle(image),
            Rights::READ,
        )
    })
    .map_err(object_error)?;
    // 0 means "the whole object".
    let len = if image_len == 0 {
        object.size()
    } else {
        image_len
    };
    if len > object.size() || len > SPAWN_MAX_IMAGE as u64 {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len as usize)
        .map_err(|_| Error::OutOfMemory)?;
    bytes.resize(len as usize, 0);
    object.read(0, &mut bytes).map_err(object_error)?;
    drop(object);
    // Reject a bad image before any handle leaves the caller's table.
    process::validate_image(&bytes)?;

    let count = len_arg(handles_len, MAX_INITIAL_HANDLES)?;
    let handles = read_handles(process, handles_ptr, count, MAX_INITIAL_HANDLES)?;
    let initial = take_for_transfer(process, &handles)?;
    let name = alloc::format!("{}/{child_name}", process.name());
    let child = process::spawn_child(process, &name, &bytes, initial, arg)?;
    let raw = insert(
        process,
        Capability::new(
            KernelObject::Process(child),
            default_rights(ObjectKind::Process),
        ),
    )?;
    Ok((raw, 0))
}

fn process_wait(process: &Process, raw: u64) -> SyscallResult {
    let child = arch::without_interrupts(|| {
        object::process(
            &mut process.capabilities().lock(),
            handle(raw),
            Rights::WAIT,
        )
    })
    .map_err(object_error)?;
    let code = child.wait_exit();
    Ok((0, code as u64))
}

// ---- ABI 3 -----------------------------------------------------------------

fn notification_create(process: &Process) -> SyscallResult {
    let raw = insert(
        process,
        Capability::new(
            KernelObject::Notification(Notification::new()),
            default_rights(ObjectKind::Notification),
        ),
    )?;
    Ok((raw, 0))
}

fn notification_signal(process: &Process, raw: u64, bits: u64) -> SyscallResult {
    let notification = arch::without_interrupts(|| {
        object::notification(
            &mut process.capabilities().lock(),
            handle(raw),
            Rights::SIGNAL,
        )
    })
    .map_err(object_error)?;
    notification.signal(bits);
    Ok((0, 0))
}

fn notification_wait(process: &Process, raw: u64) -> SyscallResult {
    let notification = arch::without_interrupts(|| {
        object::notification(
            &mut process.capabilities().lock(),
            handle(raw),
            Rights::WAIT,
        )
    })
    .map_err(object_error)?;
    Ok((notification.wait(), 0))
}

fn process_watch(process: &Process, raw: u64, notification: u64, bits: u64) -> SyscallResult {
    if bits == 0 {
        return Err(Error::InvalidArgument);
    }
    let (child, notification) = arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        let child = object::process(&mut table, handle(raw), Rights::WAIT)?;
        let notification = object::notification(&mut table, handle(notification), Rights::SIGNAL)?;
        Ok((child, notification))
    })
    .map_err(object_error)?;
    child.watch_exit(notification, bits);
    Ok((0, 0))
}

fn sleep(ms: u64) -> SyscallResult {
    sched::sleep_ms(ms);
    Ok((0, 0))
}

// ---- ABI 4 -----------------------------------------------------------------

/// Bytes written to the console per lock hold, bounding how long output
/// keeps interrupts disabled.
const CONSOLE_WRITE_CHUNK: usize = 64;

fn check_console(process: &Process, raw: u64, required: Rights) -> Result<(), Error> {
    arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        let capability = table.get(handle(raw), Rights::NONE).map_err(cap_error)?;
        match capability.object() {
            KernelObject::Console => capability.check(required).map_err(cap_error),
            _ => Err(Error::WrongType),
        }
    })
}

fn console_read(process: &Process, raw: u64, ptr: u64, capacity: u64) -> SyscallResult {
    check_console(process, raw, Rights::READ)?;
    let capacity = len_arg(capacity, CONSOLE_IO_MAX)?;
    if capacity == 0 {
        return Ok((0, 0));
    }
    let mut buffer = [0u8; CONSOLE_IO_MAX];
    // Validate the destination before consuming input, so a bad pointer
    // does not lose keystrokes.
    process.copy_to_user(ptr, &buffer[..capacity])?;
    let count = console::read(&mut buffer[..capacity]);
    process.copy_to_user(ptr, &buffer[..count])?;
    Ok((count as u64, 0))
}

fn console_write(process: &Process, raw: u64, ptr: u64, len: u64) -> SyscallResult {
    check_console(process, raw, Rights::WRITE)?;
    let len = len_arg(len, CONSOLE_IO_MAX)?;
    let mut buffer = [0u8; CONSOLE_IO_MAX];
    process.copy_from_user(ptr, &mut buffer[..len])?;
    for chunk in buffer[..len].chunks(CONSOLE_WRITE_CHUNK) {
        console::write(chunk);
    }
    Ok((len as u64, 0))
}

// ---- ABI 5 -----------------------------------------------------------------

fn endpoint_mint(process: &Process, server: u64, badge: u64) -> SyscallResult {
    if badge == 0 {
        return Err(Error::InvalidArgument);
    }
    let end = arch::without_interrupts(|| {
        object::server_end(
            &mut process.capabilities().lock(),
            handle(server),
            Rights::MANAGE,
        )
    })
    .map_err(object_error)?;
    let client = end.mint(badge);
    drop(end);
    let raw = insert(
        process,
        Capability::new(
            KernelObject::EndpointClient(client),
            default_rights(ObjectKind::EndpointClient),
        ),
    )?;
    Ok((raw, 0))
}

fn memory_size(process: &Process, raw: u64) -> SyscallResult {
    let memory = arch::without_interrupts(|| {
        object::memory(
            &mut process.capabilities().lock(),
            handle(raw),
            Rights::NONE,
        )
    })
    .map_err(object_error)?;
    Ok((memory.size(), 0))
}

// ---- ABI 6 -----------------------------------------------------------------

fn system_info(process: &Process, raw: u64, kind: u64, ptr: u64, capacity: u64) -> SyscallResult {
    use oceans_abi::sysinfo::{self, KernelInfo, MemoryInfo, ProcessRecord, UptimeInfo};

    arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        let capability = table.get(handle(raw), Rights::NONE).map_err(cap_error)?;
        match capability.object() {
            KernelObject::SystemInfo => capability.check(Rights::READ).map_err(cap_error),
            _ => Err(Error::WrongType),
        }
    })?;

    let mut bytes = Vec::new();
    match kind {
        sysinfo::KERNEL => {
            let mut record = [0u8; KernelInfo::SIZE];
            KernelInfo::new(
                oceans_abi::ABI_VERSION,
                env!("CARGO_PKG_VERSION"),
                arch::NAME,
            )
            .encode(&mut record);
            bytes.extend_from_slice(&record);
        }
        sysinfo::MEMORY => {
            let frames = crate::memory::frames::stats();
            let heap = crate::memory::heap::stats();
            let mut record = [0u8; MemoryInfo::SIZE];
            MemoryInfo {
                page_size: 4096,
                total_frames: frames.managed_frames,
                free_frames: frames.free_frames,
                kernel_heap: (heap.small_in_use + heap.large_in_use) as u64,
            }
            .encode(&mut record);
            bytes.extend_from_slice(&record);
        }
        sysinfo::UPTIME => {
            let mut record = [0u8; UptimeInfo::SIZE];
            UptimeInfo {
                ticks: crate::time::ticks(),
                hz: u64::from(crate::time::HZ),
            }
            .encode(&mut record);
            bytes.extend_from_slice(&record);
        }
        sysinfo::PROCESSES => {
            for summary in process::snapshot() {
                let mut record = ProcessRecord {
                    id: summary.id,
                    parent: summary.parent,
                    exit_code: summary.exit_code.unwrap_or(ProcessRecord::RUNNING),
                    memory: summary.memory,
                    name: [0; 32],
                };
                record.set_name(&summary.name);
                let mut encoded = [0u8; ProcessRecord::SIZE];
                record.encode(&mut encoded);
                bytes.extend_from_slice(&encoded);
            }
        }
        _ => return Err(Error::InvalidArgument),
    }
    if bytes.len() as u64 > capacity {
        return Err(Error::TooLarge);
    }
    process.copy_to_user(ptr, &bytes)?;
    Ok((bytes.len() as u64, 0))
}

// ---- ABI 7 -----------------------------------------------------------------

/// Checks that `raw` names the device bus with `required` rights.
fn check_bus(process: &Process, raw: u64, required: Rights) -> Result<(), Error> {
    arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        let capability = table.get(handle(raw), Rights::NONE).map_err(cap_error)?;
        match capability.object() {
            KernelObject::DeviceBus => capability.check(required).map_err(cap_error),
            _ => Err(Error::WrongType),
        }
    })
}

fn device(process: &Process, raw: u64, required: Rights) -> Result<Arc<crate::pci::Device>, Error> {
    arch::without_interrupts(|| {
        object::device(&mut process.capabilities().lock(), handle(raw), required)
    })
    .map_err(object_error)
}

/// Rights of the memory objects a driver gets for BARs and DMA: map and
/// use, but never pass on, so they cannot outlive the driver's own
/// address space in another process.
const DRIVER_MEMORY_RIGHTS: Rights = Rights::READ.union(Rights::WRITE).union(Rights::MAP);

fn device_list(process: &Process, bus: u64, ptr: u64, capacity: u64) -> SyscallResult {
    use oceans_abi::device::DeviceRecord;

    check_bus(process, bus, Rights::READ)?;
    let records = crate::pci::records();
    let len = records.len() * DeviceRecord::SIZE;
    if len as u64 > capacity {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| Error::OutOfMemory)?;
    for record in records {
        let mut encoded = [0u8; DeviceRecord::SIZE];
        record.encode(&mut encoded);
        bytes.extend_from_slice(&encoded);
    }
    process.copy_to_user(ptr, &bytes)?;
    Ok((len as u64, 0))
}

fn device_open(process: &Process, bus: u64, selector: u64, index: u64) -> SyscallResult {
    check_bus(process, bus, Rights::MANAGE)?;
    let vendor = u16::try_from(selector >> 16).map_err(|_| Error::InvalidArgument)?;
    let device = crate::pci::open(vendor, selector as u16, index)?;
    let raw = insert(
        process,
        Capability::new(
            KernelObject::Device(device),
            default_rights(ObjectKind::Device),
        ),
    )?;
    Ok((raw, 0))
}

fn device_config_read(process: &Process, raw: u64, offset: u64, width: u64) -> SyscallResult {
    let device = device(process, raw, Rights::READ)?;
    Ok((u64::from(device.config_read(offset, width)?), 0))
}

fn device_enable(process: &Process, raw: u64) -> SyscallResult {
    device(process, raw, Rights::MANAGE)?.enable();
    Ok((0, 0))
}

fn device_bar(process: &Process, raw: u64, index: u64) -> SyscallResult {
    let memory = device(process, raw, Rights::MANAGE)?.bar(index)?;
    let size = memory.size();
    let raw = insert(
        process,
        Capability::new(KernelObject::Memory(memory), DRIVER_MEMORY_RIGHTS),
    )?;
    Ok((raw, size))
}

fn device_dma_create(process: &Process, raw: u64, size: u64) -> SyscallResult {
    let (memory, address) = device(process, raw, Rights::MANAGE)?.dma_create(size)?;
    let raw = insert(
        process,
        Capability::new(KernelObject::Memory(memory), DRIVER_MEMORY_RIGHTS),
    )?;
    Ok((raw, address))
}

fn device_irq(
    process: &Process,
    raw: u64,
    entry: u64,
    notification: u64,
    bits: u64,
) -> SyscallResult {
    let (device, notification) = arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        let device = object::device(&mut table, handle(raw), Rights::MANAGE)?;
        let notification = object::notification(&mut table, handle(notification), Rights::SIGNAL)?;
        Ok((device, notification))
    })
    .map_err(object_error)?;
    device.bind_irq(entry, notification, bits)?;
    Ok((0, 0))
}

// ---- ABI 8 -----------------------------------------------------------------

fn endpoint_bind(process: &Process, server: u64, notification: u64) -> SyscallResult {
    let (end, notification) = arch::without_interrupts(|| {
        let mut table = process.capabilities().lock();
        let end = object::server_end(&mut table, handle(server), Rights::RECEIVE)?;
        let notification = object::notification(&mut table, handle(notification), Rights::WAIT)?;
        Ok((end, notification))
    })
    .map_err(object_error)?;
    end.bind(notification).map_err(ipc_error)?;
    Ok((0, 0))
}

fn timer_set(process: &Process, raw: u64, bits: u64, ms: u64) -> SyscallResult {
    let notification = arch::without_interrupts(|| {
        object::notification(
            &mut process.capabilities().lock(),
            handle(raw),
            Rights::SIGNAL,
        )
    })
    .map_err(object_error)?;
    crate::ipc::notification::set_timer(&notification, bits, ms);
    Ok((0, 0))
}

// ---- ABI 9 -----------------------------------------------------------------

fn random(process: &Process, ptr: u64, len: u64) -> SyscallResult {
    let len = len_arg(len, oceans_abi::RANDOM_MAX)?;
    let mut bytes = [0u8; oceans_abi::RANDOM_MAX];
    crate::random::fill(&mut bytes[..len]);
    process.copy_to_user(ptr, &bytes[..len])?;
    Ok((len as u64, 0))
}
