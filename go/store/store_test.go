package store

import (
	"crypto/sha256"
	"encoding/hex"
	"strings"
	"testing"
)

var pkg = []byte("a package")

func entry() App {
	sum := sha256.Sum256(pkg)
	return App{
		ID: "app.oceans.tiles", Name: "Tiles", Version: "1.0.0", Publisher: "Oceans Examples",
		Description: "A colour", Permissions: []string{"window"},
		Package: "tiles-1.0.0.opk", Size: len(pkg), SHA256: hex.EncodeToString(sum[:]),
	}
}

func TestParsesACatalog(t *testing.T) {
	c, err := Parse([]byte(`{"apps":[{"id":"app.oceans.tiles","name":"Tiles","version":"1.0.0",
		"publisher":"Oceans Examples","description":"A colour","permissions":["window"],
		"package":"tiles-1.0.0.opk","size":9,"sha256":"` + entry().SHA256 + `"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	app, ok := c.Find("app.oceans.tiles")
	if !ok || app.Name != "Tiles" || app.Size != 9 {
		t.Fatalf("%+v", c)
	}
	if _, ok := c.Find("app.oceans.other"); ok {
		t.Fatal("found an app not listed")
	}
}

func TestRefusesBadEntries(t *testing.T) {
	bad := []func(*App){
		func(a *App) { a.ID = "Not An Id" },
		func(a *App) { a.ID = "single" },
		func(a *App) { a.Version = "1.0" },
		func(a *App) { a.Version = "01.0.0" },
		func(a *App) { a.Name = "" },
		func(a *App) { a.Name = "line\nbreak" },
		func(a *App) { a.Package = "../escape.opk" },
		func(a *App) { a.Package = "sub/dir.opk" },
		func(a *App) { a.Package = "tiles.exe" },
		func(a *App) { a.Size = 0 },
		func(a *App) { a.Size = MaxPackage + 1 },
		func(a *App) { a.SHA256 = "abc" },
		func(a *App) { a.SHA256 = strings.ToUpper(a.SHA256) },
		func(a *App) { a.Permissions = []string{""} },
	}
	for i, change := range bad {
		a := entry()
		change(&a)
		if a.check() == nil {
			t.Errorf("change %d accepted: %+v", i, a)
		}
	}
	if err := entry().check(); err != nil {
		t.Fatal(err)
	}
	if _, err := Parse([]byte(`{"apps":[` + twice() + `]}`)); err == nil {
		t.Error("an id listed twice was accepted")
	}
	if _, err := Parse([]byte(`not json`)); err == nil {
		t.Error("not JSON accepted")
	}
	if _, err := Parse(make([]byte, MaxIndex+1)); err == nil {
		t.Error("an oversized index accepted")
	}
}

func twice() string {
	one := `{"id":"app.oceans.tiles","name":"Tiles","version":"1.0.0","publisher":"P","description":"",` +
		`"package":"t.opk","size":1,"sha256":"` + entry().SHA256 + `"}`
	return one + "," + one
}

func TestVerifiesDownloads(t *testing.T) {
	a := entry()
	if err := a.Verify(pkg); err != nil {
		t.Fatal(err)
	}
	if a.Verify([]byte("a packagE")) == nil {
		t.Error("a changed package passed")
	}
	if a.Verify(append(pkg, 0)) == nil {
		t.Error("a longer package passed")
	}
}

func TestStates(t *testing.T) {
	for _, c := range []struct{ catalog, installed, want string }{
		{"1.0.0", "", Available},
		{"1.0.0", "1.0.0", Installed},
		{"1.2.0", "1.10.0", Installed},
		{"1.10.0", "1.9.9", Update},
		{"2.0.0", "1.99.99", Update},
	} {
		if got := StateOf(c.catalog, c.installed); got != c.want {
			t.Errorf("%s over %q: %s, not %s", c.catalog, c.installed, got, c.want)
		}
	}
}

func TestPaths(t *testing.T) {
	if IndexPath("/store/") != "/store/index.json" || PackagePath("/store", "t.opk") != "/store/t.opk" {
		t.Fatal(IndexPath("/store/"), PackagePath("/store", "t.opk"))
	}
}
