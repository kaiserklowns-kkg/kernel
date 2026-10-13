use super::*;

const FIRMWARE: &[u8] = include_bytes!("../../../firmware/intel/iwlwifi-8000C-36.ucode");

#[test]
fn the_8260_firmware_parses() {
    let fw = fw::Firmware::parse(FIRMWARE).unwrap();
    assert_eq!(fw.version, (36, 0xc8e8_e144, 0));
    // INIT: 5 sections for CPU1, 4 for CPU2, then the paging pair.
    assert_eq!((fw.init.cpu1.len(), fw.init.cpu2.len()), (5, 4));
    // REGULAR: 4 and 4.
    assert_eq!((fw.regular.cpu1.len(), fw.regular.cpu2.len()), (4, 4));
    assert_eq!(fw.regular.cpu1[2].devoff, 0);
    assert_eq!(fw.regular.cpu1[2].data.len(), 327_680);
    assert_eq!(fw.regular.paging_css.map(<[u8]>::len), Some(664));
    assert_eq!(fw.regular.paging.map(<[u8]>::len), Some(155_648));
    assert_eq!(fw.paging_size, 155_648);
    assert_eq!(fw.phy_sku, 0x0033_0018);
    assert_eq!(
        fw.calib_init,
        fw::Calibration {
            flow: 0x5a8e_00db,
            event: 0
        }
    );
    assert_eq!(
        fw.calib_regular,
        fw::Calibration {
            flow: 0x488e_10db,
            event: 0x0a02_909b
        }
    );
    assert!(fw.has_capa(fw::capa::DQA_SUPPORT));
    assert!(fw.has_capa(fw::capa::UMAC_SCAN));
    assert!(fw.dword_byte_counts());
    assert_eq!(fw.n_scan_channels, 52);
}

#[test]
fn other_files_are_refused() {
    assert_eq!(fw::Firmware::parse(b""), Err(fw::Error::NotFirmware));
    assert_eq!(
        fw::Firmware::parse(&FIRMWARE[..200]),
        Err(fw::Error::Truncated)
    );
    let mut bad = FIRMWARE.to_vec();
    bad[4] = 0;
    assert_eq!(fw::Firmware::parse(&bad), Err(fw::Error::NotFirmware));
}

#[test]
fn command_headers_and_tfds() {
    let mut h = [0u8; 8];
    assert_eq!(cmd::header(&mut h, 0x88, 0, 0, 5, 0, 8), 4);
    assert_eq!(&h[..4], &[0x88, 0, 5, 0]);
    assert_eq!(cmd::header(&mut h, 0x4f, 1, 0, 6, 0, 144), 8);
    assert_eq!(h, [0x4f, 1, 6, 0, 144, 0, 0, 0]);
    let tfd = cmd::tfd(0x3_1234_5600, 12);
    assert_eq!(tfd[3], 1);
    assert_eq!(&tfd[4..8], &0x1234_5600u32.to_le_bytes());
    assert_eq!(u16::from_le_bytes([tfd[8], tfd[9]]), 3 | (12 << 4));
    assert_eq!(cmd::byte_count(12, true), 5);
    assert_eq!(cmd::byte_count(12, false), 20);
    assert_eq!(reg::cbbc_queue(0), 0x19d0);
    assert_eq!(reg::cbbc_queue(17), 0x1bf4);
    assert_eq!(reg::cbbc_queue(21), 0x1b24);
    assert_eq!(reg::tcsr_config(9), 0x1e20);
    assert_eq!(reg::tfdib_ctrl0(9), 0x1948);
}

#[test]
fn packets_in_a_buffer() {
    let mut buffer = vec![0u8; 4096];
    // An ALIVE notification (qid 0x80), 68 bytes of payload.
    let size: usize = 4 + 68;
    buffer[..4].copy_from_slice(&(size as u32).to_le_bytes());
    buffer[4..8].copy_from_slice(&[cmd::ALIVE, 0, 0, 0x80]);
    buffer[8..10].copy_from_slice(&alive::OK.to_le_bytes());
    buffer[8 + 40..8 + 44].copy_from_slice(&0x0080_0000u32.to_le_bytes());
    // Then a response to command slot 3 on queue 0, at the next 64 bytes.
    let next = (4 + size).next_multiple_of(64);
    buffer[next..next + 4].copy_from_slice(&8u32.to_le_bytes());
    buffer[next + 4..next + 8].copy_from_slice(&[0x98, 0, 3, 0]);
    let packets: Vec<_> = rx::packets(&buffer).collect();
    assert_eq!(packets.len(), 2);
    assert!(packets[0].notification());
    let a = alive::parse(packets[0].payload).unwrap();
    assert_eq!((a.status, a.scd_base), (alive::OK, 0x80_0000));
    assert!(!packets[1].notification());
    assert_eq!(
        (packets[1].code, packets[1].idx, packets[1].payload.len()),
        (0x98, 3, 4)
    );
    // An overrunning length ends the walk.
    buffer[..4].copy_from_slice(&5000u32.to_le_bytes());
    assert_eq!(rx::packets(&buffer).count(), 0);
}

