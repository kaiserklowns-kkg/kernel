//! TLS for Oceans (ADR-0031): [rustls] with a crypto provider built from
//! pure-Rust (RustCrypto) primitives, and a blocking [`Client`] over any
//! byte [`Transport`]. `no_std` with `alloc`; host-tested.
//!
//! Supported: TLS 1.3 and 1.2; AES-128/256-GCM and ChaCha20-Poly1305;
//! X25519, P-256 and P-384 key exchange; ECDSA, Ed25519, RSA PKCS#1 and
//! RSA-PSS certificates; Mozilla's root store ([`web_roots`]).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod aead;
mod client;
mod hash;
mod kx;
#[cfg(any(test, feature = "server"))]
mod sign;
mod verify;

#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use rustls::crypto::tls12::PrfUsingHmac;
use rustls::crypto::tls13::HkdfUsingHmac;
use rustls::crypto::{
    CipherSuiteCommon, CryptoProvider, KeyExchangeAlgorithm, KeyProvider, SecureRandom,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::SigningKey;
use rustls::time_provider::TimeProvider;
use rustls::{
    CipherSuite, ClientConfig, Error, RootCertStore, SignatureScheme, SupportedCipherSuite,
    Tls12CipherSuite, Tls13CipherSuite,
};

pub use client::{Client, Error as ClientError, Transport};
pub use rustls;

/// The provider: every suite, group and signature algorithm above, with
/// randomness from `random`. Build it once per process (it allocates its
/// key exchange groups for the process's lifetime).
pub fn provider(random: &'static dyn SecureRandom) -> CryptoProvider {
    let leak = |group: Box<dyn rustls::crypto::SupportedKxGroup>| -> &'static dyn rustls::crypto::SupportedKxGroup {
        Box::leak(group)
    };
    CryptoProvider {
        cipher_suites: CIPHER_SUITES.to_vec(),
        kx_groups: vec![
            leak(Box::new(kx::X25519(random))),
            leak(Box::new(kx::P256(random))),
            leak(Box::new(kx::P384(random))),
        ],
        signature_verification_algorithms: verify::ALGORITHMS,
        secure_random: random,
        key_provider: &Keys,
    }
}

/// Mozilla's trusted roots (the `webpki-roots` crate).
pub fn web_roots() -> RootCertStore {
    RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

/// A client configuration: TLS 1.3 and 1.2, certificates checked against
/// `roots` at the time `time` gives, no client certificate.
pub fn client_config(
    provider: Arc<CryptoProvider>,
    time: Arc<dyn TimeProvider>,
    roots: RootCertStore,
) -> Result<ClientConfig, Error> {
    Ok(ClientConfig::builder_with_details(provider, time)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// Certificates from PEM text (every `CERTIFICATE` block) or one DER
/// certificate.
pub fn parse_certificates(bytes: &[u8]) -> Vec<CertificateDer<'static>> {
    if bytes.starts_with(b"-----") || bytes.windows(11).any(|w| w == b"-----BEGIN ") {
        CertificateDer::pem_slice_iter(bytes)
            .filter_map(Result::ok)
            .collect()
    } else {
        vec![CertificateDer::from(bytes.to_vec())]
    }
}

#[derive(Debug)]
struct Keys;

impl KeyProvider for Keys {
    #[cfg(any(test, feature = "server"))]
    fn load_private_key(&self, key: PrivateKeyDer<'static>) -> Result<Arc<dyn SigningKey>, Error> {
        sign::load(&key)
    }

    #[cfg(not(any(test, feature = "server")))]
    fn load_private_key(&self, _key: PrivateKeyDer<'static>) -> Result<Arc<dyn SigningKey>, Error> {
        Err(Error::General("this build has no signing keys".into()))
    }
}

// ---- Cipher suites ---------------------------------------------------------

static TLS12_ECDSA: &[SignatureScheme] = &[
    SignatureScheme::ED25519,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ECDSA_NISTP256_SHA256,
];

static TLS12_RSA: &[SignatureScheme] = &[
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA256,
];

/// AES-GCM may protect 2^24.5 full records per key (RFC 8446 §5.5);
/// rustls rekeys (1.3) or closes (1.2) before.
const GCM_LIMIT: u64 = 1 << 24;

const fn common(suite: CipherSuite, sha384: bool, limit: u64) -> CipherSuiteCommon {
    CipherSuiteCommon {
        suite,
        hash_provider: if sha384 { &hash::SHA384 } else { &hash::SHA256 },
        confidentiality_limit: limit,
    }
}

static HKDF_SHA256: HkdfUsingHmac<'static> = HkdfUsingHmac(&hash::HMAC_SHA256);
static HKDF_SHA384: HkdfUsingHmac<'static> = HkdfUsingHmac(&hash::HMAC_SHA384);
static PRF_SHA256: PrfUsingHmac<'static> = PrfUsingHmac(&hash::HMAC_SHA256);
static PRF_SHA384: PrfUsingHmac<'static> = PrfUsingHmac(&hash::HMAC_SHA384);

static TLS13_AES_128_GCM_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: common(CipherSuite::TLS13_AES_128_GCM_SHA256, false, GCM_LIMIT),
    hkdf_provider: &HKDF_SHA256,
    aead_alg: &aead::AES_128_GCM,
    quic: None,
};

static TLS13_AES_256_GCM_SHA384: Tls13CipherSuite = Tls13CipherSuite {
    common: common(CipherSuite::TLS13_AES_256_GCM_SHA384, true, GCM_LIMIT),
    hkdf_provider: &HKDF_SHA384,
    aead_alg: &aead::AES_256_GCM,
    quic: None,
};

static TLS13_CHACHA20_POLY1305_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: common(CipherSuite::TLS13_CHACHA20_POLY1305_SHA256, false, u64::MAX),
    hkdf_provider: &HKDF_SHA256,
    aead_alg: &aead::CHACHA20_POLY1305,
    quic: None,
};

