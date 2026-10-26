#!/usr/bin/env python3
"""record.py chrome|korvus OUT.json [DELAY_MS]: this directory's HTTP/2 probe on one engine (ABPROG F12).

A loopback TLS server speaking h2 only (the leaf of ../wire-spelling, 127.0.0.1) serves page.html at `/` and records
every HEADERS frame a client sends -- its stream, its priority fields (exclusive, dependency, weight byte), `:path`
and the `priority` field -- answering every request but `/` and `/report` DELAY ms late (default 150), so streams
overlap. An HTTP/1.1 origin answers `/r-*` with status lines of their own reason phrases. The page reports the
statusText it read. Chrome: /usr/bin/google-chrome headed on Xvfb :99 (--ozone-platform=x11), NO DevTools, HOME with
an NSS db trusting ../wire-spelling/tls-ca.pem. korvus: `korvus fetch` ($KORVUS_BIN) with KORVUS_EXTRA_CA. Needs the
`hpack` module. Run it through abkit/live_slot.sh."""
import http.server, json, os, pathlib, socket, socketserver, ssl, struct, subprocess, sys, tempfile, threading, time
import hpack

ENGINE, OUT = sys.argv[1], sys.argv[2]
DELAY = float(sys.argv[3]) / 1000 if len(sys.argv) > 3 else 0.15
HERE = pathlib.Path(__file__).resolve().parent
TLS = HERE.parent / 'wire-spelling'
sys.path.insert(0, str(HERE.parents[4] / 'tools/parity'))
import replay as R  # noqa: E402

def fp():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); p = s.getsockname()[1]; s.close(); return p

H2, H1 = fp(), fp()
PAGE = (HERE / 'page.html').read_bytes().replace(b'{{H1}}', b'http://127.0.0.1:%d' % H1)
log, lock, got, report = [], threading.Lock(), threading.Event(), {}

def frame(t, flags, sid, payload):
    return struct.pack('>I', len(payload))[1:] + bytes([t, flags]) + struct.pack('>I', sid) + payload

def serve_h2(s, conn):
    buf, wlock, enc, dec, pending, bodies = b'', threading.Lock(), hpack.Encoder(), hpack.Decoder(), {}, {}
    def send(b):
        with wlock:
            try: s.sendall(b)
            except Exception: pass
    def respond(sid, path, body_in=b''):
        if path == '/report':
            report.update(json.loads(body_in or b'{}')); got.set()
        if path not in ('/', '/report'):
            time.sleep(DELAY)
        body = PAGE if path == '/' else b'ok'
        hdr = [(':status', '200'), ('content-type', 'text/html' if path == '/' else 'text/plain'), ('cache-control', 'no-store'), ('content-length', str(len(body)))]
        with wlock:
            block = enc.encode(hdr)
        send(frame(1, 4, sid, block) + frame(0, 1, sid, body))
    def need(n):
        nonlocal buf
        while len(buf) < n:
            d = s.recv(65536)
            if not d: raise EOFError
            buf += d
    need(24); buf = buf[24:]
    send(frame(4, 0, 0, struct.pack('>HI', 3, 100)))
    t0 = time.time()
    while True:
        need(9)
        ln = struct.unpack('>I', b'\x00' + buf[:3])[0]
        ft, fl, sid = buf[3], buf[4], struct.unpack('>I', buf[5:9])[0] & 0x7fffffff
        need(9 + ln)
        pl, buf = buf[9:9 + ln], buf[9 + ln:]
        if ft == 4 and not fl & 1:
            send(frame(4, 1, 0, b''))
        elif ft == 6 and not fl & 1:
            send(frame(6, 1, 0, pl))
        elif ft == 0:
            bodies[sid] = bodies.get(sid, b'') + pl
            if fl & 1 and sid in pending:
                threading.Thread(target=respond, args=(sid, pending.pop(sid), bodies.get(sid, b'')), daemon=True).start()
        elif ft == 1:
            q = 1 if fl & 0x8 else 0
            prio = None
            if fl & 0x20:
                dep = struct.unpack('>I', pl[q:q + 4])[0]
                prio = [dep >> 31, dep & 0x7fffffff, pl[q + 4]]; q += 5
            blk = pl[q:len(pl) - (pl[0] if fl & 0x8 else 0)]
            hs = dict((k if isinstance(k, str) else k.decode(), v if isinstance(v, str) else v.decode()) for k, v in dec.decode(blk))
            path = hs.get(':path', '/')
            with lock:
                log.append({'conn': conn, 'ms': round((time.time() - t0) * 1000, 1), 'stream': sid, 'prio': prio, 'path': path, 'priority': hs.get('priority')})
            if fl & 1:
                threading.Thread(target=respond, args=(sid, path), daemon=True).start()
            else:
                pending[sid] = path
        elif ft == 7:
            return

ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(str(TLS / 'tls-leaf.pem'), str(TLS / 'tls-leaf.key'))
ctx.set_alpn_protocols(['h2'])
ls = socket.socket(); ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); ls.bind(('127.0.0.1', H2)); ls.listen(16)
def accept():
    n = 0
    while True:
        c, _ = ls.accept(); n += 1
        def run(c=c, n=n):
            try: serve_h2(ctx.wrap_socket(c, server_side=True), n)
            except Exception: pass
        threading.Thread(target=run, daemon=True).start()
