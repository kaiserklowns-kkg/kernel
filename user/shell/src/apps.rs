//! `app`: installing, running and managing apps through Oceans Core
//! (ADR-0045 to ADR-0047), and asking the user for their permissions.
//!
//! The shell is the consent agent: when Core says an app needs a decision,
//! the shell shows what the system says the permission means (never text
//! of the app's choosing, except its declared reason, shown as a quote)
//! and sends the user's answer back.

use core::fmt::Write;

use oceans_core_proto::{
    Core, CoreError, Decision, Status, field, op, outcome, parts, run_flags, source,
};
use oceans_fs_proto::{FsError, Kind};
use oceans_package::Permission;
use oceans_rt::{Buffer, Handle, rights};

use super::{LINE_MAX, Shell};

const USAGE: &str = "usage: app list | info ID | install PATH | run ID [ARGS...] | \
                     start ID [ARGS...] | stop ID | enable ID | disable ID | \
                     remove ID [--keep-data] | \
                     grant ID PERMISSION | revoke ID PERMISSION | rollback ID | audit\r\n";

impl Shell {
    fn core(&self) -> Option<Core> {
        self.directory.find("use", "core").map(Core)
    }

    /// `app ...`
    pub(super) fn app(&self, words: &[&str]) {
        let Some(core) = self.core() else {
            return self.print(format_args!("app: this shell cannot manage apps\r\n"));
        };
        match words {
            ["list"] => self.app_list(core),
            ["info", id] => self.app_info(core, id),
            ["install", path] => self.app_install(core, path),
            ["run", id, args @ ..] => self.app_run(core, id, args, false),
            ["start", id, args @ ..] => self.app_run(core, id, args, true),
            ["stop", id] => self.app_simple(core, op::STOP, &[], id, "stopped"),
            ["enable", id] => self.app_enable(core, id),
            ["disable", id] => self.app_simple(core, op::DISABLE, &[], id, "disabled"),
            ["remove", id] => self.app_simple(core, op::REMOVE, &[0], id, "removed"),
            ["remove", id, "--keep-data"] => {
                self.app_simple(core, op::REMOVE, &[1], id, "removed (its data kept)")
            }
            ["grant", id, permission] => self.app_decide(core, id, permission, true),
            ["revoke", id, permission] => self.app_decide(core, id, permission, false),
            ["reset", id, permission] => self.app_reset(core, id, permission),
            ["rollback", id] => {
                let mut reply = [0u8; 64];
                match core.about(op::ROLLBACK, &[], id, &mut reply) {
                    Ok(got) => self.print(format_args!(
                        "app: {id} rolled back to {}\r\n",
                        core::str::from_utf8(&reply[..got.len]).unwrap_or("?")
                    )),
                    Err((error, _)) => self.app_error(id, error, &[]),
                }
            }
            ["audit"] => self.app_audit(core),
            _ => self.write(USAGE.as_bytes()),
        }
    }

    fn app_error(&self, what: &str, error: CoreError, text: &[u8]) {
        let detail = core::str::from_utf8(text).unwrap_or("");
        if detail.is_empty() {
            self.print(format_args!("app: {what}: {}\r\n", error.message()));
        } else {
            self.print(format_args!("app: {what}: {detail}\r\n"));
        }
    }

    fn app_simple(&self, core: Core, operation: u64, prefix: &[u8], id: &str, done: &str) {
        let mut reply = [0u8; 8];
        match core.about(operation, prefix, id, &mut reply) {
            Ok(_) => self.print(format_args!("app: {done} {id}\r\n")),
            Err((error, _)) => self.app_error(id, error, &[]),
        }
    }

    /// `app enable ID` (ADR-0049): asks for undecided permissions first,
    /// as a run does.
    fn app_enable(&self, core: Core, id: &str) {
        let mut reply = [0u8; 8];
        for attempt in 0..2 {
            match core.about(op::ENABLE, &[], id, &mut reply) {
                Ok(_) => {
                    return self.print(format_args!(
                        "app: enabled {id}: it runs now and at every boot\r\n"
                    ));
                }
                Err((CoreError::Status(Status::NeedsConsent), _)) if attempt == 0 => {
                    if !self.ask_permissions(core, id) {
                        return;
                    }
                }
                Err((error, _)) => return self.app_error(id, error, &[]),
            }
        }
    }

