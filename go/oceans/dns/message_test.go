package dns

import (
	"bytes"
	"encoding/binary"
	"errors"
	"net/netip"
	"strings"
	"testing"
)

func TestBuildQuery(t *testing.T) {
	q, err := BuildQuery(0x1234, "example.com", TypeA)
	if err != nil {
		t.Fatal(err)
	}
	want := []byte{0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0,
		7, 'e', 'x', 'a', 'm', 'p', 'l', 'e', 3, 'c', 'o', 'm', 0, 0, 1, 0, 1}
	if !bytes.Equal(q, want) {
		t.Fatalf("% x", q)
	}
	dotted, _ := BuildQuery(0x1234, "example.com.", TypeA)
	if !bytes.Equal(dotted, want) {
		t.Fatal("a trailing dot changes the query")
	}
	aaaa, _ := BuildQuery(1, "a.b", TypeAAAA)
	if binary.BigEndian.Uint16(aaaa[len(aaaa)-4:]) != 28 {
		t.Fatalf("AAAA query type: % x", aaaa)
	}
	if _, err := BuildQuery(1, "bad name", TypeA); !errors.Is(err, ErrBadName) {
		t.Fatalf("bad name: %v", err)
	}
}

func TestValidName(t *testing.T) {
	for _, good := range []string{"a", "oceans.test", "x-1.example.com", "_srv.host", "a.b.c.", "MODELS.Oceans.Test"} {
		if !ValidName(good) {
			t.Errorf("%q refused", good)
		}
	}
	for _, bad := range []string{"", ".", "a..b", "-a.com", "a-.com", "a b", "ä.com", strings.Repeat("a", 64),
		strings.Repeat("abcdefghi.", 26) + "x", "a/b", "a:b", "[::1]"} {
		if ValidName(bad) {
			t.Errorf("%q accepted", bad)
		}
	}
}

// response builds a response to `query` with these answer records (name
// pointers into the question allowed) and rcode.
func response(query []byte, rcode uint16, answers ...[]byte) []byte {
	out := append([]byte(nil), query...)
	binary.BigEndian.PutUint16(out[2:], 0x8180|rcode)
	binary.BigEndian.PutUint16(out[6:], uint16(len(answers)))
	for _, a := range answers {
		out = append(out, a...)
	}
	return out
}

// record: owner name (wire form), type, TTL, data.
func record(owner []byte, kind uint16, ttl uint32, data []byte) []byte {
	out := append([]byte(nil), owner...)
	out = binary.BigEndian.AppendUint16(out, kind)
	out = binary.BigEndian.AppendUint16(out, 1)
	out = binary.BigEndian.AppendUint32(out, ttl)
	out = binary.BigEndian.AppendUint16(out, uint16(len(data)))
	return append(out, data...)
}

// The question's name, by pointer.
var questionName = []byte{0xc0, 12}

func wire(name string) []byte {
	var out []byte
	for label := range strings.SplitSeq(name, ".") {
		out = append(out, byte(len(label)))
		out = append(out, label...)
	}
	return append(out, 0)
}

func TestParseAddress(t *testing.T) {
	q, _ := BuildQuery(7, "models.oceans.test", TypeA)
	got, err := ParseResponse(7, "Models.Oceans.Test", TypeA, response(q, 0, record(questionName, 1, 60, []byte{10, 0, 2, 2})))
	if err != nil || got.Kind != Found || got.Address != netip.MustParseAddr("10.0.2.2") || got.TTL != 60 {
		t.Fatalf("%+v %v", got, err)
	}
	q6, _ := BuildQuery(8, "v6.test", TypeAAAA)
	address := netip.MustParseAddr("2001:db8::6").As16()
	got, err = ParseResponse(8, "v6.test", TypeAAAA, response(q6, 0, record(questionName, 28, 5, address[:])))
	if err != nil || got.Kind != Found || got.Address != netip.MustParseAddr("2001:db8::6") {
		t.Fatalf("AAAA: %+v %v", got, err)
	}
}

func TestParseFollowsCNAMEs(t *testing.T) {
	q, _ := BuildQuery(9, "api.example.com", TypeA)
	// api.example.com CNAME edge.cdn.test; an unrelated A record first;
	// edge.cdn.test A 192.0.2.7.
	cname := record(questionName, 5, 300, wire("edge.cdn.test"))
	other := record(wire("other.test"), 1, 1, []byte{1, 1, 1, 1})
	// The CNAME's target starts 12 bytes into its record (after the
	// pointer and the fixed fields): point to it.
	targetAt := len(q) + len(other) + 12
	a := record([]byte{0xc0 | byte(targetAt>>8), byte(targetAt)}, 1, 30, []byte{192, 0, 2, 7})
	got, err := ParseResponse(9, "api.example.com", TypeA, response(q, 0, other, cname, a))
	if err != nil || got.Kind != Found || got.Address != netip.MustParseAddr("192.0.2.7") {
		t.Fatalf("%+v %v", got, err)
	}
	// A CNAME loop ends with no address.
	loop := response(q, 0, record(questionName, 5, 1, questionName))
	got, err = ParseResponse(9, "api.example.com", TypeA, loop)
	if err != nil || got.Kind != NoAddress {
		t.Fatalf("loop: %+v %v", got, err)
	}
}

