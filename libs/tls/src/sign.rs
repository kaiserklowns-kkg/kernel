//! Server-side signing with ECDSA P-256 keys (feature `server`): enough for
//! test servers and tools; Oceans programs are clients.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use p256::ecdsa::signature::Signer as _;
use p256::pkcs8::DecodePrivateKey;
use rustls::pki_types::PrivateKeyDer;
use rustls::sign::{Signer, SigningKey};
use rustls::{Error, SignatureAlgorithm, SignatureScheme};

/// Loads a PKCS#8 P-256 key.
pub fn load(der: &PrivateKeyDer<'_>) -> Result<Arc<dyn SigningKey>, Error> {
    let PrivateKeyDer::Pkcs8(pkcs8) = der else {
        return Err(Error::General("only PKCS#8 keys are supported".into()));
    };
    let key = p256::ecdsa::SigningKey::from_pkcs8_der(pkcs8.secret_pkcs8_der())
        .map_err(|_| Error::General("not a P-256 PKCS#8 key".into()))?;
    Ok(Arc::new(EcdsaKey(Arc::new(key))))
}

struct EcdsaKey(Arc<p256::ecdsa::SigningKey>);

impl fmt::Debug for EcdsaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EcdsaKey(P-256)")
    }
}

impl SigningKey for EcdsaKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(EcdsaSigner(self.0.clone())) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

struct EcdsaSigner(Arc<p256::ecdsa::SigningKey>);

impl fmt::Debug for EcdsaSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EcdsaSigner(P-256)")
    }
}

impl Signer for EcdsaSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        // RFC 6979: deterministic nonces, no randomness needed.
        let signature: p256::ecdsa::Signature = self.0.sign(message);
        Ok(signature.to_der().as_bytes().to_vec())
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}
