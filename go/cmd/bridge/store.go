package main

import (
	"crypto/x509"
	"fmt"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/ai/gateway"
	"github.com/kaiserklowns-kkg/kernel/go/ai/httpc"
	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/dns"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/fs"
	"github.com/kaiserklowns-kkg/kernel/go/store"
)

// sourceFile keeps the store's URL in the bridge's storage.
const sourceFile = "store-source"

// oceansStore is the Store on Oceans: http(s) through the network
// service, the system's root certificates for https, the source kept in
// the bridge's own storage (`use storage`).
type oceansStore struct {
	net     oceans.Handle
	storage fs.Node
	source  string
	roots   *x509.CertPool
	say     func(string)
}

func newOceansStore(net oceans.Handle, storage fs.Node, say func(string)) *oceansStore {
	s := &oceansStore{net: net, storage: storage, say: say}
	if file, _, err := storage.Open(sourceFile, 0); err == nil {
		if data, err := file.ReadAll(512); err == nil {
			s.source = strings.TrimSpace(string(data))
		}
		file.Close()
	}
	return s
}

func (s *oceansStore) Source() string { return s.source }

func (s *oceansStore) SetSource(url string) error {
	if err := s.storage.WriteFile(sourceFile, []byte(url)); err != nil {
		return &apiError{500, "cannot keep the store's URL: " + err.Error()}
	}
	s.source = url
	return nil
}

// transport reaches the store's server; `path` is the store's base path.
func (s *oceansStore) transport() (*gateway.Transport, string, error) {
	e, err := httpc.ParseEndpoint(s.source)
	if err != nil {
		return nil, "", err
	}
	t := &gateway.Transport{
		Net:      s.net,
		Endpoint: e,
		Resolver: &dns.Resolver{Net: s.net},
	}
	if e.TLS {
		if t.Roots, err = s.systemRoots(); err != nil {
			return nil, "", err
		}
	}
	return t, e.Path, nil
}

func (s *oceansStore) systemRoots() (*x509.CertPool, error) {
	if s.roots != nil {
		return s.roots, nil
	}
	module, ok := oceans.Find("module", rootsModule)
	if !ok {
		return nil, fmt.Errorf("this system has no root certificates for https")
	}
	bundle, err := oceans.ReadMemory(module, gateway.MaxCertificates)
	if err != nil {
		return nil, err
	}
	pool, _, err := gateway.ParseCertificates(bundle)
	if err != nil {
		return nil, err
	}
	s.roots = pool
	return pool, nil
}

func (s *oceansStore) fetch(path func(base string) string, limit int) ([]byte, error) {
	t, base, err := s.transport()
	if err != nil {
		return nil, err
	}
	status, body, err := t.Get(path(base), limit)
	if err != nil {
		return nil, err
	}
	if status != 200 {
		return nil, fmt.Errorf("the server answered %d", status)
	}
	return body, nil
}

func (s *oceansStore) Catalog() (store.Catalog, error) {
	index, err := s.fetch(store.IndexPath, store.MaxIndex)
	if err != nil {
		return store.Catalog{}, err
	}
	return store.Parse(index)
}

func (s *oceansStore) Download(app store.App) ([]byte, error) {
	return s.fetch(func(base string) string { return store.PackagePath(base, app.Package) }, store.MaxPackage+1024)
}

// rootsModule is the system's trust store (ADR-0054), granted to the
// bridge as a boot module.
const rootsModule = "ca-roots.pem"
