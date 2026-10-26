# korvus plugin SDK — Python

Write a korvus plugin in Python, over the [remote-plugin bridge](../README.md). No C, no korvus rebuild, no
dependencies — the standard library is enough.

## Install

Copy `korvus_plugin.py` next to your code (it is a single, dependency-free module), or add this directory to your
`PYTHONPATH`.

## Use

```python
from korvus_plugin import KorvusPlugin, CONTINUE, BLOCK

class MyPlugin(KorvusPlugin):
    def on_event(self, ev):
        print(ev.kind, ev.detail)        # observer: context/page/navigation/request/response/console

    def on_request(self, req):
        return BLOCK if "ads." in req.url else CONTINUE   # network verdict

MyPlugin(name="my-plugin").serve(port=9099)
```

Or hand the slots in as callables instead of subclassing:

```python
KorvusPlugin(on_request=lambda req: BLOCK if req.destination == "image" else CONTINUE).serve(port=9099)
```

Then load the bridge into korvus, pointed at your server:

```sh
korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'
```

Drop `"forward_requests":true` if you only want the observer stream (`on_request` is then never asked).

## The slots the bridge forwards

| slot | method | you return | korvus does |
|---|---|---|---|
| observer | `on_event(Event)` | nothing | reports context/page/navigation/request/response/console events (fire-and-forget) |
| network | `on_request(Request)` | `CONTINUE` or `BLOCK` | lets the hop proceed, or refuses it |

`Event` has `.kind`, `.detail` (the event's JSON payload as a dict, or `None`), `.pid`, `.tag`. `Request` has
`.url`, `.method`, `.destination`, `.pid`, `.tag`. A slot runs on the connection's own thread, so make it
thread-safe; a slot that raises is logged and treated as "did nothing" (the hop continues).

## Run the sample

```sh
python3 hello.py --port 9099 --block-substr ads.
```

`hello.py` prints every event to stderr and blocks any request whose URL contains `--block-substr`. With
`--port 0` it binds an ephemeral port and prints `PORT <n>` on stdout.

## How it connects

The bridge speaks newline-delimited JSON over a plain TCP stream. korvus runs the browser endpoint and each page as
separate processes and loads the bridge into each, so several connections arrive at once — one per process; the
`tag`/`pid` on each message tell them apart. See [`../README.md`](../README.md) for the wire protocol.
