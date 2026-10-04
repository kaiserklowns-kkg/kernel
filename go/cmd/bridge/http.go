package main

import (
	"bytes"
	"errors"
	"io"
	"strconv"
	"strings"
)

// A small, strict HTTP/1.1 server side (RFC 9112): one request per
// connection (`Connection: close`), bounded sizes, no request bodies but
// Content-Length ones. Anything unusual is refused rather than guessed at,
// so no two parsers can read one request differently.

// Limits of one request.
const (
	maxHeaderBytes = 8 << 10
	maxHeaders     = 64
	maxBodyBytes   = 16 << 10
)

type request struct {
	method string
	// path is the target without its query.
	path  string
	query string
	// header holds the fields by lower-case name; a repeated field keeps
	// its values joined by ", " (RFC 9110 §5.3), Cookie by "; ".
	header map[string]string
	body   []byte
}

func (r *request) get(name string) string { return r.header[name] }

// httpError is a request that cannot be served: the status to answer.
type httpError struct {
	status int
	reason string
}

func (e *httpError) Error() string { return e.reason }

func badRequest(reason string) error { return &httpError{400, reason} }

// readRequest reads one request from `r`.
func readRequest(r io.Reader) (*request, error) {
	buf := make([]byte, 0, 1024)
	chunk := make([]byte, 512)
	end := -1
	for end < 0 {
		if len(buf) >= maxHeaderBytes {
			return nil, &httpError{431, "request header too large"}
		}
		n, err := r.Read(chunk[:min(len(chunk), maxHeaderBytes-len(buf))])
		buf = append(buf, chunk[:n]...)
		end = bytes.Index(buf, []byte("\r\n\r\n"))
		if end >= 0 {
			break
		}
		if err == io.EOF {
			if len(buf) == 0 {
				return nil, io.EOF
			}
			return nil, badRequest("incomplete request")
		}
		if err != nil {
			return nil, err
		}
	}
	req, err := parseHead(string(buf[:end]))
	if err != nil {
		return nil, err
	}
	rest := buf[end+4:]
	length, err := contentLength(req)
	if err != nil {
		return nil, err
	}
	if len(rest) > length {
		// Pipelined requests are not served (one per connection).
		return nil, badRequest("data after the request")
	}
	body := make([]byte, length)
	copy(body, rest)
	if _, err := io.ReadFull(r, body[len(rest):]); err != nil {
		if err == io.EOF || err == io.ErrUnexpectedEOF {
			return nil, badRequest("incomplete body")
		}
		return nil, err
	}
	req.body = body
	return req, nil
}

func parseHead(head string) (*request, error) {
	lines := strings.Split(head, "\r\n")
	parts := strings.Split(lines[0], " ")
	if len(parts) != 3 {
		return nil, badRequest("malformed request line")
	}
	method, target, version := parts[0], parts[1], parts[2]
	if version != "HTTP/1.1" && version != "HTTP/1.0" {
		return nil, &httpError{505, "HTTP/1.1 only"}
	}
	if method == "" || !isToken(method) {
		return nil, badRequest("malformed method")
	}
	if !strings.HasPrefix(target, "/") || len(target) > 1024 {
		return nil, badRequest("malformed target")
	}
	path, query, _ := strings.Cut(target, "?")
	if !cleanPath(path) {
		return nil, badRequest("malformed path")
	}
	req := &request{method: method, path: path, query: query, header: map[string]string{}}
	if len(lines)-1 > maxHeaders {
		return nil, &httpError{431, "too many header fields"}
	}
	for _, line := range lines[1:] {
		name, value, ok := strings.Cut(line, ":")
		// No obsolete line folding, no space before the colon.
		if !ok || name == "" || !isToken(name) {
			return nil, badRequest("malformed header field")
		}
		value = strings.Trim(value, " \t")
		if !fieldValue(value) {
			return nil, badRequest("malformed header value")
		}
		name = strings.ToLower(name)
		if old, seen := req.header[name]; seen {
			switch name {
			case "content-length", "host", "authorization", "content-type", "origin":
				return nil, badRequest("repeated " + name)
			case "cookie":
				value = old + "; " + value
			default:
				value = old + ", " + value
			}
		}
		req.header[name] = value
	}
	if version == "HTTP/1.1" && req.get("host") == "" {
		return nil, badRequest("no Host")
	}
	return req, nil
}

