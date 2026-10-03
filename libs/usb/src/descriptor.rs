//! USB descriptors (USB 2.0 §9.6, HID 1.11 §6.2). Every length and offset
//! a device reports is checked: a malicious device gets `None`, never a
//! panic or an out-of-bounds read.

pub const DEVICE: u8 = 1;
pub const CONFIGURATION: u8 = 2;
pub const STRING: u8 = 3;
pub const INTERFACE: u8 = 4;
pub const ENDPOINT: u8 = 5;
/// SuperSpeed endpoint companion (USB 3.2 §9.6.7).
pub const SS_ENDPOINT_COMPANION: u8 = 0x30;
/// The HID descriptor, inside a configuration (HID 1.11 §6.2.1), and the
/// report descriptor it announces (§6.2.2).
pub const HID: u8 = 0x21;
pub const HID_REPORT: u8 = 0x22;

/// Interface classes.
pub const CLASS_HID: u8 = 3;
pub const CLASS_MASS_STORAGE: u8 = 8;
pub const CLASS_HUB: u8 = 9;
/// HID boot interface subclass, and its keyboard and mouse protocols.
pub const HID_SUBCLASS_BOOT: u8 = 1;
pub const HID_PROTOCOL_KEYBOARD: u8 = 1;
pub const HID_PROTOCOL_MOUSE: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Device {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub device_version: u16,
    pub manufacturer: u8,
    pub product_name: u8,
    pub serial: u8,
    pub configurations: u8,
}

impl Device {
    pub const SIZE: usize = 18;

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..Self::SIZE)?;
        if usize::from(bytes[0]) < Self::SIZE || bytes[1] != DEVICE {
            return None;
        }
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        Some(Self {
            usb_version: u16_at(2),
            class: bytes[4],
            subclass: bytes[5],
            protocol: bytes[6],
            max_packet0: bytes[7],
            vendor: u16_at(8),
            product: u16_at(10),
            device_version: u16_at(12),
            manufacturer: bytes[14],
            product_name: bytes[15],
            serial: bytes[16],
            configurations: bytes[17],
        })
    }

    /// Endpoint 0's packet size: `bMaxPacketSize0` before USB 3, an
    /// exponent from USB 3 on.
    pub fn max_packet0_bytes(&self) -> Option<u16> {
        if self.usb_version >= 0x0300 {
            (self.max_packet0 == 9).then_some(512)
        } else {
            matches!(self.max_packet0, 8 | 16 | 32 | 64).then_some(u16::from(self.max_packet0))
        }
    }
}

/// The 9-byte header of a configuration descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Configuration {
    /// Length of the configuration with all its interfaces and endpoints.
    pub total_length: u16,
    pub interfaces: u8,
    pub value: u8,
    pub attributes: u8,
    /// In 2 mA units (USB 2) or 8 mA units (USB 3).
    pub max_power: u8,
}

