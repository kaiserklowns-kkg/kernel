extern crate std;

use super::*;
use std::vec::Vec;

#[test]
fn control_values() {
    // EN, BAM, SECRC; BSIZE 2048, legacy descriptors, no UPE/MPE/LPE.
    assert_eq!(receive_control(), 0x0400_8012);
    // EN, PSP, CT 0x0f, COLD 0x3f, RTLC.
    assert_eq!(transmit_control(), 0x0103_f0fa);
    assert_eq!(TRANSMIT_IPG, 0x0060_2008);
    assert!(reg::SPAN > reg::RAH0 && reg::SPAN > reg::MTA + 4 * reg::MTA_LEN);
}

#[test]
fn interrupt_routing() {
    // Every cause valid (bit 3) on vector 0, interrupt on each write-back.
    assert_eq!(ivar(0), 0x8008_0808);
    assert_eq!(ivar(2), 0x800a_0a0a);
    // Vectors above 4 do not exist; the field is three bits.
    assert_eq!(ivar(9) & 0xf, 0x9);
    let msix = interrupt_mask(true);
    assert_ne!(msix & reg::int::RX_QUEUE0, 0);
    assert_ne!(msix & reg::int::OTHER, 0);
    assert_ne!(msix & reg::int::LINK_STATUS_CHANGE, 0);
    let legacy = interrupt_mask(false);
    assert_ne!(legacy & reg::int::RX_TIMER, 0);
    assert_eq!(legacy & reg::int::OTHER, 0);
}

#[test]
fn receive_address_round_trip() {
    let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let (low, high) = receive_address(&mac);
    assert_eq!(low, 0x1200_5452);
    assert_eq!(high, 0x8000_5634);
    assert_eq!(mac_from_receive_address(low, high), Some(mac));
    // Not marked valid, multicast, or all zeros: not a station address.
    assert_eq!(mac_from_receive_address(low, high & !reg::rah::VALID), None);
    assert_eq!(mac_from_receive_address(low | 1, high), None);
    assert_eq!(mac_from_receive_address(0, reg::rah::VALID), None);
}

#[test]
fn nvm_words() {
    assert_eq!(nvm::read_request(0), 1);
    assert_eq!(nvm::read_request(2), (2 << 2) | 1);
    assert_eq!(nvm::read_result(0x5452_0002), Some(0x5452));
    assert_eq!(nvm::read_result(0x5452_0001), None);
    assert_eq!(
        nvm::mac(&[0x5452, 0x1200, 0x5634]),
        Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56])
    );
    assert_eq!(nvm::mac(&[0, 0, 0]), None);
    assert_eq!(nvm::mac(&[0x0001, 0, 0]), None);
    assert_eq!(nvm::mac(&[1, 2]), None);

    let mut words = [0u16; nvm::CHECKSUM_WORDS];
    words[..3].copy_from_slice(&[0x5452, 0x1200, 0x5634]);
    let sum = words[..0x3f].iter().fold(0u16, |s, &w| s.wrapping_add(w));
    words[0x3f] = nvm::CHECKSUM.wrapping_sub(sum);
    assert!(nvm::checksum_valid(&words));
    words[5] ^= 1;
    assert!(!nvm::checksum_valid(&words));
}

#[test]
fn phy_access() {
    // Register 0, PHY 1, read opcode.
    assert_eq!(mdic::read(mdic::PHY_CONTROL), 0x0820_0000);
    assert_eq!(mdic::write(1, 0x1234), 0x0421_1234);
    assert_eq!(mdic::state(0x0820_0000), mdic::State::Busy);
    assert_eq!(mdic::state(0x1820_1140), mdic::State::Done(0x1140));
    assert_eq!(mdic::state(0x5820_0000), mdic::State::Failed);
}

#[test]
fn link_status() {
    assert_eq!(Link::from_status(0), Link::Down);
    // QEMU's e1000e: link up, 1000 Mb/s, full duplex.
    let link = Link::from_status(0x0008_0083);
    assert_eq!(
        link,
        Link::Up {
            mbps: 1000,
            full_duplex: true
        }
    );
    assert_eq!(std::format!("{link}"), "up, 1000 Mb/s full duplex");
    assert_eq!(
        std::format!("{}", Link::from_status(0x42)),
        "up, 100 Mb/s half duplex"
    );
    assert_eq!(
        std::format!("{}", Link::from_status(3)),
        "up, 10 Mb/s full duplex"
    );
}

