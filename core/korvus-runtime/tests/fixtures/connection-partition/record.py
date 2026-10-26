#!/usr/bin/env python3
"""np/run.py ENGINE OUT.json: which CONNECTION each request rides (Chrome's partitioning), over plain HTTP/1.1
keep-alive. Server A serves the page (http://127.0.0.1:A/) and a CROSS-SITE frame (http://localhost:A/frame);
server B (127.0.0.1:B) answers every load and records the client PORT it arrived from. The page and the frame
each load B with credentials (an <img>) and without (a cors fetch(), credentials same-origin to another
origin = omitted), then a second <img> each after the first. Out: tag -> connection class."""
import http.server, json, os, socket, subprocess, sys, tempfile, threading, time
ENGINE, OUT = sys.argv[1], sys.argv[2]
def port():
    s = socket.socket(); s.bind(('127.0.0.1', 0)); p = s.getsockname()[1]; s.close(); return p
PA, PB = port(), port()
hits, done = [], threading.Event()
GIF = bytes.fromhex('47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b')
SEQ = """
const B = 'http://127.0.0.1:%d/hit?tag=';
const img = (t) => new Promise((r) => { const i = new Image(); i.onload = i.onerror = () => r(); i.src = B + t; });
async function run(who) {
  await img(who + '-img1');
  await fetch(B + who + '-fetch1').then((r) => r.text()).catch(() => {});
  await img(who + '-img2');
  await fetch(B + who + '-fetch2').then((r) => r.text()).catch(() => {});
}
""" % PB
PAGE = ("<!doctype html><body><script>" + SEQ + """
run('page').then(() => new Promise((r) => { addEventListener('message', r); const f = document.createElement('iframe'); f.src = 'http://localhost:%d/frame'; document.body.appendChild(f); }))
  .then(() => fetch('/done'));
</script></body>""" % PA).encode()
FRAME = ("<!doctype html><body><script>" + SEQ + "run('frame').then(() => parent.postMessage('ok', '*'));</script></body>").encode()
class A(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        b = PAGE if self.path == '/' else FRAME if self.path == '/frame' else b'ok'
        if self.path == '/done': done.set()
        self.send_response(200); self.send_header('content-type', 'text/html'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
class B(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        tag = self.path.split('tag=')[-1]
        hits.append((tag, self.client_address[1], self.headers.get('cookie')))
        img = tag.endswith(('img1', 'img2'))
        b = GIF if img else b'ok'
        self.send_response(200); self.send_header('content-type', 'image/gif' if img else 'text/plain')
        self.send_header('access-control-allow-origin', '*'); self.send_header('content-length', str(len(b))); self.end_headers(); self.wfile.write(b)
for P, H in ((PA, A), (PB, B)):
    srv = http.server.ThreadingHTTPServer(('127.0.0.1', P), H); threading.Thread(target=srv.serve_forever, daemon=True).start()
url = 'http://127.0.0.1:%d/' % PA
if ENGINE == 'chrome':
    prof = tempfile.mkdtemp()
    p = subprocess.Popen(['setsid', os.environ.get('CHROME_BIN', '/usr/bin/google-chrome'), '--ozone-platform=x11', '--user-data-dir=' + prof, '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--disable-component-update', '--disable-sync', url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    done.wait(30); time.sleep(0.3)
    subprocess.run(['pkill', '-TERM', '-f', '--', '--user-data-dir=' + prof]); time.sleep(1); subprocess.run(['pkill', '-KILL', '-f', '--', '--user-data-dir=' + prof])
else:
    env = dict(os.environ, KORVUS_BUNDLE='chrome-152-linux-20261016T1022')
    subprocess.run(['timeout', '60', os.environ.get('KORVUS_BIN', '/home/yoons/바탕화면/projects/korvus-browser/target/release/korvus'), 'fetch', url, '--wait', '4'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
classes, out = {}, {}
for tag, prt, _ in hits:
    classes.setdefault(prt, 'conn%d' % (len(classes) + 1)); out[tag] = classes[prt]
json.dump(out, open(OUT, 'w'), indent=1, sort_keys=True); print(ENGINE, json.dumps(out, sort_keys=True))