static TLS12_AES_128_GCM: aead::Tls12Gcm<aes_gcm::Aes128Gcm> = aead::Tls12Gcm(&aead::AES_128_GCM);
static TLS12_AES_256_GCM: aead::Tls12Gcm<aes_gcm::Aes256Gcm> = aead::Tls12Gcm(&aead::AES_256_GCM);

macro_rules! tls12 {
    ($name:ident, $suite:ident, $sign:expr, $aead:expr, $sha384:expr, $limit:expr) => {
        static $name: Tls12CipherSuite = Tls12CipherSuite {
            common: common(CipherSuite::$suite, $sha384, $limit),
            kx: KeyExchangeAlgorithm::ECDHE,
            sign: $sign,
            aead_alg: $aead,
            prf_provider: if $sha384 { &PRF_SHA384 } else { &PRF_SHA256 },
        };
    };
}

tls12!(
    ECDSA_AES_256_GCM,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    TLS12_ECDSA,
    &TLS12_AES_256_GCM,
    true,
    GCM_LIMIT
);
tls12!(
    ECDSA_AES_128_GCM,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    TLS12_ECDSA,
    &TLS12_AES_128_GCM,
    false,
    GCM_LIMIT
);
tls12!(
    ECDSA_CHACHA20,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS12_ECDSA,
    &aead::Tls12ChaCha,
    false,
    u64::MAX
);
tls12!(
    RSA_AES_256_GCM,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    TLS12_RSA,
    &TLS12_AES_256_GCM,
    true,
    GCM_LIMIT
);
tls12!(
    RSA_AES_128_GCM,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    TLS12_RSA,
    &TLS12_AES_128_GCM,
    false,
    GCM_LIMIT
);
tls12!(
    RSA_CHACHA20,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS12_RSA,
    &aead::Tls12ChaCha,
    false,
    u64::MAX
);

/// In preference order: TLS 1.3 first; ChaCha20 before AES, which is
/// several times slower in (constant-time, bitsliced) software.
static CIPHER_SUITES: &[SupportedCipherSuite] = &[
    SupportedCipherSuite::Tls13(&TLS13_CHACHA20_POLY1305_SHA256),
    SupportedCipherSuite::Tls13(&TLS13_AES_256_GCM_SHA384),
    SupportedCipherSuite::Tls13(&TLS13_AES_128_GCM_SHA256),
    SupportedCipherSuite::Tls12(&ECDSA_CHACHA20),
    SupportedCipherSuite::Tls12(&ECDSA_AES_256_GCM),
    SupportedCipherSuite::Tls12(&ECDSA_AES_128_GCM),
    SupportedCipherSuite::Tls12(&RSA_CHACHA20),
    SupportedCipherSuite::Tls12(&RSA_AES_256_GCM),
    SupportedCipherSuite::Tls12(&RSA_AES_128_GCM),
];
