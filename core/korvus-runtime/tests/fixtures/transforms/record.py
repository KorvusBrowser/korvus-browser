#!/usr/bin/env python3
"""Run one of this directory's probe scripts in stock Chrome and write its answers:

    xvfb-run -a -s "-screen 0 1920x1080x24" python3 record.py /usr/bin/google-chrome widget.js widget.chrome-150.json [runs]

Chrome is launched HEADED by hand with a debugging port (no automation flag) and attached over CDP; the script is
evaluated on an empty page served from 127.0.0.1 (`<!doctype html><html><head><meta charset=utf-8></head><body>`, the
page `layout_root_live::answers` loads) and its return value -- a promise is awaited -- is the answer list. A fresh
profile per run; the runs (default 3) must agree, or the script stops. The process each run starts is the only one
it stops."""
import json, os, shutil, signal, socket, subprocess, sys, tempfile, threading, time
from playwright.sync_api import sync_playwright

PAGE = b'<!doctype html><html><head><meta charset=utf-8></head><body></body></html>'


def serve():
    import http.server
    import socketserver

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(PAGE)))
            self.end_headers()
            self.wfile.write(PAGE)

    class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    s = S(('127.0.0.1', 0), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s.server_address[1]


def run(chrome, url, script):
    s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close()
    prof = tempfile.mkdtemp(prefix='tf-')
    p = subprocess.Popen([chrome, '--ozone-platform=x11', f'--remote-debugging-port={port}', f'--user-data-dir={prof}', '--no-first-run',
                          '--no-default-browser-check', '--window-size=1920,1080', 'about:blank'],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        for _ in range(100):
            try:
                socket.create_connection(('127.0.0.1', port), 0.2).close()
                break
            except OSError:
                time.sleep(0.1)
        with sync_playwright() as pw:
            b = pw.chromium.connect_over_cdp(f'http://127.0.0.1:{port}', timeout=30000)
            pg = b.contexts[0].pages[0] if b.contexts[0].pages else b.contexts[0].new_page()
            pg.goto(url, wait_until='load', timeout=30000)
            return json.loads(pg.evaluate('Promise.resolve(%s).then(r => JSON.stringify(r))' % script))
    finally:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except Exception:
            pass
        p.wait()
        shutil.rmtree(prof, ignore_errors=True)


def main():
    chrome, probe, out = sys.argv[1], sys.argv[2], sys.argv[3]
    runs = int(sys.argv[4]) if len(sys.argv) > 4 else 3
    script = open(probe).read()
    url = 'http://127.0.0.1:%d/layout-root' % serve()
    answers = [run(chrome, url, script) for _ in range(runs)]
    if any(a != answers[0] for a in answers):
        raise SystemExit('the runs disagree:\n' + json.dumps(answers))
    with open(out, 'w') as f:
        json.dump(answers[0], f, separators=(',', ':'))
        f.write('\n')
    print(len(answers[0]), 'answers')


if __name__ == '__main__':
    main()
