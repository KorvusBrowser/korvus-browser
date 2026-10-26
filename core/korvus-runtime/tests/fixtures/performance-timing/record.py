#!/usr/bin/env python3
"""Run `probe.html` in stock Chrome and write its answers: python3 record.py /usr/bin/google-chrome chrome-150.json [runs]

Run under an X server (`xvfb-run -a -s "-screen 0 1920x1080x24" python3 record.py ...`): Chrome is launched HEADED by
hand with a debugging port, as the Chrome persona runs (no automation flag), and the answer is read over CDP once the
page has set `window.__r` -- the probe waits on timers after the load event, which a `--dump-dom` run does not. One
loopback server (`/` the probe, `/frame` a same-origin frame document), a fresh profile per run; the runs (default 3)
must agree, or the script stops. The process each run starts is the only one it stops."""
import json, os, shutil, signal, socket, subprocess, sys, tempfile, threading, time
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
PAGES = {'/': open(os.path.join(HERE, 'probe.html'), 'rb').read(), '/frame': open(os.path.join(HERE, 'frame.html'), 'rb').read()}


def serve():
    import http.server
    import socketserver

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            body = PAGES.get(self.path)
            if body is None:
                self.send_response(404)
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    s = S(('127.0.0.1', 0), H)
    threading.Thread(target=s.serve_forever, daemon=True).start()
    return s.server_address[1]


def run(chrome, url):
    s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close()
    prof = tempfile.mkdtemp(prefix='pt-')
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
            pg.wait_for_function('window.__r', timeout=15000)
            return json.loads(pg.evaluate('window.__r'))
    finally:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except Exception:
            pass
        p.wait()
        shutil.rmtree(prof, ignore_errors=True)


def main():
    chrome, out = sys.argv[1], sys.argv[2]
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    url = 'http://127.0.0.1:%d/' % serve()
    answers = [run(chrome, url) for _ in range(runs)]
    if any(a != answers[0] for a in answers):
        raise SystemExit('the runs disagree:\n' + json.dumps(answers, indent=1))
    with open(out, 'w') as f:
        json.dump(answers[0], f, indent=1, ensure_ascii=False)
        f.write('\n')
    print(json.dumps(answers[0], indent=1))


if __name__ == '__main__':
    main()
