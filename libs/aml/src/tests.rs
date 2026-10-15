extern crate std;

use super::*;
use std::vec;
use std::vec::Vec as StdVec;

/// AML for the tests, put together as an ASL compiler would.
mod asl {
    use super::StdVec;

    /// A package: its length (which counts its own bytes), then `body`.
    pub fn pkg(body: &[u8]) -> StdVec<u8> {
        for extra in 0..4usize {
            let total = body.len() + 1 + extra;
            let fits = match extra {
                0 => total < 0x40,
                1 => total < 0x1000,
                2 => total < 0x10_0000,
                _ => true,
            };
            if fits {
                let mut out = StdVec::new();
                if extra == 0 {
                    out.push(total as u8);
                } else {
                    out.push(((extra as u8) << 6) | (total & 0x0f) as u8);
                    for i in 0..extra {
                        out.push((total >> (4 + 8 * i)) as u8);
                    }
                }
                out.extend_from_slice(body);
                return out;
            }
        }
        unreachable!()
    }

    /// A name: `\` or `^` prefixes, then dotted segments padded with `_`.
    pub fn name(text: &str) -> StdVec<u8> {
        let mut out = StdVec::new();
        let mut rest = text;
        while let Some(r) = rest.strip_prefix('\\').or_else(|| rest.strip_prefix('^')) {
            out.push(rest.as_bytes()[0]);
            rest = r;
        }
        let segs: StdVec<[u8; 4]> = rest
            .split('.')
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut seg = *b"____";
                seg[..s.len()].copy_from_slice(s.as_bytes());
                seg
            })
            .collect();
        match segs.len() {
            0 => out.push(0),
            1 => {}
            2 => out.push(0x2e),
            n => {
                out.push(0x2f);
                out.push(n as u8);
            }
        }
        for seg in segs {
            out.extend_from_slice(&seg);
        }
        out
    }

    pub fn cat(parts: &[&[u8]]) -> StdVec<u8> {
        parts.concat()
    }

    pub fn scope(path: &str, body: &[u8]) -> StdVec<u8> {
        cat(&[&[0x10], &pkg(&cat(&[&name(path), body]))])
    }

    pub fn device(path: &str, body: &[u8]) -> StdVec<u8> {
        cat(&[&[0x5b, 0x82], &pkg(&cat(&[&name(path), body]))])
    }

    pub fn method(path: &str, args: u8, body: &[u8]) -> StdVec<u8> {
        cat(&[&[0x14], &pkg(&cat(&[&name(path), &[args], body]))])
    }

    pub fn named(path: &str, value: &[u8]) -> StdVec<u8> {
        cat(&[&[0x08], &name(path), value])
    }

    pub fn dword(value: u32) -> StdVec<u8> {
        cat(&[&[0x0c], &value.to_le_bytes()])
    }

    /// `EisaId ("PNP0C0A")` and the like.
    pub fn eisa(id: &str) -> StdVec<u8> {
        let b = id.as_bytes();
        let letter = |c: u8| u32::from(c - b'@');
        let hex = |c: u8| {
            u32::from(if c.is_ascii_digit() {
                c - b'0'
            } else {
                c - b'A' + 10
            })
        };
        let value = (letter(b[0]) << 26)
            | (letter(b[1]) << 21)
            | (letter(b[2]) << 16)
            | (hex(b[3]) << 12)
            | (hex(b[4]) << 8)
            | (hex(b[5]) << 4)
            | hex(b[6]);
        dword(value.swap_bytes())
    }
}

use asl::*;

/// A small DSDT: a PCI root with a region and fields, a battery with a
/// method, and objects only some operating systems get.
fn dsdt() -> StdVec<u8> {
    let field = cat(&[
        &[0x5b, 0x81],
        &pkg(&cat(&[
            &name("GNVS"),
            &[0x01], // DWordAcc, NoLock, Preserve
            &name("OSYS"),
            &[16],
            &[0x00, 8], // 8 reserved bits
            &name("BRTL"),
            &[8],
            &[0x01, 0x03, 0x00], // AccessAs (DWordAcc)
            &name("BIG_"),
            &[0x40, 0x04], // 64 bits: two-byte length
        ])),
    ]);
    let region = cat(&[
        &[0x5b, 0x80],
        &name("GNVS"),
        &[0x00],
        &dword(0x1000),
        &[0x0a, 0x40],
    ]);
    let battery = device(
        "BAT0",
        &cat(&[
            &named("_HID", &eisa("PNP0C0A")),
            &named("_UID", &[0x01]),
            &method(
                "_BST",
                0,
                &[0xa4, 0x12, 0x06, 0x04, 0x00, 0x01, 0x0a, 0x20, 0x0b],
            ),
        ]),
    );
    let pci = device(
        "PCI0",
        &cat(&[
            &named("_HID", &eisa("PNP0A08")),
            &method("_STA", 0, &[0xa4, 0x0a, 0x0f]),
            &region,
            &field,
            &battery,
            &method("GETV", 2, &[0xa4, 0x68]),
        ]),
    );
    // If (LEqual (\_SB.PCI0.OSYS, 0x07DF)) { Name (\WIN8, One) } Else { Name (\WIN8, Zero) }
    let condition = cat(&[&[0x93], &name("\\_SB.PCI0.OSYS"), &[0x0b, 0xdf, 0x07]]);
    let if_else = cat(&[
        &[0xa0],
        &pkg(&cat(&[&condition, &named("\\WIN8", &[0x01])])),
        &[0xa1],
        &pkg(&named("\\WIN8", &[0x00])),
    ]);
    // A load-time call of a method with two arguments: its arguments must
    // be read for the next object to be found.
    let call = cat(&[&name("\\_SB.PCI0.GETV"), &[0x0a, 0x01, 0x0a, 0x02]]);
    let external = cat(&[&[0x15], &name("\\_SB.EXT0"), &[0x08, 0x02]]);
    let lid = device("\\_SB.LID0", &named("_HID", &eisa("PNP0C0D")));
    cat(&[
        &external,
        // As QEMU's DSDT opens: `Scope (\) { Name (DBGV, 0x07) }`.
        &scope("\\", &named("DBGV", &[0x0a, 0x07])),
        &scope("\\_SB", &pci),
        &if_else,
        &call,
        &lid,
        &[0x06], // Alias (\_SB.LID0, \LIDA)
        &name("\\_SB.LID0"),
        &name("\\LIDA"),
    ])
}

