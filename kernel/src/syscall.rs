//! System call dispatch, ABI version 1 (`oceans-abi`, ADR-0014).
//!
//! Every argument is untrusted: handles are looked up with the required
//! rights in the caller's own capability table, and buffers are copied
//! through the process's page tables (`Process::copy_*_user`). Locks are
//! never held across blocking IPC, and, because syscalls run with interrupts
//! enabled, every spinlock is taken with interrupts disabled (a preempted
//! lock holder would otherwise deadlock the CPU).

use alloc::sync::Arc;

use oceans_abi::{DEBUG_WRITE_MAX, Error, IPC_MAX_INLINE, nr};
use oceans_capability::{CapError, Handle, Rights};

use crate::arch::{self, SyscallFrame};
use crate::ipc::{IpcError, Message};
use crate::object::{self, KernelObject, ObjectError};
use crate::process::{self, Process};
use crate::{klog, sched};

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
    }
}

fn ipc_error(error: IpcError) -> Error {
    match error {
        IpcError::PeerClosed => Error::PeerClosed,
        IpcError::NoReply => Error::NoReply,
        IpcError::MessageTooLarge | IpcError::TooManyCapabilities => Error::TooLarge,
        IpcError::OutOfMemory => Error::OutOfMemory,
    }
}
