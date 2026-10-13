use super::state::*;
use super::{Bss, Security};

fn bss(ssid: &[u8], security: Security, rssi: i8, channel: u8) -> Bss {
    let mut b = Bss {
        bssid: [2, 0, 0, 0, 0, 1],
        ssid: [0; 32],
        ssid_len: ssid.len() as u8,
        channel,
        security,
        beacon_interval: 100,
        capability: 1,
        rssi,
        tsf: 0,
        dtim_count: 0,
        dtim_period: 1,
        basic_rate: 2,
    };
    b.ssid[..ssid.len()].copy_from_slice(ssid);
    b
}

#[test]
fn network_lines_both_ways() {
    let mut out = [0u8; MAX_LINE];
    let len = network_line(&bss(b"oceans", Security::Wpa2Personal, -48, 6), &mut out).unwrap();
    assert_eq!(&out[..len], b"W -48 6 6f6365616e73\n");
    let listed = parse_network(&out[..len - 1]).unwrap();
    assert_eq!(listed.ssid.as_bytes(), b"oceans");
    assert_eq!(
        (listed.security, listed.rssi, listed.channel),
        (Security::Wpa2Personal, -48, 6)
    );

    // Any bytes in a name: spaces, a newline, the full 32.
    let odd = b"cafe wifi\n\xff";
    let len = network_line(&bss(odd, Security::Open, -90, 149), &mut out).unwrap();
    assert_eq!(parse_network(&out[..len - 1]).unwrap().ssid.as_bytes(), odd);
    let long = [b'x'; 32];
    let len = network_line(&bss(&long, Security::Unsupported, -1, 1), &mut out).unwrap();
    let back = parse_network(&out[..len - 1]).unwrap();
    assert_eq!(
        (back.ssid.as_bytes(), back.security),
        (&long[..], Security::Unsupported)
    );
    // Too small a buffer: nothing written.
    assert_eq!(
        network_line(&bss(&long, Security::Open, -1, 1), &mut out[..20]),
        None
    );

    // A file of lines, with junk and Windows line endings.
    let file = b"W -40 1 61\r\nnonsense\nO -70 36 62\n\nX -1 1 63\nW -1 1 6\nW -1 1 \n";
    let listed: Vec<_> = lines(file).filter_map(parse_network).collect();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].ssid.as_bytes(), b"b");
    assert_eq!(parse_network(b"W -999 1 61"), None);
    assert_eq!(parse_network(b"W -40 1 61 extra"), None);
}

#[test]
fn status_lines_both_ways() {
    let ssid = Ssid::new(b"my net").unwrap();
    let cases = [
        Status::Off,
        Status::Idle,
        Status::Scanning,
        Status::Joining(ssid),
        Status::Joined {
            ssid,
            rssi: -52,
            channel: 11,
        },
        Status::Failed {
            ssid,
            reason: "no WPA2 handshake (a wrong password?)",
        },
    ];
    let mut out = [0u8; MAX_LINE];
    for status in cases {
        let len = status_line(&status, &mut out).unwrap();
        assert_eq!(out[len - 1], b'\n');
        assert_eq!(parse_status(&out[..len]), Some(status), "{status:?}");
    }
    let len = status_line(&cases[4], &mut out).unwrap();
    assert_eq!(&out[..len], b"joined 6d79206e6574 -52 11\n");
    // A reason is kept to one printable line.
    let failed = Status::Failed {
        ssid,
        reason: "bad\nline\u{7f}",
    };
    let len = status_line(&failed, &mut out).unwrap();
    assert_eq!(
        parse_status(&out[..len]),
        Some(Status::Failed {
            ssid,
            reason: "bad line "
        })
    );
    assert_eq!(parse_status(b"joined zz -1 1"), None);
    assert_eq!(parse_status(b"what"), None);
    assert_eq!(parse_status(b""), None);
    assert_eq!(Ssid::new(&[0; 33]), None);
}
