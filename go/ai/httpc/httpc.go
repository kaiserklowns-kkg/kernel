// Package httpc is a small HTTP/1.1 client for the model gateway
// (ADR-0051): one POST per connection (`Connection: close`), responses with
// Content-Length, chunked transfer, or ended by the close.
package httpc

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"net/netip"
	"strconv"
	"strings"
)

// Endpoint is a parsed `http://` or `https://` URL of a model server.
type Endpoint struct {
	// TLS: an https:// URL.
	TLS bool
	// Name is the host as written: a DNS name, or an address literal
	// (IPv6 without its brackets). It is what TLS verifies.
	Name string
	// Address is the host's address when Name is a literal (else invalid:
	// the name is resolved when connecting).
	Address netip.Addr
	Port    uint16
	// Path prefix, without a trailing slash ("" or "/v1").
	Path string
	// Host header value: the host as written, with the port unless it is
	// the scheme's default.
	Host string
}

// ParseEndpoint accepts `http[s]://HOST[:PORT][/PATH]`, HOST a DNS name,
// an IPv4 address or a bracketed IPv6 address. It refuses what a model
// server URL has no use for and could hide another destination: user
// information, queries, fragments, zones, percent-escapes and characters
// outside the path's plain ASCII set.
func ParseEndpoint(url string) (Endpoint, error) {
	var e Endpoint
	rest, ok := strings.CutPrefix(url, "http://")
	if !ok {
		if rest, ok = strings.CutPrefix(url, "https://"); !ok {
			return Endpoint{}, errors.New("the URL must start with http:// or https://")
		}
		e.TLS = true
	}
	authority, path, _ := strings.Cut(rest, "/")
	if strings.ContainsAny(authority, "@?#%") {
		return Endpoint{}, fmt.Errorf("%q: only a host and a port may come before the path", authority)
	}
	host, portText, hasPort := authority, "", false
	if strings.HasPrefix(authority, "[") {
		end := strings.IndexByte(authority, ']')
		if end < 0 {
			return Endpoint{}, fmt.Errorf("%q: an IPv6 address must end with ]", authority)
		}
		host, rest = authority[1:end], authority[end+1:]
		if rest != "" {
			if portText, hasPort = strings.CutPrefix(rest, ":"); !hasPort {
				return Endpoint{}, fmt.Errorf("%q: expected :PORT after the address", authority)
			}
		}
		address, err := netip.ParseAddr(host)
		if err != nil || !address.Is6() || address.Is4In6() {
			return Endpoint{}, fmt.Errorf("%q is not an IPv6 address", host)
		}
		e.Address = address
	} else {
		host, portText, hasPort = strings.Cut(authority, ":")
		if allDigitsAndDots(host) {
			address, err := netip.ParseAddr(host)
			if err != nil || !address.Is4() {
				return Endpoint{}, fmt.Errorf("%q is not an IPv4 address", host)
			}
			e.Address = address
		} else if !validHostName(host) {
			return Endpoint{}, fmt.Errorf("%q is not a valid host name", host)
		}
	}
	e.Name = host
	e.Port = 80
	if e.TLS {
		e.Port = 443
	}
	if hasPort {
		if !allDigits(portText) {
			return Endpoint{}, fmt.Errorf("bad port %q", portText)
		}
		port, err := strconv.ParseUint(portText, 10, 16)
		if err != nil || port == 0 {
			return Endpoint{}, fmt.Errorf("bad port %q", portText)
		}
		e.Port = uint16(port)
	}
	for _, c := range []byte(path) {
		if !pathByte(c) {
			return Endpoint{}, fmt.Errorf("the path %q has a character URLs here may not use", "/"+path)
		}
	}
	e.Path = strings.TrimSuffix("/"+path, "/")
	e.Host = authority
	if hasPort && (e.Port == 80 && !e.TLS || e.Port == 443 && e.TLS) {
		e.Host = strings.TrimSuffix(authority, ":"+portText)
	}
	return e, nil
}

func allDigits(s string) bool {
	for _, c := range []byte(s) {
		if c < '0' || c > '9' {
			return false
		}
	}
	return s != ""
}

func allDigitsAndDots(s string) bool {
	for _, c := range []byte(s) {
		if (c < '0' || c > '9') && c != '.' {
			return false
		}
	}
	return s != ""
}

