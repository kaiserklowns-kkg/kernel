//! Getting a release and checking it (ADR-0075, ADR-0128): `SHA256SUMS`
//! signed with the release key, the system package listed in it, and the
//! package's own signature by the same key. Only what the install needs is
//! downloaded: the checksums, their signature and the package.

use std::path::Path;

use oceans_package::{Package, TrustedKey};

/// Where releases are published: the latest one's files, by name.
pub const RELEASES: &str = "https://github.com/kaiserklowns-kkg/kernel/releases/latest/download/";
/// The release key's public half, as published in the repository.
pub const PUBLISHED_KEY: &str = include_str!("../../keys/oceans-release.pub");

/// The key's hex and its publisher, from the published file
/// (`KEY-HEX PUBLISHER`): a package must be signed by that key under that
/// name.
pub fn release_key() -> Option<(&'static str, &'static str)> {
    let line = PUBLISHED_KEY
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))?;
    let (key, publisher) = line.split_once(char::is_whitespace)?;
    Some((key, publisher.trim()))
}

/// A release, checked: what goes on the staging partition.
pub struct Release {
    /// `VERSION CHANNEL`.
    pub name: String,
    pub publisher: String,
    pub package: Vec<u8>,
    pub kernel: Vec<u8>,
    pub initrd: Vec<u8>,
    pub limine: Vec<u8>,
}

/// The system package's name among the checksums: the one `.opk` that is
/// not the Secure Boot image's.
pub fn package_name(listed: &[(String, String)]) -> Option<&str> {
    let mut packages = listed
        .iter()
        .map(|(_, name)| name.as_str())
        .filter(|name| name.ends_with(".opk"));
    let first = packages.next()?;
    packages.next().is_none().then_some(first)
}

/// Checks a release whose files `fetch` returns by name, against the
/// release key `key_hex` of `publisher`. `say` reports progress.
pub fn check(
    key_hex: &str,
    publisher: &str,
    mut fetch: impl FnMut(&str) -> Result<Vec<u8>, String>,
    mut say: impl FnMut(&str),
) -> Result<Release, String> {
    let text = |bytes: Vec<u8>, name: &str| {
        String::from_utf8(bytes).map_err(|_| format!("{name} is not text"))
    };
    let sums = text(fetch(oceans_dev::release::SUMS)?, oceans_dev::release::SUMS)?;
    let signature = text(
        fetch(oceans_dev::release::SIGNATURE)?,
        oceans_dev::release::SIGNATURE,
    )?;
    let listed = oceans_dev::release::verify_checksums(&sums, &signature, key_hex)?;
    say("the release's checksums are signed by the Oceans release key");
    let name = package_name(&listed).ok_or("the release lists no single system package")?;
    say(&format!("downloading {name}"));
    let package = fetch(name)?;
    let want = &listed.iter().find(|(_, n)| n == name).expect("listed").0;
    if &oceans_dev::release::sha256_hex(&package) != want {
        return Err(format!(
            "{name} does not match its checksum: the download was changed"
        ));
    }
    let key = oceans_dev::release::parse_key(key_hex).ok_or("the release key is not valid")?;
    let trusted = [TrustedKey { publisher, key }];
    let opened = Package::open(&package, &trusted)
        .map_err(|e| format!("{name} is refused: {}", e.message()))?;
    if opened.manifest.id != "system.oceans" {
        return Err(format!("{name} is not an Oceans release"));
    }
    let file = |name: &str| {
        opened
            .file(name)
            .map(<[u8]>::to_vec)
            .ok_or(format!("the release lacks {name}"))
    };
    let release = Release {
        name: format!("{} {}", opened.manifest.version, opened.manifest.channel),
        publisher: opened.manifest.publisher.to_string(),
        kernel: file(oceans_install::KERNEL)?,
        initrd: file(oceans_install::INITRD)?,
        limine: file("limine.efi")?,
        package: Vec::new(),
    };
    say(&format!(
        "Oceans {} from {}, verified",
        release.name, release.publisher
    ));
    Ok(Release { package, ..release })
}

/// Fetches from a local folder (`--from`).
pub fn from_folder(dir: &Path) -> impl FnMut(&str) -> Result<Vec<u8>, String> + '_ {
    move |name| std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))
}

/// The staging partition's files for `release`, naming `esp` as the
/// installed system's ESP.
pub fn staged(release: &Release, esp: &[u8; 16]) -> Vec<(&'static str, Vec<u8>)> {
    let configuration = oceans_install::staging_configuration(&release.name);
    let order = oceans_install::install_order(esp);
    oceans_install::staged_files(
        &release.package,
        &release.kernel,
        &release.initrd,
        &release.limine,
        &configuration,
        &order,
    )
    .iter()
    .map(|&(path, bytes)| (path, bytes.to_vec()))
    .collect()
}

/// Writes the staged files under `root` (a drive's root, or a folder for
/// a dry run).
pub fn write_staged(root: &Path, files: &[(&str, Vec<u8>)]) -> Result<(), String> {
    for (path, bytes) in files {
        let target = root.join(path);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&target, bytes).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_key_is_read() {
        let (key, publisher) = release_key().unwrap();
        assert_eq!((key.len(), publisher), (64, "Oceans"));
    }

    #[test]
    fn the_package_is_the_single_opk() {
        let listed = |names: &[&str]| -> Vec<(String, String)> {
            names
                .iter()
                .map(|n| (String::new(), n.to_string()))
                .collect()
        };
        assert_eq!(
            package_name(&listed(&[
                "a-usb.img",
                "oceans-0.1.0-alpha.opk",
                "BUILD-INFO"
            ])),
            Some("oceans-0.1.0-alpha.opk")
        );
        assert_eq!(package_name(&listed(&["a.opk", "b.opk"])), None);
        assert_eq!(package_name(&listed(&["a-usb.img"])), None);
    }
}
