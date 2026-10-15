//! The destinations the stack has sent to, each named once (ADR-0146).
//!
//! The network service writes a line the first time a socket connects or
//! sends to a destination, to the serial line only (never kept: a list of
//! where the network went is the user's history). The smoke test reads
//! those lines to check that an idle system contacts nothing it should
//! not (the promise of no telemetry, ADR-0146).

use alloc::collections::BTreeSet;
use core::fmt;

use oceans_inet::{Colons, Dotted, IpAddr};

/// How many destinations are remembered; past it, one line says so and the
/// rest go unnamed.
pub const MAX_CONTACTS: usize = 256;

/// What reached a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Protocol {
    Tcp,
    Udp,
    Ping,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::Ping => "ping",
        }
    }
}

/// What [`Contacts::note`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contact {
    /// Never contacted before: name it.
    First,
    /// Named already, or past the limit after [`Contact::Full`].
    Known,
    /// The limit was reached just now: say no more are named.
    Full,
}

/// The destinations named so far.
#[derive(Default)]
pub struct Contacts {
    seen: BTreeSet<(Protocol, IpAddr, u16)>,
    full: bool,
}

impl Contacts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes a send or a connection to `address` and `port` (0 for ping).
    pub fn note(&mut self, protocol: Protocol, address: IpAddr, port: u16) -> Contact {
        if self.seen.contains(&(protocol, address, port)) {
            return Contact::Known;
        }
        if self.seen.len() >= MAX_CONTACTS {
            if self.full {
                return Contact::Known;
            }
            self.full = true;
            return Contact::Full;
        }
        self.seen.insert((protocol, address, port));
        Contact::First
    }

    /// Notes a datagram received from `address` and `port`: someone else
    /// reached us, so answering them is not a contact of ours. Never named;
    /// past the limit it is simply not remembered.
    pub fn heard(&mut self, protocol: Protocol, address: IpAddr, port: u16) {
        if self.seen.len() < MAX_CONTACTS {
            self.seen.insert((protocol, address, port));
        }
    }
}

/// `udp 10.0.2.2:123`, `tcp [2001:db8::1]:443`, `ping 10.0.2.2`: how a
/// destination is named in the log.
pub struct Destination(pub Protocol, pub IpAddr, pub u16);

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(protocol, address, port) = *self;
        f.write_str(protocol.name())?;
        match (address, protocol) {
            (IpAddr::V4(v4), Protocol::Ping) => write!(f, " {}", Dotted(v4)),
            (IpAddr::V6(v6), Protocol::Ping) => write!(f, " {}", Colons(v6)),
            (IpAddr::V4(v4), _) => write!(f, " {}:{port}", Dotted(v4)),
            (IpAddr::V6(v6), _) => write!(f, " [{}]:{port}", Colons(v6)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn each_destination_is_named_once() {
        let mut contacts = Contacts::new();
        let host = IpAddr::V4([10, 0, 2, 2]);
        assert_eq!(contacts.note(Protocol::Udp, host, 123), Contact::First);
        assert_eq!(contacts.note(Protocol::Udp, host, 123), Contact::Known);
        // Another port, another protocol: other destinations.
        assert_eq!(contacts.note(Protocol::Udp, host, 124), Contact::First);
        assert_eq!(contacts.note(Protocol::Tcp, host, 123), Contact::First);
    }

    #[test]
    fn answering_who_reached_us_is_not_a_contact() {
        // The echo service answers the host's probe (ADR-0146).
        let mut contacts = Contacts::new();
        let prober = IpAddr::V4([10, 0, 2, 2]);
        contacts.heard(Protocol::Udp, prober, 53942);
        assert_eq!(contacts.note(Protocol::Udp, prober, 53942), Contact::Known);
        // Another port of the same host is still ours to name.
        assert_eq!(contacts.note(Protocol::Udp, prober, 53943), Contact::First);
    }

    #[test]
    fn past_the_limit_one_line_says_so() {
        let mut contacts = Contacts::new();
        for i in 0..MAX_CONTACTS {
            let address = IpAddr::V4([10, 0, (i / 256) as u8, i as u8]);
            assert_eq!(contacts.note(Protocol::Tcp, address, 80), Contact::First);
        }
        let more = IpAddr::V4([192, 0, 2, 1]);
        assert_eq!(contacts.note(Protocol::Tcp, more, 80), Contact::Full);
        assert_eq!(contacts.note(Protocol::Tcp, more, 81), Contact::Known);
        // Those named before stay known.
        let first = IpAddr::V4([10, 0, 0, 0]);
        assert_eq!(contacts.note(Protocol::Tcp, first, 80), Contact::Known);
    }

    #[test]
    fn destinations_are_named_as_the_smoke_test_reads_them() {
        let v4 = IpAddr::V4([10, 0, 2, 2]);
        let v6 = IpAddr::V6([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(
            format!("{}", Destination(Protocol::Udp, v4, 123)),
            "udp 10.0.2.2:123"
        );
        assert_eq!(
            format!("{}", Destination(Protocol::Tcp, v6, 443)),
            "tcp [2001:db8::1]:443"
        );
        assert_eq!(
            format!("{}", Destination(Protocol::Ping, v4, 0)),
            "ping 10.0.2.2"
        );
    }
}
