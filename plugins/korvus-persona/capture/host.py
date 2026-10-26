#!/usr/bin/env python3
"""Serve the one-visit reference capture, and write back whatever the browser posts.

    python3 plugins/korvus-persona/capture/host.py            # a fresh origin: random port, printed
    python3 plugins/korvus-persona/capture/host.py 9000       # pin a port (the origin's state persists)

WHY A SERVER AND NOT A CONSOLE PASTE. Every recording this project is built on was taken by pasting a
script into DevTools, and DevTools is not a neutral observer: it injects `$`, `$$`, `$x`, `inspect`,
`getEventListeners` and twenty more onto the global. The committed baseline carries all of them, plus
the Discourse page it was taken on -- `jQuery`, `Discourse`, `EmberENV`, an `ItsATrap` class that
Korvus then shipped as a native interface on every page, and Ember's extensions to `Array.prototype`
recorded as MEMBERS of Array. Three separate filters exist in this repo to undo that. A page the user
simply opens needs none of them.

It is `localhost` on purpose: Chromium treats it as a potentially-trustworthy origin, so
`isSecureContext` is true and the `[SecureContext]` APIs -- `crypto.subtle`, `Cache`, `CacheStorage`
-- are present. The worker baseline is wrong without that, which is why its old instructions said to
run it on https://example.com.

The browser POSTs the JSON here rather than offering a download, because the download would land in
whatever the user's browser calls Downloads and this needs to land in `reference/captures/`.
"""
import json
import re
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

def _repo_root():
    """Walk up to the workspace root instead of counting `..` hops.

    Every tool here computed its root as a fixed number of parents, and every one of them broke the
    moment the file moved -- twice in one day: the webapi grader's persona path silently resolved to
    a directory that did not exist, and these six were three levels out after moving into the persona
    crate. A hop count is a fact about where a file sits, which is exactly the thing that changes."""
    import pathlib as _p
    d = _p.Path(__file__).resolve().parent
    for cand in [d, *d.parents]:
        m = cand / "Cargo.toml"
        if m.exists() and "[workspace]" in m.read_text(encoding="utf-8", errors="ignore"):
            return cand
    raise SystemExit("cannot find the workspace root above " + str(d))


ROOT = _repo_root()
OUT_DIR = os.path.join(ROOT, "reference", "captures")

# Served verbatim, and that is the point: `surface-dump.js` and `behavior-run.js` are the same files
# Korvus evaluates in its own isolate, so the two sides stay diffable. A copy here would drift.
SHARED = {
    "surface-dump.js": "core/korvus-webapi/tests/fixtures/surface-dump.js",
    "behavior-run.js": "core/korvus-webapi/tests/fixtures/behavior-run.js",
    "capture-render-fingerprint.console.js": "core/korvus-runtime/tests/fixtures/capture-render-fingerprint.console.js",
}
sys.path.insert(0, os.path.join(ROOT, "tools/sync"))
from bundle import artifact as _bundle_artifact  # the shipped bundle's manifest is the index (I6)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import promote  # `browser_of` / `product_major`: ONE derivation of who this capture is (L7f)
CASES_DIR = _bundle_artifact("oracle")

