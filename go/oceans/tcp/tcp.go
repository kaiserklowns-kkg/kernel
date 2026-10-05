// Package tcp is a TCP client over the Oceans network service (net-proto,
// ADR-0023, ADR-0024, ADR-0043) for Go programs: Dial and DialAddr return
// a connection that is an io.ReadWriteCloser, with time limits and
// deadlines (what crypto/tls needs of a connection).
package tcp

import (
	"encoding/binary"
	"io"
	"net/netip"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/netproto"
)

// TCP states, as TCP_STATUS reports them.
const (
	stateSynSent     = 2
	stateSynReceived = 3
	stateEstablished = 4
	stateCloseWait   = 7

	maxStream = 248
)

// Error is a net-proto status.
type Error = netproto.Error

// ErrTimeout: a time limit or deadline passed.
var ErrTimeout = netproto.ErrTimeout

// Conn is a TCP connection.
type Conn struct {
	handle       oceans.Handle
	notification oceans.Handle
	remote       netip.AddrPort
	// Timeout bounds each wait in Read and Write.
	Timeout       time.Duration
	readDeadline  time.Time
	writeDeadline time.Time
	pending       []byte
	eof           bool
	// A shared buffer attached to the connection (ADR-0030; 0: none):
	// received data arrives through it, not in messages.
	shared     oceans.Handle
	sharedSize int
}

// UseSharedBuffer attaches a shared buffer of `size` bytes (4 KiB to
// 1 MiB): reads then take up to that much per call instead of a message's
// worth, for bulk transfers (a package from the Store, ADR-0061).
func (c *Conn) UseSharedBuffer(size int) error {
	if size < 4<<10 || size > 1<<20 {
		return netproto.StatusBadRequest
	}
	memory, err := oceans.MemoryCreate(uint64(size))
	if err != nil {
		return err
	}
	theirs, err := oceans.Duplicate(memory, oceans.RightRead|oceans.RightWrite|oceans.RightMap|oceans.RightTransfer)
	if err == nil {
		var reply oceans.Message
		reply, err = oceans.Call(c.handle, netproto.OpTCPAttach, nil, []oceans.Handle{theirs})
		if err == nil {
			err = netproto.Status(reply.Label)
		}
	}
	if err != nil {
		_ = oceans.Close(memory)
		return err
	}
	c.shared, c.sharedSize = memory, size
	return nil
}

// receive takes what the stack has for us: through the shared buffer if
// one is attached, else in the reply.
func (c *Conn) receive() (oceans.Message, []byte, error) {
	if c.shared == 0 {
		reply, err := oceans.Call(c.handle, netproto.OpTCPRecv, nil, nil)
		return reply, reply.Data, err
	}
	request := make([]byte, 8)
	binary.LittleEndian.PutUint32(request[4:], uint32(c.sharedSize))
	reply, err := oceans.Call(c.handle, netproto.OpTCPRecvBuf, request, nil)
	if err != nil || Error(reply.Label) != netproto.StatusOK {
		return reply, nil, err
	}
	if len(reply.Data) < 4 {
		return reply, nil, netproto.StatusBadRequest
	}
	n := int(binary.LittleEndian.Uint32(reply.Data))
	if n > c.sharedSize {
		return reply, nil, netproto.StatusBadRequest
	}
	data := make([]byte, n)
	if _, err := oceans.MemoryRead(c.shared, 0, data); err != nil {
		return reply, nil, err
	}
	return reply, data, nil
}

// Dial connects to IPv4 `address:port` through the network service
// `net`, waiting up to `timeout` for the connection.
func Dial(net oceans.Handle, address [4]byte, port uint16, timeout time.Duration) (*Conn, error) {
	return DialAddr(net, netip.AddrPortFrom(netip.AddrFrom4(address), port), timeout)
}

// DialAddr connects to `to`, IPv4 (TCP_CONNECT) or IPv6 (TCP_CONNECT6),
// waiting up to `timeout` for the connection.
func DialAddr(net oceans.Handle, to netip.AddrPort, timeout time.Duration) (*Conn, error) {
	address := to.Addr().Unmap()
	if !address.IsValid() || to.Port() == 0 {
		return nil, netproto.StatusBadRequest
	}
	notification, shared, err := netproto.Notification()
	if err != nil {
		return nil, err
	}
	var op uint64
	var data []byte
	if address.Is4() {
		op, data = netproto.OpTCPConnect, make([]byte, 14)
		a := address.As4()
		copy(data, a[:])
		netproto.PutPortBits(data[4:], to.Port(), netproto.Readable)
	} else {
		op, data = netproto.OpTCPConnect6, make([]byte, 26)
		netproto.PutAddr16(data, address)
		netproto.PutPortBits(data[16:], to.Port(), netproto.Readable)
	}
	reply, err := oceans.Call(net, op, data, []oceans.Handle{shared})
	if err == nil {
		err = netproto.Status(reply.Label)
	}
	if err == nil && len(reply.Handles) != 1 {
		err = netproto.StatusBadRequest
	}
	if err != nil {
		_ = oceans.Close(notification)
		return nil, err
	}
	c := &Conn{
		handle:       reply.Handles[0],
		notification: notification,
		remote:       netip.AddrPortFrom(address, to.Port()),
		Timeout:      timeout,
	}
	if err := netproto.Wait(c.notification, timeout, c.connected); err != nil {
		c.Close()
		return nil, err
	}
	return c, nil
}

