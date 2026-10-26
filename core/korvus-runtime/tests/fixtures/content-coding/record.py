#!/usr/bin/env python3
"""record.py chrome|korvus OUT.json: this directory's content-coding probe on one engine (ABPROG F12).

`page.html` fetches every case of `cases.json` -- a method, the `Content-Encoding` lines, a body file (`make_bodies.py`)
-- and reports, per case, the status and the bytes the page got (length, a position-weighted sum, the first bytes,
the `Content-Encoding` it reads), or where it failed; then three of them over XHR. Chrome: /usr/bin/google-chrome
headed on Xvfb :99 (--ozone-platform=x11), NO DevTools, a fresh profile. korvus: `korvus fetch` ($KORVUS_BIN, default
the frozen abkit binary). Run it through abkit/live_slot.sh."""
import http.server, json, os, socket, socketserver, subprocess, sys, tempfile, threading, time

ENGINE, OUT = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
CASES = json.load(open(os.path.join(HERE, 'cases.json')))
PAGE = open(os.path.join(HERE, 'page.html'), 'rb').read()
got = threading.Event()

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def answer(self, body, ctype, codings=(), head=False):
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Cache-Control', 'no-store')
        for c in codings:
            self.send_header('Content-Encoding', c)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        if not head:
            self.wfile.write(body)
    def route(self, head):
        p = self.path
        if p == '/': return self.answer(PAGE, 'text/html; charset=utf-8', head=head)
        if p == '/cases.json': return self.answer(json.dumps(CASES).encode(), 'application/json', head=head)
        if p.startswith('/c/') and p[3:] in CASES:
            method, codings, body = CASES[p[3:]]
            data = open(os.path.join(HERE, body + '.bin'), 'rb').read() if body else b''
            return self.answer(data, 'text/plain', codings, head=head)
        self.answer(b'', 'text/plain', head=head)
    def do_GET(self): self.route(False)
    def do_HEAD(self): self.route(True)
    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0')); body = self.rfile.read(n)
        self.answer(b'', 'text/plain')
        if self.path == '/report':
            open(OUT, 'wb').write(body); got.set()

class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True

def port():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); p = s.getsockname()[1]; s.close(); return p

PORT = port()
srv = S(('127.0.0.1', PORT), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
url = 'http://127.0.0.1:%d/' % PORT
if ENGINE == 'chrome':
    prof = tempfile.mkdtemp(prefix='coding-', dir='/tmp/claude-1000')
    p = subprocess.Popen(['/usr/bin/google-chrome', '--ozone-platform=x11', '--user-data-dir=' + prof, '--no-first-run',
                          '--no-default-browser-check', '--window-size=1920,1080', url],
                         env=dict(os.environ, DISPLAY=':99'), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
else:
    kbin = os.environ.get('KORVUS_BIN', 'target/release/korvus')
    p = subprocess.Popen([kbin, 'fetch', url, '--wait', '8'], env=dict(os.environ, KORVUS_BUNDLE='chrome-152-linux-20261016T1022'),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
got.wait(30)
os.killpg(p.pid, 9)
p.wait()
srv.shutdown()
print(ENGINE, '->', OUT, 'ok' if got.is_set() else 'NO REPORT')
