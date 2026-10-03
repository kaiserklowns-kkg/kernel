//! `cargo xtask` — Oceans build and emulator tooling.
//!
//! Cross-platform (Windows, Linux, macOS) and dependency-free so it works on
//! any machine with a Rust toolchain. See docs/development/getting-started.md.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "oceans-kernel";
/// User programs (in the `user/` workspace) shipped as boot modules.
const USER_PROGRAMS: &[&str] = &[
    "init",
    "echo-service",
    "hello-client",
    "crasher",
    "fs",
    "shell",
    "ps",
    "mem",
    "uptime",
    "uname",
    "ipc-test",
];
/// Service manifests for init: normal boots and smoke tests.
const MANIFEST: &str = "config/services.conf";
const SMOKE_MANIFEST: &str = "config/services-smoke.conf";
const LIMINE_REPO: &str = "https://github.com/limine-bootloader/limine.git";
const LIMINE_BRANCH: &str = "v9.x-binary";
const ONLINE_BANNER: &str = "OCEANS KERNEL ONLINE";
/// The smoke test types `SHELL_SCRIPT` into the serial console when the
/// kernel log shows `SHELL_READY` (user/shell, ADR-0018), then requires
/// every `SHELL_EXPECT` line in the console output.
const SHELL_READY: &str = "shell: ready";
/// Delay between typed bytes.
const TYPING_DELAY: Duration = Duration::from_millis(2);
/// Lines end in CR LF: Enter is CR on a serial terminal, and QEMU's Windows
/// stdio backend drops a lone CR from piped input. 0x7f is Backspace.
const SHELL_SCRIPT: &[&[u8]] = &[
    b"help\r\n",
    b"echo hello from the shell\r\n",
    b"echo abc\x7fd\r\n",
    b"grants\r\n",
    b"call echo ping\r\n",
    // Filesystem (ADR-0019).
    b"ls /bin\r\n",
    b"mkdir /docs\r\n",
    b"write /docs/note.txt hello filesystem\r\n",
    b"cat /docs/note.txt\r\n",
    b"ls /docs\r\n",
    b"rm /docs/note.txt\r\n",
    b"cat /docs/note.txt\r\n",
    b"write /bin/evil x\r\n",
    b"rm /bin/crasher\r\n",
    // Programs come from /bin and get only the typed authority.
    b"run hello-client log use:echo\r\n",
    b"run /bin/crasher log\r\n",
    b"run hello-client use:nothing\r\n",
    // Utilities (ADR-0020): bare commands get only what their manifest
    // requests, and only low-risk grants.
    b"uname\r\n",
    b"uptime\r\n",
    b"mem\r\n",
    b"ps\r\n",
    b"run ps out\r\n",
    b"hello-client\r\n",
    b"run nosuch\r\n",
    b"frobnicate\r\n",
    b"exit\r\n",
];
/// Output the script must produce: `Line` must be a whole console line,
/// `Contains` a substring of one (never text that is also typed input).
const SHELL_EXPECT: &[Expect] = &[
    Expect::Contains("Oceans shell."),
    Expect::Contains("run PROGRAM [GRANT...]"),
    Expect::Line("hello from the shell"),
    Expect::Line("abd"),
    Expect::Contains("use fs"),
    Expect::Line("PING"),
    Expect::Line("  hello-client"),
    Expect::Line("  crasher"),
    Expect::Line("hello filesystem"),
    Expect::Line("  note.txt"),
    Expect::Contains("cat: /docs/note.txt: not found"),
    Expect::Contains("write: /bin/evil: permission denied"),
    Expect::Contains("rm: /bin/crasher: permission denied"),
    Expect::Line("hello-client exited with 0"),
    Expect::Contains("crasher was killed by CPU exception 14"),
    Expect::Contains("run: use:nothing: this shell does not hold it"),
    Expect::Contains("Oceans 0.1.0 x86_64 (ABI 6)"),
    Expect::Contains(" seconds"),
    Expect::Contains("MiB free of"),
    Expect::Contains("PID  PPID  MEMORY"),
    Expect::Contains("init/shell/ps"),
    Expect::Contains("ps: needs the sysinfo capability"),
    Expect::Contains("hello-client: has no manifest"),
    Expect::Contains("run: nosuch: not found"),
    Expect::Contains("frobnicate: unknown command"),
];

