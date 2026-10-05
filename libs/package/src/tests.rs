extern crate std;

use std::format;
use std::string::String;
use std::vec::Vec;

use super::*;

const SEED: [u8; 32] = [7; 32];
const OTHER_SEED: [u8; 32] = [9; 32];

const MANIFEST_TEXT: &str = "\
# An example app.
id = app.oceans.hello
name = Hello
version = 1.2.3
publisher = Oceans Examples
description = Says hello and counts its runs
architecture = x86_64
api = 1
entry = hello
permission = storage
permission = network: fetch today's greeting
source = https://example.org/hello
";

fn trust(seed: &[u8; 32], publisher: &str) -> String {
    std::format!("# trusted\n{} {publisher}\n", public_key_hex(seed))
}

fn package(manifest: &str, seed: &[u8; 32]) -> Vec<u8> {
    build(
        &[
            ("manifest", manifest.as_bytes()),
            ("hello", b"\x7fELF program"),
        ],
        seed,
    )
    .unwrap()
}

#[test]
fn manifests_parse() {
    let manifest = Manifest::parse(MANIFEST_TEXT).unwrap();
    assert_eq!(manifest.id, "app.oceans.hello");
    assert_eq!(manifest.name, "Hello");
    assert_eq!(
        manifest.version,
        Version {
            major: 1,
            minor: 2,
            patch: 3
        }
    );
    assert_eq!(manifest.publisher, "Oceans Examples");
    assert_eq!(
        (manifest.api, manifest.entry, manifest.channel),
        (1, "hello", "stable")
    );
    assert_eq!(manifest.source, Some("https://example.org/hello"));
    let requests: Vec<_> = manifest.requests().collect();
    assert_eq!(
        requests,
        [
            Request {
                permission: Permission::Storage,
                reason: ""
            },
            Request {
                permission: Permission::Network,
                reason: "fetch today's greeting"
            },
        ]
    );
    assert!(manifest.asks_for(Permission::Network));
    assert!(!manifest.service);
    let text = std::format!("{MANIFEST_TEXT}kind = service\n");
    assert!(Manifest::parse(&text).unwrap().service);
    assert!(Manifest::parse(&std::format!("{MANIFEST_TEXT}kind = daemon\n")).is_err());
    assert!(!manifest.asks_for(Permission::Files));
}

#[test]
fn bad_manifests_are_refused() {
    let replace = |from: &str, to: &str| MANIFEST_TEXT.replace(from, to);
    let cases = [
        (
            replace("id = app.oceans.hello", "id = Hello"),
            ManifestError::BadId,
        ),
        (
            replace("id = app.oceans.hello", "id = app..hello"),
            ManifestError::BadId,
        ),
        (
            replace("id = app.oceans.hello", "id = app.oceans/x"),
            ManifestError::BadId,
        ),
        (
            replace("version = 1.2.3", "version = 1.2"),
            ManifestError::BadVersion,
        ),
        (
            replace("version = 1.2.3", "version = 01.2.3"),
            ManifestError::BadVersion,
        ),
        (
            replace("version = 1.2.3", "version = 1.2.3-beta"),
            ManifestError::BadVersion,
        ),
        (replace("api = 1", "api = one"), ManifestError::BadApi),
        (
            replace("permission = storage", "permission = camera"),
            ManifestError::UnknownPermission(10),
        ),
        (
            replace("permission = storage", "permission = network"),
            ManifestError::DuplicatePermission(11),
        ),
        (
            replace("name = Hello", "nmae = Hello"),
            ManifestError::UnknownKey(3),
        ),
        (
            replace("name = Hello", "name = Hello\nname = Again"),
            ManifestError::DuplicateKey(4),
        ),
        (
            replace("name = Hello", "just words"),
            ManifestError::Syntax(3),
        ),
        (
            replace("entry = hello\n", ""),
            ManifestError::Missing("the manifest has no entry"),
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(Manifest::parse(&text), Err(expected), "{text}");
    }
    for (from, to) in [
        ("entry = hello", "entry = ../hello"),
        ("entry = hello", "entry = signature"),
        ("architecture = x86_64", "architecture = X86 64"),
        ("name = Hello", "name = Hello\u{7}"),
        ("source = https://example.org/hello", "source = not a url"),
    ] {
        assert!(
            matches!(
                Manifest::parse(&replace(from, to)),
                Err(ManifestError::BadText(_))
            ),
            "{to}"
        );
    }
    let many: String = (0..17)
        .map(|i| std::format!("permission = x{i}\n"))
        .collect();
    assert!(Manifest::parse(&many).is_err());
}

#[test]
fn versions_order() {
    let v = |text| Version::parse(text).unwrap();
    assert!(v("1.10.0") > v("1.9.9"));
    assert!(v("2.0.0") > v("1.99.99"));
    assert_eq!(std::format!("{}", v("0.1.0")), "0.1.0");
    assert_eq!(Version::parse("1.2.3.4"), None);
    assert_eq!(Version::parse("-1.2.3"), None);
}

#[test]
fn ids() {
    assert!(valid_id("app.oceans.hello"));
    assert!(valid_id("com.example.my-app2"));
    assert!(!valid_id("hello"));
    assert!(!valid_id("app.Oceans.hello"));
    assert!(!valid_id("app.1oceans"));
    assert!(!valid_id("app.oceans."));
    assert!(!valid_id(&"a.".repeat(40)));
}

#[test]
fn trust_lists_parse() {
    let text = std::format!(
        "{}\nnot-hex Someone\n{} \n",
        trust(&SEED, "Oceans Examples"),
        public_key_hex(&OTHER_SEED)
    );
    let keys: Vec<_> = trusted_keys(&text).collect();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].publisher, "Oceans Examples");
    assert_eq!(trust_errors(&text), 2);
}

