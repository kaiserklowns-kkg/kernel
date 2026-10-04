package main

import "testing"

func TestColoursGoRound(t *testing.T) {
	colour := 0
	seen := map[int]bool{}
	for range palette {
		seen[colour] = true
		colour = next(colour)
	}
	if colour != 0 || len(seen) != len(palette) {
		t.Fatalf("ended at %d after %d colours", colour, len(seen))
	}
}
