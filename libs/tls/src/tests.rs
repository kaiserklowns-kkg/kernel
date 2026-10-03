//! Handshakes against a rustls server (using this provider's signing) on
//! the loopback interface: every suite and group, every certificate
//! algorithm, and the failures a client must detect.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::string::{String, ToString};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::vec::Vec;
use std::{format, vec};

use rustls::crypto::{CryptoProvider, GetRandomFailed, SecureRandom};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::{
    CertificateError, CipherSuite, Error, NamedGroup, ProtocolVersion, RootCertStore, ServerConfig,
    ServerConnection, StreamOwned, SupportedProtocolVersion,
};

use crate::{Client, ClientError, Transport, client_config, parse_certificates};

#[derive(Debug)]
struct TestRandom;

static RNG: Mutex<oceans_random::Rng> = Mutex::new(oceans_random::Rng::new());

impl SecureRandom for TestRandom {
    fn fill(&self, out: &mut [u8]) -> Result<(), GetRandomFailed> {
        let mut rng = RNG.lock().unwrap();
        if !rng.is_seeded() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            rng.reseed(&now.as_nanos().to_le_bytes());
        }
        rng.fill(out);
        Ok(())
    }
}

#[derive(Debug)]
struct At(u64);

impl TimeProvider for At {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(Duration::from_secs(self.0)))
    }
}

static TLS13_ONLY: &[&SupportedProtocolVersion] = &[&rustls::version::TLS13];
static TLS12_ONLY: &[&SupportedProtocolVersion] = &[&rustls::version::TLS12];

/// 2027-01-15: inside every test certificate's validity.
const NOW: u64 = 1_800_000_000;

fn provider() -> CryptoProvider {
    crate::provider(&TestRandom)
}

fn testdata(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

struct Tcp(TcpStream);

impl Transport for Tcp {
    type Error = io::Error;

    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.0.write_all(data)
    }

    fn recv(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

struct Server {
    provider: CryptoProvider,
    versions: &'static [&'static SupportedProtocolVersion],
    chain: &'static str,
    body: Vec<u8>,
    close_notify: bool,
}

impl Server {
    fn new() -> Self {
        Self {
            provider: provider(),
            versions: rustls::ALL_VERSIONS,
            chain: "server-ecdsa.der",
            body: b"hello over TLS\n".to_vec(),
            close_notify: true,
        }
    }

    /// Serves one connection: reads a request head, answers with the body.
    fn start(self) -> (u16, JoinHandle<Result<(), String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let config = ServerConfig::builder_with_provider(Arc::new(self.provider))
                .with_protocol_versions(self.versions)
                .map_err(|e| e.to_string())?
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(testdata(self.chain))],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(testdata("server.key.der"))),
                )
                .map_err(|e| e.to_string())?;
            let (socket, _) = listener.accept().map_err(|e| e.to_string())?;
            let connection = ServerConnection::new(Arc::new(config)).map_err(|e| e.to_string())?;
            let mut stream = StreamOwned::new(connection, socket);
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).map_err(|e| e.to_string())? == 0 {
                    return Err("request cut short".into());
                }
                request.push(byte[0]);
            }
            stream.write_all(&self.body).map_err(|e| e.to_string())?;
            if self.close_notify {
                stream.conn.send_close_notify();
            }
            stream.flush().map_err(|e| e.to_string())?;
            Ok(())
        });
        (port, handle)
    }
}

struct Outcome {
    body: Vec<u8>,
    version: ProtocolVersion,
    suite: CipherSuite,
}

fn fetch(port: u16, trust: &str, name: &str, at: u64) -> Result<Outcome, ClientError<io::Error>> {
    let mut roots = RootCertStore::empty();
    for certificate in parse_certificates(&testdata(trust)) {
        roots.add(certificate).unwrap();
    }
    let config = client_config(Arc::new(provider()), Arc::new(At(at)), roots).unwrap();
    let socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let name = ServerName::try_from(name.to_string()).unwrap();
    let mut client = Client::connect(Arc::new(config), name, Tcp(socket))?;
    let (version, suite) = client.describe().unwrap();
    client.write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")?;
    let mut body = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match client.read(&mut buffer)? {
            0 => break,
            len => body.extend_from_slice(&buffer[..len]),
        }
    }
    client.close()?;
    Ok(Outcome {
        body,
        version,
        suite,
    })
}

fn certificate_error(result: Result<Outcome, ClientError<io::Error>>) -> CertificateError {
    match result {
        Err(ClientError::Tls(Error::InvalidCertificate(error))) => error,
        Err(other) => panic!("expected a certificate error, got {other:?}"),
        Ok(_) => panic!("expected a certificate error, got a connection"),
    }
}

