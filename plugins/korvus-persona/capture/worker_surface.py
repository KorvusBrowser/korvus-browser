#!/usr/bin/env python3
"""Re-capture ONE artifact -- `worker.surface.json` -- from a real browser, over CDP.

    python3 plugins/korvus-persona/capture/worker_surface.py --bundle brave-151-linux-20260907T1006
    python3 plugins/korvus-persona/capture/worker_surface.py --bundle ... --write

WHY THIS FILE EXISTS. `surface-dump.js` records `ctorIndex` -- where `constructor` sits among a
prototype's own keys -- and an earlier change re-read the WINDOW surface of all four
bundles to bank it. The worker artifact was not re-read, so every `worker.surface.json` in this tree
describes the same browsers with the field missing: 294/294 interfaces, four bundles. The engine
could therefore seat `constructor` from the capture in the window realm and not in the worker one,
and bridged the gap by BORROWING the window's index for a worker interface whose member list is the
window's exactly (R22). A borrow is an inference. This tool replaces it with a measurement.

WHY IT IS A TOOL AND NOT A SESSION. L1.25 did its re-read by hand and committed only the artifacts,
so the next unit that needs one starts from nothing -- which is this unit. A capture that cannot be
re-run is a number nobody can check.

WHAT IT REPRODUCES, EXACTLY. `collect.js` takes the worker surface by building a Blob worker whose
whole body is `self.postMessage(<surface-dump.js>)` and JSON-parsing what comes back, from a page on
`localhost` (a potentially-trustworthy origin, so `crypto.subtle`, `Cache` and the rest of the
`[SecureContext]` surface are present -- the worker baseline is wrong without it). The page this
serves does the same thing with the same file, because a capture taken under different conditions is
a different capture, not a newer one.

WHAT IT REFUSES TO DO. It never edits the artifact it is comparing against. `--write` replaces the
file with what the browser returned, whole, after printing every field that moved; without `--write`
it prints the comparison and touches nothing. The browser's output is the artifact -- a hand-merged
field would make the file evidence of nothing.

BRAVE IS CAPTURED WITH FINGERPRINTING OFF, AND THAT IS NOT A PREFERENCE. Measured here on the first
run: shields-on Brave answered this worker `navigator.hardwareConcurrency = 10` where the bundle's
window artifact and `values.json` record the device's 12, and `navigator.languages = ["ko-KR"]` where
they record the four-entry list. `farbling.json` says what those two are — `hardwareConcurrency` is
**uniform over [2, N]**, 11 distinct values in 152 sampled cells, so a single reading of it is one
session's dice roll; `languages` is a REDUCTION applied at the JS binding, which `values.json` records
as a policy and deliberately does not apply. Writing either into a surface artifact would bank a
sample as a device fact (R19) and would make one bundle's two realms disagree about a device that has
no such disagreement (R22). So the flag is the CONTROL COLUMN `farbling.py` already uses, the reading
is the device's, and `--farbling default` is there for whoever wants to measure the other column.

BROWSER HYGIENE is `farbling.Session`'s, imported rather than repeated: a real `--remote-debugging-port`
we picked (`=0` sets `navigator.webdriver`, measured by L18.1), a throwaway profile, reaped by the PID
we started and never by a name pattern, and a profile removal that loops until the directory is GONE
(a single `rmtree(..., ignore_errors=True)` reports success while a surviving renderer rewrites the
tree; that cost 8 leaked directories out of ~120 sessions before it was measured).
"""
import argparse
import http.server
import json
import os
import pathlib
import socket
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from farbling import ROOT, Session, _free_port  # browser hygiene, one derivation
from host import SHARED  # the shared dumper's path, one derivation

BUNDLES = ROOT / "plugins/korvus-persona/bundles"

# The browser each bundle was captured from. The binary is the real ELF in every case: Opera and
# Brave ship a wrapper script whose `--version` and whose process tree are not the browser's, and a
# capture must come from the thing that renders.
BINARIES = {
    "brave": "/opt/brave.com/brave/brave",
    "chrome": "/opt/google/chrome/chrome",
    "edge": "/opt/microsoft/msedge/msedge",
    "opera": "/usr/lib/x86_64-linux-gnu/opera-stable/opera",
}

