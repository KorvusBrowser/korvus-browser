#!/usr/bin/env python3
"""Run `probe.js` in stock Chrome and write its answers: python3 record.py /usr/bin/google-chrome chrome-150.json [runs]

One loopback server, a fresh profile per run, `--headless=new --dump-dom`; the page writes the probe's answers into
`<pre id=out>`. The runs (default 3) must agree, or the script stops."""
import html, json, os, re, shutil, subprocess, sys, tempfile, threading

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = ('<!doctype html><html><head><title>t</title></head><body><pre id="out"></pre><script>'
        'Promise.resolve(%s).then((r) => { document.getElementById("out").textContent = JSON.stringify(r); });'
        '</script></body></html>' % open(os.path.join(HERE, 'probe.js')).read()).encode()


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


def run(chrome, url):
    prof = tempfile.mkdtemp(prefix='sc-')
    try:
        out = subprocess.run([chrome, '--headless=new', '--no-first-run', '--no-default-browser-check', '--user-data-dir=' + prof,
                              '--dump-dom', url], capture_output=True, text=True, timeout=60).stdout
    finally:
        shutil.rmtree(prof, ignore_errors=True)
    m = re.search(r'<pre id="out">(.*?)</pre>', out, flags=re.S)
    if not m or not m.group(1):
        raise SystemExit('no answers:\n' + out[:2000])
    return json.loads(html.unescape(m.group(1)))


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
    print(len(answers[0]), 'answers')


if __name__ == '__main__':
    main()
