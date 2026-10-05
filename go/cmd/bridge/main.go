// bridge: the Oceans web experience's service (ADR-0058), a Go service run
// by the Go host.
//
// It serves the web app (ui/, built by Bun and embedded) and the System
// API as JSON over HTTP/1.1 on TCP port 8080, for a browser on another
// device today (and on Oceans once it has an HTML engine). The UI is never
// needed for the system to work: everything here is also a shell command.
//
// Its own authority is small: a log, the network (to listen, and to reach
// the Store), read-only system information, the AI runtime's endpoint, its
// own storage (Store downloads, the Store's URL) and the system's root
// certificates. It holds **no authority over apps** of its own. The user's
// agent pairs it (`ui pair` in the shell): it sends a Core capability
// limited to querying, running and auditing apps and proposing installs
// (ADR-0061: Core installs only once the user confirms on the device),
// with a fresh random token the browser must present.
// `ui unpair` closes that capability; without it every API but the
// login answers 401.
//
// The service is single-threaded: IPC calls (pairing) and connections
// (signalled on a notification bound to its endpoint) are handled one at
// a time, each connection one request (`Connection: close`) within a time
// limit.
package main

import (
	"encoding/binary"
	"errors"
	"io"
	"strconv"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/fs"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/tcp"
)

// port the bridge listens on.
const port = 8080

// Bits of the notification bound to the endpoint.
const (
	connectionBit = 1 << 0
	retryBit      = 1 << 1
	// A connection waits on the web apps' port (ADR-0064).
	appConnectionBit = 1 << 2
)

// Time limits: reading one request, each write wait, and retrying the
// listener while the network starts.
const (
	requestTime   = 10 * time.Second
	writeTimeout  = 10 * time.Second
	retryInterval = 1000 // ms
	maxRetries    = 60
)

type service struct {
	log      oceans.Handle
	net      oceans.Handle
	events   oceans.Handle
	listener *tcp.Listener
	// The web apps' port (ADR-0064).
	apps    *tcp.Listener
	retries int
	// The paired Core capability; 0 while unpaired.
	core   oceans.Handle
	bridge *bridge
}

func (s *service) say(text string) {
	if s.log != 0 {
		_ = oceans.DebugWrite(s.log, "bridge: "+text)
	}
}

func main() {
	server, ok := oceans.Find("provide", "bridge")
	if !ok {
		return
	}
	s := &service{}
	s.log, _ = oceans.Find("log", "log")
	s.net, _ = oceans.Find("use", "net")
	sysinfo, _ := oceans.Find("sysinfo", "sysinfo")
	ai, _ := oceans.Find("use", "ai")
	web, err := loadSite(webFiles, "web")
	if err != nil {
		s.say("cannot read the web app: " + err.Error())
		web = &site{files: map[string]*asset{}}
	}
	sys := &oceansSystem{sysinfo: sysinfo, ai: ai, core: func() oceans.Handle { return s.core }}
	s.bridge = newBridge(sys, web, s.say)
	// The Store (ADR-0061): the network, and storage for its URL. Web
	// apps' data (ADR-0064) lives there too.
	if storage, ok := oceans.Find("use", "storage"); ok && s.net != 0 {
		s.bridge.store = newOceansStore(s.net, fs.FromHandle(storage), s.say)
		s.bridge.appData = oceansAppData{storage: fs.FromHandle(storage)}
	} else {
		s.say("no storage or network: the Store is not available")
	}
	if !web.built() {
		s.say("the web app was not built in; only the API is served")
	}
	s.events, err = oceans.NotificationCreate()
	if err == nil {
		err = oceans.EndpointBind(server, s.events)
	}
	if err != nil {
		s.say("cannot wait for connections: " + err.Error())
		return
	}
	s.listen()
	for {
		msg, err := oceans.Receive(server)
		if err == oceans.ErrPeerClosed {
			return // every client end is gone (init keeps one): shutdown
		}
		if err != nil {
			s.say("receive failed: " + err.Error())
			return
		}
		if msg.Signals != 0 {
			if msg.Signals&retryBit != 0 && s.listener == nil {
				s.listen()
			}
			if msg.Signals&connectionBit != 0 {
				s.serveWaiting(s.listener, s.bridge.serve)
			}
			if msg.Signals&appConnectionBit != 0 {
				s.serveWaiting(s.apps, s.bridge.serveApp)
			}
			continue
		}
		if msg.Closed {
			continue
		}
		label, data := s.handle(msg)
		_ = oceans.Reply(label, data, nil)
	}
}

