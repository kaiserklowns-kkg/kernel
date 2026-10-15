//! Oceans Setup on Linux (ADR-0131): in a terminal, as root, step by step:
//!
//! 1. checks this PC: UEFI, Secure Boot off, Linux on an NVMe SSD (GPT,
//!    512-byte sectors), its EFI system partition mounted with room for the
//!    installer;
//! 2. gets the latest release (or one in a folder, `--from`) and checks it
//!    against the Oceans release key;
//! 3. asks the user to type ERASE: Linux and everything on the SSD go;
//! 4. stages the installer in the EFI system partition, adds the UEFI boot
//!    entries ("Oceans" first; the installer for the next start only) and
//!    restarts. Oceans' installer (ADR-0127) does the rest.
//!
//! Until the restart nothing is lost but the room the files take: Linux
//! still starts if the PC is not restarted; the entries can be removed
//! with `efibootmgr -B`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::linux_plan::{self, Esp, LinuxDisk};
use crate::{efi, hardware, release};

const USAGE: &str =
    "usage: sudo ./OceansSetup-linux [--from DIR] [--key HEX PUBLISHER] [--no-restart]";

fn step(text: &str) {
    println!("==> {text}");
}

/// The whole of it; the process's exit code.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    match install(args) {
        Ok(()) => 0,
        Err(problem) => {
            eprintln!("\nOceans Setup stopped: {problem}");
            eprintln!("Nothing was changed on this PC unless said above.");
            1
        }
    }
}

struct Options {
    from: Option<PathBuf>,
    key: Option<(String, String)>,
    restart: bool,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        from: None,
        key: None,
        restart: true,
    };
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--from" => options.from = Some(words.next().ok_or(USAGE)?.into()),
            "--key" => {
                options.key = words.next().cloned().zip(words.next().cloned());
                if options.key.is_none() {
                    return Err(USAGE.into());
                }
            }
            "--no-restart" => options.restart = false,

            _ => return Err(USAGE.into()),
        }
    }
    Ok(options)
}

