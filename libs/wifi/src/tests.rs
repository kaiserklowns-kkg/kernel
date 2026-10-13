use super::*;

const AP: MacAddress = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x01];
const US: MacAddress = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

/// The RSN element of a WPA2-Personal network (its value).
const WPA2_RSN: [u8; 20] = [
    1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0x0c, 0,
];

fn beacon(ssid: &[u8], privacy: bool, rsn: Option<&[u8]>, channel: u8) -> Vec<u8> {
    let mut frame = vec![kind::BEACON, 0, 0, 0];
    frame.extend_from_slice(&BROADCAST);
    frame.extend_from_slice(&AP);
    frame.extend_from_slice(&AP);
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&[0; 8]);
    frame.extend_from_slice(&100u16.to_le_bytes());
    let capability: u16 = 0x0001 | if privacy { 0x0010 } else { 0 };
    frame.extend_from_slice(&capability.to_le_bytes());
    frame.extend_from_slice(&[0, ssid.len() as u8]);
    frame.extend_from_slice(ssid);
    frame.extend_from_slice(&[1, 4, 0x82, 0x84, 0x8b, 0x96]);
    frame.extend_from_slice(&[3, 1, channel]);
    if let Some(rsn) = rsn {
        frame.extend_from_slice(&[48, rsn.len() as u8]);
        frame.extend_from_slice(rsn);
    }
    frame
}

#[test]
fn networks_from_beacons() {
    let bss = Bss::parse(&beacon(b"oceans", true, Some(&WPA2_RSN), 6), 6, -40).unwrap();
    assert_eq!(bss.ssid(), b"oceans");
    assert_eq!((bss.bssid, bss.channel, bss.rssi), (AP, 6, -40));
    assert_eq!(bss.security, Security::Wpa2Personal);
    assert_eq!(bss.beacon_interval, 100);

    let open = Bss::parse(&beacon(b"cafe", false, None, 11), 11, -70).unwrap();
    assert_eq!(open.security, Security::Open);
    // Privacy without RSN: WEP. TKIP only: refused too.
    let wep = Bss::parse(&beacon(b"old", true, None, 1), 1, -70).unwrap();
    assert_eq!(wep.security, Security::Unsupported);
    let mut tkip = WPA2_RSN;
    tkip[5] = 2;
    tkip[11] = 2;
    let tkip = Bss::parse(&beacon(b"tkip", true, Some(&tkip), 1), 1, -70).unwrap();
    assert_eq!(tkip.security, Security::Unsupported);
    // The DS parameter is the channel, not where it was heard.
    let elsewhere = Bss::parse(&beacon(b"x", false, None, 36), 40, -50).unwrap();
    assert_eq!(elsewhere.channel, 36);
}

#[test]
fn malformed_beacons_are_dropped() {
    let good = beacon(b"oceans", true, Some(&WPA2_RSN), 6);
    for len in 0..37 {
        assert_eq!(Bss::parse(&good[..len], 6, -40), None, "{len}");
    }
    let mut long = beacon(&[b'x'; 33], false, None, 6);
    assert_eq!(Bss::parse(&long, 6, -40), None);
    long[0] = kind::AUTH;
    assert_eq!(Bss::parse(&long, 6, -40), None);
    // A truncated element ends the walk; what was read stays.
    let mut cut = beacon(b"oceans", false, None, 6);
    cut.extend_from_slice(&[48, 200, 1]);
    assert_eq!(Bss::parse(&cut, 6, -40).unwrap().security, Security::Open);
}

#[test]
fn the_clients_requests() {
    let mut out = [0u8; 256];
    let len = probe_request(&mut out, &US, b"oceans", 6).unwrap();
    assert_eq!(out[0], kind::PROBE_REQUEST);
    assert_eq!(&out[4..10], &BROADCAST);
    assert_eq!(&out[10..16], &US);
    let elements: Vec<_> = super::elements(&out[24..len]).collect();
    assert_eq!(elements[0], (0, &b"oceans"[..]));
    assert_eq!(elements.len(), 3, "SSID, rates, extended rates");

    let len = auth_request(&mut out, &US, &AP).unwrap();
    assert_eq!((out[0], len), (kind::AUTH, 30));
    assert_eq!(&out[24..30], &[0, 0, 1, 0, 0, 0]);

    let bss = Bss::parse(&beacon(b"oceans", true, Some(&WPA2_RSN), 36), 36, -40).unwrap();
    let rsn = [&[48u8, 20][..], &WPA2_RSN].concat();
    let len = assoc_request(&mut out, &US, &bss, Some(&rsn)).unwrap();
    assert_eq!(out[0], kind::ASSOC_REQUEST);
    assert_eq!(u16::from_le_bytes([out[24], out[25]]) & 0x0010, 0x0010);
    let elements: Vec<_> = super::elements(&out[28..len]).collect();
    assert_eq!(elements[0], (0, &b"oceans"[..]));
    // 5 GHz: OFDM rates only, no extended rates; then the RSN element.
    assert_eq!(elements[1].1[0], 0x8c);
    assert_eq!(elements[2], (48, &WPA2_RSN[..]));
}

