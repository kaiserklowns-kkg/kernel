//! Secure Boot (ADR-0091, ADR-0092): a USB image whose boot chain
//! firmware can verify, updates for it, and the test that both work.
//!
//! - **The image** ([`write_secure_usb_image`]): GPT entry 1, the EFI system
//!   partition, holds the **selector**: Limine, signed, with a
//!   configuration that never changes (its hash enrolled in it) and starts
//!   the slot at entry 2 ("Oceans") or entry 3 ("previous"). Each **slot**
//!   is a partition of its own with its own signed Limine, whose
//!   configuration names the slot's kernel and boot archive by BLAKE2B.
//! - **Updates** carry a slot's signed Limine and configuration
//!   ([`slot_boot_files`]); `update` writes them into the slot not starting
//!   first and swaps the two entries (ADR-0092).
//! - **`cargo xtask smoke-secure-boot`**: on QEMU's Secure Boot firmware
//!   (OVMF with SMM), a fresh variable store gets the development
//!   certificate (`oceans-enroll`, in Setup Mode); then the signed image
//!   must boot, install a signed update and start it, while an unsigned
//!   selector, an unsigned slot Limine and a changed kernel must not
//!   start.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use blake2::{Blake2b512, Digest};
use oceans_dev::secure_boot::SecureBootKey;

use super::{
    ENROLL_PACKAGE, KERNEL_PACKAGE, Profile, Result, Setup, build_image_for, cargo, find_qemu,
    release, root, run_command,
};

pub const UEFI_TARGET: &str = "x86_64-unknown-uefi";

/// The development Secure Boot key (public, like the development seed).
const DEV_KEY: &str = include_str!("../../keys/oceans-dev-secure-boot.key");
/// Where Limine keeps the configuration's hash in its executable.
const CONFIG_MARKER: &[u8] = b"++CONFIG_B2SUM_SIGNATURE++";
/// Written on a Secure Boot image's boot partition: `update` then updates
/// through the slot partitions (ADR-0092).
pub const MARKER_FILE: &str = "boot/secure-boot";
/// The certificate, for enrolling from the firmware's setup screens.
pub const CERTIFICATE_FILE: &str = "oceans-secure-boot.cer";

pub fn development_key() -> Result<SecureBootKey> {
    SecureBootKey::from_file(DEV_KEY)
}

