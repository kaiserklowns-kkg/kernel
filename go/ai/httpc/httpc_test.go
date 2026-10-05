package httpc

import (
	"bytes"
	"net/netip"
	"strings"
	"testing"
)

func TestParseEndpoint(t *testing.T) {
	for url, want := range map[string]Endpoint{
		"http://10.0.2.2:11434/v1/":  {Name: "10.0.2.2", Address: netip.MustParseAddr("10.0.2.2"), Port: 11434, Path: "/v1", Host: "10.0.2.2:11434"},
		"http://192.168.1.5":         {Name: "192.168.1.5", Address: netip.MustParseAddr("192.168.1.5"), Port: 80, Host: "192.168.1.5"},
		"https://api.example.com/v1": {TLS: true, Name: "api.example.com", Port: 443, Path: "/v1", Host: "api.example.com"},
		"https://models.oceans.test:8443/v1": {TLS: true, Name: "models.oceans.test", Port: 8443, Path: "/v1",
			Host: "models.oceans.test:8443"},
		"https://api.example.com:443/": {TLS: true, Name: "api.example.com", Port: 443, Host: "api.example.com"},
		"http://localhost:80/a/b":      {Name: "localhost", Port: 80, Path: "/a/b", Host: "localhost"},
		"http://[fec0::2]:8080/v1":     {Name: "fec0::2", Address: netip.MustParseAddr("fec0::2"), Port: 8080, Path: "/v1", Host: "[fec0::2]:8080"},
		"https://[2001:db8::1]":        {TLS: true, Name: "2001:db8::1", Address: netip.MustParseAddr("2001:db8::1"), Port: 443, Host: "[2001:db8::1]"},
	} {
		got, err := ParseEndpoint(url)
		if err != nil || got != want {
			t.Errorf("%s: %+v %v", url, got, err)
		}
	}
	for _, bad := range []string{
		"ftp://x", "HTTP://x", "https//x", "http://", "http:///v1", "http://1.2.3:80", "http://1.2.3.4:0",
		"http://1.2.3.400", "http://01.2.3.4", "http://1.2.3.4:", "http://1.2.3.4:+80", "http://1.2.3.4:65536",
		"http://user@host/v1", "http://host/v1?key=1", "http://host/v1#x", "http://host/v%2e1", "http://host/a b",
		"http://-host/", "http://host-.test/", "http://ho st/", "http://host./v1", "http://a..b/",
		"http://[fec0::2", "http://[fec0::2]x", "http://[fe80::1%eth0]/", "http://[::ffff:1.2.3.4]/",
		"http://[1.2.3.4]/", "http://fec0::2/", "http://host:80:80/", "http://exämple.com/",
	} {
		if e, err := ParseEndpoint(bad); err == nil {
			t.Errorf("%q accepted as %+v", bad, e)
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

func TestGetAndItsLimit(t *testing.T) {
	c := &conn{reply: strings.NewReader("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nopk")}
	status, body, err := Get(c, "h:1", "/store/t.opk", 1000)
	if err != nil || status != 200 || string(body) != "opk" {
		t.Fatalf("%d %q %v", status, body, err)
	}
	if !strings.HasPrefix(c.written.String(), "GET /store/t.opk HTTP/1.1\r\nHost: h:1\r\n") {
		t.Errorf("request: %q", c.written.String())
	}
	c = &conn{reply: strings.NewReader("HTTP/1.1 200 OK\r\n\r\n" + strings.Repeat("x", 100))}
	if _, _, err := Get(c, "h:1", "/big", 50); err == nil {
		t.Error("a response past the limit was accepted")
	}
}
