//! Ephemeral key exchange: X25519, P-256 and P-384.

use alloc::boxed::Box;
use alloc::vec::Vec;

use rustls::crypto::{ActiveKeyExchange, SecureRandom, SharedSecret, SupportedKxGroup};
use rustls::{Error, NamedGroup, PeerMisbehaved};
use zeroize::Zeroizing;

fn bad_share() -> Error {
    PeerMisbehaved::InvalidKeyShare.into()
}

#[derive(Debug)]
pub struct X25519(pub &'static dyn SecureRandom);

impl SupportedKxGroup for X25519 {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let mut seed = Zeroizing::new([0u8; 32]);
        self.0.fill(&mut *seed)?;
        let secret = x25519_dalek::StaticSecret::from(*seed);
        let public = x25519_dalek::PublicKey::from(&secret).to_bytes();
        Ok(Box::new(X25519Exchange { secret, public }))
    }

    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

struct X25519Exchange {
    secret: x25519_dalek::StaticSecret,
    public: [u8; 32],
}

impl ActiveKeyExchange for X25519Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        let peer: [u8; 32] = peer.try_into().map_err(|_| bad_share())?;
        let shared = self
            .secret
            .diffie_hellman(&x25519_dalek::PublicKey::from(peer));
        // A low-order peer key gives an all-zero secret (RFC 7748 §6.1).
        if !shared.was_contributory() {
            return Err(bad_share());
        }
        Ok(SharedSecret::from(&shared.as_bytes()[..]))
    }

    fn pub_key(&self) -> &[u8] {
        &self.public
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

/// NIST curves: keys are uncompressed points (RFC 8446 §4.2.8.2).
macro_rules! nist {
    ($group:ident, $exchange:ident, $curve:ident, $name:expr, $scalar:expr) => {
        #[derive(Debug)]
        pub struct $group(pub &'static dyn SecureRandom);

        impl SupportedKxGroup for $group {
            fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
                use $curve::elliptic_curve::sec1::ToEncodedPoint;
                // Rejection sampling: a random string is a valid scalar
                // with overwhelming probability.
                let mut bytes = Zeroizing::new([0u8; $scalar]);
                let secret = loop {
                    self.0.fill(&mut *bytes)?;
                    if let Ok(key) = $curve::SecretKey::from_slice(&*bytes) {
                        break key;
                    }
                };
                let public = secret
                    .public_key()
                    .to_encoded_point(false)
                    .as_bytes()
                    .to_vec();
                Ok(Box::new($exchange { secret, public }))
            }

            fn name(&self) -> NamedGroup {
                $name
            }
        }

        struct $exchange {
            secret: $curve::SecretKey,
            public: Vec<u8>,
        }

        impl ActiveKeyExchange for $exchange {
            fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
                // Uncompressed points only: 0x04, then x and y.
                if peer.len() != 1 + 2 * $scalar || peer[0] != 4 {
                    return Err(bad_share());
                }
                let peer = $curve::PublicKey::from_sec1_bytes(peer).map_err(|_| bad_share())?;
                let shared =
                    $curve::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), peer.as_affine());
                Ok(SharedSecret::from(&shared.raw_secret_bytes()[..]))
            }

            fn pub_key(&self) -> &[u8] {
                &self.public
            }

            fn group(&self) -> NamedGroup {
                $name
            }
        }
    };
}

nist!(P256, P256Exchange, p256, NamedGroup::secp256r1, 32);
nist!(P384, P384Exchange, p384, NamedGroup::secp384r1, 48);
