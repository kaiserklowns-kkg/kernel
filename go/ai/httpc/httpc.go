// Package httpc is a small HTTP/1.1 client for the model gateway
// (ADR-0051): one POST per connection (`Connection: close`), responses with
// Content-Length, chunked transfer, or ended by the close.
package httpc

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"
)

// Endpoint is a parsed `http://ADDRESS:PORT/PATH` URL.
type Endpoint struct {
	// IPv4 address (names need DNS, which the gateway does not do yet).
	Address [4]byte
	Port    uint16
	// Path prefix, without a trailing slash ("" or "/v1").
	Path string
	// Host header value.
	Host string
}

// ParseEndpoint accepts `http://A.B.C.D[:PORT][/PATH]`.
func ParseEndpoint(url string) (Endpoint, error) {
	rest, ok := strings.CutPrefix(url, "http://")
	if !ok {
		if strings.HasPrefix(url, "https://") {
			return Endpoint{}, errors.New("https is not supported yet (use a local model server over http)")
		}
		return Endpoint{}, errors.New("the URL must start with http://")
	}
	hostPort, path, _ := strings.Cut(rest, "/")
	host, portText, hasPort := strings.Cut(hostPort, ":")
	port := uint64(80)
	if hasPort {
		var err error
		if port, err = strconv.ParseUint(portText, 10, 16); err != nil || port == 0 {
			return Endpoint{}, fmt.Errorf("bad port %q", portText)
		}
	}
	parts := strings.Split(host, ".")
	if len(parts) != 4 {
		return Endpoint{}, fmt.Errorf("%q is not an IPv4 address", host)
	}
	var address [4]byte
	for i, part := range parts {
		n, err := strconv.ParseUint(part, 10, 8)
		if err != nil {
			return Endpoint{}, fmt.Errorf("%q is not an IPv4 address", host)
		}
		address[i] = byte(n)
	}
	path = strings.TrimSuffix(path, "/")
	if path != "" {
		path = "/" + path
	}
	return Endpoint{Address: address, Port: uint16(port), Path: path, Host: hostPort}, nil
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
	if _, err := conn.Write(request.Bytes()); err != nil {
		return 0, nil, fmt.Errorf("sending the request: %w", err)
	}
	data, err := io.ReadAll(io.LimitReader(conn, MaxResponse+1))
	if err != nil {
		return 0, nil, fmt.Errorf("reading the response: %w", err)
	}
	if len(data) > MaxResponse {
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