// validHostName: dot-separated labels of 1–63 letters, digits and
// hyphens (not at either end), at most 253 characters, no trailing dot.
func validHostName(name string) bool {
	if name == "" || len(name) > 253 {
		return false
	}
	for label := range strings.SplitSeq(name, ".") {
		if len(label) < 1 || len(label) > 63 || label[0] == '-' || label[len(label)-1] == '-' {
			return false
		}
		for _, c := range []byte(label) {
			if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_') {
				return false
			}
		}
	}
	return true
}

// pathByte: unreserved characters, sub-delimiters, ':', '@' and '/'
// (RFC 3986 pchar, without percent-escapes).
func pathByte(c byte) bool {
	switch {
	case c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z', c >= '0' && c <= '9':
		return true
	}
	return strings.IndexByte("-._~!$&'()*+,;=:@/", c) >= 0
}

// MaxResponse bounds a response.
const MaxResponse = 4 << 20

// Post sends `body` as JSON to `path` over `conn` and returns the status
// and body of the response.
func Post(conn io.ReadWriter, host, path string, body []byte) (int, []byte, error) {
	var request bytes.Buffer
	fmt.Fprintf(&request, "POST %s HTTP/1.1\r\nHost: %s\r\nContent-Type: application/json\r\n"+
		"Accept: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n", path, host, len(body))
	request.Write(body)
	return exchange(conn, request.Bytes(), MaxResponse)
}

// Get fetches `path` over `conn`; the response may be up to `limit` bytes
// (a package from the Store, ADR-0061).
func Get(conn io.ReadWriter, host, path string, limit int) (int, []byte, error) {
	request := fmt.Sprintf("GET %s HTTP/1.1\r\nHost: %s\r\nAccept: */*\r\nConnection: close\r\n\r\n", path, host)
	return exchange(conn, []byte(request), limit)
}

// exchange sends a request and reads the whole response (the server
// closes the connection after it).
func exchange(conn io.ReadWriter, request []byte, limit int) (int, []byte, error) {
	if _, err := conn.Write(request); err != nil {
		return 0, nil, fmt.Errorf("sending the request: %w", err)
	}
	data, err := io.ReadAll(io.LimitReader(conn, int64(limit)+1))
	if err != nil {
		return 0, nil, fmt.Errorf("reading the response: %w", err)
	}
	if len(data) > limit {
		return 0, nil, errors.New("the response is too large")
	}
	return ParseResponse(data)
}

// ParseResponse splits a complete HTTP/1.1 response.
func ParseResponse(data []byte) (int, []byte, error) {
	head, body, ok := bytes.Cut(data, []byte("\r\n\r\n"))
	if !ok {
		return 0, nil, errors.New("the response ended inside its headers")
	}
	lines := strings.Split(string(head), "\r\n")
	fields := strings.Fields(lines[0])
	if len(fields) < 2 || !strings.HasPrefix(fields[0], "HTTP/1.") {
		return 0, nil, fmt.Errorf("not an HTTP response: %q", lines[0])
	}
	status, err := strconv.Atoi(fields[1])
	if err != nil {
		return 0, nil, fmt.Errorf("bad status %q", fields[1])
	}
	length, chunked := -1, false
	for _, line := range lines[1:] {
		name, value, _ := strings.Cut(line, ":")
		value = strings.TrimSpace(value)
		switch strings.ToLower(strings.TrimSpace(name)) {
		case "content-length":
			if length, err = strconv.Atoi(value); err != nil || length < 0 {
				return 0, nil, fmt.Errorf("bad Content-Length %q", value)
			}
		case "transfer-encoding":
			chunked = strings.EqualFold(value, "chunked")
		}
	}
	switch {
	case chunked:
		body, err = dechunk(body)
		if err != nil {
			return 0, nil, err
		}
	case length >= 0:
		if len(body) < length {
			return 0, nil, errors.New("the response body is shorter than its Content-Length")
		}
		body = body[:length]
	}
	return status, body, nil
}

func dechunk(data []byte) ([]byte, error) {
	var out []byte
	for {
		line, rest, ok := bytes.Cut(data, []byte("\r\n"))
		if !ok {
			return nil, errors.New("a chunk header is cut off")
		}
		sizeText, _, _ := strings.Cut(string(line), ";")
		size, err := strconv.ParseUint(strings.TrimSpace(sizeText), 16, 32)
		if err != nil {
			return nil, fmt.Errorf("bad chunk size %q", sizeText)
		}
		if size == 0 {
			return out, nil
		}
		if uint64(len(rest)) < size+2 {
			return nil, errors.New("a chunk is cut off")
		}
		out = append(out, rest[:size]...)
		data = rest[size+2:]
	}
}
