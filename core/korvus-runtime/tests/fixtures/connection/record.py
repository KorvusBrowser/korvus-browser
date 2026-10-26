#!/usr/bin/env python3
"""record.py OUT.json: `navigator.connection.rtt` / `.downlink` as stock Chrome 150 reads them on 21 hosts (ABPROG F12).

A page on 127.0.0.1 frames `http://127.0.0.K:PORT/frame` for K = 2..22 -- 21 hosts, each another site, so each frame
reads Blink's per-host noise (`GetRandomMultiplier`) under its own renderer's salt -- and reports every frame's pair.
Chrome: /usr/bin/google-chrome headed on Xvfb :99 (--ozone-platform=x11), NO DevTools, a fresh profile. Run it
through abkit/live_slot.sh; the fixture keeps several runs' readings (`tests/connection_estimate_live.rs`)."""
import http.server, json, os, socket, socketserver, subprocess, sys, tempfile, threading

OUT = sys.argv[1]
HOSTS = ['127.0.0.%d' % k for k in range(2, 23)]
PAGE = ('<!doctype html><title>connection</title><body><script>var got = {};\n'
        'addEventListener("message", function (e) { got[e.data.host] = [e.data.rtt, e.data.downlink];\n'
        '  if (Object.keys(got).length === %d) fetch("/report", {method: "POST", body: JSON.stringify(got)}); });\n'
        'for (var h of %s) { var f = document.createElement("iframe"); f.src = "http://" + h + ":" + location.port + "/frame"; document.body.appendChild(f); }\n'
        '</script>' % (len(HOSTS), json.dumps(HOSTS))).encode()
FRAME = (b'<!doctype html><script>parent.postMessage({host: location.hostname, rtt: navigator.connection.rtt, '
         b'downlink: navigator.connection.downlink}, "*")</script>')
got = threading.Event()

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        b = PAGE if self.path == '/' else FRAME if self.path == '/frame' else b''
        self.send_response(200); self.send_header('Content-Type', 'text/html'); self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0')); body = self.rfile.read(n)
        self.send_response(200); self.send_header('Content-Length', '0'); self.end_headers()
        if self.path == '/report':
            open(OUT, 'wb').write(body); got.set()

class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True

s = socket.socket(); s.bind(('127.0.0.1', 0)); PORT = s.getsockname()[1]; s.close()
for h in ['127.0.0.1'] + HOSTS:
    threading.Thread(target=S((h, PORT), H).serve_forever, daemon=True).start()
prof = tempfile.mkdtemp(prefix='conn-', dir='/tmp/claude-1000')
p = subprocess.Popen(['/usr/bin/google-chrome', '--ozone-platform=x11', '--user-data-dir=' + prof, '--no-first-run',
                      '--no-default-browser-check', '--window-size=1920,1080', 'http://127.0.0.1:%d/' % PORT],
                     env=dict(os.environ, DISPLAY=':99'), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
got.wait(30)
os.killpg(p.pid, 9)
p.wait()
print('->', OUT, 'ok' if got.is_set() else 'NO REPORT')