#[test]
fn a_dsdt_becomes_a_namespace() {
    let mut namespace = Namespace::new();
    let loaded = namespace.load(&dsdt()).unwrap();
    assert_eq!(loaded.devices, 3);
    assert_eq!(
        namespace.get(&Path::parse(r"\DBGV").unwrap()),
        Some(&Object::Data(Expr::Int(7)))
    );
    assert_eq!(loaded.methods, 3);
    assert_eq!(loaded.duplicates, 1, "WIN8 in both arms: the first kept");

    let ids: StdVec<(std::string::String, Option<std::string::String>)> = namespace
        .devices()
        .map(|(path, id)| (std::format!("{path}"), id))
        .collect();
    assert_eq!(
        ids,
        [
            ("\\_SB_.LID0".into(), Some("PNP0C0D".into())),
            ("\\_SB_.PCI0".into(), Some("PNP0A08".into())),
            ("\\_SB_.PCI0.BAT0".into(), Some("PNP0C0A".into())),
        ]
    );

    let path = |text| Path::parse(text).unwrap();
    assert_eq!(
        namespace.get(&path("\\_SB.PCI0.BRTL")),
        Some(&Object::Field {
            region: path("\\_SB.PCI0.GNVS"),
            bit_offset: 24,
            bits: 8,
            flags: 0x01,
        })
    );
    assert!(matches!(
        namespace.get(&path("\\_SB.PCI0.BIG")),
        Some(Object::Field {
            bit_offset: 32,
            bits: 64,
            flags: 0x03,
            ..
        })
    ));
    assert!(matches!(
        namespace.get(&path("\\_SB.PCI0.GNVS")),
        Some(Object::Region {
            space: 0,
            offset: Expr::Int(0x1000),
            length: Expr::Int(0x40)
        })
    ));
    assert_eq!(
        namespace.get(&path("\\WIN8")),
        Some(&Object::Data(Expr::Int(1)))
    );
    assert!(matches!(
        namespace.get(&path("\\_SB.EXT0")),
        Some(Object::External { kind: 8, args: 2 })
    ));
    // The search rules and aliases.
    let lid = namespace.lookup(
        &NameString {
            root: true,
            up: 0,
            segs: vec![*b"LIDA"],
        },
        &Path::root(),
    );
    assert_eq!(lid, Some(path("\\_SB.LID0")));
    let sta = NameString {
        root: false,
        up: 0,
        segs: vec![*b"_STA"],
    };
    assert_eq!(
        namespace.lookup(&sta, &path("\\_SB.PCI0.BAT0")),
        Some(path("\\_SB.PCI0._STA")),
        "found one level up"
    );
}

#[test]
fn package_lengths_and_names() {
    let long = vec![0xa3u8; 5000]; // Noops
    let encoded = pkg(&long);
    let mut r = parse::Reader::new(&encoded);
    assert_eq!(r.pkg_length(encoded.len()), Ok(encoded.len()));
    assert_eq!(encoded[0] >> 6, 2, "a three-byte length");
    let mut r = parse::Reader::new(&[0x3f]);
    assert_eq!(
        r.pkg_length(10),
        Err(Error::Malformed(0)),
        "past its container"
    );

    let bytes = name("^^AB.CD.EF");
    let parsed = parse::Reader::new(&bytes).name().unwrap();
    assert_eq!((parsed.up, parsed.segs.len()), (2, 3));
    assert_eq!(std::format!("{parsed}"), "^^AB__.CD__.EF__");
    let scope = Path::parse("\\A.B.C").unwrap();
    assert_eq!(parsed.resolve(&scope), Path::parse("\\A.AB.CD.EF"));
    assert_eq!(
        parsed.resolve(&Path::parse("\\A").unwrap()),
        None,
        "above the root"
    );
    assert_eq!(&name::eisa_id(0x0a0c_d041), b"PNP0C0A");
}

#[test]
fn damaged_aml_is_an_error_never_a_panic() {
    let good = dsdt();
    // Cut anywhere: an error (or a smaller namespace), never a panic.
    for len in 0..good.len() {
        let _ = Namespace::new().load(&good[..len]);
    }
    // Every byte changed to a few values.
    for i in 0..good.len() {
        for value in [0x00u8, 0x5b, 0x10, 0xff, 0x40, 0x14] {
            let mut bad = good.clone();
            bad[i] = value;
            let _ = Namespace::new().load(&bad);
        }
    }
    // Deep nesting stops.
    let mut deep = name("X");
    for _ in 0..200 {
        deep = cat(&[&[0x10], &pkg(&cat(&[&name("A"), &deep]))]);
    }
    assert!(matches!(
        Namespace::new().load(&deep),
        Err(Error::TooDeep(_))
    ));
    assert!(matches!(
        Namespace::new().load(&[0x5b, 0x77]),
        Err(Error::UnknownOpcode(0, 0x5b77))
    ));
}
