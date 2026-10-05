// Package gateway connects the AI runtime's model gateway to model servers
// (ADR-0054): it resolves the server's name over the Oceans socket
// protocol, connects over Oceans TCP, speaks TLS (Go's crypto/tls) for
// https:// endpoints against the system's trusted roots plus any CA the
// user added, and posts with httpc.
package gateway

import (
	"bytes"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"strconv"
	"strings"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/ai/httpc"
	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/dns"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/tcp"
)

// Settings is a model server configuration (`CONFIGURE`): `URL MODEL
// [--dns SERVER]`.
type Settings struct {
	Endpoint httpc.Endpoint
	Model    string
	// DNS is the server that resolves the endpoint's name; the zero
	// value: the one the network service reports.
	DNS netip.AddrPort
}

// Usage is how CONFIGURE's text is written.
const Usage = "usage: URL MODEL [--dns SERVER[:PORT]]"

// ParseSettings parses CONFIGURE's text.
func ParseSettings(text string) (Settings, error) {
	fields := strings.Fields(text)
	if len(fields) != 2 && len(fields) != 4 {
		return Settings{}, errors.New(Usage)
	}
	endpoint, err := httpc.ParseEndpoint(fields[0])
	if err != nil {
		return Settings{}, err
	}
	if !validModel(fields[1]) {
		return Settings{}, fmt.Errorf("%q is not a model name", fields[1])
	}
	s := Settings{Endpoint: endpoint, Model: fields[1]}
	if len(fields) == 4 {
		if fields[2] != "--dns" {
			return Settings{}, errors.New(Usage)
		}
		if s.DNS, err = ParseServer(fields[3]); err != nil {
			return Settings{}, err
		}
	}
	return s, nil
}

// validModel: a model name as servers spell them ("llama3.1:8b",
// "org/model-v2"), printable ASCII without quotes or backslashes.
func validModel(name string) bool {
	if len(name) > 128 {
		return false
	}
	for _, c := range []byte(name) {
		if c <= ' ' || c > '~' || c == '"' || c == '\\' {
			return false
		}
	}
	return name != ""
}

// ParseServer parses a DNS server: an IPv4 or IPv6 address, with a port
// (`10.0.2.2:5353`, `[fec0::2]:53`) or without (port 53).
func ParseServer(text string) (netip.AddrPort, error) {
	if address, err := netip.ParseAddr(text); err == nil && address.Zone() == "" {
		return netip.AddrPortFrom(address.Unmap(), dns.Port), nil
	}
	server, err := netip.ParseAddrPort(text)
	if err != nil || server.Addr().Zone() != "" || server.Port() == 0 {
		return netip.AddrPort{}, fmt.Errorf("%q is not a DNS server address", text)
	}
	return netip.AddrPortFrom(server.Addr().Unmap(), server.Port()), nil
}

// MaxCertificates bounds a PEM bundle (the system's roots, or a CA the
// user added).
const MaxCertificates = 1 << 20

// ParseCertificates reads a PEM bundle: every block must be a
// CERTIFICATE that parses (text between blocks is allowed, as bundles
// carry comments; zeros after the last block are a memory object's
// padding). It returns the pool and how many certificates it holds.
func ParseCertificates(bundle []byte) (*x509.CertPool, int, error) {
	pool := x509.NewCertPool()
	n, err := addCertificates(pool, bundle)
	return pool, n, err
}

func addCertificates(pool *x509.CertPool, bundle []byte) (int, error) {
	if len(bundle) > MaxCertificates {
		return 0, errors.New("the certificate file is too large")
	}
	rest := bytes.TrimRight(bundle, "\x00")
	count := 0
	for {
		var block *pem.Block
		block, rest = pem.Decode(rest)
		if block == nil {
			break
		}
		if block.Type != "CERTIFICATE" {
			return 0, fmt.Errorf("a %s block is not a certificate", block.Type)
		}
		certificate, err := x509.ParseCertificate(block.Bytes)
		if err != nil {
			return 0, fmt.Errorf("certificate %d: %w", count+1, err)
		}
		pool.AddCert(certificate)
		count++
	}
	if strings.Contains(string(rest), "-----BEGIN") {
		return 0, errors.New("a certificate is cut off or malformed")
	}
	if count == 0 {
		return 0, errors.New("no certificates in PEM form")
	}
	return count, nil
}

// WithExtra returns `roots` plus the certificates of `bundle`.
func WithExtra(roots *x509.CertPool, bundle []byte) (*x509.CertPool, int, error) {
	pool := x509.NewCertPool()
	if roots != nil {
		pool = roots.Clone()
	}
	n, err := addCertificates(pool, bundle)
	return pool, n, err
}

// Time limits.
const (
	DefaultConnectTimeout = 10 * time.Second
	// DefaultHandshakeTimeout: interpreted Go is slow at public-key
	// cryptography (ADR-0054 measures it).
	DefaultHandshakeTimeout = 30 * time.Second
	// DefaultIdleTimeout bounds each wait for the server while sending
	// and reading (a local model may think long before answering).
	DefaultIdleTimeout = 120 * time.Second
)

// Resolver returns a name's addresses in the order to try (a
// dns.Resolver on Oceans).
type Resolver interface {
	Resolve(name string) ([]netip.Addr, error)
}

