//! `ui`: pairing a browser with the Oceans web experience (ADR-0058).
//!
//! The bridge service serves the web app and the System API, but holds no
//! authority over apps of its own. The shell, the user's agent, gives it
//! some: a Core capability limited to querying, running and auditing apps
//! (never installing or deciding permissions, ADR-0048), with a fresh
//! random code from the kernel's generator that the browser must present.
//! `ui unpair` takes it back.

use core::fmt::Write;

use oceans_core_proto::{Core, access, op as core_op};
use oceans_net_proto::Dotted;
use oceans_rt::{Buffer, Handle};

use super::Shell;

/// The `bridge` protocol (go/cmd/bridge/protocol.go).
mod op {
    pub const PAIR: u64 = 1;
    pub const UNPAIR: u64 = 2;
    pub const STATUS: u64 = 3;
}

mod status {
    pub const OK: u64 = 0;
    pub const NOT_PAIRED: u64 = 2;
    pub const UNAVAILABLE: u64 = 3;
}

/// What a paired browser may do with Oceans Core.
const PAIRED_RIGHTS: u8 = access::QUERY | access::RUN | access::AUDIT;
/// Random bytes in a pairing code (128 bits, 32 hex digits).
const CODE_BYTES: usize = 16;

const USAGE: &str = "usage: ui pair | ui unpair | ui status\r\n";

impl Shell {
    /// `ui ...`
    pub(super) fn ui(&self, words: &[&str]) {
        let Some(bridge) = self.directory.find("use", "bridge") else {
            return self.print(format_args!("ui: this shell has no web experience\r\n"));
        };
        match words {
            ["pair"] => self.ui_pair(bridge),
            ["unpair"] => match oceans_rt::ipc_call(bridge, op::UNPAIR, &[], &mut []) {
                Ok((_, status::OK)) => self.print(format_args!(
                    "ui: unpaired; the browser's access is closed\r\n"
                )),
                Ok((_, status::NOT_PAIRED)) => {
                    self.print(format_args!("ui: no browser is paired\r\n"))
                }
                Ok(_) => self.print(format_args!("ui: the bridge refused\r\n")),
                Err(error) => self.print(format_args!("ui: {error:?}\r\n")),
            },
            ["status"] => {
                let mut reply = [0u8; 8];
                match oceans_rt::ipc_call(bridge, op::STATUS, &[], &mut reply) {
                    Ok((len, status::OK)) if len >= 3 => {
                        let port = u16::from_le_bytes([reply[1], reply[2]]);
                        let paired = if reply[0] != 0 {
                            "paired"
                        } else {
                            "not paired"
                        };
                        if port == 0 {
                            self.print(format_args!("ui: {paired}; not listening\r\n"));
                        } else {
                            self.print(format_args!("ui: {paired}; listening on port {port}\r\n"));
                        }
                    }
                    Ok(_) => self.print(format_args!("ui: the bridge refused\r\n")),
                    Err(error) => self.print(format_args!("ui: {error:?}\r\n")),
                }
            }
            _ => self.write(USAGE.as_bytes()),
        }
    }

    fn ui_pair(&self, bridge: Handle) {
        let Some(core) = self.directory.find("use", "core") else {
            return self.print(format_args!("ui: this shell cannot manage apps\r\n"));
        };
        let mut random = [0u8; CODE_BYTES];
        if oceans_rt::random(&mut random).is_err() {
            return self.print(format_args!("ui: no random numbers to make a code\r\n"));
        }
        let mut code = Buffer::<{ 2 * CODE_BYTES }>::new();
        for byte in random {
            let _ = write!(code, "{byte:02x}");
        }
        let mut minted = [0u8; 8];
        let delegated = match Core(core).call(core_op::MINT, &[PAIRED_RIGHTS], &[], &mut minted) {
            Ok(got) => got.handle,
            Err((error, _)) => {
                return self.print(format_args!("ui: {}\r\n", error.message()));
            }
        };
        let Some(delegated) = delegated else {
            return self.print(format_args!("ui: Oceans Core gave no capability\r\n"));
        };
        // The capability moves to the bridge with the call.
        let mut reply = [0u8; 8];
        let mut back = [Handle(0); 1];
        let port = match oceans_rt::ipc_call_msg(
            bridge,
            op::PAIR,
            code.as_bytes(),
            &[delegated],
            &mut reply,
            &mut back,
        ) {
            Ok(got) if got.label == status::OK && got.data_len >= 2 => {
                u16::from_le_bytes([reply[0], reply[1]])
            }
            Ok(got) if got.label == status::UNAVAILABLE => {
                return self.print(format_args!(
                    "ui: paired, but the bridge is not listening (see the log)\r\n"
                ));
            }
            Ok(_) => return self.print(format_args!("ui: the bridge refused to pair\r\n")),
            Err(error) => {
                // Not delivered: the capability is still ours.
                let _ = oceans_rt::close(delegated);
                return self.print(format_args!("ui: {error:?}\r\n"));
            }
        };
        let mut address = Buffer::<24>::new();
        match self
            .directory
            .find("use", "net")
            .and_then(|net| oceans_net_proto::info(net).ok())
        {
            Some(info) if info.configured => {
                let _ = write!(address, "{}", Dotted(info.address));
            }
            _ => {
                let _ = address.write_str("THIS-COMPUTER");
            }
        }
        self.print(format_args!(
            "ui: paired. In a browser, open http://{}:{port}/ and enter the code below.\r\n\
             ui: it may list, start and stop apps, read the audit log and ask Oceans AI;\r\n\
             ui: it cannot install apps or decide permissions. `ui unpair` ends it.\r\n",
            address.as_str()
        ));
        self.print(format_args!("ui: pairing code: {}\r\n", code.as_str()));
    }
}