#[test]
fn receive_descriptors() {
    let posted = RxDescriptor::posted(0x1234_5000);
    let bytes = posted.encode();
    assert_eq!(&bytes[..8], &0x1234_5000u64.to_le_bytes());
    assert_eq!(&bytes[8..], &[0; 8]);
    assert!(!posted.done());

    let written = RxDescriptor {
        address: 0x1234_5000,
        length: 60,
        checksum: 0xabcd,
        status: rx_status::DONE | rx_status::END_OF_PACKET,
        errors: 0,
        special: 7,
    };
    assert_eq!(RxDescriptor::decode(&written.encode()), written);
    assert_eq!(written.encode()[STATUS_OFFSET], written.status);
    assert!(written.done());
    assert_eq!(written.outcome(2048), RxOutcome::Frame(60));

    let with = |length, status, errors| RxDescriptor {
        length,
        status,
        errors,
        ..written
    };
    let whole = rx_status::DONE | rx_status::END_OF_PACKET;
    assert_eq!(with(1514, whole, 0).outcome(2048), RxOutcome::Frame(1514));
    assert_eq!(with(13, whole, 0).outcome(2048), RxOutcome::BadLength(13));
    assert_eq!(
        with(1515, whole, 0).outcome(2048),
        RxOutcome::BadLength(1515)
    );
    // A length beyond the buffer is the device lying.
    assert_eq!(
        with(1000, whole, 0).outcome(512),
        RxOutcome::BadLength(1000)
    );
    assert_eq!(with(60, whole, 1).outcome(2048), RxOutcome::Error(1));
    assert_eq!(with(60, whole, 0x80).outcome(2048), RxOutcome::Error(0x80));
    // Checksum offload errors are not frame errors (offload is off).
    assert_eq!(with(60, whole, 0x60).outcome(2048), RxOutcome::Frame(60));
    assert_eq!(
        with(2048, rx_status::DONE, 0).outcome(2048),
        RxOutcome::Fragment
    );
}

#[test]
fn multi_buffer_frames_are_dropped_whole() {
    let whole = rx_status::DONE | rx_status::END_OF_PACKET;
    let descriptor = |length, status| RxDescriptor {
        length,
        status,
        ..RxDescriptor::default()
    };
    let mut assembler = RxAssembler::default();
    let results: Vec<_> = [
        descriptor(100, whole),
        descriptor(2048, rx_status::DONE),
        descriptor(2048, rx_status::DONE),
        descriptor(500, whole), // the dropped frame's end
        descriptor(200, whole),
        descriptor(5, whole),
        descriptor(300, whole),
    ]
    .iter()
    .map(|d| assembler.accept(d, 2048))
    .collect();
    assert_eq!(
        results,
        [
            Ok(100),
            Err(RxOutcome::Fragment),
            Err(RxOutcome::Fragment),
            Err(RxOutcome::Fragment),
            Ok(200),
            Err(RxOutcome::BadLength(5)),
            Ok(300),
        ]
    );
}

#[test]
fn transmit_descriptors() {
    let descriptor = TxDescriptor::frame(0xdead_b000, 60);
    let bytes = descriptor.encode();
    assert_eq!(&bytes[..8], &0xdead_b000u64.to_le_bytes());
    assert_eq!(&bytes[8..10], &60u16.to_le_bytes());
    // EOP | IFCS | RS; status clear for the device to set.
    assert_eq!(bytes[11], 0x0b);
    assert_eq!(bytes[STATUS_OFFSET], 0);
    assert_eq!(descriptor.words(), [0xdead_b000, 0x0b00_003c]);
    assert!(!TxDescriptor::failed(tx_status::DONE));
    assert!(TxDescriptor::failed(
        tx_status::DONE | tx_status::LATE_COLLISION
    ));
}

#[test]
fn ring_sizes() {
    assert!(valid_ring_size(8));
    assert!(valid_ring_size(64));
    assert!(!valid_ring_size(0));
    assert!(!valid_ring_size(4));
    assert!(!valid_ring_size(12));
    assert!(!valid_ring_size(8192));
}

#[test]
fn receive_ring_keeps_one_descriptor_unposted() {
    let mut ring = RxRing::new(8);
    assert_eq!(ring.initial_tail(), 7);
    // Simulate the device: it may fill descriptors from its head up to
    // the tail, never the tail itself.
    let mut tail = ring.initial_tail();
    let mut head = 0u16;
    let mut filled = 0;
    for round in 0..40 {
        // The device fills what it may (a few at a time).
        for _ in 0..(round % 5) {
            if head == tail {
                break;
            }
            head = (head + 1) % 8;
            filled += 1;
        }
        // The driver consumes some of them, in order.
        for _ in 0..(round % 3) {
            if filled == 0 {
                break;
            }
            assert_ne!(ring.next(), tail, "consumed the unposted descriptor");
            tail = ring.consume();
            filled -= 1;
            // The consumed one is the new unposted descriptor.
            assert_eq!((tail + 1) % 8, ring.next());
        }
        assert!(filled <= 7);
    }
    assert_eq!(ring.size(), 8);
}

#[test]
fn transmit_ring_wraps_and_fills() {
    let mut ring = TxRing::new(8);
    assert_eq!((ring.free(), ring.in_flight(), ring.oldest()), (7, 0, None));
    let claimed: Vec<_> = core::iter::from_fn(|| ring.claim()).collect();
    assert_eq!(claimed, [0, 1, 2, 3, 4, 5, 6]);
    // Full: one descriptor stays empty so TDT never reaches TDH.
    assert_eq!((ring.free(), ring.tail()), (0, 7));
    assert_eq!(ring.oldest(), Some(0));
    ring.complete();
    ring.complete();
    assert_eq!((ring.free(), ring.oldest()), (2, Some(2)));
    assert_eq!(ring.claim(), Some(7));
    assert_eq!(ring.claim(), Some(0));
    assert_eq!(ring.tail(), 1);
    assert_eq!(ring.claim(), None);
    for _ in 0..7 {
        ring.complete();
    }
    assert_eq!((ring.in_flight(), ring.oldest()), (0, None));
    // Completing an empty ring changes nothing.
    ring.complete();
    assert_eq!((ring.free(), ring.tail()), (7, 1));
}
