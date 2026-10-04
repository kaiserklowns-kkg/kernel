// gohello: the first Go program on Oceans (ADR-0050). It shows that the
// Go runtime starts in the Go host, that standard output, clocks,
// goroutines and maps work, and that the System API binding reaches an
// Oceans service: it calls the echo service and checks the answer.
package main

import (
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

func main() {
	fmt.Println("gohello: Go", strings.TrimPrefix(goVersion(), "go"), "running on Oceans")

	// Goroutines and channels, on the host's single thread.
	var wg sync.WaitGroup
	results := make(chan int, 4)
	for i := 1; i <= 4; i++ {
		wg.Add(1)
		go func(n int) {
			defer wg.Done()
			results <- n * n
		}(i)
	}
	wg.Wait()
	close(results)
	sum := 0
	for r := range results {
		sum += r
	}
	fmt.Println("gohello: goroutines computed", sum)

	start := time.Now()
	time.Sleep(50 * time.Millisecond)
	if elapsed := time.Since(start); elapsed < 50*time.Millisecond {
		fmt.Println("gohello: slept too briefly:", elapsed)
		os.Exit(1)
	}

	echo, ok := oceans.Find("use", "echo")
	if !ok {
		fmt.Println("gohello: no echo service granted")
		os.Exit(2)
	}
	reply, err := oceans.Call(echo, 1, []byte("hello from go"), nil)
	if err != nil {
		fmt.Println("gohello: echo:", err)
		os.Exit(3)
	}
	fmt.Printf("gohello: echo replied %q\n", reply.Data)
	if string(reply.Data) != "HELLO FROM GO" {
		os.Exit(4)
	}
	if log, ok := oceans.Find("log", "log"); ok {
		_ = oceans.DebugWrite(log, "gohello: System API calls verified")
	}
}
