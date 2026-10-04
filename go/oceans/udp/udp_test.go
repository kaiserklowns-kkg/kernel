package udp

import (
	"net/netip"
	"testing"
)

func TestParseRecv6(t *testing.T) {
	mapped := netip.MustParseAddr("::ffff:10.0.2.2").As16()
	data := append(mapped[:], 0x35, 0x00, 0, 'h', 'i')
	d, err := parseRecv6(data)
	if err != nil || d.From != netip.MustParseAddrPort("10.0.2.2:53") || string(d.Payload) != "hi" || d.Truncated {
		t.Fatalf("%+v %v", d, err)
	}
	v6 := netip.MustParseAddr("fec0::2").As16()
	d, err = parseRecv6(append(v6[:], 1, 0, truncated))
	if err != nil || d.From != netip.MustParseAddrPort("[fec0::2]:1") || len(d.Payload) != 0 || !d.Truncated {
		t.Fatalf("%+v %v", d, err)
	}
	if _, err := parseRecv6(data[:18]); err == nil {
		t.Fatal("a short RECV6 reply was accepted")
	}
}
