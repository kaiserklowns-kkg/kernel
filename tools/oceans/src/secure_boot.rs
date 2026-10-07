//! Secure Boot signing (ADR-0091): the Oceans Secure Boot key (RSA-2048,
//! with a self-signed X.509 certificate that firmware enrols in `db`), and
//! Authenticode signatures on UEFI executables (PE32+), which firmware
//! checks before it runs them.
//!
//! UEFI firmware verifies only RSA signatures in PKCS#7 `SignedData` over
//! the image's Authenticode hash (Microsoft's "Windows Authenticode
//! Portable Executable Signature Format"), so this is separate from the
//! Ed25519 keys that sign packages and releases. The DER is written by
//! hand: a few fixed structures, each checked by tests (and, end to end,
//! by the firmware in `cargo xtask smoke-secure-boot`).

use rsa::pkcs1::EncodeRsaPublicKey;
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};

// ---- DER ---------------------------------------------------------------

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = len
            .to_be_bytes()
            .into_iter()
            .skip_while(|&b| b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend_from_slice(&bytes);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// A SET: DER sorts its elements by their encodings.
fn set(parts: &[&[u8]]) -> Vec<u8> {
    let mut sorted: Vec<&[u8]> = parts.to_vec();
    sorted.sort_unstable();
    tlv(0x31, &sorted.concat())
}

fn explicit(number: u8, content: &[u8]) -> Vec<u8> {
    tlv(0xa0 | number, content)
}

fn oid(arcs: &[u64]) -> Vec<u8> {
    let mut body = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &arc in &arcs[2..] {
        let mut groups = vec![(arc & 0x7f) as u8];
        let mut rest = arc >> 7;
        while rest > 0 {
            groups.push(0x80 | (rest & 0x7f) as u8);
            rest >>= 7;
        }
        groups.reverse();
        body.extend(groups);
    }
    tlv(0x06, &body)
}

/// A non-negative INTEGER from big-endian bytes.
fn unsigned(bytes: &[u8]) -> Vec<u8> {
    let trimmed: Vec<u8> = bytes.iter().copied().skip_while(|&b| b == 0).collect();
    let mut body = if trimmed.is_empty() { vec![0] } else { trimmed };
    if body[0] & 0x80 != 0 {
        body.insert(0, 0);
    }
    tlv(0x02, &body)
}

fn octets(bytes: &[u8]) -> Vec<u8> {
    tlv(0x04, bytes)
}

fn bits(bytes: &[u8]) -> Vec<u8> {
    let mut body = vec![0];
    body.extend_from_slice(bytes);
    tlv(0x03, &body)
}

const NULL: &[u8] = &[0x05, 0x00];
const SHA256: &[u64] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];
const RSA_ENCRYPTION: &[u64] = &[1, 2, 840, 113_549, 1, 1, 1];
const SHA256_WITH_RSA: &[u64] = &[1, 2, 840, 113_549, 1, 1, 11];
const DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 1];
const SIGNED_DATA: &[u64] = &[1, 2, 840, 113_549, 1, 7, 2];
const CONTENT_TYPE: &[u64] = &[1, 2, 840, 113_549, 1, 9, 3];
const MESSAGE_DIGEST: &[u64] = &[1, 2, 840, 113_549, 1, 9, 4];
const SPC_INDIRECT_DATA: &[u64] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 4];
const SPC_PE_IMAGE_DATA: &[u64] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 15];
const SPC_SP_OPUS_INFO: &[u64] = &[1, 3, 6, 1, 4, 1, 311, 2, 1, 12];
const COMMON_NAME: &[u64] = &[2, 5, 4, 3];
const KEY_USAGE: &[u64] = &[2, 5, 29, 15];
const EXTENDED_KEY_USAGE: &[u64] = &[2, 5, 29, 37];
const CODE_SIGNING: &[u64] = &[1, 3, 6, 1, 5, 5, 7, 3, 3];

fn algorithm(arcs: &[u64]) -> Vec<u8> {
    seq(&[&oid(arcs), NULL])
}

fn name(common_name: &str) -> Vec<u8> {
    let attribute = seq(&[&oid(COMMON_NAME), &tlv(0x0c, common_name.as_bytes())]);
    seq(&[&set(&[&attribute])])
}

// ---- The key -----------------------------------------------------------

