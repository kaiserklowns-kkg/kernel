//! Phase 2 exit criterion: isolated processes exchanging IPC messages.
//!
//! One program, five roles chosen by the start argument:
//! - **server**: answers every call with the label + 1 and the data in
//!   upper case, until the client end closes;
//! - **client**: 1000 calls, verifying each reply, plus checks that bad
//!   handles and pointers are rejected with the right errors;
//! - **intruder**: tries to use capabilities it was not given and to read
//!   kernel memory, and must be killed by the kernel.
//! - **parent** (ABI 2): memory objects and mapping rules (rights, W^X
//!   across mappings, overlap), an endpoint it creates, a **child** it spawns
//!   from its image, and capabilities moved in both directions;
//! - **child**: maps a memory object received over IPC and replies with a
//!   new one of its own;
//! - **victim** (ABI 12): blocks in one way or another (receive, call,
//!   notification wait, sleep, a busy loop) until the parent kills it.
//!
//! Every process gets a log capability as handle 0.

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_rt::{Buffer, Error, Handle, Start};

oceans_rt::entry!(main);

const ROLE_SERVER: u64 = 1;
const ROLE_CLIENT: u64 = 2;
const ROLE_INTRUDER: u64 = 3;
const ROLE_PARENT: u64 = 4;
const ROLE_CHILD: u64 = 5;
/// Victims: `ROLE_VICTIM + way` (see [`Victim`]).
const ROLE_VICTIM: u64 = 0x10;

/// How a victim waits to be killed.
#[derive(Clone, Copy)]
enum Victim {
    /// In `ipc_receive` on the server end it was given.
    Receive = 0,
    /// In `ipc_call` on the client end it was given (nobody answers).
    Call = 1,
    NotificationWait = 2,
    Sleep = 3,
    /// Running user code, never entering the kernel.
    Spin = 4,
}

const LABEL_SUM: u64 = 0x5u64 << 8;
const GREETING: &[u8] = b"hello from child";

const CALLS: u64 = 1000;

/// Exit codes reporting which check failed.
const FAIL_PROTOCOL: i64 = 10;
const FAIL_ERROR_CODE: i64 = 11;
const FAIL_SURVIVED: i64 = 99;

fn main(start: Start) -> i64 {
    let Some(&log) = start.handles.first() else {
        return 1;
    };
    match (start.arg, start.handles.get(1)) {
        (ROLE_SERVER, Some(&server)) => run_server(log, server),
        (ROLE_CLIENT, Some(&client)) => run_client(log, client),
        (ROLE_INTRUDER, _) => run_intruder(log),
        (ROLE_PARENT, Some(&image)) => run_parent(log, image),
        (ROLE_CHILD, Some(&server)) => run_child(log, server),
        (way, end) if (ROLE_VICTIM..ROLE_VICTIM + 5).contains(&way) => {
            run_victim(way - ROLE_VICTIM, end)
        }
        _ => 1,
    }
}