PAGE = """<!doctype html><html><head><meta charset="utf-8"><title>worker surface</title></head>
<body><pre id="log">starting</pre><script>
// The worker surface, taken the way `collect.js` takes it: ONE Blob worker, whose entire body is
// `self.postMessage(<the shared dumper>)`. The dumper evaluates to a JSON string, so nothing here
// parses or reshapes it -- the bytes the browser produced are the bytes that get POSTed back.
(async () => {
  const say = (m) => { document.getElementById("log").textContent = m; };
  const post = (path, body) => fetch(path, { method: "POST", body });
  try {
    const src = await (await fetch("/shared/surface-dump.js", { cache: "no-store" })).text();
    say("dumper " + src.length + " bytes; starting worker");
    const url = URL.createObjectURL(new Blob(["self.postMessage(\\n" + src + "\\n);"],
                                             { type: "text/javascript" }));
    const json = await new Promise((res, rej) => {
      const w = new Worker(url);
      w.onmessage = (e) => { res(e.data); w.terminate(); URL.revokeObjectURL(url); };
      w.onerror = (e) => { rej(new Error("worker: " + ((e && e.message) || e))); URL.revokeObjectURL(url); };
      setTimeout(() => rej(new Error("worker did not answer in 20s")), 20000);
    });
    say("worker answered " + json.length + " bytes");
    // The conditions the reading is contingent on travel with it, in a SEPARATE post: a capture that
    // does not state them is a capture that will be misread.
    await post("/conditions", JSON.stringify({
      href: location.href,
      isSecureContext: !!self.isSecureContext,
      ua: navigator.userAgent,
      webdriver: navigator.webdriver,
      capturedAt: new Date().toISOString(),
    }));
    await post("/worker-surface", json);
    say("posted");
  } catch (e) {
    await post("/failed", String((e && e.message) || e));
    say("failed: " + e);
  }
})();
</script></body></html>
"""


class _Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _send(self, code, body, ctype="text/plain; charset=utf-8"):
        raw = body.encode("utf-8") if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(raw)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        if self.path in ("/", "/index.html"):
            return self._send(200, PAGE, "text/html; charset=utf-8")
        if self.path == "/shared/surface-dump.js":
            with open(os.path.join(ROOT, SHARED["surface-dump.js"]), encoding="utf-8") as f:
                return self._send(200, f.read(), "text/javascript; charset=utf-8")
        return self._send(404, "not found")

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = b""
        while len(raw) < n:
            chunk = self.rfile.read(min(1 << 20, n - len(raw)))
            if not chunk:
                break
            raw += chunk
        self.server.got[self.path.lstrip("/")] = raw.decode("utf-8", "replace")
        return self._send(200, "ok")


def capture(binary, display=None, window=(1200, 800), timeout=90.0, farbling="off"):
    """Launch the browser, open the page, and return `(surface dict, conditions dict)`.

    `farbling="off"` disables Brave's fingerprinting protection for the reading (the docstring above
    says why it is the default) and is a no-op for a browser that has none, which is why the flag is
    only ever passed to Brave rather than to whatever binary happens to be named."""
    port = _free_port()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), _Handler)
    srv.daemon_threads = True
    srv.got = {}
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    # `localhost`, not `127.0.0.1`: it is the origin `host.py` serves the one-visit capture from, and
    # the stored artifact is that origin's reading.
    url = "http://localhost:%d/" % port
    ses = None
    try:
        ses = Session(binary, window, display=display,
                      farbled=not ("brave" in binary and farbling == "off"))
        ses.open_tab(url)
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if "failed" in srv.got:
                raise RuntimeError("the page reported: " + srv.got["failed"][:400])
            if "worker-surface" in srv.got:
                return (json.loads(srv.got["worker-surface"]),
                        json.loads(srv.got.get("conditions") or "{}"))
            time.sleep(0.1)
        raise TimeoutError("no worker surface arrived in %.0fs" % timeout)
    finally:
        if ses is not None:
            # `Session.close` is true only when BOTH the PID is gone from `/proc` and the profile
            # directory is gone from the disk, after its own retry loop. A false here is a leak and
            # is printed rather than swallowed.
            if not ses.close():
                print("  WARNING: the browser's PID or its profile directory survived close()")
        srv.shutdown()
        srv.server_close()


