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

use core::fmt::{self, Write};

use oceans_fs_proto::{FsError, Kind, MAX_NAME, Node, flags};
use oceans_rt::{Buffer, Error, Handle, Start, prot, rights};

oceans_rt::entry!(main);

const PROMPT: &[u8] = b"oceans> ";
const LINE_MAX: usize = 200;
const MAX_ARGS: usize = 16;
const MAX_CHILD_HANDLES: usize = 8;

const CTRL_C: u8 = 0x03;
const CTRL_U: u8 = 0x15;
const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7f;
const ESCAPE: u8 = 0x1b;

/// What the shell holds: its handle directory (`<index> <kind> <name>`).
struct Directory {
    text: &'static str,
    handles: &'static [Handle],
}

impl Directory {
    fn find(&self, kind: &str, name: &str) -> Option<Handle> {
        self.text.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            let index: usize = words.next()?.parse().ok()?;
            (words.next()? == kind && words.next()? == name)
                .then(|| self.handles.get(index).copied())?
        })
    }
}

struct Shell {
    log: Handle,
    console: Handle,
    directory: Directory,
}

fn main(start: Start) -> i64 {
    let Some(&directory_handle) = start.handles.last() else {
        return 1;
    };
    let Some(text) = map_text(directory_handle) else {
        return 2;
    };
    let directory = Directory {
        text,
        handles: start.handles,
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
    };
    let _ = oceans_rt::debug_write(log, "shell: ready");
    shell.print(format_args!("Oceans shell. Type `help` for commands.\r\n"));
    shell.run()
}

fn map_text(memory: Handle) -> Option<&'static str> {
    let base = oceans_rt::memory_map(memory, 0, prot::READ).ok()?;
    // SAFETY: mapped readable, at least one page, mapped for our lifetime.
    let page = unsafe { core::slice::from_raw_parts(base, 4096) };
    let len = page.iter().position(|&b| b == 0)?;
    core::str::from_utf8(&page[..len]).ok()
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
        let mut input = Input::new();
        loop {
            self.write(PROMPT);
            let Some(len) = self.read_line(&mut input, &mut line) else {
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
                for (i, word) in words.iter().enumerate() {
                    self.print(format_args!("{}{word}", if i > 0 { " " } else { "" }));
                }
                self.write(b"\r\n");
            }
            ["grants"] => {
                for line in self.directory.text.lines() {
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
            ["clear"] => self.write(b"\x1b[2J\x1b[H"),
            ["exit"] => return Some(0),
            [command, ..] => {
                self.print(format_args!("{command}: unknown command (try `help`)\r\n"))
            }
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
             \x20                              log, console, use:ENDPOINT\r\n\
             \x20                              (PROGRAM: a granted module, /bin/NAME, or a path)\r\n\
             \x20 ls [PATH]                  list a directory\r\n\
             \x20 cat PATH                   print a file\r\n\
             \x20 write PATH TEXT            replace a file's contents with TEXT\r\n\
             \x20 mkdir PATH                 create a directory\r\n\
             \x20 rm PATH                    remove a file or empty directory\r\n\
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
        let mut offset = 0;
        let mut chunk = [0u8; oceans_fs_proto::MAX_DATA];
        loop {
            match file.read(offset, &mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    // Files use LF; the terminal needs CR LF.
                    for (i, part) in chunk[..n].split(|&b| b == b'\n').enumerate() {
                        if i > 0 {
                            self.write(b"\r\n");
                        }
                        self.write(part);
                    }
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
                let mut done = 0;
                while done < target.len() {
                    let n = file
                        .read(done as u64, &mut target[done..])
                        .map_err(FsError::message)?;
                    if n == 0 {
                        break;
                    }
                    done += n;
                }
                let _ = oceans_rt::memory_unmap(base);
                Ok(())
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

    fn run_program(&self, program: &str, grants: &[&str]) {
        let (image, owned) = match self.find_image(program) {
            Ok(found) => found,
            Err(problem) => {
                self.print(format_args!("run: {program}: {problem}\r\n"));
                return;
            }
        };
        let name = program.rsplit('/').next().unwrap_or(program);
        self.spawn_and_wait(image, name, grants);
        if owned {
            let _ = oceans_rt::close(image);
        }
    }

    fn spawn_and_wait(&self, image: Handle, program: &str, grants: &[&str]) {
        let mut handles = [Handle(0); MAX_CHILD_HANDLES];
        let mut count = 0;
        for grant in grants {
            if count == MAX_CHILD_HANDLES {
                self.print(format_args!("run: too many grants\r\n"));
                return self.close_all(&handles[..count]);
            }
            match self.grant(grant) {
                Ok(handle) => {
                    handles[count] = handle;
                    count += 1;
                }
                Err(problem) => {
                    self.print(format_args!("run: {grant}: {problem}\r\n"));
                    return self.close_all(&handles[..count]);
                }
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
            // CPU exception vectors map to -128 - vector (ADR-0014).
            Ok(code) if code <= -128 => self.print(format_args!(
                "{program} was killed by CPU exception {} (exit {code})\r\n",
                -128 - code
            )),
            Ok(code) => self.print(format_args!("{program} exited with {code}\r\n")),
            Err(error) => self.print(format_args!("run: {program}: {error:?}\r\n")),
        }
    }

    /// A capability for a child, narrowed to what the grant names.
    fn grant(&self, grant: &str) -> Result<Handle, &'static str> {
        let (source, granted_rights) = match grant {
            "log" => (Some(self.log), rights::WRITE | rights::TRANSFER),
            "console" => (
                Some(self.console),
                rights::READ | rights::WRITE | rights::TRANSFER,
            ),
            _ => match grant.strip_prefix("use:") {
                Some(endpoint) => (
                    self.directory.find("use", endpoint),
                    rights::SEND | rights::TRANSFER,
                ),
                None => return Err("unknown grant (log, console, use:ENDPOINT)"),
            },
        };
        let source = source.ok_or("this shell does not hold it")?;
        oceans_rt::duplicate(source, granted_rights).map_err(|error| match error {
            Error::MissingRights => "this shell may not pass it on",
            _ => "cannot duplicate it",
        })
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
