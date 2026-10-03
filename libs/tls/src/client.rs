//! A blocking TLS client over any byte transport, driving rustls's
//! unbuffered (no_std) connection.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use rustls::ClientConfig;
use rustls::client::UnbufferedClientConnection;
use rustls::pki_types::ServerName;
use rustls::unbuffered::{
    ConnectionState, EncodeError, EncryptError, InsufficientSizeError, UnbufferedStatus,
};

/// A reliable byte stream, such as a TCP connection.
pub trait Transport {
    type Error;
    /// Sends all of `data`.
    fn send(&mut self, data: &[u8]) -> Result<(), Self::Error>;
    /// Waits for at least one byte; returns how many were received, 0 when
    /// the peer has closed its side.
    fn recv(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error>;
}

#[derive(Debug)]
pub enum Error<E> {
    Transport(E),
    Tls(rustls::Error),
    /// The connection ended without a TLS close: the data may have been
    /// cut short by an attacker.
    Truncated,
    /// Records larger than the client buffers.
    TooLarge,
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(f),
            Self::Tls(error) => write!(f, "TLS: {error}"),
            Self::Truncated => f.write_str("TLS: connection closed without close_notify"),
            Self::TooLarge => f.write_str("TLS: message too large"),
        }
    }
}

/// Room for one full record (RFC 8446 §5.2: 2^14 + 256 bytes of
/// ciphertext, plus the header).
const RECORD: usize = 16_384 + 256 + 5;
/// Handshake messages may span records and rustls needs each one whole;
/// certificate chains are the largest.
const MAX_INCOMING: usize = 256 * 1024;
/// Plaintext per record when sending.
const MAX_FRAGMENT: usize = 16_384;

enum Goal<'a> {
    Handshake,
    Write(&'a [u8]),
    Read,
    Close,
}

pub struct Client<T> {
    connection: UnbufferedClientConnection,
    transport: T,
    /// Received TLS bytes; `filled` of them are valid.
    incoming: Vec<u8>,
    filled: usize,
    /// Encoded TLS bytes waiting to be sent.
    outgoing: Vec<u8>,
    pending: usize,
    /// Decrypted application data; `consumed` of it has been read.
    plain: Vec<u8>,
    consumed: usize,
    peer_closed: bool,
}

impl<T: Transport> Client<T> {
    /// Connects: completes the handshake, including certificate checks,
    /// over `transport`.
    pub fn connect(
        config: Arc<ClientConfig>,
        name: ServerName<'static>,
        transport: T,
    ) -> Result<Self, Error<T::Error>> {
        let connection = UnbufferedClientConnection::new(config, name).map_err(Error::Tls)?;
        let mut client = Self {
            connection,
            transport,
            incoming: vec![0; RECORD],
            filled: 0,
            outgoing: vec![0; RECORD],
            pending: 0,
            plain: Vec::new(),
            consumed: 0,
            peer_closed: false,
        };
        client.pump(Goal::Handshake)?;
        Ok(client)
    }

    /// The negotiated protocol version and cipher suite, for display.
    pub fn describe(&self) -> Option<(rustls::ProtocolVersion, rustls::CipherSuite)> {
        let version = self.connection.protocol_version()?;
        let suite = self.connection.negotiated_cipher_suite()?;
        Some((version, suite.suite()))
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<(), Error<T::Error>> {
        for chunk in data.chunks(MAX_FRAGMENT) {
            self.pump(Goal::Write(chunk))?;
        }
        Ok(())
    }

    /// Reads decrypted data; 0 once the server has closed cleanly.
    /// [`Error::Truncated`] if the connection ended without a TLS close.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Error<T::Error>> {
        if self.consumed == self.plain.len() {
            self.plain.clear();
            self.consumed = 0;
            self.pump(Goal::Read)?;
        }
        let len = buffer.len().min(self.plain.len() - self.consumed);
        buffer[..len].copy_from_slice(&self.plain[self.consumed..self.consumed + len]);
        self.consumed += len;
        Ok(len)
    }

    /// Sends a TLS close and returns the transport.
    pub fn close(mut self) -> Result<T, Error<T::Error>> {
        self.pump(Goal::Close)?;
        Ok(self.transport)
    }