func contentLength(req *request) (int, error) {
	if _, ok := req.header["transfer-encoding"]; ok {
		return 0, &httpError{501, "Transfer-Encoding is not supported"}
	}
	text, ok := req.header["content-length"]
	if !ok {
		return 0, nil
	}
	if text == "" || len(text) > 7 || strings.Trim(text, "0123456789") != "" {
		return 0, badRequest("malformed Content-Length")
	}
	n, err := strconv.Atoi(text)
	if err != nil {
		return 0, badRequest("malformed Content-Length")
	}
	if n > maxBodyBytes {
		return 0, &httpError{413, "request body too large"}
	}
	return n, nil
}

// cleanPath: a path of plain segments; no escapes, dot segments,
// backslashes or control characters, so it means exactly what it says.
func cleanPath(path string) bool {
	if strings.ContainsAny(path, "%\\") || strings.Contains(path, "//") {
		return false
	}
	for _, segment := range strings.Split(path, "/") {
		if segment == "." || segment == ".." {
			return false
		}
	}
	for i := 0; i < len(path); i++ {
		if path[i] <= ' ' || path[i] >= 0x7f {
			return false
		}
	}
	return true
}

// isToken: RFC 9110 token characters.
func isToken(s string) bool {
	for i := 0; i < len(s); i++ {
		c := s[i]
		ok := c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' ||
			strings.IndexByte("!#$%&'*+-.^_`|~", c) >= 0
		if !ok {
			return false
		}
	}
	return true
}

// fieldValue: visible ASCII, spaces and tabs (no control characters).
func fieldValue(s string) bool {
	for i := 0; i < len(s); i++ {
		if c := s[i]; (c < ' ' && c != '\t') || c == 0x7f {
			return false
		}
	}
	return true
}

// cookie returns the value of cookie `name`, or "".
func (r *request) cookie(name string) string {
	for _, part := range strings.Split(r.get("cookie"), ";") {
		key, value, ok := strings.Cut(strings.TrimSpace(part), "=")
		if ok && key == name {
			return value
		}
	}
	return ""
}

type response struct {
	status int
	header [][2]string
	body   []byte
}

func (r *response) get(name string) (string, bool) {
	for _, field := range r.header {
		if field[0] == name {
			return field[1], true
		}
	}
	return "", false
}

func (r *response) set(name, value string) {
	for i := range r.header {
		if r.header[i][0] == name {
			r.header[i][1] = value
			return
		}
	}
	r.header = append(r.header, [2]string{name, value})
}

var statusText = map[int]string{
	200: "OK", 204: "No Content", 304: "Not Modified", 400: "Bad Request", 401: "Unauthorized",
	403: "Forbidden", 404: "Not Found", 405: "Method Not Allowed", 409: "Conflict",
	413: "Content Too Large", 415: "Unsupported Media Type", 422: "Unprocessable Content",
	431: "Request Header Fields Too Large", 500: "Internal Server Error",
	501: "Not Implemented", 502: "Bad Gateway", 503: "Service Unavailable",
	505: "HTTP Version Not Supported",
}

// encode returns the response's bytes; `head` leaves out the body (HEAD).
func (r *response) encode(head bool) []byte {
	text, ok := statusText[r.status]
	if !ok {
		text = "Status"
	}
	var out bytes.Buffer
	out.WriteString("HTTP/1.1 " + strconv.Itoa(r.status) + " " + text + "\r\n")
	for _, field := range r.header {
		out.WriteString(field[0] + ": " + field[1] + "\r\n")
	}
	if r.status != 204 && r.status != 304 {
		out.WriteString("Content-Length: " + strconv.Itoa(len(r.body)) + "\r\n")
	}
	out.WriteString("Connection: close\r\n\r\n")
	if !head && r.status != 204 && r.status != 304 {
		out.Write(r.body)
	}
	return out.Bytes()
}

var errDeadline = errors.New("request took too long")
