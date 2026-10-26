# korvus plugin SDK — Go

Write a korvus plugin in Go, over the [remote-plugin bridge](../../README.md). No C, no korvus rebuild, no
dependencies — the standard library (`net`, `encoding/json`) is enough. Go 1.21+.

## Install

```sh
go get github.com/KorvusBrowser/korvus-browser/plugins/sdk/go/korvusplugin
```

(Or vendor this directory — it is a single, dependency-free package.)

## Use

```go
package main

import (
	"log"
	"strings"

	"github.com/KorvusBrowser/korvus-browser/plugins/sdk/go/korvusplugin"
)

func main() {
	p := &korvusplugin.Plugin{
		Name:    "my-plugin",
		OnEvent: func(ev korvusplugin.Event) { log.Println(ev.Kind) }, // observer stream
		OnRequest: func(req korvusplugin.Request) korvusplugin.Verdict { // network verdict
			if strings.Contains(req.URL, "ads.") {
				return korvusplugin.Block
			}
			return korvusplugin.Continue
		},
	}
	log.Fatal(p.Serve("127.0.0.1:9099"))
}
```

Then load the bridge into korvus, pointed at your server:

```sh
korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
```

Drop `"forward_requests":true` if you only want the observer stream (`OnRequest` is then never asked).

## The slots the bridge forwards

| slot | field | you return | korvus does |
|---|---|---|---|
| observer | `OnEvent func(Event)` | nothing | reports context/page/navigation/request/response/console events (fire-and-forget) |
| network | `OnRequest func(Request) Verdict` | `Continue` or `Block` | lets the hop proceed, or refuses it |

`Event` has `Kind`, `Detail` (the event's JSON payload as `json.RawMessage`, or nil), `PID`, `Tag`. `Request` has
`URL`, `Method`, `Destination`, `PID`, `Tag`. A nil slot means "this plugin does not fill that slot". Each
connection is served on its own goroutine, so the slot funcs must be safe for concurrent use; a slot that panics is
recovered and treated as "did nothing" (the hop continues).

## Run the sample

```sh
cd ../hello && go run . --port 9099 --block-substr ads.
```

`hello/main.go` prints every event to stderr and blocks any request whose URL contains `--block-substr`. With
`--port 0` it binds an ephemeral port and prints `PORT <n>` on stdout.

## How it connects

The bridge speaks newline-delimited JSON over a plain TCP stream. korvus runs the browser endpoint and each page as
separate processes and loads the bridge into each, so several connections arrive at once — one per process; the
`Tag`/`PID` on each message tell them apart. See [`../../README.md`](../../README.md) for the wire protocol.
