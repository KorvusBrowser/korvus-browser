# Native plugin SDKs — write a korvus plugin in Python, Node or Go

A korvus plugin is a C shared library built against [`korvus_plugin.h`](../korvus_plugin.h). That is exactly right
for the hot slots — `on_call`, `on_request` — where a plugin must be fast and in-process. It is a high bar for the
**infrequent** slots: an observer that ships events to a log pipeline, a request judge that calls out to a policy
service. Someone filling those would often rather write Python, Node or Go than C.

The [`remote-bridge`](../../../tools/abi3/firstparty/remote-bridge.c) first-party plugin is the door. It is a tiny C
shim loaded into korvus that forwards the slot callbacks to a **server over a socket**, so the real logic lives on
the other end of the wire, in whatever language you like. These SDKs are that other end — thin wrappers that let you
fill the slots the bridge forwards in each language, and nothing else:

| language | SDK | import |
|---|---|---|
| Python | [`python/korvus_plugin.py`](python/korvus_plugin.py) | `from korvus_plugin import KorvusPlugin, CONTINUE, BLOCK` |
| Node   | [`node/korvus-plugin.mjs`](node/korvus-plugin.mjs)   | `import { KorvusPlugin, CONTINUE, BLOCK } from "./korvus-plugin.mjs"` |
| Go     | [`go/korvusplugin/plugin.go`](go/korvusplugin/plugin.go) | `import ".../plugins/sdk/go/korvusplugin"` |

Each has its own `README.md` and a runnable `hello` sample. They are **userland** — they depend only on their
language's standard library, never on korvus itself, and run as an ordinary process beside the browser.

## Running one

Start your server (see each SDK's README), then load the bridge into korvus pointed at it:

```sh
korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
```

- `host` / `port` — where your server listens (the bridge connects out to it).
- `forward_requests` — when `true`, the bridge forwards `on_request` and applies your verdict; leave it off for an
  observer that only watches events.
- `tag` — an opaque label echoed in every message, so one server can tell several bridges apart.

## The slots the bridge forwards

The bridge carries exactly two of korvus's slots — the ones a non-C author reaches for:

- **observer** (`on_event`) — every context / page / navigation / request / response / console event korvus
  reports, forwarded fire-and-forget. Your handler returns nothing; observing never slows the browser down.
- **network verdict** (`on_request`, when `forward_requests` is on) — each request hop, and the bridge **blocks**
  on your one-line reply, then applies it: return `BLOCK` to refuse the hop (korvus's `KORVUS_BLOCK`), anything else
  continues.

The richer slots in the header (`on_call`, `on_response`, identity, input planning, the ABI-3 event/verdict slots)
stay C-side for now; extending the bridge to carry more is a change to `remote-bridge.c`, not to these SDKs.

## Wire protocol

The bridge speaks the simplest real forward: a plain TCP stream of **newline-delimited JSON** (NDJSON), one object
per line, UTF-8. No framing, no handshake, no dependencies — which is why a server in any language is a few lines.

Each line the bridge **sends**:

```jsonc
{"t":"event",   "pid":P, "tag":T, "kind":"page.created", "json":{...}}   // or "json":null
{"t":"request", "pid":P, "tag":T, "id":N, "url":"...", "method":"GET", "destination":"document"}
```

For a `"request"` line — and only then — the bridge reads exactly one reply line back on the **same** connection:

```jsonc
{"id":N, "verdict":"block"}      // or "continue"
```

korvus runs the browser endpoint and each page as separate OS processes and loads the bridge into each, so your
server will see **several connections at once** — one per process. Within a connection, messages are strictly
serial (the bridge waits for each request's reply before sending the next line). The SDKs handle each connection on
its own thread/goroutine/async chain and expose `pid` and `tag` on every message so you can stitch the session
together. The reference server these SDKs generalize is
[`remote_bridge_server.py`](../../../tools/abi3/firstparty/remote_bridge_server.py).

> **A note on the wire.** The bridge is framed as plain TCP NDJSON, not WebSocket — a WebSocket handshake would be
> extra code on the C side for nothing these slots need. These SDKs match the bridge as shipped.