fn install(args: &[String]) -> Result<(), String> {
    let options = options(args)?;
    println!(
        "Oceans Setup: installs Oceans on this PC, replacing Linux and everything on its SSD.\n"
    );

    step("checking this PC");
    let disk = check_pc()?;
    println!(
        "    {} ({} GB), the EFI system partition {} at {}",
        disk.model,
        disk.size / 1_000_000_000,
        disk.esp.device,
        disk.esp.mount
    );

    // Know before you install (ADR-0161): every device, judged by the
    // table Oceans itself uses.
    step("checking this PC's hardware");
    let report = hardware::report(&pci_functions());
    for line in &report.lines {
        println!("  {line}");
    }
    let saved = std::env::current_dir().map(|dir| dir.join("oceans-hardware.txt"));
    if let Ok(path) = saved
        && std::fs::write(&path, report.text()).is_ok()
    {
        println!("    (this list is saved in {})", path.display());
    }
    if let Some(why) = report.assessment.refusal() {
        return Err(format!("not installed: {why}"));
    }

    step("getting the release");
    let (key, publisher) = match options.key {
        Some(key) => key,
        None => release::release_key()
            .map(|(k, p)| (k.to_string(), p.to_string()))
            .ok_or("no release key built in")?,
    };
    let say = |line: &str| println!("    {line}");
    let checked = match &options.from {
        Some(dir) => release::check(&key, &publisher, release::from_folder(dir), say)?,
        None => release::check(&key, &publisher, download, say)?,
    };

    let mut esp_guid = [0u8; 16];
    random(&mut esp_guid)?;
    esp_guid[7] = (esp_guid[7] & 0x0f) | 0x40;
    esp_guid[8] = (esp_guid[8] & 0x3f) | 0x80;
    let staged = release::staged(&checked, &esp_guid);
    let root = Path::new(&disk.esp.mount);
    let fallback_exists = root.join(oceans_install::STAGED_FALLBACK).exists();
    let files = linux_plan::files_for_esp(&staged, fallback_exists);
    let needed: u64 = files.iter().map(|(_, bytes)| bytes.len() as u64).sum();
    let why = linux_plan::refusals(&disk, needed);
    if !why.is_empty() {
        return Err(format!(
            "Oceans cannot be installed here:\n  - {}",
            why.join("\n  - ")
        ));
    }

    println!(
        "\nEverything on {} ({} GB) will be erased at the restart: Linux, its files, its apps.",
        disk.model,
        disk.size / 1_000_000_000
    );
    print!(
        "Type ERASE to install Oceans {}, anything else to stop: ",
        checked.name
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| format!("cannot read the answer: {e}"))?;
    if answer.trim() != "ERASE" {
        return Err("not confirmed".into());
    }

    step(&format!(
        "writing the installer to the EFI system partition ({} MiB)",
        needed.div_ceil(1 << 20)
    ));
    for (path, bytes) in &files {
        let target = root.join(path);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let _ = Command::new("sync").status();

    step("adding the boot entries: Oceans, and the installer for the next start");
    boot_entries(&disk.esp, &esp_guid)?;

    if !options.restart {
        println!("\nReady. Oceans' installer starts at the next restart.");
        return Ok(());
    }
    print!("\nReady. Press Enter to restart into the installer (Ctrl+C to restart later): ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    step("restarting");
    let restarted = Command::new("systemctl").arg("reboot").status();
    if !restarted.is_ok_and(|s| s.success()) {
        Command::new("reboot")
            .status()
            .map_err(|e| format!("cannot restart ({e}): restart the PC yourself"))?;
    }
    Ok(())
}

fn read(path: impl AsRef<Path>) -> Result<String, String> {
    let path = path.as_ref();
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Everything step 1 checks; the disk and its EFI system partition.
fn check_pc() -> Result<LinuxDisk, String> {
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if uid != "0" {
        return Err(format!("run it as root: {USAGE}"));
    }
    if !Path::new(linux_plan::EFIVARS).is_dir() {
        return Err(
            "this PC did not start Linux through UEFI (no /sys/firmware/efi/efivars)".into(),
        );
    }
    if let Ok(secure) = std::fs::read(linux_plan::efivar_file("SecureBoot"))
        && linux_plan::secure_boot_on(&secure)
    {
        return Err(
            "Secure Boot is on: turn it off in the firmware's settings first (Oceans' installer is not signed for it)"
                .into(),
        );
    }
    let (device, mount) = linux_plan::find_esp(&read("/proc/mounts")?)
        .ok_or("no EFI system partition is mounted (at /boot/efi, /efi or /boot)")?;
    let part = device
        .strip_prefix("/dev/")
        .ok_or_else(|| format!("{device} is not a disk partition"))?;
    let class = PathBuf::from("/sys/class/block").join(part);
    let sys = std::fs::canonicalize(&class).map_err(|e| format!("{}: {e}", class.display()))?;
    let sys = sys.to_string_lossy().to_string();
    let name = linux_plan::parent_disk(&sys)
        .ok_or_else(|| format!("which disk holds {device}?"))?
        .to_string();
    let block = PathBuf::from("/sys/block").join(&name);
    let number = |path: PathBuf| -> Result<u64, String> {
        linux_plan::sys_number(&read(&path)?).ok_or(format!("{} is not a number", path.display()))
    };
    let mut header = [0u8; 520];
    let gpt = std::fs::File::open(format!("/dev/{name}"))
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut header))
        .is_ok()
        && &header[512..520] == b"EFI PART";
    let free = Command::new("df")
        .args(["-B1", "--output=avail", &mount])
        .output()
        .ok()
        .and_then(|o| linux_plan::df_available(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(0);
    Ok(LinuxDisk {
        model: read(block.join("device").join("model"))
            .unwrap_or_default()
            .trim()
            .to_string(),
        logical_sector: number(block.join("queue").join("logical_block_size"))?,
        size: number(block.join("size"))? * 512,
        gpt,
        esp: Esp {
            number: number(class.join("partition"))? as u32,
            start: number(class.join("start"))?,
            sectors: number(class.join("size"))?,
            guid: partition_guid(&device)?,
            free,
            device,
            mount,
        },
        name,
    })
}

/// The partition's GPT GUID, from `/dev/disk/by-partuuid`.
fn partition_guid(device: &str) -> Result<[u8; 16], String> {
    let target = std::fs::canonicalize(device).map_err(|e| format!("{device}: {e}"))?;
    let dir = Path::new("/dev/disk/by-partuuid");
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    entries
        .flatten()
        .find(|entry| std::fs::canonicalize(entry.path()).is_ok_and(|p| p == target))
        .and_then(|entry| oceans_install::parse_guid(&entry.file_name().to_string_lossy()))
        .ok_or_else(|| format!("{device} has no GPT partition GUID"))
}

fn random(bytes: &mut [u8]) -> Result<(), String> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(bytes))
        .map_err(|e| format!("no randomness: {e}"))
}

/// Downloads a file of the latest release (curl, else wget).
fn download(name: &str) -> Result<Vec<u8>, String> {
    let url = format!("{}{name}", release::RELEASES);
    // Straight into memory: no file (as root, a file at a known path in a
    // shared temporary directory could be another user's symlink).
    let fetch = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .arg(&url)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| out.stdout)
    };
    fetch("curl", &["-fsSL"])
        .or_else(|| fetch("wget", &["-q", "-O", "-"]))
        .ok_or_else(|| format!("{url} could not be downloaded (with curl or wget)"))
}

