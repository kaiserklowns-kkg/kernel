//! `sysreport`: what Oceans found on this machine, for the hardware
//! compatibility matrix (ADR-0068): the CPU against the Tier 1 baseline,
//! memory, every PCI device with what Oceans gives it, USB devices and the
//! network. Needs `devices` and `sysinfo`; `use:usb` and `use:net` add
//! their parts.

#![no_std]
#![no_main]

use core::arch::x86_64::{__cpuid, CpuidResult};
use core::fmt::Write;

use oceans_abi::device::DeviceRecord;
use oceans_abi::sysinfo::{self, KernelInfo, MemoryInfo};
use oceans_hardware::{Cpuid, Support, lookup};
use oceans_net_proto::{Dotted, info};
use oceans_rt::{Out, Start};
use oceans_usb::service::{self, Record};
use utils::{EXIT_FAILED, console, require};

oceans_rt::manifest!(b"grant out\ngrant devices\ngrant sysinfo\n");
oceans_rt::entry!(main);

const MAX_FUNCTIONS: usize = 128;

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let (bus, sysinfo) = match (
        require(
            &mut out,
            &directory,
            "sysreport",
            "devices",
            "devices",
            "devices",
        ),
        require(
            &mut out,
            &directory,
            "sysreport",
            "sysinfo",
            "sysinfo",
            "sysinfo",
        ),
    ) {
        (Ok(bus), Ok(sysinfo)) => (bus, sysinfo),
        (Err(code), _) | (_, Err(code)) => return code,
    };
    let _ = writeln!(out, "== Oceans system report (ADR-0068) ==");

    let mut record = [0u8; KernelInfo::SIZE];
    if let Some(kernel) = oceans_rt::system_info(sysinfo, sysinfo::KERNEL, &mut record)
        .ok()
        .and_then(|len| KernelInfo::decode(&record[..len]))
    {
        let _ = writeln!(
            out,
            "system: Oceans {} {} (ABI {})",
            sysinfo::text(&kernel.version),
            sysinfo::text(&kernel.arch),
            kernel.abi_version
        );
    }

    let cpu = cpu(&mut out);
    let mut record = [0u8; MemoryInfo::SIZE];
    let memory_ok = match oceans_rt::system_info(sysinfo, sysinfo::MEMORY, &mut record)
        .ok()
        .and_then(|len| MemoryInfo::decode(&record[..len]))
    {
        Some(memory) => {
            let mib = (memory.total_frames * memory.page_size) >> 20;
            let _ = writeln!(out, "memory: {mib} MiB usable");
            mib >= 200
        }
        None => false,
    };

    let Some(unsupported) = pci(&mut out, bus) else {
        let _ = writeln!(out, "sysreport: cannot read the PCI device list");
        return EXIT_FAILED;
    };
    if let Some(usb) = directory.find("use", "usb") {
        usb_devices(&mut out, usb);
    }
    if let Some(net) = directory.find("use", "net") {
        match info(net) {
            Ok(net) if net.configured => {
                let _ = writeln!(
                    out,
                    "network: {}/{} (gateway {})",
                    Dotted(net.address),
                    net.prefix,
                    Dotted(net.gateway)
                );
            }
            Ok(_) => {
                let _ = writeln!(out, "network: not configured (no link, or no DHCP answer)");
            }
            Err(error) => {
                let _ = writeln!(out, "network: {}", error.message());
            }
        }
    }

    let _ = writeln!(out, "== summary ==");
    let _ = writeln!(
        out,
        "tier 1 baseline: {}",
        if cpu.meets_baseline() && memory_ok {
            "met"
        } else {
            "NOT met"
        }
    );
    let _ = writeln!(out, "pci devices without a driver: {unsupported}");
    0
}

