//! Release images and the keys images trust (ADR-0072).
//!
//! Development images trust the development key (tools/keys), whose seed
//! is public: anyone can sign apps and system updates for them. A release
//! image trusts the **release key** instead, for apps (`trust.keys`) and
//! for system updates (`update.keys`). Its private half is a key file made
//! with `oceans keygen` and kept outside the repository; `cargo xtask
//! release` reads it from `OCEANS_RELEASE_KEY`.

use std::fmt::Write as _;

use super::*;
use oceans_dev::DeveloperKey;

/// Where `cargo xtask release` puts what it publishes.
const RELEASE_DIR: &str = "build/release";

/// The keys an image trusts, for apps and for system updates.
pub struct ImageKeys {
    pub publisher: String,
    public: String,
    release: bool,
    /// Signs the apps the image brings (ADR-0080).
    pub seed: [u8; 32],
}

impl ImageKeys {
    /// The development key (tools/keys): QEMU, the smoke tests, `usb`.
    pub fn development() -> Result<Self> {
        let seed = dev_seed()?;
        Ok(Self {
            publisher: DEV_PUBLISHER.to_string(),
            public: oceans_package::public_key_hex(&seed),
            release: false,
            seed,
        })
    }

    /// A release key: the only key such an image trusts.
    pub fn release(key: &DeveloperKey) -> Self {
        Self {
            publisher: key.publisher.clone(),
            public: key.public_hex(),
            release: true,
            seed: key.seed,
        }
    }

    fn which(&self) -> &'static str {
        if self.release {
            "The release key (ADR-0072)."
        } else {
            "The development key (tools/keys): never in a release image."
        }
    }

    /// The image's `trust.keys` (ADR-0046): publishers of apps.
    pub fn trust_list(&self) -> String {
        format!(
            "# Publisher keys Oceans Core trusts (ADR-0046): `KEY-HEX PUBLISHER`.\n\
             # {}\n{} {}\n",
            self.which(),
            self.public,
            self.publisher
        )
    }

    /// The image's `update.keys` (ADR-0071): publishers of systems.
    pub fn update_keys(&self) -> String {
        format!(
            "# Keys Oceans accepts system updates from (ADR-0071): `KEY-HEX PUBLISHER`.\n\
             # {}\n{} {}\n",
            self.which(),
            self.public,
            self.publisher
        )
    }
}

/// The hardware smoke test's release key: a test key, public like the
/// development key, so that the test boots an image that trusts a key
/// other than the development key (ADR-0072).
pub fn test_release_key() -> DeveloperKey {
    DeveloperKey {
        publisher: "Oceans Test Release".to_string(),
        seed: [0x0c; 32],
    }
}

/// Reads the release key, refusing one that cannot be private: inside
/// the repository, or the development key.
/// The Secure Boot key for a release (ADR-0091): outside the repository,
/// not the development one, and the one whose certificate is published in
/// [`PUBLISHED_SECURE_BOOT_CERTIFICATE`].
fn load_secure_boot_key(
    path: &Path,
    repository: &Path,
) -> Result<oceans_dev::secure_boot::SecureBootKey> {
    let full = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let repository = fs::canonicalize(repository).map_err(|e| e.to_string())?;
    if full.starts_with(&repository) {
        return Err(format!(
            "{} is inside the repository: keep the Secure Boot key elsewhere",
            path.display()
        ));
    }
    let text = fs::read_to_string(&full).map_err(|e| format!("{}: {e}", path.display()))?;
    let key = oceans_dev::secure_boot::SecureBootKey::from_file(&text)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if key.certificate == crate::secure_boot::development_key()?.certificate {
        return Err("that is the development Secure Boot key, which is public".into());
    }
    let published = fs::read(root().join(PUBLISHED_SECURE_BOOT_CERTIFICATE)).map_err(|_| {
        format!(
            "publish the Secure Boot certificate first: commit the key's .cer as \
             {PUBLISHED_SECURE_BOOT_CERTIFICATE} (what users enrol)"
        )
    })?;
    if published != key.certificate {
        return Err(format!(
            "that Secure Boot key's certificate is not the one published in \
             {PUBLISHED_SECURE_BOOT_CERTIFICATE}"
        ));
    }
    Ok(key)
}

