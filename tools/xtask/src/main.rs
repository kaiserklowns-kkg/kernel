//! `cargo xtask` — Oceans build and emulator tooling.
//!
//! Cross-platform (Windows, Linux, macOS) and dependency-free so it works on
//! any machine with a Rust toolchain. See docs/development/getting-started.md.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
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
    "lspci",
    "disk",
    "virtio-blk",
    "virtio-net",
    "net",
    "net-echo",
    "ifconfig",
    "ping",
    "host",
    "nc",
    "fetch",
    "ipc-test",
];
/// The virtio disk QEMU attaches (ADR-0021), holding the filesystem
/// (ADR-0022). `run` keeps its disk across boots: it is the system's
/// storage. `smoke` starts from a fresh blank disk, boots twice, and mounts
/// the result on the host.
const DISK_IMAGE: &str = "build/disk.img";
const SMOKE_DISK_IMAGE: &str = "build/smoke-disk.img";
const DISK_SIZE: u64 = 8 * 1024 * 1024;
/// What the first smoke boot stores and the second reads back.
const KEPT_PATH: [&str; 2] = ["keep", "note.txt"];
const KEPT_TEXT: &str = "kept across reboots";
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
    // Devices (ADR-0021). Raw disk access is the filesystem's alone.
    b"lspci\r\n",
    b"run lspci out devices\r\n",
    b"run disk out\r\n",
    b"run disk out use:block -- info\r\n",
    // Files on disk (ADR-0022), read back by the second boot.
    b"mkdir /keep\r\n",
    b"write /keep/note.txt kept across reboots\r\n",
    b"write /keep/gone.txt temporary\r\n",
    b"rm /keep/gone.txt\r\n",
    b"sync\r\n",
    // The network (ADR-0023): QEMU's user network answers DHCP and pings
    // to its gateway; 10.0.2.99 does not exist.
    b"ifconfig\r\n",
    b"run ifconfig out use:net\r\n",
    b"run ping out use:net -- 10.0.2.2 2\r\n",
    b"run ping out use:net -- 10.0.2.99 1\r\n",
    // TCP and DNS (ADR-0024), against servers xtask runs on the host
    // (reached as 10.0.2.2; `$DNS` and `$TCP` are their ports).
    b"run host out use:net -- oceans.test 10.0.2.2:$DNS\r\n",
    b"run host out use:net -- missing.test 10.0.2.2:$DNS\r\n",
    b"run nc out use:net -- 10.0.2.2 $TCP hello over tcp\r\n",
    // HTTP (ADR-0028), from a server xtask runs on the host.
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/hello.txt\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/redirect\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/chunked\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/missing\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/big /keep/big.bin\r\n",
    b"exit\r\n",
];
/// The second smoke boot, on the disk the first one left.
const REBOOT_SCRIPT: &[&[u8]] = &[
    b"cat /keep/note.txt\r\n",
    b"ls /keep\r\n",
    b"ls /\r\n",
    b"write /bin/evil x\r\n",
    b"uname\r\n",
    b"exit\r\n",
];
const REBOOT_EXPECT: &[Expect] = &[
    Expect::Contains("fs: mounted the disk: generation"),
    Expect::Line("kept across reboots"),
    Expect::Line("  note.txt"),
    Expect::Line("  keep/"),
    Expect::Line("  docs/"),
    Expect::Line("  bin/"),
    Expect::Contains("write: /bin/evil: permission denied"),
    Expect::Contains("Oceans 0.1.0 x86_64 (ABI 9)"),
    Expect::Contains("net: configured 10.0.2.15/24"),
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
    Expect::Contains("Oceans 0.1.0 x86_64 (ABI 9)"),
    Expect::Contains(" seconds"),
    Expect::Contains("MiB free of"),
    Expect::Contains("PID  PPID  MEMORY"),
    Expect::Contains("init/shell/ps"),
    Expect::Contains("ps: needs the sysinfo capability"),
    Expect::Contains("hello-client: has no manifest"),
    Expect::Contains("run: nosuch: not found"),
    Expect::Contains("frobnicate: unknown command"),
    Expect::Contains("lspci: requests `devices`"),
    Expect::Contains("8086:29c0  host bridge"),
    Expect::Contains("1af4:1042  mass storage  (driver attached)"),
    Expect::Contains("disk: needs the block capability"),
    Expect::Contains("run: use:block: this shell does not hold it"),
    Expect::Contains("fs: formatted a blank disk"),
    Expect::Contains("virtio-net: MAC 52:54:00:12:34:56, MSI-X"),
    Expect::Contains("net: configured 10.0.2.15/24 gateway 10.0.2.2 dns 10.0.2.3 (DHCP)"),
    Expect::Contains("net-echo: listening on UDP and TCP port 7"),
    Expect::Contains("ifconfig: requests `use:net`"),
    Expect::Line("net0: 10.0.2.15/24 gateway 10.0.2.2 dns 10.0.2.3"),
    Expect::Line("      mac 52:54:00:12:34:56"),
    Expect::Contains("reply from 10.0.2.2: seq=1"),
    Expect::Contains("reply from 10.0.2.2: seq=2"),
    Expect::Line("2 sent, 2 received"),
    Expect::Line("timeout: seq=1"),
    Expect::Line("1 sent, 0 received"),
    Expect::Line("oceans.test has address 10.1.2.3"),
    Expect::Contains("host: missing.test: not found"),
    Expect::Line("hello from the host: hello over tcp"),
    Expect::Line("hello over http"),
    Expect::Line("you were redirected"),
    Expect::Line("chunked transfer works"),
    Expect::Contains("fetch: HTTP 404 Not Found"),
    Expect::Contains("fetch: saved 1048576 bytes"),
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
const SMOKE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long the last command waits for the host's echo probes.
const PROBE_WAIT: Duration = Duration::from_secs(30);
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
/// boot/initrd                 the boot archive: every program and the
///                             service manifest (ADR-0025)
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
    let manifest = if cmdline.is_some() {
        SMOKE_MANIFEST
    } else {
        MANIFEST
    };
    let mut files = Vec::new();
    for program in USER_PROGRAMS {
        let path = user.join(program);
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        files.push((program.to_string(), bytes));
    }
    let manifest_path = root().join(manifest);
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    files.push(("services.conf".to_string(), manifest_bytes));
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let mut archive = vec![0u8; oceans_archive::archive_len(&entries)];
    oceans_archive::write(&entries, &mut archive)
        .map_err(|e| format!("cannot build the boot archive: {e:?}"))?;
    let archive_path = esp.join("boot").join("initrd");
    fs::write(&archive_path, &archive)
        .map_err(|e| format!("cannot write {}: {e}", archive_path.display()))?;

    let mut conf = String::from("timeout: 0\n\n/Oceans\n    protocol: limine\n");
    conf.push_str(&format!("    path: boot():/boot/{KERNEL_PACKAGE}\n"));
    if let Some(cmdline) = cmdline {
        conf.push_str(&format!("    cmdline: {cmdline}\n"));
    }
    conf.push_str("    module_path: boot():/boot/initrd\n");
    let conf_path = limine_conf_dir.join("limine.conf");
    fs::write(&conf_path, conf)
        .map_err(|e| format!("cannot write {}: {e}", conf_path.display()))?;

    println!(
        "image ready in {} (boot archive: {} files, {} KiB)",
        esp.display(),
        entries.len(),
        archive.len() / 1024
    );
    Ok(esp)
}

