#!/usr/bin/env python3
"""hc/run.py ENGINE OUT.json: the HTTP cache at the wire. One server answers every probe path with the caching headers
its name asks for and logs each request's validators (If-None-Match / If-Modified-Since / Cache-Control / Pragma);
the page fetches each path twice (and some with a RequestInit.cache mode) and reports status, body, transferSize and
deliveryType of the second. Out: per case, the page's view and the server's request log."""
import http.server, json, os, socket, subprocess, sys, tempfile, threading, time
ENGINE, OUT = sys.argv[1], sys.argv[2]
def port():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); p = s.getsockname()[1]; s.close(); return p
P = port()
log, done, report = [], threading.Event(), {}
LM = 'Wed, 07 Oct 2026 12:10:03 GMT'
CASES = {
    # name: response headers for the resource
    'fresh': [('Cache-Control', 'max-age=300')],
    'stale_lm': [('Cache-Control', 'max-age=300'), ('Age', '3178'), ('Last-Modified', LM)],
    'nocache_etag': [('Cache-Control', 'no-cache'), ('ETag', '"v1"')],
    'nostore': [('Cache-Control', 'no-store'), ('ETag', '"v1"')],
    'maxage0_both': [('Cache-Control', 'max-age=0'), ('ETag', '"v2"'), ('Last-Modified', LM)],
    'heuristic': [('Last-Modified', 'Wed, 01 Oct 2026 00:00:00 GMT')],
    'none': [],
    'mode_nocache': [('Cache-Control', 'max-age=300'), ('ETag', '"v3"')],
    'mode_reload': [('Cache-Control', 'max-age=300'), ('ETag', '"v4"')],
    'mode_force': [('Cache-Control', 'max-age=0'), ('ETag', '"v5"')],
    'mode_nostore': [('Cache-Control', 'max-age=300')],
    'vary_origin': [('Cache-Control', 'max-age=300'), ('Vary', 'Origin')],
}
SECOND_MODE = {'mode_nocache': 'no-cache', 'mode_reload': 'reload', 'mode_force': 'force-cache', 'mode_nostore': 'no-store'}
PAGE = ("""<!doctype html><body><script>
(async () => {
  const R = {};
  const cases = %s; const modes = %s;
  for (const c of cases) {
    const u = '/r/' + c;
    const a = await fetch(u).then((r) => r.text());
    const init = modes[c] ? { cache: modes[c] } : {};
    const r2 = await fetch(u, init);
    const b = await r2.text();
    await new Promise((r) => setTimeout(r, 30));
    const es = performance.getEntriesByName(location.origin + u);
    const e = es[es.length - 1];
    R[c] = [r2.status, b, e ? e.transferSize : null, e ? e.deliveryType : null, es.length];
  }
  await fetch('/report', { method: 'POST', body: JSON.stringify(R) });
})();
</script></body>""" % (json.dumps(list(CASES)), json.dumps(SECOND_MODE))).encode()
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == '/':
            b = PAGE; self.send_response(200); self.send_header('content-type', 'text/html'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b); return
        if self.path.startswith('/r/'):
            c = self.path[3:]
            log.append((c, self.headers.get('if-none-match'), self.headers.get('if-modified-since'), self.headers.get('cache-control'), self.headers.get('pragma')))
            hs = CASES.get(c, [])
            etag = dict(hs).get('ETag'); lm = dict(hs).get('Last-Modified')
            if (etag and self.headers.get('if-none-match') == etag) or (not etag and lm and self.headers.get('if-modified-since') == lm):
                self.send_response(304)
                for k, v in hs: self.send_header(k, v)
                self.send_header('content-length', '0'); self.end_headers(); return
            b = ('body-' + c).encode()
            self.send_response(200); self.send_header('content-type', 'text/plain'); self.send_header('date', self.date_time_string())
            for k, v in hs: self.send_header(k, v)
            self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b); return
        self.send_response(404); self.send_header('content-length', '0'); self.end_headers()
    def do_POST(self):
        n = int(self.headers.get('content-length') or 0); report['page'] = json.loads(self.rfile.read(n)); done.set()
        self.send_response(200); self.send_header('content-length', '0'); self.end_headers()
srv = http.server.ThreadingHTTPServer(('127.0.0.1', P), H); threading.Thread(target=srv.serve_forever, daemon=True).start()
url = 'http://127.0.0.1:%d/' % P
if ENGINE == 'chrome':
    prof = tempfile.mkdtemp()
    subprocess.Popen(['setsid', '/usr/bin/google-chrome', '--ozone-platform=x11', '--user-data-dir=' + prof, '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--disable-component-update', '--disable-sync', url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    done.wait(30); time.sleep(0.3)
    subprocess.run(['pkill', '-TERM', '-f', '--', '--user-data-dir=' + prof]); time.sleep(1); subprocess.run(['pkill', '-KILL', '-f', '--', '--user-data-dir=' + prof])
else:
    subprocess.run(['timeout', '60', '/home/yoons/바탕화면/projects/korvus-browser/target/release/korvus', 'fetch', url, '--wait', '5'], env=dict(os.environ, KORVUS_BUNDLE='chrome-152-linux-20261016T1022'), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
out = {}
for c in CASES:
    reqs = [list(r[1:]) for r in log if r[0] == c]
    out[c] = {'page': (report.get('page') or {}).get(c), 'requests': len(reqs), 'second': reqs[1] if len(reqs) > 1 else None}
json.dump(out, open(OUT, 'w'), indent=1, sort_keys=True)
