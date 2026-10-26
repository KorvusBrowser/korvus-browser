#!/usr/bin/env python3
"""The loopback server `bodies-probe.js` is recorded against in Chrome.

    python3 bodies-probe-server.py 18977
    # launch google-chrome --headless=new --remote-debugging-port=0 yourself, attach over CDP
    # (websocket.create_connection(url, suppress_origin=True)), navigate to http://127.0.0.1:18977/any,
    # Runtime.evaluate "(<bodies-probe.js source>)(location.origin + '/')" and store the resolved string
    # plus a newline as bodies-probe.chrome-150.txt.

Every route answers what `tests/bodies_live.rs`'s Rust server answers (same status, same content
headers, same bytes, `Connection: close`; Python adds Server/Date, which the probe never reads), so the two
runs are comparable line for line.
"""
import http.server
import sys

ROUTES = {
    "/b.bin": ("application/octet-stream", bytes(range(256))),
    "/j.json": ("application/json", '{"k":"vé","n":[1,2]}'.encode("utf-8")),
    "/latin.txt": ("text/plain", b"caf\xe9"),
    "/bom.json": ("application/json", b'\xef\xbb\xbf{"a":1}'),
    "/u16.txt": ("text/plain", b"\xff\xfeh\x00i\x00"),
}

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send(self, status, ctype, body, extra=()):
        self.send_response(status)
        if ctype is not None:
            self.send_header("Content-Type", ctype)
        for k, v in extra:
            self.send_header(k, v)
        if status != 204:
            self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        if status != 204:
            self.wfile.write(body)

    def do_GET(self):
        if self.path == "/empty204":
            return self.send(204, None, b"")
        if self.path in ROUTES:
            ctype, body = ROUTES[self.path]
            return self.send(200, ctype, body)
        return self.send(404, "text/plain", b"not found")

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n)
        ct = self.headers.get("content-type") or "none"
        if self.path == "/echo":
            return self.send(200, "application/octet-stream", body, [("x-ct", ct)])
        return self.send(404, "text/plain", b"not found")

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
