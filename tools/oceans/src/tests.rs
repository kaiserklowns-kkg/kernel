use super::*;

fn project() -> Project {
    Project {
        id: "app.example.hello".into(),
        name: "Hello Example".into(),
        publisher: "Example Developer".into(),
        sdk: "/sdk".into(),
    }
}

#[test]
fn templates_are_filled_completely() {
    let p = project();
    for template in [Template::Rust, Template::Go, Template::SvelteKit] {
        for (path, text) in template.files() {
            let filled = p.fill(text);
            assert!(
                !filled.contains("{{ID}}") && !filled.contains("{{NAME}}"),
                "{path}"
            );
            assert!(
                !filled.contains("{{CRATE}}") && !filled.contains("{{SDK}}"),
                "{path}"
            );
            assert!(!filled.contains("{{PUBLISHER}}"), "{path}");
        }
        let manifest = template
            .files()
            .iter()
            .find(|(path, _)| *path == "manifest")
            .map(|(_, text)| p.fill(text))
            .unwrap();
        let parsed = Manifest::parse(&manifest).unwrap();
        assert_eq!(parsed.id, "app.example.hello");
        assert_eq!(parsed.publisher, "Example Developer");
        assert_eq!(
            parsed.entry,
            match template {
                Template::Rust => "hello",
                Template::Go => "hello.wasm",
                Template::SvelteKit => "web.bundle",
            }
        );
    }
    assert!(p.fill("{{SDK}}/user/sdk").starts_with("/sdk/"));
}

#[test]
fn names_are_checked_before_they_reach_files() {
    assert!(project().check().is_ok());
    for (id, name, publisher) in [
        ("Hello", "Hello", "Dev"),
        ("app.example.hello", "Hello\"; evil", "Dev"),
        ("app.example.hello", "Hello", "Dev\nid = app.other"),
        ("app.example.hello", "{oops}", "Dev"),
        ("app.example.hello", "", "Dev"),
    ] {
        let p = Project {
            id: id.into(),
            name: name.into(),
            publisher: publisher.into(),
            sdk: "/sdk".into(),
        };
        assert!(p.check().is_err(), "{id} {name} {publisher}");
    }
}

#[test]
fn keys_round_trip_and_never_print_their_seed() {
    let key = DeveloperKey::generate("Example Developer").unwrap();
    let again = DeveloperKey::parse(&key.to_file()).unwrap();
    assert_eq!(key, again);
    let seed_hex: String = key.seed.iter().map(|b| format!("{b:02x}")).collect();
    assert!(!format!("{key:?}").contains(&seed_hex));
    assert_eq!(
        key.trust_command(),
        format!("app trust add {} Example Developer", key.public_hex())
    );
    assert_ne!(
        DeveloperKey::generate("x").unwrap().seed,
        DeveloperKey::generate("x").unwrap().seed
    );
    assert!(DeveloperKey::parse("publisher = A\nseed = 12").is_err());
    assert!(DeveloperKey::parse("seed = ".to_string().as_str()).is_err());
}

const MANIFEST: &str = "id = app.example.hello\nname = Hello\nversion = 1.2.0\n\
    publisher = Example Developer\ndescription = Says \"hi\"\narchitecture = x86_64\napi = 1\n\
    entry = hello\npermission = console\npermission = storage\n";

#[test]
fn packages_are_signed_for_the_manifests_publisher_only() {
    let key = DeveloperKey::generate("Example Developer").unwrap();
    let pkg = package(MANIFEST, b"\x7fELF program", &key).unwrap();
    let trusted = [oceans_package::TrustedKey {
        publisher: "Example Developer",
        key: parse_seed(&key.public_hex()).unwrap(),
    }];
    let opened = oceans_package::Package::open(&pkg, &trusted).unwrap();
    assert_eq!(opened.manifest.id, "app.example.hello");

    let other = DeveloperKey::generate("Someone Else").unwrap();
    assert!(
        package(MANIFEST, b"\x7fELF program", &other)
            .unwrap_err()
            .contains("publisher")
    );
    assert!(
        package(MANIFEST, b"not an ELF", &key)
            .unwrap_err()
            .contains("native")
    );
    assert_eq!(
        package_name(MANIFEST).unwrap(),
        "app.example.hello-1.2.0.opk"
    );
}

#[test]
fn the_store_index_lists_packages_with_their_hashes() {
    let key = DeveloperKey::generate("Example Developer").unwrap();
    let pkg = package(MANIFEST, b"\x7fELF program", &key).unwrap();
    let index = store_index(&[
        ("app.example.hello-1.2.0.opk".into(), pkg.clone()),
        ("broken.opk".into(), b"not a package".to_vec()),
    ]);
    assert!(index.starts_with("{\"apps\":[{\"id\":\"app.example.hello\""));
    assert!(index.contains("\"description\":\"Says \\\"hi\\\"\""));
    assert!(index.contains("\"permissions\":[\"console\",\"storage\"]"));
    assert!(index.contains(&format!("\"size\":{}", pkg.len())));
    assert!(!index.contains("broken.opk"));
}

#[test]
fn sveltekit_builds_become_web_bundles() {
    let dir = std::env::temp_dir().join(format!("oceans-dev-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("_app/immutable")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(dir.join("index.html.br"), "brotli").unwrap();
    std::fs::write(dir.join("_app/immutable/start.js"), "export {}").unwrap();
    let bundle = web_bundle(&dir).unwrap();
    let mut paths = Vec::new();
    oceans_package::web::read(&bundle, |path, _| paths.push(path.to_string())).unwrap();
    assert_eq!(paths, ["_app/immutable/start.js", "index.html"]);
    assert!(oceans_package::Runtime::Web.accepts(&bundle));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The SDK may live under a path with spaces (`E:\Model Business\os`):
/// the Go template quotes it, since go.mod splits words at spaces.
#[test]
fn sdk_paths_with_spaces_stay_one_word() {
    let p = Project {
        sdk: "E:/Model Business/os".into(),
        ..project()
    };
    let go_mod = Template::Go
        .files()
        .iter()
        .find(|(path, _)| *path == "go.mod")
        .map(|(_, text)| p.fill(text))
        .unwrap();
    assert!(
        go_mod.contains("=> \"E:/Model Business/os/go\""),
        "{go_mod}"
    );
}