#[test]
fn nvm_and_the_mac() {
    assert_eq!(nvm::read(11, 2048), [0, 0, 11, 0, 0, 8, 0, 8]);
    let mut answer = vec![0u8, 0, 4, 0, 11, 0, 0, 0];
    answer.extend_from_slice(&[1, 2, 3, 4]);
    assert_eq!(nvm::answer(&answer), Some(&[1u8, 2, 3, 4][..]));
    answer[6] = 1;
    assert_eq!(nvm::answer(&answer), None);
    let section = [0, 0, 0x34, 0x13, 0xe8, 0x01, 0x02, 0x03];
    assert_eq!(
        nvm::mac_override(&section),
        Some([0x34, 0x13, 0xe8, 1, 2, 3])
    );
    assert_eq!(
        nvm::mac_override(&[0, 0, 0x02, 0xcc, 0xaa, 0xff, 0xee, 0x00]),
        None
    );
    assert_eq!(
        nvm::mac_from_registers(0xa0b0_c0d0, 0x0000_e0f0),
        Some([0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0])
    );
}

#[test]
fn paging_of_the_8260_firmware() {
    assert_eq!(paging::layout(155_648), (38, 5, 6));
    let addresses = [
        0x1000u64, 0x10_0000, 0x11_0000, 0x12_0000, 0x13_0000, 0x14_0000,
    ];
    let cmd = paging::command(&addresses, 6);
    assert_eq!(&cmd[..4], &0x306u32.to_le_bytes());
    assert_eq!(&cmd[8..12], &5u32.to_le_bytes());
    assert_eq!(&cmd[12..16], &1u32.to_le_bytes());
    assert_eq!(&cmd[32..36], &0x140u32.to_le_bytes());
}

#[test]
fn nvm_channels_lar_and_antennas() {
    // Channels 1, 6, 11 and 36 valid; 2 marked but not valid.
    let mut regulatory = vec![0u8; 2 * 0x508];
    for (i, flags) in [
        (0usize, 0x0fu16),
        (1, 0x08),
        (5, 0x09),
        (10, 0x01),
        (14, 0x0f),
    ] {
        regulatory[2 * i..2 * i + 2].copy_from_slice(&flags.to_le_bytes());
    }
    let all: Vec<u8> = nvm::channels(&regulatory, true).collect();
    assert_eq!(all, [1, 6, 11, 36]);
    let low: Vec<u8> = nvm::channels(&regulatory, false).collect();
    assert_eq!(low, [1, 6, 11]);
    // LAR: word 0x507 for newer NVMs, 0x4c7 for older.
    assert!(!nvm::lar_enabled(&regulatory, 0xe39));
    regulatory[2 * 0x507] = 0x07;
    assert!(nvm::lar_enabled(&regulatory, 0xe39));
    assert!(!nvm::lar_enabled(&regulatory, 0xe38));
    assert!(!nvm::lar_enabled(&[], 0xe39));
    assert_eq!(nvm::version(&[0x39, 0x0e]), Some(0xe39));
    let sku = nvm::sku(&[0, 0, 0, 0x33, 3, 0, 0, 0]).unwrap();
    assert_eq!(
        sku,
        nvm::Sku {
            band_24ghz: true,
            band_5ghz: true,
            tx_ant: 3,
            rx_ant: 3
        }
    );
}