PAGE = """<!doctype html>
<meta charset="utf-8">
<title>Korvus reference capture</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
         max-width: 46rem; margin: 3rem auto; padding: 0 1.5rem; }
  h1 { font-size: 1.35rem; margin: 0 0 .25rem; letter-spacing: -.01em; }
  .sub { opacity: .65; margin: 0 0 1.75rem; }
  ol { padding-left: 1.2rem; } li { margin: .45rem 0; }
  button { font: inherit; font-weight: 600; padding: .7rem 1.5rem; border-radius: 6px;
           border: 1px solid currentColor; background: transparent; cursor: pointer; }
  button[disabled] { opacity: .45; cursor: default; }
  #log { font: 12.5px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre-wrap;
         border: 1px solid rgba(128,128,128,.35); border-radius: 6px; padding: .8rem 1rem;
         margin-top: 1.25rem; max-height: 22rem; overflow: auto; }
  .ok { color: #2c7a4b; font-weight: 600; } .bad { color: #b3261e; font-weight: 600; }
  code { font: 12.5px ui-monospace, monospace; background: rgba(128,128,128,.14); padding: .1em .35em; border-radius: 3px; }
  .note { border-left: 3px solid rgba(128,128,128,.5); padding: .1rem 0 .1rem .9rem; margin: 1.1rem 0; opacity: .9; }
</style>
<h1>Korvus reference capture</h1>
<p class="sub">One visit, every reference artifact. Nothing is uploaded anywhere &mdash; it is written to
<code>reference/captures/</code> on this machine.</p>

<div class="note">
  <b>Before you press it:</b> open Brave Shields (the lion in the address bar) &rarr; Fingerprinting &rarr;
  <b>Allow all fingerprinting</b> for this page, then reload. That turns farbling off, and farbling is
  the reason a recording cannot otherwise be trusted: Brave re-rolls a seed each session and shuffles
  exactly the surfaces this is recording. The page detects the setting either way and writes it down.
</div>

<ol>
  <li>Shields down (above), reload.</li>
  <li>Press <b>Capture</b>. It takes about 20&ndash;40 seconds and never opens a dialog.</li>
  <li>Leave the tab in the foreground &mdash; a backgrounded tab throttles timers and the timing
      measurements come out wrong.</li>
</ol>

<p><button id="go">Capture</button></p>
<div id="log">ready</div>

<script>
// The collector's corpus frame loads "/" too, but the host answers a frame navigation with a bare
// document (see do_GET), so this script never runs there. The guard stays as a second wall: a copy
// of this page starting a capture inside a frame would be a disaster.
if (window.top !== window) { document.body.innerHTML = ""; }
else {
const logEl = document.getElementById("log");
const lines = [];
globalThis.__capLog = (m) => { lines.push("  " + m); logEl.textContent = lines.join("\\n"); logEl.scrollTop = 1e9; };
const say = (m, cls) => { lines.push(cls ? `<${cls}>` + m : m); logEl.textContent = lines.join("\\n"); logEl.scrollTop = 1e9; };
document.getElementById("go").addEventListener("click", async (ev) => {
  const btn = ev.currentTarget;
  btn.disabled = true;
  lines.length = 0;
  say("starting");
  if (!isSecureContext) say("WARNING: not a secure context -- the worker surface will be missing crypto.subtle");
  try {
    const src = await (await fetch("/shared/collect.js", { cache: "no-store" })).text();
    const result = await (0, eval)(src);
    // Held on the window so the catch below can still write it out if the POST cannot be made.
    window.__korvusResult = result;
    say("posting " + (JSON.stringify(result).length / 1048576).toFixed(1) + " MB back to the host");
    const r = await fetch("/save", { method: "POST", headers: { "content-type": "application/json" },
                                     body: JSON.stringify(result) });
    const t = await r.text();
    logEl.innerHTML = lines.map((l) => l.replace(/[<>&]/g, (c) => ({ "<": "&lt;", ">": "&gt;", "&": "&amp;" }[c]))).join("\\n")
      + '\\n\\n<span class="' + (r.ok ? "ok" : "bad") + '">' + (r.ok ? "SAVED  " : "FAILED  ") + t + "</span>";
  } catch (e) {
    // THE POST CAN FAIL AND THE CAPTURE MUST SURVIVE IT. Two minutes of work and a 6.9 MB result
    // were lost exactly once: the host was restarted while this page was open, so the socket it
    // posts to no longer existed and the browser answered "TypeError: Failed to fetch". There was
    // no fallback, so the result went with it.
    //
    // The POST is still preferred -- it lands the file in reference/captures/ rather than in
    // whatever this browser calls Downloads, which is the whole reason the host serves this page.
    // But a download the operator has to move is incomparably better than a capture that no longer
    // exists, so a failed POST offers one rather than reporting and discarding.
    say("FAILED to post: " + ((e && e.message) || e));
    try {
      const blob = new Blob([JSON.stringify(window.__korvusResult || null)], { type: "application/json" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = "korvus-capture-" + new Date().toISOString().replace(/[:.]/g, "").slice(0, 15) + ".json";
      document.body.appendChild(a);
      a.click();
      a.remove();
      say("the host is not answering, so the capture was DOWNLOADED instead.");
      say("move it into reference/captures/ and it is exactly the same file the POST would have written.");
    } catch (e2) {
      say("and the download fallback also failed: " + ((e2 && e2.message) || e2));
    }
    logEl.innerHTML = logEl.textContent.replace(/FAILED[^\\n]*/, (m) => '<span class="bad">' + m + "</span>");
  }
  btn.disabled = false;
});
// `?auto=1` starts the capture without a click: tools/version/next.py opens this page in a browser no one sits at.
// The click it stands in for is synthetic, so the page holds no user activation -- which only makes the
// dialog-capable expressions (DIALOG_CAPABLE below) less able to open anything.
if (new URLSearchParams(location.search).has("auto")) document.getElementById("go").click();
}
</script>
"""


