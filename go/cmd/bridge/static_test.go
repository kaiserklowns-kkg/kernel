package main

import (
	"strings"
	"testing"
	"testing/fstest"
)

const page = `<!doctype html><html><head>` +
	`<meta http-equiv="content-security-policy" content="default-src &#39;self&#39;; script-src &#39;self&#39; &#39;sha256-abc=&#39;">` +
	`<title>Oceans</title></head><body><script>start()</script></body></html>`

func testSite(t *testing.T) *site {
	t.Helper()
	s, err := loadSite(fstest.MapFS{
		"web/.gitkeep":                  {},
		"web/index.html":                {Data: []byte(page)},
		"web/index.html.gz":             {Data: []byte("gzipped page")},
		"web/index.html.br":             {Data: []byte("brotli page")},
		"web/favicon.svg":               {Data: []byte("<svg/>")},
		"web/_app/immutable/app.abc.js": {Data: []byte("app()")},
		"web/_app/version.json":         {Data: []byte(`{}`)},
	}, "web")
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func fetch(s *site, path string, headers ...string) *response {
	req := &request{method: "GET", path: path, header: map[string]string{}}
	for i := 0; i+1 < len(headers); i += 2 {
		req.header[headers[i]] = headers[i+1]
	}
	return harden(s.serve(req))
}

func TestServesTheApp(t *testing.T) {
	s := testSite(t)
	if len(s.files) != 4 {
		t.Fatalf("files %v", s.files)
	}
	resp := fetch(s, "/")
	if resp.status != 200 || string(resp.body) != page || header(resp, "Content-Type") != "text/html; charset=utf-8" {
		t.Fatalf("%d %q", resp.status, resp.body)
	}
	csp := header(resp, "Content-Security-Policy")
	if csp != "default-src 'self'; script-src 'self' 'sha256-abc='; frame-ancestors 'none'" {
		t.Fatalf("csp %q", csp)
	}
	if header(resp, "Cache-Control") != "no-cache" || header(resp, "X-Frame-Options") != "DENY" {
		t.Fatal("headers")
	}
	// The app's own routes get the page.
	for _, route := range []string{"/apps", "/ai", "/settings"} {
		if resp := fetch(s, route); resp.status != 200 || string(resp.body) != page {
			t.Errorf("%s: %d", route, resp.status)
		}
	}
	if resp := fetch(s, "/missing.js"); resp.status != 404 {
		t.Fatalf("missing file: %d", resp.status)
	}
	if resp := fetch(s, "/.gitkeep"); resp.status != 404 {
		t.Fatalf("dot file: %d", resp.status)
	}
	js := fetch(s, "/_app/immutable/app.abc.js")
	if js.status != 200 || !strings.Contains(header(js, "Cache-Control"), "immutable") ||
		header(js, "Content-Type") != "text/javascript; charset=utf-8" {
		t.Fatalf("immutable: %d %v", js.status, js.header)
	}
	if resp := harden(s.serve(&request{method: "POST", path: "/", header: map[string]string{}})); resp.status != 405 {
		t.Fatalf("POST: %d", resp.status)
	}
}

func TestServesGzipToThoseWhoAccept(t *testing.T) {
	s := testSite(t)
	for _, c := range []struct {
		accept string
		gzip   bool
	}{
		{"", false}, {"gzip", true}, {"br, gzip;q=0.8", true}, {"gzip;q=0", false}, {"identity", false},
	} {
		resp := fetch(s, "/", "accept-encoding", c.accept)
		if got := header(resp, "Content-Encoding") == "gzip"; got != c.gzip {
			t.Errorf("%q: gzip %v", c.accept, got)
		}
		if header(resp, "Vary") != "Accept-Encoding" {
			t.Errorf("%q: no Vary", c.accept)
		}
	}
}

func TestAPageWithoutAPolicyGetsAStrictOne(t *testing.T) {
	for _, body := range []string{
		"<html><script>x()</script></html>",
		`<meta http-equiv="content-security-policy" content="script-src *">`,
	} {
		policy := pagePolicy(&asset{body: []byte(body)})
		if policy != strictPolicy+"; frame-ancestors 'none'" {
			t.Errorf("%q: %q", body, policy)
		}
	}
}

func TestAnUnbuiltAppSaysSo(t *testing.T) {
	s, err := loadSite(fstest.MapFS{"web/.gitkeep": {}}, "web")
	if err != nil {
		t.Fatal(err)
	}
	if resp := fetch(s, "/"); resp.status != 503 || !strings.Contains(string(resp.body), "not built") {
		t.Fatalf("%d %s", resp.status, resp.body)
	}
	// The embedded directory always loads.
	if _, err := loadSite(webFiles, "web"); err != nil {
		t.Fatal(err)
	}
}
