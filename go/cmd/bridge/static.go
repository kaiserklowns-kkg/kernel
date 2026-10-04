package main

import (
	"embed"
	"io/fs"
	"path"
	"strings"
)

// The web experience (ui/, ADR-0058): the SvelteKit app built by Bun as
// static files, which xtask copies here before building the bridge.
// `web/.gitkeep` keeps the directory (and this embed) valid when the app
// was not built; the bridge then says so.
//
//go:embed all:web
var webFiles embed.FS

type asset struct {
	body        []byte
	gzip        []byte
	contentType string
	// Under /_app/immutable/: named by content hash, cached for good.
	immutable bool
}

type site struct {
	files map[string]*asset
	// The page's policy: its own origin only, plus the hashes of the
	// inline bootstrap scripts SvelteKit writes into index.html.
	csp string
}

var contentTypes = map[string]string{
	".html":        "text/html; charset=utf-8",
	".js":          "text/javascript; charset=utf-8",
	".mjs":         "text/javascript; charset=utf-8",
	".css":         "text/css; charset=utf-8",
	".json":        "application/json; charset=utf-8",
	".svg":         "image/svg+xml",
	".png":         "image/png",
	".ico":         "image/x-icon",
	".woff2":       "font/woff2",
	".txt":         "text/plain; charset=utf-8",
	".webmanifest": "application/manifest+json",
}

// loadSite reads the files under `root` of `files`. Brotli copies are
// skipped; `NAME.gz` is NAME compressed, served to clients that accept it.
func loadSite(files fs.FS, root string) (*site, error) {
	s := &site{files: map[string]*asset{}}
	compressed := map[string][]byte{}
	err := fs.WalkDir(files, root, func(name string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		base := path.Base(name)
		if entry.IsDir() || strings.HasPrefix(base, ".") || strings.HasSuffix(name, ".br") {
			return nil
		}
		body, err := fs.ReadFile(files, name)
		if err != nil {
			return err
		}
		url := strings.TrimPrefix(name, root)
		if original, ok := strings.CutSuffix(url, ".gz"); ok {
			compressed[original] = body
			return nil
		}
		kind, ok := contentTypes[path.Ext(url)]
		if !ok {
			kind = "application/octet-stream"
		}
		s.files[url] = &asset{body: body, contentType: kind, immutable: strings.HasPrefix(url, "/_app/immutable/")}
		return nil
	})
	if err != nil {
		return nil, err
	}
	for url, body := range compressed {
		if a, ok := s.files[url]; ok {
			a.gzip = body
		}
	}
	s.csp = pagePolicy(s.files["/index.html"])
	return s, nil
}

// strictPolicy: what a page gets when it states no policy of its own (its
// inline scripts then do not run: safe, if not useful).
const strictPolicy = "default-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'"

// pagePolicy is the page's header policy: the one SvelteKit writes into
// the page (kit.csp: its own origin, plus the hashes of the inline
// bootstrap script it generates), which must be `default-src 'self'` at
// its root, and frame-ancestors, which only a header can carry.
func pagePolicy(index *asset) string {
	policy := strictPolicy
	if index != nil {
		if meta, ok := metaPolicy(string(index.body)); ok && strings.HasPrefix(meta, "default-src 'self'") {
			policy = meta
		}
	}
	return strings.TrimRight(policy, "; ") + "; frame-ancestors 'none'"
}

// metaPolicy: the content of the page's
// `<meta http-equiv="content-security-policy" content="...">`.
func metaPolicy(page string) (string, bool) {
	const marker = `http-equiv="content-security-policy"`
	at := strings.Index(strings.ToLower(page), marker)
	if at < 0 {
		return "", false
	}
	tag := page[at:]
	if end := strings.IndexByte(tag, '>'); end >= 0 {
		tag = tag[:end]
	}
	_, content, ok := strings.Cut(tag, `content="`)
	if !ok {
		return "", false
	}
	content, _, ok = strings.Cut(content, `"`)
	if !ok || !fieldValue(content) {
		return "", false
	}
	return strings.ReplaceAll(content, "&#39;", "'"), true
}

func (s *site) built() bool { return s.files["/index.html"] != nil }

// serve answers a request for the app's files. Paths that name no file
// and have no extension are the app's own routes (/apps, /ai): they get
// the page, which routes in the browser.
func (s *site) serve(req *request) *response {
	if req.method != "GET" && req.method != "HEAD" {
		return notAllowed("GET")
	}
	if !s.built() {
		resp := &response{status: 503, body: []byte("The Oceans web experience was not built into this system (Bun is needed to build it).\n")}
		resp.set("Content-Type", "text/plain; charset=utf-8")
		return resp
	}
	name := req.path
	if name == "/" {
		name = "/index.html"
	}
	a, ok := s.files[name]
	if !ok {
		if strings.Contains(path.Base(name), ".") {
			resp := &response{status: 404, body: []byte("not found\n")}
			resp.set("Content-Type", "text/plain; charset=utf-8")
			return resp
		}
		a = s.files["/index.html"]
	}
	resp := &response{status: 200, body: a.body}
	resp.set("Content-Type", a.contentType)
	if a.gzip != nil {
		resp.set("Vary", "Accept-Encoding")
		if acceptsGzip(req.get("accept-encoding")) {
			resp.body = a.gzip
			resp.set("Content-Encoding", "gzip")
		}
	}
	if a.immutable {
		resp.set("Cache-Control", "public, max-age=31536000, immutable")
	} else {
		resp.set("Cache-Control", "no-cache")
	}
	if strings.HasPrefix(a.contentType, "text/html") {
		resp.set("Content-Security-Policy", s.csp)
	}
	return resp
}

// acceptsGzip: Accept-Encoding lists gzip without q=0.
func acceptsGzip(header string) bool {
	for _, item := range strings.Split(header, ",") {
		coding, params, _ := strings.Cut(strings.TrimSpace(item), ";")
		if strings.TrimSpace(strings.ToLower(coding)) != "gzip" {
			continue
		}
		q := strings.ReplaceAll(strings.ToLower(params), " ", "")
		return q != "q=0" && q != "q=0.0" && q != "q=0.00" && q != "q=0.000"
	}
	return false
}