# Expressions that can open a DIALOG, and are therefore not askable during an unattended capture.
#
# The collector's own header states the rule -- "WHAT IT DELIBERATELY DOES NOT DO: prompt. No alert,
# no getUserMedia, no queryLocalFonts, no Notification.requestPermission, no navigation. A capture
# that blocks on a dialog is a capture nobody finishes, and two earlier oracle runs died exactly that
# way." That rule was applied to the collector's OWN probes and never to the 14,564-expression corpus
# it evaluates, which is harvested from WebIDL and contains every one of them.
#
# Measured: a capture reached `pass 1: 7000/14564` and stopped. Indices 7316/7369/7398 are
# `navigator.share()` and 7371/7399/7400 are `navigator.requestMIDIAccess()` -- and the capture runs
# from a button click, so the page holds a transient user activation and the browser honours them.
# One opens the OS share sheet, the other a permission bubble whose promise simply never settles.
#
# 42 of 14,564 expressions match (0.29%). Dropping them is the difference between a capture that
# finishes and one that does not; the bundle records the dimension as not re-recorded.
DIALOG_CAPABLE = re.compile(
    r"\.share\(|requestMIDIAccess|getUserMedia|getDisplayMedia|requestPermission|requestDevice|"
    r"requestPort|showOpenFilePicker|showSaveFilePicker|showDirectoryPicker|queryLocalFonts|"
    r"requestFullscreen|webkitRequestFullscreen|webkitRequestFullScreen|requestPictureInPicture|"
    r"Notification\.requestPermission|wakeLock\.request|credentials\.(get|create)|requestAdapter\(|"
    r"\.requestPresent\(",
    re.I,
)

SKIPPED_DIALOG_CASES = []


