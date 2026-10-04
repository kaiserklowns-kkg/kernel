package gateway

import (
	"bufio"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/netip"
	"strings"
	"testing"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/ai/httpc"
)

func TestParseSettings(t *testing.T) {
	s, err := ParseSettings("https://models.oceans.test:8443/v1 oceans-test --dns 10.0.2.2:5353")
	if err != nil || !s.Endpoint.TLS || s.Endpoint.Name != "models.oceans.test" || s.Model != "oceans-test" ||
		s.DNS != netip.MustParseAddrPort("10.0.2.2:5353") {
		t.Fatalf("%+v %v", s, err)
	}
	s, err = ParseSettings("  http://10.0.2.2:11434/v1   llama3.1:8b ")
	if err != nil || s.DNS.IsValid() || s.Model != "llama3.1:8b" {
		t.Fatalf("%+v %v", s, err)
	}
	for _, bad := range []string{"", "http://h/v1", "http://h/v1 m extra", "http://h/v1 m --dns", "http://h/v1 m --dsn 1.2.3.4",
		"http://h/v1 m --dns name.test", "ftp://h m", "http://h m\"x", "http://h/v1 m --dns 1.2.3.4:0"} {
		if s, err := ParseSettings(bad); err == nil {
			t.Errorf("%q accepted as %+v", bad, s)
		}
	}
}

func TestParseServer(t *testing.T) {
	for text, want := range map[string]string{
		"10.0.2.3": "10.0.2.3:53", "10.0.2.2:5353": "10.0.2.2:5353", "fec0::3": "[fec0::3]:53",
		"[fec0::2]:5300": "[fec0::2]:5300", "::ffff:10.0.2.3": "10.0.2.3:53",
	} {
		got, err := ParseServer(text)
		if err != nil || got != netip.MustParseAddrPort(want) {
			t.Errorf("%s: %v %v", text, got, err)
		}
	}
	for _, bad := range []string{"", "dns.test", "10.0.2.2:", "10.0.2.2:99999", "[fe80::1%1]:53", "fe80::1%1"} {
		if _, err := ParseServer(bad); err == nil {
			t.Errorf("%q accepted", bad)
		}
	}
}

// testPKI is a CA and a server certificate it issued for `names`.
type testPKI struct {
	caPEM  []byte
	server tls.Certificate
}

func newPKI(t *testing.T, names ...string) testPKI {
	t.Helper()
	caKey, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	ca := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "test CA"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		IsCA:                  true,
		BasicConstraintsValid: true,
		KeyUsage:              x509.KeyUsageCertSign,
	}
	caDER, err := x509.CreateCertificate(rand.Reader, ca, ca, &caKey.PublicKey, caKey)
	if err != nil {
		t.Fatal(err)
	}
	ca, _ = x509.ParseCertificate(caDER)
	key, _ := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	leaf := &x509.Certificate{
		SerialNumber: big.NewInt(2),
		Subject:      pkix.Name{CommonName: names[0]},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	for _, name := range names {
		if address, err := netip.ParseAddr(name); err == nil {
			leaf.IPAddresses = append(leaf.IPAddresses, net.IP(address.AsSlice()))
		} else {
			leaf.DNSNames = append(leaf.DNSNames, name)
		}
	}
	leafDER, err := x509.CreateCertificate(rand.Reader, leaf, ca, &key.PublicKey, caKey)
	if err != nil {
		t.Fatal(err)
	}
	return testPKI{
		caPEM:  pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: caDER}),
		server: tls.Certificate{Certificate: [][]byte{leafDER}, PrivateKey: key},
	}
}

func TestParseCertificates(t *testing.T) {
	a, b := newPKI(t, "a.test"), newPKI(t, "b.test")
	bundle := append(append([]byte("# a comment\n"), a.caPEM...), b.caPEM...)
	if _, n, err := ParseCertificates(append(bundle, make([]byte, 100)...)); err != nil || n != 2 {
		t.Fatalf("%d %v", n, err)
	}
	key := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: []byte{1}})
	broken := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: []byte{1, 2, 3}})
	for name, bad := range map[string][]byte{
		"empty":         nil,
		"text":          []byte("hello"),
		"a key":         append(append([]byte(nil), a.caPEM...), key...),
		"not DER":       broken,
		"cut off":       a.caPEM[:len(a.caPEM)-30],
		"cut off after": append(append([]byte(nil), a.caPEM...), b.caPEM[:40]...),
		"too large":     make([]byte, MaxCertificates+1),
	} {
		if _, n, err := ParseCertificates(bad); err == nil {
			t.Errorf("%s: accepted (%d)", name, n)
		}
	}
	roots, _, _ := ParseCertificates(a.caPEM)
	pool, n, err := WithExtra(roots, b.caPEM)
	if err != nil || n != 1 || !pool.Equal(func() *x509.CertPool { p, _, _ := ParseCertificates(bundle); return p }()) {
		t.Fatalf("with extra: %d %v", n, err)
	}
}

