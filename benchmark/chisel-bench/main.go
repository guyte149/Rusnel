// Chisel-style end-to-end benchmark with warmup + median-of-N sampling.
// Adapted from github.com/jpillora/chisel/test/bench.
//
//                       (direct)
//           .--------------->----------------.
//          /    rusnel         rusnel         \
//  request--->client:2001--->server:2002---->fileserver:3000
//          \    chisel         chisel         /
//           '->client:2003--->server:2004---'

package main

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"os/exec"
	"sort"
	"strconv"
	"time"
)

const (
	B  = 1
	KB = 1000 * B
	MB = 1000 * KB
)

// Sampling parameters. Override via env vars (CHISEL_BENCH_RUNS,
// CHISEL_BENCH_WARMUP) so CI / local can dial sample count up or down.
var (
	runs    = envInt("CHISEL_BENCH_RUNS", 5)
	warmups = envInt("CHISEL_BENCH_WARMUP", 1)
)

type Result struct {
	Tool    string    `json:"tool"`
	Bytes   int       `json:"bytes"`
	Samples []float64 `json:"samples_ms"`
	Median  float64   `json:"median_ms"`
	Min     float64   `json:"min_ms"`
	Max     float64   `json:"max_ms"`
}

func main() {
	fs := startFileServer(":3000")
	defer fs.Close()
	time.Sleep(100 * time.Millisecond)

	fmt.Printf("Sampling: %d warmup + %d timed runs per (tool, size)\n", warmups, runs)

	var results []Result

	fmt.Println("\n=== Direct (no tunnel) ===")
	results = append(results, benchSizes("direct", "3000")...)

	fmt.Println("\n=== Rusnel (QUIC) ===")
	rusnelServer := startCmd("rusnel", "server", "--insecure", "-p", "2002")
	rusnelClient := startCmd("rusnel", "client", "--insecure", "127.0.0.1:2002", "0.0.0.0:2001:127.0.0.1:3000")
	waitForPort("2001", 10*time.Second)
	results = append(results, benchSizes("rusnel", "2001")...)
	stopCmds(rusnelClient, rusnelServer)

	fmt.Println("\n=== Chisel (SSH/WebSocket) ===")
	chiselServer := startCmd("chisel", "server", "--port", "2004")
	waitForPort("2004", 10*time.Second)
	chiselClient := startCmd("chisel", "client", "http://127.0.0.1:2004", "2003:3000")
	waitForPort("2003", 10*time.Second)
	results = append(results, benchSizes("chisel", "2003")...)
	stopCmds(chiselClient, chiselServer)

	printSummary(results)
	saveResults(results)
}

func benchSizes(tool, port string) []Result {
	var results []Result
	for size := 1; size <= 100*MB; size *= 10 {
		results = append(results, sampleTunnel(tool, port, size))
	}
	return results
}

func sampleTunnel(tool, port string, size int) Result {
	for i := 0; i < warmups; i++ {
		_ = runRequest(tool, port, size)
	}

	samples := make([]float64, 0, runs)
	for i := 0; i < runs; i++ {
		samples = append(samples, runRequest(tool, port, size))
	}

	sorted := append([]float64(nil), samples...)
	sort.Float64s(sorted)
	median := sorted[len(sorted)/2]
	if len(sorted)%2 == 0 {
		median = (sorted[len(sorted)/2-1] + sorted[len(sorted)/2]) / 2
	}

	fmt.Printf("  :%s %-7s  median=%7.3fms  min=%7.3fms  max=%7.3fms  (n=%d)\n",
		port, fmtBytes(size), median, sorted[0], sorted[len(sorted)-1], len(samples))

	return Result{
		Tool:    tool,
		Bytes:   size,
		Samples: samples,
		Median:  median,
		Min:     sorted[0],
		Max:     sorted[len(sorted)-1],
	}
}

