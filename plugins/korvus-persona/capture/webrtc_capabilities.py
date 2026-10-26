#!/usr/bin/env python3
"""Measure each installed browser's `RTCRtpReceiver.getCapabilities` / `RTCRtpSender.getCapabilities` for audio and
video into its bundle's `webrtc-capabilities.json`.

usage: plugins/korvus-persona/capture/webrtc_capabilities.py [bundle-id ...]   (default: every bundle with a binary)

Which codecs a list holds is the BUILD's and the DEVICE's -- libwebrtc's encoder and decoder factories, hardware
codecs included -- like the offer SDP beside it (`webrtc-offer.json`, measured the same way, 2026-10-07): the
receiver's video list adds decode-only codecs (H.264 High, H.265 on Chrome 150) the offer never lists, and the
sender's is a different subset in a different order, so neither list can be derived from the offer. Each browser
runs headed under its own Xvfb with a fresh profile, the page served from loopback; the answers are written as
measured, one codec or header extension per line (tools/parity/tests/test_bundle_json_is_reviewable.py caps a
line's length).
"""
import http.server
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import threading

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import emit  # noqa: E402  -- the one formatter, shared with `emit.py --role webrtc-capabilities`

REPO = pathlib.Path(__file__).resolve().parents[3]
BUNDLES = REPO / "plugins/korvus-persona/bundles"
BINARIES = {k: v for k, v in {"chrome": "/usr/bin/google-chrome", "brave": "/usr/bin/brave-browser",
                              "edge": "/usr/bin/microsoft-edge", "opera": "/usr/bin/opera"}.items() if os.path.exists(v)}
PAGE = b"""<!doctype html><script>
const out = {};
for (const side of ['RTCRtpReceiver', 'RTCRtpSender'])
  for (const kind of ['audio', 'video']) out[side + '.' + kind] = window[side].getCapabilities(kind);
fetch('/result', {method: 'POST', body: JSON.stringify(out)});
</script>"""


def browser_answer(binary, page):
    """The JSON object `page` POSTs to /result when `binary` loads it, headed under its own Xvfb with a fresh
    profile ({} when it never answers)."""
    got = {}
    done = threading.Event()

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            self.send_response(200)
            self.send_header("content-type", "text/html")
            self.send_header("content-length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)

        def do_POST(self):
            got.update(json.loads(self.rfile.read(int(self.headers["content-length"]))))
            self.send_response(204)
            self.end_headers()
            done.set()

    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    prof = tempfile.mkdtemp(prefix="caps-")
    try:
        p = subprocess.Popen(["xvfb-run", "-a", "-s", "-screen 0 1920x1080x24", binary, "--ozone-platform=x11",
                              f"--user-data-dir={prof}", "--no-first-run", "--no-default-browser-check",
                              f"http://127.0.0.1:{port}/"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
        done.wait(30)
        os.killpg(p.pid, 15)
        p.wait(10)
    finally:
        srv.shutdown()
        shutil.rmtree(prof, ignore_errors=True)
    return got


def browser_version(product, binary):
    """`--version` as the browser prints it, with the product's name when it prints a bare number (Opera)."""
    version = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    return f"{product.capitalize()} {version}" if version[:1].isdigit() else version


def main():
    ids = sys.argv[1:] or sorted(d.name for d in BUNDLES.iterdir() if d.is_dir())
    for bid in ids:
        product = bid.split("-")[0]
        binary = BINARIES.get(product)
        if not binary:
            print(f"{bid}: no installed {product}")
            continue
        version = browser_version(product, binary)
        got = browser_answer(binary, PAGE)
        if len(got) != 4:
            print(f"{bid}: no answer from {version}")
            continue
        out = BUNDLES / bid / "webrtc-capabilities.json"
        out.write_text(emit.format_webrtc_capabilities(got, f"measured on the installed {version}, headed under Xvfb "
                       "(plugins/korvus-persona/capture/webrtc_capabilities.py) -- not this bundle's capture session"))
        print(f"{bid}: {version} -> {out.relative_to(REPO)} "
              + " ".join(f"{k}={len(v['codecs'])}/{len(v['headerExtensions'])}" for k, v in got.items()))


if __name__ == "__main__":
    main()