impl Configuration {
    pub const SIZE: usize = 9;

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..Self::SIZE)?;
        if usize::from(bytes[0]) < Self::SIZE || bytes[1] != CONFIGURATION {
            return None;
        }
        let total_length = u16::from_le_bytes([bytes[2], bytes[3]]);
        if usize::from(total_length) < Self::SIZE {
            return None;
        }
        Some(Self {
            total_length,
            interfaces: bytes[4],
            value: bytes[5],
            attributes: bytes[7],
            max_power: bytes[8],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Interface {
    pub number: u8,
    pub alternate: u8,
    pub endpoints: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Endpoint {
    /// Bit 7: IN (device to host); bits 0–3: the endpoint number.
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
}

/// Endpoint transfer types.
pub const CONTROL: u8 = 0;
pub const ISOCHRONOUS: u8 = 1;
pub const BULK: u8 = 2;
pub const INTERRUPT: u8 = 3;

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn number(&self) -> u8 {
        self.address & 0x0f
    }

    pub fn transfer_type(&self) -> u8 {
        self.attributes & 0x03
    }

    /// Bytes per packet (bits 0–10; high-bandwidth multipliers ignored).
    pub fn packet_size(&self) -> u16 {
        self.max_packet & 0x07ff
    }

    /// The xHCI device context index (endpoint 0 is 1).
    pub fn dci(&self) -> u8 {
        self.number() * 2 + u8::from(self.is_in())
    }
}

/// One descriptor inside a configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Interface(Interface),
    Endpoint(Endpoint),
    /// Follows a SuperSpeed endpoint: packets per burst, less one.
    Companion {
        max_burst: u8,
    },
    /// A HID descriptor: the length of the interface's report descriptor
    /// (0 if it lists none first).
    Hid {
        report_length: u16,
    },
    /// Anything else: its type and bytes.
    Other(u8, usize),
}

/// The descriptors of a full configuration, in order. Stops at the first
/// malformed one.
pub struct Items<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Items<'a> {
    /// `bytes`: the configuration as read, at most `total_length` long.
    pub fn new(bytes: &'a [u8]) -> Self {
        let len =
            Configuration::parse(bytes).map_or(0, |c| usize::from(c.total_length).min(bytes.len()));
        Self {
            bytes: &bytes[..len],
            at: 0,
        }
    }
}

impl Iterator for Items<'_> {
    type Item = Item;

    fn next(&mut self) -> Option<Item> {
        // Skip the configuration header itself.
        if self.at == 0 {
            self.at = usize::from(*self.bytes.first()?);
        }
        let rest = self.bytes.get(self.at..)?;
        let len = usize::from(*rest.first()?);
        if len < 2 || len > rest.len() {
            self.at = self.bytes.len();
            return None;
        }
        let item = &rest[..len];
        self.at += len;
        Some(match item[1] {
            INTERFACE if len >= 9 => Item::Interface(Interface {
                number: item[2],
                alternate: item[3],
                endpoints: item[4],
                class: item[5],
                subclass: item[6],
                protocol: item[7],
            }),
            ENDPOINT if len >= 7 => Item::Endpoint(Endpoint {
                address: item[2],
                attributes: item[3],
                max_packet: u16::from_le_bytes([item[4], item[5]]),
                interval: item[6],
            }),
            SS_ENDPOINT_COMPANION if len >= 6 => Item::Companion {
                max_burst: item[2].min(15),
            },
            HID if len >= 9 => Item::Hid {
                report_length: if item[6] == HID_REPORT {
                    u16::from_le_bytes([item[7], item[8]])
                } else {
                    0
                },
            },
            kind => Item::Other(kind, len),
        })
    }
}

/// The first boot-protocol interface with `protocol` (keyboard or mouse)
/// in alternate setting 0, and its interrupt IN endpoint.
pub fn find_boot_interface(configuration: &[u8], protocol: u8) -> Option<(Interface, Endpoint)> {
    let mut current: Option<Interface> = None;
    for item in Items::new(configuration) {
        match item {
            Item::Interface(interface) => {
                current = (interface.class == CLASS_HID
                    && interface.subclass == HID_SUBCLASS_BOOT
                    && interface.protocol == protocol
                    && interface.alternate == 0)
                    .then_some(interface);
            }
            Item::Endpoint(endpoint) => {
                if let Some(interface) = current
                    && endpoint.is_in()
                    && endpoint.transfer_type() == INTERRUPT
                    && endpoint.packet_size() > 0
                {
                    return Some((interface, endpoint));
                }
            }
            Item::Companion { .. } | Item::Hid { .. } | Item::Other(..) => {}
        }
    }
    None
}

/// A HID interface (alternate setting 0) with its interrupt IN endpoint
/// and the length of its report descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HidInterface {
    pub interface: Interface,
    pub endpoint: Endpoint,
    pub report_length: u16,
}

impl HidInterface {
    pub fn is_boot(&self, protocol: u8) -> bool {
        self.interface.subclass == HID_SUBCLASS_BOOT && self.interface.protocol == protocol
    }
}