def compare(stored, live):
    """Every way the two readings differ, as lines. `ctorIndex` is reported apart from the rest.

    The point of the split: this re-capture is supposed to ADD a field and change nothing else, and
    "changed nothing else" is a claim that has to be checked rather than asserted. Anything in the
    second list is a real drift between the stored capture and this browser, and it is printed --
    never quietly merged away."""
    added, moved = [], []
    if stored.get("meta") != live.get("meta"):
        moved.append("meta: %s -> %s" % (stored.get("meta"), live.get("meta")))
    for key in ("globals", "documentKeys"):
        a, b = stored.get(key) or [], live.get(key) or []
        if a != b:
            only_a = [x for x in a if x not in set(b)]
            only_b = [x for x in b if x not in set(a)]
            moved.append("%s: %d -> %d (gone: %s; new: %s; order %s)"
                         % (key, len(a), len(b), only_a[:8], only_b[:8],
                            "same" if [x for x in a if x in set(b)] == [x for x in b if x in set(a)] else "CHANGED"))
    if stored.get("fpToStringNative") != live.get("fpToStringNative"):
        moved.append("fpToStringNative: %s -> %s" % (stored.get("fpToStringNative"), live.get("fpToStringNative")))
    si, li = stored.get("interfaces") or {}, live.get("interfaces") or {}
    for name in sorted(set(si) | set(li)):
        if name not in li:
            moved.append("%s: gone from the live reading" % name)
            continue
        if name not in si:
            moved.append("%s: new in the live reading" % name)
            continue
        a, b = si[name], li[name]
        for field in sorted(set(a) | set(b)):
            if field == "ctorIndex" and field not in a:
                added.append("%s: ctorIndex %s" % (name, b.get(field)))
                continue
            if a.get(field) != b.get(field):
                if field in ("members", "statics"):
                    an = [m.get("name") for m in a.get(field) or []]
                    bn = [m.get("name") for m in b.get(field) or []]
                    if an != bn:
                        moved.append("%s.%s: %s -> %s" % (name, field, an, bn))
                    else:
                        moved.append("%s.%s: same %d names, a descriptor differs" % (name, field, len(an)))
                else:
                    moved.append("%s.%s: %r -> %r" % (name, field, a.get(field), b.get(field)))
    for key in ("singletons", "instances"):
        a, b = stored.get(key) or {}, live.get(key) or {}
        if json.dumps(a, sort_keys=True) != json.dumps(b, sort_keys=True):
            moved.append("%s: differs (%d vs %d entries)" % (key, len(a), len(b)))
    return added, moved


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bundle", required=True, help="the bundle directory's capture id")
    ap.add_argument("--binary", help="override the browser binary this bundle's name implies")
    ap.add_argument("--display", help="X display to launch on (an Xvfb, say)")
    ap.add_argument("--farbling", choices=("off", "default"), default="off",
                    help="Brave only: 'off' reads the DEVICE (the default, see the module docstring), "
                         "'default' reads one session's farbled draw")
    ap.add_argument("--write", action="store_true", help="replace the bundle's worker.surface.json")
    ap.add_argument("--raw-out", help="also write the browser's JSON here, unformatted")
    a = ap.parse_args()

    bundle = BUNDLES / a.bundle
    dest = bundle / "worker.surface.json"
    if not dest.exists():
        raise SystemExit("no such artifact: %s" % dest)
    binary = a.binary or BINARIES.get(a.bundle.split("-", 1)[0])
    if not binary or not os.path.exists(binary):
        raise SystemExit("no binary for bundle %s (looked at %r)" % (a.bundle, binary))

    stored = json.loads(dest.read_text(encoding="utf-8"))
    print("%s\n  binary   %s\n  stored   %d interfaces, %d with ctorIndex"
          % (a.bundle, binary, len(stored.get("interfaces") or {}),
             sum(1 for v in (stored.get("interfaces") or {}).values() if "ctorIndex" in v)))
    live, cond = capture(binary, display=a.display, farbling=a.farbling)
    print("  live     %d interfaces, %d with ctorIndex"
          % (len(live.get("interfaces") or {}),
             sum(1 for v in (live.get("interfaces") or {}).values() if "ctorIndex" in v)))
    cond["farbling"] = a.farbling if "brave" in binary else "n/a"
    print("  under    %s" % json.dumps(cond))

    added, moved = compare(stored, live)
    print("  ADDED    %d interfaces gain ctorIndex" % len(added))
    print("  MOVED    %d other differences" % len(moved))
    for line in moved[:60]:
        print("    ! " + line)
    if len(moved) > 60:
        print("    ! ... and %d more" % (len(moved) - 60))

    if a.raw_out:
        pathlib.Path(a.raw_out).write_text(json.dumps(live), encoding="utf-8")
    if a.write:
        # `emit.py`'s `emit_worker_surface` formatting, because this writes the same artifact it does
        # and a file that round-trips differently is a file whose next diff is unreadable.
        dest.write_text(json.dumps(live, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
        print("  wrote    %s" % dest)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
