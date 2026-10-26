#!/usr/bin/env python3
"""korvus_plugin -- write a korvus plugin in Python, over the remote-plugin bridge.

korvus's plugin ABI is a C ABI: every slot is a C function pointer, which is exactly right for the hot slots
(on_call, on_request) where a plugin must be fast and in-process. The `remote-bridge` first-party plugin opens that
door to the *infrequent* slots for people who would rather not write C: it is a tiny C shim loaded into korvus that
forwards the slot callbacks to a server over a socket, so the real logic lives on the other end of the wire, in
whatever language the author likes. This module is that other end, in Python.

You subclass `KorvusPlugin` (or hand it callbacks) and fill the slots the bridge forwards:

    from korvus_plugin import KorvusPlugin, CONTINUE, BLOCK

    class MyPlugin(KorvusPlugin):
        def on_event(self, ev):
            print(ev.kind, ev.detail)

        def on_request(self, req):
            return BLOCK if "ads." in req.url else CONTINUE

    MyPlugin(name="my-plugin").serve(port=9099)

Then load the bridge into korvus, pointed at this server:

    korvus serve --plugin './remote-bridge.so:{"host":"127.0.0.1","port":9099,"forward_requests":true}'

WHAT THE BRIDGE FORWARDS. The bridge speaks a plain TCP stream of newline-delimited JSON (NDJSON) -- one object per
line, UTF-8, no framing or handshake. It carries exactly two of korvus's slots:

  * on_event (the observer slot) -- every context / page / navigation / request / response / console event korvus
    reports. Fire-and-forget: this server sends no reply, so observing never slows the browser down.
  * on_request (the network verdict slot), when the bridge is loaded with `forward_requests:true` -- each request
    hop, and the bridge BLOCKS on this server's one-line reply, then applies it. Return `BLOCK` to refuse the hop
    (korvus's KORVUS_BLOCK); anything else continues.

korvus runs the browser endpoint and each page as separate OS processes and loads the bridge into each, so several
connections arrive at once (one per process); `tag`/`pid` on each message tell them apart. This server handles each
connection on its own thread and the slot methods must be thread-safe. A slot that raises is logged and treated as
"did nothing" (on_request continues), so a bug on this side never wedges the browser.

Stdlib only (socket, threading, json); no dependencies. Works on CPython 3.8+.
"""
from __future__ import annotations

import json
import socket
import sys
import threading
from typing import Callable, Optional

# on_request verdicts (the wire values the bridge understands). Return one from on_request.
CONTINUE = "continue"  # let the hop proceed (the default)
BLOCK = "block"        # refuse the hop (korvus's KORVUS_BLOCK)


class Event:
    """One observer event the bridge forwarded (on_event). Fire-and-forget: there is nothing to answer.

    `kind` is korvus's event name -- "context.created", "context.closed", "page.created", "page.closed",
    "navigation", "request", "response" or "console". `detail` is the event's own JSON payload as a dict (or None
    when korvus sent none, or it was too large for the bridge to embed). `pid` and `tag` identify the korvus process
    and the bridge instance the line came from. `raw` is the whole decoded line.
    """

    __slots__ = ("kind", "detail", "pid", "tag", "raw")

    def __init__(self, raw: dict) -> None:
        self.raw = raw
        self.kind: str = str(raw.get("kind") or "")
        self.detail: Optional[dict] = raw.get("json") if isinstance(raw.get("json"), (dict, list)) else None
        self.pid: int = int(raw.get("pid") or 0)
        self.tag: str = str(raw.get("tag") or "")

    def __repr__(self) -> str:
        return f"Event(kind={self.kind!r}, pid={self.pid}, tag={self.tag!r})"


class Request:
    """One request hop awaiting a verdict (on_request). Return CONTINUE or BLOCK from on_request.

    `url`, `method` and `destination` (the Fetch request destination -- "document", "image", "script", ...) describe
    the hop. `pid`/`tag` identify the source process/bridge, and `id` is the bridge's per-connection request id (the
    reply is matched to it automatically).
    """

    __slots__ = ("url", "method", "destination", "pid", "tag", "id", "raw")

    def __init__(self, raw: dict) -> None:
        self.raw = raw
        self.url: str = str(raw.get("url") or "")
        self.method: str = str(raw.get("method") or "")
        self.destination: str = str(raw.get("destination") or "")
        self.pid: int = int(raw.get("pid") or 0)
        self.tag: str = str(raw.get("tag") or "")
        self.id = raw.get("id")

    def __repr__(self) -> str:
        return f"Request(method={self.method!r}, url={self.url!r}, destination={self.destination!r})"


