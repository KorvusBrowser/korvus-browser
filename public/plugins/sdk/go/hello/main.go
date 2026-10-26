// hello -- the smallest korvus plugin written in Go over the remote-plugin bridge.
//
// It observes every event korvus forwards (printed to stderr) and refuses any request hop whose URL contains a
// chosen substring. Run it, then point the remote-bridge plugin at it:
//
//	go run . --port 9099 --block-substr ads.
//	korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
//
// With --port 0 it binds an ephemeral port and prints `PORT <n>` on stdout, so a parent process can learn the port
// (the SDK probe drives it this way). Events go to stderr, keeping stdout for that one line.
package main

import (
	"flag"
	"fmt"
	"log"
	"net"
	"os"
	"strings"

	"github.com/KorvusBrowser/korvus-browser/plugins/sdk/go/korvusplugin"
)

func main() {
	host := flag.String("host", "127.0.0.1", "bind host")
	port := flag.Int("port", 9099, "TCP port (0 binds an ephemeral port, printed as PORT <n>)")
	blockSubstr := flag.String("block-substr", "", "refuse a request whose URL contains this substring")
	flag.Parse()

	logger := log.New(os.Stderr, "", 0)

	p := &korvusplugin.Plugin{
		Name: "hello-go",
		OnEvent: func(ev korvusplugin.Event) {
			logger.Printf("event: %s (pid=%d tag=%s)", ev.Kind, ev.PID, ev.Tag)
		},
		OnRequest: func(req korvusplugin.Request) korvusplugin.Verdict {
			if *blockSubstr != "" && strings.Contains(req.URL, *blockSubstr) {
				logger.Printf("block: %s %s", req.Method, req.URL)
				return korvusplugin.Block
			}
			return korvusplugin.Continue
		},
	}

	ln, err := p.Listen(fmt.Sprintf("%s:%d", *host, *port))
	if err != nil {
		logger.Fatalf("listen: %v", err)
	}
	fmt.Printf("PORT %d\n", ln.Addr().(*net.TCPAddr).Port) // announce the bound port, then serve until killed
	log.Fatal(p.ServeListener(ln))
}
