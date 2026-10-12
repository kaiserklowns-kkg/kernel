extern crate std;

use super::*;

#[test]
fn families_from_device_ids() {
    assert_eq!(family(DEVICE_82576), Some(Family::I82576));
    assert_eq!(family(0x1533), Some(Family::I210));
    assert_eq!(family(0x15f3), Some(Family::I225));
    assert_eq!(family(0x125c), Some(Family::I225));
    // Fibre, SerDes and virtual functions are not taken.
    assert_eq!(family(0x10e6), None);
    assert_eq!(family(0x10ca), None);
    for (index, &(id, _)) in DEVICES.iter().enumerate() {
        assert!(DEVICES[index + 1..].iter().all(|&(other, _)| other != id));
    }
    assert!(blank_nvm(0x15fd) && !blank_nvm(0x15f3));
}

#[test]
fn what_differs_between_families() {
    assert_eq!(Family::I82576.reset_bit(), 1 << 26);
    assert_eq!(Family::I225.reset_bit(), 1 << 29);
    assert_eq!(
        (
            Family::I82576.icr(),
            Family::I82576.ims(),
            Family::I82576.imc()
        ),
        (0xc0, 0xd0, 0xd8)
    );
    assert_eq!(
        (Family::I225.icr(), Family::I225.ims(), Family::I225.imc()),
        (0x1500, 0x1508, 0x150c)
    );
    assert_eq!(
        (Family::I210.phy_address(), Family::I225.phy_address()),
        (1, 0)
    );
    assert!(reg::SPAN > Family::I225.imc() && reg::SPAN > reg::RAH0);
}

#[test]
fn queue_values() {
    assert_eq!(split_receive_control(2048), 2 | (1 << 25));
    assert_eq!(split_receive_control(1500), 2 | (1 << 25));
    assert_eq!(receive_queue(0xfff0_0000) & QUEUE_ENABLE, QUEUE_ENABLE);
    assert_eq!(receive_queue(0x0000_ffff), QUEUE_ENABLE | 0x0004_0808);
    assert_eq!(
        transmit_queue() & (QUEUE_ENABLE | (1 << 22)),
        QUEUE_ENABLE | (1 << 22)
    );
    // EN, MPE, BAM, SECRC; DTYP 0.
    assert_eq!(receive_control(), 0x0400_8012);
    assert_eq!(receive_control() & (3 << 10), 0);
}

#[test]
fn link_from_status() {
    assert_eq!(Link::from_status(0), Link::Down);
    assert_eq!(
        Link::from_status(0b11 | (2 << 6)),
        Link::Up {
            mbps: 1000,
            full_duplex: true
        }
    );
    assert_eq!(
        Link::from_status(0b11 | (2 << 6) | (1 << 22)),
        Link::Up {
            mbps: 2500,
            full_duplex: true
        }
    );
    assert_eq!(
        Link::from_status(0b10 | (1 << 6)),
        Link::Up {
            mbps: 100,
            full_duplex: false
        }
    );
}

#[test]
fn advanced_descriptors() {
    let [address, words] = tx_frame(0x1234_5000, 60);
    assert_eq!(address, 0x1234_5000);
    let command = words as u32;
    assert_eq!(command & 0xffff, 60);
    assert_eq!((command >> 20) & 0xf, 0b0011);
    for bit in [24, 25, 27, 29] {
        assert!(command & (1 << bit) != 0, "bit {bit}");
    }
    assert_eq!((words >> 32) as u32 >> 14, 60);
    assert_eq!(rx_posted(0x8000), [0x8000, 0]);

    let written =
        |status: u32, len: u16| RxWriteBack::decode(u64::from(status) | (u64::from(len) << 32));
    let mut assembler = RxAssembler::default();
    assert!(written(3, 60).done());
    assert_eq!(assembler.accept(written(3, 60), 2048), RxOutcome::Frame(60));
    assert_eq!(
        assembler.accept(written(3 | (1 << 31), 60), 2048),
        RxOutcome::Bad
    );
    assert_eq!(assembler.accept(written(3, 10), 2048), RxOutcome::Bad);
    assert_eq!(assembler.accept(written(3, 2000), 2048), RxOutcome::Bad);
    // A frame over two buffers goes whole; the next one is taken.
    assert_eq!(
        assembler.accept(written(1, 2048), 2048),
        RxOutcome::Fragment
    );
    assert_eq!(assembler.accept(written(3, 100), 2048), RxOutcome::Fragment);
    assert_eq!(assembler.accept(written(3, 64), 2048), RxOutcome::Frame(64));
}

#[test]
fn phy_access() {
    assert_eq!(mdic::read(1, 0), (1 << 21) | (2 << 26));
    assert_eq!(mdic::result(0, 0), None);
    assert_eq!(mdic::result((1 << 28) | 0x1140, 0), Some(Ok(0x1140)));
    assert_eq!(mdic::result((1 << 28) | (1 << 30), 0), Some(Err(())));
    assert_eq!(mdic::result((1 << 28) | (3 << 16), 0), Some(Err(())));
}
