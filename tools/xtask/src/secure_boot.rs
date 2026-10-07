//! Secure Boot (ADR-0091): an image whose boot chain firmware can verify,
//! and the test that it does.
//!
//! - **Signing an image** ([`secure_esp`]): Limine's configuration names the
//!   kernel and the boot archive with their BLAKE2B hashes; the
//!   configuration's own hash is written into Limine's executable
//!   (`enroll-config`); the executable is signed with the Secure Boot key
//!   (Authenticode). Firmware checks the signature, Limine the
//!   configuration, the configuration the files.
//! - **`cargo xtask smoke-secure-boot`**: on QEMU's Secure Boot firmware
//!   (OVMF with SMM), a fresh variable store gets the development
//!   certificate (`oceans-enroll`, in Setup Mode); then the signed image
//!   must boot to the shell, while an unsigned Limine and a changed kernel
//!   must not.

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
/// Written on a Secure Boot image's boot partition: `update` refuses to
/// rewrite a configuration whose hash is enrolled.
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

/// The configuration with every `path:` and `module_path:` under `boot():/`
/// followed by its file's BLAKE2B, and Limine's output also on the serial
/// line (where tests and diagnostics read it).
pub fn hashed_config(config: &str, esp: &Path) -> Result<String> {
    let mut out = String::from("serial: yes\n");
    for line in config.lines() {
        let trimmed = line.trim_start();
        let file = ["path: boot():/", "module_path: boot():/"]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix));
        match file {
            Some(file) if !file.contains('#') => {
                let bytes = fs::read(esp.join(file)).map_err(|e| format!("{file}: {e}"))?;
                out.push_str(&format!("{line}#{}\n", blake2b(&bytes)));
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// Makes the boot partition in `esp` a Secure Boot one (module docs).
pub fn secure_esp(esp: &Path, key: &SecureBootKey) -> Result {
    let configs: Vec<PathBuf> = [
        esp.join("limine.conf"),
        esp.join("boot").join("limine").join("limine.conf"),
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .collect();
    // Limine reads the first it finds: every copy is the same text, so the
    // one enrolled hash fits whichever that is.
    let first = configs
        .first()
        .ok_or("the boot partition has no limine.conf")?;
    let text = fs::read_to_string(first).map_err(|e| format!("{}: {e}", first.display()))?;
    let hashed = hashed_config(&text, esp)?;
    for path in &configs {
        fs::write(path, &hashed).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let efi = esp.join("EFI").join("BOOT").join("BOOTX64.EFI");
    let mut limine = fs::read(&efi).map_err(|e| format!("{}: {e}", efi.display()))?;
    enroll_config(&mut limine, hashed.as_bytes())?;
    let signed = key.sign(&limine)?;
    fs::write(&efi, signed).map_err(|e| format!("{}: {e}", efi.display()))?;
    fs::write(
        esp.join(MARKER_FILE),
        format!(
            "Signed for Secure Boot (ADR-0091) by the certificate {}\n",
            key.fingerprint()
        ),
    )
    .map_err(|e| format!("cannot write the Secure Boot marker: {e}"))?;
    fs::write(esp.join(CERTIFICATE_FILE), &key.certificate)
        .map_err(|e| format!("cannot write the certificate: {e}"))?;
    println!(
        "boot partition signed for Secure Boot: certificate {}",
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
    /// A partitioned USB image file (`write_usb_image`), as a USB stick: the
    /// system sees it at `/usb`, as on a real PC.
    UsbImage(&'a str),
    /// A folder QEMU serves as a FAT disk, on IDE: where the FAT tools are
    /// missing. (QEMU's folder emulation does not boot over usb-storage.)
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

/// Packs `esp` as the USB image when the FAT tools are there (then the
/// system sees its stick at `/usb`); else boots the folder itself.
fn boot_disk(esp: &Path) -> Boot<'static> {
    match super::hardware::write_usb_image(esp) {
        Ok(()) => Boot::UsbImage(super::hardware::USB_IMAGE),
        Err(why) => {
            println!("no USB image ({why}): booting the folder itself, as an IDE disk");
            Boot::Folder("build/esp")
        }
    }
}

/// Runs `machine` until a line contains `until` (then types `then`, if
/// any, and waits for `after`), or it exits, or `timeout` passes. Returns
/// what it printed.
fn watch(
    mut machine: Command,
    until: &str,
    typed: Option<(&str, &str)>,
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
    let mut pending = typed;
    let found = loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(wait) {
            Ok(line) => {
                print!("  | {line}");
                output.push_str(&line);
                if line.contains(target) {
                    match pending.take() {
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
    println!("secure boot 1 of 4: enrolling the development certificate in Setup Mode");
    let (enrolled, _) = watch(
        secure_machine(
            &qemu,
            &code,
            &vars,
            Boot::Folder("build/secure-boot-enroll"),
            None,
        ),
        "oceans-enroll: enrolled",
        None,
        Duration::from_secs(120),
    )?;
    if !enrolled {
        return Err("the firmware did not take the development certificate".into());
    }

    // 2. The signed image boots, all the way to the shell.
    let esp = build_image_for(
        profile,
        None,
        Setup::Hardware,
        &release::ImageKeys::development()?,
    )?;
    secure_esp(&esp, &key)?;
    let nvme = "build/secure-boot-nvme.img";
    super::prepare_blank(nvme, 64 << 20, true)?;
    let signed_efi =
        fs::read(esp.join("EFI").join("BOOT").join("BOOTX64.EFI")).map_err(|e| e.to_string())?;
    println!("secure boot 2 of 4: the signed image, with Secure Boot on");
    let disk = boot_disk(&esp);
    // With its stick at /usb, `update apply` must refuse (ADR-0091).
    let typed = matches!(disk, Boot::UsbImage(_)).then_some((
        "run update out use:fs -- apply /usb/none.opk\r\n",
        "update: this system boots with Secure Boot",
    ));
    let (booted, output) = watch(
        secure_machine(&qemu, &code, &vars, disk, Some(nvme)),
        "oceans> ",
        typed,
        Duration::from_secs(240),
    )?;
    if !booted {
        return Err("the signed image did not boot to the shell under Secure Boot".into());
    }
    if output.contains("Access Denied") {
        return Err("the firmware reported a refusal while booting the signed image".into());
    }
    if typed.is_none() {
        println!(
            "(update's refusal not checked: no USB image without the FAT tools; CI checks it)"
        );
    }

    // 3. Limine unsigned: the firmware refuses it.
    let unsigned = super::limine_dir().join("BOOTX64.EFI");
    fs::copy(&unsigned, esp.join("EFI").join("BOOT").join("BOOTX64.EFI"))
        .map_err(|e| e.to_string())?;
    println!("secure boot 3 of 4: an unsigned Limine must not start");
    let disk = boot_disk(&esp);
    let (started, _) = watch(
        secure_machine(&qemu, &code, &vars, disk, Some(nvme)),
        "Oceans 0.1.0 on x86_64",
        None,
        Duration::from_secs(45),
    )?;
    if started {
        return Err("an unsigned Limine started under Secure Boot".into());
    }

    // 4. The signed Limine, but a changed kernel: Limine refuses it.
    fs::write(
        esp.join("EFI").join("BOOT").join("BOOTX64.EFI"),
        &signed_efi,
    )
    .map_err(|e| e.to_string())?;
    let kernel = esp.join("boot").join("a").join(KERNEL_PACKAGE);
    let mut bytes = fs::read(&kernel).map_err(|e| e.to_string())?;
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x5a;
    fs::write(&kernel, bytes).map_err(|e| e.to_string())?;
    println!("secure boot 4 of 4: a changed kernel must not start");
    let disk = boot_disk(&esp);
    let (started, _) = watch(
        secure_machine(&qemu, &code, &vars, disk, Some(nvme)),
        "Oceans 0.1.0 on x86_64",
        None,
        Duration::from_secs(45),
    )?;
    if started {
        return Err("a changed kernel started under Secure Boot".into());
    }
    println!(
        "secure boot smoke test passed: the signed chain booted; an unsigned Limine and a \
         changed kernel did not"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files_into_the_configuration() {
        let dir = std::env::temp_dir().join(format!("oceans-sb-{}", std::process::id()));
        let slot = dir.join("boot").join("a");
        fs::create_dir_all(&slot).unwrap();
        fs::write(slot.join(KERNEL_PACKAGE), b"kernel").unwrap();
        fs::write(slot.join("initrd"), b"archive").unwrap();
        let conf = format!(
            "timeout: 3\n\n/Oceans\n    protocol: limine\n    path: boot():/boot/a/{KERNEL_PACKAGE}\n    module_path: boot():/boot/a/initrd\n"
        );
        let hashed = hashed_config(&conf, &dir).unwrap();
        assert!(hashed.starts_with("serial: yes\ntimeout: 3\n"));
        assert!(hashed.contains(&format!(
            "    path: boot():/boot/a/{KERNEL_PACKAGE}#{}\n",
            blake2b(b"kernel")
        )));
        assert!(hashed.contains(&format!(
            "    module_path: boot():/boot/a/initrd#{}\n",
            blake2b(b"archive")
        )));
        // A missing file is an error, not an unhashed path.
        fs::remove_file(slot.join("initrd")).unwrap();
        assert!(hashed_config(&conf, &dir).is_err());
        let _ = fs::remove_dir_all(&dir);
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
