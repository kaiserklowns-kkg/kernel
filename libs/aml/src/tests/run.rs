//! Running AML (ADR-0153): methods that compute, an embedded controller
//! and a battery that reads it, and damaged methods.

use super::*;
use crate::battery;
use crate::eval::{Host, HostError, Machine, RunError, Value, space};
use std::collections::BTreeMap as Map;

/// A machine's hardware for the tests: bytes by space and address, and
/// what was notified. Port 0x60 is refused, as the system keeps it.
#[derive(Default)]
struct Fake {
    bytes: Map<(u8, u64), u8>,
    notified: StdVec<(std::string::String, u64)>,
    slept: u64,
}

impl Host for Fake {
    fn read(&mut self, space: u8, address: u64, width: u8) -> Result<u64, HostError> {
        if space == space::SYSTEM_IO && address == 0x60 {
            return Err(HostError::Denied);
        }
        Ok((0..u64::from(width)).fold(0, |v, i| {
            v | u64::from(*self.bytes.get(&(space, address + i)).unwrap_or(&0)) << (8 * i)
        }))
    }

    fn write(&mut self, space: u8, address: u64, width: u8, value: u64) -> Result<(), HostError> {
        for i in 0..u64::from(width) {
            self.bytes
                .insert((space, address + i), (value >> (8 * i)) as u8);
        }
        Ok(())
    }

    fn sleep(&mut self, ms: u64) {
        self.slept += ms;
    }

    fn notify(&mut self, object: &Path, value: u64) {
        self.notified.push((std::format!("{object}"), value));
    }
}

fn byte(v: u8) -> StdVec<u8> {
    vec![0x0a, v]
}

fn word(v: u16) -> StdVec<u8> {
    cat(&[&[0x0b], &v.to_le_bytes()])
}

fn string(text: &str) -> StdVec<u8> {
    cat(&[&[0x0d], text.as_bytes(), &[0]])
}

fn store(source: &[u8], target: &[u8]) -> StdVec<u8> {
    cat(&[&[0x70], source, target])
}

fn index(source: &[u8], i: &[u8]) -> StdVec<u8> {
    cat(&[&[0x88], source, i, &[0x00]])
}

fn ret(value: &[u8]) -> StdVec<u8> {
    cat(&[&[0xa4], value])
}

fn if_(predicate: &[u8], body: &[u8]) -> StdVec<u8> {
    cat(&[&[0xa0], &pkg(&cat(&[predicate, body]))])
}

fn else_(body: &[u8]) -> StdVec<u8> {
    cat(&[&[0xa1], &pkg(body)])
}

fn while_(predicate: &[u8], body: &[u8]) -> StdVec<u8> {
    cat(&[&[0xa2], &pkg(&cat(&[predicate, body]))])
}

fn lequal(a: &[u8], b: &[u8]) -> StdVec<u8> {
    cat(&[&[0x93], a, b])
}

fn package(items: &[&[u8]]) -> StdVec<u8> {
    cat(&[
        &[0x12],
        &pkg(&cat(&[&[items.len() as u8], &items.concat()])),
    ])
}

const LOCAL0: u8 = 0x60;
const ARG0: u8 = 0x68;
const ARG1: u8 = 0x69;