#[test]
fn signed_packages_open() {
    let bytes = package(MANIFEST_TEXT, &SEED);
    let trust = trust(&SEED, "Oceans Examples");
    let keys: Vec<_> = trusted_keys(&trust).collect();
    let opened = Package::open(&bytes, &keys).unwrap();
    assert_eq!(opened.manifest.id, "app.oceans.hello");
    assert_eq!(opened.entry(), b"\x7fELF program");
    assert_eq!(std::format!("{}", public_key_hex(&SEED)), {
        let mut hex = String::new();
        for b in opened.key {
            hex.push_str(&std::format!("{b:02x}"));
        }
        hex
    });
}

#[test]
fn tampering_and_trust_are_checked() {
    let trust_text = trust(&SEED, "Oceans Examples");
    let keys: Vec<_> = trusted_keys(&trust_text).collect();
    let good = package(MANIFEST_TEXT, &SEED);

    // Any changed byte of a file breaks the archive checksum or the
    // signature; rebuilding the archive around changed contents with the
    // old signature breaks the signature.
    let archive = oceans_archive::Archive::parse(&good).unwrap();
    let signature = archive.find(SIGNATURE).unwrap().to_vec();
    let forged_files: [(&str, &[u8]); 3] = [
        ("manifest", MANIFEST_TEXT.as_bytes()),
        ("hello", b"\x7fELF evil!!"),
        (SIGNATURE, &signature),
    ];
    let mut forged = std::vec![0u8; oceans_archive::archive_len(&forged_files)];
    oceans_archive::write(&forged_files, &mut forged).unwrap();
    assert_eq!(
        Package::open(&forged, &keys).err(),
        Some(PackageError::BadSignature)
    );

    let mut flipped = good.clone();
    let last = flipped.len() - 20;
    flipped[last] ^= 1;
    assert!(Package::open(&flipped, &keys).is_err());

    // Unsigned, signed by a stranger, or by a key of another publisher.
    let unsigned_files: [(&str, &[u8]); 2] =
        [("manifest", MANIFEST_TEXT.as_bytes()), ("hello", b"x")];
    let mut unsigned = std::vec![0u8; oceans_archive::archive_len(&unsigned_files)];
    oceans_archive::write(&unsigned_files, &mut unsigned).unwrap();
    assert_eq!(
        Package::open(&unsigned, &keys).err(),
        Some(PackageError::Unsigned)
    );
    let stranger = package(MANIFEST_TEXT, &OTHER_SEED);
    assert_eq!(
        Package::open(&stranger, &keys).err(),
        Some(PackageError::UntrustedKey)
    );
    let other_trust = std::format!("{trust_text}{} Someone Else\n", public_key_hex(&OTHER_SEED));
    let both: Vec<_> = trusted_keys(&other_trust).collect();
    assert_eq!(
        Package::open(&stranger, &both).err(),
        Some(PackageError::WrongPublisher)
    );

    // Valid signature, but unusable contents.
    let cases = [
        (
            MANIFEST_TEXT.replace("architecture = x86_64", "architecture = aarch64"),
            PackageError::WrongArchitecture,
        ),
        (
            MANIFEST_TEXT.replace("api = 1", "api = 2"),
            PackageError::ApiTooNew,
        ),
        (
            MANIFEST_TEXT.replace("entry = hello", "entry = missing"),
            PackageError::NoEntry,
        ),
    ];
    for (text, expected) in cases {
        assert_eq!(
            Package::open(&package(&text, &SEED), &keys).err(),
            Some(expected)
        );
    }
}

