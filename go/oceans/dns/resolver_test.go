package dns

import (
	"encoding/binary"
	"errors"
	"net/netip"
	"testing"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans/netproto"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/udp"
)

// fakeConn answers queries through `answer`, which sees every query sent
// and returns the datagrams to deliver (none: silence).
type fakeConn struct {
	sent    [][]byte
	inbox   []udp.Datagram
	answer  func(to netip.AddrPort, query []byte, n int) []udp.Datagram
	closed  bool
	waited  time.Duration
	sendErr error
}

func (c *fakeConn) SendTo(to netip.AddrPort, payload []byte) error {
	if c.sendErr != nil {
		return c.sendErr
	}
	c.sent = append(c.sent, payload)
	c.inbox = append(c.inbox, c.answer(to, payload, len(c.sent))...)
	return nil
}

func (c *fakeConn) Receive(timeout time.Duration) (udp.Datagram, error) {
	if len(c.inbox) == 0 {
		c.waited += timeout
		return udp.Datagram{}, netproto.ErrTimeout
	}
	d := c.inbox[0]
	c.inbox = c.inbox[1:]
	return d, nil
}

func (c *fakeConn) Close() error { c.closed = true; return nil }

var server = netip.MustParseAddrPort("10.0.2.2:5353")

// answerWith answers A queries with `a` and AAAA queries with `aaaa`
// (invalid: no record; both invalid: NXDOMAIN).
func answerWith(a, aaaa netip.Addr) func(netip.AddrPort, []byte, int) []udp.Datagram {
	return func(to netip.AddrPort, query []byte, _ int) []udp.Datagram {
		qtype := binary.BigEndian.Uint16(query[len(query)-4:])
		var answers [][]byte
		switch {
		case qtype == 1 && a.IsValid():
			b := a.As4()
			answers = append(answers, record(questionName, 1, 60, b[:]))
		case qtype == 28 && aaaa.IsValid():
			b := aaaa.As16()
			answers = append(answers, record(questionName, 28, 60, b[:]))
		}
		rcode := uint16(0)
		if !a.IsValid() && !aaaa.IsValid() {
			rcode = 3
		}
		return []udp.Datagram{{From: to, Payload: response(query, rcode, answers...)}}
	}
}

func resolver(c *fakeConn, reach Reach) *Resolver {
	return &Resolver{
		Server:  server,
		Timeout: 10 * time.Millisecond,
		open:    func() (conn, error) { return c, nil },
		reach:   func() (Reach, error) { return reach, nil },
	}
}

func TestResolveLiteralsNeedNoQuery(t *testing.T) {
	r := &Resolver{reach: func() (Reach, error) { t.Fatal("asked the stack"); return Reach{}, nil }}
	for text, want := range map[string]string{"10.0.2.2": "10.0.2.2", "fec0::2": "fec0::2", "::ffff:1.2.3.4": "1.2.3.4"} {
		got, err := r.Resolve(text)
		if err != nil || len(got) != 1 || got[0] != netip.MustParseAddr(want) {
			t.Errorf("%s: %v %v", text, got, err)
		}
	}
}

func TestResolveName(t *testing.T) {
	c := &fakeConn{answer: answerWith(netip.MustParseAddr("10.0.2.2"), netip.MustParseAddr("fec0::2"))}
	got, err := resolver(c, Reach{V4: true}).Resolve("models.oceans.test")
	if err != nil || len(got) != 1 || got[0] != netip.MustParseAddr("10.0.2.2") {
		t.Fatalf("IPv4 only: %v %v", got, err)
	}
	if len(c.sent) != 1 || !c.closed {
		t.Fatalf("sent %d queries, closed %v", len(c.sent), c.closed)
	}
	c = &fakeConn{answer: answerWith(netip.MustParseAddr("10.0.2.2"), netip.MustParseAddr("fec0::2"))}
	got, err = resolver(c, Reach{V4: true, V6: true}).Resolve("models.oceans.test")
	if err != nil || len(got) != 2 || got[0] != netip.MustParseAddr("fec0::2") || got[1] != netip.MustParseAddr("10.0.2.2") {
		t.Fatalf("both, IPv6 first: %v %v", got, err)
	}
	c = &fakeConn{answer: answerWith(netip.Addr{}, netip.MustParseAddr("fec0::2"))}
	got, err = resolver(c, Reach{V4: true, V6: true}).Resolve("v6.test")
	if err != nil || len(got) != 1 || got[0] != netip.MustParseAddr("fec0::2") {
		t.Fatalf("AAAA only: %v %v", got, err)
	}
}

