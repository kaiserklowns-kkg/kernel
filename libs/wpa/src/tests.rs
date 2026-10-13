use super::*;
use crate::eapol::{Key, info};

fn hex(text: &str) -> Vec<u8> {
    let text: String = text.split_whitespace().collect();
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn the_passphrase_key_of_the_standard() {
    // 802.11-2020 J.4.2: "password", SSID "IEEE".
    let key = psk("password", b"IEEE").unwrap();
    assert_eq!(
        key.to_vec(),
        hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
    );
    assert_eq!(psk("short", b"IEEE"), None);
    assert_eq!(psk(&"x".repeat(64), b"IEEE"), None);
    assert_eq!(psk("password\u{7f}", b"IEEE"), None);
    assert_eq!(psk("password", b""), None);
}

#[test]
fn the_prf_of_the_standard() {
    // 802.11-2020 J.3.2, test case 1: PRF-512.
    let mut out = [0u8; 64];
    prf(&[0x0b; 20], "prefix", b"Hi There", &mut out);
    assert_eq!(
        out.to_vec(),
        hex(
            "bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606e17d8da35402ffee\
             75df78c3d31e0f889f012120c0862beb67753e7439ae242edb8373698356cf5a"
        )
    );
}

#[test]
fn aes_key_wrap_of_rfc_3394() {
    // RFC 3394 §4.1.
    let kek: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
    let wrapped = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    let mut out = [0u8; 32];
    let len = eapol::unwrap(&kek, &wrapped, &mut out).unwrap();
    assert_eq!(out[..len].to_vec(), hex("00112233445566778899aabbccddeeff"));
    let mut bad = wrapped.clone();
    bad[5] ^= 1;
    assert_eq!(eapol::unwrap(&kek, &bad, &mut out), None);
}

#[test]
fn ccmp_of_the_standard() {
    // 802.11-2020 J.6.4 (CCMP-128).
    let tk: [u8; 16] = hex("c97c1f67ce371185514a8a19f2bdd52f").try_into().unwrap();
    let pn = 0xb503_9776_e70c;
    let mac = hex("08 48 c3 2c 0f d2 e1 28 a5 7c 50 30 f1 84 44 08 ab ae a5 b8 fc ba 80 33");
    let header = ccmp::Header { bytes: &mac };
    assert_eq!(
        header.nonce(pn).to_vec(),
        hex("00 50 30 f1 84 44 08 b5 03 97 76 e7 0c")
    );
    let mut aad = [0u8; 32];
    let len = header.aad(&mut aad);
    assert_eq!(
        aad[..len].to_vec(),
        hex("08 40 0f d2 e1 28 a5 7c 50 30 f1 84 44 08 ab ae a5 b8 fc ba 00 00")
    );
    assert_eq!(
        ccmp::ccmp_header(pn, 0).to_vec(),
        hex("0c e7 00 20 76 97 03 b5")
    );
    let plain = hex("f8 ba 1a 55 d0 2f 85 ae 96 7b b6 2f b6 cd a8 eb 7e 78 a0 50");
    let mut body = plain.clone();
    body.resize(plain.len() + ccmp::MIC, 0);
    let len = ccmp::seal(&tk, &header, pn, &mut body, plain.len()).unwrap();
    assert_eq!(
        body[..len].to_vec(),
        hex("f3 d0 a2 fe 9a 3d bf 23 42 a6 43 e4 32 46 e8 0c 3c 04 d0 19 78 45 ce 0b 16 f9 76 23")
    );
    assert_eq!(ccmp::read_header(&ccmp::ccmp_header(pn, 2)), Some((pn, 2)));
    let opened = ccmp::open(&tk, &header, pn, &mut body).unwrap();
    assert_eq!(body[..opened].to_vec(), plain);
    // Any change is caught.
    let mut forged =
        hex("f3 d0 a2 fe 9a 3d bf 23 42 a6 43 e4 32 46 e8 0c 3c 04 d0 19 78 45 ce 0b 16 f9 76 24");
    assert_eq!(ccmp::open(&tk, &header, pn, &mut forged), None);
    let mut replay = ccmp::Replay::default();
    assert!(replay.accept(5) && !replay.accept(5) && !replay.accept(4) && replay.accept(6));
}

/// The access point's side, for the test: messages 1 and 3, and a rekey.
struct AccessPoint {
    pmk: [u8; 32],
    aa: MacAddress,
    spa: MacAddress,
    anonce: [u8; 32],
    gtk: [u8; 16],
}

impl AccessPoint {
    fn message1(&self, replay: u64) -> Vec<u8> {
        let mut out = vec![0u8; 256];
        let info = info::VERSION_HMAC_SHA1_AES | info::PAIRWISE | info::ACK;
        let len = eapol::build(&mut out, info, replay, &self.anonce, &[]).unwrap();
        out.truncate(len);
        out
    }

    fn wrapped_gtk(&self, kek: &[u8; 16], key_id: u8, gtk: &[u8; 16]) -> Vec<u8> {
        // GTK KDE, padded to a multiple of 8 with dd 00...
        let mut plain = vec![0xdd, 22, 0x00, 0x0f, 0xac, 1, key_id, 0];
        plain.extend_from_slice(gtk);
        plain.push(0xdd);
        while plain.len() % 8 != 0 {
            plain.push(0);
        }
        let mut wrapped = vec![0u8; plain.len() + 8];
        aes_kw::KekAes128::from(*kek)
            .wrap(&plain, &mut wrapped)
            .unwrap();
        wrapped
    }

    fn message3(&self, ptk: &Ptk, replay: u64) -> Vec<u8> {
        let data = self.wrapped_gtk(&ptk.kek, 1, &self.gtk);
        let mut out = vec![0u8; 512];
        let info = info::VERSION_HMAC_SHA1_AES
            | info::PAIRWISE
            | info::INSTALL
            | info::ACK
            | info::MIC
            | info::SECURE
            | info::ENCRYPTED;
        let len = eapol::build(&mut out, info, replay, &self.anonce, &data).unwrap();
        out.truncate(len);
        eapol::sign(&ptk.kck, &mut out);
        out
    }
}

#[test]
fn a_whole_handshake_and_a_rekey() {
    let pmk = psk("correct horse battery", b"oceans").unwrap();
    let ap = AccessPoint {
        pmk,
        aa: [2, 0, 0, 0, 0, 1],
        spa: [2, 0, 0, 0, 0, 2],
        anonce: [0xa1; 32],
        gtk: [0x47; 16],
    };
    let snonce = [0x5e; 32];
    let mut client = Supplicant::new(pmk, ap.aa, ap.spa, snonce);
    let mut out = [0u8; 512];

    // Message 1 -> message 2, signed with the right key, our RSN element.
    let Action::Send(len) = client.receive(&ap.message1(1), &mut out) else {
        panic!("message 2");
    };
    let ptk = Ptk::derive(&ap.pmk, &ap.aa, &ap.spa, &ap.anonce, &snonce);
    assert!(eapol::verify(&ptk.kck, &out[..len]));
    let m2 = Key::parse(&out[..len]).unwrap();
    assert_eq!(
        (m2.nonce, m2.data, m2.replay),
        (snonce, &RSN_ELEMENT[..], 1)
    );

    // A message 3 with a wrong MIC, or another ANonce, is ignored.
    let mut forged = ap.message3(&ptk, 2);
    let last = forged.len() - 1;
    forged[last] ^= 1;
    assert_eq!(client.receive(&forged, &mut out), Action::Ignore);
    let other = AccessPoint {
        anonce: [0xb2; 32],
        ..ap
    };
    assert_eq!(
        client.receive(&other.message3(&ptk, 2), &mut out),
        Action::Ignore
    );
    let ap = AccessPoint {
        anonce: [0xa1; 32],
        ..other
    };

    // Message 3 -> message 4 and the keys.
    let Action::SendAndInstall(len, keys) = client.receive(&ap.message3(&ptk, 2), &mut out) else {
        panic!("message 4");
    };
    assert_eq!(keys.pairwise, Some(ptk.tk));
    assert_eq!(keys.group, Some((1, [0x47; 16])));
    assert!(eapol::verify(&ptk.kck, &out[..len]));
    let m4 = Key::parse(&out[..len]).unwrap();
    assert_eq!(m4.replay, 2);
    assert!(m4.info & info::SECURE != 0 && m4.data.is_empty());
    assert!(client.done());

    // The same message 3 again (a replay) is ignored.
    assert_eq!(
        client.receive(&ap.message3(&ptk, 2), &mut out),
        Action::Ignore
    );
    // Retransmitted with a newer counter (our message 4 lost): message 4
    // again, and no keys installed a second time (KRACK).
    let Action::Send(len) = client.receive(&ap.message3(&ptk, 3), &mut out) else {
        panic!("message 4 again, without keys");
    };
    assert_eq!(Key::parse(&out[..len]).unwrap().replay, 3);
    assert!(eapol::verify(&ptk.kck, &out[..len]));
    // A message 1 after the keys does not restart the handshake.
    assert_eq!(client.receive(&ap.message1(4), &mut out), Action::Ignore);

    // A group rekey.
    let data = ap.wrapped_gtk(&ptk.kek, 2, &[0x99; 16]);
    let mut rekey = vec![0u8; 512];
    let info = info::VERSION_HMAC_SHA1_AES | info::ACK | info::MIC | info::SECURE | info::ENCRYPTED;
    let len = eapol::build(&mut rekey, info, 5, &[0; 32], &data).unwrap();
    rekey.truncate(len);
    eapol::sign(&ptk.kck, &mut rekey);
    let Action::SendAndInstall(_, keys) = client.receive(&rekey, &mut out) else {
        panic!("group message 2");
    };
    assert_eq!(
        keys,
        Keys {
            pairwise: None,
            group: Some((2, [0x99; 16]))
        }
    );
}

#[test]
fn malformed_frames_are_ignored() {
    let mut client = Supplicant::new([0; 32], [0; 6], [1; 6], [0; 32]);
    let mut out = [0u8; 512];
    for frame in [
        &[][..],
        &[2, 3, 0, 0][..],
        &[0u8; 98][..],
        &[2, 3, 0xff, 0xff, 2][..],
    ] {
        assert_eq!(client.receive(frame, &mut out), Action::Ignore);
    }
    // A key data length past the frame.
    let mut frame = vec![0u8; 99];
    frame[1] = 3;
    frame[3] = 95;
    frame[4] = 2;
    frame[97] = 0x10;
    assert_eq!(Key::parse(&frame), None);
}
