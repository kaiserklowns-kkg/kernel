//! Echo service: answers every call with its label + 1 and the data in
//! upper case. Manifest grants: `log`, `provide = echo`.

#![no_std]
#![no_main]

use oceans_rt::{Error, Start};

oceans_rt::entry!(main);

fn main(start: Start) -> i64 {
    let (Some(&log), Some(&server)) = (start.handles.first(), start.handles.get(1)) else {
        return 1;
    };
    let _ = oceans_rt::debug_write(log, "echo: ready");
    let mut buffer = [0u8; 256];
    loop {
        match oceans_rt::ipc_receive(server, &mut buffer) {
            Ok((len, label)) => {
                let reply = &mut buffer[..len];
                reply.make_ascii_uppercase();
                if oceans_rt::ipc_reply(label + 1, reply).is_err() {
                    return 2;
                }
            }
            // No clients left.
            Err(Error::PeerClosed) => return 0,
            Err(_) => return 3,
        }
    }
}