fn copy(from: &Path, to: &Path) -> Result {
    fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// Creates a blank (zero-filled) disk image, which the filesystem formats
/// on first use; an existing one is replaced only if `fresh`.
fn prepare_disk(path: &str, fresh: bool) -> Result {
    let path = root().join(path);
    if path.is_file() && !fresh {
        return Ok(());
    }
    fs::write(&path, vec![0u8; DISK_SIZE as usize])
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The MAC QEMU gives the guest's network device.
const GUEST_MAC: &str = "52:54:00:12:34:56";

/// `forward`: host (UDP, TCP) ports forwarded to the guest's port 7.
fn qemu_command(headless: bool, disk: &str, forward: Option<(u16, u16)>) -> Result<Command> {
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
    // read-only vvfat node on a writable IDE disk (and refuses `snapshot`
    // with `rw`). vvfat writes guest changes back to this directory: the
    // firmware stores its variables there (NvVars), and the write-back has
    // rewritten the boot loader itself, so callers rebuild the image before
    // every boot.
    .args(["-drive", "format=raw,file=fat:rw:build/esp"])
    // A modern-only virtio disk (PCI ID 1af4:1042), driven by the
    // userspace virtio-blk service.
    .arg("-drive")
    .arg(format!("if=none,id=disk0,format=raw,file={disk}"))
    .args(["-device", "virtio-blk-pci,drive=disk0,disable-legacy=on"])
    // A modern-only virtio NIC (1af4:1041) on QEMU's user network (NAT,
    // DHCP at 10.0.2.2), driven by the userspace virtio-net service.
    .arg("-netdev")
    .arg(match forward {
        Some((udp, tcp)) => {
            format!("user,id=net0,hostfwd=udp:127.0.0.1:{udp}-:7,hostfwd=tcp:127.0.0.1:{tcp}-:7")
        }
        None => "user,id=net0".to_string(),
    })
    .arg("-device")
    .arg(format!(
        "virtio-net-pci,netdev=net0,disable-legacy=on,mac={GUEST_MAC}"
    ))
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
    prepare_disk(DISK_IMAGE, false)?;
    run_command(&mut qemu_command(false, DISK_IMAGE, None)?)
}

fn smoke(profile: Profile) -> Result {
    build_image(profile, Some("oceans.test=smoke"))?;
    prepare_disk(SMOKE_DISK_IMAGE, true)?;
    println!("smoke boot 1 of 2: blank disk");
    smoke_boot(SHELL_SCRIPT, SHELL_EXPECT)?;
    println!("smoke boot 2 of 2: the same disk");
    // A clean boot image: the first boot's firmware wrote into it (vvfat).
    build_image(profile, Some("oceans.test=smoke"))?;
    smoke_boot(REBOOT_SCRIPT, REBOOT_EXPECT)?;
    check_smoke_disk()?;
    println!("smoke test passed: kernel came online, files survived a reboot");
    Ok(())
}

/// What the guest's console showed.
enum Console {
    Line(String),
    /// The shell's prompt: it waits for the next command.
    Prompt,
}

const SHELL_PROMPT: &[u8] = b"oceans> ";

/// One headless boot: types `script` into the shell, one command per
/// prompt, and requires every `expected` line, the online banner and a
/// successful exit.
fn smoke_boot(script: &[&[u8]], expected: &[Expect]) -> Result {
    let udp_forward = free_udp_port()?;
    let tcp_forward = free_tcp_port()?;
    let udp_echo = udp_echo_probe(udp_forward);
    let tcp_echo = tcp_echo_probe(tcp_forward);
    let dns_port = dns_server()?;
    let tcp_port = tcp_greeter()?;
    let http_port = http_server()?;
    let expand = |command: &[u8]| -> Vec<u8> {
        String::from_utf8_lossy(command)
            .replace("$DNS", &dns_port.to_string())
            .replace("$TCP", &tcp_port.to_string())
            .replace("$HTTP", &http_port.to_string())
            .into_bytes()
    };
    let mut child = qemu_command(true, SMOKE_DISK_IMAGE, Some((udp_forward, tcp_forward)))?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start QEMU: {e}"))?;
    let mut serial_input = child.stdin.take().expect("stdin is piped");

    let stdout = child.stdout.take().expect("stdout is piped");
    let (events_tx, events_rx) = mpsc::channel();
    thread::spawn(move || {
        // Bytes, not lines: the prompt has no line ending.
        let mut line = Vec::new();
        for byte in BufReader::new(stdout)
            .bytes()
            .map_while(std::io::Result::ok)
        {
            if byte == b'\n' {
                let text = String::from_utf8_lossy(&line).into_owned();
                line.clear();
                if events_tx.send(Console::Line(text)).is_err() {
                    break;
                }
            } else {
                line.push(byte);
                if line.ends_with(SHELL_PROMPT) && events_tx.send(Console::Prompt).is_err() {
                    break;
                }
            }
        }
    });

    let deadline = Instant::now() + SMOKE_TIMEOUT;
    let mut online = false;
    let mut ready = false;
    let mut commands = script.iter();
    let mut unmet: Vec<Expect> = expected.to_vec();
    let (mut udp_answered, mut tcp_answered) = (false, false);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match events_rx.recv_timeout(remaining) {
            Ok(Console::Line(line)) => {
                println!("  | {line}");
                online |= line.contains(ONLINE_BANNER);
                ready |= line.contains(SHELL_READY);
                unmet.retain(|expect| !expect.matches(&line));
            }
            // One command per prompt: typing while the guest (or QEMU, during
            // a disk flush) is busy overflows the 16-byte UART FIFO, because
            // QEMU's Windows stdio backend ignores backpressure.
            Ok(Console::Prompt) if ready => {
                if let Some(command) = commands.next() {
                    // The last command ends the boot: the host's probes into
                    // the guest's echo service must be done by then.
                    if commands.len() == 0 {
                        udp_answered = udp_answered || udp_echo.recv_timeout(PROBE_WAIT).is_ok();
                        tcp_answered = tcp_answered || tcp_echo.recv_timeout(PROBE_WAIT).is_ok();
                    }
                    for byte in expand(command) {
                        serial_input
                            .write_all(&[byte])
                            .and_then(|()| serial_input.flush())
                            .map_err(|e| format!("cannot type into the serial console: {e}"))?;
                        thread::sleep(TYPING_DELAY);
                    }
                }
            }
            Ok(Console::Prompt) => {}
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
    if !(udp_answered || udp_echo.try_recv().is_ok()) {
        return Err("the guest's UDP echo service never answered from the host".into());
    }
    if !(tcp_answered || tcp_echo.try_recv().is_ok()) {
        return Err("the guest's TCP echo service never answered from the host".into());
    }
    println!("UDP and TCP echo answered from the host through the guest's network stack");
    if !unmet.is_empty() {
        return Err(format!("shell output missing: {unmet:?}"));
    }
    match (online, status.code()) {
        (true, Some(QEMU_EXIT_SUCCESS)) => Ok(()),
        (false, _) => Err(format!(
            "kernel never printed `{ONLINE_BANNER}` (QEMU {status})"
        )),
        (true, _) => Err(format!("kernel came online but QEMU exited with {status}")),
    }
}

/// A disk image file as a block device, for mounting on the host.
struct ImageFile(Vec<u8>);

impl oceans_volume::BlockDevice for ImageFile {
    fn block_count(&self) -> u64 {
        (self.0.len() / oceans_volume::BLOCK_SIZE) as u64
    }

    fn read_block(
        &mut self,
        block: u64,
        out: &mut oceans_volume::BlockBuf,
    ) -> std::result::Result<(), oceans_volume::IoError> {
        let at = block as usize * oceans_volume::BLOCK_SIZE;
        let bytes = self.0.get(at..at + oceans_volume::BLOCK_SIZE);
        out.copy_from_slice(bytes.ok_or(oceans_volume::IoError)?);
        Ok(())
    }

    fn write_block(
        &mut self,
        _: u64,
        _: &oceans_volume::BlockBuf,
    ) -> std::result::Result<(), oceans_volume::IoError> {
        Err(oceans_volume::IoError) // read-only inspection
    }

    fn flush(&mut self) -> std::result::Result<(), oceans_volume::IoError> {
        Ok(())
    }
}

/// Mounts the smoke disk on the host with the same volume code: the file
/// the guest kept must be there with its text, and the removed one gone.
fn check_smoke_disk() -> Result {
    use oceans_volume::{ROOT, Volume};

    let path = root().join(SMOKE_DISK_IMAGE);
    let image = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (mut volume, _) = Volume::open(ImageFile(image), false)
        .map_err(|e| format!("{} does not mount on the host: {e:?}", path.display()))?;
    let lookup = |volume: &Volume<ImageFile>, dir, name| {
        volume
            .lookup(dir, name)
            .map_err(|e| format!("/{}: {e:?}", KEPT_PATH.join("/")))
    };
    let keep = lookup(&volume, ROOT, KEPT_PATH[0])?;
    let note = lookup(&volume, keep, KEPT_PATH[1])?;
    let mut text = vec![0u8; volume.size(note).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(note, 0, &mut text)
        .map_err(|e| format!("reading the kept file: {e:?}"))?;
    // The shell's `write` ends the text with a newline.
    let big = lookup(&volume, keep, "big.bin")?;
    let mut downloaded = vec![0u8; volume.size(big).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(big, 0, &mut downloaded)
        .map_err(|e| format!("reading the downloaded file: {e:?}"))?;
    if downloaded != big_body() {
        return Err("the file fetched over HTTP does not match what the host served".into());
    }
    if text != format!("{KEPT_TEXT}\n").as_bytes() {
        return Err(format!(
            "kept file holds {:?}",
            String::from_utf8_lossy(&text)
        ));
    }
    if volume.lookup(keep, "gone.txt").is_ok() {
        return Err("a removed file is still on the disk".into());
    }
    println!(
        "disk verified on the host: generation {}, /{} intact",
        volume.generation(),
        KEPT_PATH.join("/")
    );
    Ok(())
}

/// A UDP port free on the host now, for QEMU to forward.
fn free_udp_port() -> Result<u16> {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .map(|address| address.port())
        .map_err(|e| format!("cannot find a free UDP port: {e}"))
}

/// Sends datagrams from the host to the guest's UDP echo service (through
/// QEMU's port forwarding) until one comes back; reports success once.
fn udp_echo_probe(port: u16) -> mpsc::Receiver<()> {
    const PROBE: &[u8] = b"oceans udp echo probe";
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
            return;
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let deadline = Instant::now() + SMOKE_TIMEOUT;
        let mut reply = [0u8; 64];
        while Instant::now() < deadline {
            if socket.send_to(PROBE, ("127.0.0.1", port)).is_err() {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            if let Ok((len, _)) = socket.recv_from(&mut reply)
                && &reply[..len] == PROBE
            {
                let _ = done_tx.send(());
                return;
            }
        }
    });
    done_rx
}

/// A TCP port free on the host now, for QEMU to forward.
fn free_tcp_port() -> Result<u16> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|e| format!("cannot find a free TCP port: {e}"))
}

/// Connects from the host to the guest's TCP echo service (through QEMU's
/// port forwarding) until a probe comes back; reports success once.
fn tcp_echo_probe(port: u16) -> mpsc::Receiver<()> {
    const PROBE: &[u8] = b"oceans tcp echo probe";
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let deadline = Instant::now() + SMOKE_TIMEOUT;
        while Instant::now() < deadline {
            let attempt = (|| -> std::io::Result<bool> {
                let mut stream = TcpStream::connect(("127.0.0.1", port))?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(PROBE)?;
                let mut reply = vec![0u8; PROBE.len()];
                let mut got = 0;
                while got < reply.len() {
                    match stream.read(&mut reply[got..])? {
                        0 => return Ok(false),
                        n => got += n,
                    }
                }
                Ok(reply == PROBE)
            })();
            if let Ok(true) = attempt {
                let _ = done_tx.send(());
                return;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
    done_rx
}

/// A DNS server on the host for the guest's resolver: `oceans.test` is
/// 10.1.2.3, every other name does not exist. Returns its UDP port.
fn dns_server() -> Result<u16> {
    let socket =
        UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("cannot start the DNS server: {e}"))?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let deadline = Instant::now() + 2 * SMOKE_TIMEOUT;
        let mut query = [0u8; 512];
        let mut answer = [0u8; 512];
        while Instant::now() < deadline {
            let Ok((len, from)) = socket.recv_from(&mut query) else {
                continue;
            };
            let query = &query[..len];
            let known = query.get(12..25) == Some(b"\x06oceans\x04test\x00".as_slice());
            let address = known.then_some([10, 1, 2, 3]);
            if let Ok(len) = oceans_dns::respond(query, address, &mut answer) {
                let _ = socket.send_to(&answer[..len], from);
            }
        }
    });
    Ok(port)
}

/// A TCP server on the host: answers each line with a greeting, then
/// closes. Returns its port.
fn tcp_greeter() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("cannot start the TCP server: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while let Ok(1) = stream.read(&mut byte) {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            let greeting = format!("hello from the host: {}\n", String::from_utf8_lossy(&line));
            let _ = stream.write_all(greeting.as_bytes());
        }
    });
    Ok(port)
}

