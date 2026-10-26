#!/usr/bin/env python3
"""The Python twin of `tests/canvas_draw_image_live.rs`'s server, for re-recording Chrome's answers.

    python3 canvas-probe-server.py <same-origin port> <cross-origin port>

Both ports serve this directory with the same routes the Rust server answers:
  /<file>            the file, typed by extension, `Cache-Control: no-store`
  /cors/<file>       the same, plus `Access-Control-Allow-Origin: *`
  /garbage/<file>    200 with the file's type and bytes that are no image
  /page.html         an empty document
  anything else      404
"""
import http.server
import os
import socketserver
import sys
import threading

D = os.path.dirname(os.path.abspath(__file__))
TYPES = {'png': 'image/png', 'jpg': 'image/jpeg', 'gif': 'image/gif', 'bmp': 'image/bmp', 'webp': 'image/webp'}


class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, status, ctype, body, acao=False):
        self.send_response(status)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        if acao:
            self.send_header('Access-Control-Allow-Origin', '*')
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split('?')[0]
        if path == '/page.html':
            return self.send(200, 'text/html', b'<!doctype html><html><head></head><body></body></html>')
        name = path.rsplit('/', 1)[-1]
        f = os.path.join(D, name)
        ext = name.rsplit('.', 1)[-1]
        if ext not in TYPES or not os.path.isfile(f):
            return self.send(404, 'text/plain', b'not found')
        body = b'this is not an image at all' if path.startswith('/garbage/') else open(f, 'rb').read()
        self.send(200, TYPES[ext], body, acao=path.startswith('/cors/'))


class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


for port in sys.argv[1:3]:
    s = S(('127.0.0.1', int(port)), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()
threading.Event().wait()
