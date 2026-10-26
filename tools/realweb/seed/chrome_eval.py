#!/usr/bin/env python3
"""Re-measure a fixture's expected value in STOCK Chrome (not automation-flagged: launched by hand with a
real --remote-debugging-port, attached over CDP only to read). usage: chrome_eval.py URL EXPR [WAIT_MS]
EXPR is evaluated after load + WAIT_MS (default 2000); prints the result as one line. Run under xvfb-run
for a headed oracle, or pass CHROME_HEADLESS=1."""
import json, os, shutil, signal, socket, subprocess, sys, tempfile, time
from playwright.sync_api import sync_playwright
url, expr = sys.argv[1], sys.argv[2]; wait = int(sys.argv[3]) if len(sys.argv) > 3 else 2000
s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close(); prof = tempfile.mkdtemp(prefix="rwchr-")
args = [os.environ.get("CHROME_BIN", "/usr/bin/google-chrome")] + os.environ.get("CHROME_EXTRA", "").split() + [ f"--remote-debugging-port={port}", f"--user-data-dir={prof}", "--no-first-run", "--no-default-browser-check", "--window-size=1920,1080", "about:blank"]
if os.environ.get("CHROME_HEADLESS"): args.insert(1, "--headless=new")
# Headed means the X display (xvfb-run's): under a Wayland login Chrome otherwise opens on the desktop.
else: args.insert(1, "--ozone-platform=x11")
p = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
try:
    for _ in range(150):
        try: socket.create_connection(("127.0.0.1", port), 0.2).close(); break
        except OSError: time.sleep(0.1)
    with sync_playwright() as pw:
        b = pw.chromium.connect_over_cdp(f"http://127.0.0.1:{port}"); pg = b.contexts[0].pages[0]
        pg.goto(url, wait_until="load"); pg.wait_for_timeout(wait)
        v = pg.evaluate(expr); print(v if isinstance(v, str) else json.dumps(v, ensure_ascii=False))
finally:
    os.killpg(p.pid, signal.SIGKILL); p.wait(); shutil.rmtree(prof, ignore_errors=True)