def behavior_cases():
    """The recorded corpus, flattened to {iface, member, expr, await?} for re-answering in one pass.

    Only expressions that already carry a `source` are sent: those are the ones an oracle has
    answered before, so a fresh answer is directly comparable. A skeleton with no answer yet has
    nothing to compare against and belongs to the `gencases` path, not here.

    `await` rides along because it is part of the QUESTION, not decoration on it: the recorded answer
    for a flagged case is what the promise SETTLED to, and a re-answer taken without the flag would
    compare that against `"[object Promise]"` and report a difference in every one of them.
    """
    out = []
    if not os.path.isdir(CASES_DIR):
        return out
    for name in sorted(os.listdir(CASES_DIR)):
        if not name.endswith(".json"):
            continue
        try:
            with open(os.path.join(CASES_DIR, name), encoding="utf-8") as f:
                for c in json.load(f):
                    if c.get("source"):
                        if DIALOG_CAPABLE.search(c["expr"]):
                            SKIPPED_DIALOG_CASES.append(
                                "%s.%s" % (c.get("iface") or name[:-5], c["member"]))
                            continue
                        one = {"iface": c.get("iface") or name[:-5], "member": c["member"], "expr": c["expr"]}
                        if c.get("await") is True:
                            one["await"] = True
                        out.append(one)
        except Exception:
            pass
    return out


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, code, body, ctype="text/plain; charset=utf-8"):
        raw = body if isinstance(body, bytes) else body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(raw)))
        # No caching anywhere: a capture must never read a stale copy of a script that has been edited.
        self.send_header("Cache-Control", "no-store")
        # Anything that is not a real asset gets its socket torn down. The behavior corpus contains
        # `new EventSource("x")`, `new WebSocket("x")` and `sendBeacon("x")`, which resolve against the
        # page and knock on this server; only one of the seven EventSources is ever closed by its case.
        # Answered keep-alive they sit in Chrome's connection pool, and since Chrome allows six
        # connections per host, the pool is exhausted a third of the way through the corpus and every
        # later request -- including the POST that saves the capture -- queues forever. Closing here
        # costs nothing (these responses carry no content worth reusing a socket for) and is the whole
        # difference between a capture that finishes and one that hangs at 40%.
        if code != 200:
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/index.html"):
            # THE CORPUS FRAME GETS A BARE DOCUMENT AT THE SAME URL. collect.js runs the behavior
            # corpus in an iframe loaded from "/" so that location, origin and referrer read as the
            # tab's do -- and until now that frame received THIS page, whose one <style> made
            # `document.styleSheets.length` answer 1. The earlier captures answered 0 only because a
            # `location.pathname = "x"` case had moved the frame onto a text/plain 404 by then; both
            # numbers were facts about the rig. Chrome marks a frame navigation with
            # `Sec-Fetch-Dest: iframe`, so the same URL can hand the frame a document with nothing in
            # it: a doctype (the corpus asks `document.doctype.name`), an empty head, an empty body.
            # What the corpus then records is the answer for a fresh document, which is what the
            # engine is asked for.
            if self.headers.get("Sec-Fetch-Dest") == "iframe":
                return self._send(200, "<!doctype html><html><head></head><body></body></html>",
                                  "text/html; charset=utf-8")
            return self._send(200, PAGE, "text/html; charset=utf-8")
        if path == "/shared/collect.js":
            with open(os.path.join(ROOT, "plugins/korvus-persona/capture/collect.js"), encoding="utf-8") as f:
                return self._send(200, f.read(), "text/javascript; charset=utf-8")
        if path == "/shared/behavior-cases.json":
            return self._send(200, json.dumps(behavior_cases()), "application/json")
        if path == "/shared/request-headers.json":
            # THE ONLY THING THE PAGE CANNOT SEE ABOUT ITSELF. `Accept-Language` is a persona value
            # like `navigator.languages`, but it exists only on the wire: no JS reads its own request
            # headers, and the two are not the same function in every browser (Brave reduces the list
            # and farbles the q-value; Chrome, Edge and Opera run the full list through Chromium's
            # q-ladder). So the SERVER reports what it received and `collect.js` records it as a
            # captured value -- read off the browser's own request, not derived from a rule.
            #
            # Everything, lower-cased, rather than one name: a later gap in this same class costs
            # another capture session, and a header block is a few hundred bytes.
            seen = {}
            for k, v in self.headers.items():
                seen.setdefault(k.lower(), v)
            return self._send(200, json.dumps(seen), "application/json")
        name = path.rsplit("/", 1)[-1]
        if path.startswith("/shared/") and name in SHARED:
            with open(os.path.join(ROOT, SHARED[name]), encoding="utf-8") as f:
                return self._send(200, f.read(), "text/javascript; charset=utf-8")
        return self._send(404, "not found")

    def do_POST(self):
        if self.path != "/save":
            return self._send(404, "not found")
        n = int(self.headers.get("Content-Length") or 0)
        raw = b""
        while len(raw) < n:
            chunk = self.rfile.read(min(1 << 20, n - len(raw)))
            if not chunk:
                break
            raw += chunk
        try:
            doc = json.loads(raw.decode("utf-8"))
        except Exception as e:
            return self._send(400, "not JSON: %s" % e)
        cond = doc.get("conditions") or {}
        # The filename states the two things that decide how the file may be read: which browser it
        # came from, and whether farbling was on. A capture whose provenance is only inside it is a
        # capture that gets mixed up with the next one.
        #
        # THE DERIVATION IS PROMOTE.PY'S, imported rather than repeated. This line used to be its own
        # `isBrave` ternary next to the one in `promote.py`, and when that one learned about
        # Edge and Opera the two would have disagreed: the raw file would have been called
        # `chrome-151-...` while the bundle promoted from it said `edge`. One derivation, two callers.
        engine = promote.browser_of(cond)
        ua = cond.get("ua") or ""
        # The browser's OWN major, so `opera-135-...` is not filed as `opera-151-...`. Identical to
        # the Chromium major for every other member of the family.
        major = str(promote.product_major(cond)) if "Chrome/" in ua else "unknown"
        farb = (cond.get("farbling") or {}).get("verdict") or "unknown"
        farb = "raw" if farb.startswith("off") else ("farbled" if farb.startswith("on") else "unknownfarb")
        plat = "mac" if "Macintosh" in ua else ("win" if "Windows" in ua else ("linux" if "Linux" in ua or "X11" in ua else "os"))
        os.makedirs(OUT_DIR, exist_ok=True)
        stamp = time.strftime("%Y%m%dT%H%M%S")
        fname = "%s-%s-%s-%s-%s.json" % (engine, major, plat, farb, stamp)
        dest = os.path.join(OUT_DIR, fname)
        with open(dest, "wb") as f:
            f.write(raw)
        rel = os.path.relpath(dest, ROOT)
        drift = self._drift(doc)
        # The summary is printed on both sides: the browser shows it, and the terminal keeps it.
        surf = (doc.get("surfaces") or {}).get("window") or {}
        summary = ("%s  (%.1f MB)\n"
                   "  engine %s %s on %s | farbling %s | secure %s | devtools-api %s\n"
                   "  interfaces %s | globals %s | css properties %s | fonts %s | behavior answers %s"
                   % (rel, len(raw) / 1048576, engine, major, plat, farb,
                      cond.get("isSecureContext"), len(cond.get("hasDevToolsApi") or []),
                      len(surf.get("interfaces") or {}), len(surf.get("globals") or []),
                      (doc.get("css") or {}).get("count"), (doc.get("fonts") or {}).get("count"),
                      (doc.get("behavior") or {}).get("recorded"))
                   + drift)
        print("\n[saved] " + summary + "\n", flush=True)
        return self._send(200, summary)

    def _drift(self, doc):
        """What this capture adds or drops against the committed baseline.

        Printed immediately, because the one question a fresh capture has to answer is whether it is
        CLEANER than the one it would replace. The committed baseline was taken with DevTools open on a
        Discourse forum, and it carries `$`, `inspect`, `jQuery`, `Discourse` and an `ItsATrap` class
        that Korvus then shipped as a native interface on every page it loaded. If those names appear
        in the DROPPED column, this capture is the fix. If new ones appear in the ADDED column, the
        browser that ran it has extensions injecting globals, and it is worth re-running in a guest
        window before this becomes the baseline.
        """
        try:
            base_path = _bundle_artifact("surface")
            with open(base_path, encoding="utf-8") as f:
                base = json.load(f)
            new = (doc.get("surfaces") or {}).get("window") or {}
            bg, ng = set(base.get("globals") or []), set(new.get("globals") or [])
            bi, ni = set(base.get("interfaces") or {}), set(new.get("interfaces") or {})
            added_g, dropped_g = sorted(ng - bg), sorted(bg - ng)
            added_i, dropped_i = sorted(ni - bi), sorted(bi - ni)
            show = lambda xs, n=14: (", ".join(xs[:n]) + (" ...+%d" % (len(xs) - n) if len(xs) > n else "")) or "-"
            return ("\n  vs the committed baseline:"
                    "\n    globals    +%d / -%d\n      added   %s\n      dropped %s"
                    "\n    interfaces +%d / -%d\n      added   %s\n      dropped %s"
                    % (len(added_g), len(dropped_g), show(added_g), show(dropped_g),
                       len(added_i), len(dropped_i), show(added_i), show(dropped_i)))
        except Exception as e:
            return "\n  (could not diff against the baseline: %s)" % e

    def log_message(self, fmt, *args):
        # One terse line per request. There are only about eight in a whole capture (the page, the
        # four shared scripts, the case corpus, the POST), and seeing them arrive is the only feedback
        # the operator gets that the page is alive during the two minutes the behavior corpus runs.
        # `getattr`, not attribute access: a keep-alive socket that times out before sending a request
        # line has NEITHER `command` nor `path` set, and the default handler calls this from inside its
        # own error path -- so reading them raised AttributeError there, and Python printed a
        # twenty-line traceback for what is just a browser closing an idle connection. Forty of those
        # buried the log the operator is actually watching.
        sys.stderr.write("  . %s %s\n" % (getattr(self, "command", None) or "?",
                                          (getattr(self, "path", None) or "?").split("?")[0]))

    # A socket that goes quiet must not hold its thread forever; the corpus leaves several open.
    timeout = 30

    def handle_one_request(self):
        # A browser closes keep-alive sockets when it feels like it, and it closes the POST socket the
        # moment the page navigates away. Both surface as ConnectionReset/BrokenPipe and neither is a
        # problem, but the default handler prints a full traceback for each -- which buried the one
        # line that matters (the saved-capture summary) under forty of them on the first run.
        try:
            BaseHTTPRequestHandler.handle_one_request(self)
        except (ConnectionResetError, BrokenPipeError):
            self.close_connection = True


