#!/usr/bin/env python3
"""Run `probe.js` over CDP against a browser or `korvus serve`, one section and phase at a time.

    xvfb-run -a -s "-screen 0 1920x1080x24" python3 record.py chrome /usr/bin/google-chrome <out prefix>
    python3 record.py korvus <korvus binary> <out prefix>

Writes `<out prefix>.<section>.txt`. Chrome is launched BY THIS SCRIPT, headed (PARITY R14: run it
under `xvfb-run`), with `--remote-debugging-port` and a throwaway profile -- never Playwright's
launcher, which adds `--enable-automation` -- and attached with `suppress_origin=True`, so no flag is
added to the browser for the tool's sake. The korvus side is `korvus serve-page <port>`, the same server
the parity driver attaches to. The fixture is an empty document on a loopback port (and `frame.html`,
which one frame loads). The process this
script started is the only one it stops.
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
DOC = b'<!doctype html><html><head></head><body></body></html>'
FRAME = b'<!doctype html><style>html,body{margin:0}</style><div id="in" style="position:absolute;left:15px;top:25px;width:40px;height:30px"></div>'
PAGES = {'/page.html': DOC, '/frame.html': FRAME}
SECTIONS = [('scroll', 2), ('frames', 3)]


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
            body = PAGES.get(self.path.split('?')[0])
            ok = body is not None
            body = body if ok else b'not found'
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

    def eval(self, expr):
        r = self.call('Runtime.evaluate', expression=expr, awaitPromise=True, returnByValue=True)
        if 'exceptionDetails' in r:
            return 'EVAL THREW ' + json.dumps(r['exceptionDetails'].get('exception', {}).get('description'))
        return r['result'].get('value')


def page_target(port):
    for _ in range(150):
        try:
            for t in json.load(urllib.request.urlopen(f'http://127.0.0.1:{port}/json/list', timeout=1)):
                if t.get('type') == 'page' and t.get('webSocketDebuggerUrl'):
                    return t['webSocketDebuggerUrl']
        except Exception:
            pass
        time.sleep(0.2)
    raise RuntimeError('no page target')


def launch(kind, binary, port):
    if kind == 'korvus':
        return subprocess.Popen([binary, 'serve-page', str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL), None
    prof = tempfile.mkdtemp(prefix='p13a-profile-')
    argv = [binary, f'--remote-debugging-port={port}', f'--user-data-dir={prof}', '--no-first-run',
            '--no-default-browser-check', '--window-size=1920,1080', 'about:blank']
    return subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL), prof


def main():
    kind, binary, prefix = sys.argv[1], sys.argv[2], sys.argv[3]
    only = sys.argv[4:]
    a = free_port()
    serve(a)
    url = f'http://127.0.0.1:{a}/page.html'
    probe = open(os.path.join(HERE, 'probe.js')).read()
    for section, phases in SECTIONS:
        if only and section not in only:
            continue
        # A FRESH BROWSER PER SECTION: nothing one section leaves behind can reach the next.
        port = free_port()
        proc, prof = launch(kind, binary, port)
        try:
            tab = Tab(page_target(port))
            tab.call('Page.enable')
            tab.call('Page.navigate', url=url)
            for _ in range(100):
                if tab.eval('document.readyState') == 'complete':
                    break
                time.sleep(0.1)
            lines = []
            for phase in range(phases):
                s = tab.eval(f'({probe})({json.dumps(section)}, {phase})')
                lines.append(f'-- phase {phase}')
                lines.append(str(s))
            with open(f'{prefix}.{section}.txt', 'w') as f:
                f.write('\n'.join(lines) + '\n')
            print(section, file=sys.stderr)
        finally:
            proc.terminate()
            try:
                proc.wait(10)
            except subprocess.TimeoutExpired:
                proc.kill()
            if prof:
                shutil.rmtree(prof, ignore_errors=True)


main()
