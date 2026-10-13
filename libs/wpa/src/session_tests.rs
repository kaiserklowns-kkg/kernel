use crate::ccmp;
use crate::{Keys, Session};

const AP: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x01];
const US: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
const TK: [u8; 16] = [7; 16];
const GTK: [u8; 16] = [9; 16];

/// A data frame: ToDS from us (`up`), else FromDS from the access point
/// to `to`; LLC/SNAP and an IPv4 payload.
fn frame(up: bool, to: [u8; 6]) -> Vec<u8> {
    let mut f = vec![0x08, if up { 0x01 } else { 0x02 }, 0, 0];
    if up {
        f.extend_from_slice(&AP);
        f.extend_from_slice(&US);
        f.extend_from_slice(&to);
    } else {
        f.extend_from_slice(&to);
        f.extend_from_slice(&AP);
        f.extend_from_slice(&[0x02, 9, 9, 9, 9, 9]);
    }
    f.extend_from_slice(&[0x10, 0]);
    f.extend_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 8, 0]);
    f.extend_from_slice(b"an IPv4 packet");
    f
}

fn session() -> Session {
    let mut s = Session::default();
    s.install(Keys {
        pairwise: Some(TK),
        group: Some((1, GTK)),
    });
    s
}

#[test]
fn unicast_both_ways() {
    let mut ours = session();
    let mut theirs = session();
    assert!(ours.protected());
    assert!(!Session::default().protected());

    let plain = frame(true, [0x02, 9, 9, 9, 9, 9]);
    let mut air = [0u8; 256];
    let len = ours.protect(&plain, 24, &mut air).unwrap();
    assert_eq!(len, plain.len() + ccmp::HEADER + ccmp::MIC);
    assert_eq!(air[1], 0x41);
    assert_eq!(ccmp::read_header(&air[24..32]), Some((1, 0)));
    assert_ne!(&air[32..40], &plain[24..32], "the body is encrypted");

    let mut back = [0u8; 256];
    let got = theirs.unprotect(&air[..len], 24, &mut back).unwrap();
    assert_eq!(&back[..got], &plain[..]);
    // The same frame again: a replay.
    assert_eq!(theirs.unprotect(&air[..len], 24, &mut back), None);
    // The next one passes; one with a changed bit does not.
    let len2 = ours.protect(&plain, 24, &mut air).unwrap();
    let mut forged = air[..len2].to_vec();
    forged[40] ^= 1;
    assert_eq!(theirs.unprotect(&forged, 24, &mut back), None);
    assert!(theirs.unprotect(&air[..len2], 24, &mut back).is_some());
    // A frame in the clear is not "unprotected".
    assert_eq!(theirs.unprotect(&plain, 24, &mut back), None);

    // The key in use installed again: its packet numbers go on (2 so far),
    // never back to 1 (KRACK).
    ours.install(Keys {
        pairwise: Some(TK),
        group: None,
    });
    ours.protect(&plain, 24, &mut air).unwrap();
    assert_eq!(ccmp::read_header(&air[24..32]), Some((3, 0)));
    // A new key starts them afresh.
    ours.install(Keys {
        pairwise: Some([8; 16]),
        group: None,
    });
    ours.protect(&plain, 24, &mut air).unwrap();
    assert_eq!(ccmp::read_header(&air[24..32]), Some((1, 0)));
}

#[test]
fn group_frames_and_rekeys() {
    let mut ours = session();
    // A broadcast from the access point, under the group key (ID 1).
    let plain = frame(false, [0xff; 6]);
    let mut air = plain[..24].to_vec();
    air[1] |= 0x40;
    air.extend_from_slice(&ccmp::ccmp_header(5, 1));
    let mut body = plain[24..].to_vec();
    body.extend_from_slice(&[0; ccmp::MIC]);
    let n = plain.len() - 24;
    ccmp::seal(&GTK, &ccmp::Header { bytes: &air[..24] }, 5, &mut body, n).unwrap();
    air.extend_from_slice(&body);

    let mut back = [0u8; 256];
    let got = ours.unprotect(&air, 24, &mut back).unwrap();
    assert_eq!(&back[..got], &plain[..]);
    assert_eq!(ours.unprotect(&air, 24, &mut back), None, "replayed");

    // Under another key ID: refused until that key is installed.
    let mut other = air.clone();
    other[24 + 3] = 2 << 6 | 0x20;
    assert_eq!(ours.unprotect(&other, 24, &mut back), None);
    // A group rekey to the same key keeps its counter; a new one resets it.
    ours.install(Keys {
        pairwise: None,
        group: Some((1, GTK)),
    });
    assert_eq!(ours.unprotect(&air, 24, &mut back), None);
    ours.install(Keys {
        pairwise: None,
        group: Some((2, [3; 16])),
    });
    assert!(ours.protected(), "the pairwise key stays");
    assert_eq!(ours.unprotect(&air, 24, &mut back), None, "old key gone");
}