fn blake2b(bytes: &[u8]) -> String {
    Blake2b512::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Writes the configuration's hash into Limine's executable.
pub fn enroll_config(limine: &mut [u8], config: &[u8]) -> Result {
    let at = limine
        .windows(CONFIG_MARKER.len())
        .position(|w| w == CONFIG_MARKER)
        .ok_or("Limine's executable has no place for the configuration's hash")?
        + CONFIG_MARKER.len();
    let slot = limine
        .get_mut(at..at + 128)
        .ok_or("Limine's executable is cut short")?;
    slot.copy_from_slice(blake2b(config).as_bytes());
    Ok(())
}

/// The selector's configuration (ADR-0092): it never changes, so its
/// enrolled hash never does. "Oceans" starts whichever slot partition is
/// GPT entry 2, "previous" entry 3; an update swaps the two entries.
/// Firmware checks each slot's Limine before it runs (`protocol: efi`).
pub const SELECTOR_CONFIG: &str = "serial: yes\ntimeout: 3\n\n\
    /Oceans\n    protocol: efi\n    path: boot(2):/EFI/BOOT/BOOTX64.EFI\n\n\
    /Oceans (previous)\n    protocol: efi\n    path: boot(3):/EFI/BOOT/BOOTX64.EFI\n";

/// A slot's files (ADR-0092), on its own partition.
pub const SLOT_KERNEL: &str = "boot/oceans-kernel";
pub const SLOT_INITRD: &str = "boot/initrd";
pub const SLOT_RELEASE: &str = "release";
/// In an update package: the slot's signed Limine and its configuration.
pub const UPDATE_EFI: &str = "BOOTX64.EFI";
pub const UPDATE_CONFIG: &str = "limine.conf";

/// A slot's configuration: its own kernel and boot archive, by hash,
/// started at once (the selector had the menu).
pub fn slot_config(release: &str, kernel: &[u8], initrd: &[u8]) -> String {
    format!(
        "serial: yes\ntimeout: 0\n\n/Oceans {release}\n    protocol: limine\n    \
         path: boot():/{SLOT_KERNEL}#{}\n    module_path: boot():/{SLOT_INITRD}#{}\n",
        blake2b(kernel),
        blake2b(initrd)
    )
}

/// Limine with `config`'s hash enrolled, signed with `key`.
pub fn signed_limine(config: &str, key: &SecureBootKey) -> Result<Vec<u8>> {
    let path = super::limine_dir().join("BOOTX64.EFI");
    let mut limine = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    enroll_config(&mut limine, config.as_bytes())?;
    key.sign(&limine)
}

/// What a release puts on a slot partition, beside its kernel and boot
/// archive: its signed Limine and that Limine's configuration. Update
/// packages for Secure Boot systems carry them too.
pub fn slot_boot_files(
    release: &str,
    kernel: &[u8],
    initrd: &[u8],
    key: &SecureBootKey,
) -> Result<(Vec<u8>, String)> {
    let config = slot_config(release, kernel, initrd);
    Ok((signed_limine(&config, key)?, config))
}

/// Writes `bytes` to `dir/relative`, making folders on the way.
fn put(dir: &Path, relative: &str, bytes: &[u8]) -> Result {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// The Secure Boot USB image (ADR-0092) from the release built in `esp`
/// (slot `boot/a`), signed with `key`, written to [`super::hardware::USB_IMAGE`]:
/// 1. the EFI system partition: the selector, the marker, the certificate,
///    room for update files;
/// 2. slot A: the release;
/// 3. slot B: empty until an update.
pub fn write_secure_usb_image(esp: &Path, key: &SecureBootKey) -> Result {
    let slot = esp.join("boot").join("a");
    let read = |name: &str| fs::read(slot.join(name)).map_err(|e| format!("{name}: {e}"));
    let (kernel, initrd) = (read(KERNEL_PACKAGE)?, read("initrd")?);
    let release = String::from_utf8(read("release")?).map_err(|_| "the release is not text")?;
    let release = release.trim();

    let work = root().join("build").join("secure-boot-image");
    let _ = fs::remove_dir_all(&work);
    let (selector, slot_a) = (work.join("esp"), work.join("slot"));
    put(
        &selector,
        "EFI/BOOT/BOOTX64.EFI",
        &signed_limine(SELECTOR_CONFIG, key)?,
    )?;
    put(&selector, "limine.conf", SELECTOR_CONFIG.as_bytes())?;
    put(
        &selector,
        MARKER_FILE,
        format!(
            "Signed for Secure Boot (ADR-0091, ADR-0092) by the certificate {}\n",
            key.fingerprint()
        )
        .as_bytes(),
    )?;
    put(&selector, CERTIFICATE_FILE, &key.certificate)?;
    let (efi, config) = slot_boot_files(release, &kernel, &initrd, key)?;
    put(&slot_a, "EFI/BOOT/BOOTX64.EFI", &efi)?;
    put(&slot_a, "limine.conf", config.as_bytes())?;
    put(&slot_a, SLOT_KERNEL, &kernel)?;
    put(&slot_a, SLOT_INITRD, &initrd)?;
    put(&slot_a, SLOT_RELEASE, format!("{release}\n").as_bytes())?;

    let release_mib = ((kernel.len() + initrd.len()) >> 20) as u64;
    let children = |dir: &Path| -> Result<Vec<PathBuf>> {
        fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .map(|entry| entry.map(|e| e.path()).map_err(|e| e.to_string()))
            .collect()
    };
    let slot_mib = (release_mib * 2 + 32).max(64);
    super::disk_image::gpt_image(
        &root().join(super::hardware::USB_IMAGE),
        &[
            super::disk_image::Volume {
                kind: oceans_gpt::EFI_SYSTEM,
                name: "EFI system partition",
                label: "OCEANS",
                // Room for an update file.
                mib: (release_mib + 48).max(64),
                sources: children(&selector)?,
            },
            super::disk_image::Volume {
                kind: oceans_gpt::OCEANS_SLOT,
                name: "Oceans slot",
                label: "OCEANS-A",
                mib: slot_mib,
                sources: children(&slot_a)?,
            },
            super::disk_image::Volume {
                kind: oceans_gpt::OCEANS_SLOT,
                name: "Oceans slot",
                label: "OCEANS-B",
                mib: slot_mib,
                sources: Vec::new(),
            },
        ],
        &kernel,
    )?;
    println!(
        "{}: signed for Secure Boot (certificate {}); Oceans {release} in slot A, slot B empty",
        super::hardware::USB_IMAGE,
        key.fingerprint()
    );
    Ok(())
}

// ---- The test -------------------------------------------------------------

/// QEMU's Secure Boot firmware and a variable store template for it.
fn secure_firmware(qemu: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut pairs = Vec::new();
    if let Some(dir) = qemu.parent() {
        let share = dir.join("share");
        pairs.push((
            share.join("edk2-x86_64-secure-code.fd"),
            share.join("edk2-i386-vars.fd"),
        ));
    }
    for (code, vars) in [
        (
            "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ),
        (
            "/usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd",
            "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
        ),
        (
            "/usr/share/qemu/edk2-x86_64-secure-code.fd",
            "/usr/share/qemu/edk2-i386-vars.fd",
        ),
    ] {
        pairs.push((PathBuf::from(code), PathBuf::from(vars)));
    }
    pairs
        .into_iter()
        .find(|(code, vars)| code.is_file() && vars.is_file())
        .ok_or_else(|| "QEMU's Secure Boot firmware (OVMF with SMM) not found".to_string())
}

/// What a Secure Boot test machine boots from.
#[derive(Clone, Copy)]
enum Boot<'a> {
    /// A partitioned USB image file, as a USB stick: the system sees it at
    /// `/usb`, as on a real PC.
    UsbImage(&'a str),
    /// A folder QEMU serves as a FAT disk, on IDE (the enrolment program
    /// alone; QEMU's folder emulation does not boot over usb-storage).
    Folder(&'a str),
}

/// A QEMU machine on the Secure Boot firmware, booting `boot`, with a
/// blank NVMe disk for the root filesystem if given.
fn secure_machine(
    qemu: &Path,
    code: &Path,
    vars: &Path,
    boot: Boot<'_>,
    nvme: Option<&str>,
) -> Command {
    let mut cmd = Command::new(qemu);
    cmd.current_dir(root())
        .args([
            "-machine",
            "q35,smm=on",
            "-cpu",
            "max",
            "-m",
            "512M",
            "-smp",
            "2",
        ])
        .args(["-global", "driver=cfi.pflash01,property=secure,value=on"])
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            code.display()
        ))
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,unit=1,file={}",
            vars.display()
        ))
        .args(["-no-reboot", "-serial", "stdio", "-display", "none"])
        .args([
            "-device",
            "qemu-xhci,id=usb",
            "-device",
            "usb-kbd,bus=usb.0,port=1",
        ]);
    match boot {
        Boot::UsbImage(image) => {
            cmd.arg("-drive")
                .arg(format!("if=none,id=stick,format=raw,file={image}"))
                .args([
                    "-device",
                    "usb-storage,bus=usb.0,port=2,drive=stick,bootindex=0",
                ]);
        }
        Boot::Folder(folder) => {
            cmd.arg("-drive")
                .arg(format!("format=raw,file=fat:rw:{folder}"));
        }
    }
    if let Some(nvme) = nvme {
        cmd.arg("-drive")
            .arg(format!("if=none,id=nvme0,format=raw,file={nvme}"))
            .args(["-device", "nvme,serial=oceans-sb,drive=nvme0"]);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd
}

/// Runs `machine` until a line contains `until`; then, for each step,
/// types its text and waits for a line containing its answer. Stops when
/// an answer does not come within `timeout` of the start, or the machine
/// exits. Returns whether every wait was met, and the output.
fn watch(
    mut machine: Command,
    until: &str,
    steps: &[(&str, &str)],
    timeout: Duration,
) -> Result<(bool, String)> {
    let mut child: Child = machine
        .spawn()
        .map_err(|e| format!("failed to start QEMU: {e}"))?;
    let mut input = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let (lines_tx, lines) = mpsc::channel::<String>();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        while let Ok(n) = reader.read_until(b'\n', &mut line) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            line.clear();
            if lines_tx.send(text).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    let mut target = until;
    let mut pending = steps.iter();
    let found = loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(wait) {
            Ok(line) => {
                print!("  | {line}");
                output.push_str(&line);
                if line.contains(target) {
                    match pending.next() {
                        Some((text, after)) => {
                            for byte in text.bytes() {
                                let _ = input.write_all(&[byte]).and_then(|()| input.flush());
                                thread::sleep(Duration::from_millis(5));
                            }
                            target = after;
                        }
                        None => break true,
                    }
                }
            }
            Err(_) => break false,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    Ok((found, output))
}

/// `update`, run from the shell with the stick's disk (ADR-0092).
fn update(args: &str) -> String {
    format!("run update out use:fs use:usbdisk -- {args}\r\n")
}

/// Reads `name` in directory `path` (from the root) of `fat`.
fn read_fat_file<D: oceans_fat::Disk>(
    fat: &mut oceans_fat::Fat<D>,
    path: &[&str],
    name: &str,
) -> Result<(oceans_fat::NodeId, Vec<u8>)> {
    // Each directory held while the next name is looked up in it: a node
    // nothing holds can be reused. The caller releases the last.
    let mut dir = oceans_fat::ROOT;
    for part in path {
        let child = fat
            .lookup(dir, part)
            .map_err(|e| format!("{part}: {e:?}"))?;
        fat.retain(child).map_err(|e| format!("{part}: {e:?}"))?;
        fat.release(dir);
        dir = child;
    }
    let file = fat
        .lookup(dir, name)
        .map_err(|e| format!("{name}: {e:?}"))?;
    let size = fat.size(file).map_err(|e| format!("{name}: {e:?}"))?;
    let mut bytes = vec![0u8; size as usize];
    fat.read_file(file, 0, &mut bytes)
        .map_err(|e| format!("{name}: {e:?}"))?;
    Ok((dir, bytes))
}

/// Replaces `name` in directory `path` of partition `entry` of the USB
/// image with `change(old bytes)`; returns the old bytes.
fn change_on_stick(
    entry: usize,
    path: &[&str],
    name: &str,
    change: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Result<Vec<u8>> {
    let image = root().join(super::hardware::USB_IMAGE);
    let mut fat = super::disk_image::open_partition(&image, entry)?;
    let (dir, old) = read_fat_file(&mut fat, path, name)?;
    super::disk_image::put_file(&mut fat, dir, name, &change(&old))?;
    fat.release(dir);
    fat.sync().map_err(|e| format!("{name}: {e:?}"))?;
    Ok(old)
}

/// `cargo xtask smoke-secure-boot` (module docs).
pub fn smoke_secure_boot(profile: Profile) -> Result {
    let qemu = find_qemu()?;
    let (code, template) = secure_firmware(&qemu)?;
    let key = development_key()?;
    let build = root().join("build");
    let vars = build.join("secure-boot-vars.fd");
    fs::copy(&template, &vars).map_err(|e| format!("cannot copy {}: {e}", template.display()))?;

    // 1. The certificate goes in, in Setup Mode.
    run_command(cargo().args([
        "build",
        "--release",
        "-p",
        ENROLL_PACKAGE,
        "--target",
        UEFI_TARGET,
    ]))?;
    let enroll = root()
        .join("target")
        .join(UEFI_TARGET)
        .join("release")
        .join(format!("{ENROLL_PACKAGE}.efi"));
    let enroll_esp = build.join("secure-boot-enroll");
    let _ = fs::remove_dir_all(&enroll_esp);
    let boot = enroll_esp.join("EFI").join("BOOT");
    fs::create_dir_all(&boot).map_err(|e| e.to_string())?;
    fs::copy(&enroll, boot.join("BOOTX64.EFI"))
        .map_err(|e| format!("{}: {e}", enroll.display()))?;
    println!("secure boot 1 of 6: enrolling the development certificate in Setup Mode");
    let (enrolled, _) = watch(
        secure_machine(
            &qemu,
            &code,
            &vars,
            Boot::Folder("build/secure-boot-enroll"),
            None,
        ),
        "oceans-enroll: enrolled",
        &[],
        Duration::from_secs(120),
    )?;
    if !enrolled {
        return Err("the firmware did not take the development certificate".into());
    }

    // 2. The signed image boots, and takes a signed update (ADR-0092).
    let release_key = release::test_release_key();
    let esp = build_image_for(
        profile,
        None,
        Setup::Hardware,
        &release::ImageKeys::release(&release_key),
    )?;
    write_secure_usb_image(&esp, &key)?;
    let image = super::hardware::USB_IMAGE;
    let signed_update = super::hardware::system_package(
        &esp,
        "0.1.1",
        &release_key,
        "A system update (the Secure Boot smoke test's)",
        Some(&key),
    )?;
    let plain_update = super::hardware::system_package(
        &esp,
        "0.1.1",
        &release_key,
        "A system update without Secure Boot files",
        None,
    )?;
    {
        let mut fat = super::disk_image::open_partition(&root().join(image), 1)?;
        for (name, bytes) in [
            ("oceans-0.1.1.opk", &signed_update),
            ("plain-0.1.1.opk", &plain_update),
        ] {
            super::disk_image::put_file(&mut fat, oceans_fat::ROOT, name, bytes)?;
        }
        fat.sync().map_err(|e| format!("{image}: {e:?}"))?;
    }
    let nvme = "build/secure-boot-nvme.img";
    super::prepare_blank(nvme, 64 << 20, true)?;
    println!("secure boot 2 of 6: the signed image boots, and installs a signed update");
    let status = update("status");
    let plain = update("apply /usb/plain-0.1.1.opk");
    let apply = update("apply /usb/oceans-0.1.1.opk");
    let (updated, output) = watch(
        secure_machine(&qemu, &code, &vars, Boot::UsbImage(image), Some(nvme)),
        "oceans> ",
        &[
            (&status, "update: previous: slot B is empty"),
            (
                &plain,
                "update: refused: this system boots with Secure Boot",
            ),
            (&apply, "installed in slot B"),
        ],
        Duration::from_secs(300),
    )?;
    if !updated {
        return Err("the signed image did not boot and install the signed update".into());
    }
    if !output.contains("update: running Oceans 0.1.0 alpha (Secure Boot, slot A)") {
        return Err("`update status` did not name slot A as running".into());
    }

    // 3. The next boot starts the update, from slot B; slot A stays as the
    //    previous release.
    println!("secure boot 3 of 6: the update starts, the previous release stays");
    let (moved, output) = watch(
        secure_machine(&qemu, &code, &vars, Boot::UsbImage(image), Some(nvme)),
        "oceans> ",
        &[(&status, "update: previous: slot A (0.1.0 alpha)")],
        Duration::from_secs(240),
    )?;
    if !moved || !output.contains("update: running Oceans 0.1.1 alpha (Secure Boot, slot B)") {
        return Err("the update did not start from slot B with slot A as the previous".into());
    }

    // 4. The selector unsigned: the firmware refuses it.
    let unsigned = fs::read(super::limine_dir().join("BOOTX64.EFI")).map_err(|e| e.to_string())?;
    let selector = change_on_stick(1, &["EFI", "BOOT"], "BOOTX64.EFI", |_| unsigned)?;
    println!("secure boot 4 of 6: an unsigned selector must not start");
    // Firmware refuses it, then may try the stick's other partitions: a
    // slot's own Limine is signed and starts its own release, which is
    // still a checked chain. What must not appear is the selector's menu.
    let (started, output) = watch(
        secure_machine(&qemu, &code, &vars, Boot::UsbImage(image), Some(nvme)),
        "Oceans (previous)",
        &[],
        Duration::from_secs(45),
    )?;
    if started || !output.contains("Access Denied") {
        return Err("an unsigned selector started under Secure Boot".into());
    }

    // 5. The selector signed again, the Limine of the slot that starts
    //    first unsigned: the firmware refuses it when the selector starts
    //    it.
    change_on_stick(1, &["EFI", "BOOT"], "BOOTX64.EFI", |_| selector)?;
    let unsigned = fs::read(super::limine_dir().join("BOOTX64.EFI")).map_err(|e| e.to_string())?;
    let slot_limine = change_on_stick(2, &["EFI", "BOOT"], "BOOTX64.EFI", |_| unsigned)?;
    println!("secure boot 5 of 6: a slot's unsigned Limine must not start");
    let (started, _) = watch(
        secure_machine(&qemu, &code, &vars, Boot::UsbImage(image), Some(nvme)),
        "Oceans 0.1.0 on x86_64",
        &[],
        Duration::from_secs(45),
    )?;
    if started {
        return Err("the selector started an unsigned Limine under Secure Boot".into());
    }

    // 6. Every Limine signed again, but the kernel of the slot that starts
    //    first (entry 2: B, since the update) changed: its Limine refuses
    //    it.
    change_on_stick(2, &["EFI", "BOOT"], "BOOTX64.EFI", |_| slot_limine)?;
    change_on_stick(2, &["boot"], "oceans-kernel", |old| {
        let mut bytes = old.to_vec();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x5a;
        bytes
    })?;
    println!("secure boot 6 of 6: a changed kernel must not start");
    let (started, _) = watch(
        secure_machine(&qemu, &code, &vars, Boot::UsbImage(image), Some(nvme)),
        "Oceans 0.1.0 on x86_64",
        &[],
        Duration::from_secs(45),
    )?;
    if started {
        return Err("a changed kernel started under Secure Boot".into());
    }
    println!(
        "secure boot smoke test passed: the signed chain booted and updated itself; an \
         unsigned selector, an unsigned slot Limine and a changed kernel did not start"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_configurations_name_their_files_by_hash() {
        let conf = slot_config("0.1.0 alpha", b"kernel", b"archive");
        assert!(conf.starts_with("serial: yes\ntimeout: 0\n\n/Oceans 0.1.0 alpha\n"));
        assert!(conf.contains(&format!(
            "\n    path: boot():/boot/oceans-kernel#{}\n",
            blake2b(b"kernel")
        )));
        assert!(conf.contains(&format!(
            "\n    module_path: boot():/boot/initrd#{}\n",
            blake2b(b"archive")
        )));
        // Each line as Limine reads it: no stray indentation.
        assert!(conf.lines().all(|l| l.is_empty()
            || !l.starts_with(' ')
            || l.starts_with("    ") && !l.starts_with("     ")));
        assert!(SELECTOR_CONFIG.lines().all(|l| !l.starts_with(" /")));
        // The selector names partitions, never files by hash: it outlives
        // every release.
        assert!(!SELECTOR_CONFIG.contains('#'));
        assert!(SELECTOR_CONFIG.contains("path: boot(2):/EFI/BOOT/BOOTX64.EFI"));
        assert!(SELECTOR_CONFIG.contains("path: boot(3):/EFI/BOOT/BOOTX64.EFI"));
    }

    #[test]
    fn enrolls_the_configuration_hash() {
        // BLAKE2b-512 of "abc" (RFC 7693, appendix A).
        assert!(blake2b(b"abc").starts_with("ba80a53f981c4d0d6a2797b69f12f6e9"));
        let mut limine = b"head ++CONFIG_B2SUM_SIGNATURE++".to_vec();
        limine.extend_from_slice(&[b'0'; 128]);
        limine.extend_from_slice(b" tail");
        enroll_config(&mut limine, b"config").unwrap();
        let at = limine.len() - 5 - 128;
        assert_eq!(&limine[at..at + 128], blake2b(b"config").as_bytes());
        assert!(limine.ends_with(b" tail"));
        assert!(enroll_config(&mut b"no marker".to_vec(), b"x").is_err());
    }

    #[test]
    fn the_development_key_signs_limine() {
        let path = super::super::limine_dir().join("BOOTX64.EFI");
        let Ok(mut limine) = fs::read(&path) else {
            // Limine is fetched by `cargo xtask limine`; CI runs it first.
            return;
        };
        let key = development_key().unwrap();
        enroll_config(&mut limine, b"timeout: 3\n").unwrap();
        let signed = key.sign(&limine).unwrap();
        oceans_dev::secure_boot::verify(&signed, &key).unwrap();
    }
}