fn log_line(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<256>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn run_server(log: Handle, server: Handle) -> i64 {
    let mut request = [0u8; 256];
    let mut served = 0u64;
    loop {
        match oceans_rt::ipc_receive(server, &mut request) {
            Ok((len, label)) => {
                let reply = &mut request[..len];
                reply.make_ascii_uppercase();
                if oceans_rt::ipc_reply(label + 1, reply).is_err() {
                    return FAIL_PROTOCOL;
                }
                served += 1;
            }
            Err(Error::PeerClosed) => {
                log_line(
                    log,
                    format_args!("server: client gone after {served} calls"),
                );
                return 0;
            }
            Err(_) => return FAIL_PROTOCOL,
        }
    }
}

fn run_client(log: Handle, client: Handle) -> i64 {
    if oceans_rt::abi_version() < 1 {
        return FAIL_PROTOCOL;
    }
    let mut reply = [0u8; 256];
    for i in 0..CALLS {
        let mut request = Buffer::<32>::new();
        let _ = write!(request, "ping-{i}");
        let Ok((len, label)) = oceans_rt::ipc_call(client, i, request.as_bytes(), &mut reply)
        else {
            return FAIL_PROTOCOL;
        };
        let mut expected = Buffer::<32>::new();
        let _ = write!(expected, "PING-{i}");
        if label != i + 1 || &reply[..len] != expected.as_bytes() {
            return FAIL_PROTOCOL;
        }
    }

    // The kernel validates everything it is handed.
    let bad_pointer = unsafe { core::slice::from_raw_parts(0x10 as *const u8, 4) };
    let checks = [
        (
            oceans_rt::ipc_call(client, 0, bad_pointer, &mut reply).err(),
            Error::BadAddress,
        ),
        (
            oceans_rt::ipc_call(log, 0, b"x", &mut reply).err(),
            Error::WrongType,
        ),
        (
            oceans_rt::ipc_call(Handle(0xdead), 0, b"x", &mut reply).err(),
            Error::InvalidHandle,
        ),
        (oceans_rt::ipc_reply(0, b"x").err(), Error::NoPendingCall),
    ];
    if checks.iter().any(|&(got, want)| got != Some(want)) {
        return FAIL_ERROR_CODE;
    }

    log_line(
        log,
        format_args!("client: {CALLS} round trips verified, bad requests rejected"),
    );
    // Exiting closes the client end; the server sees PeerClosed.
    0
}

fn run_intruder(log: Handle) -> i64 {
    // Handles it was never given do not exist for it.
    if oceans_rt::debug_write(Handle(1), "forged").err() != Some(Error::InvalidHandle) {
        return FAIL_ERROR_CODE;
    }
    log_line(log, format_args!("intruder: reading kernel memory"));
    // Supervisor-only page: this must fault, and the kernel kills us.
    let kernel = 0xffff_ffff_8000_0000 as *const u64;
    // SAFETY: deliberately invalid; the point is that it cannot succeed.
    let value = unsafe { kernel.read_volatile() };
    log_line(
        log,
        format_args!("intruder: read {value:#x} (isolation broken!)"),
    );
    FAIL_SURVIVED
}

fn pattern(i: usize) -> u8 {
    (i * 7 + 3) as u8
}

/// Exit code `base + step` identifies the failing step.
fn run_parent(log: Handle, image: Handle) -> i64 {
    match parent_steps(log, image) {
        Ok(()) => 0,
        Err(step) => 20 + step,
    }
}

fn parent_steps(log: Handle, image: Handle) -> Result<(), i64> {
    use oceans_rt::{
        close, duplicate, endpoint_create, ipc_call_msg, memory_create, memory_map, memory_unmap,
        process_spawn, process_wait, prot, rights,
    };
    let expect = |step: i64, ok: bool| if ok { Ok(()) } else { Err(step) };

    expect(1, oceans_rt::abi_version() >= 2)?;

    // Memory: create, map read-write, fill.
    const SIZE: usize = 3 * 4096;
    let memory = memory_create(SIZE as u64).map_err(|_| 2)?;
    let rw = memory_map(memory, 0, prot::READ | prot::WRITE).map_err(|_| 3)?;
    // SAFETY: the kernel just mapped SIZE writable bytes at `rw`.
    let bytes = unsafe { core::slice::from_raw_parts_mut(rw, SIZE) };
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = pattern(i);
    }

    // A read-only view of the same object sees the same bytes.
    let read_only =
        duplicate(memory, rights::READ | rights::MAP | rights::TRANSFER).map_err(|_| 4)?;
    let ro = memory_map(read_only, 0, prot::READ).map_err(|_| 5)?;
    // SAFETY: mapped readable above.
    let view = unsafe { core::slice::from_raw_parts(ro, SIZE) };
    expect(
        6,
        view[100] == pattern(100) && view[SIZE - 1] == pattern(SIZE - 1),
    )?;

    // Mapping rules.
    expect(
        7,
        memory_map(read_only, 0, prot::READ | prot::WRITE) == Err(Error::MissingRights),
    )?;
    expect(
        8,
        memory_map(memory, 0, prot::READ | prot::WRITE | prot::EXECUTE)
            == Err(Error::InvalidArgument),
    )?;
    // Already mapped writable, so never executable (W^X across mappings).
    expect(
        9,
        memory_map(memory, 0, prot::READ | prot::EXECUTE) == Err(Error::InvalidArgument),
    )?;
    expect(
        10,
        memory_map(memory, rw as u64, prot::READ) == Err(Error::AddressInUse),
    )?;
    expect(
        11,
        duplicate(read_only, rights::READ | rights::WRITE) == Err(Error::MissingRights),
    )?;
    expect(12, memory_unmap(ro).is_ok())?;
    expect(13, memory_unmap(ro) == Err(Error::InvalidArgument))?;

    // A child process, given a log and the server end of a new endpoint.
    let (server, client) = endpoint_create().map_err(|_| 14)?;
    let child_log = duplicate(log, rights::WRITE | rights::TRANSFER).map_err(|_| 15)?;
    let child = process_spawn(image, 0, &[child_log, server], ROLE_CHILD).map_err(|_| 16)?;
    expect(17, close(server) == Err(Error::InvalidHandle))?; // moved to the child

    // Send the read-only view; get the sum and a new object back.
    let mut reply = [0u8; 64];
    let mut reply_handles = [Handle(0); 4];
    let got = ipc_call_msg(
        client,
        LABEL_SUM,
        &[],
        &[read_only],
        &mut reply,
        &mut reply_handles,
    )
    .map_err(|_| 18)?;
    let sum: u64 = (0..16).map(|i| u64::from(pattern(i))).sum();
    expect(
        19,
        got.label == LABEL_SUM + 1 && got.data_len == 8 && reply[..8] == sum.to_le_bytes(),
    )?;
    expect(20, close(read_only) == Err(Error::InvalidHandle))?; // moved to the child
    expect(21, got.handles_len == 1)?;
    let greeting = memory_map(reply_handles[0], 0, prot::READ).map_err(|_| 22)?;
    // SAFETY: mapped readable above; objects are at least one page.
    let text = unsafe { core::slice::from_raw_parts(greeting, GREETING.len()) };
    expect(23, text == GREETING)?;

    // Closing our end ends the child; wait for it.
    close(client).map_err(|_| 24)?;
    expect(25, process_wait(child) == Ok(0))?;

    log_line(
        log,
        format_args!(
            "parent: memory rules, spawn, capability transfer both ways and wait verified"
        ),
    );
    kill_steps(log, image)?;
    log_line(
        log,
        format_args!(
            "parent: processes killed while receiving, calling, waiting, sleeping and running"
        ),
    );
    Ok(())
}

