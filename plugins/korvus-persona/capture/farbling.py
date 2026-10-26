#!/usr/bin/env python3
"""Sample a browser's fingerprint farbling across fresh sessions, and emit it as a bundle artifact.

    plugins/korvus-persona/capture/farbling.py --sessions 30 --bundle brave-151-linux-20260907T1006
    plugins/korvus-persona/capture/farbling.py --sessions 2 --dry-run          # smoke the rig
    plugins/korvus-persona/capture/farbling.py --sessions 8 --cpus 0-3 --dry-run

WHY THIS IS NOT `collect.js` / `promote.py`.

`collect.js` is ONE visit to ONE browser, and its own header says why that is the right shape for
everything it records and the wrong shape for this one: "Brave re-rolls a seed per session and
shuffles exactly the surfaces a fingerprinter reads, so a shields-up recording of
`navigator.plugins` is one session's dice roll, not a fact about the browser." So the capture rig
was told to turn farbling OFF, and it does — which is correct for every constant the bundle serves,
and leaves the farbling itself unrecorded.

The rule for what to do instead: **a fact that is a DISTRIBUTION is still CAPTURED, not
declared.** The emitter samples it across N sessions and the artifact records the POLICY, the SAMPLE
SIZE and the OBSERVED SPREAD, the way `fonts/metrics.json` records measured advances rather than a
font's name. A value somebody typed into a manifest is not an artifact at all. This is that emitter.

WHAT IT PRODUCES: `<bundle>/farbling.json`, role `farbling`, `matches_capture = true` — the capture's
own output, so it does not move `every_foreign_artifact_is_accounted_for` off its floor of 1.

THE TWO COLUMNS ARE ONE RUN, and that is load-bearing. A farbled reading means nothing without the
unfarbled reading it is a perturbation OF: the canvas deltas are `observed - fill`, the audio fudge is
`observed / unfarbled`, and `hardwareConcurrency`'s support is bounded by the real core count. So this
tool launches both — shields at their DEFAULT for the sample, and `--disable-features=BraveFarbling`
for the base — and writes them into one file. A base copied from somewhere else would be the
"two machines, one persona" this rig was built to end, reintroduced one directory down.

BROWSER HYGIENE, because this launches dozens. The binary is the real ELF (`/opt/brave.com/brave/brave`,
not the `/usr/bin/brave-browser` shell wrapper), so the PID we hold IS the browser's and killing it
kills the browser rather than orphaning its children. Sessions are sequential by default, each profile
is removed as its browser is reaped, and every kill is verified through `/proc/<pid>` — never through a
name pattern, which would match this process's own command line.
"""
import argparse
import http.server
import json
import os
import pathlib
import random
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request


def _repo_root():
    """Walk up to the workspace root rather than counting `..` hops — see host.py's note."""
    d = pathlib.Path(__file__).resolve().parent
    for cand in [d, *d.parents]:
        m = cand / "Cargo.toml"
        if m.exists() and "[workspace]" in m.read_text(encoding="utf-8", errors="ignore"):
            return cand
    raise SystemExit("cannot find the workspace root above %s" % d)


ROOT = _repo_root()
PROBE = pathlib.Path(__file__).resolve().parent / "farbling-probe.js"

# Four ORIGINS, three addresses. `localhost` and `127.0.0.1` are the same server over the same
# socket, so a browser that keyed its seed on the ADDRESS would give them one draw and a browser that
# keys on the HOST STRING gives them two. That pair is the whole reason there are four rather than
# three, and the scope section reports it as its own line.
HOSTS = ["127.0.0.1", "127.0.0.2", "127.0.0.3", "localhost"]
BIND = {"127.0.0.1": "127.0.0.1", "127.0.0.2": "127.0.0.2", "127.0.0.3": "127.0.0.3",
        "localhost": "127.0.0.1"}

# The window sizes sessions cycle through. `screen.*` is not a free draw — it is the smallest entry
# of a fixed resolution list that contains the (already farbled) outer window — so the axis that
# recovers the LIST is WINDOW SIZE, not session count.
#
# IT IS A LAUNCH FLAG AND NOT `Browser.setWindowBounds`, and that is measured, not preferred: 14
# `setWindowBounds` calls in one session moved `innerWidth`/`outerWidth` by 0 px on this bare Xvfb
# (the smoke run of 2026-09-19). `--window-size` moves them every time. So the sweep rides on the
# sessions the distributions need anyway — session `i` gets window `i % len(WINDOWS)` — and the
# resolution list costs no extra browser launch at all.
# The last two are BOUNDARY windows and they are the control, not more of the same. `outer` is
# `inner + U[0,8]`, so on an ordinary window the two land on the same list entry and "the rule holds
# against outer" and "the rule holds against inner" both read 100 % — a control that cannot fire.
# These two put `inner` EXACTLY on a list entry (1320x881 -> 1280x800 and 1480x981 -> 1440x900, on a
# profile whose chrome measures 40x81), so eight draws in nine push `outer` over the edge and the two
# readings must disagree. If they do not, the rule is wrong and this run says so.
WINDOWS = [(600, 400), (800, 600), (1000, 700), (1200, 800), (1290, 810), (1350, 850),
           (1360, 820), (1400, 900), (1450, 920), (1500, 1000), (1600, 1000), (1700, 1050),
           (1920, 1052), (1920, 1080), (2000, 1200), (2200, 1300), (2700, 1500),
           (1320, 881), (1480, 981)]


# ───────────────────────────── the origin server ──────────────────────────────────────────────────

