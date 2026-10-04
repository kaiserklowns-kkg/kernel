package netproto

import (
	"errors"
	"net/netip"
	"testing"
)

func TestParseInfo(t *testing.T) {
	data := []byte{1, 10, 0, 2, 15, 24, 10, 0, 2, 2, 10, 0, 2, 3, 0x52, 0x54, 0, 0x12, 0x34, 0x56}
	info, err := ParseInfo(data)
	if err != nil || !info.Configured || info.Address != netip.MustParseAddr("10.0.2.15") || info.Prefix != 24 ||
		info.Gateway != netip.MustParseAddr("10.0.2.2") || info.DNS != netip.MustParseAddr("10.0.2.3") {
		t.Fatalf("%+v %v", info, err)
	}
	unset := make([]byte, 20)
	if info, err := ParseInfo(unset); err != nil || info.Configured || info.DNS.IsValid() {
		t.Fatalf("unset: %+v %v", info, err)
	}
	if _, err := ParseInfo(data[:19]); err == nil {
		t.Fatal("a short INFO was accepted")
	}
}

func TestParseInfo6(t *testing.T) {
	record := func(address string, state, linkLocal byte) []byte {
		a := netip.MustParseAddr(address).As16()
		return append(a[:], 64, state, linkLocal)
	}
	data := []byte{1, 64, 0xdc, 0x05, 2}
	data = append(data, record("fe80::1", 1, 1)...)
	data = append(data, record("fec0::15", 1, 0)...)
	router := netip.MustParseAddr("fe80::2").As16()
	dns := netip.MustParseAddr("fec0::3").As16()
	data = append(append(data, router[:]...), dns[:]...)
	info, err := ParseInfo6(data)
	if err != nil || !info.Enabled || !info.Global || info.DNS != netip.MustParseAddr("fec0::3") {
		t.Fatalf("%+v %v", info, err)
	}
	// Only a tentative global address: not global yet.
	tentative := append([]byte{1, 64, 0, 0, 1}, record("fec0::15", 0, 0)...)
	tentative = append(tentative, make([]byte, 32)...)
	if info, err := ParseInfo6(tentative); err != nil || info.Global || info.DNS.IsValid() {
		t.Fatalf("tentative: %+v %v", info, err)
	}
	for _, bad := range [][]byte{nil, data[:4], data[:len(data)-1], {1, 64, 0, 0, 9}} {
		if _, err := ParseInfo6(bad); err == nil {
			t.Errorf("% x accepted", bad)
		}
	}
}

func TestErrors(t *testing.T) {
	if Status(0) != nil {
		t.Fatal("OK is an error")
	}
	if err := Status(9); err.Error() != "network: connection refused" {
		t.Fatal(err)
	}
	if Error(99).Error() != "network: error 99" {
		t.Fatal(Error(99).Error())
	}
	var timeout interface{ Timeout() bool }
	if !errors.As(ErrTimeout, &timeout) || !timeout.Timeout() {
		t.Fatal("ErrTimeout is not a timeout")
	}
}
