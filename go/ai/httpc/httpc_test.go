package httpc

import (
	"bytes"
	"strings"
	"testing"
)

func TestParseEndpoint(t *testing.T) {
	e, err := ParseEndpoint("http://10.0.2.2:11434/v1/")
	if err != nil || e.Address != [4]byte{10, 0, 2, 2} || e.Port != 11434 || e.Path != "/v1" || e.Host != "10.0.2.2:11434" {
		t.Fatalf("%+v %v", e, err)
	}
	e, err = ParseEndpoint("http://192.168.1.5")
	if err != nil || e.Port != 80 || e.Path != "" {
		t.Fatalf("%+v %v", e, err)
	}
	for _, bad := range []string{"https://api.example.com/v1", "ftp://x", "http://example.com/v1", "http://1.2.3:80", "http://1.2.3.4:0", "http://1.2.3.400"} {
		if _, err := ParseEndpoint(bad); err == nil {
			t.Errorf("%q accepted", bad)
		}
	}
}

// conn records what is written and serves a canned response.
type conn struct {
	written bytes.Buffer
	reply   *strings.Reader
}

func (c *conn) Write(b []byte) (int, error) { return c.written.Write(b) }
func (c *conn) Read(b []byte) (int, error)  { return c.reply.Read(b) }

func TestPost(t *testing.T) {
	c := &conn{reply: strings.NewReader("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello trailing")}
	status, body, err := Post(c, "h:1", "/v1/chat", []byte(`{}`))
	if err != nil || status != 200 || string(body) != "hello" {
		t.Fatalf("%d %q %v", status, body, err)
	}
	sent := c.written.String()
	for _, want := range []string{"POST /v1/chat HTTP/1.1\r\n", "Host: h:1\r\n", "Content-Length: 2\r\n", "Connection: close\r\n", "\r\n\r\n{}"} {
		if !strings.Contains(sent, want) {
			t.Errorf("request lacks %q:\n%s", want, sent)
		}
	}
}

func TestParseResponse(t *testing.T) {
	status, body, err := ParseResponse([]byte("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nwiki\r\n5;x=1\r\npedia\r\n0\r\n\r\n"))
	if err != nil || status != 200 || string(body) != "wikipedia" {
		t.Fatalf("chunked: %d %q %v", status, body, err)
	}
	status, body, err = ParseResponse([]byte("HTTP/1.0 404 Not Found\r\n\r\nuntil the end"))
	if err != nil || status != 404 || string(body) != "until the end" {
		t.Fatalf("close-delimited: %d %q %v", status, body, err)
	}
	for _, bad := range []string{"garbage", "HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort", "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", "SMTP 200\r\n\r\n"} {
		if _, _, err := ParseResponse([]byte(bad)); err == nil {
			t.Errorf("%q accepted", bad)
		}
	}
}