class _Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):  # the access log is `self.server.seen`, not stderr
        pass

    def do_GET(self):
        self.server.seen.append({
            # Stamped by the RUNNER, not parsed out of the path: a browser asks for `/favicon.ico`
            # on its own and that request carries the same `accept-language` as the page's, so
            # attributing by path would throw away the evidence that one q covers every request a
            # cell makes — which is the whole claim. Sessions are sequential, so "the session that
            # is running" is unambiguous.
            "session": self.server.current[0],
            "farbled": self.server.current[1],
            "host": self.headers.get("Host", ""),
            "path": self.path,
            # The header this exists for: Brave farbles the q-value and leaves the list alone, so the
            # whole signal is in a string no page can read about itself.
            "accept_language": self.headers.get("Accept-Language"),
            "sec_gpc": self.headers.get("Sec-GPC"),
        })
        body = b"<!doctype html><meta charset=utf-8><title>farbling probe</title><body>ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


class Origins:
    """One port, three loopback bindings, one shared access log."""

    def __init__(self):
        self.seen: list[dict] = []
        self.current = ["?", True]
        self.port = _free_port()
        self.servers = []
        for addr in sorted({BIND[h] for h in HOSTS}):
            srv = http.server.ThreadingHTTPServer((addr, self.port), _Handler)
            srv.daemon_threads = True
            srv.seen = self.seen
            srv.current = self.current
            threading.Thread(target=srv.serve_forever, daemon=True).start()
            self.servers.append(srv)

    def begin(self, sid, farbled):
        self.current[0], self.current[1] = sid, farbled

    def url(self, host, path):
        return "http://%s:%d%s" % (host, self.port, path)

    def close(self):
        for s in self.servers:
            s.shutdown()
            s.server_close()


def _free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# ───────────────────────────── the browser ────────────────────────────────────────────────────────

QUIET = [
    "--no-first-run", "--no-default-browser-check", "--disable-background-networking",
    "--disable-component-update", "--disable-domain-reliability", "--disable-sync",
    "--metrics-recording-only", "--no-service-autorun", "--password-store=basic",
    "--use-mock-keychain",
]


