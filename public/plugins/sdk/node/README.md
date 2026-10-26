# korvus plugin SDK — Node

Write a korvus plugin in Node, over the [remote-plugin bridge](../README.md). No C, no korvus rebuild, no
dependencies — Node's built-in `node:net` is enough. ES modules (`.mjs`); Node 16+.

## Install

Copy `korvus-plugin.mjs` next to your code (it is a single, dependency-free module), or import it by path.

## Use

```js
import { KorvusPlugin, CONTINUE, BLOCK } from "./korvus-plugin.mjs";

class MyPlugin extends KorvusPlugin {
  onEvent(ev)  { console.error(ev.kind, ev.detail); }       // observer stream
  onRequest(req) { return req.url.includes("ads.") ? BLOCK : CONTINUE; }  // network verdict (may be async)
}

await new MyPlugin({ name: "my-plugin" }).listen(9099);
```

Or hand the slots in as callbacks instead of subclassing:

```js
await new KorvusPlugin({
  onRequest: (req) => (req.destination === "image" ? BLOCK : CONTINUE),
}).listen(9099);
```

Then load the bridge into korvus, pointed at your server:

```sh
korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
```

Drop `"forward_requests":true` if you only want the observer stream (`onRequest` is then never asked).

## The slots the bridge forwards

| slot | method | you return | korvus does |
|---|---|---|---|
| observer | `onEvent(Event)` | nothing | reports context/page/navigation/request/response/console events (fire-and-forget) |
| network | `onRequest(Request)` | `CONTINUE` or `BLOCK` (sync or a Promise) | lets the hop proceed, or refuses it |

`Event` has `.kind`, `.detail` (the event's JSON payload, or `null`), `.pid`, `.tag`. `Request` has `.url`,
`.method`, `.destination`, `.pid`, `.tag`. Each connection's lines are processed strictly in order even when
`onRequest` is async; a slot that throws or rejects is logged and treated as "did nothing" (the hop continues).

## Run the sample

```sh
node hello.mjs --port 9099 --block-substr ads.
```

`hello.mjs` prints every event to stderr and blocks any request whose URL contains `--block-substr`. With
`--port 0` it binds an ephemeral port and prints `PORT <n>` on stdout.

## How it connects

The bridge speaks newline-delimited JSON over a plain TCP stream. korvus runs the browser endpoint and each page as
separate processes and loads the bridge into each, so several connections arrive at once — one per process; the
`tag`/`pid` on each message tell them apart. See [`../README.md`](../README.md) for the wire protocol.
