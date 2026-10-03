//! `cargo xtask` — Oceans build and emulator tooling.
//!
//! Cross-platform (Windows, Linux, macOS) and dependency-free so it works on
//! any machine with a Rust toolchain. See docs/development/getting-started.md.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "oceans-kernel";
/// User programs (in the `user/` workspace) shipped as boot modules.
const USER_PROGRAMS: &[&str] = &["ipc-test"];
const LIMINE_REPO: &str = "https://github.com/limine-bootloader/limine.git";
const LIMINE_BRANCH: &str = "v9.x-binary";
const ONLINE_BANNER: &str = "OCEANS KERNEL ONLINE";
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
  OCEANS_LIMINE directory containing BOOTX64.EFI (default: build/limine)";

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
fn build_user(profile: Profile) -> Result<PathBuf> {
    let mut cmd = user_cargo();
    cmd.arg("build");
    if let Profile::Release = profile {
        cmd.arg("--release");
    }
    run_command(&mut cmd)?;
    Ok(root()
        .join("user")
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile.dir()))
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
    let user = build_user(profile)?;

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

    let mut conf = String::from("timeout: 0\n\n/Oceans\n    protocol: limine\n");
    conf.push_str(&format!("    path: boot():/boot/{KERNEL_PACKAGE}\n"));
    if let Some(cmdline) = cmdline {
        conf.push_str(&format!("    cmdline: {cmdline}\n"));
    }
    for program in USER_PROGRAMS {
        conf.push_str(&format!("    module_path: boot():/boot/{program}\n"));
    }
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start QEMU: {e}"))?;

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
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines_rx.recv_timeout(remaining) {
            Ok(line) => {
                println!("  | {line}");
                online |= line.contains(ONLINE_BANNER);
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