/// The release Secure Boot certificate, published like the release key.
const PUBLISHED_SECURE_BOOT_CERTIFICATE: &str = "tools/keys/oceans-secure-boot.cer";

fn load_release_key(path: &Path, repository: &Path) -> Result<DeveloperKey> {
    let full = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let repository = fs::canonicalize(repository).map_err(|e| e.to_string())?;
    if full.starts_with(&repository) {
        return Err(format!(
            "{} is inside the repository: keep the release key elsewhere, \
             where it is never committed",
            path.display()
        ));
    }
    let text = fs::read_to_string(&full).map_err(|e| format!("{}: {e}", path.display()))?;
    let key = DeveloperKey::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if key.seed == dev_seed()? {
        return Err("that is the development key, whose seed is public".into());
    }
    if key.publisher == DEV_PUBLISHER {
        return Err(format!(
            "the release key's publisher must not be \"{DEV_PUBLISHER}\" (the development key's)"
        ));
    }
    Ok(key)
}

fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root())
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The release key's public half, in the repository (ADR-0075).
const PUBLISHED_KEY: &str = "tools/keys/oceans-release.pub";

/// Whether `key` is the one in the published key file (a trust list).
fn published_key_matches(published: &str, key: &DeveloperKey) -> bool {
    let public = key.public_hex();
    oceans_package::trusted_keys(published).any(|trusted| {
        let mut hex = String::new();
        for byte in trusted.key {
            let _ = write!(hex, "{byte:02x}");
        }
        hex == public && trusted.publisher == key.publisher
    })
}

