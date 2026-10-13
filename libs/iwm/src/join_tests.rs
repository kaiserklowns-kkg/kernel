use super::join::*;
use super::mvm;

const AP: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x01];
const US: [u8; 6] = [0x34, 0x13, 0xe8, 1, 2, 3];

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[test]
fn rates() {
    assert_eq!(legacy_rate(2), Some(10 | 1 << 14 | 1 << 9));
    assert_eq!(legacy_rate(12), Some(13 | 1 << 14));
    assert_eq!(legacy_rate(108), Some(3 | 1 << 14));
    assert_eq!(legacy_rate(13), None);
    assert_eq!(management_rate(0, true), 2);
    assert_eq!(management_rate(0, false), 12);
    assert_eq!(management_rate(24, true), 24);

    // 2.4 GHz, every rate basic from 1 Mb/s: 54 ... 6, then 11 ... 1.
    let lq = link_quality(0, true, 2);
    assert_eq!((lq[0], lq[6], lq[7]), (0, 1, 3));
    assert_eq!(word(&lq, 20), legacy_rate(108).unwrap());
    assert_eq!(word(&lq, 20 + 4 * 7), legacy_rate(12).unwrap());
    assert_eq!(word(&lq, 20 + 4 * 8), legacy_rate(22).unwrap());
    assert_eq!(word(&lq, 20 + 4 * 11), legacy_rate(2).unwrap());
    assert_eq!(word(&lq, 20 + 4 * 15), legacy_rate(2).unwrap());
    // 5 GHz, 6 Mb/s lowest: OFDM only, padded with 6 Mb/s.
    let lq = link_quality(0, false, 12);
    assert_eq!(word(&lq, 20 + 4 * 7), legacy_rate(12).unwrap());
    assert_eq!(word(&lq, 20 + 4 * 8), legacy_rate(12).unwrap());
    // 2.4 GHz with 802.11b turned off (lowest basic 6 Mb/s): no CCK.
    let lq = link_quality(0, true, 12);
    assert!((0..16).all(|i| word(&lq, 20 + 4 * i) & 1 << 9 == 0));
}

#[test]
fn the_mac_context() {
    let mut ctx = MacContext {
        action: mvm::action::ADD,
        us: US,
        bssid: AP,
        band_24ghz: true,
        assoc: None,
    };
    let cmd = mac_context(&ctx);
    assert_eq!(word(&cmd, 4), 1);
    assert_eq!(word(&cmd, 8), 5);
    assert_eq!(&cmd[16..22], &US);
    assert_eq!(&cmd[24..30], &AP);
    assert_eq!(word(&cmd, 32), 0xf);
    assert_eq!(word(&cmd, 52), 1 << 2 | 1 << 6, "beacons pass");
    // BE is FIFO 1: CWmin 15, CWmax 1023, AIFSN 3.
    assert_eq!(&cmd[68..76], &[15, 0, 0xff, 3, 3, 2, 0, 0]);
    assert_eq!(word(&cmd, 100), 0, "not associated");

    ctx.action = mvm::action::MODIFY;
    ctx.band_24ghz = false;
    ctx.assoc = Some(Association {
        aid: 5,
        beacon_interval: 100,
        dtim_count: 1,
        dtim_period: 3,
        tsf: 1_000_000,
        received: 5000,
    });
    let cmd = mac_context(&ctx);
    assert_eq!(word(&cmd, 32), 0);
    assert_eq!(word(&cmd, 52), 1 << 2, "beacons filtered once associated");
    assert_eq!(word(&cmd, 100), 1);
    assert_eq!(word(&cmd, 104), 5000 + 102_400);
    assert_eq!(&cmd[108..116], &1_102_400u64.to_le_bytes());
    assert_eq!(word(&cmd, 116), 100);
    assert_eq!(word(&cmd, 120), u32::MAX / 100);
    assert_eq!(word(&cmd, 124), 300);
    assert_eq!(word(&cmd, 136), 5);
    assert_eq!(word(&cmd, 140), 5000);
    // An unknown DTIM keeps beacons passing and the station part empty.
    ctx.assoc = Some(Association {
        dtim_period: 0,
        ..ctx.assoc.unwrap()
    });
    let cmd = mac_context(&ctx);
    assert_eq!(word(&cmd, 52), 1 << 2 | 1 << 6);
    assert_eq!(word(&cmd, 100), 0);
}

