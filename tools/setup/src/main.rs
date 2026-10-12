//! Oceans Setup (ADR-0128): installs Oceans from the web over a Windows PC,
//! replacing Windows, without a USB stick.
//!
//! On Windows it is a small window that, step by step:
//! 1. checks this PC: UEFI, Secure Boot off, Windows on an NVMe SSD with
//!    room to free;
//! 2. downloads the latest release's checksums, their signature and the
//!    system package, and checks them against the Oceans release key
//!    (built in, as published in the repository);
//! 3. asks the user to type ERASE: Windows and everything on the SSD go;
//! 4. shrinks C:, makes a small FAT32 partition with the release and
//!    Limine, adds the UEFI boot entries ("Oceans", first; the installer,
//!    for the next start only) and restarts. Oceans' installer (ADR-0127)
//!    does the rest.
//!
//! Until the restart nothing is lost: cancelled, Windows still starts, and
//! the small partition can be deleted.
//!
//! `OceansSetup --dry-run OUT --from DIR [--key HEX PUBLISHER] [--esp GUID]` (any
//! system) checks a release in DIR and writes the staging files into OUT,
//! changing nothing else: for tests and for the curious.

#![cfg_attr(windows, windows_subsystem = "windows")]

// Used by the Windows path; elsewhere only their tests run.
#[cfg_attr(not(windows), allow(dead_code))]
mod efi;
#[cfg(windows)]
mod gui;
#[cfg_attr(not(windows), allow(dead_code))]
mod plan;
#[cfg_attr(not(windows), allow(dead_code))]
mod release;
#[cfg(windows)]
mod windows;

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--dry-run") {
        std::process::exit(match dry_run(&args[1..]) {
            Ok(()) => 0,
            Err(problem) => {
                eprintln!("OceansSetup: {problem}");
                1
            }
        });
    }
    #[cfg(windows)]
    gui::run();
    #[cfg(not(windows))]
    {
        eprintln!(
            "Oceans Setup runs on Windows; elsewhere only: \
             OceansSetup --dry-run OUT --from DIR [--key HEX]"
        );
        std::process::exit(2);
    }
}

/// Checks a local release and writes the staging files, nothing else.
fn dry_run(args: &[String]) -> Result<(), String> {
    let usage = "usage: OceansSetup --dry-run OUT --from DIR [--key HEX PUBLISHER] [--esp GUID]";
    let (out, rest) = args.split_first().ok_or(usage)?;
    let mut from = None;
    let mut key =
        release::release_key().map(|(key, publisher)| (key.to_string(), publisher.to_string()));
    // By default a GUID of zeros: nothing will boot from it.
    let mut esp = [0u8; 16];
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--from" => from = words.next().map(PathBuf::from),
            "--key" => {
                key = words.next().cloned().zip(words.next().cloned());
                if key.is_none() {
                    return Err(usage.into());
                }
            }
            "--esp" => {
                esp = words
                    .next()
                    .and_then(|text| oceans_install::parse_guid(text))
                    .ok_or(usage)?;
            }
            _ => return Err(usage.into()),
        }
    }
    let from = from.ok_or(usage)?;
    let (key, publisher) = key.ok_or("no release key")?;
    let checked = release::check(&key, &publisher, release::from_folder(&from), |line| {
        println!("{line}")
    })?;
    let files = release::staged(&checked, &esp);
    release::write_staged(&PathBuf::from(out), &files)?;
    println!(
        "staged in {out}: {} files; nothing else was changed",
        files.len()
    );
    Ok(())
}

/// The install itself, on Windows: the staging partition, the boot
/// entries, the restart. `say` reports progress.
#[cfg(windows)]
pub fn install_now(
    disk: &plan::SystemDisk,
    checked: &release::Release,
    say: &dyn Fn(&str),
) -> Result<(), String> {
    windows::firmware_access()?;
    let mut esp = [0u8; 16];
    getrandom::getrandom(&mut esp).map_err(|_| "no randomness")?;
    esp[7] = (esp[7] & 0x0f) | 0x40;
    esp[8] = (esp[8] & 0x3f) | 0x80;

    say("making room: shrinking C: and making a partition for the installer");
    let staging = plan::parse_staging(&windows::powershell(&plan::staging_script(disk.number))?)?;
    say(&format!(
        "writing the installer to the new partition ({}:)",
        staging.letter
    ));
    let root = PathBuf::from(format!("{}:\\", staging.letter));
    release::write_staged(&root, &release::staged(checked, &esp))?;
    let _ = windows::powershell(&plan::hide_script(
        disk.number,
        staging.number,
        staging.letter,
    ));

    say("adding the boot entries: Oceans, and the installer for the next start");
    let order = efi::boot_order(&windows::read_variable("BootOrder").unwrap_or_default());
    let mut used = order.clone();
    let mut free = Vec::new();
    for number in 0..=0xffffu16 {
        if free.len() == 2 {
            break;
        }
        if !used.contains(&number) && windows::read_variable(&efi::entry_name(number)).is_none() {
            free.push(number);
            used.push(number);
        }
    }
    let [installer, oceans] = free[..] else {
        return Err("the firmware has no room for another boot entry".into());
    };
    let staged_partition = efi::Partition {
        number: staging.number,
        first_lba: staging.offset / 512,
        sectors: staging.size / 512,
        guid: staging.guid,
    };
    let installed_esp = efi::Partition {
        number: 1,
        first_lba: oceans_install::ALIGN,
        sectors: oceans_install::ESP_MIB * 2048,
        guid: esp,
    };
    windows::write_variable(
        &efi::entry_name(installer),
        &efi::load_option(
            "Oceans Setup",
            &efi::device_path(&staged_partition, "\\EFI\\oceans\\BOOTX64.EFI"),
        ),
    )?;
    windows::write_variable(
        &efi::entry_name(oceans),
        &efi::load_option(
            "Oceans",
            &efi::device_path(&installed_esp, "\\EFI\\BOOT\\BOOTX64.EFI"),
        ),
    )?;
    let mut new_order = vec![oceans];
    new_order.extend(order);
    windows::write_variable("BootOrder", &efi::encode_order(&new_order))?;
    windows::write_variable("BootNext", &installer.to_le_bytes())?;
    say("restarting into the installer");
    windows::restart()
}