    fn app_list(&self, core: Core) {
        let mut reply = [0u8; 256];
        for index in 0u32.. {
            match core.call(op::LIST, &index.to_le_bytes(), &[], &mut reply) {
                Ok(got) if got.len >= 1 => {
                    let mut fields = parts(&reply[1..got.len]);
                    let (id, version, name) = (
                        fields.next().unwrap_or("?"),
                        fields.next().unwrap_or("?"),
                        fields.next().unwrap_or("?"),
                    );
                    self.print(format_args!(
                        "  {id}  {version}  {name}{}\r\n",
                        if reply[0] != 0 { "  (running)" } else { "" }
                    ));
                }
                Err((CoreError::Status(Status::NotFound), _)) => {
                    if index == 0 {
                        self.print(format_args!("app: no apps installed\r\n"));
                    }
                    return;
                }
                Ok(_) => return,
                Err((error, _)) => return self.app_error("list", error, &[]),
            }
        }
    }

    /// One `INFO` field as text.
    fn app_field(&self, core: Core, id: &str, which: u8) -> Result<Buffer<256>, CoreError> {
        let mut reply = [0u8; 256];
        let got = core
            .about(op::INFO, &[which], id, &mut reply)
            .map_err(|(error, _)| error)?;
        let mut text = Buffer::new();
        let _ = text.write_str(core::str::from_utf8(&reply[..got.len]).unwrap_or("?"));
        Ok(text)
    }

    fn app_info(&self, core: Core, id: &str) {
        let name = match self.app_field(core, id, field::NAME) {
            Ok(name) => name,
            Err(error) => return self.app_error(id, error, &[]),
        };
        let get = |which| self.app_field(core, id, which).unwrap_or_default();
        self.print(format_args!(
            "{} ({id}) {}\r\n  publisher: {} (key {})\r\n",
            name.as_str(),
            get(field::VERSION).as_str(),
            get(field::PUBLISHER).as_str(),
            get(field::KEY).as_str()
        ));
        let description = get(field::DESCRIPTION);
        if !description.as_str().is_empty() {
            self.print(format_args!("  {}\r\n", description.as_str()));
        }
        let previous = get(field::PREVIOUS);
        let runtime = get(field::RUNTIME);
        if !runtime.as_str().is_empty() {
            self.print(format_args!("  runtime: {}\r\n", runtime.as_str()));
        }
        self.print(format_args!(
            "  {}, {}, channel: {}{}{}\r\n  permissions:\r\n",
            get(field::KIND).as_str(),
            get(field::STATE).as_str(),
            get(field::CHANNEL).as_str(),
            if previous.as_str().is_empty() {
                ""
            } else {
                ", rollback to "
            },
            previous.as_str()
        ));
        let mut reply = [0u8; 256];
        for index in 0u8.. {
            let Ok(got) = core.about(op::PERMISSION, &[index], id, &mut reply) else {
                break;
            };
            let Some((permission, decision)) = permission_reply(&reply[..got.len]) else {
                break;
            };
            let reason = core::str::from_utf8(&reply[2..got.len]).unwrap_or("");
            self.print(format_args!(
                "    {:<12} {:<11} {}\r\n",
                permission.name(),
                decision.word(),
                reason
            ));
        }
    }