/// An embedded controller and a battery that reads it, as laptops have
/// them; and methods that compute.
fn dsdt() -> StdVec<u8> {
    // OperationRegion (ECOR, EmbeddedControl, Zero, 0xFF)
    let region = cat(&[&[0x5b, 0x80], &name("ECOR"), &[0x03, 0x00], &byte(0xff)]);
    // Field (ECOR, ByteAcc, Lock, Preserve) { Offset (0x10), BRC0, 16,
    // BFC0, 16, BST0, 8, BRT0, 16 }
    let field = cat(&[
        &[0x5b, 0x81],
        &pkg(&cat(&[
            &name("ECOR"),
            &[0x11],
            &[0x00, 0x40, 0x08], // 128 reserved bits: to offset 0x10
            &name("BRC0"),
            &[16],
            &name("BFC0"),
            &[16],
            &name("BST0"),
            &[8],
            &name("BRT0"),
            &[16],
        ])),
    ]);
    // Method (_REG, 2) { If (LEqual (Arg0, 3)) { Store (Arg1, ECOK) } }
    let reg = method(
        "_REG",
        2,
        &if_(&lequal(&[ARG0], &byte(3)), &store(&[ARG1], &name("ECOK"))),
    );
    let ec = device(
        "EC0",
        &cat(&[
            &named("_HID", &eisa("PNP0C09")),
            &named("ECOK", &[0x00]),
            &region,
            &field,
            &reg,
        ]),
    );
    // The battery: its packages filled from the EC once it is ready.
    let bst = method(
        "_BST",
        0,
        &cat(&[
            &if_(
                &name("^^EC0.ECOK"),
                &cat(&[
                    &store(&name("^^EC0.BST0"), &index(&name("PBST"), &[0x00])),
                    &store(&name("^^EC0.BRT0"), &index(&name("PBST"), &[0x01])),
                    &store(&name("^^EC0.BRC0"), &index(&name("PBST"), &byte(2))),
                ]),
            ),
            &ret(&name("PBST")),
        ]),
    );
    let bif = method(
        "_BIF",
        0,
        &cat(&[
            &store(&name("^^EC0.BFC0"), &index(&name("PBIF"), &byte(2))),
            &ret(&name("PBIF")),
        ]),
    );
    let battery = device(
        "BAT0",
        &cat(&[
            &named("_HID", &eisa("PNP0C0A")),
            &method("_STA", 0, &ret(&byte(0x1f))),
            &named("PBST", &package(&[&[0x00], &[0x00], &[0x00], &[0x00]])),
            &named(
                "PBIF",
                &package(&[
                    &[0x00],          // mWh
                    &dword(u32::MAX), // design: unknown
                    &[0x00],          // last full: from the EC
                    &[0x01],
                    &word(11_400),
                ]),
            ),
            &bst,
            &bif,
        ]),
    );
    // Method (CNT, 1) { Store (Zero, Local0) While (LLess (Local0, Arg0))
    // { Increment (Local0) } Return (Multiply (Local0, 2)) }
    let count = method(
        "CNT",
        1,
        &cat(&[
            &store(&[0x00], &[LOCAL0]),
            &while_(&[0x95, LOCAL0, ARG0], &[0x75, LOCAL0]),
            &ret(&cat(&[&[0x77, LOCAL0], &byte(2), &[0x00]])),
        ]),
    );
    let osi = method(
        "OSIT",
        0,
        &cat(&[
            &if_(
                &cat(&[&name("\\_OSI"), &string("Windows 2015")]),
                &ret(&byte(1)),
            ),
            &else_(&ret(&byte(2))),
        ]),
    );
    let linux = method("LNXT", 0, &ret(&cat(&[&name("\\_OSI"), &string("Linux")])));
    // Name (BUF0, Buffer (4) {}) and a method that makes a word field of it.
    let buffer = named("BUF0", &cat(&[&[0x11], &pkg(&byte(4))]));
    let field_of_buffer = method(
        "BFT",
        0,
        &cat(&[
            &cat(&[&[0x8b], &name("BUF0"), &[0x01], &name("WRD0")]),
            &store(&word(0xbeef), &name("WRD0")),
            &ret(&name("BUF0")),
        ]),
    );
    let notify = method("NTF", 0, &cat(&[&[0x86], &name("\\_SB.BAT0"), &byte(0x80)]));
    let forever = method("LOOP", 0, &while_(&[0x01], &[0xa3]));
    // A port the system keeps: the host refuses it.
    let denied = cat(&[&[0x5b, 0x80], &name("KBD"), &[0x01], &byte(0x60), &[0x01]]);
    let kbd_field = cat(&[
        &[0x5b, 0x81],
        &pkg(&cat(&[&name("KBD"), &[0x01], &name("KDAT"), &[8]])),
    ]);
    let read_kbd = method("RKB", 0, &ret(&name("KDAT")));
    let sleeper = method("SLP", 0, &cat(&[&[0x5b, 0x22], &byte(10)]));
    scope(
        "\\_SB",
        &cat(&[
            &ec,
            &battery,
            &count,
            &osi,
            &linux,
            &buffer,
            &field_of_buffer,
            &notify,
            &forever,
            &denied,
            &kbd_field,
            &read_kbd,
            &sleeper,
        ]),
    )
}

