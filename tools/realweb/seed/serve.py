#!/usr/bin/env python3
"""Serve a seed directory for the repro pages. usage: serve.py DIR PORT [PORT ...]
Static files (.js as text/javascript); a POST to an existing file serves that file (form-post navigation),
any other POST answers 200 'ok' (XHR/fetch/beacon probes). One thread per port."""
import http.server, os, sys, threading
DIR = os.path.abspath(sys.argv[1])
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k): super().__init__(*a, directory=DIR, **k)
    def log_message(self, *a): pass
    def guess_type(self, p): return 'text/javascript' if p.split('?')[0].endswith('.js') else super().guess_type(p)
    def do_POST(self):
        n = int(self.headers.get('content-length', 0) or 0); self.rfile.read(n)
        if os.path.isfile(os.path.join(DIR, self.path.split('?')[0].lstrip('/'))): return self.do_GET()
        self.send_response(200); self.send_header('content-type', 'text/plain'); self.send_header('content-length', '2'); self.end_headers(); self.wfile.write(b'ok')
servers = [http.server.ThreadingHTTPServer(('127.0.0.1', int(p)), H) for p in sys.argv[2:]]
for s in servers[1:]: threading.Thread(target=s.serve_forever, daemon=True).start()
servers[0].serve_forever()