/// The HID interfaces of a configuration, in order, into `out`; returns
/// how many were found (at most `out.len()`). Interfaces without an
/// interrupt IN endpoint are skipped.
pub fn find_hid_interfaces(configuration: &[u8], out: &mut [HidInterface]) -> usize {
    let mut count = 0;
    let mut current: Option<(Interface, u16)> = None;
    for item in Items::new(configuration) {
        match item {
            Item::Interface(interface) => {
                current = (interface.class == CLASS_HID && interface.alternate == 0)
                    .then_some((interface, 0));
            }
            Item::Hid { report_length } => {
                if let Some((_, length)) = current.as_mut() {
                    *length = report_length;
                }
            }
            Item::Endpoint(endpoint) => {
                if let Some((interface, report_length)) = current
                    && endpoint.is_in()
                    && endpoint.transfer_type() == INTERRUPT
                    && endpoint.packet_size() > 0
                {
                    if count == out.len() {
                        break;
                    }
                    out[count] = HidInterface {
                        interface,
                        endpoint,
                        report_length,
                    };
                    count += 1;
                    current = None;
                }
            }
            Item::Companion { .. } | Item::Other(..) => {}
        }
    }
    count
}

/// A bulk endpoint and its SuperSpeed burst size (0 below SuperSpeed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bulk {
    pub endpoint: Endpoint,
    pub max_burst: u8,
}

/// The first interface with this class, subclass and protocol in
/// alternate setting 0, with its bulk IN and OUT endpoints.
pub fn find_bulk_interface(
    configuration: &[u8],
    class: u8,
    subclass: u8,
    protocol: u8,
) -> Option<(Interface, Bulk, Bulk)> {
    let mut current: Option<Interface> = None;
    let (mut bulk_in, mut bulk_out): (Option<Bulk>, Option<Bulk>) = (None, None);
    let mut last: Option<bool> = None;
    for item in Items::new(configuration) {
        match item {
            Item::Interface(interface) => {
                if let (Some(found), Some(i), Some(o)) = (current, bulk_in, bulk_out) {
                    return Some((found, i, o));
                }
                current = (interface.class == class
                    && interface.subclass == subclass
                    && interface.protocol == protocol
                    && interface.alternate == 0)
                    .then_some(interface);
                (bulk_in, bulk_out, last) = (None, None, None);
            }
            Item::Endpoint(endpoint) if current.is_some() && endpoint.transfer_type() == BULK => {
                let bulk = Some(Bulk {
                    endpoint,
                    max_burst: 0,
                });
                if endpoint.is_in() {
                    bulk_in = bulk_in.or(bulk);
                } else {
                    bulk_out = bulk_out.or(bulk);
                }
                last = Some(endpoint.is_in());
            }
            Item::Companion { max_burst } => {
                let target = match last {
                    Some(true) => bulk_in.as_mut(),
                    Some(false) => bulk_out.as_mut(),
                    None => None,
                };
                if let Some(bulk) = target {
                    bulk.max_burst = max_burst;
                }
                last = None;
            }
            _ => last = None,
        }
    }
    match (current, bulk_in, bulk_out) {
        (Some(found), Some(i), Some(o)) => Some((found, i, o)),
        _ => None,
    }
}

/// Decodes a string descriptor (UTF-16LE) into `out` as UTF-8, replacing
/// anything outside printable ASCII with `?`. Returns the length.
pub fn string(bytes: &[u8], out: &mut [u8]) -> Option<usize> {
    let len = usize::from(*bytes.first()?).min(bytes.len());
    if len < 2 || bytes[1] != STRING {
        return None;
    }
    let mut written = 0;
    for unit in bytes[2..len].as_chunks::<2>().0 {
        if written == out.len() {
            break;
        }
        let code = u16::from_le_bytes([unit[0], unit[1]]);
        out[written] = match code {
            0x20..=0x7e => code as u8,
            _ => b'?',
        };
        written += 1;
    }
    Some(written)
}