// RemoteAddr is the address connected to.
func (c *Conn) RemoteAddr() netip.AddrPort { return c.remote }

// connected: done (true), still connecting (false), or failed.
func (c *Conn) connected() (bool, error) {
	reply, err := oceans.Call(c.handle, netproto.OpTCPStatus, nil, nil)
	if err != nil {
		return false, err
	}
	if err := netproto.Status(reply.Label); err != nil {
		return false, err
	}
	if len(reply.Data) < 2 {
		return false, netproto.StatusBadRequest
	}
	if reply.Data[1] != 0 {
		return false, Error(reply.Data[1])
	}
	switch reply.Data[0] {
	case stateEstablished, stateCloseWait:
		return true, nil
	case stateSynSent, stateSynReceived:
		return false, nil
	}
	return false, netproto.StatusNotConnected
}

// limit is how long one wait may take: the timeout, cut short by the
// deadline if one is set (an error if it has passed).
func (c *Conn) limit(deadline time.Time) (time.Duration, error) {
	limit := c.Timeout
	if limit <= 0 {
		limit = 30 * time.Second
	}
	if !deadline.IsZero() {
		left := time.Until(deadline)
		if left <= 0 {
			return 0, ErrTimeout
		}
		limit = min(limit, left)
	}
	return limit, nil
}

// SetDeadline sets the read and write deadlines (zero: none).
func (c *Conn) SetDeadline(t time.Time) error {
	c.readDeadline, c.writeDeadline = t, t
	return nil
}

// SetReadDeadline sets the time after which Read fails with ErrTimeout.
func (c *Conn) SetReadDeadline(t time.Time) error {
	c.readDeadline = t
	return nil
}

// SetWriteDeadline sets the time after which Write fails with ErrTimeout.
func (c *Conn) SetWriteDeadline(t time.Time) error {
	c.writeDeadline = t
	return nil
}

// Write sends all of `b`, waiting while the send buffer is full.
func (c *Conn) Write(b []byte) (int, error) {
	written := 0
	for written < len(b) {
		limit, err := c.limit(c.writeDeadline)
		if err != nil {
			return written, err
		}
		chunk := b[written:min(len(b), written+maxStream)]
		var accepted int
		err = netproto.Wait(c.notification, limit, func() (bool, error) {
			reply, err := oceans.Call(c.handle, netproto.OpTCPSend, chunk, nil)
			if err != nil {
				return false, err
			}
			if err := netproto.Status(reply.Label); err != nil {
				return false, err
			}
			if len(reply.Data) < 4 {
				return false, netproto.StatusBadRequest
			}
			accepted = int(binary.LittleEndian.Uint32(reply.Data))
			if accepted > len(chunk) {
				return false, netproto.StatusBadRequest
			}
			return accepted > 0, nil
		})
		if err != nil {
			return written, err
		}
		written += accepted
	}
	return written, nil
}

// Read reads what has arrived, waiting for something or the end.
func (c *Conn) Read(b []byte) (int, error) {
	if len(c.pending) == 0 && !c.eof {
		limit, err := c.limit(c.readDeadline)
		if err != nil {
			return 0, err
		}
		err = netproto.Wait(c.notification, limit, func() (bool, error) {
			reply, data, err := c.receive()
			if err != nil {
				return false, err
			}
			switch Error(reply.Label) {
			case netproto.StatusOK:
				c.pending = append(c.pending, data...)
				return true, nil
			case netproto.StatusEmpty:
				return false, nil
			case netproto.StatusEOF:
				c.eof = true
				return true, nil
			}
			return false, Error(reply.Label)
		})
		if err != nil {
			return 0, err
		}
	}
	if len(c.pending) == 0 {
		return 0, io.EOF
	}
	n := copy(b, c.pending)
	c.pending = c.pending[n:]
	return n, nil
}

// CloseWrite sends our end of stream.
func (c *Conn) CloseWrite() error {
	reply, err := oceans.Call(c.handle, netproto.OpTCPShutdown, nil, nil)
	if err == nil {
		err = netproto.Status(reply.Label)
	}
	return err
}

// Close ends the connection.
func (c *Conn) Close() error {
	err := oceans.Close(c.handle)
	_ = oceans.Close(c.notification)
	if c.shared != 0 {
		_ = oceans.Close(c.shared)
	}
	return err
}
