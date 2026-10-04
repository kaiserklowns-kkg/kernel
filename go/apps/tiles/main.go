// tiles: the example windowed Go app (ADR-0060). Started by Oceans Core in
// the Go host with the `window` permission, it opens a window and fills it
// with a colour; every key typed while it has the focus moves to the next
// colour, and the close button ends it.
package main

import (
	"os"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/window"
)

// The colours, 0x00RRGGBB.
var palette = []uint32{0x2e7d6b, 0xc75b39, 0x6a5acd}

const (
	width  = 320
	height = 200
	events = 1 // the notification bit for window events
)

func main() {
	windows, ok := oceans.Find("use", "windows")
	if !ok {
		os.Exit(3)
	}
	notification, err := oceans.NotificationCreate()
	if err != nil {
		os.Exit(2)
	}
	w, err := window.Open(windows, notification, events, width, height, "a Go app")
	if err != nil {
		os.Exit(3)
	}
	colour := 0
	paint(w, palette[colour])
	for {
		if _, err := oceans.NotificationWait(notification); err != nil {
			os.Exit(2)
		}
		for {
			batch, err := window.Events(windows)
			if err != nil {
				os.Exit(3)
			}
			if len(batch) == 0 {
				break
			}
			for _, e := range batch {
				switch e.Kind {
				case window.Key:
					colour = next(colour)
					paint(w, palette[colour])
				case window.Close:
					_ = w.Close()
					return
				}
			}
		}
	}
}

// next is the colour after `colour`, round the palette.
func next(colour int) int {
	return (colour + 1) % len(palette)
}

func paint(w *window.Window, rgb uint32) {
	for i := range w.Pixels {
		w.Pixels[i] = rgb
	}
	_ = w.Present()
}
