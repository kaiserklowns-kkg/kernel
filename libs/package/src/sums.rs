//! A release's `SHA256SUMS`, checked on the device (ADR-0158): its
//! signature by a key the system trusts for updates (`update.keys`), and its
//! `HASH  NAME` lines. The same format `oceans release` signs and Oceans
//! Setup checks (ADR-0075, ADR-0128).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use sha2::{Digest, Sha256};

use crate::TrustedKey;

/// What the signature covers: this context, then the checksums' text.
const CONTEXT: &[u8] = b"Oceans release checksums (ADR-0075)\n";

/// The checksums' file name in a release.
pub const SUMS: &str = "SHA256SUMS";
/// Its signature's file name.
pub const SIGNATURE: &str = "SHA256SUMS.sig";

/// A listed file: its SHA-256 in hex, and its plain name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub sha256: String,
    pub name: String,
}

/// Why checksums were refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SumsError {
    /// The signature is not 128 hex digits.
    NotASignature,
    /// No key the system trusts for updates signed it.
    NotSigned,
    /// A line is not `HASH  NAME`, or names more than a plain file.
    BadLine,
    /// It lists nothing.
    Empty,
}

impl SumsError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotASignature => "SHA256SUMS.sig is not a signature",
            Self::NotSigned => "SHA256SUMS is not signed by a key this system trusts for updates",
            Self::BadLine => "SHA256SUMS has a line that is not `HASH  NAME` of a plain file",
            Self::Empty => "SHA256SUMS lists no files",
        }
    }
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let text = text.trim();
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// `bytes` in lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The SHA-256 of `bytes`, in hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Checks `sums` against `signature` (its `.sig` file's text) by any of
/// `keys`, then returns its lines. Names are plain file names only: a
/// signed list must not reach anywhere but beside itself.
pub fn verify(
    sums: &str,
    signature: &str,
    keys: &[TrustedKey<'_>],
) -> Result<Vec<Listed>, SumsError> {
    let signature: [u8; 64] = unhex(signature).ok_or(SumsError::NotASignature)?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature);
    let mut message = CONTEXT.to_vec();
    message.extend_from_slice(sums.as_bytes());
    let signed = keys.iter().any(|trusted| {
        ed25519_dalek::VerifyingKey::from_bytes(&trusted.key)
            .is_ok_and(|key| key.verify_strict(&message, &signature).is_ok())
    });
    if !signed {
        return Err(SumsError::NotSigned);
    }
    let mut listed = Vec::new();
    for line in sums.lines().filter(|line| !line.trim().is_empty()) {
        let (sha256, name) = line.split_once("  ").ok_or(SumsError::BadLine)?;
        let plain = !name.is_empty()
            && !name.contains(['/', '\\', ':'])
            && !name.starts_with('.')
            && unhex::<32>(sha256).is_some();
        if !plain {
            return Err(SumsError::BadLine);
        }
        listed.push(Listed {
            sha256: sha256.to_ascii_lowercase(),
            name: name.to_string(),
        });
    }
    if listed.is_empty() {
        return Err(SumsError::Empty);
    }
    Ok(listed)
}

/// The system package among `listed`: the one `.opk` that is not the
/// Secure Boot image's (as Oceans Setup chooses, ADR-0128).
pub fn system_package(listed: &[Listed]) -> Option<&Listed> {
    let mut packages = listed
        .iter()
        .filter(|l| l.name.ends_with(".opk") && !l.name.contains("secure-boot"));
    let first = packages.next()?;
    packages.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn sign(seed: [u8; 32], sums: &str) -> (TrustedKey<'static>, String) {
        let key = SigningKey::from_bytes(&seed);
        let mut message = CONTEXT.to_vec();
        message.extend_from_slice(sums.as_bytes());
        let signature = hex(&key.sign(&message).to_bytes());
        (
            TrustedKey {
                publisher: "Oceans Test Release",
                key: key.verifying_key().to_bytes(),
            },
            signature,
        )
    }

    const SUMS_TEXT: &str = "\
        aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  oceans-0.1.1-alpha.opk\n\
        bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  oceans-0.1.1-alpha-usb.img\n";

    #[test]
    fn signed_sums_are_read_and_the_package_found() {
        let (key, signature) = sign([7; 32], SUMS_TEXT);
        let listed = verify(SUMS_TEXT, &signature, &[key]).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            system_package(&listed).map(|l| l.name.as_str()),
            Some("oceans-0.1.1-alpha.opk")
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }

    #[test]
    fn changed_or_unsigned_sums_are_refused() {
        let (key, signature) = sign([7; 32], SUMS_TEXT);
        let changed = SUMS_TEXT.replace("0.1.1", "6.6.6");
        assert_eq!(
            verify(&changed, &signature, &[key]),
            Err(SumsError::NotSigned)
        );
        let (other, _) = sign([9; 32], SUMS_TEXT);
        assert_eq!(
            verify(SUMS_TEXT, &signature, &[other]),
            Err(SumsError::NotSigned)
        );
        assert_eq!(
            verify(SUMS_TEXT, "zz", &[key]),
            Err(SumsError::NotASignature)
        );
    }

    #[test]
    fn names_reach_nowhere_else() {
        for bad in [
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  ../evil.opk\n",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  /bin/x.opk\n",
            "aaaa  short.opk\n",
            "no two spaces\n",
        ] {
            let (key, signature) = sign([7; 32], bad);
            assert_eq!(
                verify(bad, &signature, &[key]),
                Err(SumsError::BadLine),
                "{bad}"
            );
        }
        let (key, signature) = sign([7; 32], "\n");
        assert_eq!(verify("\n", &signature, &[key]), Err(SumsError::Empty));
    }
}