/// The first language ID of string descriptor 0 (usually 0x0409, US
/// English).
pub fn first_language(bytes: &[u8]) -> Option<u16> {
    let len = usize::from(*bytes.first()?).min(bytes.len());
    if len < 4 || bytes[1] != STRING {
        return None;
    }
    Some(u16::from_le_bytes([bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QEMU's usb-kbd: device and configuration descriptors.
    const KEYBOARD_DEVICE: [u8; 18] = [
        18, 1, 0x00, 0x02, 0, 0, 0, 8, 0x27, 0x06, 0x01, 0x00, 0x00, 0x00, 1, 4, 11, 1,
    ];
    const KEYBOARD_CONFIGURATION: [u8; 34] = [
        9, 2, 34, 0, 1, 1, 7, 0xa0, 50, // configuration
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface: HID boot keyboard
        9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, // HID descriptor
        7, 5, 0x81, 3, 8, 0, 7, // endpoint 1 IN, interrupt, 8 bytes, 7 ms
    ];

    #[test]
    fn parses_a_keyboard() {
        let device = Device::parse(&KEYBOARD_DEVICE).unwrap();
        assert_eq!((device.vendor, device.product), (0x0627, 0x0001));
        assert_eq!(device.max_packet0_bytes(), Some(8));
        assert_eq!(device.configurations, 1);
        let configuration = Configuration::parse(&KEYBOARD_CONFIGURATION).unwrap();
        assert_eq!((configuration.total_length, configuration.value), (34, 1));
        let (interface, endpoint) =
            find_boot_interface(&KEYBOARD_CONFIGURATION, HID_PROTOCOL_KEYBOARD).unwrap();
        assert_eq!(interface.number, 0);
        assert_eq!(endpoint.address, 0x81);
        assert_eq!((endpoint.number(), endpoint.dci()), (1, 3));
        assert_eq!((endpoint.packet_size(), endpoint.interval), (8, 7));
        assert!(find_boot_interface(&KEYBOARD_CONFIGURATION, HID_PROTOCOL_MOUSE).is_none());
    }

    #[test]
    fn walks_items_in_order() {
        let items: [Option<Item>; 4] = {
            let mut it = Items::new(&KEYBOARD_CONFIGURATION);
            [it.next(), it.next(), it.next(), it.next()]
        };
        assert!(matches!(items[0], Some(Item::Interface(_))));
        assert_eq!(items[1], Some(Item::Hid { report_length: 63 }));
        assert!(matches!(items[2], Some(Item::Endpoint(_))));
        assert_eq!(items[3], None);
    }

    #[test]
    fn hostile_descriptors_are_refused() {
        assert!(Device::parse(&KEYBOARD_DEVICE[..17]).is_none());
        let mut wrong = KEYBOARD_DEVICE;
        wrong[1] = 2;
        assert!(Device::parse(&wrong).is_none());
        let mut bad_packet = Device::parse(&KEYBOARD_DEVICE).unwrap();
        bad_packet.max_packet0 = 7;
        assert_eq!(bad_packet.max_packet0_bytes(), None);
        // A descriptor longer than the buffer, and one of length 0, end
        // the walk instead of reading past it or looping.
        let mut long = KEYBOARD_CONFIGURATION;
        long[9] = 200;
        assert_eq!(Items::new(&long).count(), 0);
        let mut zero = KEYBOARD_CONFIGURATION;
        zero[18] = 0;
        assert_eq!(Items::new(&zero).count(), 1);
        // total_length shorter than the buffer bounds the walk.
        let mut short = KEYBOARD_CONFIGURATION;
        short[2] = 27;
        assert!(find_boot_interface(&short, HID_PROTOCOL_KEYBOARD).is_none());
        assert!(Configuration::parse(&[9, 2, 3, 0, 1, 1, 0, 0, 0]).is_none());
    }

    #[test]
    fn finds_bulk_interfaces() {
        // A USB 3 stick: mass storage BOT, bulk IN 0x81 and OUT 0x02 with
        // companions (burst 4 and 0).
        let configuration = [
            9, 2, 44, 0, 1, 1, 0, 0x80, 50, //
            9, 4, 0, 0, 2, 8, 6, 0x50, 0, //
            7, 5, 0x81, 2, 0, 4, 0, //
            6, 0x30, 3, 0, 0, 0, //
            7, 5, 0x02, 2, 0, 4, 0, //
            6, 0x30, 0, 0, 0, 0,
        ];
        let (interface, bulk_in, bulk_out) =
            find_bulk_interface(&configuration, 8, 6, 0x50).unwrap();
        assert_eq!(interface.number, 0);
        assert_eq!((bulk_in.endpoint.address, bulk_in.max_burst), (0x81, 3));
        assert_eq!((bulk_out.endpoint.address, bulk_out.max_burst), (0x02, 0));
        assert_eq!(bulk_in.endpoint.packet_size(), 1024);
        assert!(find_bulk_interface(&configuration, 8, 6, 0x62).is_none());
        assert!(find_bulk_interface(&configuration[..28], 8, 6, 0x50).is_none());
    }

    #[test]
    fn finds_hid_interfaces() {
        // A combo receiver: a boot keyboard, a boot mouse, and a HID
        // interface with no IN endpoint; then QEMU's usb-tablet alone.
        let combo = [
            9, 2, 75, 0, 3, 1, 0, 0xa0, 50, //
            9, 4, 0, 0, 1, 3, 1, 1, 0, //
            9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, //
            7, 5, 0x81, 3, 8, 0, 7, //
            9, 4, 1, 0, 1, 3, 1, 2, 0, //
            9, 0x21, 0x11, 0x01, 0, 1, 0x22, 0x34, 0x01, //
            7, 5, 0x82, 3, 4, 0, 10, //
            9, 4, 2, 0, 1, 3, 0, 0, 0, //
            7, 5, 0x03, 3, 8, 0, 10,
        ];
        let mut found = [HidInterface {
            interface: Interface::default(),
            endpoint: Endpoint::default(),
            report_length: 0,
        }; 4];
        assert_eq!(find_hid_interfaces(&combo, &mut found), 2);
        assert!(found[0].is_boot(HID_PROTOCOL_KEYBOARD));
        assert_eq!(found[0].report_length, 63);
        assert!(found[1].is_boot(HID_PROTOCOL_MOUSE));
        assert_eq!(found[1].interface.number, 1);
        assert_eq!(found[1].endpoint.address, 0x82);
        assert_eq!(found[1].report_length, 0x134);
        // Room for one: the first.
        assert_eq!(find_hid_interfaces(&combo, &mut found[..1]), 1);
        assert_eq!(found[0].interface.number, 0);

        let tablet = [
            9, 2, 34, 0, 1, 1, 0, 0xa0, 50, //
            9, 4, 0, 0, 1, 3, 0, 0, 0, //
            9, 0x21, 0x01, 0x00, 0, 1, 0x22, 74, 0, //
            7, 5, 0x81, 3, 8, 0, 4,
        ];
        assert_eq!(find_hid_interfaces(&tablet, &mut found), 1);
        assert!(!found[0].is_boot(HID_PROTOCOL_MOUSE));
        assert_eq!(found[0].report_length, 74);
        // A HID descriptor naming another descriptor type first.
        let mut odd = tablet;
        odd[24] = 0x23;
        assert_eq!(find_hid_interfaces(&odd, &mut found), 1);
        assert_eq!(found[0].report_length, 0);
    }

    #[test]
    fn decodes_strings() {
        let product = [14, 3, b'Q', 0, b'E', 0, b'M', 0, b'U', 0, 0xe9, 0, b'!', 0];
        let mut out = [0u8; 16];
        let len = string(&product, &mut out).unwrap();
        assert_eq!(&out[..len], b"QEMU?!");
        let mut small = [0u8; 2];
        assert_eq!(string(&product, &mut small), Some(2));
        assert_eq!(first_language(&[4, 3, 0x09, 0x04]), Some(0x0409));
        assert_eq!(first_language(&[2, 3]), None);
        assert_eq!(string(&[2, 1], &mut out), None);
    }
}