fn read_variable(name: &str) -> Option<Vec<u8>> {
    let file = std::fs::read(linux_plan::efivar_file(name)).ok()?;
    linux_plan::efivar_value(&file).map(<[u8]>::to_vec)
}

/// Writes a firmware variable. efivarfs files are immutable until told
/// otherwise; the attributes and the value go in one write.
fn write_variable(name: &str, value: &[u8]) -> Result<(), String> {
    let path = linux_plan::efivar_file(name);
    if Path::new(&path).exists() {
        let _ = Command::new("chattr").args(["-i", &path]).status();
    }
    std::fs::write(&path, linux_plan::efivar_bytes(value))
        .map_err(|e| format!("the firmware refused to store {name}: {e}"))
}

/// "Oceans Setup" (this ESP, the next start only) and "Oceans" (the ESP
/// the installer will make, first in the order).
fn boot_entries(esp: &Esp, installed_esp: &[u8; 16]) -> Result<(), String> {
    let names: Vec<String> = std::fs::read_dir(linux_plan::EFIVARS)
        .map_err(|e| format!("{}: {e}", linux_plan::EFIVARS))?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    let mut used = linux_plan::existing_entries(names.iter().map(String::as_str));
    let order = efi::boot_order(&read_variable("BootOrder").unwrap_or_default());
    used.extend(&order);
    let [installer, oceans] = linux_plan::free_entries(&used, 2)[..] else {
        return Err("the firmware has no room for another boot entry".into());
    };
    let here = efi::Partition {
        number: esp.number,
        first_lba: esp.start,
        sectors: esp.sectors,
        guid: esp.guid,
    };
    let installed = efi::Partition {
        number: 1,
        first_lba: oceans_install::ALIGN,
        sectors: oceans_install::ESP_MIB * 2048,
        guid: *installed_esp,
    };
    write_variable(
        &efi::entry_name(installer),
        &efi::load_option(
            "Oceans Setup",
            &efi::device_path(&here, "\\EFI\\oceans\\BOOTX64.EFI"),
        ),
    )?;
    write_variable(
        &efi::entry_name(oceans),
        &efi::load_option(
            "Oceans",
            &efi::device_path(&installed, "\\EFI\\BOOT\\BOOTX64.EFI"),
        ),
    )?;
    let mut new_order = vec![oceans];
    new_order.extend(order);
    write_variable("BootOrder", &efi::encode_order(&new_order))?;
    write_variable("BootNext", &installer.to_le_bytes())?;
    println!(
        "    Boot{oceans:04X} \"Oceans\" first; Boot{installer:04X} \"Oceans Setup\" for the next start"
    );
    Ok(())
}

/// Every PCI function Linux lists (`/sys/bus/pci/devices/*`).
fn pci_functions() -> Vec<oceans_hardware::assess::Function> {
    let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let field = |name: &str| std::fs::read_to_string(path.join(name)).ok();
            hardware::from_sysfs(&field("vendor")?, &field("device")?, &field("class")?)
        })
        .collect()
}
