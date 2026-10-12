extern crate std;

use super::*;

#[test]
fn revisions_from_tx_config() {
    // Revision bits only; the rest of TxConfig is ignored.
    assert_eq!(
        Revision::from_tx_config(0x5400_0000 | 0x0300_0700),
        Some(Revision::H)
    );
    assert_eq!(Revision::from_tx_config(0x4c00_0000), Some(Revision::G));
    assert_eq!(Revision::from_tx_config(0x2c80_0000), Some(Revision::EVl));
    assert_eq!(Revision::from_tx_config(0x3840_0000), Some(Revision::B));
    // An 810x fast-Ethernet part, an old 8169, an RTL8125: not taken.
    assert_eq!(Revision::from_tx_config(0x4480_0000), None);
    assert_eq!(Revision::from_tx_config(0x1000_0000), None);
    assert_eq!(Revision::from_tx_config(0x6090_0000), None);
    assert!(Revision::H.gated() && !Revision::F.gated());
    assert!(!Revision::B.address_in_registers() && Revision::C.address_in_registers());
    assert_eq!(Revision::F.early_off(), 0x3800);
    assert_eq!(Revision::H.early_off(), 0x0800);
    assert_eq!(Revision::D.early_off(), 0);
    assert!(known(0x10ec, 0x8168) && known(0x10ff, 0x8168) && !known(0x10ec, 0x8139));
}

#[test]
fn configuration_values() {
    assert_eq!(rx_config(Revision::E), 0xff0e);
    assert_eq!(rx_config(Revision::H), 0xff0e | 0x0800);
    assert_eq!(TX_CONFIG, 0x0300_0700);
    assert!(SPAN > reg::RX_DESC_HIGH && SPAN > reg::MCU_CMD);
}

#[test]
fn station_address() {
    let mac = [0x00, 0xe0, 0x4c, 0x68, 0x01, 0x02];
    let (low, high) = station_registers(&mac);
    assert_eq!(station(low, high), Some(mac));
    // The two upper bytes of IDR4 are not part of it.
    assert_eq!(station(low, high | 0xffff_0000), Some(mac));
    assert_eq!(station(0, 0), None);
    assert_eq!(station(!0, 0xffff), None);
    // A multicast address is no station's.
    assert_eq!(station(0x0000_0001, 0), None);
}

#[test]
fn descriptors() {
    let [command, address] = rx_posted(0x1000, 2048, false);
    assert_eq!((command, address), (u64::from(desc::OWN) | 2048, 0x1000));
    let [command, _] = rx_posted(0x1000, 2048, true);
    assert!(command & u64::from(desc::END_OF_RING) != 0);
    let [command, address] = tx_frame(0x2000, 60, true);
    assert_eq!(
        command as u32,
        desc::OWN | desc::END_OF_RING | desc::FIRST | desc::LAST | 60
    );
    assert_eq!((command >> 32, address), (0, 0x2000));
    assert_eq!(tx_empty(false), [0, 0]);
    assert_eq!(tx_empty(true)[0], u64::from(desc::END_OF_RING));

    let whole = desc::FIRST | desc::LAST;
    assert_eq!(rx_outcome(desc::OWN | 64, 2048), RxOutcome::Pending);
    // The CRC is counted in the length.
    assert_eq!(rx_outcome(whole | 64, 2048), RxOutcome::Frame(60));
    assert_eq!(
        rx_outcome(whole | desc::RX_ERROR | 64, 2048),
        RxOutcome::Bad
    );
    assert_eq!(rx_outcome(desc::FIRST | 64, 2048), RxOutcome::Bad);
    assert_eq!(rx_outcome(whole | 10, 2048), RxOutcome::Bad);
    assert_eq!(rx_outcome(whole | 1518, 2048), RxOutcome::Frame(1514));
    assert_eq!(rx_outcome(whole | 1519, 2048), RxOutcome::Bad);
}

#[test]
fn phy_and_link() {
    assert_eq!(phyar::read(4), 4 << 16);
    assert_eq!(phyar::write(0, 0x1200), (1 << 31) | 0x1200);
    assert_eq!(Link::from_phy_status(0), Link::Down);
    assert_eq!(
        Link::from_phy_status(0x13),
        Link::Up {
            mbps: 1000,
            full_duplex: true
        }
    );
    assert_eq!(
        Link::from_phy_status(0x0a),
        Link::Up {
            mbps: 100,
            full_duplex: false
        }
    );
}