/// `PROCESS_KILL` (ADR-0044): a victim is killed in each way it can wait,
/// exits with `EXIT_KILLED`, and leaves nothing behind (no pending call, no
/// server end).
fn kill_steps(log: Handle, image: Handle) -> Result<(), i64> {
    use oceans_rt::{
        close, duplicate, endpoint_create, ipc_call, ipc_receive, process_kill, process_spawn,
        process_wait, rights, sleep_ms,
    };
    let expect = |step: i64, ok: bool| if ok { Ok(()) } else { Err(step) };
    let killed = Ok(oceans_rt::EXIT_KILLED);
    for (index, way) in [
        Victim::Receive,
        Victim::Call,
        Victim::NotificationWait,
        Victim::Sleep,
        Victim::Spin,
    ]
    .into_iter()
    .enumerate()
    {
        let step = 30 + 5 * index as i64;
        let (server, client) = endpoint_create().map_err(|_| step)?;
        // The victim gets the end it waits on; we keep the other.
        let (given, kept) = match way {
            Victim::Receive => (server, client),
            _ => (client, server),
        };
        let victim_log = duplicate(log, rights::WRITE | rights::TRANSFER).map_err(|_| step)?;
        let victim = process_spawn(image, 0, &[victim_log, given], ROLE_VICTIM + way as u64)
            .map_err(|_| step + 1)?;
        // Long enough for it to block (or spin) on the single CPU.
        sleep_ms(30);
        expect(step + 2, process_kill(victim).is_ok())?;
        expect(step + 3, process_wait(victim) == killed)?;
        // Its ends closed with it, and no call it made stayed queued.
        let mut buffer = [0u8; 8];
        let gone = match way {
            Victim::Receive => ipc_call(kept, 1, b"", &mut buffer).map(drop),
            _ => ipc_receive(kept, &mut buffer).map(drop),
        };
        expect(step + 4, gone == Err(Error::PeerClosed))?;
        let _ = close(kept);
        // Killing it again does nothing.
        expect(step + 4, process_kill(victim).is_ok())?;
        let _ = close(victim);
    }
    // Without MANAGE, a process handle cannot kill.
    let victim_log = duplicate(log, rights::WRITE | rights::TRANSFER).map_err(|_| 60)?;
    let victim = process_spawn(image, 0, &[victim_log], ROLE_VICTIM + Victim::Sleep as u64)
        .map_err(|_| 60)?;
    let watch_only = duplicate(victim, rights::WAIT).map_err(|_| 61)?;
    expect(62, process_kill(watch_only) == Err(Error::MissingRights))?;
    expect(
        63,
        process_kill(victim).is_ok() && process_wait(victim) == killed,
    )?;
    let _ = close(victim);
    // Killed at once, before it settles: the kill may come (on another
    // CPU) just as it goes to sleep or to wait, and must not be lost.
    for round in 0..16 {
        let way = if round % 2 == 0 {
            Victim::Sleep
        } else {
            Victim::NotificationWait
        };
        let victim_log = duplicate(log, rights::WRITE | rights::TRANSFER).map_err(|_| 64)?;
        let victim =
            process_spawn(image, 0, &[victim_log], ROLE_VICTIM + way as u64).map_err(|_| 64)?;
        expect(
            65,
            process_kill(victim).is_ok() && process_wait(victim) == killed,
        )?;
        let _ = close(victim);
    }
    Ok(())
}

