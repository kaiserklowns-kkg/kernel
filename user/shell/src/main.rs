//! The Oceans shell (ADR-0018): an interactive command line on the console.
//!
//! A capability-explicit shell: it can only use what init granted it (see
//! `grants`), and `run` gives a program **only the authority listed on the
//! command line**: `run hello-client log use:echo`.
//!
//! The shell does its own terminal handling (the kernel delivers raw bytes,
//! ADR-0017): echo, backspace, Ctrl-C, Ctrl-U, CR/LF, and it ignores ANSI
//! escape sequences such as arrow keys.
//!
//! Handles: whatever `services.conf` grants (at least `log` and `console`),
//! plus init's handle directory as the last handle.

#![no_std]
#![no_main]

mod ai;
mod apps;
mod ui;

use core::cell::RefCell;
use core::fmt::{self, Write};

use oceans_elf::{Executable, Limits};
use oceans_fs_proto::tree::{self, Copier, CopyError, Side, Totals};
use oceans_fs_proto::{FsError, Kind, MAX_NAME, Node, Shared, Status, flags};
use oceans_rt::{Buffer, Directory, Error, Handle, Start, prot, rights};

oceans_rt::entry!(main);

const PROMPT: &[u8] = b"oceans> ";
const LINE_MAX: usize = 200;
const MAX_ARGS: usize = 16;
const MAX_CHILD_HANDLES: usize = 8;

/// Capabilities a bare command receives automatically when its manifest
/// requests them: console output and read-only system information. Input,
/// endpoints, files and the log always need an explicit `run`.
const AUTOMATIC_GRANTS: [&str; 2] = ["out", "sysinfo"];

const CTRL_C: u8 = 0x03;
const CTRL_U: u8 = 0x15;
const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7f;
const ESCAPE: u8 = 0x1b;

struct Shell {
    log: Handle,
    console: Handle,
    /// What the shell holds (`grants`), from init.
    directory: Directory,
    /// Console input typed ahead, shared by the command line and questions
    /// asked while a command runs (consent, ADR-0047).
    input: RefCell<Input>,
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return 2;
    };
    let (Some(log), Some(console)) = (
        directory.find("log", "log"),
        directory.find("console", "console"),
    ) else {
        return 3;
    };
    let shell = Shell {
        log,
        console,
        directory,
        input: RefCell::new(Input::new()),
    };
    let _ = oceans_rt::debug_write(log, "shell: ready");
    shell.print(format_args!("Oceans shell. Type `help` for commands.\r\n"));
    shell.run()
}

/// The program's `.oceans.manifest` (requested capabilities), if any.
fn requested_grants(image: Handle) -> Option<Buffer<256>> {
    let size = usize::try_from(oceans_rt::memory_size(image).ok()?).ok()?;
    let base = oceans_rt::memory_map(image, 0, prot::READ).ok()?;
    // SAFETY: the whole object is mapped readable at `base` until unmapped
    // below.
    let bytes = unsafe { core::slice::from_raw_parts(base, size) };
    let limits = Limits {
        lowest: 0x1_0000,
        highest: 0x0000_8000_0000_0000,
        max_total: u64::MAX,
    };
    let manifest = Executable::parse(bytes, limits)
        .ok()
        .and_then(|executable| executable.section(".oceans.manifest"))
        .and_then(|section| core::str::from_utf8(section).ok())
        .map(|text| {
            let mut copy = Buffer::new();
            let _ = copy.write_str(text);
            copy
        });
    let _ = oceans_rt::memory_unmap(base);
    manifest
}

impl Shell {
    fn write(&self, bytes: &[u8]) {
        let _ = oceans_rt::console_write(self.console, bytes);
    }

    /// Formats into a 512-byte buffer; longer output is cut off at the
    /// buffer boundary rather than lost (write static text with `write`).
    fn print(&self, args: fmt::Arguments<'_>) {
        let mut text = Buffer::<512>::new();
        let _ = text.write_fmt(args);
        self.write(text.as_bytes());
    }

    fn run(&self) -> i64 {
        let mut line = [0u8; LINE_MAX];
        loop {
            self.write(PROMPT);
            let len = self.read_line(&mut self.input.borrow_mut(), &mut line);
            let Some(len) = len else {
                return 4; // console unreadable
            };
            let Ok(text) = core::str::from_utf8(&line[..len]) else {
                self.print(format_args!("error: input is not UTF-8\r\n"));
                continue;
            };
            if let Some(code) = self.execute(text.trim()) {
                return code;
            }
        }
    }

