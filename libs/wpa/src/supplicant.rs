//! The client side of the 4-way and group key handshakes (802.11-2020
//! §12.7.6, §12.7.7), as a state machine fed EAPOL-Key frames.

use crate::eapol::{self, Key, info};
use crate::{MacAddress, Ptk, RSN_ELEMENT};

/// Keys to install in the chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keys {
    /// The pairwise temporal key: unicast frames.
    pub pairwise: Option<[u8; 16]>,
    /// The group key and its ID: broadcast and multicast frames.
    pub group: Option<(u8, [u8; 16])>,
}

/// What to do after a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing (the frame was not for us, or was refused).
    Ignore,
    /// Send the frame written in the output buffer, this long.
    Send(usize),
    /// Send it, then install the keys: the link is protected from now.
    SendAndInstall(usize, Keys),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for message 1.
    Start,
    /// Message 2 sent; waiting for message 3 with this ANonce.
    Sent2 { anonce: [u8; 32] },
    /// Keys installed (from the message 3 with this ANonce); group rekeys
    /// may come.
    Done { anonce: [u8; 32] },
}

/// One association's handshakes.
pub struct Supplicant {
    pmk: [u8; 32],
    aa: MacAddress,
    spa: MacAddress,
    snonce: [u8; 32],
    ptk: Option<Ptk>,
    state: State,
    /// The highest replay counter accepted with a valid MIC.
    replay: Option<u64>,
}

impl Supplicant {
    /// For the access point `aa` and our address `spa`, with our nonce
    /// `snonce` (random, fresh for each association).
    pub fn new(pmk: [u8; 32], aa: MacAddress, spa: MacAddress, snonce: [u8; 32]) -> Self {
        Self {
            pmk,
            aa,
            spa,
            snonce,
            ptk: None,
            state: State::Start,
            replay: None,
        }
    }

    pub fn done(&self) -> bool {
        matches!(self.state, State::Done { .. })
    }

    /// Handles an EAPOL-Key frame from the access point; a reply goes into
    /// `out`.
    pub fn receive(&mut self, frame: &[u8], out: &mut [u8]) -> Action {
        let Some(key) = Key::parse(frame) else {
            return Action::Ignore;
        };
        if key.info & info::VERSION_MASK != info::VERSION_HMAC_SHA1_AES
            || key.info & (info::ERROR | info::REQUEST) != 0
            || key.info & info::ACK == 0
        {
            return Action::Ignore;
        }
        // A replayed or old frame is never acted on.
        if self.replay.is_some_and(|last| key.replay <= last) {
            return Action::Ignore;
        }
        let pairwise = key.info & info::PAIRWISE != 0;
        match (pairwise, key.info & info::MIC != 0) {
            (true, false) => self.message1(&key, out),
            (true, true) => self.message3(frame, &key, out),
            (false, true) => self.group1(frame, &key, out),
            (false, false) => Action::Ignore,
        }
    }

    /// Message 1: the ANonce. Derive the keys, answer with message 2.
    fn message1(&mut self, key: &Key<'_>, out: &mut [u8]) -> Action {
        // A new message 1 restarts the handshake (the access point may
        // have missed message 2), but never after keys are installed.
        if matches!(self.state, State::Done { .. }) {
            return Action::Ignore;
        }
        let ptk = Ptk::derive(&self.pmk, &self.aa, &self.spa, &key.nonce, &self.snonce);
        let info = info::VERSION_HMAC_SHA1_AES | info::PAIRWISE | info::MIC;
        let Some(len) = eapol::build(out, info, key.replay, &self.snonce, &RSN_ELEMENT) else {
            return Action::Ignore;
        };
        eapol::sign(&ptk.kck, &mut out[..len]);
        self.ptk = Some(ptk);
        self.state = State::Sent2 { anonce: key.nonce };
        Action::Send(len)
    }

    /// Message 3: the same ANonce, a valid MIC, the group key inside.
    /// Answer with message 4 and install both keys. A retransmitted
    /// message 3 (our message 4 lost) is answered again, and installs
    /// nothing: a key in use is never installed again, which would restart
    /// its packet numbers (the key reinstallation attack, KRACK).
    fn message3(&mut self, frame: &[u8], key: &Key<'_>, out: &mut [u8]) -> Action {
        let (anonce, installed) = match self.state {
            State::Sent2 { anonce } => (anonce, false),
            State::Done { anonce } => (anonce, true),
            State::Start => return Action::Ignore,
        };
        let Some(ptk) = self.ptk else {
            return Action::Ignore;
        };
        let wanted = info::INSTALL | info::SECURE | info::ENCRYPTED;
        if key.nonce != anonce || key.info & wanted != wanted || !eapol::verify(&ptk.kck, frame) {
            return Action::Ignore;
        }
        if installed {
            self.replay = Some(key.replay);
            let info = info::VERSION_HMAC_SHA1_AES | info::PAIRWISE | info::MIC | info::SECURE;
            let Some(len) = eapol::build(out, info, key.replay, &[0; 32], &[]) else {
                return Action::Ignore;
            };
            eapol::sign(&ptk.kck, &mut out[..len]);
            return Action::Send(len);
        }
        let mut plain = [0u8; 512];
        let Some(group) =
            eapol::unwrap(&ptk.kek, key.data, &mut plain).and_then(|len| eapol::gtk(&plain[..len]))
        else {
            return Action::Ignore;
        };
        self.replay = Some(key.replay);
        let info = info::VERSION_HMAC_SHA1_AES | info::PAIRWISE | info::MIC | info::SECURE;
        let Some(len) = eapol::build(out, info, key.replay, &[0; 32], &[]) else {
            return Action::Ignore;
        };
        eapol::sign(&ptk.kck, &mut out[..len]);
        self.state = State::Done { anonce };
        Action::SendAndInstall(
            len,
            Keys {
                pairwise: Some(ptk.tk),
                group: Some(group),
            },
        )
    }

    /// A group rekey: a new group key; answer and install it.
    fn group1(&mut self, frame: &[u8], key: &Key<'_>, out: &mut [u8]) -> Action {
        let (State::Done { .. }, Some(ptk)) = (self.state, self.ptk) else {
            return Action::Ignore;
        };
        let wanted = info::SECURE | info::ENCRYPTED;
        if key.info & wanted != wanted || !eapol::verify(&ptk.kck, frame) {
            return Action::Ignore;
        }
        let mut plain = [0u8; 512];
        let Some(group) =
            eapol::unwrap(&ptk.kek, key.data, &mut plain).and_then(|len| eapol::gtk(&plain[..len]))
        else {
            return Action::Ignore;
        };
        self.replay = Some(key.replay);
        let info = info::VERSION_HMAC_SHA1_AES | info::MIC | info::SECURE;
        let Some(len) = eapol::build(out, info, key.replay, &[0; 32], &[]) else {
            return Action::Ignore;
        };
        eapol::sign(&ptk.kck, &mut out[..len]);
        Action::SendAndInstall(
            len,
            Keys {
                pairwise: None,
                group: Some(group),
            },
        )
    }
}
