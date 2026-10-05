extern crate std;

use super::*;

/// A modern x86-64 (QEMU `-cpu max`-like) and an old one (no SSE4.2).
fn modern() -> Cpuid {
    Cpuid {
        leaf1: (
            0x000a_0652,
            0,
            (1 << 0) | (1 << 9) | (1 << 13) | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 23),
            1 << 9,
        ),
        ext1: (0, 0, 1, 1 << 20),
    }
}

#[test]
fn the_baseline_is_x86_64_v2_with_nx_and_an_apic() {
    let cpu = modern();
    assert!(cpu.meets_baseline());
    assert!(cpu.x2apic());
    let mut old = cpu;
    old.leaf1.2 &= !(1 << 20);
    assert!(!old.meets_baseline());
    assert_eq!(
        old.baseline()
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(n, _)| *n)
            .next(),
        Some("SSE4.2")
    );
    let mut no_nx = cpu;
    no_nx.ext1.3 = 0;
    assert!(!no_nx.meets_baseline());
}

#[test]
fn signatures_apply_the_extended_fields() {
    // Family 6, extended model 0xa, model 5 → 0xa5 (a Comet Lake part).
    assert_eq!(modern().signature(), (6, 0xa5, 2));
    // AMD family 0x17 (base 0xf + extended 0x8), model 0x71.
    let zen2 = Cpuid {
        leaf1: (0x0087_0f10, 0, 0, 0),
        ..Cpuid::default()
    };
    assert_eq!(zen2.signature(), (0x17, 0x71, 0));
}

#[test]
fn devices_get_the_most_specific_row() {
    assert_eq!(
        support(0x144d, 0xa808, 0x01, 0x08, 0x02),
        Support::Driver("nvme (ADR-0040)")
    );
    assert!(matches!(
        support(0x8086, 0xa36d, 0x0c, 0x03, 0x30),
        Support::Driver(_)
    ));
    // A USB 2 EHCI controller: no driver.
    assert_eq!(support(0x8086, 0x1e2d, 0x0c, 0x03, 0x20), Support::None);
    assert_eq!(
        support(0x8086, 0x10d3, 0x02, 0x00, 0x00),
        Support::Driver("e1000e (ADR-0041)")
    );
    // Another Intel NIC (I219-V): not yet.
    assert_eq!(support(0x8086, 0x15bc, 0x02, 0x00, 0x00), Support::None);
    assert!(matches!(
        support(0x10de, 0x1c82, 0x03, 0x00, 0x00),
        Support::Firmware(_)
    ));
    assert_eq!(support(0x8086, 0x29c0, 0x06, 0x00, 0x00), Support::Platform);
    // A Realtek RTL8168: not yet.
    assert_eq!(support(0x10ec, 0x8168, 0x02, 0x00, 0x00), Support::None);
}

/// The published matrix lists every row with what it gets.
#[test]
fn the_published_matrix_matches() {
    let doc = include_str!("../../../docs/hardware/compatibility.md");
    for row in MATRIX {
        let support = match row.support {
            Support::Driver(driver) => driver,
            Support::Firmware(how) => how,
            Support::Platform => "platform",
            Support::None => "none",
        };
        let line = std::format!("| {} | {} |", row.what, support);
        assert!(
            doc.contains(&line),
            "docs/hardware/compatibility.md lacks: {line}"
        );
    }
}
