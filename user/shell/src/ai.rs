//! `ai`: asking the Oceans AI runtime (ADR-0051).
//!
//! The shell is the user's agent here too. For each question it delegates
//! to the session exactly what an agent may use: a Core capability limited
//! to querying and running apps (ADR-0048), never `decide`. When the agent
//! wants to change something, the runtime stops and asks; the shell shows
//! the question, worded by the tool, and sends the user's answer.

use core::fmt::Write;

use oceans_core_proto::{Core, access, op as core_op};
use oceans_rt::{Buffer, Handle};

use super::{LINE_MAX, Shell};

/// The `ai` protocol (go/cmd/ai/protocol.go).
mod op {
    pub const ASK: u64 = 1;
    pub const CONTINUE: u64 = 2;
    pub const TEXT: u64 = 3;
    pub const ACTIVITY: u64 = 4;
    pub const CONFIGURE: u64 = 5;
}

mod status {
    pub const DONE: u64 = 0;
    pub const NEEDS_APPROVAL: u64 = 1;
    pub const FAILED: u64 = 2;
}

const USAGE: &str =
    "usage: ai ask QUESTION... | ai model URL MODEL [--dns SERVER] [--ca PATH] | ai activity\r\n";
/// Approvals asked in one session at most (the runtime bounds its steps too).
const MAX_APPROVALS: usize = 8;

impl Shell {
    /// `ai ...`
    pub(super) fn ai(&self, words: &[&str]) {
        let Some(ai) = self.directory.find("use", "ai") else {
            return self.print(format_args!("ai: this shell has no AI runtime\r\n"));
        };
        match words {
            ["model", url, model, options @ ..] => self.ai_model(ai, url, model, options),
            ["ask", question @ ..] if !question.is_empty() => self.ai_ask(ai, question),
            ["activity"] => self.ai_activity(ai),
            _ => self.write(USAGE.as_bytes()),
        }
    }

    /// `ai model URL MODEL [--dns SERVER] [--ca PATH]` (ADR-0054). The AI
    /// service has no filesystem: the shell reads the CA file and hands
    /// the service a read-only copy for this server only.
    fn ai_model(&self, ai: Handle, url: &str, model: &str, options: &[&str]) {
        let mut text = Buffer::<{ LINE_MAX + 2 }>::new();
        let _ = write!(text, "{url} {model}");
        let mut ca = None;
        let mut rest = options;
        loop {
            match rest {
                [] => break,
                ["--dns", server, more @ ..] => {
                    let _ = write!(text, " --dns {server}");
                    rest = more;
                }
                ["--ca", path, more @ ..] if ca.is_none() => {
                    match self.load_file(path) {
                        Ok(memory) => ca = Some((memory, *path)),
                        Err(problem) => {
                            return self.print(format_args!("ai: {path}: {problem}\r\n"));
                        }
                    }
                    rest = more;
                }
                _ => {
                    if let Some((memory, _)) = ca {
                        let _ = oceans_rt::close(memory);
                    }
                    return self.write(USAGE.as_bytes());
                }
            }
        }
        // Read-only, and nothing more: the service can only look at it.
        let shared = match ca {
            Some((memory, path)) => {
                let shared = oceans_rt::duplicate(
                    memory,
                    oceans_rt::rights::READ | oceans_rt::rights::MAP | oceans_rt::rights::TRANSFER,
                );
                let _ = oceans_rt::close(memory);
                match shared {
                    Ok(shared) => Some(shared),
                    Err(error) => return self.print(format_args!("ai: {path}: {error:?}\r\n")),
                }
            }
            None => None,
        };
        let handles: &[Handle] = match &shared {
            Some(handle) => core::slice::from_ref(handle),
            None => &[],
        };
        let mut reply = [0u8; 256];
        match oceans_rt::ipc_call_msg(
            ai,
            op::CONFIGURE,
            text.as_bytes(),
            handles,
            &mut reply,
            &mut [],
        ) {
            Ok(got) if got.label == status::DONE => {
                self.print(format_args!("ai: using {model} at {url}\r\n"));
            }
            Ok(got) => self.print(format_args!(
                "ai: {}\r\n",
                core::str::from_utf8(&reply[..got.data_len]).unwrap_or("refused")
            )),
            Err(error) => {
                if let Some(handle) = shared {
                    let _ = oceans_rt::close(handle);
                }
                self.print(format_args!("ai: {error:?}\r\n"));
            }
        }
    }