threading.Thread(target=accept, daemon=True).start()

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        line = {'/r-canon': 'HTTP/1.1 200 OK', '/r-awesome': 'HTTP/1.1 200 Awesome  ', '/r-none': 'HTTP/1.1 200', '/r-404': 'HTTP/1.1 404 Nope'}.get(self.path, 'HTTP/1.1 404 Not Found')
        self.wfile.write((line + '\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nok').encode())
class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
threading.Thread(target=S(('127.0.0.1', H1), H).serve_forever, daemon=True).start()

url = 'https://127.0.0.1:%d/' % H2
if ENGINE == 'chrome':
    home = R.chrome_home_trusting(pathlib.Path(tempfile.mkdtemp(prefix='h2prio-', dir='/tmp/claude-1000')), TLS / 'tls-ca.pem')
    prof = tempfile.mkdtemp(prefix='h2prio-p-', dir='/tmp/claude-1000')
    p = subprocess.Popen(['/usr/bin/google-chrome', '--ozone-platform=x11', '--user-data-dir=' + prof, '--no-first-run',
                          '--no-default-browser-check', '--window-size=1920,1080', url],
                         env=dict(os.environ, DISPLAY=':99', HOME=str(home)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
else:
    kbin = os.environ.get('KORVUS_BIN', 'target/release/korvus')
    p = subprocess.Popen([kbin, 'fetch', url, '--wait', '6'], env=dict(os.environ, KORVUS_BUNDLE='chrome-152-linux-20261016T1022', KORVUS_EXTRA_CA=str(TLS / 'tls-ca.pem')),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
got.wait(30)
time.sleep(0.3)
os.killpg(p.pid, 9); p.wait()
# the frames by path, in arrival order, the dependency named by the path it depends on
by_id = {}
rows = []
for f in log:
    if f['path'] == '/favicon.ico':
        by_id[(f['conn'], f['stream'])] = f['path']
        continue
    by_id[(f['conn'], f['stream'])] = f['path']
    excl, dep, weight = f['prio'] or [None, None, None]
    rows.append({'path': f['path'], 'priority': f['priority'], 'exclusive': excl, 'weight': weight,
                 'depends_on': None if dep is None else ('' if dep == 0 else by_id.get((f['conn'], dep), '?%d' % dep))})
json.dump({'engine': ENGINE, 'report': report, 'headers': rows, 'raw': log}, open(OUT, 'w'), indent=1)
print(ENGINE, '->', OUT, 'ok' if got.is_set() else 'NO REPORT', len(rows), 'requests')
