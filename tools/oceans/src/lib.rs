//! The Oceans developer tool's logic (ADR-0062), apart from the command
//! line: projects from templates, developer keys, packages, and the store
//! index `oceans serve` publishes. Host-tested.

use std::fmt::Write as _;
use std::path::Path;

use oceans_package::{Manifest, Runtime};

pub mod release;
pub mod secure_boot;

/// A project template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// A native Rust program on the `oceans-sdk` crate.
    Rust,
    /// A Go program (`runtime = wasm`) on the SDK's Go packages.
    Go,
    /// A SvelteKit web app (`runtime = web`, ADR-0064), built with Bun.
    SvelteKit,
}

impl Template {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "rust" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "sveltekit" => Some(Self::SvelteKit),
            _ => None,
        }
    }

    /// `(path in the project, template text)`.
    pub fn files(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Rust => &[
                (
                    "Cargo.toml",
                    include_str!("../../../sdk/templates/rust/Cargo.toml.tmpl"),
                ),
                (
                    ".cargo/config.toml",
                    include_str!("../../../sdk/templates/rust/cargo-config.toml.tmpl"),
                ),
                (
                    "rust-toolchain.toml",
                    include_str!("../../../rust-toolchain.toml"),
                ),
                (
                    "manifest",
                    include_str!("../../../sdk/templates/rust/manifest.tmpl"),
                ),
                (
                    "src/main.rs",
                    include_str!("../../../sdk/templates/rust/main.rs.tmpl"),
                ),
            ],
            Self::Go => &[
                (
                    "go.mod",
                    include_str!("../../../sdk/templates/go/go.mod.tmpl"),
                ),
                (
                    "manifest",
                    include_str!("../../../sdk/templates/go/manifest.tmpl"),
                ),
                (
                    "main.go",
                    include_str!("../../../sdk/templates/go/main.go.tmpl"),
                ),
            ],
            Self::SvelteKit => &[
                (
                    "package.json",
                    include_str!("../../../sdk/templates/sveltekit/package.json"),
                ),
                // Pinned: the same versions every time, never resolved anew.
                (
                    "bun.lock",
                    include_str!("../../../sdk/templates/sveltekit/bun.lock"),
                ),
                (
                    "svelte.config.js",
                    include_str!("../../../sdk/templates/sveltekit/svelte.config.js.tmpl"),
                ),
                (
                    "vite.config.js",
                    include_str!("../../../sdk/templates/sveltekit/vite.config.js"),
                ),
                (
                    "manifest",
                    include_str!("../../../sdk/templates/sveltekit/manifest.tmpl"),
                ),
                (
                    "src/app.html",
                    include_str!("../../../sdk/templates/sveltekit/src/app.html.tmpl"),
                ),
                (
                    "src/routes/+layout.js",
                    include_str!("../../../sdk/templates/sveltekit/src/routes/+layout.js"),
                ),
                (
                    "src/routes/+page.svelte",
                    include_str!("../../../sdk/templates/sveltekit/src/routes/+page.svelte.tmpl"),
                ),
                (
                    "src/lib/oceans.js",
                    include_str!("../../../sdk/templates/sveltekit/src/lib/oceans.js"),
                ),
            ],
        }
    }
}

/// What a new project is called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// `app.example.hello`
    pub id: String,
    /// `Hello`
    pub name: String,
    pub publisher: String,
    /// The SDK's root (this repository), with forward slashes.
    pub sdk: String,
}

impl Project {
    /// The program's name: the id's last label.
    pub fn program(&self) -> &str {
        self.id.rsplit('.').next().unwrap_or(&self.id)
    }

    /// Checks names before they go into files (manifests, Rust strings).
    pub fn check(&self) -> Result<(), String> {
        if !oceans_package::valid_id(&self.id) {
            return Err(format!(
                "{}: not an app id (reverse-DNS, lowercase: app.example.hello)",
                self.id
            ));
        }
        if oceans_package::system_id(&self.id) {
            return Err(format!(
                "{}: ids starting with `system.` are system updates, not apps",
                self.id
            ));
        }
        for (what, text) in [("name", &self.name), ("publisher", &self.publisher)] {
            // Letters and digits of any script (Thai with its vowel and tone
            // marks: ADR-0077), and nothing that could end a string in a
            // template.
            let plain = text.chars().all(|c| {
                c.is_alphanumeric()
                    || " -_.'".contains(c)
                    || ('\u{0e00}'..='\u{0e7f}').contains(&c)
                    || ('\u{0300}'..='\u{036f}').contains(&c)
            });
            if text.trim().is_empty() || text.len() > 64 || !plain {
                return Err(format!(
                    "the {what} must be 1 to 64 bytes of letters, digits, spaces, - _ . or '"
                ));
            }
        }
        Ok(())
    }

    /// A template's text with this project's names.
    pub fn fill(&self, text: &str) -> String {
        text.replace("{{ID}}", &self.id)
            .replace("{{NAME}}", &self.name)
            .replace("{{PUBLISHER}}", &self.publisher)
            .replace("{{CRATE}}", self.program())
            .replace("{{SDK}}", &self.sdk)
    }
}

/// A developer's signing key: a publisher's name and an Ed25519 seed.
#[derive(Clone, PartialEq, Eq)]
pub struct DeveloperKey {
    pub publisher: String,
    pub seed: [u8; 32],
}

impl std::fmt::Debug for DeveloperKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the seed.
        f.debug_struct("DeveloperKey")
            .field("publisher", &self.publisher)
            .finish_non_exhaustive()
    }
}