/// The body of `/big`: 1 MiB in a pattern that catches reordering.
fn big_body() -> Vec<u8> {
    (0..1_048_576u32).map(|i| (i % 251) as u8).collect()
}

/// An HTTP server on the host for the guest's `fetch`. Returns its port.
fn http_server() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("cannot start the HTTP server: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                match stream.read(&mut byte) {
                    Ok(1) => request.push(byte[0]),
                    _ => break,
                }
            }
            let request = String::from_utf8_lossy(&request);
            let path = request.split(' ').nth(1).unwrap_or("").to_string();
            let fixed = |status: &str, body: &[u8]| {
                let mut response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend_from_slice(body);
                response
            };
            let response = match path.as_str() {
                "/hello.txt" => fixed("200 OK", b"hello over http\n"),
                "/moved.txt" => fixed("200 OK", b"you were redirected\n"),
                "/redirect" => b"HTTP/1.1 302 Found\r\nLocation: /moved.txt\r\nContent-Length: 0\r\n\r\n".to_vec(),
                "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nchunked \r\nF\r\ntransfer works\n\r\n0\r\n\r\n".to_vec(),
                "/big" => fixed("200 OK", &big_body()),
                _ => fixed("404 Not Found", b"not here\n"),
            };
            let _ = stream.write_all(&response);
        }
    });
    Ok(port)
}
