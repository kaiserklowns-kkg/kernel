// Package udp is a UDP socket over the Oceans network service (net-proto
// UDP_OPEN, SEND_TO/SEND_TO6, RECV6; ADR-0023, ADR-0043) for Go programs.
// As with TCP, the stack signals the socket's notification when a
// datagram is waiting, and Receive sleeps on it with a time limit.
package udp

import (
	"encoding/binary"
	"net/netip"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/netproto"
)

// Largest payloads per datagram: to an IPv4 address (SEND_TO), and to an
// IPv6 one or received (SEND_TO6, RECV6: the 16-byte address leaves less
// room in a message).
const (
	MaxData  = 240
	MaxData6 = 236
)

// truncated is RECV6's flag: the payload was cut to MaxData6.
const truncated = 1

// Datagram is what the socket received.
type Datagram struct {
	// From: the sender (IPv4 senders as IPv4, not IPv4-mapped).
	From    netip.AddrPort
	Payload []byte
	// Truncated: the datagram was longer than MaxData6.
	Truncated bool
}

// Socket is a UDP socket.
type Socket struct {
	handle       oceans.Handle
	notification oceans.Handle
	port         uint16
}

// Open opens a UDP socket on `port` (0: an ephemeral port) through the
// network service `net`.
func Open(net oceans.Handle, port uint16) (*Socket, error) {
	notification, shared, err := netproto.Notification()
	if err != nil {
		return nil, err
	}
	data := make([]byte, 10)
	netproto.PutPortBits(data, port, netproto.Readable)
	reply, err := oceans.Call(net, netproto.OpUDPOpen, data, []oceans.Handle{shared})
	if err == nil {
		err = netproto.Status(reply.Label)
	}
	if err == nil && (len(reply.Handles) != 1 || len(reply.Data) < 2) {
		for _, h := range reply.Handles {
			_ = oceans.Close(h)
		}
		err = netproto.StatusBadRequest
	}
	if err != nil {
		_ = oceans.Close(notification)
		return nil, err
	}
	return &Socket{
		handle:       reply.Handles[0],
		notification: notification,
		port:         binary.LittleEndian.Uint16(reply.Data),
	}, nil
}

// Port is the socket's local port.
func (s *Socket) Port() uint16 { return s.port }

// SendTo sends `payload` to `to`, an IPv4 or IPv6 address.
func (s *Socket) SendTo(to netip.AddrPort, payload []byte) error {
	address := to.Addr().Unmap()
	var op uint64
	var data []byte
	switch {
	case !address.IsValid():
		return netproto.StatusBadRequest
	case address.Is4():
		if len(payload) > MaxData {
			return Error(5)
		}
		a := address.As4()
		op, data = netproto.OpSendTo, append(a[:], 0, 0)
		binary.LittleEndian.PutUint16(data[4:], to.Port())
	default:
		if len(payload) > MaxData6 {
			return Error(5)
		}
		op, data = netproto.OpSendTo6, make([]byte, 18, 18+len(payload))
		netproto.PutAddr16(data, address)
		binary.LittleEndian.PutUint16(data[16:], to.Port())
	}
	reply, err := oceans.Call(s.handle, op, append(data, payload...), nil)
	if err != nil {
		return err
	}
	return netproto.Status(reply.Label)
}

// Error is a net-proto status.
type Error = netproto.Error

// Recv returns the next waiting datagram, or false if none is.
func (s *Socket) Recv() (Datagram, bool, error) {
	reply, err := oceans.Call(s.handle, netproto.OpRecv6, nil, nil)
	if err != nil {
		return Datagram{}, false, err
	}
	if reply.Label == uint64(netproto.StatusEmpty) {
		return Datagram{}, false, nil
	}
	if err := netproto.Status(reply.Label); err != nil {
		return Datagram{}, false, err
	}
	d, err := parseRecv6(reply.Data)
	return d, err == nil, err
}

// parseRecv6 decodes RECV6's reply: [address 16][port u16][flags u8]
// [payload].
func parseRecv6(data []byte) (Datagram, error) {
	if len(data) < 19 {
		return Datagram{}, netproto.StatusBadRequest
	}
	from := netip.AddrFrom16([16]byte(data[:16])).Unmap()
	return Datagram{
		From:      netip.AddrPortFrom(from, binary.LittleEndian.Uint16(data[16:])),
		Payload:   append([]byte(nil), data[19:]...),
		Truncated: data[18]&truncated != 0,
	}, nil
}

// Receive waits up to `timeout` for a datagram; ErrTimeout if none comes.
func (s *Socket) Receive(timeout time.Duration) (Datagram, error) {
	var got Datagram
	err := netproto.Wait(s.notification, timeout, func() (bool, error) {
		d, ok, err := s.Recv()
		got = d
		return ok, err
	})
	return got, err
}

// ErrTimeout: Receive's time limit passed.
var ErrTimeout = netproto.ErrTimeout

// Close closes the socket.
func (s *Socket) Close() error {
	err := oceans.Close(s.handle)
	_ = oceans.Close(s.notification)
	return err
}
