//! Phase 2 exit criterion: isolated processes exchanging IPC messages.
//!
//! One program, three roles chosen by the start argument:
//! - **server**: answers every call with the label + 1 and the data in
//!   upper case, until the client end closes;
//! - **client**: 1000 calls, verifying each reply, plus checks that bad
//!   handles and pointers are rejected with the right errors;
//! - **intruder**: tries to use capabilities it was not given and to read
//!   kernel memory, and must be killed by the kernel.
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
    if oceans_rt::abi_version() != 1 {
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