func TestResolveFailures(t *testing.T) {
	c := &fakeConn{answer: answerWith(netip.Addr{}, netip.Addr{})}
	if _, err := resolver(c, Reach{V4: true}).Resolve("missing.test"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("NXDOMAIN: %v", err)
	}
	c = &fakeConn{answer: answerWith(netip.Addr{}, netip.MustParseAddr("::1"))}
	if _, err := resolver(c, Reach{V4: true}).Resolve("v6only.test"); !errors.Is(err, ErrNoAddress) {
		t.Fatalf("no usable address: %v", err)
	}
	if _, err := resolver(c, Reach{V4: true}).Resolve("bad name"); !errors.Is(err, ErrBadName) {
		t.Fatalf("bad name: %v", err)
	}
	r := resolver(c, Reach{V4: true})
	r.Server = netip.AddrPort{}
	if _, err := r.Resolve("a.test"); !errors.Is(err, ErrNoServer) {
		t.Fatalf("no server: %v", err)
	}
	c = &fakeConn{sendErr: netproto.StatusNotConfigured}
	if _, err := resolver(c, Reach{V4: true}).Resolve("a.test"); !errors.Is(err, netproto.StatusNotConfigured) {
		t.Fatalf("network error: %v", err)
	}
}

func TestResolveRetriesAndTimesOut(t *testing.T) {
	// Silent twice, then answers: the third try succeeds.
	inner := answerWith(netip.MustParseAddr("192.0.2.1"), netip.Addr{})
	c := &fakeConn{answer: func(to netip.AddrPort, q []byte, n int) []udp.Datagram {
		if n < 3 {
			return nil
		}
		return inner(to, q, n)
	}}
	got, err := resolver(c, Reach{V4: true}).Resolve("slow.test")
	if err != nil || len(got) != 1 || len(c.sent) != 3 {
		t.Fatalf("%v %v after %d queries", got, err, len(c.sent))
	}
	// Never answers: DefaultTries queries, then ErrNoAnswer.
	c = &fakeConn{answer: func(netip.AddrPort, []byte, int) []udp.Datagram { return nil }}
	if _, err := resolver(c, Reach{V4: true}).Resolve("silent.test"); !errors.Is(err, ErrNoAnswer) || len(c.sent) != DefaultTries {
		t.Fatalf("%v after %d queries", err, len(c.sent))
	}
}

func TestResolveIgnoresForgeries(t *testing.T) {
	inner := answerWith(netip.MustParseAddr("192.0.2.1"), netip.Addr{})
	c := &fakeConn{answer: func(to netip.AddrPort, q []byte, n int) []udp.Datagram {
		real := inner(to, q, n)[0]
		forged := real
		forged.From = netip.MustParseAddrPort("10.0.2.99:5353")
		forged.Payload = response(q, 0, record(questionName, 1, 60, []byte{6, 6, 6, 6}))
		wrongID := udp.Datagram{From: to, Payload: append([]byte(nil), forged.Payload...)}
		wrongID.Payload[0] ^= 0xff
		garbage := udp.Datagram{From: to, Payload: []byte{1, 2, 3}}
		return []udp.Datagram{forged, wrongID, garbage, real}
	}}
	got, err := resolver(c, Reach{V4: true}).Resolve("a.test")
	if err != nil || len(got) != 1 || got[0] != netip.MustParseAddr("192.0.2.1") {
		t.Fatalf("%v %v", got, err)
	}
}

func TestResolveRefusesTruncatedAnswers(t *testing.T) {
	c := &fakeConn{answer: func(to netip.AddrPort, q []byte, _ int) []udp.Datagram {
		return []udp.Datagram{{From: to, Payload: response(q, 0)[:udp.MaxData6/8], Truncated: true}}
	}}
	if _, err := resolver(c, Reach{V4: true}).Resolve("big.test"); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("%v", err)
	}
}

func TestOrdered(t *testing.T) {
	a := Addresses{V4: netip.MustParseAddr("1.2.3.4")}
	if got := a.Ordered(Reach{V6: true}); len(got) != 1 || !got[0].Is4() {
		t.Fatalf("%v", got)
	}
}
