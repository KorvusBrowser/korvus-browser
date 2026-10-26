# srv.py PORT PROBE OUT: the probe (an expression answering rows, or a promise of them) inlined in a page at `/`; the
# page POSTs its rows to /report, written to OUT, and the server exits. Dual-stack, so http://localhost:PORT is a second,
# cross-site origin. `tests/worker_script_loading_live.rs` serves the same routes:
#   /js?b=BODY&ct=TYPE&st=STATUS&d=DELAY_MS&xcto=1&cc=CACHE_CONTROL&acao=1   BODY; ct=none sends no Content-Type
#   /redir?to=URL                                                          302 to URL
#   /html?b=BODY                                                           BODY as text/html
#   /count                                                                 {host + path: [request headers, t (epoch ms)]}
import http.server, socketserver, socket, sys, os, json, urllib.parse, threading, time
PORT, PROBE, OUT = int(sys.argv[1]), open(sys.argv[2], encoding='utf-8').read(), sys.argv[3]
PAGE = ('<!doctype html><html><head><meta charset=utf-8></head><body><script>\nvar __r;\n'
        'try { __r = ' + PROBE + '; } catch (e) { __r = [["FATAL-OUTER", String(e)]]; }\n'
        "Promise.resolve(__r).then(function (r) { window.__report = JSON.stringify(r); fetch('/report', {method: 'POST', body: window.__report}); });\n"
        '</script></body></html>').encode()
SEEN = {}; LOCK = threading.Lock()
KEEP = ('sec-fetch-dest', 'sec-fetch-mode', 'sec-fetch-site', 'cache-control', 'pragma', 'referer', 'origin')
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        u = urllib.parse.urlsplit(self.path); q = dict(urllib.parse.parse_qsl(u.query, keep_blank_values=True))
        host = self.headers.get('Host', '').split(':')[0]
        with LOCK:
            e = {k.lower(): v for k, v in self.headers.items() if k.lower() in KEEP}; e['t'] = time.time() * 1000
            SEEN.setdefault(host + self.path, []).append(e)
        st, hs, b = 200, [], b''
        if u.path == '/':
            b = PAGE; hs = [('Content-Type', 'text/html; charset=utf-8'), ('Cache-Control', 'no-store')]
        elif u.path == '/js':
            if q.get('d'): time.sleep(int(q['d']) / 1000.0)
            b = q.get('b', '').encode(); st = int(q.get('st', '200'))
            ct = q.get('ct', 'text/javascript')
            if ct != 'none': hs.append(('Content-Type', ct))
            if q.get('xcto'): hs.append(('X-Content-Type-Options', 'nosniff'))
            hs.append(('Cache-Control', q.get('cc', 'no-store')))
            if q.get('acao'): hs.append(('Access-Control-Allow-Origin', '*'))
        elif u.path == '/redir':
            st = 302; hs = [('Location', q.get('to', '/')), ('Cache-Control', 'no-store')]
        elif u.path == '/html':
            b = q.get('b', '').encode(); hs = [('Content-Type', 'text/html; charset=utf-8'), ('Cache-Control', 'no-store')]
        elif u.path == '/count':
            with LOCK: b = json.dumps(SEEN).encode()
            hs = [('Content-Type', 'application/json'), ('Cache-Control', 'no-store'), ('Access-Control-Allow-Origin', '*')]
        else:
            st = 404; hs = [('Content-Type', 'text/plain'), ('Cache-Control', 'no-store')]
        self.send_response(st)
        for k, v in hs: self.send_header(k, v)
        self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0')); body = self.rfile.read(n)
        self.send_response(200); self.send_header('Content-Length', '0'); self.end_headers()
        if self.path == '/report':
            open(OUT, 'wb').write(body); os._exit(0)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True; allow_reuse_address = True; address_family = socket.AF_INET6
    def server_bind(self):
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0); super().server_bind()
S(('::', PORT), H).serve_forever()
