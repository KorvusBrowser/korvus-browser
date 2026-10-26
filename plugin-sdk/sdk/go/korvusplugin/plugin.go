// Package korvusplugin lets you write a korvus plugin in Go, over the remote-plugin bridge.
//
// korvus's plugin ABI is a C ABI: every slot is a C function pointer, which is exactly right for the hot slots
// (on_call, on_request) where a plugin must be fast and in-process. The remote-bridge first-party plugin opens that
// door to the infrequent slots for people who would rather not write C: it is a tiny C shim loaded into korvus that
// forwards the slot callbacks to a server over a socket, so the real logic lives on the other end of the wire, in
// whatever language the author likes. This package is that other end, in Go.
//
// You set the slots the bridge forwards on a Plugin and serve it:
//
//	p := &korvusplugin.Plugin{
//		Name:      "my-plugin",
//		OnEvent:   func(ev korvusplugin.Event) { log.Println(ev.Kind) },
//		OnRequest: func(req korvusplugin.Request) korvusplugin.Verdict {
//			if strings.Contains(req.URL, "ads.") {
//				return korvusplugin.Block
//			}
//			return korvusplugin.Continue
//		},
//	}
//	log.Fatal(p.Serve("127.0.0.1:9099"))
//
// Then load the bridge into korvus, pointed at this server:
//
//	korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
//
// WHAT THE BRIDGE FORWARDS. The bridge speaks a plain TCP stream of newline-delimited JSON (NDJSON) -- one object
// per line, UTF-8, no framing or handshake. It carries exactly two of korvus's slots:
//
//   - OnEvent (the observer slot) -- every context / page / navigation / request / response / console event korvus
//     reports. Fire-and-forget: this server sends no reply, so observing never slows the browser down.
//   - OnRequest (the network verdict slot), when the bridge is loaded with forward_requests:true -- each request
//     hop, and the bridge BLOCKS on this server's one-line reply, then applies it. Return Block to refuse the hop
//     (korvus's KORVUS_BLOCK); anything else continues.
//
// korvus runs the browser endpoint and each page as separate OS processes and loads the bridge into each, so
// several connections arrive at once (one per process); Tag/PID on each message tell them apart. Each connection is
// served on its own goroutine, so the slot funcs must be safe for concurrent use; a slot that panics is recovered
// and treated as "did nothing" (OnRequest continues), so a bug on this side never wedges the browser.
//
// Standard library only (net, encoding/json); no dependencies.
package korvusplugin

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"strings"
)

// Verdict is what OnRequest returns: the wire values the bridge understands.
type Verdict string

const (
	Continue Verdict = "continue" // let the hop proceed (the default)
	Block    Verdict = "block"    // refuse the hop (korvus's KORVUS_BLOCK)
)

// Event is one observer event the bridge forwarded (OnEvent). Fire-and-forget: there is nothing to answer.
type Event struct {
	Kind   string          // korvus's event name: "context.created", "page.created", "navigation", "request", "response", "console", ...
	Detail json.RawMessage // the event's own JSON payload, or nil when korvus sent none / it was too large to embed
	PID    int             // the korvus process that emitted it
	Tag    string          // the bridge instance's tag
	Raw    json.RawMessage // the whole decoded line
}

// Request is one request hop awaiting a verdict (OnRequest). Return Continue or Block.
type Request struct {
	URL         string          // the hop's URL
	Method      string          // the HTTP method
	Destination string          // the Fetch request destination: "document", "image", "script", ...
	PID         int             // the korvus process that emitted it
	Tag         string          // the bridge instance's tag
	ID          json.RawMessage // the bridge's per-connection request id (echoed back untouched)
}

// Plugin is a korvus plugin written in Go, served over the remote-plugin bridge. Set the slots the bridge forwards,
// then call Serve. A nil slot means "this plugin does not fill that slot".
type Plugin struct {
	Name      string                // a label for log lines
	OnEvent   func(Event)           // the observer slot; the return value is ignored
	OnRequest func(Request) Verdict // the network verdict slot; return Continue (default) or Block
}

// the NDJSON lines on the wire.
type wireMsg struct {
	T           string          `json:"t"`
	PID         int             `json:"pid"`
	Tag         string          `json:"tag"`
	Kind        string          `json:"kind"`
	JSON        json.RawMessage `json:"json"`
	ID          json.RawMessage `json:"id"`
	URL         string          `json:"url"`
	Method      string          `json:"method"`
	Destination string          `json:"destination"`
}