func TestParseOutcomes(t *testing.T) {
	q, _ := BuildQuery(3, "missing.test", TypeA)
	if got, err := ParseResponse(3, "missing.test", TypeA, response(q, 3)); err != nil || got.Kind != NotFound {
		t.Fatalf("NXDOMAIN: %+v %v", got, err)
	}
	if got, err := ParseResponse(3, "missing.test", TypeA, response(q, 2)); err != nil || got.Kind != ServerError || got.RCode != 2 {
		t.Fatalf("SERVFAIL: %+v %v", got, err)
	}
	if got, err := ParseResponse(3, "missing.test", TypeA, response(q, 0)); err != nil || got.Kind != NoAddress {
		t.Fatalf("empty: %+v %v", got, err)
	}
	// Only AAAA, but A asked.
	address := netip.MustParseAddr("::1").As16()
	if got, err := ParseResponse(3, "missing.test", TypeA, response(q, 0, record(questionName, 28, 1, address[:]))); err != nil || got.Kind != NoAddress {
		t.Fatalf("other type: %+v %v", got, err)
	}
}

func TestParseRefusesWhatIsNotTheAnswer(t *testing.T) {
	q, _ := BuildQuery(5, "a.test", TypeA)
	good := response(q, 0, record(questionName, 1, 1, []byte{1, 2, 3, 4}))
	if _, err := ParseResponse(5, "a.test", TypeA, good); err != nil {
		t.Fatal(err)
	}
	mutate := func(f func(m []byte) []byte) []byte { return f(append([]byte(nil), good...)) }
	cases := map[string][]byte{
		"empty":           {},
		"short header":    good[:11],
		"another id":      mutate(func(m []byte) []byte { m[1] = 6; return m }),
		"a query":         mutate(func(m []byte) []byte { m[2] &^= 0x80; return m }),
		"opcode":          mutate(func(m []byte) []byte { m[2] |= 0x08; return m }),
		"two questions":   mutate(func(m []byte) []byte { m[5] = 2; return m }),
		"another name":    mutate(func(m []byte) []byte { m[13] = 'b'; return m }),
		"another type":    mutate(func(m []byte) []byte { m[len(q)-3] = 28; return m }),
		"another class":   mutate(func(m []byte) []byte { m[len(q)-1] = 3; return m }),
		"cut record":      good[:len(good)-1],
		"long address":    response(q, 0, record(questionName, 1, 1, []byte{1, 2, 3, 4, 5})),
		"forward pointer": response(q, 0, record([]byte{0xc0, 0xff}, 1, 1, []byte{1, 2, 3, 4})),
		"self pointer": func() []byte {
			at := len(q)
			return response(q, 0, record([]byte{0xc0 | byte(at>>8), byte(at)}, 1, 1, []byte{1, 2, 3, 4}))
		}(),
		"bad label kind": response(q, 0, record([]byte{0x40, 1}, 1, 1, []byte{1, 2, 3, 4})),
		"label past end": append(append([]byte(nil), q[:12]...), 60, 'a'),
		"dotted label":   response(q, 0, record(append([]byte{3, 'a', '.', 'b'}, 0), 1, 1, []byte{1, 2, 3, 4})),
		"answer count":   mutate(func(m []byte) []byte { m[7] = 2; return m }),
	}
	for name, m := range cases {
		if got, err := ParseResponse(5, "a.test", TypeA, m); err == nil {
			t.Errorf("%s: accepted as %+v", name, got)
		}
	}
	truncated := mutate(func(m []byte) []byte { m[2] |= 0x02; return m })
	if _, err := ParseResponse(5, "a.test", TypeA, truncated); !errors.Is(err, ErrTruncated) {
		t.Errorf("TC: %v", err)
	}
}

func TestParseBoundsPointerChains(t *testing.T) {
	q, _ := BuildQuery(5, "a.test", TypeA)
	// chain: an unknown record whose data is `n` pointers, each to the
	// one before (the first to the question), then an A record owned by
	// the last.
	chain := func(n int) []byte {
		dataAt := len(q) + 12
		var data []byte
		prev := 12
		for i := range n {
			at := dataAt + 2*i
			data = append(data, 0xc0|byte(prev>>8), byte(prev))
			prev = at
		}
		holder := record(questionName, 99, 1, data)
		a := record([]byte{0xc0 | byte(prev>>8), byte(prev)}, 1, 1, []byte{1, 2, 3, 4})
		return response(q, 0, holder, a)
	}
	if got, err := ParseResponse(5, "a.test", TypeA, chain(10)); err != nil || got.Kind != Found {
		t.Fatalf("a short chain: %+v %v", got, err)
	}
	if _, err := ParseResponse(5, "a.test", TypeA, chain(20)); err == nil {
		t.Fatal("a long pointer chain was accepted")
	}
}

func FuzzParseResponse(f *testing.F) {
	q, _ := BuildQuery(5, "a.test", TypeA)
	f.Add(response(q, 0, record(questionName, 1, 1, []byte{1, 2, 3, 4})))
	f.Add(response(q, 0, record(questionName, 5, 1, questionName)))
	f.Fuzz(func(t *testing.T, m []byte) {
		_, _ = ParseResponse(5, "a.test", TypeA, m)
	})
}
