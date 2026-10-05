//! Signed release checksums (ADR-0075).
//!
//! A release's `SHA256SUMS` is signed with the release key into
//! `SHA256SUMS.sig`, so that a download can be checked against the
//! release key's public half, obtained elsewhere (the repository, the
//! project's site), not against checksums from the same page.

use std::fmt::Write as _;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use sha2::Digest;

use crate::DeveloperKey;

/// What is signed: this context, then the text of `SHA256SUMS`. The
/// context keeps the signature from ever passing for a package's.
const CONTEXT: &[u8] = b"Oceans release checksums (ADR-0075)\n";

pub const SUMS: &str = "SHA256SUMS";
pub const SIGNATURE: &str = "SHA256SUMS.sig";

fn hex(bytes: &[u8]) -> String {
    let mut text = String::new();
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
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

fn message(sums: &str) -> Vec<u8> {
    let mut message = CONTEXT.to_vec();
    message.extend_from_slice(sums.as_bytes());
    message
}

/// The SHA-256 of `bytes`, in hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha2::Sha256::digest(bytes))
}

/// `SHA256SUMS.sig`'s text: the signature of `sums` by `key`, in hex.
pub fn sign_checksums(key: &DeveloperKey, sums: &str) -> String {
    let signature = SigningKey::from_bytes(&key.seed).sign(&message(sums));
    format!("{}\n", hex(&signature.to_bytes()))
}

/// Checks a downloaded release in `dir` against the release key
/// `public_hex`: the signature of `SHA256SUMS`, then every file it lists.
/// Returns the names checked.
pub fn verify_release(dir: &Path, public_hex: &str) -> Result<Vec<String>, String> {
    let public: [u8; 32] = unhex(public_hex).ok_or("the release key is not 64 hex digits")?;
    let key = VerifyingKey::from_bytes(&public).map_err(|_| "not an Ed25519 public key")?;
    let read = |name: &str| {
        std::fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()))
    };
    let sums = String::from_utf8(read(SUMS)?).map_err(|_| format!("{SUMS} is not text"))?;
    let signature_text =
        String::from_utf8(read(SIGNATURE)?).map_err(|_| format!("{SIGNATURE} is not text"))?;
    let signature: [u8; 64] =
        unhex(&signature_text).ok_or(format!("{SIGNATURE} is not a signature"))?;
    key.verify_strict(&message(&sums), &ed25519_dalek::Signature::from_bytes(&signature))
        .map_err(|_| {
            format!("{SUMS} is not signed by that key: the download was changed, or the key is not this release's")
        })?;
    let mut checked = Vec::new();
    for line in sums.lines().filter(|line| !line.trim().is_empty()) {
        let (want, name) = line
            .split_once("  ")
            .ok_or_else(|| format!("{SUMS}: not `HASH  NAME`: {line}"))?;
        // Only files beside SHA256SUMS: a signed list must not reach
        // anywhere else on the disk.
        if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') {
            return Err(format!("{SUMS}: {name}: not a plain file name"));
        }
        let got = sha256_hex(&read(name)?);
        if got != want {
            return Err(format!(
                "{name}: its SHA-256 does not match {SUMS}: the file was changed"
            ));
        }
        checked.push(name.to_string());
    }
    if checked.is_empty() {
        return Err(format!("{SUMS} lists no files"));
    }
    Ok(checked)
}
