#!/usr/bin/env python3
"""record.py chrome|korvus OUT.json [HOLD_MS]: the connect-timing probe (page.html + frame.html) on one engine, both
engines behind a CONNECT proxy that holds each tunnel HOLD ms (default 40) before "200 Connection established" -- a
stand-in for a network round trip, so a connection's TCP phase (connectStart -> secureConnectionStart, which spans the
tunnel's setup behind a proxy) has a known length on loopback. HTTPS (HTTP/1.1, keep-alive) on 127.0.0.1 with a leaf
from the parity CA for 127.0.0.1 and localhost.

Chrome: /usr/bin/google-chrome headed on Xvfb :99 (--ozone-platform=x11), NO DevTools, --proxy-server with
--proxy-bypass-list=<-loopback> (loopback otherwise skips the proxy), HOME with an NSS db trusting the CA. korvus:
`korvus fetch` (KORVUS_BIN, default the frozen abkit binary) with HTTPS_PROXY, NO_PROXY="" and KORVUS_EXTRA_CA.
Run it through abkit/live_slot.sh. Writes the frame's {rows, info}; the fixture keeps `rows`."""
import http.server, ipaddress, os, pathlib, socket, socketserver, ssl, subprocess, sys, tempfile, threading, time

ENGINE, OUT = sys.argv[1], sys.argv[2]
HOLD = float(sys.argv[3]) if len(sys.argv) > 3 else 40.0
HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[4]
sys.path.insert(0, str(REPO / 'tools/parity'))
import replay as R  # noqa: E402
from mitmproxy import certs  # noqa: E402
from cryptography import x509  # noqa: E402
from cryptography.hazmat.primitives import serialization  # noqa: E402

def port():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); p = s.getsockname()[1]; s.close(); return p

ca = R.ensure_ca()
CA = str(ca.cert_pem)
store = certs.CertStore.from_store(ca.confdir, 'mitmproxy', 2048)
entry = store.get_cert('127.0.0.1', [x509.IPAddress(ipaddress.ip_address('127.0.0.1')), x509.DNSName('localhost')])
d = tempfile.mkdtemp()
open(d + '/c.pem', 'wb').write(entry.cert.to_pem() + store.default_ca.to_pem())
open(d + '/k.pem', 'wb').write(entry.privatekey.private_bytes(serialization.Encoding.PEM,
    serialization.PrivateFormat.TraditionalOpenSSL, serialization.NoEncryption()))

PAGE = (HERE / 'page.html').read_bytes()
FRAME = (HERE / 'frame.html').read_bytes()
got = threading.Event()

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def answer(self, body, ctype, extra=()):
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Cache-Control', 'no-store')
        for k, v in extra:
            self.send_header(k, v)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        p = self.path
        if p == '/': self.answer(PAGE, 'text/html; charset=utf-8')
        elif p == '/frame': self.answer(FRAME, 'text/html; charset=utf-8')
        elif p in ('/r1', '/r2'): self.answer(b'ok', 'text/plain')
        elif p == '/tao': self.answer(b'ok', 'text/plain', [('Access-Control-Allow-Origin', '*'), ('Timing-Allow-Origin', '*')])
        elif p == '/notao': self.answer(b'ok', 'text/plain', [('Access-Control-Allow-Origin', '*')])
        else: self.answer(b'', 'text/plain')
    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0')); body = self.rfile.read(n)
        self.answer(b'', 'text/plain')
        if self.path == '/report':
            open(OUT, 'wb').write(body); got.set()

class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True

PORT, PPORT = port(), port()
srv = S(('127.0.0.1', PORT), H)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); ctx.load_cert_chain(d + '/c.pem', d + '/k.pem')
ctx.set_alpn_protocols(['http/1.1'])
srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
threading.Thread(target=srv.serve_forever, daemon=True).start()

def pipe(a, b):
    try:
        while True:
            x = a.recv(65536)
            if not x: break
            b.sendall(x)
    except OSError:
        pass
    finally:
        for s in (a, b):
            try: s.shutdown(socket.SHUT_RDWR)
            except OSError: pass

def tunnel(c):
    buf = b''
    while b'\r\n\r\n' not in buf:
        x = c.recv(4096)
        if not x: c.close(); return
        buf += x
    line = buf.split(b'\r\n', 1)[0].decode('latin-1').split()
    if len(line) < 2 or line[0] != 'CONNECT':
        c.sendall(b'HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n'); c.close(); return
    host, p = line[1].rsplit(':', 1)
    up = socket.create_connection(('127.0.0.1' if host.strip('[]') in ('localhost', '127.0.0.1') else host.strip('[]'), int(p)))
    time.sleep(HOLD / 1000.0)
    c.sendall(b'HTTP/1.1 200 Connection established\r\n\r\n')
    threading.Thread(target=pipe, args=(c, up), daemon=True).start()
    threading.Thread(target=pipe, args=(up, c), daemon=True).start()

def proxy():
    l = socket.socket(); l.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    l.bind(('127.0.0.1', PPORT)); l.listen(64)
    while True:
        c, _ = l.accept()
        threading.Thread(target=tunnel, args=(c,), daemon=True).start()
threading.Thread(target=proxy, daemon=True).start()
time.sleep(0.3)

url = 'https://127.0.0.1:%d/' % PORT
proxy_url = 'http://127.0.0.1:%d' % PPORT
if ENGINE == 'chrome':
    t = pathlib.Path(tempfile.mkdtemp(prefix='ct-chrome-', dir=os.environ.get('TMPDIR', '/tmp')))
    home = R.chrome_home_trusting(t, CA)
    env = dict(os.environ, HOME=str(home), DISPLAY=':99')
    p = subprocess.Popen(['/usr/bin/google-chrome', '--ozone-platform=x11', '--user-data-dir=%s/profile' % t,
                          '--no-first-run', '--no-default-browser-check', '--window-size=1920,1080',
                          '--proxy-server=' + proxy_url, '--proxy-bypass-list=<-loopback>', url],
                         env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    got.wait(30)
    os.killpg(p.pid, 9)
else:
    kbin = os.environ.get('KORVUS_BIN', 'target/release/korvus')
    env = dict(os.environ, KORVUS_EXTRA_CA=CA, KORVUS_BUNDLE='chrome-152-linux-20261016T1022', NO_PROXY='', no_proxy='')
    for k in ('HTTPS_PROXY', 'HTTP_PROXY', 'ALL_PROXY', 'https_proxy', 'http_proxy', 'all_proxy'):
        env[k] = proxy_url
    subprocess.run(['timeout', '60', kbin, 'fetch', url, '--wait', '5'], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(open(OUT).read() if got.is_set() else 'NO REPORT')
