// Package dns resolves host names for Go programs on Oceans (ADR-0054):
// DNS messages (RFC 1035; AAAA, RFC 3596), built and parsed as libs/dns
// does for Rust programs, and a resolver that asks a DNS server over the
// Oceans UDP socket protocol, with timeouts and retries.
//
// A response is untrusted network input. It must come from the server
// asked, match the query's id and question; every length, label and
// compression pointer is bounds-checked, and pointer chains are bounded
// and must point backwards, so a hostile packet cannot loop the parser or
// make it read past the message.
package dns

import (
	"encoding/binary"
	"errors"
	"net/netip"
	"strings"
)

// Limits and constants.
const (
	// MaxName is the longest name in text form (without a trailing dot).
	MaxName = 253
	// MaxMessage is the longest DNS message over UDP without EDNS.
	MaxMessage = 512
	// Port is the DNS server port.
	Port = 53

	header      = 12
	maxPointers = 16
	maxAnswers  = 16
	classIN     = 1
	typeCNAME   = 5
)

// Type is a query type: A or AAAA.
type Type uint16

const (
	TypeA    Type = 1
	TypeAAAA Type = 28
)

func (t Type) size() int {
	if t == TypeAAAA {
		return 16
	}
	return 4
}

// Errors of building and parsing.
var (
	ErrBadName = errors.New("dns: not a valid host name")
	// ErrMalformed: the response does not belong to the query, or is
	// malformed.
	ErrMalformed = errors.New("dns: malformed response")
	// ErrTruncated: the response was truncated (TC); it would need TCP.
	ErrTruncated = errors.New("dns: truncated response")
)

// Kind is what a valid response says about the name.
type Kind int

const (
	// Found: an address of the kind asked for.
	Found Kind = iota
	// NotFound: the name does not exist (NXDOMAIN).
	NotFound
	// NoAddress: the name exists but has no address of the kind asked for.
	NoAddress
	// ServerError: the server failed or refused (another RCODE).
	ServerError
)

// Answer is a parsed response.
type Answer struct {
	Kind Kind
	// Address and TTL (seconds), when Found.
	Address netip.Addr
	TTL     uint32
	// RCode, when ServerError.
	RCode uint8
}

// ValidName reports whether `name` is a host name we will ask for:
// dot-separated labels of 1–63 letters, digits, hyphens or underscores, not
// starting or ending with a hyphen, at most MaxName long (an optional
// trailing dot).
func ValidName(name string) bool {
	name = strings.TrimSuffix(name, ".")
	if name == "" || len(name) > MaxName {
		return false
	}
	for label := range strings.SplitSeq(name, ".") {
		if len(label) < 1 || len(label) > 63 || label[0] == '-' || label[len(label)-1] == '-' {
			return false
		}
		for i := 0; i < len(label); i++ {
			c := label[i]
			if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_') {
				return false
			}
		}
	}
	return true
}

// BuildQuery returns a recursive query with `id` for `name`'s records of
// type `t`.
func BuildQuery(id uint16, name string, t Type) ([]byte, error) {
	if !ValidName(name) {
		return nil, ErrBadName
	}
	name = strings.TrimSuffix(name, ".")
	out := make([]byte, header, header+len(name)+2+4)
	binary.BigEndian.PutUint16(out, id)
	out[2] = 0x01 // RD
	out[5] = 1    // QDCOUNT
	for label := range strings.SplitSeq(name, ".") {
		out = append(out, byte(len(label)))
		out = append(out, label...)
	}
	out = append(out, 0)
	out = binary.BigEndian.AppendUint16(out, uint16(t))
	out = binary.BigEndian.AppendUint16(out, classIN)
	return out, nil
}

func be16(m []byte, at int) (uint16, error) {
	if at < 0 || at+2 > len(m) {
		return 0, ErrMalformed
	}
	return binary.BigEndian.Uint16(m[at:]), nil
}

// walkName walks the (possibly compressed) name at `at`, calling `label`
// for each label in order; it returns the offset just past the name where
// it starts.
func walkName(m []byte, at int, label func([]byte)) (int, error) {
	end, pointers, total := -1, 0, 0
	for {
		if at >= len(m) {
			return 0, ErrMalformed
		}
		n := int(m[at])
		switch n >> 6 {
		case 0b00:
			if n == 0 {
				if end < 0 {
					end = at + 1
				}
				return end, nil
			}
			if at+1+n > len(m) {
				return 0, ErrMalformed
			}
			total += 1 + n
			if total > MaxName+1 {
				return 0, ErrMalformed
			}
			if label != nil {
				label(m[at+1 : at+1+n])
			}
			at += 1 + n
		case 0b11:
			v, err := be16(m, at)
			if err != nil {
				return 0, err
			}
			target := int(v & 0x3fff)
			if end < 0 {
				end = at + 2
			}
			pointers++
			// Pointers must go backwards, and chains are bounded.
			if pointers > maxPointers || target >= at {
				return 0, ErrMalformed
			}
			at = target
		default:
			return 0, ErrMalformed
		}
	}
}

