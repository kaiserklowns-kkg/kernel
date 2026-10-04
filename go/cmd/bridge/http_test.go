package main

import (
	"errors"
	"io"
	"strings"
	"testing"
)

// trickle hands out its text a few bytes at a time, as TCP may.
type trickle struct {
	text string
	step int
}

func (t *trickle) Read(b []byte) (int, error) {
	if t.text == "" {
		return 0, io.EOF
	}
	n := copy(b[:min(len(b), t.step)], t.text)
	t.text = t.text[n:]
	return n, nil
}

func statusOf(err error) int {
	var h *httpError
	if errors.As(err, &h) {
		return h.status
	}
	return 0
}

func TestReadsARequestInPieces(t *testing.T) {
	raw := "POST /api/ai/ask?x=1 HTTP/1.1\r\nHost: oceans:8080\r\nContent-Type: application/json\r\n" +
		"Cookie: a=1\r\nCookie: oceans_session=abc\r\nContent-Length: 13\r\n\r\n{\"question\":1}"
	raw = raw[:len(raw)-1] // 13 bytes of body
	req, err := readRequest(&trickle{text: raw, step: 3})
	if err != nil {
		t.Fatal(err)
	}
	if req.method != "POST" || req.path != "/api/ai/ask" || req.query != "x=1" {
		t.Fatalf("request line: %+v", req)
	}
	if string(req.body) != `{"question":1` {
		t.Fatalf("body %q", req.body)
	}
	if req.cookie("oceans_session") != "abc" || req.cookie("a") != "1" || req.cookie("b") != "" {
		t.Fatalf("cookies %q", req.get("cookie"))
	}
}

func TestRefusesMalformedRequests(t *testing.T) {
	for _, c := range []struct {
		raw    string
		status int
	}{
		{"GET / HTTP/1.1\r\n\r\n", 400},                       // no Host
		{"GET / HTTP/2.0\r\nHost: x\r\n\r\n", 505},            // version
		{"GET  / HTTP/1.1\r\nHost: x\r\n\r\n", 400},           // request line
		{"GET http://x/ HTTP/1.1\r\nHost: x\r\n\r\n", 400},    // absolute form
		{"GET /a/../b HTTP/1.1\r\nHost: x\r\n\r\n", 400},      // dot segment
		{"GET /a%2e HTTP/1.1\r\nHost: x\r\n\r\n", 400},        // escapes
		{"GET /a\\b HTTP/1.1\r\nHost: x\r\n\r\n", 400},        // backslash
		{"GET / HTTP/1.1\r\nHost: x\r\n folded\r\n\r\n", 400}, // obs-fold
		{"GET / HTTP/1.1\r\nHost : x\r\n\r\n", 400},           // space before colon
		{"GET / HTTP/1.1\r\nHost: x\r\nHost: y\r\n\r\n", 400}, // repeated Host
		{"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nab", 400},
		{"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: -1\r\n\r\n", 400},
		{"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999\r\n\r\n", 400}, // too many digits
		{"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 20000\r\n\r\n", 413},
		{"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n", 501},
		{"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\n\r\nab", 400}, // pipelined data
		{"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nab", 400}, // short body
		{"GET / HTTP/1.1\r\nHost: x", 400},                                 // never ends
		{"GET / HTTP/1.1\r\nHost: x\r\nX: a\x01b\r\n\r\n", 400},            // control character
	} {
		_, err := readRequest(&trickle{text: c.raw, step: 7})
		if got := statusOf(err); got != c.status {
			t.Errorf("%q: status %d (%v), want %d", c.raw, got, err, c.status)
		}
	}
}

func TestBoundsTheHeader(t *testing.T) {
	raw := "GET / HTTP/1.1\r\nHost: x\r\nX: " + strings.Repeat("a", maxHeaderBytes) + "\r\n\r\n"
	if _, err := readRequest(&trickle{text: raw, step: 512}); statusOf(err) != 431 {
		t.Fatalf("got %v", err)
	}
	raw = "GET / HTTP/1.1\r\nHost: x\r\n" + strings.Repeat("X: a\r\n", maxHeaders) + "\r\n"
	if _, err := readRequest(&trickle{text: raw, step: 512}); statusOf(err) != 431 {
		t.Fatalf("got %v", err)
	}
}

func TestAnEmptyConnectionIsNotAnError(t *testing.T) {
	if _, err := readRequest(&trickle{}); err != io.EOF {
		t.Fatalf("got %v", err)
	}
}

func TestEncodesResponses(t *testing.T) {
	resp := &response{status: 200, body: []byte("hi")}
	resp.set("Content-Type", "text/plain")
	resp.set("Content-Type", "text/html")
	got := string(resp.encode(false))
	want := "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"
	if got != want {
		t.Fatalf("got %q", got)
	}
	if head := string(resp.encode(true)); !strings.HasSuffix(head, "\r\n\r\n") {
		t.Fatalf("HEAD has a body: %q", head)
	}
	empty := string((&response{status: 204}).encode(false))
	if strings.Contains(empty, "Content-Length") {
		t.Fatalf("204 with a length: %q", empty)
	}
}