#[test]
fn commands_after_alive() {
    let q = mvm::scd_queue_cfg(1, 1, 8, 5, 0);
    assert_eq!(q, [0, 1, 8, 1, 1, 0, 5, 64, 0, 0, 0, 0]);

    let aux = mvm::Station::AUX.command();
    assert_eq!(aux[16], 1, "station 1");
    assert_eq!(aux[35], 4, "auxiliary");
    assert_eq!(&aux[2..8], &[0xff, 0xff, 4, 0, 0, 0]);
    assert_eq!(&aux[40..44], &2u32.to_le_bytes());
    assert_eq!(&aux[24..28], &[0; 4]);
    assert!(mvm::add_sta_ok(&[1, 0, 0, 0]));
    assert!(!mvm::add_sta_ok(&[2, 0, 0, 0]));

    let phy = mvm::phy_context(2, mvm::action::ADD, 1, 3, 3);
    assert_eq!(&phy[..8], &[2, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!(&phy[16..20], &[1, 1, 0, 0]);
    assert_eq!(&phy[24..28], &(6u32 | 1 << 10 | 1 << 12).to_le_bytes());
    assert_eq!(mvm::phy_context(0, 2, 36, 1, 1)[16], 0, "5 GHz");

    let mcc = mvm::mcc_update(*b"ZZ");
    assert_eq!(&mcc[..4], &[b'Z', b'Z', 0x10, 0]);
    assert_eq!(
        mvm::mcc_answer(&[0, 0, 0, 0, b'H', b'T', 0, 0]),
        Some((0, *b"TH"))
    );
}

#[test]
fn scan_commands() {
    let mac = [0x34, 0x13, 0xe8, 1, 2, 3];
    let channels = [1u8, 6, 11, 36];
    let cfg = mvm::scan_config(3, 3, mac, &channels, 52);
    assert_eq!(cfg.len(), 88);
    let flags = u32::from_le_bytes(cfg[..4].try_into().unwrap());
    assert_eq!(flags >> 26, 4);
    assert_eq!(flags & 0x3ff_ffff, 0x2_ef09);
    assert_eq!(&cfg[12..16], &0x0fff_0fffu32.to_le_bytes());
    assert_eq!(&cfg[24..34], &[10, 110, 44, 90, 0x34, 0x13, 0xe8, 1, 2, 3]);
    assert_eq!(&cfg[34..41], &[1, 0, 1, 6, 11, 36, 0]);

    // Passive, every channel: 1684 bytes for this firmware.
    let scan = mvm::scan_request(&channels, None, mac, true, 52);
    assert_eq!(scan.len(), 1684);
    let general = u16::from_le_bytes([scan[12], scan[13]]);
    assert_eq!(general, (1 << 2) | (1 << 3) | (1 << 5) | (1 << 13));
    assert_eq!(&scan[16..24], &[10, 110, 44, 2, 10, 0, 44, 1]);
    assert_eq!(scan[45], 4);
    assert_eq!(&scan[48 + 8..48 + 16], &[0, 0, 0, 0, 6, 1, 0, 0]);
    let tail = 48 + 8 * 52;
    assert_eq!(&scan[tail..tail + 4], &[0, 0, 1, 0]);
    // The probe template: header and empty SSID (26), 2.4 GHz rates,
    // extended rates and DS (19), 5 GHz rates (10), nothing common.
    let template = tail + 12;
    let segments: Vec<u16> = scan[template..template + 16]
        .chunks(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(segments, [0, 26, 26, 19, 45, 10, 55, 0]);
    let frame = &scan[template + 16..];
    assert_eq!(frame[0], 0x40);
    assert_eq!(&frame[10..16], &mac);
    assert_eq!(&frame[24..28], &[0, 0, 1, 8]);
    assert_eq!(scan[template + 16 + 512 + 1], 0, "no direct SSID");

    // Looking for one network: probing, and its SSID.
    let scan = mvm::scan_request(&channels, Some(b"oceans"), mac, false, 52);
    let general = u16::from_le_bytes([scan[12], scan[13]]);
    assert_eq!(general & (1 << 4 | 1 << 3), 1 << 4);
    assert_eq!(&scan[22..24], &100u16.to_le_bytes());
    assert_eq!(scan[48], 1);
    let direct = template + 16 + 512;
    assert_eq!(
        &scan[direct..direct + 8],
        &[0, 6, b'o', b'c', b'e', b'a', b'n', b's']
    );
    assert_eq!(
        &scan[template + 8..template + 12],
        &[0, 0, 0, 0],
        "no 5 GHz"
    );
}

#[test]
fn received_frames() {
    let mut phy = [0u8; 68];
    phy[1] = 4;
    phy[22] = 6;
    phy[28..32].copy_from_slice(&[45, 52, 0, 0]);
    phy[4..8].copy_from_slice(&1234u32.to_le_bytes());
    let info = mvm::rx_phy(&phy).unwrap();
    assert_eq!((info.channel, info.rssi, info.system_time), (6, -45, 1234));
    phy[28..32].fill(0);
    assert_eq!(mvm::rx_phy(&phy).unwrap().rssi, -128);
    phy[1] = 21;
    assert_eq!(mvm::rx_phy(&phy), None);
    assert_eq!(mvm::rx_phy(&phy[..60]), None);

    let frame = [0x80u8; 30];
    let mut mpdu = vec![30, 0, 0, 0];
    mpdu.extend_from_slice(&frame);
    mpdu.extend_from_slice(&3u32.to_le_bytes());
    assert_eq!(mvm::rx_mpdu(&mpdu), Some(&frame[..]));
    let at = mpdu.len() - 4;
    mpdu[at] = 2;
    assert_eq!(mvm::rx_mpdu(&mpdu), None, "bad CRC");
    assert_eq!(mvm::rx_mpdu(&mpdu[..20]), None);
}