/// The CPU, from CPUID, against the Tier 1 baseline.
fn cpu(out: &mut Out) -> Cpuid {
    // CPUID exists on every x86-64 CPU and has no side effects.
    let leaf = |n: u32| -> CpuidResult { __cpuid(n) };
    let tuple = |r: CpuidResult| (r.eax, r.ebx, r.ecx, r.edx);
    let vendor = leaf(0);
    let mut name = [0u8; 12];
    for (i, word) in [vendor.ebx, vendor.edx, vendor.ecx].iter().enumerate() {
        name[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
    }
    let extended = leaf(0x8000_0000).eax;
    let cpuid = Cpuid {
        leaf1: tuple(leaf(1)),
        ext1: if extended >= 0x8000_0001 {
            tuple(leaf(0x8000_0001))
        } else {
            (0, 0, 0, 0)
        },
    };
    let mut brand = [0u8; 48];
    if extended >= 0x8000_0004 {
        for (i, n) in (0x8000_0002u32..=0x8000_0004).enumerate() {
            let r = leaf(n);
            for (j, word) in [r.eax, r.ebx, r.ecx, r.edx].iter().enumerate() {
                let at = 16 * i + 4 * j;
                brand[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
    }
    let (family, model, stepping) = cpuid.signature();
    let _ = writeln!(
        out,
        "cpu: {} ({}, family {family:#x} model {model:#x} stepping {stepping})",
        ascii(&brand),
        ascii(&name)
    );
    let _ = write!(out, "cpu baseline (x86-64-v2, NX, APIC):");
    for (feature, present) in cpuid.baseline() {
        let _ = write!(out, " {feature}{}", if present { "" } else { "(MISSING)" });
    }
    let _ = writeln!(out, "{}", if cpuid.x2apic() { "; x2APIC" } else { "" });
    cpuid
}

/// Every PCI function and what Oceans gives it; returns how many have
/// nothing.
fn pci(out: &mut Out, bus: oceans_rt::Handle) -> Option<usize> {
    let mut buffer = [0u8; MAX_FUNCTIONS * DeviceRecord::SIZE];
    let len = oceans_rt::device_list(bus, &mut buffer).ok()?;
    let mut unsupported = 0;
    let _ = writeln!(out, "pci:");
    for d in buffer[..len]
        .as_chunks::<{ DeviceRecord::SIZE }>()
        .0
        .iter()
        .filter_map(|bytes| DeviceRecord::decode(bytes))
    {
        let _ = write!(
            out,
            "  {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x}.{:02x}  ",
            d.bus, d.slot, d.function, d.vendor, d.device, d.class, d.subclass, d.prog_if
        );
        let row = lookup(d.vendor, d.device, d.class, d.subclass, d.prog_if);
        match row.map(|row| (row.what, row.support)) {
            Some((what, Support::Driver(driver))) => {
                let _ = write!(
                    out,
                    "{what}: driver {driver}{}",
                    if d.open {
                        ", attached"
                    } else {
                        ", not attached"
                    }
                );
            }
            Some((what, Support::Firmware(how))) => {
                let _ = write!(out, "{what}: {how}");
            }
            Some((what, Support::Platform)) => {
                let _ = write!(out, "{what}");
            }
            Some((_, Support::None)) | None => {
                unsupported += 1;
                let _ = write!(out, "NO DRIVER");
            }
        }
        let _ = writeln!(out);
    }
    Some(unsupported)
}

fn usb_devices(out: &mut Out, usb: oceans_rt::Handle) {
    let _ = writeln!(out, "usb:");
    let mut reply = [0u8; service::MAX_RECORDS * service::RECORD_SIZE];
    let Ok(got) = oceans_rt::ipc_call_msg(usb, service::LIST, &[0], &[], &mut reply, &mut [])
    else {
        let _ = writeln!(out, "  the USB service did not answer");
        return;
    };
    let mut any = false;
    for record in reply[..got.data_len]
        .as_chunks::<{ service::RECORD_SIZE }>()
        .0
        .iter()
        .filter_map(|bytes| Record::decode(bytes))
    {
        any = true;
        let _ = writeln!(
            out,
            "  port {}: {:04x}:{:04x} {} {}",
            record.path(),
            record.vendor,
            record.product,
            record.name(),
            record.role()
        );
    }
    if !any {
        let _ = writeln!(out, "  no devices");
    }
}

/// CPUID text: up to the first NUL, trimmed.
fn ascii(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end]).unwrap_or("?").trim()
}
