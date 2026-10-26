#!/usr/bin/env python3
"""hello -- the smallest korvus plugin written in Python over the remote-plugin bridge.

It observes every event korvus forwards (printed to stderr) and refuses any request hop whose URL contains a chosen
substring. Run it, then point the `remote-bridge` plugin at it:

    python3 hello.py --port 9099 --block-substr ads.
    korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'

With `--port 0` it binds an ephemeral port and prints `PORT <n>` on stdout, so a parent process can learn the port
(the SDK probe drives it this way). Events go to stderr, keeping stdout for that one line.
"""
from __future__ import annotations

import argparse
import sys

from korvus_plugin import BLOCK, CONTINUE, Event, KorvusPlugin, Request


def main() -> int:
    ap = argparse.ArgumentParser(description="a hello korvus plugin over the remote bridge")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=9099, help="TCP port (0 binds an ephemeral port, printed as PORT <n>)")
    ap.add_argument("--block-substr", default="", help="refuse a request whose URL contains this substring")
    args = ap.parse_args()

    class Hello(KorvusPlugin):
        def on_event(self, ev: Event) -> None:
            print(f"event: {ev.kind} (pid={ev.pid} tag={ev.tag})", file=sys.stderr, flush=True)

        def on_request(self, req: Request) -> str:
            if args.block_substr and args.block_substr in req.url:
                print(f"block: {req.method} {req.url}", file=sys.stderr, flush=True)
                return BLOCK
            return CONTINUE

    plugin = Hello(name="hello-py")
    plugin.bind(args.host, args.port)
    print(f"PORT {plugin.port}", flush=True)  # announce the bound port, then serve forever
    try:
        plugin.serve()
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