#[test]
fn the_access_points_answers() {
    let frame = |kind: u8, body: &[u8], to: &MacAddress| -> Vec<u8> {
        let mut f = vec![kind, 0, 0, 0];
        f.extend_from_slice(to);
        f.extend_from_slice(&AP);
        f.extend_from_slice(&AP);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(body);
        f
    };
    assert_eq!(
        answer(&frame(kind::AUTH, &[0, 0, 2, 0, 0, 0], &US), &US, &AP),
        Some(Answer::Authenticated)
    );
    assert_eq!(
        answer(&frame(kind::AUTH, &[0, 0, 2, 0, 17, 0], &US), &US, &AP),
        Some(Answer::Refused(17))
    );
    assert_eq!(
        answer(
            &frame(kind::ASSOC_RESPONSE, &[0x11, 0, 0, 0, 0x05, 0xc0], &US),
            &US,
            &AP
        ),
        Some(Answer::Associated(5))
    );
    assert_eq!(
        answer(&frame(kind::DEAUTH, &[7, 0], &US), &US, &AP),
        Some(Answer::Dropped(7))
    );
    // Not to us, or a short body: nothing.
    assert_eq!(
        answer(&frame(kind::AUTH, &[0, 0, 2, 0, 0, 0], &AP), &US, &AP),
        None
    );
    assert_eq!(answer(&frame(kind::AUTH, &[0, 0], &US), &US, &AP), None);
}

#[test]
fn data_frames_and_ethernet() {
    let mut ethernet = vec![0x02, 9, 9, 9, 9, 9];
    ethernet.extend_from_slice(&US);
    ethernet.extend_from_slice(&[0x08, 0x00]);
    ethernet.extend_from_slice(b"an IPv4 packet");
    let mut air = [0u8; 128];
    let len = from_ethernet(&ethernet, &AP, 0x123, true, &mut air).unwrap();
    assert_eq!(&air[..2], &[kind::DATA, flags::TO_DS | flags::PROTECTED]);
    assert_eq!(&air[4..10], &AP);
    assert_eq!(&air[10..16], &US);
    assert_eq!(&air[16..22], &ethernet[..6]);
    assert_eq!(u16::from_le_bytes([air[22], air[23]]) >> 4, 0x123);
    assert_eq!(&air[24..30], &SNAP);
    assert_eq!(&air[30..len], &ethernet[12..]);

    // The same, coming back from the access point.
    let mut down = vec![kind::QOS_DATA, flags::FROM_DS, 0, 0];
    down.extend_from_slice(&US);
    down.extend_from_slice(&AP);
    down.extend_from_slice(&[0x02, 9, 9, 9, 9, 9]);
    down.extend_from_slice(&[0, 0, 0, 0]);
    down.extend_from_slice(&SNAP);
    down.extend_from_slice(&[0x86, 0xdd]);
    down.extend_from_slice(b"an IPv6 packet");
    let mut back = [0u8; 128];
    let len = to_ethernet(&down, &mut back).unwrap();
    assert_eq!(&back[..6], &US);
    assert_eq!(&back[6..12], &[0x02, 9, 9, 9, 9, 9]);
    assert_eq!(
        &back[12..len],
        &[&[0x86, 0xdd][..], b"an IPv6 packet"].concat()[..]
    );
    // Null data, the other direction, no SNAP: nothing.
    let mut null = down.clone();
    null[0] = 0xc8;
    assert_eq!(to_ethernet(&null, &mut back), None);
    let mut up = down.clone();
    up[1] = flags::TO_DS;
    assert_eq!(to_ethernet(&up, &mut back), None);
    let mut raw = down.clone();
    raw[26] = 0;
    assert_eq!(to_ethernet(&raw, &mut back), None);
}

#[test]
fn beacon_timing_and_rates() {
    let mut frame = beacon(b"oceans", true, Some(&WPA2_RSN), 6);
    frame[24..32].copy_from_slice(&0x1122_3344_5566u64.to_le_bytes());
    frame.extend_from_slice(&[5, 4, 1, 3, 0, 0]);
    let bss = Bss::parse(&frame, 6, -40).unwrap();
    assert_eq!(bss.tsf, 0x1122_3344_5566);
    assert_eq!((bss.dtim_count, bss.dtim_period), (1, 3));
    // Basic rates 1, 2, 5.5, 11: the lowest, 1 Mb/s.
    assert_eq!(bss.basic_rate, 2);
    // A 5 GHz network marking 6, 12 and 24 basic.
    let mut five = beacon(b"five", false, None, 36);
    let rates = five.iter().position(|&b| b == 0x82).unwrap();
    five[rates..rates + 4].copy_from_slice(&[0x98, 0x8c, 0xb0, 0x12]);
    assert_eq!(Bss::parse(&five, 36, -50).unwrap().basic_rate, 12);
}

#[test]
fn frames_from_the_access_point() {
    let mut down = vec![kind::QOS_DATA, flags::FROM_DS | flags::PROTECTED, 0, 0];
    down.extend_from_slice(&US);
    down.extend_from_slice(&AP);
    down.extend_from_slice(&[0x02, 9, 9, 9, 9, 9]);
    down.extend_from_slice(&[0, 0, 0, 0]);
    down.extend_from_slice(&[0; 16]);
    assert_eq!(from_access_point(&down, &AP), Some((26, true)));
    down[0] = kind::DATA;
    down[1] = flags::FROM_DS;
    assert_eq!(from_access_point(&down, &AP), Some((24, false)));
    assert_eq!(from_access_point(&down, &US), None, "another sender");
    down[1] = flags::TO_DS;
    assert_eq!(from_access_point(&down, &AP), None, "the other way");
    down[1] = flags::FROM_DS;
    down[0] = kind::BEACON;
    assert_eq!(from_access_point(&down, &AP), None, "not data");
}