#[derive(Clone, Copy, Debug)]
enum Expect {
    Line(&'static str),
    Contains(&'static str),
}

impl Expect {
    fn matches(self, line: &str) -> bool {
        // The console echoes the prompt and typed input; strip it so only
        // command output can satisfy a `Line`.
        let output = line.trim_end_matches('\r');
        match self {
            Self::Line(expected) => output == expected,
            Self::Contains(expected) => output.contains(expected),
        }
    }
}
const SMOKE_TIMEOUT: Duration = Duration::from_secs(60);
/// QEMU exit status for `EmulatorExit::Success` (0x10 << 1 | 1).
const QEMU_EXIT_SUCCESS: i32 = 33;

type Result<T = ()> = std::result::Result<T, String>;

const USAGE: &str = "\
usage: cargo xtask <command> [--release]

commands:
  check     format check, clippy (host + kernel) and host unit tests
  build     build the kernel
  limine    download the Limine UEFI bootloader into build/limine
  image     build the kernel and assemble the EFI system partition in build/esp
  run       boot Oceans in QEMU with serial output on this terminal
  smoke     boot Oceans headless in QEMU and verify it comes online

environment:
  OCEANS_QEMU   path to qemu-system-x86_64
  OCEANS_OVMF   path to the x86_64 UEFI firmware code image (OVMF/edk2)
  OCEANS_LIMINE directory containing BOOTX64.EFI (default: build/limine)
  OCEANS_QEMU_EXTRA extra QEMU arguments, e.g. \"-cpu max\"";

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let command = args.next();
    let rest: Vec<String> = args.collect();
    let profile = if rest.iter().any(|a| a == "--release") {
        Profile::Release
    } else {
        Profile::Dev
    };
    if let Some(unknown) = rest.iter().find(|a| *a != "--release") {
        eprintln!("unknown argument: {unknown}\n\n{USAGE}");
        return ExitCode::from(2);
    }

    let result = match command.as_deref() {
        Some("check") => check(),
        Some("build") => build_kernel(profile).map(drop),
        Some("limine") => fetch_limine(),
        Some("image") => build_image(profile, None).map(drop),
        Some("run") => run(profile),
        Some("smoke") => smoke(profile),
        Some("help" | "--help" | "-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy)]
enum Profile {
    Dev,
    Release,
}

impl Profile {
    fn dir(self) -> &'static str {
        match self {
            Self::Dev => "debug",
            Self::Release => "release",
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask lives at tools/xtask")
        .to_path_buf()
}

fn cargo() -> Command {
    let mut cmd = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(root());
    cmd
}

fn run_command(cmd: &mut Command) -> Result {
    let status = cmd
        .status()
        .map_err(|e| format!("failed to start {:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed with {status}", cmd.get_program()))
    }
}

fn check() -> Result {
    run_command(cargo().args(["fmt", "--all", "--check"]))?;
    run_command(cargo().args([
        "clippy",
        "--workspace",
        "--exclude",
        KERNEL_PACKAGE,
        "--",
        "-D",
        "warnings",
    ]))?;
    run_command(cargo().args([
        "clippy",
        "-p",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--",
        "-D",
        "warnings",
    ]))?;
    run_command(cargo().args(["test", "--workspace", "--exclude", KERNEL_PACKAGE]))?;
    run_command(user_cargo().args(["fmt", "--all", "--check"]))?;
    run_command(user_cargo().args(["clippy", "--release", "--", "-D", "warnings"]))
}

/// Cargo in the `user/` workspace, whose `.cargo/config.toml` selects the
/// target and the user code model.
fn user_cargo() -> Command {
    let mut cmd = cargo();
    cmd.current_dir(root().join("user"));
    cmd
}

/// Builds the user programs; returns the directory holding the binaries.
/// Always release builds: they are shipped system programs, loaded through
/// the filesystem, so size matters (ADR-0020); the kernel profile does not
/// change them.
fn build_user() -> Result<PathBuf> {
    run_command(user_cargo().args(["build", "--release"]))?;
    Ok(root()
        .join("user")
        .join("target")
        .join(KERNEL_TARGET)
        .join(Profile::Release.dir()))
}

fn build_kernel(profile: Profile) -> Result<PathBuf> {
    let mut cmd = cargo();
    cmd.args(["build", "-p", KERNEL_PACKAGE, "--target", KERNEL_TARGET]);
    if let Profile::Release = profile {
        cmd.arg("--release");
    }
    run_command(&mut cmd)?;
    Ok(root()
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile.dir())
        .join(KERNEL_PACKAGE))
}

fn limine_dir() -> PathBuf {
    env::var_os("OCEANS_LIMINE").map_or_else(|| root().join("build").join("limine"), PathBuf::from)
}

fn fetch_limine() -> Result {
    let dir = limine_dir();
    if dir.join("BOOTX64.EFI").is_file() {
        println!("Limine already present in {}", dir.display());
        return Ok(());
    }
    println!(
        "cloning Limine {LIMINE_BRANCH} from {LIMINE_REPO} into {}",
        dir.display()
    );
    run_command(
        Command::new("git")
            .args(["clone", "--depth=1", "--branch", LIMINE_BRANCH, LIMINE_REPO])
            .arg(&dir),
    )
}

/// Builds `build/esp`, a directory QEMU exposes as a FAT drive:
///
/// ```text
/// EFI/BOOT/BOOTX64.EFI        Limine
/// boot/limine/limine.conf
/// boot/oceans-kernel
/// boot/<user program>…       boot modules
/// ```
fn build_image(profile: Profile, cmdline: Option<&str>) -> Result<PathBuf> {
    let kernel = build_kernel(profile)?;
    let user = build_user()?;

    let limine_efi = limine_dir().join("BOOTX64.EFI");
    if !limine_efi.is_file() {
        return Err(format!(
            "{} not found; run `cargo xtask limine` or set OCEANS_LIMINE",
            limine_efi.display()
        ));
    }

    let esp = root().join("build").join("esp");
    if esp.exists() {
        fs::remove_dir_all(&esp).map_err(|e| format!("cannot clear {}: {e}", esp.display()))?;
    }
    let efi_boot = esp.join("EFI").join("BOOT");
    let limine_conf_dir = esp.join("boot").join("limine");
    for dir in [&efi_boot, &limine_conf_dir] {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }

    copy(&limine_efi, &efi_boot.join("BOOTX64.EFI"))?;
    copy(&kernel, &esp.join("boot").join(KERNEL_PACKAGE))?;
    for program in USER_PROGRAMS {
        copy(&user.join(program), &esp.join("boot").join(program))?;
    }
    let manifest = if cmdline.is_some() {
        SMOKE_MANIFEST
    } else {
        MANIFEST
    };
    copy(
        &root().join(manifest),
        &esp.join("boot").join("services.conf"),
    )?;

    let mut conf = String::from("timeout: 0\n\n/Oceans\n    protocol: limine\n");
    conf.push_str(&format!("    path: boot():/boot/{KERNEL_PACKAGE}\n"));
    if let Some(cmdline) = cmdline {
        conf.push_str(&format!("    cmdline: {cmdline}\n"));
    }
    for program in USER_PROGRAMS {
        conf.push_str(&format!("    module_path: boot():/boot/{program}\n"));
    }
    conf.push_str("    module_path: boot():/boot/services.conf\n");
    let conf_path = limine_conf_dir.join("limine.conf");
    fs::write(&conf_path, conf)
        .map_err(|e| format!("cannot write {}: {e}", conf_path.display()))?;

    println!("image ready in {}", esp.display());
    Ok(esp)
}

fn copy(from: &Path, to: &Path) -> Result {
    fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

fn qemu_command(headless: bool) -> Result<Command> {
    let qemu = find_qemu()?;
    let firmware = find_firmware(&qemu)?;

    let mut cmd = Command::new(&qemu);
    cmd.current_dir(root());
    let mut pflash = OsString::from("if=pflash,format=raw,readonly=on,file=");
    pflash.push(&firmware);
    cmd.args([
        "-machine",
        "q35",
        "-m",
        "256M",
        "-no-reboot",
        "-serial",
        "stdio",
    ])
    .arg("-drive")
    .arg(pflash)
    // Relative path: QEMU's option parser would split an absolute
    // Windows path at the drive-letter colon. `rw:` because QEMU refuses a
    // read-only vvfat node on a writable IDE disk; the image is rebuilt on
    // every run, so guest writes are harmless.
    .args(["-drive", "format=raw,file=fat:rw:build/esp"])
    .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    if headless {
        cmd.args(["-display", "none"]);
    }
    // e.g. OCEANS_QEMU_EXTRA="-cpu max" to exercise SMEP/SMAP/UMIP.
    if let Some(extra) = env::var_os("OCEANS_QEMU_EXTRA") {
        cmd.args(extra.to_string_lossy().split_whitespace());
    }
    Ok(cmd)
}

fn find_qemu() -> Result<PathBuf> {
    if let Some(path) = env::var_os("OCEANS_QEMU") {
        return Ok(PathBuf::from(path));
    }
    let exe = if cfg!(windows) {
        "qemu-system-x86_64.exe"
    } else {
        "qemu-system-x86_64"
    };
    let from_path = env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths)
                .map(|dir| dir.join(exe))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let well_known = [PathBuf::from(r"C:\Program Files\qemu").join(exe)];
    from_path
        .into_iter()
        .chain(well_known)
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| "qemu-system-x86_64 not found; install QEMU or set OCEANS_QEMU".to_string())
}

fn find_firmware(qemu: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("OCEANS_OVMF") {
        return Ok(PathBuf::from(path));
    }
    let mut candidates = Vec::new();
    if let Some(qemu_dir) = qemu.parent() {
        // Windows installer layout and Homebrew-style prefix/share/qemu.
        candidates.push(qemu_dir.join("share").join("edk2-x86_64-code.fd"));
        if let Some(prefix) = qemu_dir.parent() {
            candidates.push(
                prefix
                    .join("share")
                    .join("qemu")
                    .join("edk2-x86_64-code.fd"),
            );
        }
    }
    candidates.extend(
        [
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
            "/usr/share/qemu/edk2-x86_64-code.fd",
        ]
        .map(PathBuf::from),
    );
    candidates.into_iter().find(|c| c.is_file()).ok_or_else(|| {
        "x86_64 UEFI firmware (OVMF/edk2) not found; install it or set OCEANS_OVMF".to_string()
    })
}

fn run(profile: Profile) -> Result {
    build_image(profile, None)?;
    run_command(&mut qemu_command(false)?)
}

fn smoke(profile: Profile) -> Result {
    build_image(profile, Some("oceans.test=smoke"))?;

    let mut child = qemu_command(true)?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start QEMU: {e}"))?;
    let mut serial_input = child.stdin.take().expect("stdin is piped");

    let stdout = child.stdout.take().expect("stdout is piped");
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::io::Result::ok)
        {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + SMOKE_TIMEOUT;
    let mut online = false;
    let mut unmet: Vec<Expect> = SHELL_EXPECT.to_vec();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines_rx.recv_timeout(remaining) {
            Ok(line) => {
                println!("  | {line}");
                online |= line.contains(ONLINE_BANNER);
                unmet.retain(|expect| !expect.matches(&line));
                if line.contains(SHELL_READY) {
                    // Typed into the guest's serial port (QEMU -serial stdio).
                    // Paced like typing: the 16-byte UART FIFO overflows if a
                    // whole script arrives while the guest is busy printing.
                    for &byte in SHELL_SCRIPT.iter().flat_map(|command| command.iter()) {
                        serial_input
                            .write_all(&[byte])
                            .and_then(|()| serial_input.flush())
                            .map_err(|e| format!("cannot type into the serial console: {e}"))?;
                        thread::sleep(TYPING_DELAY);
                    }
                }
            }
            // Reader finished: QEMU closed stdout, i.e. exited.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "smoke test timed out after {}s",
                    SMOKE_TIMEOUT.as_secs()
                ));
            }
        }
    }

    let status = child.wait().map_err(|e| format!("waiting for QEMU: {e}"))?;
    if !unmet.is_empty() {
        return Err(format!("shell output missing: {unmet:?}"));
    }
    match (online, status.code()) {
        (true, Some(QEMU_EXIT_SUCCESS)) => {
            println!("smoke test passed: kernel came online");
            Ok(())
        }
        (false, _) => Err(format!(
            "kernel never printed `{ONLINE_BANNER}` (QEMU {status})"
        )),
        (true, _) => Err(format!("kernel came online but QEMU exited with {status}")),
    }
}