#[test]
fn every_tls13_suite_and_group() {
    let suites = [
        CipherSuite::TLS13_AES_128_GCM_SHA256,
        CipherSuite::TLS13_AES_256_GCM_SHA384,
        CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    ];
    let groups = [
        NamedGroup::X25519,
        NamedGroup::secp256r1,
        NamedGroup::secp384r1,
    ];
    for suite in suites {
        for group in groups {
            let mut server = Server::new();
            server.provider.cipher_suites.retain(|s| s.suite() == suite);
            server.provider.kx_groups.retain(|g| g.name() == group);
            server.versions = TLS13_ONLY;
            let (port, handle) = server.start();
            let outcome = fetch(port, "ecdsa-ca.der", "127.0.0.1", NOW).unwrap();
            assert_eq!(outcome.body, b"hello over TLS\n", "{suite:?} {group:?}");
            assert_eq!(outcome.version, ProtocolVersion::TLSv1_3);
            assert_eq!(outcome.suite, suite);
            handle.join().unwrap().unwrap();
        }
    }
}

#[test]
fn every_tls12_ecdsa_suite() {
    let suites = [
        CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    ];
    for suite in suites {
        let mut server = Server::new();
        server.provider.cipher_suites.retain(|s| s.suite() == suite);
        server.versions = TLS12_ONLY;
        let (port, handle) = server.start();
        let outcome = fetch(port, "ecdsa-ca.der", "localhost", NOW).unwrap();
        assert_eq!(outcome.body, b"hello over TLS\n");
        assert_eq!(outcome.version, ProtocolVersion::TLSv1_2);
        assert_eq!(outcome.suite, suite);
        handle.join().unwrap().unwrap();
    }
}

#[test]
fn certificates_of_every_algorithm() {
    for name in ["ecdsa", "rsa", "pss", "ed25519"] {
        let mut server = Server::new();
        server.chain = Box::leak(format!("server-{name}.der").into_boxed_str());
        let (port, handle) = server.start();
        let outcome = fetch(port, &format!("{name}-ca.pem"), "oceans.test", NOW)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(outcome.body, b"hello over TLS\n");
        handle.join().unwrap().unwrap();
    }
}

#[test]
fn a_large_body_arrives_whole() {
    let mut server = Server::new();
    server.body = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
    let expected = server.body.clone();
    let (port, handle) = server.start();
    let outcome = fetch(port, "ecdsa-ca.der", "127.0.0.1", NOW).unwrap();
    assert!(outcome.body == expected);
    handle.join().unwrap().unwrap();
}

#[test]
fn an_unknown_issuer_is_refused() {
    let (port, _server) = Server::new().start();
    let error = certificate_error(fetch(port, "rsa-ca.der", "127.0.0.1", NOW));
    assert_eq!(error, CertificateError::UnknownIssuer);
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let (port, _server) = Server::new().start();
    let error = certificate_error(fetch(port, "ecdsa-ca.der", "example.com", NOW));
    assert!(
        matches!(
            error,
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn certificates_outside_their_validity_are_refused() {
    let (port, _server) = Server::new().start();
    let error = certificate_error(fetch(port, "ecdsa-ca.der", "127.0.0.1", 2_200_000_000));
    assert!(
        matches!(
            error,
            CertificateError::Expired | CertificateError::ExpiredContext { .. }
        ),
        "{error:?}"
    );
    let (port, _server) = Server::new().start();
    let error = certificate_error(fetch(port, "ecdsa-ca.der", "127.0.0.1", 1_700_000_000));
    assert!(
        matches!(
            error,
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn a_close_without_close_notify_is_truncation() {
    let mut server = Server::new();
    server.close_notify = false;
    let (port, handle) = server.start();
    let result = fetch(port, "ecdsa-ca.der", "127.0.0.1", NOW);
    assert!(
        matches!(result, Err(ClientError::Truncated)),
        "{:?}",
        result.err()
    );
    handle.join().unwrap().unwrap();
}

#[test]
fn pem_and_der_certificates_parse() {
    assert_eq!(parse_certificates(&testdata("ecdsa-ca.pem")).len(), 1);
    assert_eq!(parse_certificates(&testdata("ecdsa-ca.der")).len(), 1);
    let both = [testdata("ecdsa-ca.pem"), testdata("rsa-ca.pem")].concat();
    assert_eq!(parse_certificates(&both).len(), 2);
}

#[test]
fn the_web_roots_load() {
    assert!(crate::web_roots().len() > 100);
}