// serve answers one HTTP request on `conn` (over TLS with `cert` if set)
// with `reply`, and reports what it read.
func serve(conn net.Conn, cert *tls.Certificate, reply string, got chan<- string) {
	defer conn.Close()
	var c net.Conn = conn
	if cert != nil {
		server := tls.Server(conn, &tls.Config{Certificates: []tls.Certificate{*cert}})
		if err := server.Handshake(); err != nil {
			got <- "handshake: " + err.Error()
			return
		}
		c = server
	}
	request, err := http.ReadRequest(bufio.NewReader(c))
	if err != nil {
		got <- "request: " + err.Error()
		return
	}
	body, _ := io.ReadAll(request.Body)
	got <- fmt.Sprintf("%s %s %s %s", request.Method, request.Host, request.URL.Path, body)
	io.WriteString(c, reply)
}

type fixedResolver []netip.Addr

func (r fixedResolver) Resolve(name string) ([]netip.Addr, error) {
	if name != "models.oceans.test" {
		return nil, errors.New(name + ": not found")
	}
	return r, nil
}

func transport(t *testing.T, url string, roots []byte, cert *tls.Certificate, got chan string) *Transport {
	t.Helper()
	endpoint, err := httpc.ParseEndpoint(url)
	if err != nil {
		t.Fatal(err)
	}
	var pool *x509.CertPool
	if roots != nil {
		pool, _, _ = ParseCertificates(roots)
	}
	return &Transport{
		Endpoint: endpoint,
		Resolver: fixedResolver{netip.MustParseAddr("fec0::2"), netip.MustParseAddr("10.0.2.2")},
		Roots:    pool,
		dial: func(to netip.AddrPort, _ time.Duration) (net.Conn, error) {
			if to.Addr().Is6() {
				return nil, errors.New("no route to host")
			}
			// Loopback TCP (not net.Pipe: its unbuffered writes would
			// deadlock a client that stops reading to send an alert).
			listener, err := net.Listen("tcp", "127.0.0.1:0")
			if err != nil {
				return nil, err
			}
			go func() {
				server, err := listener.Accept()
				listener.Close()
				if err == nil {
					serve(server, cert, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", got)
				}
			}()
			return net.Dial("tcp", listener.Addr().String())
		},
	}
}

func TestPostOverTLS(t *testing.T) {
	pki := newPKI(t, "models.oceans.test")
	got := make(chan string, 1)
	tr := transport(t, "https://models.oceans.test:8443/v1", pki.caPEM, &pki.server, got)
	var traced string
	tr.Trace = func(line string) { traced = line }
	status, body, err := tr.Post("/v1/chat/completions", []byte(`{"a":1}`))
	if err != nil || status != 200 || string(body) != "ok" {
		t.Fatalf("%d %q %v", status, body, err)
	}
	if request := <-got; request != `POST models.oceans.test:8443 /v1/chat/completions {"a":1}` {
		t.Fatalf("the server read %q", request)
	}
	if !strings.HasPrefix(traced, "TLS 1.3 with models.oceans.test in ") {
		t.Fatalf("trace: %q", traced)
	}
}

func TestPostPlain(t *testing.T) {
	got := make(chan string, 1)
	tr := transport(t, "http://10.0.2.2:8080/v1", nil, nil, got)
	status, body, err := tr.Post("/v1/x", nil)
	if err != nil || status != 200 || string(body) != "ok" || <-got != "POST 10.0.2.2:8080 /v1/x " {
		t.Fatalf("%d %q %v", status, body, err)
	}
}

func TestTLSRefusals(t *testing.T) {
	pki, other := newPKI(t, "models.oceans.test"), newPKI(t, "models.oceans.test")
	wrongName := newPKI(t, "elsewhere.test")
	for name, c := range map[string]struct {
		roots []byte
		cert  *tls.Certificate
		want  string
	}{
		"unknown issuer": {other.caPEM, &pki.server, "certificate signed by unknown authority"},
		"another name":   {wrongName.caPEM, &wrongName.server, "not models.oceans.test"},
		"no roots":       {nil, &pki.server, "no trusted root certificates"},
		"not TLS":        {pki.caPEM, nil, "TLS with models.oceans.test"},
	} {
		got := make(chan string, 2)
		tr := transport(t, "https://models.oceans.test/v1", c.roots, c.cert, got)
		tr.HandshakeTimeout = 2 * time.Second
		if _, _, err := tr.Post("/v1/x", nil); err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%s: %v", name, err)
		}
	}
	// A certificate outside its validity.
	got := make(chan string, 2)
	tr := transport(t, "https://models.oceans.test/v1", pki.caPEM, &pki.server, got)
	tr.now = func() time.Time { return time.Now().Add(48 * time.Hour) }
	if _, _, err := tr.Post("/v1/x", nil); err == nil || !strings.Contains(err.Error(), "expired") {
		t.Errorf("expired: %v", err)
	}
}

func TestConnectFailures(t *testing.T) {
	got := make(chan string, 1)
	tr := transport(t, "https://missing.test/v1", nil, nil, got)
	if _, _, err := tr.Post("/v1/x", nil); err == nil || !strings.Contains(err.Error(), "missing.test: not found") {
		t.Errorf("unresolved: %v", err)
	}
	tr = transport(t, "http://[fec0::2]:80/v1", nil, nil, got)
	if _, _, err := tr.Post("/v1/x", nil); err == nil || !strings.Contains(err.Error(), "[fec0::2]:80: no route to host") {
		t.Errorf("unreachable: %v", err)
	}
	tr = transport(t, "http://models.oceans.test/v1", nil, nil, got)
	tr.Resolver = nil
	if _, _, err := tr.Post("/v1/x", nil); err == nil {
		t.Error("a name was used without a resolver")
	}
}