def main():
    # A FRESH ORIGIN EVERY RUN. Permissions, storage grants and site data are keyed by origin, and
    # the browser remembers them across runs: the first Linux capture recorded
    # `Notification.permission` as "granted" and `navigator.storage.persisted()` as true, because
    # the user had once said yes to 127.0.0.1:8900. Those are facts about one visit, not about the
    # device, and the corpus asks about both. A random port is a new origin with nothing remembered,
    # so every capture starts from the browser's defaults. Pass a port to pin one deliberately.
    import random
    port = int(sys.argv[1]) if len(sys.argv) > 1 else random.randint(8901, 8999)
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    # Unbuffered: the operator is watching this terminal for the saved-capture summary, and Python
    # buffers stdout whenever it is not a TTY -- which is exactly the case when the host is started
    # with its output redirected to a log.
    try:
        sys.stdout.reconfigure(line_buffering=True)
    except Exception:
        pass
    print("Korvus reference capture host")
    print("  open this in the browser you want to record:  http://127.0.0.1:%d/" % port)
    print("  captures are written to:                      %s/" % os.path.relpath(OUT_DIR, ROOT))
    _cases = behavior_cases()
    print("  behavior corpus served:                       %d expressions" % len(_cases))
    if SKIPPED_DIALOG_CASES:
        _uniq = sorted(set(SKIPPED_DIALOG_CASES))
        print("  withheld (would open a dialog):               %d expressions, %d members"
              % (len(SKIPPED_DIALOG_CASES), len(_uniq)))
        print("      %s" % ", ".join(_uniq[:6]) + (", ..." if len(_uniq) > 6 else ""))
    print("  (ctrl-C to stop)")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped")


if __name__ == "__main__":
    main()