def _normalize_verdict(result) -> str:
    """Map whatever a handler returned onto the wire's "block"/"continue" (continue unless it clearly means block)."""
    if result is True:
        return BLOCK
    if isinstance(result, str) and result.strip().lower() == BLOCK:
        return BLOCK
    return CONTINUE


class KorvusPlugin:
    """A korvus plugin written in Python, served over the remote-plugin bridge.

    Fill the slots the bridge forwards, either by subclassing and overriding `on_event` / `on_request`, or by passing
    callables to the constructor (a callable wins over the subclass method for that slot). Then call `serve()`.
    """

    def __init__(
        self,
        name: str = "python-plugin",
        on_event: Optional[Callable[[Event], None]] = None,
        on_request: Optional[Callable[[Request], str]] = None,
    ) -> None:
        self.name = name
        if on_event is not None:
            self._on_event = on_event
        if on_request is not None:
            self._on_request = on_request
        self._sock: Optional[socket.socket] = None
        self.port: int = 0  # the bound port, filled by serve()/bind()

    # ── the slots (override these, or pass callables to __init__) ────────────────────────────────────────

    def on_event(self, event: Event) -> None:
        """An observer event arrived. Fire-and-forget; the return value is ignored. Default: do nothing."""

    def on_request(self, request: Request) -> str:
        """A request hop awaits a verdict. Return CONTINUE (default) or BLOCK."""
        return CONTINUE

    # The constructor's callables shadow the methods above when present.
    _on_event: Optional[Callable[[Event], None]] = None
    _on_request: Optional[Callable[[Request], str]] = None

    def _dispatch_event(self, event: Event) -> None:
        (self._on_event or self.on_event)(event)

    def _dispatch_request(self, request: Request) -> str:
        return (self._on_request or self.on_request)(request)

    # ── the server ───────────────────────────────────────────────────────────────────────────────────────

    def bind(self, host: str = "127.0.0.1", port: int = 9099) -> int:
        """Bind and listen without accepting yet; returns the bound port (useful with port 0). Call serve() after."""
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind((host, port))
        s.listen(64)
        self._sock = s
        self.port = s.getsockname()[1]
        return self.port

    def serve(self, host: str = "127.0.0.1", port: int = 9099) -> None:
        """Serve forever: accept the bridge's connections and dispatch each line to the slots. Blocks.

        If bind() was already called the host/port here are ignored.
        """
        if self._sock is None:
            self.bind(host, port)
        assert self._sock is not None
        while True:
            try:
                conn, _ = self._sock.accept()
            except OSError:
                break
            threading.Thread(target=self._serve_conn, args=(conn,), daemon=True).start()

    def close(self) -> None:
        """Stop accepting new connections (open ones drain on their own)."""
        if self._sock is not None:
            try:
                self._sock.close()
            finally:
                self._sock = None

    def _serve_conn(self, conn: socket.socket) -> None:
        buf = b""
        try:
            while True:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    reply = self._handle_line(line)
                    if reply is not None:
                        conn.sendall(reply)
        except OSError:
            pass
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def _handle_line(self, line: bytes) -> Optional[bytes]:
        """Dispatch one NDJSON line; returns the reply bytes to write (requests) or None (events / junk)."""
        line = line.strip()
        if not line:
            return None
        try:
            obj = json.loads(line.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            return None
        if not isinstance(obj, dict):
            return None
        t = obj.get("t")
        if t == "event":
            try:
                self._dispatch_event(Event(obj))
            except Exception as e:  # a slot must never kill the connection
                self._warn(f"on_event raised: {e!r}")
            return None
        if t == "request":
            verdict = CONTINUE
            try:
                verdict = _normalize_verdict(self._dispatch_request(Request(obj)))
            except Exception as e:
                self._warn(f"on_request raised: {e!r} (continuing the hop)")
            reply = {"id": obj.get("id"), "verdict": verdict}
            return (json.dumps(reply) + "\n").encode("utf-8")
        return None

    def _warn(self, msg: str) -> None:
        print(f"korvus_plugin[{self.name}]: {msg}", file=sys.stderr, flush=True)