/// The Secure Boot key: an RSA-2048 private key and its certificate.
pub struct SecureBootKey {
    key: RsaPrivateKey,
    /// The self-signed certificate (DER), what firmware enrols.
    pub certificate: Vec<u8>,
    /// Its subject and issuer (the same: self-signed).
    common_name: String,
    serial: [u8; 16],
}

impl SecureBootKey {
    /// A new key and certificate for `common_name`, valid from 2025 to 2055.
    pub fn generate(common_name: &str) -> Result<Self, String> {
        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).map_err(|e| e.to_string())?;
        let mut serial = [0u8; 16];
        getrandom::getrandom(&mut serial).map_err(|e| e.to_string())?;
        // Positive and of fixed length.
        serial[0] = (serial[0] & 0x7f) | 0x40;
        Self::with(key, common_name, serial)
    }

    fn with(key: RsaPrivateKey, common_name: &str, serial: [u8; 16]) -> Result<Self, String> {
        let public = RsaPublicKey::from(&key)
            .to_pkcs1_der()
            .map_err(|e| e.to_string())?;
        let spki = seq(&[&algorithm(RSA_ENCRYPTION), &bits(public.as_bytes())]);
        let validity = seq(&[&tlv(0x17, b"250101000000Z"), &tlv(0x18, b"20551231235959Z")]);
        let extensions = seq(&[
            // Key usage (critical): digital signature.
            &seq(&[
                &oid(KEY_USAGE),
                &[0x01, 0x01, 0xff],
                &octets(&[0x03, 0x02, 0x07, 0x80]),
            ]),
            // Extended key usage: code signing.
            &seq(&[
                &oid(EXTENDED_KEY_USAGE),
                &octets(&seq(&[&oid(CODE_SIGNING)])),
            ]),
        ]);
        let tbs = seq(&[
            &explicit(0, &unsigned(&[2])),
            &unsigned(&serial),
            &algorithm(SHA256_WITH_RSA),
            &name(common_name),
            &validity,
            &name(common_name),
            &spki,
            &explicit(3, &extensions),
        ]);
        let signature = SigningKey::<Sha256>::new(key.clone()).sign(&tbs);
        let certificate = seq(&[
            &tbs,
            &algorithm(SHA256_WITH_RSA),
            &bits(&signature.to_bytes()),
        ]);
        Ok(Self {
            key,
            certificate,
            common_name: common_name.to_string(),
            serial,
        })
    }

    /// The key file: a small text header, then the PKCS#8 private key and
    /// the certificate, both in hex.
    pub fn to_file(&self) -> Result<String, String> {
        let der = self.key.to_pkcs8_der().map_err(|e| e.to_string())?;
        Ok(format!(
            "oceans-secure-boot-key 1\nname {}\nserial {}\nkey {}\ncertificate {}\n",
            self.common_name,
            hex(&self.serial),
            hex(der.as_bytes()),
            hex(&self.certificate)
        ))
    }

    pub fn from_file(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        if lines.next() != Some("oceans-secure-boot-key 1") {
            return Err("not an Oceans Secure Boot key file".into());
        }
        let mut field = |name: &str| -> Result<String, String> {
            let line = lines.next().ok_or("the key file is cut short")?;
            line.strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(' '))
                .map(str::to_string)
                .ok_or_else(|| format!("the key file has no {name}"))
        };
        let common_name = field("name")?;
        let serial: [u8; 16] = unhex(&field("serial")?)?
            .try_into()
            .map_err(|_| "the serial is not 16 bytes")?;
        let key = RsaPrivateKey::from_pkcs8_der(&unhex(&field("key")?)?)
            .map_err(|e| format!("the private key: {e}"))?;
        let certificate = unhex(&field("certificate")?)?;
        let rebuilt = Self::with(key, &common_name, serial)?;
        if rebuilt.certificate != certificate {
            return Err("the certificate does not belong to the key".into());
        }
        Ok(rebuilt)
    }

    /// The SHA-256 of the certificate, to name it (as `sbverify` and
    /// firmware menus show certificates).
    pub fn fingerprint(&self) -> String {
        hex(&Sha256::digest(&self.certificate))
    }

    /// `image` (a PE32+ UEFI executable, unsigned) with an Authenticode
    /// signature by this key appended.
    pub fn sign(&self, image: &[u8]) -> Result<Vec<u8>, String> {
        let pe = Pe::parse(image)?;
        if pe.certificates.1 != 0 {
            return Err("the image is signed already".into());
        }
        // The certificate table must start 8-byte aligned; the padding is
        // part of what is hashed.
        let mut signed = image.to_vec();
        signed.resize(signed.len().next_multiple_of(8), 0);
        let digest = pe.digest(&signed);
        let content = indirect_data(&digest);
        let signed_data = self.signed_data(&content);

        let mut entry = Vec::new();
        let total = (8 + signed_data.len()).next_multiple_of(8);
        entry.extend_from_slice(&(total as u32).to_le_bytes());
        entry.extend_from_slice(&0x0200u16.to_le_bytes()); // WIN_CERT_REVISION_2_0
        entry.extend_from_slice(&0x0002u16.to_le_bytes()); // WIN_CERT_TYPE_PKCS_SIGNED_DATA
        entry.extend_from_slice(&signed_data);
        entry.resize(total, 0);
        let at = signed.len() as u32;
        signed.extend_from_slice(&entry);
        signed[pe.certificate_entry..pe.certificate_entry + 4].copy_from_slice(&at.to_le_bytes());
        signed[pe.certificate_entry + 4..pe.certificate_entry + 8]
            .copy_from_slice(&(total as u32).to_le_bytes());
        Ok(signed)
    }

    /// PKCS#7 `SignedData` over `content` (an `SpcIndirectDataContent`),
    /// signed with this key, carrying the certificate.
    fn signed_data(&self, content: &[u8]) -> Vec<u8> {
        // The message digest covers the content's value, without its own
        // tag and length (Authenticode).
        let value = &content[header_len(content)..];
        let attributes = [
            seq(&[&oid(CONTENT_TYPE), &set(&[&oid(SPC_INDIRECT_DATA)])]),
            seq(&[
                &oid(MESSAGE_DIGEST),
                &set(&[&octets(&Sha256::digest(value))]),
            ]),
            seq(&[&oid(SPC_SP_OPUS_INFO), &set(&[&seq(&[])])]),
        ];
        let parts: Vec<&[u8]> = attributes.iter().map(Vec::as_slice).collect();
        // Signed as a SET; carried as [0] IMPLICIT with the same contents.
        let signed_attributes = set(&parts);
        let signature = SigningKey::<Sha256>::new(self.key.clone()).sign(&signed_attributes);
        let mut carried = signed_attributes.clone();
        carried[0] = 0xa0;
        let signer = seq(&[
            &unsigned(&[1]),
            &seq(&[&name(&self.common_name), &unsigned(&self.serial)]),
            &algorithm(SHA256),
            &carried,
            &algorithm(RSA_ENCRYPTION),
            &octets(&signature.to_bytes()),
        ]);
        let signed_data = seq(&[
            &unsigned(&[1]),
            &set(&[&algorithm(SHA256)]),
            &seq(&[&oid(SPC_INDIRECT_DATA), &explicit(0, content)]),
            &tlv(0xa0, &self.certificate),
            &set(&[&signer]),
        ]);
        seq(&[&oid(SIGNED_DATA), &explicit(0, &signed_data)])
    }

    /// A time-based authenticated UEFI variable (UEFI 2.10 §8.2.2) holding
    /// this key's certificate as an `EFI_SIGNATURE_LIST`, signed by this key:
    /// what `SetVariable` takes for `db`, `KEK` or `PK`. Firmware that wants
    /// even the first `PK` self-signed (edk2's `PcdRequireSelfSignedPk`)
    /// gets that. `variable` is its name, `guid` its vendor GUID (as
    /// stored, little-endian fields), `attributes` what it is set with,
    /// `time` its `EFI_TIME` (16 bytes).
    pub fn authenticated_certificate_variable(
        &self,
        variable: &str,
        guid: &[u8; 16],
        attributes: u32,
        time: &[u8; 16],
        owner: &[u8; 16],
    ) -> Vec<u8> {
        // EFI_SIGNATURE_LIST: EFI_CERT_X509_GUID, one EFI_SIGNATURE_DATA.
        const CERT_X509: [u8; 16] = [
            0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b,
            0xf0, 0x72,
        ];
        const CERT_TYPE_PKCS7: [u8; 16] = [
            0x9d, 0xd2, 0xaf, 0x4a, 0xdf, 0x68, 0xee, 0x49, 0x8a, 0xa9, 0x34, 0x7d, 0x37, 0x56,
            0x65, 0xa7,
        ];
        let signature_size = 16 + self.certificate.len();
        let mut list = CERT_X509.to_vec();
        list.extend_from_slice(&((28 + signature_size) as u32).to_le_bytes());
        list.extend_from_slice(&0u32.to_le_bytes());
        list.extend_from_slice(&(signature_size as u32).to_le_bytes());
        list.extend_from_slice(owner);
        list.extend_from_slice(&self.certificate);

        // What is signed: the name (UTF-16, no terminator), the GUID, the
        // attributes, the time, then the data.
        let mut message: Vec<u8> = variable.encode_utf16().flat_map(u16::to_le_bytes).collect();
        message.extend_from_slice(guid);
        message.extend_from_slice(&attributes.to_le_bytes());
        message.extend_from_slice(time);
        message.extend_from_slice(&list);
        // A detached PKCS#7 SignedData (no ContentInfo around it, as the
        // UEFI specification puts it in CertData), no signed attributes.
        let signature = SigningKey::<Sha256>::new(self.key.clone()).sign(&message);
        let signer = seq(&[
            &unsigned(&[1]),
            &seq(&[&name(&self.common_name), &unsigned(&self.serial)]),
            &algorithm(SHA256),
            &algorithm(RSA_ENCRYPTION),
            &octets(&signature.to_bytes()),
        ]);
        let signed_data = seq(&[
            &unsigned(&[1]),
            &set(&[&algorithm(SHA256)]),
            &seq(&[&oid(DATA)]),
            &tlv(0xa0, &self.certificate),
            &set(&[&signer]),
        ]);

        // EFI_VARIABLE_AUTHENTICATION_2: the time, then a
        // WIN_CERTIFICATE_UEFI_GUID carrying the SignedData.
        let mut out = time.to_vec();
        out.extend_from_slice(&((24 + signed_data.len()) as u32).to_le_bytes());
        out.extend_from_slice(&0x0200u16.to_le_bytes());
        out.extend_from_slice(&0x0ef1u16.to_le_bytes());
        out.extend_from_slice(&CERT_TYPE_PKCS7);
        out.extend_from_slice(&signed_data);
        out.extend_from_slice(&list);
        out
    }
}

