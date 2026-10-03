//! SHA-2 hashes and HMAC for rustls.

use alloc::boxed::Box;
use core::marker::PhantomData;

use hmac::{Mac, digest::KeyInit};
use rustls::crypto::hash::{self, HashAlgorithm};
use rustls::crypto::hmac as rustls_hmac;
use sha2::{Digest, Sha256, Sha384};

pub struct Hash<D>(PhantomData<fn() -> D>, HashAlgorithm);

pub static SHA256: Hash<Sha256> = Hash(PhantomData, HashAlgorithm::SHA256);
pub static SHA384: Hash<Sha384> = Hash(PhantomData, HashAlgorithm::SHA384);

impl<D: Digest + Clone + Send + Sync + 'static> hash::Hash for Hash<D> {
    fn start(&self) -> Box<dyn hash::Context> {
        Box::new(Context(D::new()))
    }

    fn hash(&self, data: &[u8]) -> hash::Output {
        hash::Output::new(&D::digest(data))
    }

    fn output_len(&self) -> usize {
        <D as Digest>::output_size()
    }

    fn algorithm(&self) -> HashAlgorithm {
        self.1
    }
}

struct Context<D>(D);

impl<D: Digest + Clone + Send + Sync + 'static> hash::Context for Context<D> {
    fn fork_finish(&self) -> hash::Output {
        hash::Output::new(&self.0.clone().finalize())
    }

    fn fork(&self) -> Box<dyn hash::Context> {
        Box::new(Context(self.0.clone()))
    }

    fn finish(self: Box<Self>) -> hash::Output {
        hash::Output::new(&self.0.finalize())
    }

    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}

macro_rules! hmac {
    ($name:ident, $key:ident, $digest:ty, $len:expr) => {
        pub struct $name;

        impl rustls_hmac::Hmac for $name {
            fn with_key(&self, key: &[u8]) -> Box<dyn rustls_hmac::Key> {
                Box::new($key(
                    <hmac::Hmac<$digest> as KeyInit>::new_from_slice(key)
                        .expect("HMAC takes keys of any length"),
                ))
            }

            fn hash_output_len(&self) -> usize {
                $len
            }
        }

        struct $key(hmac::Hmac<$digest>);

        impl rustls_hmac::Key for $key {
            fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> rustls_hmac::Tag {
                let mut mac = self.0.clone();
                mac.update(first);
                for part in middle {
                    mac.update(part);
                }
                mac.update(last);
                rustls_hmac::Tag::new(&mac.finalize().into_bytes())
            }

            fn tag_len(&self) -> usize {
                $len
            }
        }
    };
}

hmac!(HmacSha256, HmacSha256Key, Sha256, 32);
hmac!(HmacSha384, HmacSha384Key, Sha384, 48);

pub static HMAC_SHA256: HmacSha256 = HmacSha256;
pub static HMAC_SHA384: HmacSha384 = HmacSha384;