// listen opens the port, retrying while the network service has no device
// yet.
func (s *service) listen() {
	if s.net == 0 {
		s.say("no network: the web experience is unavailable")
		return
	}
	listener, err := tcp.Listen(s.net, port, s.events, connectionBit)
	if err != nil {
		s.retries++
		if s.retries <= maxRetries && !errors.Is(err, tcp.Error(3)) {
			_ = oceans.TimerSet(s.events, retryBit, retryInterval)
			return
		}
		s.say("cannot listen on TCP port " + strconv.Itoa(port) + ": " + err.Error())
		return
	}
	s.listener = listener
	if apps, err := tcp.Listen(s.net, appPort, s.events, appConnectionBit); err == nil {
		s.apps = apps
	} else {
		s.say("cannot listen on TCP port " + strconv.Itoa(appPort) + " for web apps: " + err.Error())
	}
	files := len(s.bridge.site.files)
	s.say("serving the Oceans web experience on TCP port " + strconv.Itoa(port) + " (" +
		strconv.Itoa(files) + " files); not paired: run `ui pair` in the shell")
}

// handle answers the user's agent (the `bridge` protocol).
func (s *service) handle(msg oceans.Message) (uint64, []byte) {
	closeAll := func(handles []oceans.Handle) {
		for _, h := range handles {
			_ = oceans.Close(h)
		}
	}
	switch msg.Label {
	case opPair:
		if len(msg.Handles) != 1 || msg.Handles[0] == 0 {
			closeAll(msg.Handles)
			return statusBadRequest, nil
		}
		if err := s.bridge.pair(string(msg.Data)); err != nil {
			closeAll(msg.Handles)
			return statusBadRequest, []byte(err.Error())
		}
		if s.core != 0 {
			_ = oceans.Close(s.core)
		}
		s.core = msg.Handles[0]
		s.say("paired: a browser presenting the new code may query, run and stop apps, read the audit log and propose installs")
		if s.listener == nil {
			return statusUnavailable, nil
		}
		return statusOK, binary.LittleEndian.AppendUint16(nil, s.listener.Port())
	case opUnpair:
		closeAll(msg.Handles)
		was := s.bridge.unpair()
		if s.core != 0 {
			_ = oceans.Close(s.core)
			s.core = 0
		}
		if !was {
			return statusNotPaired, nil
		}
		s.say("unpaired: the browser's capability is closed and its code forgotten")
		return statusOK, nil
	case opStatus:
		closeAll(msg.Handles)
		data := []byte{0, 0, 0}
		if s.bridge.token != "" {
			data[0] = 1
		}
		if s.listener != nil {
			binary.LittleEndian.PutUint16(data[1:], s.listener.Port())
		}
		return statusOK, data
	}
	closeAll(msg.Handles)
	return statusBadRequest, nil
}

// serveWaiting serves every connection waiting on `listener` with
// `serve`.
func (s *service) serveWaiting(listener *tcp.Listener, serve func(*request) *response) {
	if listener == nil {
		return
	}
	for {
		conn, err := listener.Accept(writeTimeout)
		if err != nil {
			s.say("accept failed: " + err.Error())
			return
		}
		if conn == nil {
			return
		}
		s.serveConnection(conn, serve)
	}
}

func (s *service) serveConnection(conn *tcp.Conn, serve func(*request) *response) {
	defer conn.Close()
	req, err := readRequest(&deadlineReader{conn: conn, deadline: time.Now().Add(requestTime)})
	var resp *response
	var bad *httpError
	switch {
	case err == nil:
		resp = serve(req)
	case errors.As(err, &bad):
		resp = harden(failure(err))
	default:
		return // closed, reset or too slow: nobody to answer
	}
	conn.Timeout = writeTimeout
	if _, err := conn.Write(resp.encode(req != nil && req.method == "HEAD")); err == nil {
		_ = conn.CloseWrite()
	}
}

// deadlineReader bounds the whole request, not each wait: a client
// sending a byte at a time cannot hold the bridge.
type deadlineReader struct {
	conn     *tcp.Conn
	deadline time.Time
}

func (r *deadlineReader) Read(b []byte) (int, error) {
	left := time.Until(r.deadline)
	if left <= 0 {
		return 0, errDeadline
	}
	r.conn.Timeout = left
	n, err := r.conn.Read(b)
	if err == tcp.ErrTimeout {
		return n, errDeadline
	}
	if err == io.EOF && n > 0 {
		err = nil
	}
	return n, err
}