// Transport posts to an endpoint (model.Poster).
type Transport struct {
	Net      oceans.Handle
	Endpoint httpc.Endpoint
	// Resolver resolves the endpoint's name (unused for address
	// literals).
	Resolver Resolver
	// Roots: the certificates trusted for https:// (nil: none).
	Roots *x509.CertPool
	// Time limits (zero: the defaults).
	ConnectTimeout, HandshakeTimeout, IdleTimeout time.Duration
	// Trace, if set, hears about each TLS handshake.
	Trace func(string)

	// dial connects (tests replace it; nil: Oceans TCP).
	dial func(to netip.AddrPort, timeout time.Duration) (net.Conn, error)
	// now is the clock certificates are checked at (nil: time.Now).
	now func() time.Time
}

// Post implements model.Poster: one request per connection.
func (t *Transport) Post(path string, body []byte) (int, []byte, error) {
	conn, err := t.connect()
	if err != nil {
		return 0, nil, err
	}
	defer conn.Close()
	return httpc.Post(idle{conn, or(t.IdleTimeout, DefaultIdleTimeout)}, t.Endpoint.Host, path, body)
}

// Get fetches `path`, a response of up to `limit` bytes.
func (t *Transport) Get(path string, limit int) (int, []byte, error) {
	conn, err := t.connect()
	if err != nil {
		return 0, nil, err
	}
	defer conn.Close()
	return httpc.Get(idle{conn, or(t.IdleTimeout, DefaultIdleTimeout)}, t.Endpoint.Host, path, limit)
}

// sharedBuffer: the size of the buffer each connection reads through.
const sharedBuffer = 256 << 10

func or(d, fallback time.Duration) time.Duration {
	if d > 0 {
		return d
	}
	return fallback
}

// connect resolves, connects (each address in turn) and, for https,
// completes the TLS handshake.
func (t *Transport) connect() (net.Conn, error) {
	e := t.Endpoint
	addresses := []netip.Addr{e.Address}
	if !e.Address.IsValid() {
		if t.Resolver == nil {
			return nil, errors.New("names cannot be resolved here")
		}
		var err error
		if addresses, err = t.Resolver.Resolve(e.Name); err != nil {
			return nil, err
		}
	}
	dial := t.dial
	if dial == nil {
		dial = func(to netip.AddrPort, timeout time.Duration) (net.Conn, error) {
			c, err := tcp.DialAddr(t.Net, to, timeout)
			if err != nil {
				return nil, err
			}
			// Deadlines (idle, the handshake's) bound the waits.
			c.Timeout = time.Hour
			// Bulk reads (a package from the Store) through a shared
			// buffer; without one, a message's worth at a time.
			_ = c.UseSharedBuffer(sharedBuffer)
			return oceansConn{c}, nil
		}
	}
	var conn net.Conn
	var failures []string
	for _, address := range addresses {
		to := netip.AddrPortFrom(address, e.Port)
		c, err := dial(to, or(t.ConnectTimeout, DefaultConnectTimeout))
		if err == nil {
			conn = c
			break
		}
		failures = append(failures, to.String()+": "+err.Error())
	}
	if conn == nil {
		return nil, fmt.Errorf("cannot connect to %s (%s)", e.Name, strings.Join(failures, "; "))
	}
	if !e.TLS {
		return conn, nil
	}
	if t.Roots == nil {
		conn.Close()
		return nil, errors.New("no trusted root certificates for https")
	}
	config := &tls.Config{
		ServerName: e.Name,
		RootCAs:    t.Roots,
		MinVersion: tls.VersionTLS12,
		Time:       t.now,
	}
	secure := tls.Client(conn, config)
	started := time.Now()
	_ = conn.SetDeadline(started.Add(or(t.HandshakeTimeout, DefaultHandshakeTimeout)))
	if err := secure.Handshake(); err != nil {
		conn.Close()
		return nil, fmt.Errorf("TLS with %s: %w", e.Name, err)
	}
	_ = conn.SetDeadline(time.Time{})
	if t.Trace != nil {
		state := secure.ConnectionState()
		t.Trace(fmt.Sprintf("%s with %s in %s ms (%s, %s)", tls.VersionName(state.Version), e.Name,
			strconv.FormatInt(time.Since(started).Milliseconds(), 10),
			tls.CipherSuiteName(state.CipherSuite), state.CurveID))
	}
	return secure, nil
}

// idle bounds each read and write with a deadline `limit` ahead.
type idle struct {
	net.Conn
	limit time.Duration
}

func (c idle) Read(b []byte) (int, error) {
	_ = c.SetReadDeadline(time.Now().Add(c.limit))
	return c.Conn.Read(b)
}

func (c idle) Write(b []byte) (int, error) {
	_ = c.SetWriteDeadline(time.Now().Add(c.limit))
	return c.Conn.Write(b)
}

// oceansConn is an Oceans TCP connection as a net.Conn (for crypto/tls).
type oceansConn struct {
	*tcp.Conn
}

func (c oceansConn) LocalAddr() net.Addr { return &net.TCPAddr{} }

func (c oceansConn) RemoteAddr() net.Addr { return net.TCPAddrFromAddrPort(c.Conn.RemoteAddr()) }

var _ net.Conn = oceansConn{}