    fn app_install(&self, core: Core, path: &str) {
        let Some(root) = self.fs_root() else {
            return self.print(format_args!("app: this shell has no filesystem\r\n"));
        };
        let file = match root.walk(path, 0) {
            Ok((file, Kind::File)) => file,
            Ok((node, Kind::Directory)) => {
                node.close();
                return self.print(format_args!("app: {path}: is a directory\r\n"));
            }
            Err(error) => {
                return self.print(format_args!("app: {path}: {}\r\n", FsError::message(error)));
            }
        };
        let mut reply = [0u8; 256];
        // The file handle moves to Core, which reads the package itself.
        match core.call(op::INSTALL, &[], &[file.0], &mut reply) {
            Ok(got) if got.len >= 1 => {
                let mut fields = parts(&reply[1..got.len]);
                let (id, version, previous) = (
                    fields.next().unwrap_or("?"),
                    fields.next().unwrap_or("?"),
                    fields.next().unwrap_or(""),
                );
                if reply[0] == outcome::UPDATED {
                    self.print(format_args!(
                        "app: updated {id} {previous} -> {version}\r\n"
                    ));
                } else {
                    self.print(format_args!("app: installed {id} {version}\r\n"));
                }
            }
            Ok(_) => self.print(format_args!("app: {path}: bad reply\r\n")),
            Err((error, len)) => self.app_error(path, error, &reply[..len]),
        }
    }

    fn app_run(&self, core: Core, id: &str, args: &[&str], detach: bool) {
        let mut data = Buffer::<{ 2 + LINE_MAX }>::new();
        let mut request = [0u8; 248];
        let flags = if detach { run_flags::DETACH } else { 0 };
        let _ = data.write_str(id);
        for arg in args {
            let _ = write!(data, " {arg}");
        }
        let header = [flags, id.len() as u8];
        let body = &data.as_bytes()[id.len()..];
        let body = body.strip_prefix(b" ").unwrap_or(body);
        let len = 2 + id.len() + body.len();
        if id.len() > oceans_core_proto::MAX_ID || len > request.len() {
            return self.print(format_args!("app: {id}: arguments too long\r\n"));
        }
        request[..2].copy_from_slice(&header);
        request[2..2 + id.len()].copy_from_slice(id.as_bytes());
        request[2 + id.len()..len].copy_from_slice(body);

        // Asked at most once per run: an answer is what decides.
        for attempt in 0..2 {
            // The console the app may write to (output only).
            let out = self
                .directory
                .find("console", "console")
                .and_then(|console| {
                    oceans_rt::duplicate(console, rights::WRITE | rights::TRANSFER).ok()
                });
            let handles: &[Handle] = match &out {
                Some(out) => core::slice::from_ref(out),
                None => &[],
            };
            let mut reply = [0u8; 64];
            match core.call(op::RUN, &request[..len], handles, &mut reply) {
                Ok(got) => {
                    match got.handle {
                        Some(process) => self.wait_app(id, process),
                        None => self.print(format_args!("app: started {id}\r\n")),
                    }
                    return;
                }
                Err((CoreError::Status(Status::NeedsConsent), _)) if attempt == 0 => {
                    if !self.ask_permissions(core, id) {
                        return;
                    }
                }
                Err((error, len)) => return self.app_error(id, error, &reply[..len]),
            }
        }
    }

    fn wait_app(&self, id: &str, process: Handle) {
        let code = oceans_rt::process_wait(process);
        let _ = oceans_rt::close(process);
        match code {
            Ok(0) => {}
            Ok(oceans_rt::EXIT_KILLED) => self.print(format_args!("app: {id} was stopped\r\n")),
            Ok(code) if code <= -128 => self.print(format_args!(
                "app: {id} was killed by CPU exception {}\r\n",
                -128 - code
            )),
            Ok(code) => self.print(format_args!("app: {id} exited with {code}\r\n")),
            Err(error) => self.print(format_args!("app: {id}: {error:?}\r\n")),
        }
    }