// flatName is the name at `at` in lower case, labels joined by dots. A
// label holding a dot (which text form could not tell apart) is
// malformed.
func flatName(m []byte, at int) (string, int, error) {
	var b strings.Builder
	dotted := false
	end, err := walkName(m, at, func(label []byte) {
		if b.Len() > 0 {
			b.WriteByte('.')
		}
		dotted = dotted || strings.IndexByte(string(label), '.') >= 0
		for _, c := range label {
			if c >= 'A' && c <= 'Z' {
				c += 'a' - 'A' // ASCII only: other bytes stay as they are
			}
			b.WriteByte(c)
		}
	})
	if err == nil && dotted {
		err = ErrMalformed
	}
	return b.String(), end, err
}

// ParseResponse parses the response to query `id` for `name`'s records of
// type `t`, following CNAMEs from the name asked to an address.
func ParseResponse(id uint16, name string, t Type, m []byte) (Answer, error) {
	if len(m) < header || binary.BigEndian.Uint16(m) != id {
		return Answer{}, ErrMalformed
	}
	flags := binary.BigEndian.Uint16(m[2:])
	if flags&0x8000 == 0 || (flags>>11)&0xf != 0 {
		return Answer{}, ErrMalformed // not a response, or not a query's
	}
	if flags&0x0200 != 0 {
		return Answer{}, ErrTruncated
	}
	if binary.BigEndian.Uint16(m[4:]) != 1 {
		return Answer{}, ErrMalformed
	}
	if !ValidName(name) {
		return Answer{}, ErrBadName
	}
	// A valid name is ASCII, so this lowers exactly what flatName lowers.
	want := strings.ToLower(strings.TrimSuffix(name, "."))
	asked, at, err := flatName(m, header)
	if err != nil {
		return Answer{}, err
	}
	qtype, err := be16(m, at)
	if err != nil {
		return Answer{}, err
	}
	qclass, err := be16(m, at+2)
	if err != nil {
		return Answer{}, err
	}
	if asked != want || Type(qtype) != t || qclass != classIN {
		return Answer{}, ErrMalformed
	}
	at += 4
	switch rcode := uint8(flags & 0xf); rcode {
	case 0:
	case 3:
		return Answer{Kind: NotFound}, nil
	default:
		return Answer{Kind: ServerError, RCode: rcode}, nil
	}
	type record struct {
		owner string
		kind  uint16
		class uint16
		ttl   uint32
		data  []byte
		// target: a CNAME's name.
		target string
	}
	count := int(binary.BigEndian.Uint16(m[6:]))
	records := make([]record, 0, min(count, maxAnswers))
	for i := 0; i < count && i < maxAnswers; i++ {
		owner, next, err := flatName(m, at)
		if err != nil {
			return Answer{}, err
		}
		if next+10 > len(m) {
			return Answer{}, ErrMalformed
		}
		r := record{
			owner: owner,
			kind:  binary.BigEndian.Uint16(m[next:]),
			class: binary.BigEndian.Uint16(m[next+2:]),
			ttl:   binary.BigEndian.Uint32(m[next+4:]),
		}
		length := int(binary.BigEndian.Uint16(m[next+8:]))
		data := next + 10
		if data+length > len(m) {
			return Answer{}, ErrMalformed
		}
		r.data = m[data : data+length]
		if r.kind == typeCNAME && r.class == classIN {
			target, end, err := flatName(m, data)
			if err != nil || end > data+length {
				return Answer{}, ErrMalformed
			}
			r.target = target
		}
		records = append(records, r)
		at = data + length
	}
	// Follow the CNAME chain from the asked name (at most one step per
	// record, so a cycle ends).
	owner := want
	for range len(records) + 1 {
		next := ""
		for _, r := range records {
			if r.class != classIN || r.owner != owner {
				continue
			}
			switch {
			case Type(r.kind) == t:
				if len(r.data) != t.size() {
					return Answer{}, ErrMalformed
				}
				var address netip.Addr
				if t == TypeA {
					address = netip.AddrFrom4([4]byte(r.data))
				} else {
					address = netip.AddrFrom16([16]byte(r.data))
				}
				return Answer{Kind: Found, Address: address, TTL: r.ttl}, nil
			case r.kind == typeCNAME:
				next = r.target
			}
		}
		if next == "" {
			break
		}
		owner = next
	}
	return Answer{Kind: NoAddress}, nil
}
