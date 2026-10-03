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
            ["clear"] => self.write(b"\x1b[2J\x1b[H"),
            ["exit"] => return Some(0),
            [command, ..] => {
                self.print(format_args!("{command}: unknown command (try `help`)\r\n"))
            }
        }
        None
    }

    fn help(&self) {
        self.print(format_args!(
            "commands:\r\n\
             \x20 help                       this list\r\n\
             \x20 echo TEXT                  print TEXT\r\n\
             \x20 grants                     capabilities this shell holds\r\n\
             \x20 call ENDPOINT TEXT         send TEXT to a service endpoint I use\r\n\
             \x20 run PROGRAM [GRANT...]     run a program with only the listed authority:\r\n\
             \x20                              log, console, use:ENDPOINT\r\n\
             \x20 clear                      clear the screen\r\n\
             \x20 exit                       leave the shell\r\n"
        ));
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

    fn run_program(&self, program: &str, grants: &[&str]) {
        let Some(image) = self.directory.find("module", program) else {
            self.print(format_args!(
                "run: no program named {program} (see `grants`)\r\n"
            ));
            return;
        };
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
