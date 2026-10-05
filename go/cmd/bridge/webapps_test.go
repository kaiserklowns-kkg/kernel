package main

import (
	"encoding/binary"
	"strings"
	"testing"
)

// bundle writes a web bundle as libs/package's `web::write` does.
func bundle(files ...[2]string) []byte {
	out := []byte(bundleMagic)
	out = binary.LittleEndian.AppendUint32(out, bundleVersion)
	out = binary.LittleEndian.AppendUint32(out, uint32(len(files)))
	for _, f := range files {
		out = binary.LittleEndian.AppendUint16(out, uint16(len(f[0])))
		out = append(out, f[0]...)
		out = binary.LittleEndian.AppendUint32(out, uint32(len(f[1])))
		out = append(out, f[1]...)
	}
	return out
}

const appPageHTML = `<!doctype html><html><head>` +
	`<meta http-equiv="content-security-policy" content="default-src 'self'; script-src 'self'">` +
	`</head><body><script type="module" src="/app.example.notes/_app/immutable/start.js"></script></body></html>`

// memoryData is app data in memory.
type memoryData map[string][]byte

func (m memoryData) Get(id, name string) ([]byte, error) {
	if data, ok := m[id+"/"+name]; ok {
		return data, nil
	}
	return nil, errNoData
}

func (m memoryData) Put(id, name string, data []byte) error {
	m[id+"/"+name] = append([]byte(nil), data...)
	return nil
}

func newWebBridge(t *testing.T) (*bridge, *fake, memoryData) {
	b, sys, _ := newTestBridge(t)
	sys.webApp = true
	sys.bundle = bundle(
		[2]string{"index.html", appPageHTML},
		[2]string{"_app/immutable/start.js", "export {}"},
	)
	data := memoryData{}
	b.appData = data
	return b, sys, data
}

func app(b *bridge, method, path, auth, body string, extra ...string) *response {
	req := &request{method: method, path: path, header: map[string]string{"host": "oceans:8081"}, body: []byte(body)}
	if auth != "" {
		req.header["cookie"] = sessionCookie + "=" + auth
	}
	for i := 0; i+1 < len(extra); i += 2 {
		req.header[extra[i]] = extra[i+1]
	}
	return b.serveApp(req)
}

func tokenOf(t *testing.T, resp *response) string {
	t.Helper()
	_, rest, ok := strings.Cut(string(resp.body), `name="oceans-app-token" content="`)
	if !ok {
		t.Fatalf("no token in the page: %s", resp.body)
	}
	token, _, _ := strings.Cut(rest, `"`)
	return token
}

func TestBundlesAreChecked(t *testing.T) {
	files, err := parseBundle(bundle([2]string{"index.html", "x"}, [2]string{"a/b.js", "y"}))
	if err != nil || string(files["a/b.js"]) != "y" {
		t.Fatalf("%v %v", files, err)
	}
	for _, bad := range [][]byte{
		bundle([2]string{"../x", "1"}),
		bundle([2]string{"/abs", "1"}),
		bundle([2]string{".env", "1"}),
		bundle([2]string{"a//b", "1"}),
		bundle([2]string{"a.js", "1"}, [2]string{"a.js", "2"}),
		append(bundle([2]string{"a.js", "1"}), 0),
		bundle([2]string{"a.js", "1"})[:20],
		[]byte("not a bundle at all"),
	} {
		if _, err := parseBundle(bad); err == nil {
			t.Errorf("accepted % x", bad)
		}
	}
	if _, err := bundleSite(bundle([2]string{"a.js", "1"})); err == nil {
		t.Error("a site without index.html was accepted")
	}
}

func TestAppPagesAreForThePairedBrowserSandboxedAndTokened(t *testing.T) {
	b, _, _ := newWebBridge(t)
	if resp := app(b, "GET", "/app.example.notes/", "", ""); resp.status != 401 {
		t.Fatalf("unpaired: %d", resp.status)
	}
	resp := app(b, "GET", "/app.example.notes/", testToken, "")
	if resp.status != 200 {
		t.Fatalf("%d %s", resp.status, resp.body)
	}
	csp := header(resp, "Content-Security-Policy")
	if !strings.HasPrefix(csp, "default-src 'self'") || !strings.Contains(csp, "sandbox allow-scripts allow-forms") ||
		strings.Contains(csp, "allow-same-origin") {
		t.Fatalf("policy: %s", csp)
	}
	first := tokenOf(t, resp)
	// Its routes get the same page and token; files are public and CORS.
	if again := app(b, "GET", "/app.example.notes/notes/today", testToken, ""); tokenOf(t, again) != first {
		t.Fatal("another token for the same app")
	}
	script := app(b, "GET", "/app.example.notes/_app/immutable/start.js", "", "")
	if script.status != 200 || header(script, "Access-Control-Allow-Origin") != "*" ||
		header(script, "Cross-Origin-Resource-Policy") != "cross-origin" {
		t.Fatalf("script: %d %v", script.status, script.header)
	}
	if resp := app(b, "GET", "/app.example.notes/missing.js", "", ""); resp.status != 404 {
		t.Fatalf("missing file: %d", resp.status)
	}
	if resp := app(b, "GET", "/app.oceans.hello/", testToken, ""); resp.status != 404 {
		t.Fatalf("a native app as a web app: %d", resp.status)
	}
	// Unpairing ends the tokens.
	b.unpair()
	put := app(b, "PUT", "/app.example.notes/api/data/note", "", "x", "authorization", "Bearer "+first)
	if put.status != 403 {
		t.Fatalf("after unpairing: %d", put.status)
	}
}

func TestTheAppAPIReachesOnlyTheAppsOwnData(t *testing.T) {
	b, _, data := newWebBridge(t)
	token := tokenOf(t, app(b, "GET", "/app.example.notes/", testToken, ""))
	auth := "Bearer " + token
	if resp := app(b, "OPTIONS", "/app.example.notes/api/data/note", "", ""); resp.status != 204 ||
		!strings.Contains(header(resp, "Access-Control-Allow-Headers"), "authorization") {
		t.Fatalf("preflight: %d", resp.status)
	}
	if resp := app(b, "PUT", "/app.example.notes/api/data/note", "", "remember", "authorization", auth); resp.status != 204 {
		t.Fatalf("put: %d %s", resp.status, resp.body)
	}
	if resp := app(b, "GET", "/app.example.notes/api/data/note", "", "", "authorization", auth); resp.status != 200 ||
		string(resp.body) != "remember" {
		t.Fatalf("get: %d %s", resp.status, resp.body)
	}
	if string(data["app.example.notes/note"]) != "remember" {
		t.Fatalf("stored: %v", data)
	}
	for _, c := range []struct{ path, auth string }{
		{"/app.example.notes/api/data/note", ""},
		{"/app.example.notes/api/data/note", "Bearer " + strings.Repeat("0", 32)},
		{"/app.example.notes/api/data/note", "Bearer " + testToken},
		{"/app.oceans.hello/api/data/note", auth},
	} {
		if resp := app(b, "GET", c.path, "", "", "authorization", c.auth); resp.status != 403 {
			t.Errorf("%s with %q: %d", c.path, c.auth, resp.status)
		}
	}
	if resp := app(b, "PUT", "/app.example.notes/api/data/../x", "", "x", "authorization", auth); resp.status == 204 {
		t.Error("a bad name was accepted")
	}
	if resp := app(b, "GET", "/app.example.notes/api/data/other", "", "", "authorization", auth); resp.status != 404 {
		t.Errorf("nothing stored: %d", resp.status)
	}
}