impl DeveloperKey {
    /// A new key from the operating system's random generator.
    pub fn generate(publisher: &str) -> Result<Self, String> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|e| format!("no randomness: {e}"))?;
        Ok(Self {
            publisher: publisher.to_string(),
            seed,
        })
    }

    pub fn public_hex(&self) -> String {
        oceans_package::public_key_hex(&self.seed)
    }

    /// The key file's text.
    pub fn to_file(&self) -> String {
        let mut seed = String::new();
        for byte in self.seed {
            let _ = write!(seed, "{byte:02x}");
        }
        format!(
            "# An Oceans developer key (ADR-0063). Keep it secret: it signs\n\
             # packages as \"{}\". Its public key: {}\n\
             publisher = {}\nseed = {seed}\n",
            self.publisher,
            self.public_hex(),
            self.publisher
        )
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut publisher = None;
        let mut seed = None;
        for line in text.lines().map(str::trim) {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
                Some(("publisher", value)) => publisher = Some(value.to_string()),
                Some(("seed", value)) => seed = Some(parse_seed(value)?),
                _ => return Err(format!("not a key file line: {line}")),
            }
        }
        Ok(Self {
            publisher: publisher.ok_or("the key file names no publisher")?,
            seed: seed.ok_or("the key file has no seed")?,
        })
    }

    /// The shell command that makes Oceans trust this key.
    pub fn trust_command(&self) -> String {
        format!("app trust add {} {}", self.public_hex(), self.publisher)
    }
}

fn parse_seed(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err("the seed is not 64 hex digits".into());
    }
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| "the seed is not hex".to_string())?;
    }
    Ok(seed)
}

/// Signs a package of `manifest_text` and its program.
pub fn package(manifest_text: &str, program: &[u8], key: &DeveloperKey) -> Result<Vec<u8>, String> {
    let manifest =
        Manifest::parse(manifest_text).map_err(|e| format!("manifest: {}", e.message()))?;
    if manifest.publisher != key.publisher {
        return Err(format!(
            "the manifest's publisher is \"{}\" but the key signs for \"{}\"",
            manifest.publisher, key.publisher
        ));
    }
    if !manifest.runtime.accepts(program) {
        return Err(format!(
            "the program is not a {} program",
            match manifest.runtime {
                Runtime::Native => "native (ELF)",
                Runtime::Wasm => "WebAssembly",
                Runtime::Web => "web bundle",
            }
        ));
    }
    oceans_package::build(
        &[
            ("manifest", manifest_text.as_bytes()),
            (manifest.entry, program),
        ],
        &key.seed,
    )
    .map_err(|e| format!("cannot build the package: {e:?}"))
}

/// A web bundle (ADR-0064) of a SvelteKit build: every file under `dir`
/// (`/`-separated paths), but Brotli copies (the bridge serves gzip).
pub fn web_bundle(dir: &Path) -> Result<Vec<u8>, String> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let entries = std::fs::read_dir(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "br") {
                continue;
            }
            let relative = path
                .strip_prefix(dir)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            files.push((relative, data));
        }
    }
    files.sort();
    let named: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .collect();
    oceans_package::web::write(&named).map_err(|e| format!("the web bundle: {e:?}"))
}

/// The package's file name in `dist/`: `ID-VERSION.opk`.
pub fn package_name(manifest_text: &str) -> Result<String, String> {
    let manifest =
        Manifest::parse(manifest_text).map_err(|e| format!("manifest: {}", e.message()))?;
    Ok(format!("{}-{}.opk", manifest.id, manifest.version))
}

/// A store's `index.json` (ADR-0061) for packages `(file name, bytes)`.
/// Packages whose manifest does not read are skipped.
pub fn store_index(packages: &[(String, Vec<u8>)]) -> String {
    use sha2::Digest;
    let mut apps = Vec::new();
    for (file, bytes) in packages {
        let Some(manifest) = oceans_archive::Archive::parse(bytes)
            .ok()
            .and_then(|archive| archive.find("manifest"))
            .and_then(|text| std::str::from_utf8(text).ok())
            .and_then(|text| Manifest::parse(text).ok())
        else {
            continue;
        };
        let digest = sha2::Sha256::digest(bytes);
        let mut hex = String::new();
        for byte in digest {
            let _ = write!(hex, "{byte:02x}");
        }
        let permissions: Vec<String> = manifest
            .requests()
            .map(|r| format!("\"{}\"", r.permission.name()))
            .collect();
        apps.push(format!(
            "{{\"id\":{},\"name\":{},\"version\":\"{}\",\"publisher\":{},\"description\":{},\
             \"permissions\":[{}],\"package\":{},\"size\":{},\"sha256\":\"{hex}\"}}",
            json(manifest.id),
            json(manifest.name),
            manifest.version,
            json(manifest.publisher),
            json(manifest.description),
            permissions.join(","),
            json(file),
            bytes.len()
        ));
    }
    format!("{{\"apps\":[{}]}}\n", apps.join(","))
}

/// A JSON string.
fn json(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The SDK's root: `OCEANS_SDK`, else this repository (where the tool
/// was built), with forward slashes for templates.
pub fn sdk_root() -> String {
    let root = std::env::var("OCEANS_SDK")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../..").to_string());
    let path = Path::new(&root);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy().replace('\\', "/");
    text.strip_prefix("//?/").unwrap_or(&text).to_string()
}

#[cfg(test)]
mod tests;

/// `~/…` (or `~\…`) under `home`: shells such as PowerShell pass a leading
/// `~` through to programs unexpanded.
pub fn expand_home(path: &str, home: Option<&std::path::Path>) -> std::path::PathBuf {
    match (
        path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")),
        home,
    ) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => std::path::PathBuf::from(path),
    }
}