func runRequest(tool, port string, size int) float64 {
	t0 := time.Now()
	resp, err := http.Get("http://127.0.0.1:" + port + "/" + strconv.Itoa(size))
	if err != nil {
		log.Fatalf("%s :%s %d bytes: %v", tool, port, size, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		log.Fatalf("%s :%s %d bytes: status %d", tool, port, size, resp.StatusCode)
	}
	n, err := io.Copy(io.Discard, resp.Body)
	if err != nil {
		log.Fatalf("%s :%s %d bytes read: %v", tool, port, size, err)
	}
	if int(n) != size {
		log.Fatalf("%s: %d bytes expected, got %d", tool, size, n)
	}
	return float64(time.Since(t0).Microseconds()) / 1000.0
}

func fmtBytes(b int) string {
	switch {
	case b >= MB:
		return fmt.Sprintf("%dMB", b/MB)
	case b >= KB:
		return fmt.Sprintf("%dKB", b/KB)
	default:
		return fmt.Sprintf("%dB", b)
	}
}

func startFileServer(addr string) *http.Server {
	bsize := 3 * MB
	buf := make([]byte, bsize)
	for i := range buf {
		buf[i] = byte(i)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		rsize, _ := strconv.Atoi(r.URL.Path[1:])
		for rsize >= bsize {
			w.Write(buf)
			rsize -= bsize
		}
		w.Write(buf[:rsize])
	})

	srv := &http.Server{Addr: addr, Handler: mux}
	go srv.ListenAndServe()
	return srv
}

func startCmd(name string, args ...string) *exec.Cmd {
	cmd := exec.Command(name, args...)
	cmd.Stdout = nil
	cmd.Stderr = nil
	if err := cmd.Start(); err != nil {
		log.Fatalf("failed to start %s: %v", name, err)
	}
	return cmd
}

func stopCmds(cmds ...*exec.Cmd) {
	for _, c := range cmds {
		if c != nil && c.Process != nil {
			_ = c.Process.Kill()
		}
	}
	for _, c := range cmds {
		if c != nil {
			_, _ = c.Process.Wait()
		}
	}
	time.Sleep(200 * time.Millisecond)
}

func waitForPort(port string, timeout time.Duration) {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		conn, err := net.DialTimeout("tcp", "127.0.0.1:"+port, 500*time.Millisecond)
		if err == nil {
			conn.Close()
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	log.Fatalf("port %s not ready after %s", port, timeout)
}

func printSummary(results []Result) {
	fmt.Println("\n┌──────────┬──────────────┬──────────────┬──────────────┐")
	fmt.Println("│ Size     │ Direct (med) │ Rusnel (med) │ Chisel (med) │")
	fmt.Println("├──────────┼──────────────┼──────────────┼──────────────┤")

	bySize := map[int]map[string]float64{}
	for _, r := range results {
		if bySize[r.Bytes] == nil {
			bySize[r.Bytes] = map[string]float64{}
		}
		bySize[r.Bytes][r.Tool] = r.Median
	}

	for size := 1; size <= 100*MB; size *= 10 {
		m := bySize[size]
		fmt.Printf("│ %-8s │ %10.3fms │ %10.3fms │ %10.3fms │\n",
			fmtBytes(size), m["direct"], m["rusnel"], m["chisel"])
	}
	fmt.Println("└──────────┴──────────────┴──────────────┴──────────────┘")
}

func saveResults(results []Result) {
	outDir := os.Getenv("RESULTS_DIR")
	if outDir == "" {
		outDir = "/results"
	}
	os.MkdirAll(outDir, 0755)

	out := struct {
		Timestamp string   `json:"timestamp"`
		Config    any      `json:"config"`
		Results   []Result `json:"results"`
	}{
		Timestamp: time.Now().UTC().Format(time.RFC3339),
		Config: map[string]int{
			"runs":    runs,
			"warmups": warmups,
		},
		Results: results,
	}

	f, err := os.Create(outDir + "/results.json")
	if err != nil {
		log.Printf("warning: could not save results: %v", err)
		return
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	enc.SetIndent("", "  ")
	enc.Encode(out)
	fmt.Printf("\nResults saved to %s/results.json\n", outDir)
}

func envInt(key string, def int) int {
	if v := os.Getenv(key); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return n
		}
	}
	return def
}