    /// Runs the connection until `goal` is reached.
    fn pump(&mut self, goal: Goal<'_>) -> Result<(), Error<T::Error>> {
        loop {
            let UnbufferedStatus { discard, state } = self
                .connection
                .process_tls_records(&mut self.incoming[..self.filled]);
            let mut done = false;
            let mut receive = false;
            match state.map_err(Error::Tls)? {
                ConnectionState::ReadTraffic(mut traffic) => {
                    while let Some(record) = traffic.next_record() {
                        let record = record.map_err(Error::Tls)?;
                        self.plain.extend_from_slice(record.payload);
                    }
                }
                ConnectionState::EncodeTlsData(mut data) => loop {
                    match data.encode(&mut self.outgoing[self.pending..]) {
                        Ok(len) => {
                            self.pending += len;
                            break;
                        }
                        Err(EncodeError::InsufficientSize(InsufficientSizeError {
                            required_size,
                        })) => {
                            self.outgoing.resize(self.pending + required_size, 0);
                        }
                        Err(EncodeError::AlreadyEncoded) => break,
                    }
                },
                ConnectionState::TransmitTlsData(data) => {
                    self.transport
                        .send(&self.outgoing[..self.pending])
                        .map_err(Error::Transport)?;
                    self.pending = 0;
                    data.done();
                }
                ConnectionState::BlockedHandshake => receive = true,
                ConnectionState::PeerClosed | ConnectionState::Closed => {
                    self.peer_closed = true;
                    match goal {
                        Goal::Read | Goal::Close => done = true,
                        Goal::Handshake | Goal::Write(_) => return Err(Error::Truncated),
                    }
                }
                ConnectionState::WriteTraffic(mut traffic) => match goal {
                    Goal::Handshake => done = true,
                    Goal::Read if !self.plain.is_empty() || self.peer_closed => done = true,
                    Goal::Read => receive = true,
                    Goal::Write(data) => {
                        let len = loop {
                            match traffic.encrypt(data, &mut self.outgoing[self.pending..]) {
                                Ok(len) => break len,
                                Err(EncryptError::InsufficientSize(InsufficientSizeError {
                                    required_size,
                                })) => {
                                    self.outgoing.resize(self.pending + required_size, 0);
                                }
                                Err(EncryptError::EncryptExhausted) => {
                                    return Err(Error::Tls(rustls::Error::EncryptError));
                                }
                            }
                        };
                        self.pending += len;
                        self.transport
                            .send(&self.outgoing[..self.pending])
                            .map_err(Error::Transport)?;
                        self.pending = 0;
                        done = true;
                    }
                    Goal::Close => {
                        let len = loop {
                            match traffic.queue_close_notify(&mut self.outgoing[self.pending..]) {
                                Ok(len) => break len,
                                Err(EncryptError::InsufficientSize(InsufficientSizeError {
                                    required_size,
                                })) => {
                                    self.outgoing.resize(self.pending + required_size, 0);
                                }
                                Err(EncryptError::EncryptExhausted) => {
                                    return Err(Error::Tls(rustls::Error::EncryptError));
                                }
                            }
                        };
                        self.pending += len;
                        self.transport
                            .send(&self.outgoing[..self.pending])
                            .map_err(Error::Transport)?;
                        self.pending = 0;
                        done = true;
                    }
                },
                // Early data is a server-side state; other states are
                // future additions.
                _ => receive = true,
            }
            if discard > 0 {
                self.incoming.copy_within(discard..self.filled, 0);
                self.filled -= discard;
            }
            if done {
                return Ok(());
            }
            if receive {
                self.receive()?;
            }
        }
    }

    fn receive(&mut self) -> Result<(), Error<T::Error>> {
        if self.filled == self.incoming.len() {
            if self.incoming.len() >= MAX_INCOMING {
                return Err(Error::TooLarge);
            }
            self.incoming.resize(self.incoming.len() * 2, 0);
        }
        let len = self
            .transport
            .recv(&mut self.incoming[self.filled..])
            .map_err(Error::Transport)?;
        if len == 0 {
            return Err(Error::Truncated);
        }
        self.filled += len;
        Ok(())
    }
}
