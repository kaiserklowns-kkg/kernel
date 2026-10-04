// greeter: the example Go app (ADR-0052). It is installed as a signed
// package (runtime = wasm) and started by Oceans Core in the Go host with
// the capabilities its manifest asked for: the console and read-only
// system information. It prints who it is (its "app info"), the
// arguments it was given and the system's memory figures.
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

func main() {
	app, err := oceans.AppInfo()
	if err != nil {
		fmt.Println("greeter:", err)
		os.Exit(1)
	}
	fmt.Printf("greeter: hello from %s %s, a Go app on Oceans\n", app.ID, app.Version)

	fmt.Println("greeter:", describeArgs(os.Args[1:]))

	sysinfo, ok := oceans.Find("sysinfo", "sysinfo")
	if !ok {
		fmt.Println("greeter: no system-info permission; no memory figures")
		return
	}
	memory, err := oceans.Memory(sysinfo)
	if err != nil {
		fmt.Println("greeter: memory:", err)
		os.Exit(2)
	}
	fmt.Printf("greeter: memory: %d MiB free of %d MiB\n", memory.FreeBytes()>>20, memory.TotalBytes()>>20)
}

// describeArgs says what the arguments were, quoted.
func describeArgs(args []string) string {
	if len(args) == 0 {
		return "no arguments"
	}
	quoted := make([]string, len(args))
	for i, arg := range args {
		quoted[i] = strconv.Quote(arg)
	}
	noun := "arguments"
	if len(args) == 1 {
		noun = "argument"
	}
	return fmt.Sprintf("%d %s: %s", len(args), noun, strings.Join(quoted, " "))
}
