#!/usr/bin/env python3
"""fonts_capture.py OUT [--display :0] [--chrome /usr/bin/google-chrome] [--host-families] [--x11] [--timeout S]

The persona browser's font capture (`fonts-probe.html`) with NO DevTools attached: serve the
probe page on a loopback port with `?report`, open it in the persona's own browser -- headed, a fresh profile, on
the display the persona runs on (the owner's desktop session is `:0`, Wayland: Chrome's `system-ui` is the desktop's
interface font only there) -- and write the JSON the page POSTs to /report to OUT. That file is the capture
`emit.py --font-capture OUT --bundle <id>` turns into the bundle's `fonts/metrics.json` and `font_table.rs`.

A CDP attach (tools/realweb/seed/chrome_eval.py) changes some answers of Chrome's (memory: chrome-devtools-changes-
answers); a POST changes none. `--host-families` also serves this machine's `fc-list : family` as `families.json`,
which the page adds to its candidate names. `--x11` passes `--ozone-platform=x11` (an Xvfb display under a Wayland
login). Chrome is killed by its process group; its throwaway profile is removed.
"""
import argparse
import http.server
import json
import os
import pathlib
import shutil
import signal
import socket
import socketserver
import subprocess
import tempfile
import threading
import time

HERE = pathlib.Path(__file__).resolve().parent


def host_families():
    """Every family name fontconfig lists here, the localized ones too, sorted."""
    out = subprocess.run(["fc-list", ":", "family"], capture_output=True, text=True, check=True).stdout
    names = set()
    for line in out.splitlines():
        for name in line.split(","):
            name = name.strip()
            if name:
                names.add(name.replace("\\-", "-"))
    return sorted(names)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--display", default=os.environ.get("DISPLAY", ":0"))
    ap.add_argument("--chrome", default=os.environ.get("CHROME_BIN", "/usr/bin/google-chrome"))
    ap.add_argument("--host-families", action="store_true")
    ap.add_argument("--x11", action="store_true")
    ap.add_argument("--timeout", type=float, default=600.0)
    ap.add_argument("--verbose", action="store_true", help="print each request the page makes")
    ap.add_argument("--flag", action="append", default=[], help="an extra browser command-line flag (repeatable)")
    a = ap.parse_args()

    page = (HERE / "fonts-probe.html").read_bytes()
    families = json.dumps(host_families() if a.host_families else []).encode()
    got = {}
    done = threading.Event()

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            if a.verbose:
                print("fonts_capture: " + (fmt % args), flush=True)

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            if path in ("/", "/fonts-probe.html"):
                body, ty, code = page, "text/html; charset=utf-8", 200
            elif path == "/families.json" and a.host_families:
                body, ty, code = families, "application/json", 200
            else:
                body, ty, code = b"", "text/plain", 404
            self.send_response(code)
            self.send_header("Content-Type", ty)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            n = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(n)
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()
            if self.path.startswith("/report-error"):
                got["error"] = body.decode("utf-8", "replace")
                done.set()
            elif self.path.startswith("/report"):
                got["body"] = body
                done.set()

    class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True
        allow_reuse_address = True

    srv = S(("127.0.0.1", 0), H)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    prof = tempfile.mkdtemp(prefix="fonts-capture-")
    args = [a.chrome, "--user-data-dir=" + prof, "--no-first-run", "--no-default-browser-check", "--window-size=1200,900"]
    if a.x11:
        args.append("--ozone-platform=x11")
    args += a.flag
    args.append("http://127.0.0.1:%d/fonts-probe.html?report" % port)
    env = dict(os.environ, DISPLAY=a.display)
    started = time.strftime("%Y-%m-%d %H:%M:%S")
    p = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        if not done.wait(a.timeout):
            raise SystemExit("fonts_capture: no report within %.0f s" % a.timeout)
    finally:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        p.wait()
        srv.shutdown()
        shutil.rmtree(prof, ignore_errors=True)
    if "error" in got:
        raise SystemExit("fonts_capture: the page failed: %s" % got["error"])
    cap = json.loads(got["body"])
    cap["captureTool"] = {"tool": "plugins/korvus-persona/capture/fonts_capture.py", "display": a.display, "browser": a.chrome,
                          "hostFamilies": a.host_families, "x11": a.x11, "started": started}
    pathlib.Path(a.out).write_text(json.dumps(cap, ensure_ascii=False), encoding="utf-8")
    print("fonts_capture: %d candidates, %d present -> %s" % (len(cap.get("candidates", [])), len(cap.get("present", [])), a.out))


if __name__ == "__main__":
    main()