    /// Asks a yes/no question on the console; `None` if it cannot be read.
    /// Only `y` or `yes` is yes.
    fn ask(&self, question: &str) -> Option<bool> {
        self.write(question.as_bytes());
        let mut line = [0u8; LINE_MAX];
        let len = self.read_line(&mut self.input.borrow_mut(), &mut line)?;
        let answer = core::str::from_utf8(&line[..len]).unwrap_or("").trim();
        Some(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
    }

    /// Reads one edited line. `None` if the console cannot be read. Bytes
    /// typed ahead (after Enter) stay in `input` for the next line.
    fn read_line(&self, input: &mut Input, line: &mut [u8; LINE_MAX]) -> Option<usize> {
        let mut len = 0;
        let mut escape = false;
        loop {
            let byte = input.next(self.console)?;
            // Swallow ANSI escape sequences (ESC [ ... final byte).
            if escape {
                if (0x40..=0x7e).contains(&byte) && byte != b'[' {
                    escape = false;
                }
                continue;
            }
            // Enter is CR, LF or CR LF: a LF right after a CR is the same key.
            let after_cr = core::mem::replace(&mut input.after_cr, byte == b'\r');
            match byte {
                b'\n' if after_cr => {}
                b'\r' | b'\n' => {
                    self.write(b"\r\n");
                    return Some(len);
                }
                BACKSPACE | DELETE => {
                    if len > 0 {
                        len -= 1;
                        self.write(b"\x08 \x08");
                    }
                }
                CTRL_C => {
                    self.write(b"^C\r\n");
                    len = 0;
                    self.write(PROMPT);
                }
                CTRL_U => {
                    for _ in 0..len {
                        self.write(b"\x08 \x08");
                    }
                    len = 0;
                }
                ESCAPE => escape = true,
                0x20..=0x7e if len < LINE_MAX => {
                    line[len] = byte;
                    len += 1;
                    self.write(&[byte]);
                }
                _ => {} // other control bytes and overflow
            }
        }
    }

    /// Runs one command line. `Some(code)` ends the shell.
    fn execute(&self, line: &str) -> Option<i64> {
        let mut args = [""; MAX_ARGS];
        let mut count = 0;
        for word in line.split_whitespace() {
            if count == MAX_ARGS {
                self.print(format_args!("error: more than {MAX_ARGS} words\r\n"));
                return None;
            }
            args[count] = word;
            count += 1;
        }
        let args = &args[..count];
        match args {
            [] => {}
            ["help"] => self.help(),
            ["echo", words @ ..] => {
                // One write: a log line cannot land inside it.
                let mut text = Buffer::<{ LINE_MAX + 2 }>::new();
                for (i, word) in words.iter().enumerate() {
                    let _ = write!(text, "{}{word}", if i > 0 { " " } else { "" });
                }
                let _ = text.write_str("\r\n");
                self.write(text.as_bytes());
            }
            ["grants"] => {
                for line in self.directory.lines() {
                    self.print(format_args!("  {line}\r\n"));
                }
            }
            ["call", endpoint, words @ ..] => self.call(endpoint, words),
            ["run", program, grants @ ..] => self.run_program(program, grants),
            ["ls"] => self.list(""),
            ["ls", path] => self.list(path),
            ["cat", path] => self.cat(path),
            ["write", path, words @ ..] => self.write_file(path, words),
            ["mkdir", path] => self.make_directory(path),
            ["rm", path] => self.remove(path),
            ["rm", "-r", path] => self.remove_all(path),
            ["mv", old, new] => self.rename(old, new),
            ["cp", source, destination] => self.copy(source, destination, false),
            ["cp", "-r", source, destination] => self.copy(source, destination, true),
            ["sync"] => self.sync(),
            ["shutdown"] => self.power(oceans_rt::power::OFF),
            ["reboot"] => self.power(oceans_rt::power::RESTART),
            ["app", words @ ..] => self.app(words),
            ["volume", words @ ..] => self.volume(words),
            ["ai", words @ ..] => self.ai(words),
            ["ui", words @ ..] => self.ui(words),
            ["clear"] => self.write(b"\x1b[2J\x1b[H"),
            ["exit"] => return Some(0),
            // Anything else is a program in /bin, run with what its manifest
            // requests (low-risk grants only).
            [command, args @ ..] => self.run_command(command, args),
        }
        None
    }

    fn help(&self) {
        // Written directly: it is longer than the formatting buffer.
        self.write(
            b"commands:\r\n\
             \x20 help                       this list\r\n\
             \x20 echo TEXT                  print TEXT\r\n\
             \x20 grants                     capabilities this shell holds\r\n\
             \x20 call ENDPOINT TEXT         send TEXT to a service endpoint I use\r\n\
             \x20 run PROGRAM [GRANT...]     run a program with only the listed authority:\r\n\
             \x20                              log, console, out, sysinfo, devices, use:ENDPOINT,\r\n\
             \x20                              core:RIGHTS (query+run+manage+decide+audit)\r\n\
             \x20                              (PROGRAM: a granted module, /bin/NAME, or a path;\r\n\
             \x20                              arguments after `--`)\r\n\
             \x20 PROGRAM [ARGS...]          run /bin/PROGRAM with what its manifest requests\r\n\
             \x20                              (granted automatically: out, sysinfo only),\r\n\
             \x20                              e.g. ps, mem, uptime, uname; `run lspci out devices`\r\n\
             \x20 ls [PATH]                  list a directory\r\n\
             \x20 cat PATH                   print a file\r\n\
             \x20 write PATH TEXT            replace a file's contents with TEXT\r\n\
             \x20 mkdir PATH                 create a directory\r\n\
             \x20 rm [-r] PATH               remove a file or empty directory\r\n\
             \x20                              (-r: a directory and everything in it)\r\n\
             \x20 mv OLD NEW                 rename or move a file or directory\r\n\
             \x20                              (between filesystems: copy, sync, remove)\r\n\
             \x20 cp [-r] FROM TO            copy a file (-r: a directory), also between\r\n\
             \x20                              filesystems; into TO if it is a directory\r\n\
             \x20 sync                       make every file change durable on disk now\r\n\
             \x20 app list | info ID          installed apps and what they may do\r\n\
             \x20 app install PATH           install or update a signed package (.opk)\r\n\
             \x20 app run | start ID [ARGS]  run an app (start: in the background);\r\n\
             \x20                              asks before granting a permission\r\n\
             \x20 app stop | remove ID       stop or uninstall an app\r\n\
             \x20 app enable | disable ID    a service: start at boot (and now), or not\r\n\
             \x20 app grant | revoke ID PERM allow or withdraw a permission\r\n\
             \x20 app reset ID PERM          forget the decision: ask again next time\r\n\
             \x20 app rollback ID | audit    previous version; what was decided\r\n\
             \x20 ai ask QUESTION            ask Oceans AI (it asks before changing anything)\r\n\
             \x20 ai model URL MODEL         its model server (OpenAI-compatible, http or\r\n\
             \x20   [--dns SERVER] [--ca PATH] https); a DNS server, a CA to trust for it\r\n\
             \x20 ai activity                what the AI did, and what you decided\r\n\
             \x20 ui pair | unpair | status  let a browser use the Oceans web experience\r\n\
             \x20                              (it may list, start and stop apps, never decide)\r\n\
             \x20 volume [0-100 | mute | unmute]  the system volume (kept across reboots)\r\n\
             \x20 shutdown | reboot          stop the system, then switch off or restart\r\n\
             \x20 clear                      clear the screen\r\n\
             \x20 exit                       leave the shell\r\n"
        );
    }

    fn call(&self, endpoint: &str, words: &[&str]) {
        let Some(client) = self.directory.find("use", endpoint) else {
            self.print(format_args!(
                "call: this shell does not use an endpoint named {endpoint}\r\n"
            ));
            return;
        };
        let mut request = Buffer::<256>::new();
        for (i, word) in words.iter().enumerate() {
            let _ = write!(request, "{}{word}", if i > 0 { " " } else { "" });
        }
        let mut reply = [0u8; 256];
        match oceans_rt::ipc_call(client, 1, request.as_bytes(), &mut reply) {
            Ok((len, _)) => {
                self.write(&reply[..len]);
                self.write(b"\r\n");
            }
            Err(error) => self.print(format_args!("call: {endpoint}: {error:?}\r\n")),
        }
    }

    /// The filesystem root this shell was granted (`use = fs`). Never closed.
    fn fs_root(&self) -> Option<Node> {
        self.directory.find("use", "fs").map(Node)
    }

    /// A directory: the root for an empty path or `/`, else `path` opened
    /// from the root. The flag says whether the handle is ours to close.
    fn open_directory(&self, path: &str, open_flags: u8) -> Result<(Node, bool), &'static str> {
        let root = self.fs_root().ok_or("this shell has no filesystem")?;
        if path.trim_matches('/').is_empty() {
            return Ok((root, false));
        }
        match root.walk(path, open_flags).map_err(FsError::message)? {
            (node, Kind::Directory) => Ok((node, true)),
            (node, Kind::File) => {
                node.close();
                Err("not a directory")
            }
        }
    }

