// Package netproto holds what the Oceans socket protocol (net-proto,
// ADR-0023, ADR-0024, ADR-0043) shares between its Go clients: the
// operations, the reply statuses, the stack's configuration (INFO, INFO6)
// and waiting on a socket's notification with a time limit.
package netproto

import (
	"encoding/binary"
	"net/netip"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

// Operations (request labels), as user/net-proto defines them.
const (
	OpInfo        = 1
	OpUDPOpen     = 2
	OpSendTo      = 4
	OpRecv        = 5
	OpTCPConnect  = 6
	OpTCPSend     = 9
	OpTCPRecv     = 10
	OpTCPShutdown = 11
	OpTCPStatus   = 12
	OpTCPAttach   = 13
	OpTCPRecvBuf  = 15
	OpSendTo6     = 16
	OpRecv6       = 17
	OpTCPConnect6 = 18
	OpInfo6       = 19
)

// Notification bits used by the Go clients: the stack signals Readable;
// TimeoutBit is the clients' own timer.
const (
	Readable   = 1 << 0
	TimeoutBit = 1 << 1
)

// Statuses (reply labels).
const (
	StatusOK            Error = 0
	StatusBadRequest    Error = 1
	StatusNotConfigured Error = 2
	StatusEmpty         Error = 6
	StatusNotConnected  Error = 12
	StatusEOF           Error = 14
)

var statusText = map[Error]string{
	1: "bad request", 2: "network not configured yet", 3: "port in use", 4: "no route to host",
	5: "too large", 6: "nothing received", 7: "out of buffers", 8: "no network device",
	9: "connection refused", 10: "connection reset by peer", 11: "timed out",
	12: "not connected", 13: "connection closed", 14: "end of stream",
}

// Error is a net-proto status other than OK.
type Error uint64

func (e Error) Error() string {
	if text, ok := statusText[e]; ok {
		return "network: " + text
	}
	return "network: error " + itoa(uint64(e))
}

// Status turns a reply label into an error (nil for OK).
func Status(label uint64) error {
	if label == 0 {
		return nil
	}
	return Error(label)
}

type timeoutError struct{}

func (timeoutError) Error() string   { return "network: timed out" }
func (timeoutError) Timeout() bool   { return true }
func (timeoutError) Temporary() bool { return true }

// ErrTimeout: a time limit passed. It reports Timeout() (as net.Error).
var ErrTimeout error = timeoutError{}

// Info is the stack's IPv4 configuration (INFO).
type Info struct {
	Configured bool
	Address    netip.Addr
	Prefix     uint8
	Gateway    netip.Addr
	// DNS is the server DHCP gave; invalid when unset.
	DNS netip.Addr
}

// ParseInfo decodes INFO's reply: [configured u8][address 4][prefix u8]
// [gateway 4][dns 4][mac 6].
func ParseInfo(data []byte) (Info, error) {
	if len(data) < 20 {
		return Info{}, StatusBadRequest
	}
	v4 := func(b []byte) netip.Addr {
		a := netip.AddrFrom4([4]byte(b))
		if a.IsUnspecified() {
			return netip.Addr{}
		}
		return a
	}
	return Info{
		Configured: data[0] != 0,
		Address:    v4(data[1:5]),
		Prefix:     data[5],
		Gateway:    v4(data[6:10]),
		DNS:        v4(data[10:14]),
	}, nil
}

// Info6 is what the Go clients use of the stack's IPv6 configuration
// (INFO6).
type Info6 struct {
	Enabled bool
	// Global: a usable (preferred or deprecated) address beyond the link.
	Global bool
	// DNS is a router's RDNSS server; invalid when unset.
	DNS netip.Addr
}

// ParseInfo6 decodes INFO6's reply: [flags u8][hop limit u8][mtu u16]
// [count u8], count × [address 16][prefix u8][state u8][link-local u8],
// then [router 16][dns 16].
func ParseInfo6(data []byte) (Info6, error) {
	const record = 19
	if len(data) < 5 {
		return Info6{}, StatusBadRequest
	}
	count := int(data[4])
	if count > 8 || len(data) < 5+count*record+32 {
		return Info6{}, StatusBadRequest
	}
	info := Info6{Enabled: data[0]&1 != 0}
	for i := range count {
		r := data[5+i*record:][:record]
		state, linkLocal := r[17], r[18] != 0
		if (state == 1 || state == 2) && !linkLocal {
			info.Global = true
		}
	}
	dns := netip.AddrFrom16([16]byte(data[5+count*record+16:][:16]))
	if !dns.IsUnspecified() {
		info.DNS = dns.Unmap()
	}
	return info, nil
}

// GetInfo asks the stack behind `net` for its IPv4 configuration.
func GetInfo(net oceans.Handle) (Info, error) {
	reply, err := oceans.Call(net, OpInfo, nil, nil)
	if err != nil {
		return Info{}, err
	}
	if err := Status(reply.Label); err != nil {
		return Info{}, err
	}
	return ParseInfo(reply.Data)
}

// GetInfo6 asks the stack behind `net` for its IPv6 configuration; a
// stack without IPv6 support (BadRequest) reports it disabled.
func GetInfo6(net oceans.Handle) (Info6, error) {
	reply, err := oceans.Call(net, OpInfo6, nil, nil)
	if err != nil {
		return Info6{}, err
	}
	if reply.Label == uint64(StatusBadRequest) {
		return Info6{}, nil
	}
	if err := Status(reply.Label); err != nil {
		return Info6{}, err
	}
	return ParseInfo6(reply.Data)
}

// PutAddr16 writes `a` as net-proto's 16-byte address: IPv6, or IPv4 as
// IPv4-mapped.
func PutAddr16(out []byte, a netip.Addr) {
	b := a.As16()
	copy(out, b[:])
}

// PutPortBits writes [port u16][bits u64], little-endian.
func PutPortBits(out []byte, port uint16, bits uint64) {
	binary.LittleEndian.PutUint16(out, port)
	binary.LittleEndian.PutUint64(out[2:], bits)
}

// Wait calls `done` until it says yes, sleeping on `notification`
// between calls, for at most `limit` (at least 1 ms). It returns
// ErrTimeout when the limit passes first.
func Wait(notification oceans.Handle, limit time.Duration, done func() (bool, error)) error {
	ms := uint64(limit / time.Millisecond)
	if ms == 0 {
		ms = 1
	}
	_ = oceans.TimerSet(notification, TimeoutBit, ms)
	defer oceans.TimerSet(notification, TimeoutBit, 0)
	for {
		ok, err := done()
		if err != nil || ok {
			return err
		}
		bits, err := oceans.NotificationWait(notification)
		if err != nil {
			return err
		}
		if bits&TimeoutBit != 0 {
			// One last look: the event may have come with the timeout.
			if ok, err := done(); ok || err != nil {
				return err
			}
			return ErrTimeout
		}
	}
}

// Notification creates a notification and a copy of it to give the stack
// (SIGNAL and TRANSFER only).
func Notification() (own, shared oceans.Handle, err error) {
	own, err = oceans.NotificationCreate()
	if err != nil {
		return 0, 0, err
	}
	shared, err = oceans.Duplicate(own, oceans.RightSignal|oceans.RightTransfer)
	if err != nil {
		_ = oceans.Close(own)
		return 0, 0, err
	}
	return own, shared, nil
}

func itoa(n uint64) string {
	if n == 0 {
		return "0"
	}
	var digits [20]byte
	i := len(digits)
	for ; n > 0; n /= 10 {
		i--
		digits[i] = byte('0' + n%10)
	}
	return string(digits[i:])
}