class Session:
    """One fresh profile, launched, attached, and reaped by the PID we started.

    `--remote-debugging-port` is a REAL port we picked. `=0` is the convenient way not to choose and
    it sets `navigator.webdriver` on its own (L18.1, measured) — which would make every cell this
    tool records a reading of an automated browser rather than of a browser.
    """

    def __init__(self, binary, window, farbled=True, cpus=None, display=None):
        self.profile = tempfile.mkdtemp(prefix="korvus-farbling-")
        self.port = _free_port()
        argv = [binary,
                # The X display we were given, never the desktop: under a Wayland login a Chromium
                # browser otherwise connects to the user's compositor.
                "--ozone-platform=x11",
                "--remote-debugging-port=%d" % self.port,
                "--user-data-dir=%s" % self.profile,
                "--window-size=%d,%d" % window,
                *QUIET, "about:blank"]
        if not farbled:
            # The control column only. It is a FLAG and never a preference file, so the farbled runs
            # are the browser's own default with nothing done to them at all.
            argv.insert(1, "--disable-features=BraveFarbling")
        if cpus:
            argv = ["taskset", "-c", cpus] + argv
        env = dict(os.environ)
        if display:
            env["DISPLAY"] = display
        self.proc = subprocess.Popen(argv, env=env,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.ws = {}
        if not self._wait_for_devtools():
            self.close()
            raise RuntimeError("DevTools never answered on port %d" % self.port)

    def _wait_for_devtools(self, timeout=40.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if self.proc.poll() is not None:
                return False
            try:
                with urllib.request.urlopen("http://127.0.0.1:%d/json/version" % self.port,
                                            timeout=1) as r:
                    self.browser_ws = json.loads(r.read().decode())["webSocketDebuggerUrl"]
                return True
            except Exception:  # noqa: BLE001 — not up yet
                time.sleep(0.05)
        return False

    # -- CDP ---------------------------------------------------------------------------------------

    def _conn(self, url):
        import websocket
        if url not in self.ws:
            self.ws[url] = websocket.create_connection(url, suppress_origin=True, timeout=60)
        return self.ws[url]

    def cdp(self, url, method, params=None, timeout=60):
        c = self._conn(url)
        mid = random.randrange(1, 1 << 30)
        c.send(json.dumps({"id": mid, "method": method, "params": params or {}}))
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            msg = json.loads(c.recv())
            if msg.get("id") != mid:
                continue          # an event, or another call's reply; CDP interleaves both
            if "error" in msg:
                raise RuntimeError("%s: %s" % (method, msg["error"]))
            return msg.get("result", {})
        raise TimeoutError(method)

    def open_tab(self, url):
        r = self.cdp(self.browser_ws, "Target.createTarget", {"url": url})
        tid = r["targetId"]
        return tid, "ws://127.0.0.1:%d/devtools/page/%s" % (self.port, tid)

    def evaluate(self, page_ws, expr, timeout=60):
        r = self.cdp(page_ws, "Runtime.evaluate",
                     {"expression": expr, "awaitPromise": True, "returnByValue": True},
                     timeout=timeout)
        if r.get("exceptionDetails"):
            raise RuntimeError(json.dumps(r["exceptionDetails"])[:400])
        return r["result"].get("value")

    def wait_loaded(self, page_ws, origin, timeout=30.0):
        end = time.monotonic() + timeout
        last = None
        while time.monotonic() < end:
            try:
                last = self.evaluate(page_ws, "location.origin + '|' + document.readyState", timeout=10)
                if last == origin + "|complete":
                    return
            except Exception as e:  # noqa: BLE001 — the target may not be ready to evaluate yet
                last = repr(e)[:120]
            time.sleep(0.1)
        raise TimeoutError("tab never reached %s (last: %s)" % (origin, last))

    # -- reaping -----------------------------------------------------------------------------------

    def close(self):
        """Kill the PID we started, prove it is gone through `/proc`, then prove the profile is too.

        Never a name pattern: any matcher carrying its pattern on its own command line is in its own
        haystack. `/proc/<pid>` is the only question with one answer.

        THE PROFILE NEEDS ITS OWN LOOP, which cost 8 leaked directories out of ~120 sessions before
        it had one. Killing the browser's PID does not stop its already-forked children instantly,
        and a renderer or the cache thread writes into the profile AFTER the tree is removed —
        recreating `Default/` and `Default/Cache/` behind `rmtree`. A single `rmtree(...,
        ignore_errors=True)` reports success for exactly that case, which is how the leak stayed
        invisible. So the removal is retried until the path is GONE, and the caller is told when it
        is not, rather than being told nothing.
        """
        for c in self.ws.values():
            try:
                c.close()
            except Exception:  # noqa: BLE001
                pass
        try:
            self.proc.kill()
            self.proc.wait(timeout=30)
        except Exception:  # noqa: BLE001
            pass
        end = time.monotonic() + 15
        while pathlib.Path("/proc/%d" % self.proc.pid).exists() and time.monotonic() < end:
            time.sleep(0.05)
        gone = not pathlib.Path("/proc/%d" % self.proc.pid).exists()
        for _ in range(20):
            shutil.rmtree(self.profile, ignore_errors=True)
            if not os.path.exists(self.profile):
                break
            time.sleep(0.25)
        return gone and not os.path.exists(self.profile)


# ───────────────────────────── sampling ───────────────────────────────────────────────────────────

def sample_session(binary, origins, sid, window, farbled, cpus, display):
    """One session: five tabs over four origins.

    The FIFTH tab is origin 1 a second time, and the SECOND probe in each tab is the same code run
    twice in one page. Those two are not redundancy — they are the entire evidence for the scoping
    rule, and without them "per (session, origin)" is an assumption rather than a measurement.
    """
    probe = PROBE.read_text(encoding="utf-8")
    origins.begin(sid, farbled)
    s = Session(binary, window, farbled=farbled, cpus=cpus, display=display)
    cells = []
    try:
        plan = [(h, "read1") for h in HOSTS] + [(HOSTS[0], "read2")]
        for i, (host, tag) in enumerate(plan):
            path = "/c/%s/%d" % (sid, i)
            url = origins.url(host, path)
            _tid, ws = s.open_tab(url)
            s.wait_loaded(ws, "http://%s:%d" % (host, origins.port))
            a = s.evaluate(ws, probe)
            b = s.evaluate(ws, probe)          # same page, same code, twice
            cells.append({"session": sid, "host": host, "tag": tag, "path": path,
                          "window": list(window), "farbled": farbled, "a": a, "b": b})
    finally:
        reaped = s.close()
    return cells, reaped


# ───────────────────────────── analysis ───────────────────────────────────────────────────────────

def _counts(vals):
    out = {}
    for v in vals:
        k = json.dumps(v, ensure_ascii=False) if not isinstance(v, (str, int)) else str(v)
        out[k] = out.get(k, 0) + 1
    return dict(sorted(out.items(), key=lambda kv: (-kv[1], kv[0])))


def _chi2_uniform(counts):
    n = sum(counts.values())
    k = len(counts)
    if k < 2 or n == 0:
        return None
    exp = n / k
    return round(sum((c - exp) ** 2 / exp for c in counts.values()), 3)


def _signal(cell, path):
    cur = cell
    for p in path.split("."):
        if cur is None:
            return None
        cur = cur.get(p)
    return cur


# Every signal this emitter reads, and the ONE place that says where each one lives. A list rather
# than a tangle of accessors because the scope section, the spread section and the base column all
# have to walk exactly the same set — three walks of three different lists is how a signal comes to
# be scoped in one place and unmeasured in another.
SIGNALS = [
    ("navigator.hardwareConcurrency", "nav.hardwareConcurrency"),
    ("navigator.deviceMemory",        "nav.deviceMemory"),
    ("navigator.languages",           "nav.languages"),
    ("navigator.mimeTypes.length",    "nav.mimeTypesLength"),
    ("navigator.plugins:names",       None),
    ("canvas:toDataURL",              "canvas.drawing.urlHash"),
    ("canvas:getImageData",           None),
    ("audio:offlineSum",              "audio.sum"),
    ("screen",                        "screen"),
    ("window.outer-inner",            None),
    ("window.screenX/screenY",        None),
]


def _key(cell, name):
    """The comparable value of one signal in one probe read."""
    if name == "navigator.plugins:names":
        ps = _signal(cell, "nav.plugins") or []
        return [p.get("name") for p in ps]
    if name == "canvas:getImageData":
        flat = _signal(cell, "canvas.flat") or {}
        return {k: v.get("eight") for k, v in flat.items()}
    if name == "window.outer-inner":
        w = _signal(cell, "win") or {}
        if not w:
            return None
        return [w.get("outerWidth", 0) - w.get("innerWidth", 0),
                w.get("outerHeight", 0) - w.get("innerHeight", 0)]
    if name == "window.screenX/screenY":
        w = _signal(cell, "win") or {}
        return [w.get("screenX"), w.get("screenY")]
    path = dict((n, p) for n, p in SIGNALS)[name]
    return _signal(cell, path)


def scope_evidence(cells):
    """Is the seed per (session, origin), or per something else?

    Reported as COUNTS against each signal's OWN collision expectation, which is the right null: a
    signal with four possible values agrees across two origins a quarter of the time even when it is
    redrawn every single time. A bare "differs in 672 of 900" means nothing without the 675 beside it.
    """
    by_session = {}
    for c in cells:
        by_session.setdefault(c["session"], []).append(c)

    out = {}
    for name, _ in SIGNALS:
        same_page = [0, 0]
        same_origin_second_tab = [0, 0]
        cross_origin = [0, 0]
        cross_session = [0, 0]
        first_of_session = {}
        for sid, group in by_session.items():
            reads = {}
            for c in group:
                ka, kb = _key(c["a"], name), _key(c["b"], name)
                same_page[1] += 1
                same_page[0] += int(ka == kb)
                reads[(c["host"], c["tag"])] = ka
            r1 = reads.get((HOSTS[0], "read1"))
            r2 = reads.get((HOSTS[0], "read2"))
            if r2 is not None:
                same_origin_second_tab[1] += 1
                same_origin_second_tab[0] += int(r1 == r2)
            for h in HOSTS[1:]:
                other = reads.get((h, "read1"))
                cross_origin[1] += 1
                cross_origin[0] += int(other != r1)
            first_of_session[sid] = r1
        seq = list(first_of_session.values())
        for i in range(1, len(seq)):
            cross_session[1] += 1
            cross_session[0] += int(seq[i] != seq[i - 1])
        # The null: how often two INDEPENDENT draws from the observed distribution would differ.
        obs = _counts([v for v in first_of_session.values() if v is not None])
        n = sum(obs.values()) or 1
        collide = sum((c / n) ** 2 for c in obs.values())
        out[name] = {
            "same_page_twice_equal": "%d/%d" % tuple(same_page),
            "same_origin_second_tab_equal": "%d/%d" % tuple(same_origin_second_tab),
            "differs_across_origins": "%d/%d" % tuple(cross_origin),
            "differs_across_sessions": "%d/%d" % tuple(cross_session),
            "expected_if_redrawn_every_time": round(1 - collide, 4),
        }
    return out


def canvas_policy(farbled_cells, base_cells):
    """The canvas operation, derived from the deltas rather than described.

    The unfarbled column is the fill itself, so the base is not assumed: it is read from the control
    sessions and checked. The emitted policy is the RULE the deltas satisfy — for each channel the
    low bit is flipped or it is not — plus the per-buffer-length pattern support, which is what makes
    an implementation testable without pinning any pixel.
    """
    base = {}
    for c in base_cells:
        for fill, v in (_signal(c, "canvas.flat") or {}).items():
            base.setdefault(fill, set()).add(tuple(v["one"]))
    base_ok = {f: (len(v) == 1 and next(iter(v))) for f, v in base.items()}

    per_fill = {}
    xor_only = [0, 0]
    alpha_moved = 0
    coord_equal = [0, 0]
    for c in farbled_cells:
        for fill, v in (_signal(c, "canvas.flat") or {}).items():
            b = base_ok.get(fill)
            if not b:
                continue
            for length, key in ((1, "one"), (2, "two"), (4, "four"), (8, "eight")):
                buf = v[key]
                pats = []
                for i in range(length):
                    px = buf[i * 4:i * 4 + 4]
                    d = tuple(px[j] - b[j] for j in range(4))
                    xor_only[1] += 1
                    # `channel ^= bit` means the observed channel and the base differ in the LOW BIT
                    # and in nothing else. That is one predicate over the bytes, and it is the whole
                    # claim — a table of signed deltas would be the same fact written unfalsifiably.
                    xor_only[0] += int(all((px[j] ^ b[j]) in (0, 1) for j in range(3)))
                    alpha_moved += int(px[3] != b[3])
                    pats.append("".join(str((px[j] ^ b[j]) & 1) for j in range(3)))
                per_fill.setdefault(fill, {}).setdefault(length, {})
                for p in pats:
                    d = per_fill[fill][length]
                    d[p] = d.get(p, 0) + 1
            coord_equal[1] += 1
            coord_equal[0] += int(v["one"] == v["oneElsewhere"])

    lengths = {}
    for fill, by_len in per_fill.items():
        for length, pats in by_len.items():
            lengths.setdefault(length, set()).update(pats.keys())
    return {
        "operation": "channel ^= bit on the LOW BIT of R, G and B; alpha is not touched",
        "base_is_the_unfarbled_fill": {f: list(v) if v else None for f, v in base_ok.items()},
        "low_bit_only": "%d/%d" % tuple(xor_only),
        "alpha_perturbed": alpha_moved,
        "two_reads_of_the_same_colour_at_different_coordinates_equal": "%d/%d" % tuple(coord_equal),
        "flip_patterns_by_buffer_length": {
            str(k): sorted(v) for k, v in sorted(lengths.items())
        },
        "pattern_counts_by_fill": {f: {str(k): v for k, v in sorted(d.items())}
                                   for f, d in sorted(per_fill.items())},
    }


def plugin_policy(farbled_cells, base_cells):
    """The plugin list as a GRAMMAR, not as a table of the names it happened to produce.

    Recording 3,000 observed names would be a bigger artifact that says less and is still short of
    the space: the generator is `PRE + MID + SUF` over three small closed token lists, and the tokens
    are what an implementation needs. The shape line (4 genuine + 1 worded + 2 random) is recorded as
    a count over every read, so a read that does not have it is visible rather than averaged away.
    """
    genuine = set()
    base_len = set()
    for c in base_cells:
        ps = _signal(c, "nav.plugins") or []
        base_len.add(len(ps))
        genuine.update(p["name"] for p in ps)

    shapes, kept, invented = [0, 0], {g: 0 for g in genuine}, []
    len_counts, mime_counts = {}, {}
    for c in farbled_cells:
        ps = _signal(c, "nav.plugins") or []
        if not ps:
            continue
        len_counts[len(ps)] = len_counts.get(len(ps), 0) + 1
        mt = _signal(c, "nav.mimeTypesLength")
        mime_counts[mt] = mime_counts.get(mt, 0) + 1
        names = [p["name"] for p in ps]
        for g in genuine:
            kept[g] += int(g in names)
        pdfish = [p for p in ps if p["filename"] == "internal-pdf-viewer"]
        shapes[1] += 1
        shapes[0] += int(len(pdfish) == len(genuine) - 1)
        for p in ps:
            if p["filename"] != "internal-pdf-viewer":
                invented.append((p["name"], len(p["name"]), len(p["filename"])))

    # The grid, recovered by the only honest route: the longest match against the tokens seen so far.
    # A name that does not parse is REPORTED, not dropped — an unparsed name is the evidence that the
    # three lists are not the whole generator, and averaging it away is how a grammar comes to be
    # asserted that the browser does not use.
    worded = [n for n, ln, fn in invented if fn == 32]
    pre, mid, suf, unparsed = _grid(worded)
    randoms = [(len(n), fn) for n, ln, fn in invented if fn != 32]
    return {
        "entries": {"unfarbled": sorted(base_len), "farbled": len_counts},
        "navigator_mimeTypes_length": mime_counts,
        "genuine_names_kept": kept,
        "genuine_names_unfarbled": sorted(genuine),
        "entry_shape": "4 genuine (filename internal-pdf-viewer) + 1 generated worded name "
                       "(32-char random filename) + 2 random names — then the list is shuffled",
        "entry_shape_holds": "%d/%d" % tuple(shapes),
        "generated_name_grammar": "PRE + MID + SUF",
        "PRE": pre, "MID": mid, "SUF": suf,
        "names_parsed": "%d/%d" % (len(worded) - len(unparsed), len(worded)),
        "unparsed_names": sorted(unparsed)[:20],
        "random_entry_name_and_filename_lengths": sorted(_counts(
            ["%d/%d" % r for r in randoms]).items(), key=lambda kv: -kv[1]),
    }


def _grid(names):
    """Split `PRE + MID + SUF` out of the observed names, anchored on the MID.

    MID IS THE ONE PART THIS FUNCTION IS TOLD, and the artifact says so rather than presenting the
    whole grammar as discovered. PRE and SUF are read off the data — whatever is left to the left
    and to the right of the anchor, with their own counts — but a name like
    `OpenSource portable-document-format extension` cannot be cut into three without knowing which
    of its words belong together, and no amount of sampling supplies that for a MID that contains
    spaces.

    What makes it a measurement rather than an assertion is the FALSIFIER beside it: every name that
    does not contain any of these anchors is counted and listed in `unparsed_names`. If the browser
    grows a MID this list does not have, the count is not zero and the artifact says which name did
    it. A parser that silently dropped them would be a grammar nothing could contradict.
    """
    MIDS = ["portable-document-format", "Portable Document Format", "com.adobe.pdf",
            "PDF and PS", "document", "doc", "PDF"]
    pre, mid, suf, bad = {}, {}, {}, []
    for n in names:
        hit = None
        for m in MIDS:
            i = n.find(m)
            if i < 0:
                continue
            p, s = n[:i].rstrip(), n[i + len(m):].lstrip()
            if hit is None or len(m) > len(hit[1]):
                hit = (p, m, s)
        if hit is None:
            bad.append(n)
            continue
        p, m, s = hit
        pre[p] = pre.get(p, 0) + 1
        mid[m] = mid.get(m, 0) + 1
        suf[s] = suf.get(s, 0) + 1
    order = lambda d: dict(sorted(d.items(), key=lambda kv: (kv[0] != "", kv[0])))  # noqa: E731
    return order(pre), order(mid), order(suf), bad


def screen_policy(farbled_cells, base_cells, windows):
    """`screen.*` is not a distribution — it is the smallest list entry that contains the FARBLED
    outer window. So the artifact is a LIST plus a PREDICATE, and both are derived here.

    THE LIST IS THE SET OF RESOLUTIONS THIS RUN OBSERVED, not a list from anywhere else. Deriving
    it and then checking the predicate against the same readings is nearly circular and is stated as
    such, so the controls beside it are what carry the weight: the same predicate evaluated against
    `inner*`, scored SEPARATELY on the boundary windows where the two can disagree at all, and the
    window sizes probed, recorded as the list's bounds of validity. A list is only as long as the
    windows that reached it.
    """
    rows = [(_signal(c, "win"), _signal(c, "screen"), c.get("_window"))
            for c in farbled_cells if _signal(c, "win") and _signal(c, "screen")]
    resolutions = sorted({(s["width"], s["height"]) for _w, s, _q in rows})

    def smallest(width, height):
        for (rw, rh) in resolutions:
            if rw >= width and rh >= height:
                return [rw, rh]
        return None

    outer_ok, inner_ok, avail_eq, avail_zero = [0, 0], [0, 0], [0, 0], [0, 0]
    split_outer, split_inner = [0, 0], [0, 0]
    seen, by_window = {}, {}
    for w, s, win in rows:
        got = [s["width"], s["height"]]
        seen[tuple(got)] = seen.get(tuple(got), 0) + 1
        if win:
            d = by_window.setdefault("%dx%d" % tuple(win), {})
            d["%dx%d" % tuple(got)] = d.get("%dx%d" % tuple(got), 0) + 1
        from_outer = smallest(w["outerWidth"], w["outerHeight"])
        from_inner = smallest(w["innerWidth"], w["innerHeight"])
        outer_ok[1] += 1
        outer_ok[0] += int(from_outer == got)
        inner_ok[1] += 1
        inner_ok[0] += int(from_inner == got)
        if from_outer != from_inner:
            # The only readings that can tell the two apart. Scoring them apart from the rest is the
            # difference between a control and a decoration.
            split_outer[1] += 1
            split_inner[1] += 1
            split_outer[0] += int(from_outer == got)
            split_inner[0] += int(from_inner == got)
        avail_eq[1] += 1
        avail_eq[0] += int([s["availWidth"], s["availHeight"]] == got)
        avail_zero[1] += 1
        avail_zero[0] += int(s.get("availTop") == 0 and s.get("availLeft") == 0)
    base_screen = sorted({json.dumps(_signal(c, "screen")) for c in base_cells})
    outers = [(w["outerWidth"], w["outerHeight"]) for w, _s, _q in rows]
    return {
        "farbled": True,
        "policy": "the smallest entry of `resolution_list` with width >= outerWidth and "
                  "height >= outerHeight, where outer* is the ALREADY-FARBLED outer size",
        "read_the_scope_row_with_this": "`screen` has no draw of its own, so its line in `scope` is "
                                        "not a scoping measurement: it reads 0/N across origins "
                                        "because one session's tabs share a window, and non-zero "
                                        "across sessions only because this run CYCLES the window "
                                        "size. `screen_by_requested_window` is the real evidence.",
        "resolution_list": [list(r) for r in resolutions],
        "resolution_list_is": "the distinct values this run observed — a lower bound on the "
                              "browser's list, valid over `farbled_outer_range`",
        "rule_holds_against_farbled_outer": "%d/%d" % tuple(outer_ok),
        "same_rule_against_inner": "%d/%d" % tuple(inner_ok),
        "on_readings_where_outer_and_inner_DISAGREE": {
            "n": split_outer[1],
            "outer_wins": "%d/%d" % tuple(split_outer),
            "inner_wins": "%d/%d" % tuple(split_inner),
            "note": "0 here means this run never straddled a list boundary, so its "
                    "`same_rule_against_inner` score is not evidence either way",
        },
        "availWidth_availHeight_equal_screen": "%d/%d" % tuple(avail_eq),
        "availTop_availLeft_zero": "%d/%d" % tuple(avail_zero),
        "window_sizes_probed": ["%dx%d" % w for w in windows],
        "farbled_outer_range": [list(min(outers)), list(max(outers))] if outers else None,
        "screen_by_requested_window": by_window,
        "resolutions_observed": {("%dx%d" % k): v for k, v in sorted(seen.items())},
        "unfarbled_screen": [json.loads(b) for b in base_screen],
    }


def q_values(seen):
    """The `accept-language` q, by (session, origin) cell, from the server's own access log.

    A page cannot read the header it sent, so this is the one farbled signal that has to come off
    the wire rather than out of the probe. Every request the cell made counts, `/favicon.ico`
    included — "one q across every request" is the claim, and dropping the requests the browser made
    by itself would be assuming it.
    """
    import re

    def column(rows):
        per_cell, lists, n = {}, {}, 0
        for r in rows:
            al = r.get("accept_language")
            if not al:
                continue
            n += 1
            host = r["host"].split(":")[0]
            m = re.search(r";q=([0-9.]+)\s*$", al)
            per_cell.setdefault((r["session"], host), set()).add(m.group(1) if m else None)
            bare = re.sub(r";q=[0-9.]+", "", al)
            lists[bare] = lists.get(bare, 0) + 1
        stable = [v for v in per_cell.values() if len(v) == 1]
        return {
            "requests": n,
            "cells": len(per_cell),
            "one_q_per_cell": "%d/%d" % (len(stable), len(per_cell)),
            "q_counts": _counts([str(next(iter(v))) for v in stable]),
            "language_lists_seen": lists,
        }

    return {
        "farbled": True,
        "policy": "one q per (session, eTLD+1), across every request that cell makes, including the "
                  "ones the browser makes by itself; the language LIST is constant and unfarbled "
                  "requests carry no q parameter at all",
        "sample": column([r for r in seen if r["farbled"]]),
        "unfarbled": column([r for r in seen if not r["farbled"]]),
    }


def build_artifact(meta, farbled, base, seen, windows):
    def once(cells):
        """ONE read per (session, origin) CELL, which is the unit the seed is drawn per.

        This is not tidiness. Each session here makes ten probe reads over four origins — the same
        page read twice, and origin 1 read again in a fifth tab — and every one of those repeats is
        BY CONSTRUCTION equal to the read it repeats, because that equality is the scoping rule. A
        spread computed over all ten would report n = 10 for four independent draws, inflate every
        count by two and a half, and hand a chi-square a denominator it did not earn. The repeats
        are evidence about SCOPE and they are used there; they are not sample size.
        """
        out, seen_cells = [], set()
        for c in cells:
            key = (c["session"], c["host"])
            if key in seen_cells or not c["a"]:
                continue
            seen_cells.add(key)
            # The window this read happened in travels WITH the read: the screen rule is a function
            # of the window, so a reduction that has lost it can only report a distribution, which
            # is exactly the mistake the rule replaces.
            out.append(dict(c["a"], _window=c.get("window")))
        return out

    fa, ba = once(farbled), once(base)

    def spread(name, base_path=None):
        vals = [_key(c, name) for c in fa]
        vals = [v for v in vals if v is not None]
        counts = _counts(vals)
        out = {"n": len(vals), "distinct": len(counts),
               "counts": counts if len(counts) <= 64 else None,
               "chi2_vs_uniform": _chi2_uniform(counts) if len(counts) <= 64 else None}
        if base_path is not None:
            bv = _counts([_key(c, name) for c in ba if _key(c, name) is not None])
            out["unfarbled"] = bv
            out["farbled"] = not (len(counts) == 1 and counts.keys() == bv.keys())
        return out

    scalars = {}
    for name in ("navigator.hardwareConcurrency", "navigator.deviceMemory",
                 "navigator.languages", "navigator.mimeTypes.length"):
        scalars[name] = spread(name, base_path=True)
    hc = scalars["navigator.hardwareConcurrency"]
    hc["policy"] = ("uniform over the integers [2, N]; N is this host's core count as the UNFARBLED "
                    "browser reports it — see the `unfarbled` row here, NOT meta.machineCores")
    scalars["navigator.deviceMemory"]["policy"] = "uniform over a fixed set; see counts"
    scalars["navigator.languages"]["policy"] = (
        "NOT farbled — a REDUCTION: the farbled list is a prefix of the unfarbled one, constant "
        "within a profile, 0 variation across origins and sessions. APPLY IT AT THE "
        "`navigator.languages` BINDING AND NOWHERE ELSE: measured 2026-09-19, Brave at its default "
        "answers the reduced list to a page and still renders `<text systemLanguage=en>` exactly as "
        "the same browser with fingerprinting off does, so the DEVICE's language list is untouched "
        "and everything else that reads it must keep seeing the unreduced one.")
    scalars["navigator.mimeTypes.length"]["policy"] = "not farbled"

    # The audio fudge is a RATIO, so it needs the control column to exist at all.
    bsum = sorted({round(_signal(c, "audio.sum"), 10) for c in ba if _signal(c, "audio.sum")})
    fud = []
    if len(bsum) == 1 and bsum[0]:
        for c in fa:
            s = _signal(c, "audio.sum")
            if s:
                fud.append(s / bsum[0])
    audio = {
        "farbled": True,
        "policy": "one MULTIPLICATIVE factor per (session, eTLD+1) applied to the whole rendered "
                  "buffer; uniform on [lo, 1.0]",
        "unfarbled_sum": bsum,
        "n": len(fud),
        "factor_min": min(fud) if fud else None,
        "factor_max": max(fud) if fud else None,
        "factor_mean": round(statistics.fmean(fud), 9) if fud else None,
        "factor_deciles_from_min": _decile_histogram(fud),
        "distinct_sums": len({_signal(c, "audio.sum") for c in fa}),
    }

    geom = {}
    for name in ("window.outer-inner", "window.screenX/screenY"):
        geom[name] = spread(name, base_path=True)
        # The PAIR's chi-square is over 81 cells and says little at any n this tool will reach; the
        # claim is that each AXIS is uniform on 0..8 and that the two are drawn independently, so
        # the marginals are what carry it. A pair table alone would be a spread nobody can read.
        pairs = [_key(c, name) for c in fa if _key(c, name) is not None]
        geom[name]["per_axis"] = {
            "x": _counts([p[0] for p in pairs]), "y": _counts([p[1] for p in pairs]),
            "chi2_vs_uniform_x": _chi2_uniform(_counts([p[0] for p in pairs])),
            "chi2_vs_uniform_y": _chi2_uniform(_counts([p[1] for p in pairs])),
            "distinct_pairs_seen": len({tuple(p) for p in pairs}),
        }
    geom["window.outer-inner"]["policy"] = (
        "outer = inner + U[0, 8] per axis, drawn independently. NOTE the `unfarbled` row: farbling "
        "does not ADD to the real chrome, it REPLACES it — the unfarbled difference is this "
        "profile's actual window furniture and no farbled draw reproduces it except by coincidence. "
        "inner* is NOT farbled (ruling R18).")
    geom["window.screenX/screenY"]["policy"] = "U[0, 8] per axis, drawn independently"
    geom["window.inner"] = {"policy": "NOT farbled (ruling R18: it is the captured window)",
                            "counts": _counts([json.dumps([(_signal(c, "win") or {}).get("innerWidth"),
                                                           (_signal(c, "win") or {}).get("innerHeight")])
                                               for c in fa])}

    return {
        "meta": meta,
        "scope": {
            "policy": "every farbled signal is redrawn per (session, eTLD+1) and per nothing else — "
                      "not per page load, not per tab, not per read",
            "keyed_on": "the origin's HOST STRING as written, not the address it resolves to "
                        "(`localhost` and `127.0.0.1` are one socket and draw differently)",
            "read_the_counts_against_expected_if_redrawn_every_time": True,
            "evidence": scope_evidence(farbled),
            "unfarbled_control": scope_evidence(base),
        },
        "signals": {
            "scalars": scalars,
            "navigator.plugins": plugin_policy(fa, ba),
            "canvas": canvas_policy(fa, ba),
            "audio": audio,
            "screen": screen_policy(fa, ba, windows),
            "geometry": geom,
            "accept-language": q_values(seen),
        },
    }


def _decile_histogram(vals):
    if not vals:
        return None
    lo, hi = min(vals), max(vals)
    if hi == lo:
        return None
    out = [0] * 10
    for v in vals:
        i = min(9, int((v - lo) / (hi - lo) * 10))
        out[i] += 1
    return out


# ───────────────────────────── main ───────────────────────────────────────────────────────────────

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default="/opt/brave.com/brave/brave",
                    help="the browser's REAL ELF, not a shell wrapper: the PID must be the browser's")
    ap.add_argument("--sessions", type=int, default=30, help="farbled sessions (4 origins each)")
    ap.add_argument("--control", type=int, default=3,
                    help="unfarbled sessions (--disable-features=BraveFarbling): the base column")
    ap.add_argument("--windows", default="",
                    help="comma-separated WxH the sessions CYCLE through; empty = the built-in "
                         "sweep, which is what recovers the screen resolution list for free. "
                         "`--windows 1920x1052` pins one size.")
    ap.add_argument("--cpus", default="", help="a taskset cpu list, e.g. 0-3 — changes the host core "
                                               "count the browser sees, which is how the "
                                               "hardwareConcurrency bound is tested rather than assumed")
    ap.add_argument("--display", default="", help="DISPLAY for the browsers (an Xvfb, normally)")
    ap.add_argument("--bundle", default="", help="bundle dir or id to write farbling.json into")
    ap.add_argument("--out", default="", help="write the artifact here instead of into a bundle")
    ap.add_argument("--raw", default="", help="also write every cell, unreduced, here")
    ap.add_argument("--from-raw", default="",
                    help="launch nothing: re-reduce a saved --raw run into the artifact. The RAW "
                         "cells are the measurement; the artifact is a reduction of them, and a "
                         "reduction that can only be re-derived by re-spending 40 browser launches "
                         "is one nobody will re-derive.")
    ap.add_argument("--dry-run", action="store_true", help="sample and print, write nothing")
    a = ap.parse_args()

    if a.from_raw:
        raw = json.loads(pathlib.Path(a.from_raw).read_text(encoding="utf-8"))
        meta = dict(raw["meta"])
        # The capture keeps its own timestamp; the reduction stamps its own. A re-reduction that
        # claimed a new `capturedAt` would say browsers ran that did not.
        meta["reducedAt"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        meta["reducedFrom"] = os.path.basename(a.from_raw)
        wins = [tuple(int(x) for x in w.split("x")) for w in meta.get("windowSizes", [])]
        return _write(a, build_artifact(meta, raw["farbled"], raw["unfarbled"],
                                        raw["requests"], wins))

    windows = ([tuple(int(x) for x in w.lower().split("x")) for w in a.windows.split(",")]
               if a.windows else list(WINDOWS))
    total = a.sessions + a.control
    print("farbling: %d farbled + %d unfarbled sessions x %d origins = %d cells, %d browser launches"
          % (a.sessions, a.control, len(HOSTS), total * len(HOSTS), total))

    origins = Origins()
    farbled, base, leaked = [], [], 0
    t0 = time.time()
    try:
        for i in range(a.sessions):
            sid = "f%03d" % i
            win = windows[i % len(windows)]
            cells, reaped = sample_session(a.binary, origins, sid, win, True,
                                           a.cpus, a.display or None)
            farbled += cells
            leaked += (0 if reaped else 1)
            print("  %s  %2d/%2d  %dx%d  %d cells  %.1fs"
                  % (sid, i + 1, a.sessions, win[0], win[1], len(cells), time.time() - t0),
                  flush=True)
            time.sleep(1.0)     # a second between launches
        for i in range(a.control):
            sid = "u%03d" % i
            win = windows[i % len(windows)]
            cells, reaped = sample_session(a.binary, origins, sid, win, False,
                                           a.cpus, a.display or None)
            base += cells
            leaked += (0 if reaped else 1)
            print("  %s  %2d/%2d  %dx%d  %d cells (unfarbled)"
                  % (sid, i + 1, a.control, win[0], win[1], len(cells)), flush=True)
            time.sleep(1.0)
        seen = list(origins.seen)
    finally:
        origins.close()

    if leaked:
        print("WARNING: %d session(s) left a live process or a profile directory behind — "
              "report this, do not ignore it" % leaked)

    ver = subprocess.run([a.binary, "--version"], capture_output=True, text=True).stdout.strip()
    meta = {
        "emitter": "plugins/korvus-persona/capture/farbling.py",
        "probe": "plugins/korvus-persona/capture/farbling-probe.js",
        "browser": ver,
        "binary": a.binary,
        "capturedAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "display": a.display or os.environ.get("DISPLAY", ""),
        "windowSizes": ["%dx%d" % w for w in windows],
        # `cpuset` is what the BROWSER was given; `machineCores` is what the box has. They differ
        # under `--cpus`, which is the run that TESTS whether the farbled support's top is the core
        # count rather than the constant 12 this box happens to have. The authoritative N is
        # neither of these: it is `signals.scalars["navigator.hardwareConcurrency"].unfarbled`,
        # which is the browser's own answer, and the policy string points at it for that reason.
        "cpuset": a.cpus or "all",
        "machineCores": os.cpu_count(),
        "origins": [origins.url(x, "/") for x in HOSTS],
        "sessions": {"farbled": a.sessions, "unfarbled": a.control},
        # CELLS are (session, origin) pairs — the unit a farbled value is drawn per, and the n every
        # spread below is computed over. READS are the probe evaluations, of which there are two and
        # a half per cell, and they are the evidence for the scoping rule rather than sample size.
        "cells": {"farbled": len({(c["session"], c["host"]) for c in farbled}),
                  "unfarbled": len({(c["session"], c["host"]) for c in base})},
        "probe_reads": {"farbled": 2 * len(farbled), "unfarbled": 2 * len(base)},
        "webdriver_seen": sorted({str((c["a"] or {}).get("webdriver")) for c in farbled + base}),
        # A live process OR a surviving profile directory. Both, because they leak independently:
        # the PID goes first and the directory can come back behind `rmtree`.
        "sessions_not_fully_cleaned": leaked,
    }
    if a.raw:
        pathlib.Path(a.raw).write_text(
            json.dumps({"meta": meta, "farbled": farbled, "unfarbled": base,
                        "requests": seen}, indent=1), encoding="utf-8")
        print("  raw -> %s" % a.raw)
    return _write(a, build_artifact(meta, farbled, base, seen, windows))


def _write(a, art) -> int:
    """The artifact's one destination, shared by a sampling run and a `--from-raw` re-reduction."""
    text = json.dumps(art, indent=1, ensure_ascii=False) + "\n"
    if a.dry_run or (not a.bundle and not a.out):
        print(text)
        return 0
    if a.out:
        # A loose file, for a run that is measuring rather than promoting. It is NOT registered:
        # an artifact a manifest does not name is exactly the unrecorded state `Bundle::self_check`
        # exists to prevent, so the two are the same call or neither is.
        target = pathlib.Path(a.out)
        before = target.stat().st_size if target.exists() else 0
        target.write_text(text, encoding="utf-8")
        print("  farbling         %-24s %8d -> %8d bytes (loose: NOT registered in any manifest)"
              % (target.name, before, len(text.encode())))
        return 0

    d = pathlib.Path(a.bundle)
    if not d.is_dir():
        d = ROOT / "plugins/korvus-persona/bundles" / a.bundle
    if not d.is_dir():
        raise SystemExit("farbling: no bundle directory at %s" % a.bundle)

    # WHERE THE FILE GOES AND HOW THE MANIFEST LEARNS ABOUT IT COME FROM `emit.py`, not from here.
    # This script knows how to PRODUCE a farbling policy; it deliberately does not hold a second
    # opinion about what a bundle contains or about the shape of an `[[artifact]]` block. Two places
    # that express one fact are correct the day they are written and disagree six months later, and
    # this repo has paid for that more than once.
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
    import emit  # noqa: E402 — the bundle-artifact table and the manifest stamper

    target = d / emit.BUNDLE_FILES["farbling"]
    before = target.stat().st_size if target.exists() else 0
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")
    emit.mark_recorded(d, "farbling")
    print("  farbling         %-24s %8d -> %8d bytes, registered in %s/manifest.toml"
          % (emit.BUNDLE_FILES["farbling"], before, len(text.encode()), d.name))
    return 0


if __name__ == "__main__":
    sys.exit(main())