    /// Runs `f` on the parent directory of `path` (opened with write access,
    /// which the fs grants only if our handle to it allows) and the last
    /// component.
    fn in_parent(
        &self,
        path: &str,
        f: impl FnOnce(&Node, &str) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        let path = path.trim_end_matches('/');
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let (directory, owned) = self.open_directory(parent, flags::WRITE)?;
        let result = f(&directory, name);
        if owned {
            directory.close();
        }
        result
    }

    fn list(&self, path: &str) {
        let (directory, owned) = match self.open_directory(path, 0) {
            Ok(found) => found,
            Err(problem) => return self.print(format_args!("ls: {path}: {problem}\r\n")),
        };
        let mut name = [0u8; MAX_NAME];
        for index in 0.. {
            match directory.entry(index, &mut name) {
                Ok(Some((kind, len))) => self.print(format_args!(
                    "  {}{}\r\n",
                    core::str::from_utf8(&name[..len]).unwrap_or("?"),
                    if kind == Kind::Directory { "/" } else { "" }
                )),
                Ok(None) => break,
                Err(error) => {
                    self.print(format_args!("ls: {path}: {}\r\n", error.message()));
                    break;
                }
            }
        }
        if owned {
            directory.close();
        }
    }

    fn cat(&self, path: &str) {
        let Some(root) = self.fs_root() else {
            return self.print(format_args!("cat: this shell has no filesystem\r\n"));
        };
        let file = match root.walk(path, 0) {
            Ok((file, Kind::File)) => file,
            Ok((node, Kind::Directory)) => {
                node.close();
                return self.print(format_args!("cat: {path}: is a directory\r\n"));
            }
            Err(error) => return self.print(format_args!("cat: {path}: {}\r\n", error.message())),
        };
        let shared = file.attach(CAT_BUFFER).ok();
        let mut offset = 0;
        let mut chunk = [0u8; CAT_BUFFER];
        loop {
            match read_at(&file, shared.as_ref(), offset, &mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    // Files use LF; the terminal needs CR LF. One write per
                    // chunk, so a log line cannot split a line of text.
                    let mut out = [0u8; 2 * CAT_BUFFER];
                    let mut len = 0;
                    for &byte in &chunk[..n] {
                        if byte == b'\n' {
                            out[len] = b'\r';
                            len += 1;
                        }
                        out[len] = byte;
                        len += 1;
                    }
                    self.write(&out[..len]);
                    offset += n as u64;
                }
                Err(error) => {
                    self.print(format_args!("cat: {path}: {}\r\n", error.message()));
                    break;
                }
            }
        }
        file.close();
    }

    fn write_file(&self, path: &str, words: &[&str]) {
        let mut text = Buffer::<{ LINE_MAX + 1 }>::new();
        for (i, word) in words.iter().enumerate() {
            let _ = write!(text, "{}{word}", if i > 0 { " " } else { "" });
        }
        let _ = text.write_str("\n");
        let result = self.in_parent(path, |directory, name| {
            let (file, kind) = directory
                .open(name, flags::CREATE_FILE | flags::WRITE)
                .map_err(FsError::message)?;
            let written = if kind == Kind::File {
                file.truncate(0)
                    .and_then(|()| file.write_all(0, text.as_bytes()))
                    .map_err(FsError::message)
            } else {
                Err("is a directory")
            };
            file.close();
            written
        });
        if let Err(problem) = result {
            self.print(format_args!("write: {path}: {problem}\r\n"));
        }
    }

    fn make_directory(&self, path: &str) {
        let result = self.in_parent(path, |directory, name| {
            if let Ok((existing, _)) = directory.open(name, 0) {
                existing.close();
                return Err("already exists");
            }
            let (created, _) = directory
                .open(name, flags::CREATE_DIRECTORY)
                .map_err(FsError::message)?;
            created.close();
            Ok(())
        });
        if let Err(problem) = result {
            self.print(format_args!("mkdir: {path}: {problem}\r\n"));
        }
    }

    /// Commits the filesystem now (contents are otherwise durable when the
    /// handle that wrote them closes, ADR-0022).
    fn sync(&self) {
        let result = self
            .fs_root()
            .ok_or("this shell has no filesystem")
            .and_then(|root| root.sync().map_err(FsError::message));
        if let Err(problem) = result {
            self.print(format_args!("sync: {problem}\r\n"));
        }
    }

    /// `shutdown` and `reboot` (ADR-0085): init stops the system (this
    /// shell too) and the machine switches off or restarts.
    fn power(&self, action: u64) {
        let Some(power) = self.directory.find("power", "power") else {
            return self.print(format_args!(
                "this shell may not switch the machine off\r\n"
            ));
        };
        let what = if action == oceans_rt::power::OFF {
            "switching off"
        } else {
            "restarting"
        };
        // Said first: once init accepts, it stops this shell at once.
        self.print(format_args!("Stopping the system, then {what}...\r\n"));
        match oceans_rt::request_power(power, action) {
            Ok(()) => {
                // init stops this shell with everything else.
                loop {
                    oceans_rt::sleep_ms(60_000);
                }
            }
            Err(oceans_rt::Error::NotFound) => self.print(format_args!(
                "this machine cannot be switched off by Oceans: turn it off yourself\r\n"
            )),
            Err(error) => self.print(format_args!("cannot ask init: {error:?}\r\n")),
        }
    }

    /// `mv OLD NEW` (ADR-0038): paths from the root. Between filesystems
    /// (ADR-0039) the source is copied, the copy made durable, and only
    /// then is the source removed: a failure on the way leaves the source
    /// as it was.
    fn rename(&self, old: &str, new: &str) {
        let Some(root) = self.fs_root() else {
            return self.print(format_args!("mv: this shell has no filesystem\r\n"));
        };
        match root.rename(old, new) {
            Ok(()) => {}
            Err(FsError::Status(Status::CrossDevice)) => {
                if let Err(failed) = self.copy_tree(old, new, true, true) {
                    return self.report("mv", old, new, failed);
                }
                let removed = self.in_parent(old, |directory, name| {
                    tree::remove_tree(directory, name)
                        .and_then(|()| directory.sync())
                        .map_err(FsError::message)
                });
                if let Err(problem) = removed {
                    self.print(format_args!(
                        "mv: {old}: copied, but not removed: {problem}\r\n"
                    ));
                }
            }
            Err(error) => self.print(format_args!("mv: {old}: {}\r\n", error.message())),
        }
    }

    /// `cp [-r] FROM TO` (ADR-0039): copies a file, or with `-r` a
    /// directory and everything in it, anywhere (across filesystems too).
    /// When `TO` is a directory the copy goes into it under its own name.
    /// The data moves through one buffer shared by both filesystems, and
    /// the copy is durable when the summary is printed.
    fn copy(&self, source: &str, destination: &str, recursive: bool) {
        let started = oceans_rt::clock_ms();
        match self.copy_tree(source, destination, recursive, false) {
            Ok(totals) => self.print(format_args!(
                "cp: {} bytes in {} file{}, {} ms\r\n",
                totals.bytes,
                totals.files,
                if totals.files == 1 { "" } else { "s" },
                oceans_rt::clock_ms().saturating_sub(started)
            )),
            Err(failed) => self.report("cp", source, destination, failed),
        }
    }

    fn report(&self, command: &str, source: &str, destination: &str, failed: Failed) {
        let (path, problem) = match failed {
            Failed::Source(problem) => (source, problem),
            Failed::Destination(problem) => (destination, problem),
        };
        self.print(format_args!("{command}: {path}: {problem}\r\n"));
    }

    /// Copies `source` to `destination`; `exact` (a move) takes the
    /// destination as the new path even if it is a directory, which then
    /// must be empty, as for a rename.
    fn copy_tree(
        &self,
        source: &str,
        destination: &str,
        recursive: bool,
        exact: bool,
    ) -> Result<Totals, Failed> {
        let root = self
            .fs_root()
            .ok_or(Failed::Source("this shell has no filesystem"))?;
        let from_path = normal(source).ok_or(Failed::Source("path too long"))?;
        if from_path.as_str().is_empty() {
            return Err(Failed::Source("cannot copy the root"));
        }
        let (from, kind) = root
            .walk(source, 0)
            .map_err(|e| Failed::Source(e.message()))?;
        let copied = if kind == Kind::Directory && !recursive {
            Err(Failed::Source("is a directory (cp -r copies directories)"))
        } else {
            self.copy_to(&from, kind, from_path.as_str(), destination, exact)
        };
        from.close();
        copied
    }

    fn copy_to(
        &self,
        from: &Node,
        kind: Kind,
        from_path: &str,
        destination: &str,
        exact: bool,
    ) -> Result<Totals, Failed> {
        let too_long = Failed::Destination("path too long");
        let mut target = normal(destination).ok_or(too_long)?;
        if !exact && let Ok((directory, owned)) = self.open_directory(destination, 0) {
            if owned {
                directory.close();
            }
            let name = from_path.rsplit('/').next().unwrap_or(from_path);
            write!(target, "/{name}").map_err(|_| Failed::Destination("path too long"))?;
        }
        let target = target.as_str();
        if target == from_path {
            return Err(Failed::Destination("is the same file"));
        }
        // A directory copied below itself would never end.
        if kind == Kind::Directory
            && target.starts_with(from_path)
            && target.as_bytes().get(from_path.len()) == Some(&b'/')
        {
            return Err(Failed::Destination("is inside the source"));
        }
        let Some((parent, name)) = target.rsplit_once('/').filter(|(_, name)| !name.is_empty())
        else {
            return Err(Failed::Destination("is the root"));
        };
        let (directory, owned) = self
            .open_directory(parent, flags::WRITE)
            .map_err(Failed::Destination)?;
        let copied = copy_into(from, kind, &directory, name, exact);
        if owned {
            directory.close();
        }
        copied
    }

    /// `rm -r PATH`: removes a directory and everything in it (or a file).
    fn remove_all(&self, path: &str) {
        if path.trim_matches('/').is_empty() {
            return self.print(format_args!("rm: {path}: cannot remove the root\r\n"));
        }
        let result = self.in_parent(path, |directory, name| {
            tree::remove_tree(directory, name).map_err(FsError::message)
        });
        if let Err(problem) = result {
            self.print(format_args!("rm: {path}: {problem}\r\n"));
        }
    }

    fn remove(&self, path: &str) {
        let result = self.in_parent(path, |directory, name| {
            directory.remove(name).map_err(FsError::message)
        });
        if let Err(problem) = result {
            self.print(format_args!("rm: {path}: {problem}\r\n"));
        }
    }

    /// The image of `program`: a boot module this shell was granted, else a
    /// file: `program` itself if it contains `/`, else `/bin/<program>`.
    /// Returns the memory object and whether it is ours to close.
    fn find_image(&self, program: &str) -> Result<(Handle, bool), &'static str> {
        if !program.contains('/')
            && let Some(module) = self.directory.find("module", program)
        {
            return Ok((module, false));
        }
        let mut path = Buffer::<{ 8 + LINE_MAX }>::new();
        if program.contains('/') {
            let _ = path.write_str(program);
        } else {
            let _ = write!(path, "/bin/{program}");
        }
        self.load_file(path.as_str()).map(|memory| (memory, true))
    }

    /// Copies a file into a new memory object (for `PROCESS_SPAWN`).
    fn load_file(&self, path: &str) -> Result<Handle, &'static str> {
        let root = self
            .fs_root()
            .ok_or("no program by that name (and no filesystem)")?;
        let (file, kind) = root.walk(path, 0).map_err(FsError::message)?;
        let result = (|| {
            if kind != Kind::File {
                return Err("not a file");
            }
            let size = file.stat().map_err(FsError::message)?.size;
            let memory = oceans_rt::memory_create(size.max(1)).map_err(|_| "out of memory")?;
            let copied = (|| {
                let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE)
                    .map_err(|_| "out of memory")?;
                // SAFETY: just mapped `size` writable bytes (rounded up).
                let target = unsafe { core::slice::from_raw_parts_mut(base, size as usize) };
                let shared = file.attach(LOAD_BUFFER).ok();
                let mut done = 0;
                let mut outcome = Ok(());
                while done < target.len() {
                    match read_at(&file, shared.as_ref(), done as u64, &mut target[done..]) {
                        Ok(0) => break,
                        Ok(n) => done += n,
                        Err(error) => {
                            outcome = Err(error.message());
                            break;
                        }
                    }
                }
                // Unmapped on every path: the object is only handed on.
                let _ = oceans_rt::memory_unmap(base);
                outcome
            })();
            match copied {
                Ok(()) => Ok(memory),
                Err(problem) => {
                    let _ = oceans_rt::close(memory);
                    Err(problem)
                }
            }
        })();
        file.close();
        result
    }

    /// `run PROGRAM [GRANT...] [-- ARGS...]`: explicit authority.
    fn run_program(&self, program: &str, words: &[&str]) {
        let split = words.iter().position(|&w| w == "--");
        let (grants, args) = match split {
            Some(at) => (&words[..at], &words[at + 1..]),
            None => (words, &[][..]),
        };
        let (image, owned) = match self.find_image(program) {
            Ok(found) => found,
            Err(problem) => {
                self.print(format_args!("run: {program}: {problem}\r\n"));
                return;
            }
        };
        let name = program.rsplit('/').next().unwrap_or(program);
        self.spawn_and_wait(image, name, grants, args);
        if owned {
            let _ = oceans_rt::close(image);
        }
    }

    /// A bare command: `/bin/NAME` (or a granted module) run with the
    /// capabilities its manifest requests, if all of them are low-risk
    /// ([`AUTOMATIC_GRANTS`]); anything else needs an explicit `run`.
    fn run_command(&self, command: &str, args: &[&str]) {
        let (image, owned) = match self.find_image(command) {
            Ok(found) => found,
            Err(_) => {
                self.print(format_args!("{command}: unknown command (try `help`)\r\n"));
                return;
            }
        };
        let manifest = requested_grants(image);
        let mut grants = [""; MAX_CHILD_HANDLES];
        let mut count = 0;
        let mut refusal = None;
        match &manifest {
            None => refusal = Some("has no manifest"),
            Some(manifest) => {
                for line in manifest.as_str().lines() {
                    let Some(grant) = line.trim().strip_prefix("grant ") else {
                        continue;
                    };
                    let grant = grant.trim();
                    if !AUTOMATIC_GRANTS.contains(&grant) {
                        self.print(format_args!(
                            "{command}: requests `{grant}`; grant it explicitly with `run {command} {grant} ...`\r\n"
                        ));
                        refusal = Some("");
                        break;
                    }
                    if count < MAX_CHILD_HANDLES {
                        grants[count] = grant;
                        count += 1;
                    }
                }
            }
        }
        match refusal {
            Some("") => {}
            Some(problem) => self.print(format_args!(
                "{command}: {problem}; use `run {command} GRANT...`\r\n"
            )),
            _ => self.spawn_and_wait(image, command, &grants[..count], args),
        }
        if owned {
            let _ = oceans_rt::close(image);
        }
    }

    fn spawn_and_wait(&self, image: Handle, program: &str, grants: &[&str], args: &[&str]) {
        // The granted capabilities, an argument object if any, and a
        // directory describing them all as the last handle.
        let mut handles = [Handle(0); MAX_CHILD_HANDLES + 2];
        let mut directory = Buffer::<512>::new();
        let mut count = 0;
        for grant in grants {
            if count == MAX_CHILD_HANDLES {
                self.print(format_args!("run: too many grants\r\n"));
                return self.close_all(&handles[..count]);
            }
            match self.grant(grant) {
                Ok((handle, kind, name)) => {
                    handles[count] = handle;
                    let _ = writeln!(directory, "{count} {kind} {name}");
                    count += 1;
                }
                Err(problem) => {
                    self.print(format_args!("run: {grant}: {problem}\r\n"));
                    return self.close_all(&handles[..count]);
                }
            }
        }
        if !args.is_empty() {
            let mut text = Buffer::<{ LINE_MAX + 1 }>::new();
            for (i, arg) in args.iter().enumerate() {
                let _ = write!(text, "{}{arg}", if i > 0 { " " } else { "" });
            }
            match oceans_rt::publish_text(text.as_bytes()) {
                Ok(handle) => {
                    handles[count] = handle;
                    let _ = writeln!(directory, "{count} args args");
                    count += 1;
                }
                Err(error) => {
                    self.print(format_args!("run: arguments: {error:?}\r\n"));
                    return self.close_all(&handles[..count]);
                }
            }
        }
        let _ = writeln!(directory, "{count} directory handles");
        match oceans_rt::publish_text(directory.as_bytes()) {
            Ok(handle) => {
                handles[count] = handle;
                count += 1;
            }
            Err(error) => {
                self.print(format_args!("run: {error:?}\r\n"));
                return self.close_all(&handles[..count]);
            }
        }
        let process = match oceans_rt::process_spawn_named(image, 0, &handles[..count], 0, program)
        {
            Ok(process) => process,
            Err(error) => {
                self.print(format_args!("run: {program}: {error:?}\r\n"));
                return self.close_all(&handles[..count]);
            }
        };
        let code = oceans_rt::process_wait(process);
        let _ = oceans_rt::close(process);
        match code {
            // Silent success for commands; the result is their output.
            Ok(0) if grants.contains(&"out") || grants.contains(&"console") => {}
            // CPU exception vectors map to -128 - vector (ADR-0014).
            Ok(code) if code <= -128 => self.print(format_args!(
                "{program} was killed by CPU exception {} (exit {code})\r\n",
                -128 - code
            )),
            Ok(code) => self.print(format_args!("{program} exited with {code}\r\n")),
            Err(error) => self.print(format_args!("run: {program}: {error:?}\r\n")),
        }
    }

    /// A capability for a child, narrowed to what the grant names, with its
    /// directory kind and name.
    fn grant(&self, grant: &str) -> Result<(Handle, &'static str, &'static str), &'static str> {
        let (source, granted_rights, kind, name) = match grant {
            "log" => (
                Some(self.log),
                rights::WRITE | rights::TRANSFER,
                "log",
                "log",
            ),
            "console" => (
                Some(self.console),
                rights::READ | rights::WRITE | rights::TRANSFER,
                "console",
                "console",
            ),
            // Console output only: the program cannot read keystrokes.
            "out" => (
                Some(self.console),
                rights::WRITE | rights::TRANSFER,
                "console",
                "out",
            ),
            "sysinfo" => (
                self.directory.find("sysinfo", "sysinfo"),
                rights::READ | rights::TRANSFER,
                "sysinfo",
                "sysinfo",
            ),
            // The kept log (ADR-0070): read-only.
            "logs" => (
                self.directory.find("logs", "logs"),
                rights::READ | rights::TRANSFER,
                "logs",
                "logs",
            ),
            // The PCI device list (ADR-0021): read-only, no device access.
            "devices" => (
                self.directory.find("devices", "devices"),
                rights::READ | rights::TRANSFER,
                "devices",
                "devices",
            ),
            // A narrower Oceans Core capability (ADR-0048), minted for the
            // program: e.g. `core:query+run`.
            _ if grant.starts_with("core:") => {
                let wanted = oceans_core_proto::access::parse(&grant["core:".len()..])
                    .ok_or("core rights are query, run, manage, decide, audit (joined by +)")?;
                let core = self
                    .directory
                    .find("use", "core")
                    .ok_or("this shell does not hold it")?;
                let mut reply = [0u8; 8];
                let minted = oceans_core_proto::Core(core)
                    .call(oceans_core_proto::op::MINT, &[wanted], &[], &mut reply)
                    .map_err(|(error, _)| error.message())?;
                return minted
                    .handle
                    .map(|handle| (handle, "use", "core"))
                    .ok_or("no capability came back");
            }
            _ => match grant.strip_prefix("use:") {
                Some(endpoint) => (
                    self.directory.find("use", endpoint),
                    rights::SEND | rights::TRANSFER,
                    "use",
                    // The directory entry needs a 'static name: look it up
                    // in our own directory text, which lives forever.
                    self.directory.name("use", endpoint).unwrap_or("endpoint"),
                ),
                None => {
                    return Err(
                        "unknown grant (log, logs, console, out, sysinfo, devices, use:ENDPOINT, core:RIGHTS)",
                    );
                }
            },
        };
        let source = source.ok_or("this shell does not hold it")?;
        let handle = oceans_rt::duplicate(source, granted_rights).map_err(|error| match error {
            Error::MissingRights => "this shell may not pass it on",
            _ => "cannot duplicate it",
        })?;
        Ok((handle, kind, name))
    }

    fn close_all(&self, handles: &[Handle]) {
        for &handle in handles {
            let _ = oceans_rt::close(handle);
        }
    }
}

