#!/usr/bin/env python3
"""Record `pool.html` in stock Chrome: xvfb-run -a -s "-screen 0 1920x1080x24" python3 record-pool.py /usr/bin/google-chrome pool.chrome-150.json [runs]

A STUN server on 127.0.0.1 answering every Binding Request after 80 ms (its own thread per request, XOR-MAPPED-ADDRESS
203.0.113.9:<the request's port>), the page served from 127.0.0.1, Chrome launched HEADED by hand with a debugging port
(no automation flag) and attached over CDP; `window.__r` is read once set. Each case in its own browser, a fresh profile
per run; a case's runs (default 3) must agree, or the script stops."""
import json, os, shutil, signal, socket, struct, subprocess, sys, tempfile, threading, time
from playwright.sync_api import sync_playwright

HERE = os.path.dirname(os.path.abspath(__file__))
MAGIC = 0x2112A442


def stun():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(('127.0.0.1', 0))

    def answer(data, addr):
        time.sleep(0.08)
        xport = addr[1] ^ (MAGIC >> 16)
        xip = bytes(a ^ b for a, b in zip(bytes([203, 0, 113, 9]), struct.pack('!I', MAGIC)))
        attr = struct.pack('!HHBBH', 0x0020, 8, 0, 1, xport) + xip
        s.sendto(struct.pack('!HHI', 0x0101, len(attr), MAGIC) + data[8:20] + attr, addr)

    def loop():
        while True:
            data, addr = s.recvfrom(2048)
            if len(data) >= 20 and data[0:2] == b'\x00\x01' and struct.unpack('!I', data[4:8])[0] == MAGIC:
                threading.Thread(target=answer, args=(data, addr), daemon=True).start()

    threading.Thread(target=loop, daemon=True).start()
    return s.getsockname()[1]


def serve():
    import http.server
    import socketserver
    page = open(os.path.join(HERE, 'pool.html'), 'rb').read()

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(page)))
            self.end_headers()
            self.wfile.write(page)

    class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    srv = S(('127.0.0.1', 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv.server_address[1]


def run(chrome, url):
    s = socket.socket(); s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]; s.close()
    prof = tempfile.mkdtemp(prefix='pool-')
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
            pg.wait_for_function('window.__r', timeout=20000)
            return pg.evaluate('window.__r')
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
    base = 'http://127.0.0.1:%d/pool.html?stun=%d' % (serve(), stun())
    lines = []
    for case in ('0,1', '0,3', '1,1', '1,3'):
        answers = [run(chrome, base + '&case=' + case) for _ in range(runs)]
        if any(a != answers[0] for a in answers):
            raise SystemExit('case %s: the runs disagree:\n' % case + json.dumps(answers, indent=1))
        lines.append(answers[0])
    with open(out, 'w') as f:
        json.dump(lines, f, indent=1)
        f.write('\n')
    print(json.dumps(lines, indent=1))


if __name__ == '__main__':
    main()