#[test]
fn the_digest_depends_on_names_sizes_and_contents() {
    let base = digest([("a", &b"xy"[..]), ("b", &b"z"[..])].into_iter());
    assert_ne!(
        base,
        digest([("a", &b"x"[..]), ("b", &b"yz"[..])].into_iter())
    );
    assert_ne!(
        base,
        digest([("b", &b"xy"[..]), ("a", &b"z"[..])].into_iter())
    );
    assert_ne!(
        base,
        digest([("a", &b"xy"[..]), ("b", &b"y"[..])].into_iter())
    );
    assert_eq!(
        base,
        digest([("a", &b"xy"[..]), ("b", &b"z"[..])].into_iter())
    );
}

#[test]
fn permissions_round_trip() {
    for permission in Permission::ALL {
        assert_eq!(Permission::from_name(permission.name()), Some(permission));
        assert!(!permission.description().is_empty());
    }
    assert!(Permission::Storage.automatic());
    assert!(!Permission::Network.automatic());
    assert!(!Permission::Files.automatic());
}

#[test]
fn runtimes_parse() {
    assert_eq!(
        Manifest::parse(MANIFEST_TEXT).unwrap().runtime,
        Runtime::Native
    );
    for (line, runtime) in [
        ("runtime = native\n", Runtime::Native),
        ("runtime = wasm\n", Runtime::Wasm),
    ] {
        let text = std::format!("{MANIFEST_TEXT}{line}");
        assert_eq!(Manifest::parse(&text).unwrap().runtime, runtime);
    }
    for bad in ["runtime = jvm\n", "runtime = Wasm\n", "runtime =\n"] {
        let text = std::format!("{MANIFEST_TEXT}{bad}");
        assert!(
            matches!(Manifest::parse(&text), Err(ManifestError::BadText(_))),
            "{bad}"
        );
    }
    let twice = std::format!("{MANIFEST_TEXT}runtime = wasm\nruntime = wasm\n");
    assert_eq!(
        Manifest::parse(&twice),
        Err(ManifestError::DuplicateKey(14))
    );
    for runtime in [Runtime::Native, Runtime::Wasm] {
        assert_eq!(Runtime::from_name(runtime.name()), Some(runtime));
    }
}

#[test]
fn programs_must_match_their_runtime() {
    const WASM: &[u8] = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0";
    const ELF: &[u8] = b"\x7fELF\x02\x01\x01";
    assert!(Runtime::Wasm.accepts(WASM));
    assert!(!Runtime::Wasm.accepts(b"\0asm\x02\0\0\0"));
    assert!(!Runtime::Wasm.accepts(b"\0asm"));
    assert!(!Runtime::Wasm.accepts(ELF));
    assert!(Runtime::Native.accepts(ELF));
    assert!(!Runtime::Native.accepts(WASM));
    assert!(!Runtime::Native.accepts(b""));

    let trust_text = trust(&SEED, "Oceans Examples");
    let keys: Vec<_> = trusted_keys(&trust_text).collect();
    let wasm_manifest = std::format!("{MANIFEST_TEXT}runtime = wasm\n");
    let with = |manifest: &str, program: &[u8]| {
        build(
            &[("manifest", manifest.as_bytes()), ("hello", program)],
            &SEED,
        )
        .unwrap()
    };
    let bytes = with(&wasm_manifest, WASM);
    let opened = Package::open(&bytes, &keys).unwrap();
    assert_eq!(opened.manifest.runtime, Runtime::Wasm);
    assert_eq!(opened.entry(), WASM);
    assert_eq!(
        Package::open(&with(&wasm_manifest, ELF), &keys).err(),
        Some(PackageError::WrongFormat(Runtime::Wasm))
    );
    assert_eq!(
        Package::open(&with(MANIFEST_TEXT, WASM), &keys).err(),
        Some(PackageError::WrongFormat(Runtime::Native))
    );
    assert_ne!(
        PackageError::WrongFormat(Runtime::Wasm).message(),
        PackageError::WrongFormat(Runtime::Native).message()
    );
}

