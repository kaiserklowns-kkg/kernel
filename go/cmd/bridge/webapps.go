package main

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"path"
	"strings"
	"testing/fstest"
)

// Web apps (ADR-0064): SvelteKit apps installed as signed packages
// (`runtime = web`), served to the paired browser from their own origin,
// the app port, at /ID/. They are kept apart from the system's web
// experience and from each other:
//
//   - the app port is another origin than the system's (8080), and every
//     app page is sandboxed (CSP `sandbox allow-scripts allow-forms`): an
//     opaque origin, with no cookies, no storage and no reach into the
//     system's pages or API;
//   - a page is served only to the paired browser (its session cookie, sent
//     with the navigation); it carries a token of its own, which its
//     requests to the app API present;
//   - the app API reaches only that app's own data, and only if its
//     manifest asked for storage.
//
// The files (scripts, styles, images) are the package's, public like the
// package itself, so they are served to the sandboxed page without a
// cookie, with CORS allowing any origin (module scripts need it).

// appPort is the web apps' origin.
const appPort = 8081

// Web bundle limits (libs/package web.rs).
const (
	bundleMagic    = "OCEANSWB"
	bundleVersion  = 1
	maxBundleFiles = 2048
	maxBundlePath  = 200
	maxBundle      = 30 << 20
)

// maxAppData bounds one value of the app API (a request body's limit).
const maxAppData = maxBodyBytes

var errBundle = errors.New("the web bundle is not valid")

// validBundlePath: as libs/package's `web::valid_path`.
func validBundlePath(p string) bool {
	if p == "" || len(p) > maxBundlePath {
		return false
	}
	for _, segment := range strings.Split(p, "/") {
		if segment == "" || strings.HasPrefix(segment, ".") {
			return false
		}
		for i := 0; i < len(segment); i++ {
			c := segment[i]
			ok := c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' ||
				strings.IndexByte("._-~@+", c) >= 0
			if !ok {
				return false
			}
		}
	}
	return true
}

// parseBundle reads a web bundle's files, checking all of it.
func parseBundle(b []byte) (map[string][]byte, error) {
	if len(b) > maxBundle || len(b) < 16 || string(b[:8]) != bundleMagic ||
		binary.LittleEndian.Uint32(b[8:]) != bundleVersion {
		return nil, errBundle
	}
	count := int(binary.LittleEndian.Uint32(b[12:]))
	if count > maxBundleFiles {
		return nil, errBundle
	}
	files := make(map[string][]byte, count)
	rest := b[16:]
	for i := 0; i < count; i++ {
		if len(rest) < 2 {
			return nil, errBundle
		}
		n := int(binary.LittleEndian.Uint16(rest))
		if len(rest) < 2+n+4 {
			return nil, errBundle
		}
		name := string(rest[2 : 2+n])
		size := int(binary.LittleEndian.Uint32(rest[2+n:]))
		rest = rest[2+n+4:]
		if size > len(rest) || !validBundlePath(name) {
			return nil, errBundle
		}
		if _, dup := files[name]; dup {
			return nil, errBundle
		}
		files[name] = rest[:size]
		rest = rest[size:]
	}
	if len(rest) != 0 {
		return nil, errBundle
	}
	return files, nil
}

// bundleSite turns a bundle into a site the bridge serves (gzip copies,
// the page's policy), as the system's own web app is.
func bundleSite(bundle []byte) (*site, error) {
	files, err := parseBundle(bundle)
	if err != nil {
		return nil, err
	}
	fsys := fstest.MapFS{}
	for name, data := range files {
		fsys["web/"+name] = &fstest.MapFile{Data: data}
	}
	s, err := loadSite(fsys, "web")
	if err != nil {
		return nil, err
	}
	if !s.built() {
		return nil, errors.New("the web app has no index.html")
	}
	return s, nil
}

// webApp is a web app's site, for the version it was read at.
type webApp struct {
	version string
	site    *site
}

// appData keeps web apps' data (`storage`), each app apart.
type appData interface {
	Get(id, name string) ([]byte, error)
	Put(id, name string, data []byte) error
}

// errNoData: nothing stored under that name.
var errNoData = &apiError{404, "nothing stored under that name"}

func newAppToken() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("no randomness") // the bridge cannot work safely without it
	}
	return hex.EncodeToString(b[:])
}

// validDataName: the app API's names.
func validDataName(name string) bool {
	if name == "" || len(name) > 64 || strings.HasPrefix(name, ".") {
		return false
	}
	return strings.Trim(name, "abcdefghijklmnopqrstuvwxyz0123456789._-") == ""
}

// serveApp answers a request on the app port.
func (b *bridge) serveApp(req *request) *response {
	resp := b.appRequest(req)
	resp.set("X-Content-Type-Options", "nosniff")
	resp.set("Referrer-Policy", "no-referrer")
	if _, ok := resp.get("Content-Security-Policy"); !ok {
		resp.set("Content-Security-Policy", "default-src 'none'; sandbox; frame-ancestors 'none'")
	}
	return resp
}

func plain(status int, text string) *response {
	resp := &response{status: status, body: []byte(text + "\n")}
	resp.set("Content-Type", "text/plain; charset=utf-8")
	return resp
}