    fn ai_ask(&self, ai: Handle, question: &[&str]) {
        let mut prompt = Buffer::<{ LINE_MAX + 2 }>::new();
        for (i, word) in question.iter().enumerate() {
            let _ = write!(prompt, "{}{word}", if i > 0 { " " } else { "" });
        }
        // The session's authority: query and run apps, nothing more.
        let delegated = self.directory.find("use", "core").and_then(|core| {
            let mut reply = [0u8; 8];
            Core(core)
                .call(
                    core_op::MINT,
                    &[access::QUERY | access::RUN],
                    &[],
                    &mut reply,
                )
                .ok()
                .and_then(|got| got.handle)
        });
        // And the user's folder, read-only (ADR-0055): the agent may ask to
        // read in it, nothing else. Sent after the Core capability only.
        let files = delegated.and(
            self.fs_root()
                .and_then(|root| root.open("home", 0).ok())
                .map(|(node, _)| node.0),
        );
        let mut sent = [Handle(0); 2];
        let mut count = 0;
        for handle in [delegated, files].into_iter().flatten() {
            sent[count] = handle;
            count += 1;
        }
        let handles = &sent[..count];
        let mut reply = [0u8; 256];
        let mut handles_back = [Handle(0); 1];
        let mut got = oceans_rt::ipc_call_msg(
            ai,
            op::ASK,
            prompt.as_bytes(),
            handles,
            &mut reply,
            &mut handles_back,
        );
        for _ in 0..=MAX_APPROVALS {
            let received = match got {
                Ok(received) => received,
                Err(error) => return self.print(format_args!("ai: {error:?}\r\n")),
            };
            if received.data_len < 8 {
                return self.print(format_args!("ai: the request was refused\r\n"));
            }
            let session = u32::from_le_bytes(reply[..4].try_into().unwrap());
            let mut text = Buffer::<1024>::new();
            self.ai_text(ai, session, &reply[..received.data_len], &mut text);
            match received.label {
                status::DONE => {
                    return self.print(format_args!("Oceans AI: {}\r\n", text.as_str()));
                }
                status::FAILED => return self.print(format_args!("ai: {}\r\n", text.as_str())),
                status::NEEDS_APPROVAL => {
                    // The question's words come from the tool, not the model.
                    self.print(format_args!("Oceans AI wants to: {}\r\n", text.as_str()));
                    let approve = self.ask("Allow? [y/N] ").unwrap_or(false);
                    let mut answer = [0u8; 5];
                    answer[..4].copy_from_slice(&session.to_le_bytes());
                    answer[4] = u8::from(approve);
                    got = oceans_rt::ipc_call_msg(
                        ai,
                        op::CONTINUE,
                        &answer,
                        &[],
                        &mut reply,
                        &mut handles_back,
                    );
                }
                _ => return self.print(format_args!("ai: the request was refused\r\n")),
            }
        }
        self.print(format_args!("ai: too many approvals asked; stopped\r\n"));
    }

    /// The full text of a reply: its first part, then `TEXT` for the rest.
    fn ai_text(&self, ai: Handle, session: u32, reply: &[u8], text: &mut Buffer<1024>) {
        let total = u32::from_le_bytes(reply[4..8].try_into().unwrap()) as usize;
        let _ = text.write_str(core::str::from_utf8(&reply[8..]).unwrap_or("?"));
        let mut offset = reply.len() - 8;
        let mut part = [0u8; 256];
        while offset < total {
            let mut request = [0u8; 8];
            request[..4].copy_from_slice(&session.to_le_bytes());
            request[4..].copy_from_slice(&(offset as u32).to_le_bytes());
            match oceans_rt::ipc_call(ai, op::TEXT, &request, &mut part) {
                Ok((len, status::DONE)) if len > 0 => {
                    if text
                        .write_str(core::str::from_utf8(&part[..len]).unwrap_or("?"))
                        .is_err()
                    {
                        return;
                    }
                    offset += len;
                }
                _ => return,
            }
        }
    }

    fn ai_activity(&self, ai: Handle) {
        let mut count = 0u32;
        let mut entry = [0u8; 256];
        while let Ok((_, status::DONE)) =
            oceans_rt::ipc_call(ai, op::ACTIVITY, &count.to_le_bytes(), &mut entry)
        {
            count += 1;
        }
        if count == 0 {
            return self.print(format_args!("ai: no activity yet\r\n"));
        }
        for index in (0..count).rev() {
            if let Ok((len, status::DONE)) =
                oceans_rt::ipc_call(ai, op::ACTIVITY, &index.to_le_bytes(), &mut entry)
            {
                self.print(format_args!(
                    "  {}\r\n",
                    core::str::from_utf8(&entry[..len]).unwrap_or("?")
                ));
            }
        }
    }
}