/// Console input read in chunks, handed out byte by byte, so bytes typed
/// ahead of the current line are never lost.
struct Input {
    buffer: [u8; 64],
    position: usize,
    len: usize,
    /// The previous byte was CR (to treat CR LF as one Enter).
    after_cr: bool,
}

impl Input {
    const fn new() -> Self {
        Self {
            buffer: [0; 64],
            position: 0,
            len: 0,
            after_cr: false,
        }
    }

    fn next(&mut self, console: Handle) -> Option<u8> {
        if self.position == self.len {
            self.len = oceans_rt::console_read(console, &mut self.buffer).ok()?;
            self.position = 0;
        }
        let byte = *self.buffer.get(self.position)?;
        self.position += 1;
        Some(byte)
    }
}

/// Shared buffers (ADR-0030) for `cat` and for loading programs.
const CAT_BUFFER: usize = 4096;
/// Bytes a copy moves per request (ADR-0039).
const COPY_BUFFER: usize = 128 * 1024;

/// Which side of a copy or move failed, and why.
enum Failed {
    Source(&'static str),
    Destination(&'static str),
}

impl From<CopyError> for Failed {
    fn from(failed: CopyError) -> Self {
        match failed.side {
            Side::Source => Self::Source(failed.error.message()),
            Side::Destination => Self::Destination(failed.error.message()),
        }
    }
}

/// `path` as `/a/b` (no empty components, no trailing `/`; the root is
/// empty), with room to append a name; `None` if it does not fit.
fn normal(path: &str) -> Option<Buffer<{ LINE_MAX + 2 + MAX_NAME }>> {
    let mut normal = Buffer::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        write!(normal, "/{part}").ok()?;
    }
    Some(normal)
}

/// Copies `from` to entry `name` of `directory` (opened for writing) and
/// makes the copy durable.
fn copy_into(
    from: &Node,
    kind: Kind,
    directory: &Node,
    name: &str,
    exact: bool,
) -> Result<Totals, Failed> {
    let fs = |error: FsError| Failed::Destination(error.message());
    let mut copier = Copier::new(COPY_BUFFER).map_err(fs)?;
    let create = match kind {
        Kind::File => flags::CREATE_FILE,
        Kind::Directory => flags::CREATE_DIRECTORY,
    };
    let (to, found) = directory.open(name, create | flags::WRITE).map_err(fs)?;
    let copied = match (kind, found) {
        (Kind::File, Kind::File) => copier.file(from, &to).map(drop).map_err(Failed::from),
        (Kind::Directory, Kind::Directory) => {
            let mut probe = [0u8; MAX_NAME];
            if exact && !matches!(to.entry(0, &mut probe), Ok(None)) {
                Err(Failed::Destination("directory not empty"))
            } else {
                copier.totals.directories += 1;
                copier.directory(from, &to).map_err(Failed::from)
            }
        }
        (Kind::File, Kind::Directory) => Err(Failed::Destination("is a directory")),
        (Kind::Directory, Kind::File) => Err(Failed::Destination("not a directory")),
    };
    // Durable before success is reported, and before a move removes the
    // source.
    let copied = copied.and_then(|()| to.sync().map_err(fs));
    to.close();
    copied.map(|()| copier.totals)
}
const LOAD_BUFFER: usize = 64 * 1024;

/// Reads at `offset`: in bulk through `shared` if the file has one,
/// otherwise inline.
fn read_at(
    file: &Node,
    shared: Option<&Shared>,
    offset: u64,
    out: &mut [u8],
) -> Result<usize, FsError> {
    match shared {
        Some(shared) => file.read_shared(shared, offset, out),
        None => file.read(offset, out),
    }
}
