package dns

import (
	"crypto/rand"
	"encoding/binary"
	"errors"
	"fmt"
	"net/netip"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/netproto"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/udp"
)

// Resolution errors (wrapped with the name).
var (
	ErrNoServer  = errors.New("no DNS server configured")
	ErrNotFound  = errors.New("not found")
	ErrNoAddress = errors.New("has no address")
	ErrServer    = errors.New("DNS server failure")
	ErrNoAnswer  = errors.New("the DNS server did not answer")
	// ErrTooLarge: the answer is longer than a socket message carries
	// (udp.MaxData6 bytes).
	ErrTooLarge = errors.New("the DNS answer is too large")
)

// Defaults: each query is sent up to DefaultTries times, waiting
// DefaultTimeout for the answer each time (as user/net-proto's resolver).
const (
	DefaultTries   = 3
	DefaultTimeout = 1500 * time.Millisecond
)

// Reach is what the network stack can use now, and the DNS server it was
// given (DHCP's, else a router's RDNSS).
type Reach struct {
	V4 bool
	// V6: a usable IPv6 address beyond the link.
	V6  bool
	DNS netip.Addr
}

// GetReach asks the stack behind `net`.
func GetReach(net oceans.Handle) (Reach, error) {
	info, err := netproto.GetInfo(net)
	if err != nil {
		return Reach{}, err
	}
	info6, err := netproto.GetInfo6(net)
	if err != nil {
		return Reach{}, err
	}
	reach := Reach{V4: info.Configured, V6: info6.Global}
	if info.Configured && info.DNS.IsValid() {
		reach.DNS = info.DNS
	} else {
		reach.DNS = info6.DNS
	}
	return reach, nil
}

// Addresses are the addresses a name has.
type Addresses struct {
	V4, V6 netip.Addr
}

// Ordered lists the addresses in the order to try (RFC 6724 §6, reduced
// to what one interface needs): IPv6 first when the stack has a global
// IPv6 address, IPv4 first otherwise.
func (a Addresses) Ordered(reach Reach) []netip.Addr {
	first, second := a.V4, a.V6
	if reach.V6 {
		first, second = a.V6, a.V4
	}
	var out []netip.Addr
	for _, address := range []netip.Addr{first, second} {
		if address.IsValid() {
			out = append(out, address)
		}
	}
	return out
}

// conn is a datagram socket: a udp.Socket on Oceans, a fake in tests.
type conn interface {
	SendTo(to netip.AddrPort, payload []byte) error
	Receive(timeout time.Duration) (udp.Datagram, error)
	Close() error
}

// Resolver resolves names through the network service Net.
type Resolver struct {
	Net oceans.Handle
	// Server is the DNS server to ask; the zero value: the one the network
	// service reports.
	Server netip.AddrPort
	// Tries and Timeout per query (zero: the defaults).
	Tries   int
	Timeout time.Duration

	// For tests: the socket and the stack's reach.
	open  func() (conn, error)
	reach func() (Reach, error)
}

// Resolve returns the addresses of `host` in the order to try: an address
// literal as it is, else the A and AAAA records the stack can use.
func (r *Resolver) Resolve(host string) ([]netip.Addr, error) {
	if address, err := netip.ParseAddr(host); err == nil && address.Zone() == "" {
		return []netip.Addr{address.Unmap()}, nil
	}
	if !ValidName(host) {
		return nil, fmt.Errorf("%s: %w", host, ErrBadName)
	}
	reach, err := r.getReach()
	if err != nil {
		return nil, err
	}
	server := r.Server
	if !server.IsValid() {
		if !reach.DNS.IsValid() {
			return nil, fmt.Errorf("%s: %w", host, ErrNoServer)
		}
		server = netip.AddrPortFrom(reach.DNS, Port)
	}
	found, err := r.lookup(host, server, reach.V4 || !reach.V6, reach.V6)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", host, err)
	}
	return found.Ordered(reach), nil
}

func (r *Resolver) getReach() (Reach, error) {
	if r.reach != nil {
		return r.reach()
	}
	return GetReach(r.Net)
}

// lookup asks `server` for `name`'s A and AAAA records, as `wantA` and
// `wantAAAA` say. A failed query does not hide what the other found.
func (r *Resolver) lookup(name string, server netip.AddrPort, wantA, wantAAAA bool) (Addresses, error) {
	var c conn
	var err error
	if r.open != nil {
		c, err = r.open()
	} else {
		c, err = udp.Open(r.Net, 0)
	}
	if err != nil {
		return Addresses{}, err
	}
	defer c.Close()
	var found Addresses
	var errs []error
	for _, q := range []struct {
		want bool
		t    Type
		into *netip.Addr
	}{{wantA, TypeA, &found.V4}, {wantAAAA, TypeAAAA, &found.V6}} {
		if !q.want {
			continue
		}
		answer, err := r.query(c, server, name, q.t)
		if err == nil {
			switch answer.Kind {
			case Found:
				*q.into = answer.Address
			case NotFound:
				err = ErrNotFound
			case ServerError:
				err = fmt.Errorf("%w (code %d)", ErrServer, answer.RCode)
			}
		}
		if err != nil {
			errs = append(errs, err)
		}
	}
	if found.V4.IsValid() || found.V6.IsValid() {
		return found, nil
	}
	if len(errs) > 0 {
		// Nothing: the first failure explains it.
		return Addresses{}, errs[0]
	}
	return Addresses{}, ErrNoAddress
}

// query sends one query, retried on silence, and returns the answer from
// `server` to it. Other datagrams (another sender, another id or
// question, malformed) are ignored.
func (r *Resolver) query(c conn, server netip.AddrPort, name string, t Type) (Answer, error) {
	tries, timeout := r.Tries, r.Timeout
	if tries <= 0 {
		tries = DefaultTries
	}
	if timeout <= 0 {
		timeout = DefaultTimeout
	}
	// Unpredictable ids make forged answers much harder (ADR-0026).
	var random [2]byte
	if _, err := rand.Read(random[:]); err != nil {
		return Answer{}, err
	}
	id := binary.LittleEndian.Uint16(random[:])
	message, err := BuildQuery(id, name, t)
	if err != nil {
		return Answer{}, err
	}
	if len(message) > udp.MaxData6 {
		return Answer{}, ErrBadName
	}
	server = netip.AddrPortFrom(server.Addr().Unmap(), server.Port())
	for range tries {
		if err := c.SendTo(server, message); err != nil {
			return Answer{}, err
		}
		deadline := time.Now().Add(timeout)
		for {
			left := time.Until(deadline)
			if left <= 0 {
				break
			}
			d, err := c.Receive(left)
			if errors.Is(err, netproto.ErrTimeout) {
				break
			}
			if err != nil {
				return Answer{}, err
			}
			if d.From != server {
				continue
			}
			if d.Truncated {
				// Our answer, cut short: it cannot be read whole, and
				// waiting would only time out.
				if len(d.Payload) >= 2 && binary.BigEndian.Uint16(d.Payload) == id {
					return Answer{}, ErrTooLarge
				}
				continue
			}
			answer, err := ParseResponse(id, name, t, d.Payload)
			if errors.Is(err, ErrTruncated) {
				return Answer{}, err
			}
			if err == nil {
				return answer, nil
			}
		}
	}
	return Answer{}, ErrNoAnswer
}
