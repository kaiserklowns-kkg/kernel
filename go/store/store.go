// Package store is the Oceans Store's client side (ADR-0061): the catalog
// a store publishes, and checking what is downloaded from it.
//
// A store is a directory on a web server: `index.json` lists its apps,
// and each app's package (.opk) sits beside it. The catalog is not
// trusted: it only says what to fetch. What makes a package installable
// is its publisher's signature, which Oceans Core verifies against the
// trusted keys; then the user confirms on the device. The catalog's size
// and SHA-256 catch a damaged or swapped download before Core sees it.
package store

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
)

// Limits.
const (
	// MaxIndex bounds the catalog.
	MaxIndex = 256 << 10
	// MaxPackage is Oceans Core's limit for a package.
	MaxPackage = 32 << 20
	// MaxApps a catalog may list.
	MaxApps = 256
	maxText = 200
)

// App is one catalog entry.
type App struct {
	ID          string   `json:"id"`
	Name        string   `json:"name"`
	Version     string   `json:"version"`
	Publisher   string   `json:"publisher"`
	Description string   `json:"description"`
	Permissions []string `json:"permissions"`
	// Package is the file name beside the index.
	Package string `json:"package"`
	Size    int    `json:"size"`
	SHA256  string `json:"sha256"`
}

// Catalog is a store's index.
type Catalog struct {
	Apps []App `json:"apps"`
}

// Parse reads and checks an index: every entry well-formed, no id twice.
func Parse(data []byte) (Catalog, error) {
	if len(data) > MaxIndex {
		return Catalog{}, errors.New("the store's index is too large")
	}
	var c Catalog
	if err := json.Unmarshal(data, &c); err != nil {
		return Catalog{}, fmt.Errorf("the store's index is not valid JSON: %w", err)
	}
	if len(c.Apps) > MaxApps {
		return Catalog{}, errors.New("the store lists too many apps")
	}
	seen := map[string]bool{}
	for i, app := range c.Apps {
		if err := app.check(); err != nil {
			return Catalog{}, fmt.Errorf("the store's app %d: %w", i+1, err)
		}
		if seen[app.ID] {
			return Catalog{}, fmt.Errorf("the store lists %s twice", app.ID)
		}
		seen[app.ID] = true
	}
	return c, nil
}

func (a App) check() error {
	switch {
	case !ValidID(a.ID):
		return errors.New("not an app id")
	case !ValidVersion(a.Version):
		return errors.New("not a MAJOR.MINOR.PATCH version")
	case !text(a.Name) || a.Name == "" || !text(a.Publisher) || !text(a.Description):
		return errors.New("a name, publisher or description that is empty, too long or not plain text")
	case !validFile(a.Package):
		return errors.New("a package that is not a plain .opk file name")
	case a.Size <= 0 || a.Size > MaxPackage:
		return errors.New("a package size out of bounds")
	case len(a.SHA256) != 64 || strings.ToLower(a.SHA256) != a.SHA256 || !isHex(a.SHA256):
		return errors.New("a SHA-256 that is not 64 lowercase hex digits")
	case len(a.Permissions) > 16:
		return errors.New("too many permissions")
	}
	for _, p := range a.Permissions {
		if !text(p) || p == "" || len(p) > 32 {
			return errors.New("a permission that is not a plain name")
		}
	}
	return nil
}

// Find is the entry for `id`.
func (c Catalog) Find(id string) (App, bool) {
	for _, app := range c.Apps {
		if app.ID == id {
			return app, true
		}
	}
	return App{}, false
}

// Verify checks a downloaded package against its entry.
func (a App) Verify(pkg []byte) error {
	if len(pkg) != a.Size {
		return fmt.Errorf("the package is %d bytes, not the %d the store lists", len(pkg), a.Size)
	}
	sum := sha256.Sum256(pkg)
	if hex.EncodeToString(sum[:]) != a.SHA256 {
		return errors.New("the package does not match the store's SHA-256")
	}
	return nil
}

// State of a catalog app on this system.
const (
	Available = "available"
	Installed = "installed"
	Update    = "update"
)

// StateOf compares a catalog version with the installed one ("" if
// none).
func StateOf(catalog, installed string) string {
	switch {
	case installed == "":
		return Available
	case Newer(catalog, installed):
		return Update
	default:
		return Installed
	}
}

// Newer reports whether version a is after b (both MAJOR.MINOR.PATCH).
func Newer(a, b string) bool {
	pa, pb := parts(a), parts(b)
	for i := range pa {
		if pa[i] != pb[i] {
			return pa[i] > pb[i]
		}
	}
	return false
}

func parts(v string) [3]uint64 {
	var out [3]uint64
	for i, p := range strings.SplitN(v, ".", 3) {
		out[i], _ = strconv.ParseUint(p, 10, 32)
	}
	return out
}

// ValidVersion: MAJOR.MINOR.PATCH, no leading zeros (as libs/package).
func ValidVersion(v string) bool {
	fields := strings.Split(v, ".")
	if len(fields) != 3 {
		return false
	}
	for _, f := range fields {
		if f == "" || len(f) > 9 || (len(f) > 1 && f[0] == '0') {
			return false
		}
		for _, c := range f {
			if c < '0' || c > '9' {
				return false
			}
		}
	}
	return true
}

// ValidID: reverse-DNS, lowercase letters, digits and hyphens, at least
// two dots-separated parts, at most 64 bytes.
func ValidID(id string) bool {
	if id == "" || len(id) > 64 {
		return false
	}
	labels := strings.Split(id, ".")
	if len(labels) < 2 {
		return false
	}
	for _, label := range labels {
		if label == "" || label[0] == '-' || label[len(label)-1] == '-' {
			return false
		}
		for _, c := range label {
			if !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '-') {
				return false
			}
		}
	}
	return true
}

func validFile(name string) bool {
	if !strings.HasSuffix(name, ".opk") || len(name) > 80 || strings.HasPrefix(name, ".") {
		return false
	}
	for _, c := range name {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-' || c == '_' || c == '.') {
			return false
		}
	}
	return true
}

func text(s string) bool {
	if len(s) > maxText {
		return false
	}
	for _, c := range s {
		if c < ' ' || c == 0x7f {
			return false
		}
	}
	return true
}

func isHex(s string) bool {
	_, err := hex.DecodeString(s)
	return err == nil
}

// PackagePath is the path of a package beside the index at `base` (the
// store's URL path, without a trailing slash).
func PackagePath(base, file string) string {
	return strings.TrimSuffix(base, "/") + "/" + file
}

// IndexPath is the path of the store's index.
func IndexPath(base string) string {
	return strings.TrimSuffix(base, "/") + "/index.json"
}
