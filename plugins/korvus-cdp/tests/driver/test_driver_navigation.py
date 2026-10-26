#!/usr/bin/env python3
"""The parity driver's snapshot of a page that navigates itself, on both engines.

    KORVUS_BIN=<your build> python3 -m unittest discover -s plugins/korvus-cdp/tests/driver -p 'test_driver_*.py' -v

WHY HERE AND NOT IN tools/parity/tests
That directory is the harness (M7): an engine unit may not edit it. This test IMPORTS the driver
unchanged and asks it the question L19 E10/E11 asked by hand -- what `final_url` and `title` does
`snapshot()` record for a document that navigates -- so the answer is the driver's own, read through
its own `Page.navigate` / `loadEventFired` / settle / probe protocol, never a re-implementation of it.

WHAT IS ASSERTED
Chrome's snapshot and korvus's agree on `final_url` and `title`, and both are the TARGET document's --
for a `<meta http-equiv=refresh>`, a `location.href` in an inline script, and a cross-origin 301. Each
engine gets its own fixture server run, and both runs use the same two ports, so a URL is comparable
as a string.

The oracle is stock `/usr/bin/google-chrome`, launched by `driver.attach` exactly as the parity sweep
launches it. A box with no Chrome SKIPS (said so, loudly); it never passes.
"""
from __future__ import annotations

import http.server
import os
import shutil
import socketserver
import sys
import threading
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "tools" / "parity"))

import driver  # noqa: E402

KORVUS_BIN = Path(os.environ.get("KORVUS_BIN") or (REPO / "target" / "release" / "korvus"))
DEADLINE_S = 20.0

TARGET = b"<!doctype html><html><head><title>landed</title></head><body><p>arrived at target</p></body></html>"
PAGES = {
    "/target.html": (200, [], TARGET),
    "/mr0.html": (200, [], b'<!doctype html><title>mr0</title><meta http-equiv=refresh content="0; url=target.html"><p>mr0 body</p>'),
    "/href.html": (200, [], b"<!doctype html><title>href</title><script>location.href='target.html'</script><p>after</p>"),
    "/r301.html": (301, [("Location", "http://127.0.0.1:{B}/target.html")], b""),
}


class _Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args) -> None:  # noqa: D401 - quiet
        pass

    def do_GET(self) -> None:  # noqa: N802 - http.server's name
        path = self.path.split("?", 1)[0]
        if path not in PAGES:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        status, headers, body = PAGES[path]
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v.replace("{B}", str(self.server.other_port)))
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class _Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class Origins:
    """Two loopback origins on FIXED ports for one engine run, shut down after it."""

    def __init__(self, ports: tuple[int, int]) -> None:
        self.servers = [_Server(("127.0.0.1", p), _Handler) for p in ports]
        self.servers[0].other_port = ports[1]
        self.servers[1].other_port = ports[0]

    def __enter__(self) -> "Origins":
        for s in self.servers:
            threading.Thread(target=s.serve_forever, daemon=True).start()
        return self

    def __exit__(self, *exc) -> None:
        for s in self.servers:
            s.shutdown()
            s.server_close()


def _two_free_ports() -> tuple[int, int]:
    a = driver._free_port()
    b = driver._free_port()
    while b == a:
        b = driver._free_port()
    return a, b


class NavigatingPages(unittest.TestCase):
    snaps: dict[tuple[str, str], dict]

    @classmethod
    def setUpClass(cls) -> None:
        if not KORVUS_BIN.exists():
            raise unittest.SkipTest(f"no korvus binary at {KORVUS_BIN} -- set KORVUS_BIN")
        if not (shutil.which("google-chrome") or Path("/usr/bin/google-chrome").exists()):
            raise unittest.SkipTest("SKIPPED, NOT PASSED: no stock google-chrome to ask")
        cls.ports = _two_free_ports()
        cls.snaps = {}
        for engine in ("chrome", "korvus"):
            for path in ("/mr0.html", "/href.html", "/r301.html"):
                with Origins(cls.ports):
                    url = f"http://127.0.0.1:{cls.ports[0]}{path}"
                    cls.snaps[(engine, path)] = driver.run(
                        engine, url, deadline_s=DEADLINE_S, korvus_bin=str(KORVUS_BIN), headed=False
                    )

    def _check(self, path: str, want_url: str) -> None:
        chrome, korvus = self.snaps[("chrome", path)], self.snaps[("korvus", path)]
        self.assertEqual(chrome["final_url"], want_url, f"{path}: Chrome itself did not land where asked")
        self.assertEqual(chrome["title"], "landed", f"{path}: Chrome itself did not land where asked")
        self.assertEqual(
            (korvus["final_url"], korvus["title"]),
            (chrome["final_url"], chrome["title"]),
            f"{path}: korvus's snapshot (final_url, title) differs from Chrome's",
        )

    def test_meta_refresh(self) -> None:
        self._check("/mr0.html", f"http://127.0.0.1:{self.ports[0]}/target.html")

    def test_location_href_during_parse(self) -> None:
        self._check("/href.html", f"http://127.0.0.1:{self.ports[0]}/target.html")

    def test_cross_origin_redirect(self) -> None:
        self._check("/r301.html", f"http://127.0.0.1:{self.ports[1]}/target.html")


if __name__ == "__main__":
    unittest.main()