    /// Asks the user about each undecided permission of `id`; `false` if
    /// a question could not be asked or answered.
    fn ask_permissions(&self, core: Core, id: &str) -> bool {
        let get = |which| self.app_field(core, id, which).unwrap_or_default();
        let (name, version, publisher) =
            (get(field::NAME), get(field::VERSION), get(field::PUBLISHER));
        let mut reply = [0u8; 256];
        for index in 0u8.. {
            let Ok(got) = core.about(op::PERMISSION, &[index], id, &mut reply) else {
                return true;
            };
            let Some((permission, Decision::Undecided)) = permission_reply(&reply[..got.len])
            else {
                continue;
            };
            let reason = core::str::from_utf8(&reply[2..got.len]).unwrap_or("");
            // The words are the system's; only the quoted reason is the app's.
            self.print(format_args!(
                "{} ({id} {}, from {}) asks to:\r\n  {}\r\n",
                name.as_str(),
                version.as_str(),
                publisher.as_str(),
                permission.description()
            ));
            if !reason.is_empty() {
                self.print(format_args!("  reason given by the app: \"{reason}\"\r\n"));
            }
            let Some(allow) = self.ask("Allow? [y/N] ") else {
                return false;
            };
            let Some(index) = Permission::ALL.iter().position(|&p| p == permission) else {
                return false;
            };
            let decision = [index as u8, u8::from(allow), source::PROMPT];
            let mut answer = [0u8; 8];
            if let Err((error, _)) = core.about(op::DECIDE, &decision, id, &mut answer) {
                self.app_error(id, error, &[]);
                return false;
            }
            self.print(format_args!(
                "app: {} {}\r\n",
                permission.name(),
                if allow { "allowed" } else { "denied" }
            ));
        }
        true
    }

    fn app_decide(&self, core: Core, id: &str, permission: &str, allow: bool) {
        let Some(index) = Permission::ALL.iter().position(|p| p.name() == permission) else {
            return self.print(format_args!(
                "app: {permission}: unknown permission (console, storage, system-info, network, files, pointer)\r\n"
            ));
        };
        let decision = [index as u8, u8::from(allow), source::COMMAND];
        let mut reply = [0u8; 8];
        match core.about(op::DECIDE, &decision, id, &mut reply) {
            Ok(got) => self.print(format_args!(
                "app: {permission} {} for {id}{}\r\n",
                if allow { "allowed" } else { "revoked" },
                if got.len >= 1 && reply[0] != 0 {
                    "; it was running and has been stopped"
                } else {
                    ""
                }
            )),
            Err((CoreError::Status(Status::BadRequest), _)) => self.print(format_args!(
                "app: {id} does not ask for {permission}, or it needs no decision\r\n"
            )),
            Err((error, _)) => self.app_error(id, error, &[]),
        }
    }

    /// `app reset ID PERMISSION`: forget the decision; the next run asks.
    fn app_reset(&self, core: Core, id: &str, permission: &str) {
        let Some(index) = Permission::ALL.iter().position(|p| p.name() == permission) else {
            return self.print(format_args!("app: {permission}: unknown permission\r\n"));
        };
        let request = [
            index as u8,
            oceans_core_proto::decision::FORGET,
            source::COMMAND,
        ];
        let mut reply = [0u8; 8];
        match core.about(op::DECIDE, &request, id, &mut reply) {
            Ok(_) => self.print(format_args!(
                "app: {permission} for {id} will be asked again\r\n"
            )),
            Err((error, _)) => self.app_error(id, error, &[]),
        }
    }

    fn app_audit(&self, core: Core) {
        let mut reply = [0u8; 256];
        // Newest first, as Core keeps them; printed oldest first.
        let mut count = 0u32;
        while core
            .call(op::AUDIT, &count.to_le_bytes(), &[], &mut reply)
            .is_ok()
        {
            count += 1;
        }
        if count == 0 {
            return self.print(format_args!("app: nothing recorded yet\r\n"));
        }
        for index in (0..count).rev() {
            if let Ok(got) = core.call(op::AUDIT, &index.to_le_bytes(), &[], &mut reply) {
                self.print(format_args!(
                    "  {}\r\n",
                    core::str::from_utf8(&reply[..got.len]).unwrap_or("?")
                ));
            }
        }
    }
}

/// `[permission u8][decision u8]...` of a `PERMISSION` reply.
fn permission_reply(bytes: &[u8]) -> Option<(Permission, Decision)> {
    let [permission, decision, ..] = bytes else {
        return None;
    };
    Some((
        *Permission::ALL.get(usize::from(*permission))?,
        Decision::from_byte(*decision)?,
    ))
}
