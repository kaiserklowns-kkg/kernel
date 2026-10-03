//! SipHash-2-4 (Aumasson and Bernstein, 2012): a keyed pseudorandom
//! function, the `F()` of RFC 7217 stable IPv6 interface identifiers
//! (ADR-0043). 128-bit key, 64-bit output.

fn round(v: &mut [u64; 4]) {
    v[0] = v[0].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(13) ^ v[0];
    v[0] = v[0].rotate_left(32);
    v[2] = v[2].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(16) ^ v[2];
    v[0] = v[0].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(21) ^ v[0];
    v[2] = v[2].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(17) ^ v[2];
    v[2] = v[2].rotate_left(32);
}

fn compress(v: &mut [u64; 4], word: u64) {
    v[3] ^= word;
    round(v);
    round(v);
    v[0] ^= word;
}

/// SipHash-2-4 of `data` under `key`.
pub(crate) fn siphash24(key: &[u8; 16], data: &[u8]) -> u64 {
    let k0 = u64::from_le_bytes(key[..8].try_into().expect("8 bytes"));
    let k1 = u64::from_le_bytes(key[8..].try_into().expect("8 bytes"));
    let mut v = [
        k0 ^ 0x736f_6d65_7073_6575,
        k1 ^ 0x646f_7261_6e64_6f6d,
        k0 ^ 0x6c79_6765_6e65_7261,
        k1 ^ 0x7465_6462_7974_6573,
    ];
    let (words, rest) = data.as_chunks::<8>();
    for word in words {
        compress(&mut v, u64::from_le_bytes(*word));
    }
    let mut last = [0u8; 8];
    last[..rest.len()].copy_from_slice(rest);
    last[7] = data.len() as u8;
    compress(&mut v, u64::from_le_bytes(last));
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

#[cfg(test)]
mod tests {
    use super::siphash24;

    /// The reference vectors (the paper's appendix and `vectors.h`): key
    /// 00 01 .. 0f, messages 00 01 .. of each length.
    #[test]
    fn reference_vectors() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let message: [u8; 64] = core::array::from_fn(|i| i as u8);
        for (len, expected) in [
            (0, 0x726f_db47_dd0e_0e31),
            (1, 0x74f8_39c5_93dc_67fd),
            (7, 0xab02_00f5_8b01_d137),
            (8, 0x93f5_f579_9a93_2462),
            (15, 0xa129_ca61_49be_45e5),
            (63, 0x958a_324c_eb06_4572),
        ] {
            assert_eq!(siphash24(&key, &message[..len]), expected, "length {len}");
        }
    }
}
