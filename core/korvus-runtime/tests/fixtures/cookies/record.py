#!/usr/bin/env python3
"""record.py ENGINE OUT.json: `probe.js` on http://a.ck.localhost:PORT/d/page.html, one dual-stack server answering every
host the probe names (a.ck.localhost, b.ck.localhost, c.ck2.localhost, 127.0.0.1 -- `*.localhost` is loopback to Chrome
and to this box's resolver):

  /d/page.html         the probe inlined in a classic script; its rows go to window.__report and POST /report
  /frame.html          the cross-site frame: its document.cookie, three writes, its document.cookie again, to the parent
  /set?c=LINE[&c=..]   one `Set-Cookie: LINE` per `c`; a GIF for an <img>, text for a fetch, credentialed CORS
  /echo                the request's Cookie header as text, credentialed CORS

ENGINE is `chrome` (stock /usr/bin/google-chrome, headed on $DISPLAY -- run it under abkit/live_slot.sh with DISPLAY=:99 --
fresh profile, NO DevTools) or `korvus` ($KORVUS_BIN fetch). Ports in the rows read PORT."""
import http.server, json, os, re, signal, socket, subprocess, sys, tempfile, threading, time, urllib.parse
ENGINE, OUT = sys.argv[1], sys.argv[2]
HERE = os.path.dirname(os.path.abspath(__file__))
PROBE = open(os.path.join(HERE, 'probe.js'), encoding='utf-8').read()
PAGE = ("<!doctype html><html><head><meta charset=utf-8></head><body><script>\nvar __r;\ntry { __r = " + PROBE +
        "; } catch (e) { __r = [['FATAL-OUTER', String(e)]]; }\nPromise.resolve(__r).then(function (r) { window.__report = JSON.stringify(r); "
        "fetch('/report', {method: 'POST', body: window.__report}); }, function (e) { window.__report = JSON.stringify([['FATAL', String(e)]]); "
        "fetch('/report', {method: 'POST', body: window.__report}); });\n</script></body></html>").encode()
FRAME = (b"<!doctype html><html><body><script>\nvar r = [document.cookie];\ndocument.cookie = 'kf_lax=1';\n"
         b"document.cookie = 'kf_none=1; SameSite=None; Secure';\ndocument.cookie = 'kf_part=1; SameSite=None; Secure; Partitioned';\n"
         b"r.push(document.cookie);\nparent.postMessage(r.join(' || '), '*');\n</script></body></html>")
GIF = bytes.fromhex('47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b')
report, done = [None], threading.Event()


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *a):
        pass

    def answer(self, body, ctype, extra=()):
        self.send_response(200)
        self.send_header('Content-Type', ctype)
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        o = self.headers.get('Origin')
        if o:
            self.send_header('Access-Control-Allow-Origin', o)
            self.send_header('Access-Control-Allow-Credentials', 'true')
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urllib.parse.urlsplit(self.path)
        if u.path == '/d/page.html':
            self.answer(PAGE, 'text/html; charset=utf-8')
        elif u.path == '/frame.html':
            self.answer(FRAME, 'text/html; charset=utf-8')
        elif u.path == '/set':
            lines = urllib.parse.parse_qs(u.query, keep_blank_values=True).get('c', [])
            img = 'image' in (self.headers.get('Accept') or '') or self.headers.get('Sec-Fetch-Dest') == 'image'
            self.answer(GIF if img else b'ok', 'image/gif' if img else 'text/plain', [('Set-Cookie', l) for l in lines])
        elif u.path == '/echo':
            self.answer((self.headers.get('Cookie') or '').encode(), 'text/plain')
        else:
            self.answer(b'', 'text/plain')

    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0'))
        body = self.rfile.read(n)
        self.answer(b'', 'text/plain')
        if self.path == '/report':
            report[0] = body.decode('utf-8')
            done.set()


class S(http.server.ThreadingHTTPServer):
    address_family = socket.AF_INET6
    daemon_threads = True

    def server_bind(self):
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
        super().server_bind()


srv = S(('::', 0), H)
PORT = srv.server_address[1]
threading.Thread(target=srv.serve_forever, daemon=True).start()
url = 'http://a.ck.localhost:%d/d/page.html' % PORT
if ENGINE == 'chrome':
    prof = tempfile.mkdtemp(prefix='nocdp-', dir=os.environ.get('PROFILE_DIR'))
    p = subprocess.Popen([os.environ.get('CHROME_BIN', '/usr/bin/google-chrome'), '--ozone-platform=x11', '--user-data-dir=' + prof,
                          '--no-first-run', '--no-default-browser-check', '--window-size=1920,1080', url],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    done.wait(60)
    os.killpg(p.pid, signal.SIGKILL)
    p.wait()
else:
    env = dict(os.environ, KORVUS_BUNDLE='chrome-152-linux-20261016T1022')
    subprocess.run(['timeout', '120', os.environ['KORVUS_BIN'], 'fetch', url, '--wait', '10'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
if report[0] is None:
    sys.exit('NO ANSWER from ' + ENGINE)
rows = json.loads(re.sub(r':%d(?![0-9])' % PORT, ':PORT', report[0]))
json.dump(rows, open(OUT, 'w'), indent=1, ensure_ascii=False)
print(ENGINE, '->', OUT, len(rows), 'rows')
