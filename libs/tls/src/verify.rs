//! Signature verification for certificates and handshakes: ECDSA (P-256,
//! P-384), Ed25519, RSA PKCS#1 v1.5 and RSA-PSS.

use rustls::SignatureScheme;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{
    AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm, alg_id,
};
use sha2::{Digest, Sha256, Sha384, Sha512};

#[derive(Clone, Copy, Debug)]
enum HashKind {
    Sha256,
    Sha384,
    Sha512,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    EcdsaP256,
    EcdsaP384,
    Ed25519,
    RsaPkcs1,
    RsaPss,
}

#[derive(Debug)]
pub struct Algorithm {
    kind: Kind,
    hash: HashKind,
    public_key: AlgorithmIdentifier,
    signature: AlgorithmIdentifier,
}

/// RSA keys accepted, in bits (the Web PKI's range; the `rsa` crate stops
/// at 4096).
const RSA_BITS: core::ops::RangeInclusive<usize> = 2048..=8192;

fn ecdsa_p256(prehash: &[u8], public_key: &[u8], signature: &[u8]) -> Option<()> {
    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key).ok()?;
    let signature = p256::ecdsa::Signature::from_der(signature).ok()?;
    key.verify_prehash(prehash, &signature).ok()
}

fn ecdsa_p384(prehash: &[u8], public_key: &[u8], signature: &[u8]) -> Option<()> {
    use p384::ecdsa::signature::hazmat::PrehashVerifier;
    let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(public_key).ok()?;
    let signature = p384::ecdsa::Signature::from_der(signature).ok()?;
    key.verify_prehash(prehash, &signature).ok()
}

fn ed25519(message: &[u8], public_key: &[u8], signature: &[u8]) -> Option<()> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(public_key.try_into().ok()?).ok()?;
    let signature = ed25519_dalek::Signature::from_slice(signature).ok()?;
    key.verify_strict(message, &signature).ok()
}

fn rsa(
    kind: Kind,
    hash: HashKind,
    message: &[u8],
    public_key: &[u8],
    signature: &[u8],
) -> Option<()> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::signature::Verifier;
    use rsa::traits::PublicKeyParts;
    let key = rsa::RsaPublicKey::from_pkcs1_der(public_key).ok()?;
    if !RSA_BITS.contains(&(key.size() * 8)) {
        return None;
    }
    macro_rules! check {
        ($scheme:ident, $digest:ty) => {{
            let signature = rsa::$scheme::Signature::try_from(signature).ok()?;
            rsa::$scheme::VerifyingKey::<$digest>::new(key)
                .verify(message, &signature)
                .ok()
        }};
    }
    match (kind, hash) {
        (Kind::RsaPkcs1, HashKind::Sha256) => check!(pkcs1v15, Sha256),
        (Kind::RsaPkcs1, HashKind::Sha384) => check!(pkcs1v15, Sha384),
        (Kind::RsaPkcs1, HashKind::Sha512) => check!(pkcs1v15, Sha512),
        (Kind::RsaPss, HashKind::Sha256) => check!(pss, Sha256),
        (Kind::RsaPss, HashKind::Sha384) => check!(pss, Sha384),
        (Kind::RsaPss, HashKind::Sha512) => check!(pss, Sha512),
        _ => None,
    }
}

impl SignatureVerificationAlgorithm for Algorithm {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let prehash = || -> ([u8; 64], usize) {
            let mut out = [0u8; 64];
            let len = match self.hash {
                HashKind::Sha256 => {
                    out[..32].copy_from_slice(&Sha256::digest(message));
                    32
                }
                HashKind::Sha384 => {
                    out[..48].copy_from_slice(&Sha384::digest(message));
                    48
                }
                HashKind::Sha512 => {
                    out.copy_from_slice(&Sha512::digest(message));
                    64
                }
            };
            (out, len)
        };
        let verified = match self.kind {
            Kind::EcdsaP256 => {
                let (digest, len) = prehash();
                ecdsa_p256(&digest[..len], public_key, signature)
            }
            Kind::EcdsaP384 => {
                let (digest, len) = prehash();
                ecdsa_p384(&digest[..len], public_key, signature)
            }
            Kind::Ed25519 => ed25519(message, public_key, signature),
            Kind::RsaPkcs1 | Kind::RsaPss => {
                rsa(self.kind, self.hash, message, public_key, signature)
            }
        };
        verified.ok_or(InvalidSignature)
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        self.public_key
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.signature
    }
}

const fn algorithm(
    kind: Kind,
    hash: HashKind,
    public_key: AlgorithmIdentifier,
    signature: AlgorithmIdentifier,
) -> Algorithm {
    Algorithm {
        kind,
        hash,
        public_key,
        signature,
    }
}

