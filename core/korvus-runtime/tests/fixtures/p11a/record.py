#!/usr/bin/env python3
"""Record a stock browser's answers to `probe.js` (and the navigation table) over CDP.

    python3 record.py <browser binary> <out prefix> [--headed]

Starts `probe-server.py`'s routes on two free loopback ports, launches the browser ITSELF with
`--remote-debugging-port` and a throwaway profile (never Playwright's launcher, which adds
`--enable-automation`), attaches with `suppress_origin=True`, and writes `<out prefix>.<section>.txt`
for every section plus `<out prefix>.nav.txt`. `--headed` runs the browser headed (use under
`xvfb-run`); the default is `--headless=new`. The browser process this script started is the only one
it stops.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

import websocket

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
DOC = b'<!doctype html><html><head></head><body></body></html>'


def free_port():
    s = socket.socket()
    s.bind(('127.0.0.1', 0))
    p = s.getsockname()[1]
    s.close()
    return p


def serve(port):
    import http.server
    import socketserver

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            path = self.path.split('?')[0].split('#')[0]
            ok = path in ('/page.html', '/child.html', '/p2.html', '/x')
            body = DOC if ok else b'not found'
            self.send_response(200 if ok else 404)
            self.send_header('Content-Type', 'text/html' if ok else 'text/plain')
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(body)

    class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    s = S(('127.0.0.1', port), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()


class Tab:
    def __init__(self, ws_url):
        self.ws = websocket.create_connection(ws_url, suppress_origin=True, timeout=120)
        self.n = 0
        self.events = []

    def call(self, method, **params):
        self.n += 1
        mid = self.n
        self.ws.send(json.dumps({'id': mid, 'method': method, 'params': params}))
        while True:
            m = json.loads(self.ws.recv())
            if m.get('id') == mid:
                if 'error' in m:
                    raise RuntimeError(f'{method}: {m["error"]}')
                return m.get('result', {})
            self.events.append(m)

    def wait_event(self, name, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            for i, e in enumerate(self.events):
                if e.get('method') == name:
                    self.events.pop(i)
                    return e
            self.ws.settimeout(max(0.1, end - time.time()))
            try:
                self.events.append(json.loads(self.ws.recv()))
            except websocket.WebSocketTimeoutException:
                break
        return None

    def eval(self, expr):
        r = self.call('Runtime.evaluate', expression=expr, awaitPromise=True, returnByValue=True)
        if 'exceptionDetails' in r:
            return 'EVAL THREW ' + json.dumps(r['exceptionDetails'].get('exception', {}).get('description'))
        return r['result'].get('value')

    def goto(self, url):
        self.events.clear()
        self.call('Page.navigate', url=url)
        self.wait_event('Page.loadEventFired', 15)


def main():
    binary, prefix = sys.argv[1], sys.argv[2]
    headed = '--headed' in sys.argv
    a, b = free_port(), free_port()
    serve(a)
    serve(b)
    same, xo = f'http://127.0.0.1:{a}/', f'http://127.0.0.1:{b}/'
    probe = open(os.path.join(HERE, 'probe.js')).read()
    port = free_port()
    prof = tempfile.mkdtemp(prefix='p11a-profile-')
    argv = [binary, f'--remote-debugging-port={port}', f'--user-data-dir={prof}', '--no-first-run',
            '--no-default-browser-check', 'about:blank']
    if not headed:
        argv.insert(1, '--headless=new')
    proc = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                ver = json.load(urllib.request.urlopen(f'http://127.0.0.1:{port}/json/version', timeout=1))
                break
            except Exception:
                time.sleep(0.2)
        print('browser', ver.get('Browser'), file=sys.stderr)

        def new_tab():
            t = json.load(urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{port}/json/new?about:blank', method='PUT')))
            tab = Tab(t['webSocketDebuggerUrl'])
            tab.call('Page.enable')
            tab.call('Runtime.enable')
            return tab

        for section in [x for x in sys.argv[3:] if not x.startswith('--')] or ['desc', 'recv', 'replace', 'vt', 'xo', 'so', 'fragment', 'location']:
            tab = new_tab()
            tab.goto(same + 'page.html')
            s = tab.eval(f'({probe})({json.dumps(same)}, {json.dumps(xo)}, {json.dumps(section)})')
            with open(f'{prefix}.{section}.txt', 'w') as f:
                f.write(str(s) + '\n')
            print(section, 'lines', str(s).count('\n') + 1, file=sys.stderr)

        # Navigation through every Location setter, `window.location =`, `document.location =`, and
        # the three methods: where the tab ends up and how many history entries it gained.
        cases = [
            ('location.href = "p2.html?h"', 'location.href = "p2.html?h"'),
            ('location.pathname = "/p2.html"', 'location.pathname = "/p2.html"'),
            ('location.pathname = "p2.html"', 'location.pathname = "p2.html"'),
            ('location.search = "q=1"', 'location.search = "q=1"'),
            ('location.search = "?q=2"', 'location.search = "?q=2"'),
            ('location.search = ""', 'location.search = ""'),
            ('location.hostname = "localhost"', 'location.hostname = "localhost"'),
            ('location.host = "localhost:PORT"', f'location.host = "localhost:{a}"'),
            ('location.port = "OTHER"', f'location.port = "{b}"'),
            ('location.protocol = "http:"', 'location.protocol = "http:"'),
            ('location.hash = "h"', 'location.hash = "h"'),
            ('window.location = "p2.html?w"', 'window.location = "p2.html?w"'),
            ('document.location = "p2.html?d"', 'document.location = "p2.html?d"'),
            ('location.assign("p2.html?a")', 'location.assign("p2.html?a")'),
            ('location.replace("p2.html?r")', 'location.replace("p2.html?r")'),
            ('location.reload()', 'location.reload()'),
            ('self = 1 then location', 'self = 1, typeof self'),
        ]
        lines = []
        for label, expr in cases:
            tab = new_tab()
            tab.goto(same + 'page.html')
            h0 = tab.eval('window.__mark = 1; history.length')
            r = tab.eval(f'(() => {{ let r; try {{ r = String({expr}); }} catch (e) {{ r = "THROW " + e.name + ": " + e.message; }} return r + " | sync href=" + location.href.replace({json.dumps(same)}, "<same>/") + " mark=" + window.__mark; }})()')
            tab.wait_event('Page.loadEventFired', 3)
            time.sleep(0.5)
            after = tab.eval(f'location.href.replace({json.dumps(same)}, "<same>/").replace({json.dumps(same.replace("127.0.0.1", "localhost"))}, "<same-localhost>/").replace({json.dumps(xo)}, "<xo>/") + " dh=" + (history.length - {h0}) + " reloaded=" + (window.__mark === undefined)')
            line = f'{label} => {r} || after {after}'
            lines.append(line.replace(str(a), '<same-port>').replace(str(b), '<xo-port>'))
        with open(f'{prefix}.nav.txt', 'w') as f:
            f.write('\n'.join(lines) + '\n')
    finally:
        proc.terminate()
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(prof, ignore_errors=True)


main()
