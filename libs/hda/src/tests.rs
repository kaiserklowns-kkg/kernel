extern crate std;

use std::vec::Vec;

use super::*;

#[test]
fn capabilities_and_the_first_output_stream() {
    // QEMU's ICH6 controller: 4 output, 4 input streams, 64-bit.
    let caps = Capabilities::decode(0x4401);
    assert_eq!(
        caps,
        Capabilities {
            output_streams: 4,
            input_streams: 4,
            addressing_64: true
        }
    );
    assert_eq!(caps.first_output(), Some(0x80 + 4 * 0x20));
    assert_eq!(Capabilities::decode(0x0400).first_output(), None);
}

#[test]
fn verbs_are_encoded_as_the_specification_says() {
    // Codec 0, node 0x02, GET_PARAMETER(WIDGET_CAPS).
    assert_eq!(
        verb(0, 2, v::GET_PARAMETER, param::WIDGET_CAPS),
        0x002f_0009
    );
    // Codec 2, node 0x14, SET_PIN_CONTROL(0x40).
    assert_eq!(verb(2, 0x14, v::SET_PIN_CONTROL, 0x40), 0x2147_0740);
    // SET_FORMAT(48 kHz 16-bit stereo) on node 2.
    assert_eq!(
        long_verb(0, 2, v::SET_FORMAT, FORMAT_48K_16_STEREO),
        0x0022_0011
    );
    // Output amp, both channels, unmuted, gain 0x27.
    assert_eq!(amp_payload(true, 0, 0x27), 0xb027);
    assert_eq!(amp_payload(false, 1, 0), 0x7100);
    assert_eq!(BYTES_PER_SECOND, 192_000);
}

#[test]
fn answers_are_decoded() {
    assert_eq!(node_range(0x0002_0005), 2..7);
    assert!(is_audio_function(0x0000_0101));
    assert!(!is_audio_function(0x0000_0002));
    let dac = WidgetCaps::decode(0x0000_0405);
    assert_eq!(dac.kind, WidgetType::Output);
    assert!(dac.output_amp);
    let pin = WidgetCaps::decode(0x0040_0185);
    assert_eq!(pin.kind, WidgetType::Pin);
    assert!(pin.connections);
    // Port connectivity "none" wins over the device type.
    assert_eq!(Jack::decode(0x4011_0000), Jack::None);
    assert_eq!(Jack::decode(0x0011_0000), Jack::Speaker);
    assert_eq!(Jack::decode(0x0221_4010), Jack::Headphone);
    assert_eq!(Jack::decode(0x0101_4010), Jack::LineOut);
    assert!(pin_can_output(0x0000_0010));
    assert_eq!(connection_length(0x0000_0002), (2, false));
    assert_eq!(connections(0x0000_0302), [2, 3, 0, 0]);
    assert_eq!(pin_output_control(Jack::Headphone), 0xc0);
}

fn widget(node: u8, kind: u32, jack: Jack, inputs: &[u8]) -> Widget {
    let mut list = [0u8; 8];
    list[..inputs.len()].copy_from_slice(inputs);
    Widget {
        node,
        caps: WidgetCaps::decode(kind << 20 | 1 << 8),
        jack,
        can_output: kind == 4,
        inputs: list,
        input_count: inputs.len() as u8,
    }
}

#[test]
fn the_path_goes_from_the_preferred_pin_back_to_a_converter() {
    // Like QEMU's hda-output: one converter (2) straight to a line-out pin
    // (3).
    let simple = [
        widget(2, 0, Jack::Other, &[]),
        widget(3, 4, Jack::LineOut, &[2]),
    ];
    let path = find_output(&simple).unwrap();
    assert_eq!(path.nodes(), [3, 2]);
    assert_eq!(
        (path.pin(), path.converter(), path.jack),
        (3, 2, Jack::LineOut)
    );

    // A laptop-like codec: headphones and a speaker, each through a mixer
    // whose second input is the converter; a microphone pin; a pin wired
    // to nothing. The speaker is preferred.
    let laptop = [
        widget(0x02, 0, Jack::Other, &[]),
        widget(0x03, 0, Jack::Other, &[]),
        widget(0x0c, 2, Jack::Other, &[0x18, 0x02]),
        widget(0x0d, 2, Jack::Other, &[0x18, 0x03]),
        widget(0x14, 4, Jack::Speaker, &[0x0d]),
        widget(0x15, 4, Jack::Headphone, &[0x0c]),
        widget(0x18, 4, Jack::Other, &[]),
        widget(0x1b, 4, Jack::None, &[0x0c]),
    ];
    let path = find_output(&laptop).unwrap();
    assert_eq!(path.nodes(), [0x14, 0x0d, 0x03]);
    // Through the mixer's second input.
    assert_eq!(path.selects[1], 1);
    assert_eq!(path.jack, Jack::Speaker);
}

#[test]
fn no_path_without_an_output_pin_or_a_converter() {
    let no_converter = [
        widget(3, 4, Jack::LineOut, &[4]),
        widget(4, 2, Jack::Other, &[3]),
    ];
    assert_eq!(find_output(&no_converter), None);
    let only_inputs = [
        widget(2, 0, Jack::Other, &[]),
        widget(3, 4, Jack::None, &[2]),
    ];
    assert_eq!(find_output(&only_inputs), None);
    assert_eq!(find_output(&[]), None);
}

#[test]
fn buffer_descriptors_and_stream_control() {
    let entry = bdl_entry(0x1234_5678_9abc, 0x4000, true);
    assert_eq!(&entry[..8], &0x1234_5678_9abcu64.to_le_bytes());
    assert_eq!(&entry[8..12], &0x4000u32.to_le_bytes());
    assert_eq!(&entry[12..], &[1, 0, 0, 0]);
    assert_eq!(stream_control(1), 0x0010_0000);
}

#[test]
fn the_ring_counts_what_was_written_and_played() {
    let mut ring = Ring::new(1000);
    assert_eq!(ring.free(), 1000);
    ring.written += 800;
    assert_eq!(ring.free(), 200);
    assert_eq!(ring.write_offset(), 800);
    // The position wraps: 0 → 600 → 100 (past the end) is 1100 played.
    ring.advance(600);
    ring.advance(100);
    assert_eq!(ring.played, 1100);
    // Played past what was written: silence, so writing restarts there.
    assert_eq!(ring.written, 1100);
    assert!(ring.drained());
    assert_eq!(ring.free(), 1000);
    let mut sizes = Vec::new();
    for position in [200u32, 400, 400] {
        ring.advance(position);
        sizes.push(ring.played);
    }
    assert_eq!(sizes, [1200, 1400, 1400]);
}