static ECDSA_P256_SHA256: Algorithm = algorithm(
    Kind::EcdsaP256,
    HashKind::Sha256,
    alg_id::ECDSA_P256,
    alg_id::ECDSA_SHA256,
);
static ECDSA_P256_SHA384: Algorithm = algorithm(
    Kind::EcdsaP256,
    HashKind::Sha384,
    alg_id::ECDSA_P256,
    alg_id::ECDSA_SHA384,
);
static ECDSA_P384_SHA256: Algorithm = algorithm(
    Kind::EcdsaP384,
    HashKind::Sha256,
    alg_id::ECDSA_P384,
    alg_id::ECDSA_SHA256,
);
static ECDSA_P384_SHA384: Algorithm = algorithm(
    Kind::EcdsaP384,
    HashKind::Sha384,
    alg_id::ECDSA_P384,
    alg_id::ECDSA_SHA384,
);
static ED25519: Algorithm = algorithm(
    Kind::Ed25519,
    HashKind::Sha512,
    alg_id::ED25519,
    alg_id::ED25519,
);
static RSA_PKCS1_SHA256: Algorithm = algorithm(
    Kind::RsaPkcs1,
    HashKind::Sha256,
    alg_id::RSA_ENCRYPTION,
    alg_id::RSA_PKCS1_SHA256,
);
static RSA_PKCS1_SHA384: Algorithm = algorithm(
    Kind::RsaPkcs1,
    HashKind::Sha384,
    alg_id::RSA_ENCRYPTION,
    alg_id::RSA_PKCS1_SHA384,
);
static RSA_PKCS1_SHA512: Algorithm = algorithm(
    Kind::RsaPkcs1,
    HashKind::Sha512,
    alg_id::RSA_ENCRYPTION,
    alg_id::RSA_PKCS1_SHA512,
);
static RSA_PSS_SHA256: Algorithm = algorithm(
    Kind::RsaPss,
    HashKind::Sha256,
    alg_id::RSA_ENCRYPTION,
    alg_id::RSA_PSS_SHA256,
);
static RSA_PSS_SHA384: Algorithm = algorithm(
    Kind::RsaPss,
    HashKind::Sha384,
    alg_id::RSA_ENCRYPTION,
    alg_id::RSA_PSS_SHA384,
);
static RSA_PSS_SHA512: Algorithm = algorithm(
    Kind::RsaPss,
    HashKind::Sha512,
    alg_id::RSA_ENCRYPTION,
    alg_id::RSA_PSS_SHA512,
);

// Some certificates omit the NULL parameters of the PKCS#1 signature
// algorithms; these are the DER contents without them.
static RSA_PKCS1_SHA256_ABSENT_PARAMS: Algorithm = algorithm(
    Kind::RsaPkcs1,
    HashKind::Sha256,
    alg_id::RSA_ENCRYPTION,
    AlgorithmIdentifier::from_slice(&[
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b,
    ]),
);
static RSA_PKCS1_SHA384_ABSENT_PARAMS: Algorithm = algorithm(
    Kind::RsaPkcs1,
    HashKind::Sha384,
    alg_id::RSA_ENCRYPTION,
    AlgorithmIdentifier::from_slice(&[
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c,
    ]),
);
static RSA_PKCS1_SHA512_ABSENT_PARAMS: Algorithm = algorithm(
    Kind::RsaPkcs1,
    HashKind::Sha512,
    alg_id::RSA_ENCRYPTION,
    AlgorithmIdentifier::from_slice(&[
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d,
    ]),
);

pub static ALGORITHMS: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        &ECDSA_P256_SHA256,
        &ECDSA_P256_SHA384,
        &ECDSA_P384_SHA256,
        &ECDSA_P384_SHA384,
        &ED25519,
        &RSA_PSS_SHA256,
        &RSA_PSS_SHA384,
        &RSA_PSS_SHA512,
        &RSA_PKCS1_SHA256,
        &RSA_PKCS1_SHA384,
        &RSA_PKCS1_SHA512,
        &RSA_PKCS1_SHA256_ABSENT_PARAMS,
        &RSA_PKCS1_SHA384_ABSENT_PARAMS,
        &RSA_PKCS1_SHA512_ABSENT_PARAMS,
    ],
    // TLS 1.3 fixes the curve per scheme; TLS 1.2 does not.
    mapping: &[
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[&ECDSA_P384_SHA384, &ECDSA_P256_SHA384],
        ),
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[&ECDSA_P256_SHA256, &ECDSA_P384_SHA256],
        ),
        (SignatureScheme::ED25519, &[&ED25519]),
        (SignatureScheme::RSA_PSS_SHA512, &[&RSA_PSS_SHA512]),
        (SignatureScheme::RSA_PSS_SHA384, &[&RSA_PSS_SHA384]),
        (SignatureScheme::RSA_PSS_SHA256, &[&RSA_PSS_SHA256]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[&RSA_PKCS1_SHA512]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[&RSA_PKCS1_SHA384]),
        (SignatureScheme::RSA_PKCS1_SHA256, &[&RSA_PKCS1_SHA256]),
    ],
};