/// `SpcIndirectDataContent` for a PE image with this SHA-256 `digest`.
fn indirect_data(digest: &[u8]) -> Vec<u8> {
    // SpcPeImageData: no flags, and the customary "<<<Obsolete>>>" link
    // (a BMPString in an SpcLink's file choice).
    let obsolete: Vec<u8> = "<<<Obsolete>>>"
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    let link = explicit(2, &tlv(0x80, &obsolete));
    let image_data = seq(&[&[0x03, 0x01, 0x00], &explicit(0, &link)]);
    seq(&[
        &seq(&[&oid(SPC_PE_IMAGE_DATA), &image_data]),
        &seq(&[&algorithm(SHA256), &octets(digest)]),
    ])
}

/// Bytes of a DER element's tag and length.
fn header_len(der: &[u8]) -> usize {
    match der[1] {
        len if len < 0x80 => 2,
        len => 2 + usize::from(len & 0x7f),
    }
}

// ---- PE32+ ---------------------------------------------------------------

/// What Authenticode needs of a PE32+ image's layout.
struct Pe {
    /// Offset of the optional header's CheckSum field.
    checksum: usize,
    /// Offset of the Certificate Table's data directory entry.
    certificate_entry: usize,
    /// The Certificate Table: offset and size (0 if unsigned).
    certificates: (usize, usize),
    size_of_headers: usize,
    /// Sections' raw data (offset, size), in file order.
    sections: Vec<(usize, usize)>,
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "the image is cut short".to_string())
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "the image is cut short".to_string())
}