/// The bridge (ADR-0058) keeps its own copy of the catalog in Go: the same
/// permissions, in the same order, in the same words.
#[test]
fn the_bridge_catalog_matches() {
    let go = include_str!("../../../go/cmd/bridge/system.go");
    let start = go
        .find("var permissions = ")
        .expect("the bridge's permission list");
    let list = &go[start..start + go[start..].find("\n}\n").expect("its end")];
    let entries: Vec<&str> = list
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
        .collect();
    assert_eq!(entries.len(), Permission::ALL.len());
    for (permission, entry) in Permission::ALL.iter().zip(entries) {
        let want = format!(
            "{{\"{}\", \"{}\"}},",
            permission.name(),
            permission.description()
        );
        assert_eq!(entry, want);
    }
}

#[test]
fn web_bundles_round_trip_and_are_checked() {
    let files: [(&str, &[u8]); 3] = [
        ("index.html", b"<!doctype html>"),
        ("_app/immutable/entry/start.abc123.js", b"export {}"),
        ("favicon.svg", b"<svg/>"),
    ];
    let bundle = web::write(&files).unwrap();
    let mut read = Vec::new();
    web::read(&bundle, |path, data| read.push((path, data))).unwrap();
    assert_eq!(read, files);
    assert!(Runtime::Web.accepts(&bundle));
    assert!(!Runtime::Web.accepts(b"OCEANSWB"));

    for bad in [
        "",
        "/etc/passwd",
        "a/../b",
        "a//b",
        ".hidden",
        "a/.git/x",
        "sp ace.js",
        "back\\slash",
    ] {
        assert!(!web::valid_path(bad), "{bad}");
        assert_eq!(
            web::write(&[(bad, b"x")]),
            Err(web::BundleError::BadPath),
            "{bad}"
        );
    }
    assert_eq!(
        web::write(&[("a.js", b"1"), ("a.js", b"2")]),
        Err(web::BundleError::DuplicatePath)
    );
    // Cut short, or with bytes after the last file.
    assert_eq!(
        web::read(&bundle[..bundle.len() - 1], |_, _| {}),
        Err(web::BundleError::Truncated)
    );
    let mut longer = bundle.clone();
    longer.push(0);
    assert_eq!(
        web::read(&longer, |_, _| {}),
        Err(web::BundleError::Trailing)
    );
    // A path that lies about its length.
    let mut lying = bundle.clone();
    lying[16] = 0xff;
    assert!(web::read(&lying, |_, _| {}).is_err());
}

#[test]
fn web_apps_cannot_be_services() {
    let manifest = MANIFEST_TEXT.replace(
        "entry = hello",
        "entry = web.bundle\nruntime = web\nkind = service",
    );
    assert!(Manifest::parse(&manifest).is_err());
    // Storage and the network (ADR-0066), nothing else.
    let manifest = MANIFEST_TEXT.replace("entry = hello", "entry = web.bundle\nruntime = web");
    assert_eq!(Manifest::parse(&manifest).unwrap().runtime, Runtime::Web);
    let files = manifest.replace("permission = storage", "permission = files");
    assert!(Manifest::parse(&files).is_err());
}

#[test]
fn dates_round_trip() {
    assert_eq!(date::parse("1970-01-01"), Some(0));
    assert_eq!(date::parse("2000-03-01"), Some(11_017));
    assert_eq!(
        date::parse("2024-02-29").map(date::civil),
        Some((2024, 2, 29))
    );
    for day in (0..30_000).step_by(37) {
        let (y, m, d) = date::civil(day);
        assert_eq!(
            date::parse(&std::format!("{y:04}-{m:02}-{d:02}")),
            Some(day)
        );
    }
    for bad in [
        "2023-02-29",
        "2026-13-01",
        "2026-00-10",
        "1969-12-31",
        "2026-1-01",
        "2026/01/01",
        "abcd-ef-gh",
    ] {
        assert_eq!(date::parse(bad), None, "{bad}");
    }
    assert_eq!(date::today(86_400_000 * 3 + 5), 3);
}

#[test]
fn dated_trust_lines_expire() {
    let key = public_key_hex(&SEED);
    let text = std::format!("{key} until=2027-01-31 Example Developer\n");
    let entry = trust_entries(&text).next().unwrap();
    assert_eq!(entry.key.publisher, "Example Developer");
    let last = date::parse("2027-01-31").unwrap();
    assert!(entry.valid_on(Some(last)));
    assert!(!entry.valid_on(Some(last + 1)));
    // Without a known time, a limited key is not trusted.
    assert!(!entry.valid_on(None));
    // The boot image's list never trusts a limited key.
    assert_eq!(trusted_keys(&text).count(), 0);
    let bad = std::format!("{key} until=2027-02-30 Example Developer\n");
    assert_eq!(trust_entries(&bad).count(), 0);
    assert_eq!(trust_errors(&bad), 1);
}
