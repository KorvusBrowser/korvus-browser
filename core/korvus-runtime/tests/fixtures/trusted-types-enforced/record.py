#!/usr/bin/env python3
"""Run `probe.js` in stock Chrome over every (CSP delivery, case) pair and write Chrome's rows, with each pair's
delivery (the header, or the `<head>` meta, and the case) so the engine's test replays exactly what Chrome saw.

    python3 record.py /usr/bin/google-chrome chrome-150.json [runs]

One loopback server, one throwaway profile per page, `--headless=new --dump-dom`: the page runs the probe in an inline
script and writes its rows into `<pre id=out>`; the dumped DOM is read back. Every pair is run `runs` times (default 3)
and must answer the same rows each time, or the script stops (a racing row is not a fixture). The process each run
starts is the only one it stops."""
import html
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
PROBE = open(os.path.join(HERE, 'probe.js')).read()
TT = "require-trusted-types-for 'script'"
# (key, header name or None, header value or meta content, case)
PAIRS = [('header/' + c, 'Content-Security-Policy', TT, c) for c in ('log', 'none', 'partial', 'nul', 'thr', 'child')] + [
    ('report-only/log', 'Content-Security-Policy-Report-Only', TT, 'log'),
    ('report-only/none', 'Content-Security-Policy-Report-Only', TT, 'none'),
    ('meta/log', None, TT, 'log'),
    ('meta/none', None, TT, 'none'),
    ('nocsp/log', None, None, 'log'),
    ('names', 'Content-Security-Policy', "trusted-types a default; " + TT, 'names'),
    ('dupes', None, None, 'dupes'),
    ('evalmsg/script-src', 'Content-Security-Policy', "script-src 'unsafe-inline' 'self'", 'evalmsg'),
    ('evalmsg/default-src', 'Content-Security-Policy', "default-src 'self' 'unsafe-inline'", 'evalmsg'),
    ('evalmsg/tt', 'Content-Security-Policy', TT + "; script-src 'unsafe-inline' 'unsafe-eval'", 'evalmsg'),
    ('srcdoc', 'Content-Security-Policy', TT + "; script-src 'unsafe-inline' 'unsafe-eval'", 'srcdoc'),
]


def page(meta, case):
    head = '<meta http-equiv="Content-Security-Policy" content="%s">' % html.escape(meta) if meta else ''
    return ('<!doctype html><html><head>%s<title>t</title></head><body>x<pre id="out"></pre><script>globalThis.__case = %s;\n'
            'Promise.resolve(%s).then((r) => { document.getElementById("out").textContent = JSON.stringify(r); });</script></body></html>'
            % (head, json.dumps(case), PROBE)).encode()


def serve():
    import http.server
    import socketserver

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            key = self.path.lstrip('/').split('?')[0]
            pair = next((p for p in PAIRS if p[0] == key), None)
            if pair is None:
                self.send_response(404)
                self.end_headers()
                return
            _, hname, hval, case = pair
            body = page(hval if hname is None else None, case)
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            if hname:
                self.send_header(hname, hval)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(body)

    class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    s = S(('127.0.0.1', 0), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s.server_address[1]


def run(chrome, url):
    prof = tempfile.mkdtemp(prefix='tt-enf-')
    try:
        out = subprocess.run([chrome, '--headless=new', '--no-first-run', '--no-default-browser-check', '--user-data-dir=' + prof,
                              '--dump-dom', url], capture_output=True, text=True, timeout=60).stdout
    finally:
        shutil.rmtree(prof, ignore_errors=True)
    m = re.search(r'<pre id="out">(.*?)</pre>', out, flags=re.S)
    if not m:
        raise SystemExit('no rows from %s:\n%s' % (url, out[:2000]))
    return json.loads(html.unescape(m.group(1)))


def main():
    chrome, out = sys.argv[1], sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    port = serve()
    got = {}
    for key, *_ in PAIRS:
        answers = [run(chrome, 'http://127.0.0.1:%d/%s' % (port, key)) for _ in range(runs)]
        if any(a != answers[0] for a in answers):
            raise SystemExit('%s: the runs disagree:\n%s' % (key, json.dumps(answers, indent=1)))
        got[key] = answers[0]
        print(key, len(answers[0]), 'rows', flush=True)
    pairs = {key: {'header': [h, v] if h else None, 'meta': v if (h is None and v) else None, 'case': c} for key, h, v, c in PAIRS}
    with open(out, 'w') as f:
        json.dump({'chrome': '150.0.7871.46', 'pairs': pairs, 'answers': got}, f, indent=1, ensure_ascii=False)
        f.write('\n')


if __name__ == '__main__':
    main()