impl Pe {
    fn parse(image: &[u8]) -> Result<Self, String> {
        if image.get(..2) != Some(b"MZ") {
            return Err("not a PE image".into());
        }
        let pe = u32_at(image, 0x3c)? as usize;
        if image.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Err("not a PE image".into());
        }
        let sections = usize::from(u16_at(image, pe + 6)?);
        let optional_size = usize::from(u16_at(image, pe + 20)?);
        let optional = pe + 24;
        if u16_at(image, optional)? != 0x20b {
            return Err("not a PE32+ (64-bit) image".into());
        }
        if u32_at(image, optional + 108)? <= 4 {
            return Err("the image has no certificate table entry".into());
        }
        let certificate_entry = optional + 112 + 4 * 8;
        let certificates = (
            u32_at(image, certificate_entry)? as usize,
            u32_at(image, certificate_entry + 4)? as usize,
        );
        let size_of_headers = u32_at(image, optional + 60)? as usize;
        let table = optional + optional_size;
        let mut list = Vec::new();
        for index in 0..sections {
            let header = table + index * 40;
            let size = u32_at(image, header + 16)? as usize;
            let offset = u32_at(image, header + 20)? as usize;
            if size > 0 {
                if offset.checked_add(size).is_none_or(|end| end > image.len()) {
                    return Err("a section lies outside the image".into());
                }
                list.push((offset, size));
            }
        }
        list.sort_unstable();
        if size_of_headers > image.len() || certificate_entry + 8 > size_of_headers {
            return Err("the headers are malformed".into());
        }
        Ok(Self {
            checksum: optional + 64,
            certificate_entry,
            certificates,
            size_of_headers,
            sections: list,
        })
    }

    /// The Authenticode SHA-256 of `image` (laid out as parsed): the
    /// headers without the checksum and the certificate table entry, the
    /// sections in file order, then whatever follows them, except the
    /// certificate table.
    fn digest(&self, image: &[u8]) -> Vec<u8> {
        let mut hash = Sha256::new();
        hash.update(&image[..self.checksum]);
        hash.update(&image[self.checksum + 4..self.certificate_entry]);
        hash.update(&image[self.certificate_entry + 8..self.size_of_headers]);
        let mut hashed = self.size_of_headers;
        for &(offset, size) in &self.sections {
            hash.update(&image[offset..offset + size]);
            hashed = hashed.max(offset + size);
        }
        let end = if self.certificates.1 > 0 {
            self.certificates.0
        } else {
            image.len()
        };
        if end > hashed {
            hash.update(&image[hashed..end]);
        }
        hash.finalize().to_vec()
    }
}