#[test]
fn join_commands() {
    let b = binding(mvm::action::ADD);
    assert_eq!(&b[..12], &[0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        &b[12..24],
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]
    );
    assert!(status_ok(&[0, 0, 0, 0]));
    assert!(!status_ok(&[1, 0, 0, 0]));
    assert!(!status_ok(&[]));

    let te = time_event(200, 50);
    assert_eq!(word(&te, 4), 1);
    assert_eq!(word(&te, 16), 50);
    assert_eq!(word(&te, 28), 200);
    assert_eq!(&te[32..36], &[1, 0, 0x03, 0x08]);
    let mut notif = [0u8; 24];
    notif[16] = 1;
    assert_eq!(time_event_notification(&notif), Some((1, 0)));

    let ap = ap_station(AP).command();
    assert_eq!(ap[0], 0, "add");
    assert_eq!(&ap[8..14], &AP);
    assert_eq!(ap[16], 0);
    assert_eq!(&ap[24..28], &((3u32 << 26) | (3 << 28)).to_le_bytes());
    assert_eq!(&ap[40..44], &0x1e0u32.to_le_bytes());
    let modify = ap_station_associated(5).command();
    assert_eq!((modify[0], modify[17]), (1, 0x82));
    assert_eq!(&modify[8..14], &[0; 6]);
    assert_eq!(&modify[36..38], &[5, 0]);
    assert_eq!(&modify[24..28], &[0; 4]);

    let sf = sf_config_full_on();
    assert_eq!(word(&sf, 0), 1);
    assert_eq!(word(&sf, 8), 4096);
    assert_eq!((word(&sf, 52), word(&sf, 56)), (2016, 320));
    assert_eq!((word(&sf, 68), word(&sf, 72)), (10016, 2016));

    assert_eq!(
        mcast_filter(AP),
        [1, 0, 0, 1, 2, 0xaa, 0xbb, 0xcc, 0xdd, 1, 0, 0]
    );
    let pm = mac_power(100, 3);
    assert_eq!(&pm[4..8], &[1, 0, 25, 0]);
    assert_eq!(&mac_power(1000, 10)[6..8], &[30, 0]);
    let quota = time_quota();
    assert_eq!(word(&quota, 0), 0);
    assert_eq!(word(&quota, 4), 128);
    assert_eq!(word(&quota, 16), u32::MAX);
    assert_eq!(word(&quota, 48), u32::MAX);
}

#[test]
fn sending_frames() {
    // An authentication request: management, unicast, at 1 Mb/s.
    let auth = Frame {
        len: 30,
        header_len: 24,
        data: false,
        unicast: true,
        assoc_request: false,
        rate: 2,
    };
    let cmd = tx_command(&auth, 0x1_2345_6788);
    assert_eq!(&cmd[..2], &30u16.to_le_bytes());
    assert_eq!(
        word(&cmd, 4),
        tx_flags::BT_DIS | tx_flags::SEQ_CTL | tx_flags::ACK
    );
    assert_eq!(word(&cmd, 12), legacy_rate(2).unwrap());
    assert_eq!(cmd[16], 0);
    assert_eq!(word(&cmd, 40), u32::MAX);
    assert_eq!(word(&cmd, 44), 0x2345_6788);
    assert_eq!(&cmd[48..54], &[1, 3, 3, 8, 2, 0]);
    // Unicast data: the station's rate table, more retries, TID 0.
    let data = Frame {
        len: 100,
        data: true,
        ..auth
    };
    let cmd = tx_command(&data, 0);
    assert_eq!(word(&cmd, 4) & tx_flags::STA_RATE, tx_flags::STA_RATE);
    assert_eq!(word(&cmd, 12), 0);
    assert_eq!(&cmd[50..54], &[7, 0, 0, 0]);
    // A QoS header (26 bytes) is padded.
    let qos = Frame {
        header_len: 26,
        ..data
    };
    let cmd = tx_command(&qos, 0);
    assert_eq!(word(&cmd, 4) & tx_flags::MH_PAD, tx_flags::MH_PAD);
    assert_eq!(&cmd[2..4], &(1u16 << 13).to_le_bytes());
    // Broadcast: no ACK.
    let group = Frame {
        unicast: false,
        rate: 12,
        ..data
    };
    let cmd = tx_command(&group, 0);
    assert_eq!(word(&cmd, 4) & tx_flags::ACK, 0);
    assert_eq!(word(&cmd, 12), legacy_rate(12).unwrap());

    let mut response = [0u8; 44];
    response[0] = 1;
    response[3] = 2;
    response[36] = 1;
    response[40..44].copy_from_slice(&0x1005u32.to_le_bytes());
    assert_eq!(tx_response(&response), Some((true, 2, 5)));
    response[36] = 0x82;
    assert_eq!(tx_response(&response).map(|r| r.0), Some(false));
    assert_eq!(tx_response(&response[..40]), None);

    let t = tfd(&[(0x1000, 16), (0x2_0000_1010, 80)]);
    assert_eq!(t[3], 2);
    assert_eq!(&t[4..10], &[0, 0x10, 0, 0, 0, 1]);
    assert_eq!(&t[10..14], &0x1010u32.to_le_bytes());
    assert_eq!(u16::from_le_bytes([t[14], t[15]]), 2 | 80 << 4);
    assert_eq!(frame_byte_count(0, 100, true), 27);
    assert_eq!(frame_byte_count(1, 100, false), 1 << 12 | 108);
}
