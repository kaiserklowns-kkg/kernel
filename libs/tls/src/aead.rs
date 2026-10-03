//! Record protection: AES-GCM and ChaCha20-Poly1305 for TLS 1.3 and 1.2.

use alloc::boxed::Box;
use core::marker::PhantomData;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20poly1305::ChaCha20Poly1305;
use rustls::crypto::cipher::{
    AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv, KeyBlockShape, MessageDecrypter,
    MessageEncrypter, NONCE_LEN, Nonce, OutboundOpaqueMessage, OutboundPlainMessage,
    PrefixedPayload, Tls12AeadAlgorithm, Tls13AeadAlgorithm, UnsupportedOperationError,
    make_tls12_aad, make_tls13_aad,
};
use rustls::{ConnectionTrafficSecrets, ContentType, Error, ProtocolVersion};

/// Authentication tag length of every supported AEAD.
const TAG_LEN: usize = 16;
/// Largest plaintext of one record (RFC 8446 §5.1).
const MAX_FRAGMENT_LEN: usize = 16_384;
/// TLS 1.2 GCM: the explicit part of the nonce, sent with each record.
const EXPLICIT_NONCE_LEN: usize = 8;

/// The rustls name for a key and IV of this algorithm.
type Secrets = fn(AeadKey, Iv) -> ConnectionTrafficSecrets;

pub struct Aead<C> {
    key_len: usize,
    secrets: Secrets,
    cipher: PhantomData<fn() -> C>,
}

pub static AES_128_GCM: Aead<Aes128Gcm> = Aead {
    key_len: 16,
    secrets: |key, iv| ConnectionTrafficSecrets::Aes128Gcm { key, iv },
    cipher: PhantomData,
};

pub static AES_256_GCM: Aead<Aes256Gcm> = Aead {
    key_len: 32,
    secrets: |key, iv| ConnectionTrafficSecrets::Aes256Gcm { key, iv },
    cipher: PhantomData,
};

pub static CHACHA20_POLY1305: Aead<ChaCha20Poly1305> = Aead {
    key_len: 32,
    secrets: |key, iv| ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv },
    cipher: PhantomData,
};

/// TLS 1.2 uses GCM with a partly explicit nonce, ChaCha20-Poly1305 with
/// an implicit one (RFC 5288, RFC 7905).
pub struct Tls12Gcm<C: 'static>(pub &'static Aead<C>);
pub struct Tls12ChaCha;

trait Cipher: KeyInit + AeadInPlace + Send + Sync + 'static {}
impl<C: KeyInit + AeadInPlace + Send + Sync + 'static> Cipher for C {}

fn cipher<C: Cipher>(key: &AeadKey) -> C {
    // The key schedule produces keys of exactly `key_len` bytes.
    C::new_from_slice(key.as_ref()).expect("AEAD key of the suite's length")
}

fn seal<C: Cipher>(
    cipher: &C,
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    data: &mut [u8],
) -> Result<[u8; TAG_LEN], Error> {
    let tag = cipher
        .encrypt_in_place_detached(GenericArray::from_slice(nonce), aad, data)
        .map_err(|_| Error::EncryptError)?;
    let mut out = [0u8; TAG_LEN];
    out.copy_from_slice(&tag);
    Ok(out)
}

/// Opens `data` followed by its tag in place; returns the plaintext length.
fn open<C: Cipher>(
    cipher: &C,
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    data: &mut [u8],
) -> Result<usize, Error> {
    let plain_len = data.len().checked_sub(TAG_LEN).ok_or(Error::DecryptError)?;
    let (plain, tag) = data.split_at_mut(plain_len);
    cipher
        .decrypt_in_place_detached(
            GenericArray::from_slice(nonce),
            aad,
            plain,
            GenericArray::from_slice(tag),
        )
        .map_err(|_| Error::DecryptError)?;
    Ok(plain_len)
}

// ---- TLS 1.3 ---------------------------------------------------------------

impl<C: Cipher> Tls13AeadAlgorithm for Aead<C> {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Box::new(Tls13Encrypter {
            cipher: cipher::<C>(&key),
            iv,
        })
    }

    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Box::new(Tls13Decrypter {
            cipher: cipher::<C>(&key),
            iv,
        })
    }

    fn key_len(&self) -> usize {
        self.key_len
    }

    fn extract_keys(
        &self,
        key: AeadKey,
        iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok((self.secrets)(key, iv))
    }
}

struct Tls13Encrypter<C> {
    cipher: C,
    iv: Iv,
}

impl<C: Cipher> MessageEncrypter for Tls13Encrypter<C> {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total_len = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        payload.extend_from_chunks(&msg.payload);
        payload.extend_from_slice(&[u8::from(msg.typ)]);
        let nonce = Nonce::new(&self.iv, seq).0;
        let tag = seal(
            &self.cipher,
            &nonce,
            &make_tls13_aad(total_len),
            payload.as_mut(),
        )?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            payload,
        ))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + 1 + TAG_LEN
    }
}

struct Tls13Decrypter<C> {
    cipher: C,
    iv: Iv,
}