func (b *bridge) appRequest(req *request) *response {
	id, rest, _ := strings.Cut(strings.TrimPrefix(req.path, "/"), "/")
	if !validAppID(id) {
		return plain(404, "no such app")
	}
	if name, ok := strings.CutPrefix(rest, "api/data/"); ok {
		return b.appAPI(req, id, name)
	}
	app, err := b.webApp(id)
	if err != nil {
		return failure(err)
	}
	if req.method != "GET" && req.method != "HEAD" {
		return notAllowed("GET")
	}
	file := "/" + rest
	if rest == "" {
		file = "/index.html"
	}
	a, ok := app.site.files[file]
	if !ok {
		if strings.Contains(path.Base(file), ".") {
			return plain(404, "not found")
		}
		a = app.site.files["/index.html"]
	}
	if strings.HasPrefix(a.contentType, "text/html") {
		return b.appPage(req, id, app.site, a)
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
	// Public files of a sandboxed (opaque-origin) page.
	resp.set("Access-Control-Allow-Origin", "*")
	resp.set("Cross-Origin-Resource-Policy", "cross-origin")
	return resp
}

// appPage serves a web app's page to the paired browser only, sandboxed,
// with the app's token.
func (b *bridge) appPage(req *request, id string, s *site, page *asset) *response {
	if !b.authorized(req) {
		return plain(401, "Not paired with this browser: run `ui pair` on Oceans and sign in to the Oceans web experience first.")
	}
	token, ok := b.appTokens[id]
	if !ok {
		token = newAppToken()
		b.appTokens[id] = token
	}
	body := string(page.body)
	meta := `<meta name="oceans-app-token" content="` + token + `">`
	if at := strings.Index(strings.ToLower(body), "<head>"); at >= 0 {
		body = body[:at+6] + meta + body[at+6:]
	} else {
		body = meta + body
	}
	resp := &response{status: 200, body: []byte(body)}
	resp.set("Content-Type", page.contentType)
	resp.set("Cache-Control", "no-store")
	resp.set("Content-Security-Policy", s.csp+"; sandbox allow-scripts allow-forms")
	resp.set("Cross-Origin-Opener-Policy", "same-origin")
	return resp
}

// webApp is the installed web app `id`, read from Core when its version
// changes.
func (b *bridge) webApp(id string) (*webApp, error) {
	apps, err := b.sys.Apps()
	if err != nil {
		return nil, err
	}
	var found *App
	for i := range apps {
		if apps[i].ID == id {
			found = &apps[i]
		}
	}
	if found == nil || found.Runtime != "web" {
		return nil, &apiError{404, "no such web app"}
	}
	if cached, ok := b.webApps[id]; ok && cached.version == found.Version {
		return cached, nil
	}
	version, bundle, err := b.sys.WebBundle(id)
	if err != nil {
		return nil, err
	}
	s, err := bundleSite(bundle)
	if err != nil {
		return nil, &apiError{502, "the web app: " + err.Error()}
	}
	app := &webApp{version: version, site: s}
	b.webApps[id] = app
	return app, nil
}

// appAPI: a web app's own data, for its page (its token).
func (b *bridge) appAPI(req *request, id, name string) *response {
	cors := func(resp *response) *response {
		resp.set("Access-Control-Allow-Origin", "*")
		resp.set("Access-Control-Allow-Methods", "GET, PUT")
		resp.set("Access-Control-Allow-Headers", "authorization, content-type")
		resp.set("Cache-Control", "no-store")
		return resp
	}
	if req.method == "OPTIONS" {
		return cors(&response{status: 204})
	}
	token, issued := b.appTokens[id]
	bearer, _ := strings.CutPrefix(req.get("authorization"), "Bearer ")
	if !issued || !sameToken(strings.TrimSpace(bearer), token) {
		return cors(failure(&apiError{403, "not this app's page"}))
	}
	if !validDataName(name) {
		return cors(failure(&apiError{400, "names are 1 to 64 of a-z 0-9 . _ -"}))
	}
	if b.appData == nil {
		return cors(failure(&apiError{503, "the bridge has no storage for web apps"}))
	}
	if !b.asksForStorage(id) {
		return cors(failure(&apiError{403, "the app did not ask for storage"}))
	}
	switch req.method {
	case "GET":
		data, err := b.appData.Get(id, name)
		if err != nil {
			return cors(failure(err))
		}
		resp := &response{status: 200, body: data}
		resp.set("Content-Type", "application/octet-stream")
		return cors(resp)
	case "PUT":
		if len(req.body) > maxAppData {
			return cors(failure(&apiError{413, "at most 16 KiB per name"}))
		}
		if err := b.appData.Put(id, name, req.body); err != nil {
			return cors(failure(err))
		}
		return cors(&response{status: 204})
	}
	return cors(notAllowed("GET, PUT"))
}

func (b *bridge) asksForStorage(id string) bool {
	permissions, err := b.sys.Permissions(id)
	if err != nil {
		return false
	}
	for _, p := range permissions {
		if p.Name == "storage" && (p.Decision == "automatic" || p.Decision == "allowed") {
			return true
		}
	}
	return false
}
