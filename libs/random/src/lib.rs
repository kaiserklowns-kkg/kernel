//! Randomness for Oceans (ADR-0026), without allocation.
//!
//! - [`blake2s`]: BLAKE2s-256 (RFC 7693). The entropy pool hashes
//!   everything it is given, so any amount of weak or partly predictable
//!   input can be mixed in without harm.
//! - [`Rng`]: a ChaCha20 (RFC 8439) generator with fast key erasure. After
//!   every request the key is replaced by fresh output, so a later
//!   compromise of the state does not reveal earlier outputs. Reseeding
//!   hashes the old key with new entropy.

#![no_std]

const IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

fn compress(h: &mut [u32; 8], block: &[u8; 64], bytes: u64, last: bool) {
    let mut m = [0u32; 16];
    for (word, chunk) in m.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*chunk);
    }
    let mut v = [0u32; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(&IV);
    v[12] ^= bytes as u32;
    v[13] ^= (bytes >> 32) as u32;
    if last {
        v[14] = !v[14];
    }
    let g = |v: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32| {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(12);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(8);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(7);
    };
    for s in &SIGMA {
        g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

/// BLAKE2s-256 of the concatenation of `parts`.
pub fn blake2s(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = IV;
    h[0] ^= 0x0101_0000 ^ 32;
    let mut block = [0u8; 64];
    let mut filled = 0;
    let mut total: u64 = 0;
    for part in parts {
        for &byte in *part {
            // Compress a full block only once more input follows: the last
            // block is compressed with the final flag below.
            if filled == 64 {
                total += 64;
                compress(&mut h, &block, total, false);
                filled = 0;
            }
            block[filled] = byte;
            filled += 1;
        }
    }
    total += filled as u64;
    block[filled..].fill(0);
    compress(&mut h, &block, total, true);
    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One ChaCha20 block (RFC 8439 §2.3).
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let word = |bytes: &[u8], i: usize| {
        u32::from_le_bytes([
            bytes[4 * i],
            bytes[4 * i + 1],
            bytes[4 * i + 2],
            bytes[4 * i + 3],
        ])
    };
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for i in 0..8 {
        state[4 + i] = word(key, i);
    }
    state[12] = counter;
    for i in 0..3 {
        state[13 + i] = word(nonce, i);
    }
    let mut working = state;
    for _ in 0..10 {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 8, 13);
        quarter_round(&mut working, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for (i, chunk) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        chunk.copy_from_slice(&working[i].wrapping_add(state[i]).to_le_bytes());
    }
    out
}

/// A cryptographically secure generator: ChaCha20 with fast key erasure.
pub struct Rng {
    key: [u8; 32],
    /// Requests served since the last reseed (distinct nonces).
    requests: u64,
    seeded: bool,
}

impl Rng {
    pub const fn new() -> Self {
        Self {
            key: [0; 32],
            requests: 0,
            seeded: false,
        }
    }

    /// Whether any entropy has been mixed in yet.
    pub fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Mixes `entropy` into the key (old key and new input, hashed).
    pub fn reseed(&mut self, entropy: &[u8]) {
        self.key = blake2s(&[b"oceans rng reseed", &self.key, entropy]);
        self.requests = 0;
        self.seeded = true;
    }

    /// Fills `out` with random bytes, then replaces the key.
    pub fn fill(&mut self, out: &mut [u8]) {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.requests.to_le_bytes());
        self.requests = self.requests.wrapping_add(1);
        // Block 0 becomes the next key; the output starts at block 1.
        let next = chacha20_block(&self.key, 0, &nonce);
        for (index, chunk) in out.chunks_mut(64).enumerate() {
            let block = chacha20_block(&self.key, index as u32 + 1, &nonce);
            chunk.copy_from_slice(&block[..chunk.len()]);
        }
        self.key.copy_from_slice(&next[..32]);
    }
}

impl Default for Rng {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn blake2s_matches_reference_vectors() {
        assert_eq!(
            blake2s(&[b"abc"]),
            hex("508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982")
        );
        assert_eq!(
            blake2s(&[]),
            hex("69217a3079908094e11121d042354a7c1f55b6482ca1a51e1b250dfd1ed0eef9")
        );
        // Split input hashes like the whole.
        assert_eq!(blake2s(&[b"a", b"", b"bc"]), blake2s(&[b"abc"]));
        // Exactly one block, and one byte more, take different paths.
        let block = [7u8; 64];
        let longer = [7u8; 65];
        assert_ne!(blake2s(&[&block]), blake2s(&[&longer]));
        assert_eq!(
            blake2s(&[&longer[..64], &longer[64..]]),
            blake2s(&[&longer])
        );
    }

    #[test]
    fn chacha20_matches_rfc_8439() {
        // §2.1.1: the quarter round.
        let mut s = [0u32; 16];
        s[..4].copy_from_slice(&[0x1111_1111, 0x0102_0304, 0x9b8d_6f43, 0x0123_4567]);
        quarter_round(&mut s, 0, 1, 2, 3);
        assert_eq!(
            &s[..4],
            &[0xea2a_92f4, 0xcb1c_f8ce, 0x4581_472e, 0x5881_c4bb]
        );
        // §2.3.2: the block function.
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce = [0, 0, 0, 9, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let block = chacha20_block(&key, 1, &nonce);
        assert_eq!(
            &block[..16],
            &[
                0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20,
                0x71, 0xc4
            ]
        );
    }

    #[test]
    fn the_generator_erases_its_key_and_depends_on_its_seed() {
        let mut a = Rng::new();
        assert!(!a.is_seeded());
        a.reseed(b"seed one");
        assert!(a.is_seeded());
        let mut b = Rng::new();
        b.reseed(b"seed one");
        let mut c = Rng::new();
        c.reseed(b"seed two");

        let (mut x, mut y, mut z) = ([0u8; 100], [0u8; 100], [0u8; 100]);
        a.fill(&mut x);
        b.fill(&mut y);
        c.fill(&mut z);
        assert_eq!(x, y, "deterministic for a given seed");
        assert_ne!(x, z, "the seed matters");

        // The key changes after every request: outputs never repeat.
        let mut again = [0u8; 100];
        a.fill(&mut again);
        assert_ne!(x, again);
        let key_before = a.key;
        a.fill(&mut [0u8; 1]);
        assert_ne!(a.key, key_before, "fast key erasure");

        // Reseeding keeps what was there and adds the new input.
        let mut d = Rng::new();
        d.reseed(b"seed one");
        d.reseed(b"more");
        let mut e = Rng::new();
        e.reseed(b"more");
        let (mut p, mut q) = ([0u8; 32], [0u8; 32]);
        d.fill(&mut p);
        e.fill(&mut q);
        assert_ne!(p, q);

        // A rough uniformity check over many bytes.
        let mut counts = [0u32; 256];
        let mut buffer = [0u8; 4096];
        for _ in 0..64 {
            a.fill(&mut buffer);
            for &byte in &buffer {
                counts[byte as usize] += 1;
            }
        }
        let expected = 64 * 4096 / 256;
        assert!(
            counts
                .iter()
                .all(|&c| c > expected * 3 / 4 && c < expected * 5 / 4)
        );
    }
}