impl<C: Cipher> MessageDecrypter for Tls13Decrypter<C> {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls13_aad(payload.len());
        let plain_len = open(&self.cipher, &nonce, &aad, payload)?;
        payload.truncate(plain_len);
        msg.into_tls13_unpadded_message()
    }
}

// ---- TLS 1.2 ---------------------------------------------------------------

impl<C: Cipher> Tls12AeadAlgorithm for Tls12Gcm<C> {
    fn encrypter(
        &self,
        key: AeadKey,
        write_iv: &[u8],
        explicit: &[u8],
    ) -> Box<dyn MessageEncrypter> {
        Box::new(Tls12GcmEncrypter {
            cipher: cipher::<C>(&key),
            iv: gcm_iv(write_iv, explicit),
        })
    }

    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        let mut salt = [0u8; 4];
        salt.copy_from_slice(iv);
        Box::new(Tls12GcmDecrypter {
            cipher: cipher::<C>(&key),
            salt,
        })
    }

    fn key_block_shape(&self) -> KeyBlockShape {
        KeyBlockShape {
            enc_key_len: self.0.key_len,
            fixed_iv_len: 4,
            explicit_nonce_len: EXPLICIT_NONCE_LEN,
        }
    }

    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        explicit: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok((self.0.secrets)(key, gcm_iv(iv, explicit)))
    }
}

fn gcm_iv(write_iv: &[u8], explicit: &[u8]) -> Iv {
    let mut iv = [0u8; NONCE_LEN];
    iv[..4].copy_from_slice(write_iv);
    iv[4..].copy_from_slice(explicit);
    Iv::new(iv)
}

struct Tls12GcmEncrypter<C> {
    cipher: C,
    iv: Iv,
}

impl<C: Cipher> MessageEncrypter for Tls12GcmEncrypter<C> {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total_len = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_slice(&nonce[4..]);
        payload.extend_from_chunks(&msg.payload);
        let tag = seal(
            &self.cipher,
            &nonce,
            &aad,
            &mut payload.as_mut()[EXPLICIT_NONCE_LEN..],
        )?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + EXPLICIT_NONCE_LEN + TAG_LEN
    }
}

struct Tls12GcmDecrypter<C> {
    cipher: C,
    salt: [u8; 4],
}

impl<C: Cipher> MessageDecrypter for Tls12GcmDecrypter<C> {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        if payload.len() < EXPLICIT_NONCE_LEN + TAG_LEN {
            return Err(Error::DecryptError);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..4].copy_from_slice(&self.salt);
        nonce[4..].copy_from_slice(&payload[..EXPLICIT_NONCE_LEN]);
        let aad = make_tls12_aad(
            seq,
            msg.typ,
            msg.version,
            payload.len() - EXPLICIT_NONCE_LEN - TAG_LEN,
        );
        let plain_len = open(
            &self.cipher,
            &nonce,
            &aad,
            &mut payload[EXPLICIT_NONCE_LEN..],
        )?;
        if plain_len > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        payload.copy_within(EXPLICIT_NONCE_LEN..EXPLICIT_NONCE_LEN + plain_len, 0);
        payload.truncate(plain_len);
        Ok(msg.into_plain_message())
    }
}

impl Tls12AeadAlgorithm for Tls12ChaCha {
    fn encrypter(&self, key: AeadKey, iv: &[u8], _explicit: &[u8]) -> Box<dyn MessageEncrypter> {
        Box::new(Tls12ChaChaCrypter {
            cipher: cipher::<ChaCha20Poly1305>(&key),
            iv: Iv::copy(iv),
        })
    }

    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        Box::new(Tls12ChaChaCrypter {
            cipher: cipher::<ChaCha20Poly1305>(&key),
            iv: Iv::copy(iv),
        })
    }

    fn key_block_shape(&self) -> KeyBlockShape {
        KeyBlockShape {
            enc_key_len: 32,
            fixed_iv_len: NONCE_LEN,
            explicit_nonce_len: 0,
        }
    }

    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        _explicit: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(ConnectionTrafficSecrets::Chacha20Poly1305 {
            key,
            iv: Iv::copy(iv),
        })
    }
}

struct Tls12ChaChaCrypter {
    cipher: ChaCha20Poly1305,
    iv: Iv,
}

impl MessageEncrypter for Tls12ChaChaCrypter {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total_len = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total_len);
        let nonce = Nonce::new(&self.iv, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_chunks(&msg.payload);
        let tag = seal(&self.cipher, &nonce, &aad, payload.as_mut())?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }

    fn encrypted_payload_len(&self, payload_len: usize) -> usize {
        payload_len + TAG_LEN
    }
}

impl MessageDecrypter for Tls12ChaChaCrypter {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        let nonce = Nonce::new(&self.iv, seq).0;
        let plain_len = payload
            .len()
            .checked_sub(TAG_LEN)
            .ok_or(Error::DecryptError)?;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, plain_len);
        open(&self.cipher, &nonce, &aad, payload)?;
        if plain_len > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        payload.truncate(plain_len);
        Ok(msg.into_plain_message())
    }
}