fn machine() -> Machine<Fake> {
    let mut namespace = Namespace::new();
    namespace.load(&dsdt()).unwrap();
    Machine::new(namespace, Fake::default())
}

fn path(text: &str) -> Path {
    Path::parse(text).unwrap()
}

#[test]
fn methods_compute() {
    let mut m = machine();
    assert_eq!(
        m.evaluate(&path("\\_SB.CNT"), vec![Value::Integer(5)]),
        Ok(Value::Integer(10))
    );
    assert_eq!(
        m.evaluate_int(&path("\\_SB.OSIT"), vec![]),
        Ok(1),
        "a Windows of today"
    );
    assert_eq!(m.evaluate_int(&path("\\_SB.LNXT"), vec![]), Ok(0));
    assert_eq!(
        m.evaluate(&path("\\_SB.BFT"), vec![]),
        Ok(Value::Buffer(vec![0, 0xef, 0xbe, 0])),
        "a word field of a buffer"
    );
    assert!(
        !m.exists(&path("\\_SB.WRD0")),
        "the method's own field went with it"
    );
    m.evaluate(&path("\\_SB.NTF"), vec![]).unwrap();
    assert_eq!(m.host().notified, [("\\_SB_.BAT0".into(), 0x80)]);
    m.evaluate(&path("\\_SB.SLP"), vec![]).unwrap();
    assert_eq!(m.host().slept, 10);
    assert_eq!(
        m.evaluate(&path("\\_SB.LOOP"), vec![]),
        Err(RunError::Limit("ran too long"))
    );
    assert!(matches!(
        m.evaluate(&path("\\_SB.RKB"), vec![]),
        Err(RunError::Host(HostError::Denied, 1, 0x60))
    ));
    assert_eq!(
        m.hardware_id(&path("\\_SB.BAT0")).as_deref(),
        Some("PNP0C0A")
    );
}

#[test]
fn a_battery_through_its_embedded_controller() {
    let mut m = machine();
    let bat = path("\\_SB.BAT0");
    // The EC's bytes: 3000 left of 4000, discharging at 1500 mW.
    for (at, v) in [(0x10, 3000u16), (0x12, 4000), (0x15, 1500)] {
        m.host()
            .write(space::EMBEDDED_CONTROL, at, 2, u64::from(v))
            .unwrap();
    }
    m.host().write(space::EMBEDDED_CONTROL, 0x14, 1, 1).unwrap();
    // Until _REG, the firmware does not read its EC.
    assert_eq!(battery::status(&mut m, &bat).unwrap().remaining, Some(0));
    assert_eq!(m.connect(space::EMBEDDED_CONTROL), (1, None));
    assert!(battery::present(&mut m, &bat).unwrap());
    let info = battery::info(&mut m, &bat).unwrap();
    assert_eq!(
        info,
        battery::Info {
            milliwatts: true,
            design: None,
            last_full: Some(4000)
        }
    );
    let status = battery::status(&mut m, &bat).unwrap();
    assert!(status.discharging && !status.charging);
    assert_eq!((status.rate, status.remaining), (Some(1500), Some(3000)));
    assert_eq!(
        battery::reading(&info, &status),
        Some(battery::Reading {
            percent: 75,
            charging: false,
            discharging: true,
            minutes: Some(120),
        })
    );
    // Charging: the time to full.
    let charging = battery::Status {
        discharging: false,
        charging: true,
        ..status
    };
    assert_eq!(
        battery::reading(&info, &charging).and_then(|r| r.minutes),
        Some(40)
    );
}

#[test]
fn damaged_methods_are_errors_never_panics() {
    // Bytes changed anywhere: loading or running is an error or a value,
    // never a panic or a hang.
    let good = dsdt();
    for i in (0..good.len()).step_by(3) {
        for value in [0x00u8, 0x5b, 0x70, 0x88, 0xa2, 0xff] {
            let mut bad = good.clone();
            bad[i] = value;
            let mut namespace = Namespace::new();
            if namespace.load(&bad).is_err() {
                continue;
            }
            let mut m = Machine::new(namespace, Fake::default());
            for method in [
                "\\_SB.CNT",
                "\\_SB.BAT0._BST",
                "\\_SB.BAT0._BIF",
                "\\_SB.BFT",
            ] {
                let _ = m.evaluate(&path(method), vec![Value::Integer(3)]);
            }
        }
    }
}
