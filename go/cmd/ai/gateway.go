package main

import (
	"crypto/x509"
	"strconv"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/ai/gateway"
	"github.com/kaiserklowns-kkg/kernel/go/ai/httpc"
	"github.com/kaiserklowns-kkg/kernel/go/ai/model"
	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/dns"
)

// rootsModule is the system's trust store (ADR-0054): Mozilla's roots as
// a PEM bundle, a boot module granted to this service
// (`grant = module:ca-roots.pem`), read when https is first configured.
const rootsModule = "ca-roots.pem"

// systemRoots caches the parsed trust store.
var systemRoots *x509.CertPool

func (s *service) roots() (*x509.CertPool, error) {
	if systemRoots != nil {
		return systemRoots, nil
	}
	module, ok := oceans.Find("module", rootsModule)
	if !ok {
		return nil, nil // no trust store: only a CA the user adds
	}
	started := time.Now()
	bundle, err := oceans.ReadMemory(module, gateway.MaxCertificates)
	if err != nil {
		return nil, err
	}
	pool, n, err := gateway.ParseCertificates(bundle)
	if err != nil {
		return nil, err
	}
	s.say("trusting " + strconv.Itoa(n) + " root certificates from " + rootsModule +
		" (read in " + strconv.FormatInt(time.Since(started).Milliseconds(), 10) + " ms)")
	systemRoots = pool
	return pool, nil
}

// configure sets the model server: CONFIGURE's text is `URL MODEL [--dns
// SERVER]`; the requester may attach a read-only memory object holding
// PEM CA certificates to trust for this server, besides the system's.
func (s *service) configure(text string, handles []oceans.Handle) (uint64, []byte, []oceans.Handle) {
	settings, err := gateway.ParseSettings(text)
	if err != nil {
		return statusBadRequest, []byte(err.Error()), nil
	}
	if s.net == 0 {
		return statusBadRequest, []byte("the AI service has no network"), nil
	}
	if len(handles) > 1 {
		return statusBadRequest, []byte("at most one CA file"), nil
	}
	e := settings.Endpoint
	transport := &gateway.Transport{
		Net:      s.net,
		Endpoint: e,
		Resolver: &dns.Resolver{Net: s.net, Server: settings.DNS},
		Trace:    func(line string) { s.say("model server: " + line) },
	}
	if e.TLS {
		roots, err := s.roots()
		if err != nil {
			return statusBadRequest, []byte("the system's root certificates: " + err.Error()), nil
		}
		transport.Roots = roots
		if len(handles) == 1 {
			bundle, err := oceans.ReadMemory(handles[0], gateway.MaxCertificates)
			if err != nil {
				return statusBadRequest, []byte("cannot read the CA file: " + err.Error()), nil
			}
			pool, n, err := gateway.WithExtra(roots, bundle)
			if err != nil {
				return statusBadRequest, []byte("the CA file: " + err.Error()), nil
			}
			transport.Roots = pool
			s.say("trusting " + strconv.Itoa(n) + " more CA certificates for " + e.Name)
		}
		if transport.Roots == nil {
			return statusBadRequest, []byte("no trusted root certificates (add a CA with --ca)"), nil
		}
	} else if len(handles) == 1 {
		return statusBadRequest, []byte("a CA is only for https:// servers"), nil
	}
	s.runtime.Model = model.Chat{
		Transport: transport,
		Path:      e.Path + "/chat/completions",
		Model:     settings.Model,
	}
	line := "model " + settings.Model + " at " + urlOf(e)
	if settings.DNS.IsValid() {
		line += " (names from " + settings.DNS.String() + ")"
	}
	s.say(line)
	return statusDone, nil, nil
}

// urlOf writes an endpoint back as a URL.
func urlOf(e httpc.Endpoint) string {
	scheme := "http://"
	if e.TLS {
		scheme = "https://"
	}
	return scheme + e.Host + e.Path
}