/// Waits, as `way` says, until killed; returning at all is a failure.
fn run_victim(way: u64, end: Option<&Handle>) -> i64 {
    let mut buffer = [0u8; 8];
    match way {
        0 => {
            let _ = end.map(|&server| oceans_rt::ipc_receive(server, &mut buffer));
        }
        1 => {
            let _ = end.map(|&client| oceans_rt::ipc_call(client, 1, b"", &mut buffer));
        }
        2 => {
            if let Ok(notification) = oceans_rt::notification_create() {
                let _ = oceans_rt::notification_wait(notification);
            }
        }
        3 => oceans_rt::sleep_ms(3_600_000),
        _ => {
            let mut spins = 0u64;
            loop {
                spins = core::hint::black_box(spins.wrapping_add(1));
            }
        }
    }
    FAIL_SURVIVED
}

fn run_child(log: Handle, server: Handle) -> i64 {
    use oceans_rt::{
        close, duplicate, ipc_receive_msg, ipc_reply_msg, memory_create, memory_map, memory_unmap,
        prot, rights,
    };
    let mut data = [0u8; 64];
    let mut handles = [Handle(0); 4];
    loop {
        match ipc_receive_msg(server, &mut data, &mut handles) {
            Ok(got) if got.label == LABEL_SUM && got.handles_len == 1 => {
                let Ok(view) = memory_map(handles[0], 0, prot::READ) else {
                    return FAIL_PROTOCOL;
                };
                // SAFETY: mapped readable; objects are at least one page.
                let bytes = unsafe { core::slice::from_raw_parts(view, 16) };
                let sum: u64 = bytes.iter().map(|&b| u64::from(b)).sum();

                // Reply with a new object holding a greeting, read-only.
                let Ok(greeting) = memory_create(4096) else {
                    return FAIL_PROTOCOL;
                };
                let Ok(page) = memory_map(greeting, 0, prot::READ | prot::WRITE) else {
                    return FAIL_PROTOCOL;
                };
                // SAFETY: mapped writable, one page.
                unsafe { core::slice::from_raw_parts_mut(page, GREETING.len()) }
                    .copy_from_slice(GREETING);
                let _ = memory_unmap(page);
                let Ok(shared) = duplicate(greeting, rights::READ | rights::MAP | rights::TRANSFER)
                else {
                    return FAIL_PROTOCOL;
                };
                let _ = close(greeting);
                if ipc_reply_msg(LABEL_SUM + 1, &sum.to_le_bytes(), &[shared]).is_err() {
                    return FAIL_PROTOCOL;
                }
            }
            Err(Error::PeerClosed) => {
                log_line(
                    log,
                    format_args!("child: parent closed the endpoint, exiting"),
                );
                return 0;
            }
            _ => return FAIL_PROTOCOL,
        }
    }
}
