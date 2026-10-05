package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"github.com/kaiserklowns-kkg/kernel/go/store"
)

// fakeStore serves one catalog and its packages.
type fakeStore struct {
	source   string
	packages map[string][]byte
	broken   bool
}

func (s *fakeStore) Source() string { return s.source }

func (s *fakeStore) SetSource(url string) error {
	s.source = url
	return nil
}

func (s *fakeStore) Catalog() (store.Catalog, error) {
	if s.broken {
		return store.Catalog{}, errors.New("unreachable")
	}
	tiles := s.packages["tiles-1.0.0.opk"]
	sum := sha256.Sum256(tiles)
	return store.Catalog{Apps: []store.App{
		{ID: "app.oceans.tiles", Name: "Tiles", Version: "1.0.0", Publisher: "Oceans Examples",
			Package: "tiles-1.0.0.opk", Size: len(tiles), SHA256: hex.EncodeToString(sum[:])},
		{ID: "app.oceans.hello", Name: "Hello", Version: "2.0.0", Publisher: "Oceans Examples",
			Package: "hello-2.0.0.opk", Size: 1, SHA256: strings.Repeat("0", 64)},
	}}, nil
}

func (s *fakeStore) Download(app store.App) ([]byte, error) {
	return s.packages[app.Package], nil
}

func newStoreBridge(t *testing.T) (*bridge, *fake, *fakeStore) {
	b, sys, _ := newTestBridge(t)
	shop := &fakeStore{source: "http://10.0.2.2:8000/store", packages: map[string][]byte{
		"tiles-1.0.0.opk": []byte("tiles package"),
		"hello-2.0.0.opk": []byte("x"),
	}}
	b.store = shop
	return b, sys, shop
}

func TestStoreListsAppsWithTheirState(t *testing.T) {
	b, _, _ := newStoreBridge(t)
	resp := do(b, "GET", "/api/store", testToken, "")
	if resp.status != 200 {
		t.Fatalf("%d %s", resp.status, resp.body)
	}
	var view StoreView
	if err := json.Unmarshal(resp.body, &view); err != nil {
		t.Fatal(err)
	}
	states := map[string]string{}
	for _, app := range view.Apps {
		states[app.ID] = app.State
	}
	// The fake system has Hello 1.0.0 installed.
	if states["app.oceans.tiles"] != store.Available || states["app.oceans.hello"] != store.Update {
		t.Fatalf("%+v", view)
	}
}

func TestStoreInstallOnlyProposes(t *testing.T) {
	b, sys, _ := newStoreBridge(t)
	resp := do(b, "POST", "/api/store/install", testToken, `{"id":"app.oceans.tiles"}`)
	if resp.status != 202 || !strings.Contains(string(resp.body), "confirm on the device") {
		t.Fatalf("%d %s", resp.status, resp.body)
	}
	if len(sys.calls) == 0 || sys.calls[len(sys.calls)-1] != "propose tiles package" {
		t.Fatalf("%v", sys.calls)
	}
}

func TestStoreRefusesWhatDoesNotMatch(t *testing.T) {
	b, sys, shop := newStoreBridge(t)
	// A package that is not what the catalog lists.
	resp := do(b, "POST", "/api/store/install", testToken, `{"id":"app.oceans.hello"}`)
	if resp.status != 502 {
		t.Fatalf("a package not matching its SHA-256: %d %s", resp.status, resp.body)
	}
	for _, body := range []string{`{"id":"app.oceans.nothing"}`, `{"id":"Bad Id"}`, `{}`} {
		if resp := do(b, "POST", "/api/store/install", testToken, body); resp.status < 400 {
			t.Errorf("%s: %d", body, resp.status)
		}
	}
	shop.broken = true
	if resp := do(b, "GET", "/api/store", testToken, ""); resp.status != 502 {
		t.Errorf("an unreachable store: %d", resp.status)
	}
	for _, call := range sys.calls {
		if strings.HasPrefix(call, "propose") {
			t.Fatalf("proposed: %v", sys.calls)
		}
	}
}

func TestStoreSourceAndAuth(t *testing.T) {
	b, _, shop := newStoreBridge(t)
	if resp := do(b, "POST", "/api/store/source", "", `{"url":"http://x/store"}`); resp.status != 401 {
		t.Fatalf("unpaired: %d", resp.status)
	}
	if resp := do(b, "POST", "/api/store/source", testToken, `{"url":"ftp://x"}`); resp.status != 400 {
		t.Fatalf("a bad URL: %d", resp.status)
	}
	if resp := do(b, "POST", "/api/store/source", testToken, `{"url":"https://store.oceans.test/apps"}`); resp.status != 200 ||
		shop.source != "https://store.oceans.test/apps" {
		t.Fatalf("%d %q", resp.status, shop.source)
	}
	if resp := do(b, "POST", "/api/store/install", testToken, `{"id":"app.oceans.tiles"}`,
		"sec-fetch-site", "cross-site"); resp.status != 403 {
		t.Fatalf("cross-site: %d", resp.status)
	}
	b.store = nil
	if resp := do(b, "GET", "/api/store", testToken, ""); resp.status != 503 {
		t.Fatalf("no store: %d", resp.status)
	}
}
