package tcp

import (
	"encoding/binary"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/netproto"
)

// net-proto operations for servers (ADR-0024).
const (
	opTCPListen = 7
	opTCPAccept = 8
)

// Listener is a TCP port the network service accepts connections on.
//
// The service signals the listener's notification bits whenever a
// connection is waiting; the owner chooses the notification (Listen), so
// a server can bind it to its endpoint and see connections arrive through
// oceans.Receive alongside its IPC calls.
type Listener struct {
	handle oceans.Handle
	port   uint16
}

// Listen opens TCP port `port` on the network service `net`; the service
// signals `bits` on `notification` when a connection waits to be
// accepted. The notification stays the caller's.
func Listen(net oceans.Handle, port uint16, notification oceans.Handle, bits uint64) (*Listener, error) {
	if port == 0 || bits == 0 {
		return nil, Error(1)
	}
	shared, err := oceans.Duplicate(notification, oceans.RightSignal|oceans.RightTransfer)
	if err != nil {
		return nil, err
	}
	data := make([]byte, 10)
	binary.LittleEndian.PutUint16(data, port)
	binary.LittleEndian.PutUint64(data[2:], bits)
	reply, err := oceans.Call(net, opTCPListen, data, []oceans.Handle{shared})
	if err == nil && reply.Label != 0 {
		err = Error(reply.Label)
	}
	if err == nil && len(reply.Handles) != 1 {
		for _, h := range reply.Handles {
			_ = oceans.Close(h)
		}
		err = Error(1)
	}
	if err != nil {
		return nil, err
	}
	return &Listener{handle: reply.Handles[0], port: port}, nil
}

// Port is the port listened on.
func (l *Listener) Port() uint16 { return l.port }

// Accept returns a waiting connection, or nil (and no error) when none
// waits. The connection has its own notification; `timeout` bounds each
// wait in its Read and Write.
func (l *Listener) Accept(timeout time.Duration) (*Conn, error) {
	notification, err := oceans.NotificationCreate()
	if err != nil {
		return nil, err
	}
	shared, err := oceans.Duplicate(notification, oceans.RightSignal|oceans.RightTransfer)
	if err != nil {
		_ = oceans.Close(notification)
		return nil, err
	}
	reply, err := oceans.Call(l.handle, opTCPAccept, binary.LittleEndian.AppendUint64(nil, netproto.Readable),
		[]oceans.Handle{shared})
	if err == nil && reply.Label == uint64(netproto.StatusEmpty) {
		_ = oceans.Close(notification)
		return nil, nil
	}
	if err == nil && reply.Label != 0 {
		err = Error(reply.Label)
	}
	if err == nil && len(reply.Handles) != 1 {
		for _, h := range reply.Handles {
			_ = oceans.Close(h)
		}
		err = Error(1)
	}
	if err != nil {
		_ = oceans.Close(notification)
		return nil, err
	}
	return &Conn{handle: reply.Handles[0], notification: notification, Timeout: timeout}, nil
}

// Close stops listening; connections not yet accepted are reset.
func (l *Listener) Close() error {
	return oceans.Close(l.handle)
}