/// Checks that `image` carries an Authenticode signature by the key of
/// `certificate` over its current contents (what firmware checks, for
/// signatures made by [`SecureBootKey::sign`]).
pub fn verify(image: &[u8], key: &SecureBootKey) -> Result<(), String> {
    let pe = Pe::parse(image)?;
    let (at, size) = pe.certificates;
    if size == 0 {
        return Err("the image is not signed".into());
    }
    if at.checked_add(size).is_none_or(|end| end > image.len()) {
        return Err("the certificate table lies outside the image".into());
    }
    let digest = pe.digest(image);
    let length = u32_at(image, at)? as usize;
    let der = &image[at + 8..at + length.min(size)];
    // The signature, rebuilt from the digest found now, must be the one
    // carried: PKCS#1 v1.5 signatures are deterministic.
    let expected = key.signed_data(&indirect_data(&digest));
    if !der.starts_with(&expected) {
        return Err("the signature does not match the image".into());
    }
    Ok(())
}

/// Checks an RSA PKCS#1 v1.5 SHA-256 `signature` of `message` by the key
/// in `certificate`'s holder (for tests of the certificate itself).
pub fn verify_raw(key: &SecureBootKey, message: &[u8], signature: &[u8]) -> bool {
    let verifying = VerifyingKey::<Sha256>::new(RsaPublicKey::from(&key.key));
    Signature::try_from(signature)
        .map(|signature| verifying.verify(message, &signature).is_ok())
        .unwrap_or(false)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("odd hex".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| "bad hex".to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The development Secure Boot key (public, in this repository).
    fn dev_key() -> SecureBootKey {
        SecureBootKey::from_file(include_str!("../../keys/oceans-dev-secure-boot.key")).unwrap()
    }

    /// A minimal PE32+ image: headers, two sections, no certificate.
    fn pe_image() -> Vec<u8> {
        let mut image = vec![0u8; 0x600];
        image[..2].copy_from_slice(b"MZ");
        image[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        image[0x80..0x84].copy_from_slice(b"PE\0\0");
        image[0x86..0x88].copy_from_slice(&2u16.to_le_bytes()); // sections
        image[0x94..0x96].copy_from_slice(&240u16.to_le_bytes()); // optional size
        let optional = 0x98;
        image[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        image[optional + 60..optional + 64].copy_from_slice(&0x200u32.to_le_bytes());
        image[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        let table = optional + 240;
        for (i, (offset, size)) in [(0x200u32, 0x200u32), (0x400, 0x1f0)]
            .into_iter()
            .enumerate()
        {
            let header = table + i * 40;
            image[header + 16..header + 20].copy_from_slice(&size.to_le_bytes());
            image[header + 20..header + 24].copy_from_slice(&offset.to_le_bytes());
        }
        for (i, byte) in image[0x200..0x5f0].iter_mut().enumerate() {
            *byte = (i * 7 % 251) as u8;
        }
        image.truncate(0x5f3);
        image
    }

    #[test]
    fn der_encodes_as_the_standard_says() {
        assert_eq!(
            oid(SHA256),
            [
                0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01
            ]
        );
        assert_eq!(
            oid(SPC_INDIRECT_DATA)[2..],
            [0x2b, 6, 1, 4, 1, 0x82, 0x37, 2, 1, 4]
        );
        assert_eq!(unsigned(&[0x80]), [0x02, 0x02, 0x00, 0x80]);
        assert_eq!(unsigned(&[0, 0, 5]), [0x02, 0x01, 0x05]);
        assert_eq!(tlv(0x04, &[0; 200])[..3], [0x04, 0x81, 200]);
        assert_eq!(tlv(0x04, &[0; 300])[..4], [0x04, 0x82, 0x01, 0x2c]);
        assert_eq!(
            set(&[&[0x02, 0x01, 0x09], &[0x02, 0x01, 0x01]]),
            [0x31, 6, 2, 1, 1, 2, 1, 9]
        );
    }

    #[test]
    fn the_key_file_round_trips_and_the_certificate_is_self_signed() {
        let key = dev_key();
        let again = SecureBootKey::from_file(&key.to_file().unwrap()).unwrap();
        assert_eq!(again.certificate, key.certificate);
        // The certificate: tbs, algorithm, signature; signed by the key.
        let cert = &key.certificate;
        let body = &cert[header_len(cert)..];
        let tbs_len = header_len(body) + der_len(body);
        let tbs = &body[..tbs_len];
        let rest = &body[tbs_len..];
        let algorithm_len = header_len(rest) + der_len(rest);
        let signature = &rest[algorithm_len..];
        let signature = &signature[header_len(signature) + 1..];
        assert!(verify_raw(&key, tbs, signature));
        // A key file whose certificate was changed is refused.
        let tampered = key
            .to_file()
            .unwrap()
            .replace("certificate 30", "certificate 31");
        assert!(SecureBootKey::from_file(&tampered).is_err());
    }

    fn der_len(der: &[u8]) -> usize {
        match der[1] {
            len if len < 0x80 => usize::from(len),
            len => der[2..2 + usize::from(len & 0x7f)]
                .iter()
                .fold(0, |n, &b| n << 8 | usize::from(b)),
        }
    }

    #[test]
    fn signs_and_verifies_a_pe_image() {
        let key = dev_key();
        let image = pe_image();
        let signed = key.sign(&image).unwrap();
        // The sections unchanged, padded to 8 bytes, then the certificate
        // table, which the headers now point at.
        assert_eq!(&signed[0x200..image.len()], &image[0x200..]);
        let pe = Pe::parse(&signed).unwrap();
        assert_eq!(pe.certificates.0, 0x5f8);
        assert_eq!(pe.certificates.0 + pe.certificates.1, signed.len());
        assert_eq!(pe.certificates.1 % 8, 0);
        verify(&signed, &key).unwrap();
        // The digest leaves out the checksum and the table itself.
        let mut checksum = signed.clone();
        checksum[pe.checksum] ^= 0xff;
        verify(&checksum, &key).unwrap();
        // Any change to a section breaks it; so does signing twice.
        let mut changed = signed.clone();
        changed[0x300] ^= 1;
        assert!(verify(&changed, &key).is_err());
        assert!(key.sign(&signed).is_err());
        assert!(verify(&image, &key).is_err());
        // Not PE32+.
        assert!(key.sign(b"MZ").is_err());
        let mut pe32 = image.clone();
        pe32[0x98] = 0x0b;
        pe32[0x99] = 0x01;
        assert!(key.sign(&pe32).is_err());
    }
}