/// `cargo xtask release`: the files of a release, in `build/release`:
///
/// ```text
/// oceans-VERSION-CHANNEL-usb.img   the USB image (release build, release key)
/// oceans-VERSION-CHANNEL.opk       the same release as a system update
/// release.keys                     the release key's public half
/// BUILD-INFO                       the commit it was built from
/// SHA256SUMS
/// SHA256SUMS.sig                   the checksums, signed (ADR-0075)
/// ```
///
/// With `OCEANS_SECURE_BOOT_KEY` (ADR-0091), also the image signed for
/// Secure Boot and its certificate:
///
/// ```text
/// oceans-VERSION-CHANNEL-secure-boot-usb.img
/// oceans-secure-boot.cer
/// ```
pub fn release() -> Result {
    let path = env::var_os("OCEANS_RELEASE_KEY").ok_or(
        "set OCEANS_RELEASE_KEY to the release key file: make one with \
         `oceans keygen \"Oceans\" --out PATH`, outside the repository, and keep it secret",
    )?;
    let key = load_release_key(Path::new(&path), &root())?;
    let published = fs::read_to_string(root().join(PUBLISHED_KEY)).unwrap_or_default();
    if !published_key_matches(&published, &key) {
        return Err(format!(
            "that key ({}) is not the release key published in {PUBLISHED_KEY}: \
             releases are checked against that one (ADR-0075)",
            key.public_hex()
        ));
    }
    let changes = git(&["status", "--porcelain"])?;
    if !changes.is_empty() {
        return Err(format!(
            "the working tree has changes: a release is built from a commit\n{changes}"
        ));
    }
    let commit = git(&["rev-parse", "HEAD"])?;
    let release = format!("{RELEASE_VERSION} {RELEASE_CHANNEL}");
    let stem = format!("oceans-{RELEASE_VERSION}-{RELEASE_CHANNEL}");

    let esp = build_image_for(
        Profile::Release,
        None,
        Setup::Hardware,
        &ImageKeys::release(&key),
    )?;
    let secure_boot_key = match env::var_os("OCEANS_SECURE_BOOT_KEY") {
        Some(path) => Some(load_secure_boot_key(Path::new(&path), &root())?),
        None => None,
    };
    hardware::write_usb_image(&esp)?;
    let update = hardware::system_package(&esp, RELEASE_VERSION, &key, "Oceans")?;

    let dir = root().join(RELEASE_DIR);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let image = fs::read(root().join(hardware::USB_IMAGE)).map_err(|e| e.to_string())?;
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (format!("{stem}-usb.img"), image),
        (format!("{stem}.opk"), update),
        (
            "release.keys".to_string(),
            ImageKeys::release(&key).update_keys().into_bytes(),
        ),
        (
            "BUILD-INFO".to_string(),
            format!(
                "Oceans {release}\ncommit {commit}\nkey {}\n",
                key.public_hex()
            )
            .into_bytes(),
        ),
    ];
    // The same release, signed for Secure Boot: its configuration and
    // Limine change, the update package (made above) does not.
    if let Some(secure_boot_key) = &secure_boot_key {
        crate::secure_boot::secure_esp(&esp, secure_boot_key)?;
        hardware::write_usb_image(&esp)?;
        let signed = fs::read(root().join(hardware::USB_IMAGE)).map_err(|e| e.to_string())?;
        files.push((format!("{stem}-secure-boot-usb.img"), signed));
        files.push((
            crate::secure_boot::CERTIFICATE_FILE.to_string(),
            secure_boot_key.certificate.clone(),
        ));
    }
    let mut sums = String::new();
    for (name, bytes) in &files {
        fs::write(dir.join(name), bytes).map_err(|e| format!("cannot write {name}: {e}"))?;
        let _ = writeln!(sums, "{}  {name}", oceans_dev::release::sha256_hex(bytes));
    }
    fs::write(dir.join("SHA256SUMS"), &sums).map_err(|e| e.to_string())?;
    // Signed with the release key (ADR-0075): `oceans verify` checks a
    // download against the key, not against checksums from the same page.
    let signature = oceans_dev::release::sign_checksums(&key, &sums);
    fs::write(dir.join(oceans_dev::release::SIGNATURE), &signature).map_err(|e| e.to_string())?;
    let checked = oceans_dev::release::verify_release(&dir, &key.public_hex())?;
    assert_eq!(checked.len(), files.len(), "every file is in SHA256SUMS");
    println!(
        "Oceans {release} is in {RELEASE_DIR} (commit {commit}), signed by \"{}\" ({}):\n{sums}",
        key.publisher,
        key.public_hex()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("oceans-release-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn release_images_trust_only_the_release_key() {
        let key = test_release_key();
        let keys = ImageKeys::release(&key);
        let dev = oceans_package::public_key_hex(&dev_seed().unwrap());
        for text in [keys.trust_list(), keys.update_keys()] {
            let trusted: Vec<_> = oceans_package::trusted_keys(&text).collect();
            assert_eq!(trusted.len(), 1);
            assert_eq!(trusted[0].publisher, key.publisher);
            assert!(!text.contains(&dev));
        }
        let development = ImageKeys::development().unwrap();
        assert!(development.trust_list().contains(&dev));
    }

    #[test]
    fn releases_use_the_published_key() {
        let published = fs::read_to_string(root().join(PUBLISHED_KEY)).unwrap();
        // The published key is a real key file's public half, not a test's.
        assert!(!published_key_matches(&published, &test_release_key()));
        let key = test_release_key();
        let mine = ImageKeys::release(&key).trust_list();
        assert!(published_key_matches(&mine, &key));
        // The same key under another publisher name is not it.
        let renamed = DeveloperKey {
            publisher: "Someone Else".to_string(),
            seed: key.seed,
        };
        assert!(!published_key_matches(&mine, &renamed));
    }

    #[test]
    fn release_keys_must_be_private() {
        let outside = scratch("outside");
        let repository = scratch("repository");
        let good = DeveloperKey {
            publisher: "Oceans".to_string(),
            seed: [9; 32],
        };
        let path = outside.join("release.key");
        fs::write(&path, good.to_file()).unwrap();
        assert_eq!(load_release_key(&path, &repository).unwrap(), good);
        // Inside the repository.
        let inside = repository.join("release.key");
        fs::write(&inside, good.to_file()).unwrap();
        assert!(load_release_key(&inside, &repository).is_err());
        // The development key, or its publisher.
        let dev = DeveloperKey {
            publisher: "Oceans".to_string(),
            seed: dev_seed().unwrap(),
        };
        fs::write(&path, dev.to_file()).unwrap();
        assert!(load_release_key(&path, &repository).is_err());
        let examples = DeveloperKey {
            publisher: DEV_PUBLISHER.to_string(),
            seed: [9; 32],
        };
        fs::write(&path, examples.to_file()).unwrap();
        assert!(load_release_key(&path, &repository).is_err());
    }
}