type wireReply struct {
	ID      json.RawMessage `json:"id"`
	Verdict Verdict         `json:"verdict"`
}

// Serve binds addr (e.g. "127.0.0.1:9099") and serves the bridge's connections forever. It blocks; the error is
// non-nil only if binding or accepting fails.
func (p *Plugin) Serve(addr string) error {
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		return err
	}
	return p.ServeListener(ln)
}

// Listen binds addr without accepting yet, so a caller can read the bound port (useful with ":0"). Call
// ServeListener on the result to serve.
func (p *Plugin) Listen(addr string) (net.Listener, error) {
	return net.Listen("tcp", addr)
}

// ServeListener accepts and serves connections on an already-bound listener. It blocks until Accept fails.
func (p *Plugin) ServeListener(ln net.Listener) error {
	for {
		conn, err := ln.Accept()
		if err != nil {
			return err
		}
		go p.serveConn(conn)
	}
}

func (p *Plugin) serveConn(conn net.Conn) {
	defer conn.Close()
	if tc, ok := conn.(*net.TCPConn); ok {
		_ = tc.SetNoDelay(true)
	}
	sc := bufio.NewScanner(conn)
	sc.Buffer(make([]byte, 0, 64*1024), 1<<20) // a line can be ~64KB; give it room
	w := bufio.NewWriter(conn)
	for sc.Scan() {
		reply := p.handleLine(sc.Bytes())
		if reply != nil {
			if _, err := w.Write(reply); err != nil {
				return
			}
			if err := w.Flush(); err != nil {
				return
			}
		}
	}
}

// handleLine dispatches one NDJSON line; it returns the reply bytes to write (requests) or nil (events / junk).
func (p *Plugin) handleLine(line []byte) []byte {
	line = bytes.TrimSpace(line)
	if len(line) == 0 {
		return nil
	}
	var m wireMsg
	if err := json.Unmarshal(line, &m); err != nil {
		return nil // junk line: skip it
	}
	switch m.T {
	case "event":
		p.dispatchEvent(Event{
			Kind:   m.Kind,
			Detail: rawOrNil(m.JSON),
			PID:    m.PID,
			Tag:    m.Tag,
			Raw:    append(json.RawMessage(nil), line...),
		})
		return nil
	case "request":
		verdict := p.dispatchRequest(Request{
			URL:         m.URL,
			Method:      m.Method,
			Destination: m.Destination,
			PID:         m.PID,
			Tag:         m.Tag,
			ID:          m.ID,
		})
		reply, err := json.Marshal(wireReply{ID: m.ID, Verdict: verdict})
		if err != nil {
			return nil
		}
		return append(reply, '\n')
	}
	return nil
}

func (p *Plugin) dispatchEvent(ev Event) {
	if p.OnEvent == nil {
		return
	}
	defer p.recoverSlot("OnEvent")
	p.OnEvent(ev)
}

func (p *Plugin) dispatchRequest(req Request) (v Verdict) {
	v = Continue
	if p.OnRequest == nil {
		return
	}
	defer p.recoverSlot("OnRequest (continuing the hop)")
	return normalizeVerdict(p.OnRequest(req))
}

// recoverSlot turns a panicking slot into "did nothing": a bug here never wedges the browser.
func (p *Plugin) recoverSlot(slot string) {
	if r := recover(); r != nil {
		fmt.Fprintf(os.Stderr, "korvusplugin[%s]: %s panicked: %v\n", p.name(), slot, r)
	}
}

func (p *Plugin) name() string {
	if p.Name != "" {
		return p.Name
	}
	return "go-plugin"
}

// normalizeVerdict maps whatever a handler returned onto the wire's block/continue (continue unless it means block).
func normalizeVerdict(v Verdict) Verdict {
	if strings.EqualFold(strings.TrimSpace(string(v)), string(Block)) {
		return Block
	}
	return Continue
}

// rawOrNil returns r unless it is empty or the literal null, in which case nil.
func rawOrNil(r json.RawMessage) json.RawMessage {
	if len(r) == 0 || string(bytes.TrimSpace(r)) == "null" {
		return nil
	}
	return r
}
