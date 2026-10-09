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
    assert_eq!(Jack::decode(0x0181_3020), Jack::LineIn);
    assert_eq!(Jack::decode(0x01a1_9020), Jack::Mic);
    assert_eq!(Jack::decode(0x90a7_0130), Jack::Mic);
    assert_eq!(Jack::Mic.input_preference(), Some(0));
    assert_eq!(Jack::LineIn.input_preference(), Some(1));
    assert_eq!(Jack::Speaker.input_preference(), None);
    assert_eq!(Jack::Mic.preference(), None);
    assert!(pin_can_input(1 << 5) && !pin_can_input(1 << 4));
    assert_eq!(pin_input_control(Jack::LineIn, 1 << 12), 0x20);
    assert_eq!(pin_input_control(Jack::Mic, 1 << 12), 0x24);
    assert_eq!(pin_input_control(Jack::Mic, 0), 0x20);
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
        can_output: kind == 4 && jack.input_preference().is_none(),
        can_input: kind == 4 && jack.input_preference().is_some(),
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
fn the_input_path_goes_from_a_converter_back_to_the_preferred_pin() {
    // Like QEMU's hda-duplex: an input converter (4) straight from a
    // line-in pin (5); its output side does not get in the way.
    let duplex = [
        widget(2, 0, Jack::Other, &[]),
        widget(3, 4, Jack::LineOut, &[2]),
        widget(4, 1, Jack::Other, &[5]),
        widget(5, 4, Jack::LineIn, &[]),
    ];
    let path = find_input(&duplex).unwrap();
    assert_eq!(path.nodes(), [4, 5]);
    assert_eq!(path.jack, Jack::LineIn);
    assert_eq!(find_output(&duplex).unwrap().nodes(), [3, 2]);

    // A laptop-like codec: two input converters, each behind a selector of
    // the line-in jack (0x1a) and the built-in microphone (0x12). The
    // microphone is preferred, through the first converter's selector.
    let laptop = [
        widget(0x08, 1, Jack::Other, &[0x23]),
        widget(0x09, 1, Jack::Other, &[0x22]),
        widget(0x12, 4, Jack::Mic, &[]),
        widget(0x14, 4, Jack::Speaker, &[]),
        widget(0x1a, 4, Jack::LineIn, &[]),
        widget(0x22, 3, Jack::Other, &[0x1a, 0x12]),
        widget(0x23, 3, Jack::Other, &[0x1a, 0x12]),
    ];
    let path = find_input(&laptop).unwrap();
    assert_eq!(path.nodes(), [0x08, 0x23, 0x12]);
    assert_eq!(path.selects[1], 1, "the selector's second input");
    assert_eq!(path.jack, Jack::Mic);
}

#[test]
fn no_input_path_without_an_input_pin_or_converter() {
    // An output-only codec (QEMU's hda-output).
    let output_only = [
        widget(2, 0, Jack::Other, &[]),
        widget(3, 4, Jack::LineOut, &[2]),
    ];
    assert_eq!(find_input(&output_only), None);
    // A microphone no converter reaches.
    let unreached = [
        widget(4, 1, Jack::Other, &[6]),
        widget(5, 4, Jack::Mic, &[]),
        widget(6, 4, Jack::LineOut, &[]),
    ];
    assert_eq!(find_input(&unreached), None);
    assert_eq!(find_input(&[]), None);
}

#[test]
fn the_capture_counts_what_came_in_and_drops_the_oldest() {
    let mut capture = Capture::new(1000);
    capture.advance(400);
    assert_eq!((capture.available(), capture.read_offset()), (400, 0));
    capture.taken += 300;
    assert_eq!((capture.available(), capture.read_offset()), (100, 300));
    // Wrapped: 400 -> 900 -> 200 is 800 more, 900 held: still fits.
    capture.advance(900);
    capture.advance(200);
    assert_eq!(
        (capture.captured, capture.available(), capture.lost),
        (1200, 900, 0)
    );
    // 500 more: 1400 held in a 1000-byte buffer; the oldest 400 are lost.
    capture.advance(700);
    assert_eq!((capture.available(), capture.lost), (1000, 400));
    assert_eq!(capture.read_offset(), 700);
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

#[test]
fn the_volume_scales_samples_by_the_square_of_its_level() {
    use crate::volume::{MAX, UNITY, apply, gain};
    assert_eq!(gain(MAX, false), UNITY);
    assert_eq!(gain(250, false), UNITY);
    assert_eq!(gain(50, false), UNITY / 4);
    assert_eq!(gain(10, false), UNITY / 100);
    assert_eq!(gain(0, false), 0);
    assert_eq!(gain(MAX, true), 0);
    // Full volume leaves the bytes alone; a quarter shrinks both signs.
    let samples: [i16; 4] = [i16::MAX, i16::MIN, -400, 3];
    let mut bytes: std::vec::Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let original = bytes.clone();
    apply(&mut bytes, gain(MAX, false));
    assert_eq!(bytes, original);
    apply(&mut bytes, gain(50, false));
    let scaled: std::vec::Vec<i16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    assert_eq!(scaled, [8191, -8192, -100, 0]);
    // Muted: silence; an odd last byte is left.
    let mut odd = [0x34, 0x12, 0x7f];
    apply(&mut odd, 0);
    assert_eq!(odd, [0, 0, 0x7f]);
}
