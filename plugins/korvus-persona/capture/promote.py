#!/usr/bin/env python3
"""Turn a raw capture into a persona bundle — the step the rig was always missing.

    plugins/korvus-persona/capture/promote.py reference/captures/<raw>.json
    plugins/korvus-persona/capture/promote.py reference/captures/<raw>.json --id chrome-151-linux-20260907
    plugins/korvus-persona/capture/promote.py <raw>.json --dry-run

WHY THIS EXISTS.

`collect.js` has, since it was written, gathered every reference artifact this project needs in ONE
visit to ONE browser. Its own header says why: "the values profile describes a Mac with a 1512x982
screen, 8 GB and an M3 Pro, and the render reference describes one with 1680x1050, 16 GB and an M5.
Two machines, one persona." It was built to end exactly that.

And then its output went to `reference/captures/`, which is gitignored, and every committed artifact
kept being produced by a different errand: a console paste for the window surface, a second for the
worker surface, a CDP run for the behaviour corpus, a Blink checkout for the UA stylesheet. The
collector solved the problem and nothing consumed its answer. THIS is the consumer.

WHAT IT PRODUCES.

A bundle directory under `plugins/korvus-persona/bundles/<capture-id>/`, with:

  values.json    navigator/screen/window/intl/memory/deviceOracle — what the persona serves
  manifest.toml  the capture's identity, its artifacts, and its known incoherences

The manifest is STAMPED FROM THE CAPTURE, not written by hand. `conditions` already records the UA,
the timestamp, the origin, whether DevTools was open, whether webdriver was set, and -- the one the
old bundle had to admit it did not know -- the farbling verdict, measured by reading back a canvas
twice and comparing. A hand-written manifest is a manifest that can claim a protection state nobody
observed, which is what `known_incoherence "farbling-unknown"` exists to record.

WHAT IT DOES NOT DO, DELIBERATELY.

It does not overwrite a bundle that exists. A bundle is a record of one session; re-promoting over
it would silently rewrite history that other artifacts are keyed to. Use a new capture id.

It does not invent `known_incoherence` entries it cannot justify, and it does not omit ones the
capture proves. If the capture's UA disagrees with its own platform, that is written down.
"""
import argparse, json, os, pathlib, re, sys

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
BUNDLES = ROOT / "plugins" / "korvus-persona" / "bundles"

# THE ONE CLIENTHELLO EVERY DECLARED PRESET NOW PUTS ON THE SOCKET, read
# off a loopback TLS sink for chrome_149, edge_148 and opera_131 alike:
# `plugins/korvus-transport/tests/clienthello.rs::the_four_presets_emit_the_browser_familys_own_ja4`.
#
# It is the SAME JA4 this box measured off real Brave 151, Chrome 150, Edge 151, Opera 135 and
# a reference host-OS policy 151, so the `tls-version` entry no longer has to distinguish a measured pairing from an
# assumed one: there is one wire and all of them are on it. (The head reads `t13i1515h2` rather than
# `t13d1516h2` only because the sink is addressed by IP literal, which suppresses SNI -- FP1.3.)
#
# WHAT THE ENTRY STILL RECORDS is the preset's NAME. `coherence::audit` compares the preset's trailing
# integer to the UA's Chrome major and has no socket, so it cannot see that the bytes now agree; and
# `wreq-util 0.2` ships no preset a 150/151/135 capture could name. Removing the entry was tried and
# measured on 2026-09-19: `Engine::assemble` refuses to boot the bundle. So it is emitted, and it says
# so.
TLS_FAMILY_JA4 = "t13i1515h2_8daaf6152771_806a8c22fdea"
TLS_FAMILY_SIGALGS = "0904,0905,0906,0403,0804,0401,0503,0805,0501,0806,0601"

# Paths are RELATIVE TO THE BUNDLE DIRECTORY (the bundle is one directory; the manifest is
# its index; `Bundle::self_check` refuses a captured artifact that lives outside it). The names are
# emit.py's `BUNDLE_FILES`. Only the UA stylesheet points outside -- no capture can emit one (see
# emit.py) -- and it is declared inherited.
ARTIFACTS = [
    ("values",          "values.json",                                True),
    ("surface",         "surface.json",                               False),
    ("worker-surface",  "worker.surface.json",                        False),
    ("oracle",          "oracle",                                     False),
    ("codecs",          "codecs.json",                                False),
    ("render-reference","render/reference.json",                      False),
    ("font-metrics",    "fonts/metrics.json",                         False),
    ("theme",           "theme.json",                                 False),
    ("computed-style",  "computed-style.json",                        False),
    ("webrtc-offer",    "webrtc-offer.json",                          False),
    ("ua-stylesheet",   "../../../../core/korvus-page/ua/html.css",   False),
]


def os_family(ua: str) -> str:
    if "Android" in ua: return "android"
    if "iPhone" in ua or "iPad" in ua: return "ios"
    if "Macintosh" in ua or "Mac OS X" in ua: return "macos"
    if "Windows" in ua: return "windows"
    if "Linux" in ua or "X11" in ua: return "linux"
    return "unknown"


def chrome_major(ua: str):
    m = re.search(r"Chrome/(\d+)", ua)
    return int(m.group(1)) if m else 0


# ── which browser this capture came from, and which wire that browser speaks ─────────────────────
#
# WHY THIS IS DERIVED AND NOT DECLARED. Once this file held a module constant,
# `TLS_PRESET = "chrome_149"`, and stamped it into every manifest it wrote. With two Chromium
# captures that was merely redundant -- Brave rides Chrome's wire and Chrome is Chrome. With Edge and
# Opera in the directory it becomes a COHERENCE LIE written by a generator: three of four bundles
# would declare a Chrome ClientHello under a non-Chrome UA, and a generated lie repeats where a hand
# edit does not. The browser was already derived from the capture (`isBrave`); the wire now is too.
#
# `korvus-transport::emulation_for` deliberately does not ask which family a preset name belongs to
# (its own doc comment says so, and names this constant as the reason nothing could reach that hole).
# Deciding the family is this rig's job, at promotion time, from the capture -- not the engine's.

# The UA token that identifies each non-Chrome member of the family, and the wreq-util preset family
# it maps to. ORDER MATTERS: every one of these UAs also contains `Chrome/<n>`, so the specific token
# is tried first and plain Chrome is the fallthrough.
_UA_TOKENS = [
    # browser    UA token regex                     preset family
    ("edge",     r"Edg(?:A|iOS)?/(\d+)",            "edge"),
    ("opera",    r"OPR/(\d+)",                      "opera"),
]


def browser_of(cond: dict) -> str:
    """`brave` · `edge` · `opera` · `chrome`, from the capture alone.

    Brave is first because Brave's UA is byte-identical to Chrome's -- it carries no `Brave/` token
    on purpose -- so the only tell is `navigator.brave.isBrave()`, which `collect.js` records into
    `conditions.isBrave`. Edge and Opera go the other way: they announce themselves in the UA and
    have no `navigator.<name>` probe, so the UA token is the whole answer."""
    if cond.get("isBrave") is True:
        return "brave"
    ua = cond.get("ua", "")
    for name, token, _family in _UA_TOKENS:
        if re.search(token, ua):
            return name
    return "chrome"


def product_major(cond: dict) -> int:
    """The captured browser's OWN major version, which is not always its Chromium major.

    Edge 151 is `Chrome/151 Edg/151` and Brave 151 is `Chrome/151`, so for those two the numbers
    coincide and nothing here is visible. Opera 135 is `Chrome/151 OPR/135`: its product version is
    135 while its Chromium is 151, and every artefact that names a version has to say WHICH one it
    means. This is the product's; `chrome_major` is the engine's."""
    ua = cond.get("ua", "")
    for name, token, _family in _UA_TOKENS:
        if browser_of(cond) == name:
            m = re.search(token, ua)
            if m:
                return int(m.group(1))
    return chrome_major(ua)


def _wreq_util_presets() -> list:
    """Every TLS preset the PINNED wreq-util can emit, read from the pinned crate itself.

    NOT A LIST TYPED HERE. The constant this replaced was one browser's name; a hand list of three
    families' ceilings would be the same defect with more rows, and it would rot the first time the
    pin moves -- which is precisely the failure `emulation_for` was rewritten to stop (its
    hand-written match had 40 arms for the library's 41 and silently lacked `chrome_110`).

    The crate declares its presets as `Variant => ("name", <fn>)` rows in `src/emulate.rs`, and
    `transport::profile_names()` reads the same rows through serde at runtime, so this parse and the
    engine agree by construction. `Cargo.lock` pins the version, so the file read here is the file
    that will be compiled.

    IT FAILS CLOSED, like the function it mirrors. A fallback would put some other browser's
    ClientHello under this persona's UA, which is the entire defect class the transport refuses to
    participate in -- so a registry this cannot read is a promotion that does not happen."""
    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    m = re.search(r'\[\[package\]\]\nname = "wreq-util"\nversion = "([^"]+)"', lock)
    if not m:
        raise SystemExit("promote: Cargo.lock pins no wreq-util; cannot tell which presets exist")
    version = m.group(1)
    home = pathlib.Path(os.environ.get("CARGO_HOME") or (pathlib.Path.home() / ".cargo"))
    candidates = sorted((home / "registry" / "src").glob(f"*/wreq-util-{version}/src/emulate.rs"))
    if not candidates:
        raise SystemExit(
            f"promote: cannot find wreq-util {version}'s source under {home}/registry/src to read its "
            f"preset table. Run `cargo fetch` first. Refusing to guess a TLS preset: an unresolvable "
            f"name stops the engine at boot, and a WRONG one is the incoherence this rig exists to "
            f"prevent.")
    text = candidates[0].read_text(encoding="utf-8")
    # EVERY name, not just the ones shaped `<family>_<major>`. `safari_16.5`, `safari_ios_17.4.1` and
    # `okhttp_3.9` carry dotted minor versions, and a pattern that quietly skipped them would make the
    # count below agree with a smaller table than the library's -- the same silent-shrink failure
    # `profile_names()` guards against by comparing its length to `Profile::VARIANTS`.
    presets = re.findall(r'=>\s*\("([a-z0-9_.]+)"', text)
    rows = len(re.findall(r'=>\s*\("', text))
    if not presets or len(presets) != rows:
        raise SystemExit(
            f"promote: parsed {len(presets)} preset names out of {rows} table rows in "
            f"{candidates[0]}. wreq-util changed the shape of its preset table; fix this parse rather "
            f"than letting it silently select from a subset.")
    return presets


def tls_preset(cond: dict) -> str:
    """The newest preset the pinned library ships for THIS capture's browser, at or below its version.

    Three rules, and each one is a fact about the capture rather than a preference:

      * FAMILY comes from the browser. Brave maps to `chrome` because Brave ships Chromium's network
        stack unmodified and really does emit Chrome's ClientHello -- that is a measured property of
        Brave, not a convenience. Edge maps to `edge`, Opera to `opera`.
      * VERSION is compared inside the family, against the browser's OWN major, because each family's
        presets are numbered in that product's versioning. `opera_131` is Opera 131, whose Chromium
        is not 131; comparing it to Opera's Chromium major would pick the wrong preset for a reason
        that has nothing to do with the wire.
      * AT OR BELOW, never above. A newer preset than the browser is a wire from a future version.

    What it CANNOT do is invent a preset. wreq-util 0.2 stops at chrome_149 / edge_148 / opera_131
    and these browsers are Chrome 150, Brave 151, Edge 151 and Opera 135, so EVERY bundle this rig
    promotes today declares a wire older than its UA. That gap is recorded as the `tls-version`
    known_incoherence, measured for Chrome by L7d and stated as unmeasured for the others."""
    family = {"brave": "chrome"}.get(browser_of(cond), browser_of(cond))
    mine = product_major(cond)
    # `<family>_<major>` exactly: `safari_16_5`'s second component is a minor version and this
    # comparison is on majors, so a name with one is not a candidate for a Chromium-family capture.
    in_family = sorted((int(m.group(1)), name)
                       for name in _wreq_util_presets()
                       for m in [re.fullmatch(rf"{re.escape(family)}_(\d+)", name)] if m)
    if not in_family:
        raise SystemExit(
            f"promote: the pinned wreq-util ships no {family}_* preset at all, so this capture's wire "
            f"cannot be declared honestly. Stamping another family's preset is the lie this function "
            f"was written to end.")
    at_or_below = [name for num, name in in_family if num <= mine]
    return at_or_below[-1] if at_or_below else in_family[0][1]


def suggest_id(cond: dict) -> str:
    ua = cond.get("ua", "")
    browser = browser_of(cond)
    at = cond.get("capturedAt") or ""
    when = at[:10].replace("-", "")
    # The PRODUCT major, not the Chromium one: a directory called `opera-151-...` would name a
    # version of Opera that does not exist.
    base = f"{browser}-{product_major(cond)}-{os_family(ua)}-{when}"
    # A second session on the same day is a second bundle, not a rewrite of the first: the id
    # gains the hour and minute so both can exist and the manifests say which is which.
    if (BUNDLES / base).exists():
        return base + "T" + at[11:16].replace(":", "")
    return base


def toml_str(s: str) -> str:
    """A TOML basic string, or a multi-line one when the text has newlines."""
    if "\n" in s:
        return '"""\n' + s.replace('"""', '\\"\\"\\"') + '"""'
    return json.dumps(s)


def known_incoherences(cap: dict, cond: dict) -> list[dict]:
    """Only entries this capture can JUSTIFY. Each carries a reason and a fix, because
    `Bundle::self_check` rejects one without them -- an exception with no fix path is a defect being
    laundered, and the type system says so."""
    out = []
    ua = cond.get("ua", "")

    farbling = (cond.get("farbling") or {}).get("verdict")
    if farbling and "on" in str(farbling):
        out.append(dict(
            id="render-farbling", layers="capture <-> render reference",
            detail=f"This capture was taken with fingerprint protection ON (verdict: {farbling}), so "
                   "its canvas and audio readbacks are perturbed, while the committed pixel "
                   "calibration target was captured raw-core.",
            why="The calibration target measures how close our rasteriser is to the browser's, which "
                "is a question about the rasteriser, so raw is right for THAT. It is the wrong "
                "target for what this persona presents.",
            fix="Record both, and let the bundle say which one this persona presents."))

    # Everything below is about artifacts this promotion does NOT replace. They are listed because a
    # bundle whose manifest omits a foreign artifact fails its own self-check.
    for role, _path, from_capture in ARTIFACTS:
        if from_capture:
            continue
        out.append(dict(
            id=f"{role}-inherited", layers=f"capture <-> {role}",
            detail=f"The `{role}` artifact was not re-recorded by this promotion; it is still the one "
                   "the previous bundle used, from a different session.",
            why="Promoting the values is one step. Each remaining artifact needs its own emitter to "
                "read this capture instead of the committed file, and those land one at a time.",
            fix=("Emit it from this capture with `capture/emit.py --role %s`, then set "
                 "`matches_capture = true` here and delete this entry." % role
                 if role != "ua-stylesheet" else
                 "No emitter: a UA sheet is mostly conditional (`[type=checkbox i]`, "
                 "`details[open] > summary`, `dialog:not([open])`) and a computed style records one "
                 "element in one state, so the conditional rules vanish and host-resolved values "
                 "like the fontconfig default font take their place. The capture's `uaDefaults` "
                 "block is a cross-check against the committed sheet, not a replacement for it.")))

    # THE WIRE CANNOT CLAIM THE UA'S VERSION, AND THE MANIFEST HAS TO SAY SO. The declared preset is
    # the newest the pinned wreq-util ships for THIS browser's family at or below its version; if the
    # captured browser is newer, that difference is real and the boot audit will refuse to start
    # without an entry for it. Emitting it here is what keeps a promoted bundle bootable without
    # anyone hand-editing the file it was generated into.
    #
    # TWO comparisons, because two different things can disagree and the declared entry has to cover
    # whichever the audit finds. `coherence::audit` parses the preset's trailing integer and compares
    # it to the UA's CHROME major -- so `opera_131` under `Chrome/151 OPR/135` is flagged on 131 vs
    # 151 even though the meaningful comparison is 131 vs Opera's own 135. Both are listed below.
    preset = tls_preset(cond)
    preset_major = int(preset.rsplit("_", 1)[-1])
    mine, chromium = product_major(cond), chrome_major(ua)
    if preset_major != chromium or preset_major != mine:
        browser = browser_of(cond)
        family = preset.rsplit("_", 1)[0]
        # THE SECOND DIFFERENCE, AND IT WAS ONLY EVER OPERA'S. Quoted here rather than in every
        # entry, because a generator that copied it onto a Chrome bundle would be fabricating a
        # measurement -- the same class of defect the old `NOT MEASURED` branch existed to prevent.
        alps = ""
        if family == "opera":
            alps = (
                " This family carried a SECOND, independent difference until FP1: `opera_131` sent "
                "the SUPERSEDED ALPS codepoint 0x4469 where every browser measured sends 0x44cd, "
                "which put its third component at 02713d6af862 rather than the other presets' "
                "d8a2da3f94cd. `client_for` now sets `alps_use_new_codepoint` for every preset. Real "
                "Opera 135's ClientHello is byte-for-byte Chrome's, so that difference bought a "
                "distinction no shipping Opera has."
            )
        out.append(dict(
            id="tls-version", layers="UA <-> ClientHello",
            detail=(
                f"THE WIRE HALF OF THIS ENTRY IS CLOSED (PARITY FP1, 2026-09-19); WHAT REMAINS IS "
                f"THE PRESET'S NAME. The capture is {browser.capitalize()} {mine} on Chromium "
                f"{chromium} and the wire is the `{preset}` preset -- the newest the pinned "
                f"wreq-util ships in the `{family}` family at or below {mine}. MEASURED by reading "
                f"the ClientHello off a loopback TLS sink, not inferred: "
                f"`plugins/korvus-transport/tests/clienthello.rs`. This engine on `{preset}` emits "
                f"JA4 {TLS_FAMILY_JA4} with signature_algorithms {TLS_FAMILY_SIGALGS} -- "
                f"byte-identical, in both hashed components, to what real Brave 151.1.93.138, Google "
                f"Chrome 150.0.7871.46, Microsoft Edge 151.0.4129.93 and Opera 135.0.5973.133 "
                f"put on that same sink.{alps} "
                f"WHAT THIS ENTRY NOW RECORDS IS NOT A DIFFERENCE IN THE BYTES. `coherence::audit` "
                f"derives the `UA<->TLS.profile` finding from the preset's trailing integer "
                f"({preset_major}) against the UA's Chrome major ({chromium}) -- that is, from the "
                f"NAME -- and the name cannot be changed: wreq-util 0.2 ships no `{family}` preset "
                f"above {preset_major}, and `transport::emulation_for` refuses a preset the pinned "
                f"library cannot emit rather than substituting another browser's ClientHello. "
                f"DELETING THIS ENTRY WAS TRIED AND MEASURED on 2026-09-19 and it does not work: "
                f"with it gone, `Engine::assemble` refuses to boot the bundle."),
            why=(
                f"The pinned wreq-util 0.2 ships chrome_101..149, edge_101..148 and opera_116..131, "
                f"and this browser is {browser.capitalize()} {mine}. The preset is chosen from the "
                f"browser's OWN family -- Brave maps to chrome because Brave ships Chromium's "
                f"network stack unmodified -- so what remains is a version gap, not a family one. "
                f"THE BYTES THAT GAP USED TO COST ARE NO LONGER PAID. "
                f"`korvus-transport::client_for` prepends the three ML-DSA sigalg NAMES "
                f"(mldsa44:mldsa65:mldsa87) to the preset's own `sigalgs_list`, keeping the "
                f"preset's tail in the preset's own order, and sets `alps_use_new_codepoint`; "
                f"neither reads the bundle, so the engine still never asks which browser it is "
                f"wearing. That works only against a BoringSSL that registers those algorithms in "
                f"libssl, which the workspace `[patch.crates-io]` rev pin on btls/btls-sys/"
                f"tokio-btls provides -- upstream merged the registration on 2026-04-23 "
                f"(4a3cda40b96), three days after btls-sys 0.5.6 was published. The pin ALONE moves "
                f"nothing (measured: upstream registers ML-DSA for signing, not in the offer "
                f"default), and the hunk alone fails at Client::build; they are one change."),
            fix=(
                f"NOTHING IN THE WIRE IS LEFT TO FIX -- re-measure before assuming otherwise: "
                f"`the_four_presets_emit_the_browser_familys_own_ja4` in clienthello.rs asserts "
                f"{TLS_FAMILY_JA4} for chrome_149, edge_148 and opera_131 alike, asserts each ML-DSA "
                f"codepoint is present and asserts the current ALPS codepoint; "
                f"`the_post_quantum_key_share_is_still_offered` asserts the ML-KEM key share 0x11ec "
                f"survived the dependency move. WHAT CLOSES THE ENTRY ITSELF is one of two things, "
                f"neither of them a wire change: (a) a wreq-util release carrying a `{family}` "
                f"preset at {mine} or above, so the preset's NAME agrees with the UA and the audit "
                f"stops firing; or (b) `coherence::audit` learning to compare the wire it can "
                f"MEASURE rather than the integer in the preset's name -- the honest fix, and a "
                f"korvus-kernel change rather than a persona one, because the audit has no socket. A "
                f"capture at a version the preset reproduces exactly would also close it.")))

    # This engine has no GPU path: the strings are replayed from the capture and the pixels come from
    # its own software rasteriser. True on every host, so it is not conditioned on one.
    out.append(dict(
        id="webgl-pixels", layers="deviceOracle <-> rasteriser",
        detail="WebGL strings and getParameter values are replayed from this capture while the pixels "
               "are produced by this engine's own software GLES rasteriser.",
        why="There is no GPU path; docs/RENDER-SPINE.md records the same gap and does not claim otherwise.",
        fix="device-oracle render (relay to the real device), or a rasteriser bound to the claimed GPU."))

    if os_family(ua) == "macos" and sys.platform.startswith("linux"):
        out.append(dict(
            id="fonts-host", layers="persona <-> host",
            detail="The persona claims macOS while text is rasterised through this host's fontconfig.",
            why="Serving the persona's fonts needs a font set the render plugin can be pointed at, "
                "which does not exist yet.",
            fix="Give the bundle a font set and gate skia's FontMgr on it."))
    return out



# The `paramsByNum` re-key: `getParameter(gl.MAX_TEXTURE_SIZE)` arrives as a NUMBER, and the capture
# records params by NAME, so the two have to be joined through the WebGL constant table. The
# authoritative table is the one the generated shells expose, harvested from the surface baseline --
# which the capture also carries, so the join needs nothing outside this file.
def _gl_name_to_num(cap):
    out = {}
    ifaces = ((cap.get("surfaces") or {}).get("window") or {}).get("interfaces") or {}
    for iface in ("WebGLRenderingContext", "WebGL2RenderingContext"):
        rec = ifaces.get(iface) or {}
        for m in (rec.get("statics") or []) + (rec.get("members") or []):
            if isinstance(m, dict) and m.get("kind") == "value" and isinstance(m.get("value"), (int, float)):
                out.setdefault(m["name"], int(m["value"]))
    return out


def build_values(cap):
    """The values a persona serves, assembled from the capture's own blocks.

    The collector records one device across several top-level blocks -- `persona` holds the navigator
    scalars, `webgl` and `render` hold what a fingerprinter reads off a GPU, `fonts` the text metrics,
    `environment.intl` the locale. The loader wants them in ONE object.

    The first version of this promoter copied `persona` verbatim and shipped a bundle whose
    `deviceOracle` was empty and whose font list had nothing in it -- a strict downgrade from the
    bundle it was meant to replace, produced from a capture that contained every one of those values.
    Assembling here is the difference between promoting a capture and promoting a fifth of one."""
    v = dict(cap.get("persona") or {})

    # A `navigator.deviceMemory` QUANTISER WAS HERE, AND IT WAS OVERRIDING A CORRECT MEASUREMENT.
    #
    # It capped the captured value at 8 and rounded it into {0.25, 0.5, 1, 2, 4, 8}, on the reasoning
    # that "the Device Memory spec caps at 8 and no Chrome on earth reports 32". Every capture this
    # repo holds recorded 32; all four bundles therefore served 8 with a `_deviceMemoryNote` saying
    # so. Both the note and the branch are gone, and were deleted rather than adjusted, because a
    # correction cannot be trusted to a sentence that exists to justify discarding a reading.
    #
    # MEASURED, 2026-09-19: Brave 151.1.93.138 with fingerprinting explicitly OFF answers
    # **32** on this 60 GB box, in 16 of 16 (session, origin) cells, and shields-default Brave draws
    # `deviceMemory` uniformly from {4, 8, 16, 32} over 152 cells -- a support whose top is 32, which
    # a browser that capped at 8 could not produce. Chromium CL 7410045 raised the desktop cap from 8
    # to 32 and the floor from 0.25 to 2, on by default since Chrome 147, and rounds to the NEAREST
    # power of two; CL 7957919 deleted the flag and the old branch. So the spec sentence this code
    # was written from stopped describing Chromium four majors before the oldest bundle here.
    #
    # The promoter's job is to carry what the browser said. A value it does not like is a finding,
    # and a finding goes in `known_incoherence` where somebody has to read it -- never into a silent
    # rewrite of the capture.
    env = cap.get("environment") or {}
    intl = env.get("intl") or {}
    if intl:
        ro = intl.get("resolvedOptions") or {}
        v["intl"] = {
            "timeZone": intl.get("timeZone") or ro.get("timeZone") or "",
            "locale": intl.get("locale") or ro.get("locale") or "",
            "timeZoneName": intl.get("timeZoneName") or "",
            "offsetMinutes": intl.get("offsetMinutes"),
        }
    fonts = cap.get("fonts") or {}
    if fonts:
        v["fonts"] = fonts
    # The device's speech voices (Chrome: its Google voices; Brave: none). The computed-style
    # names are a SURFACE fact and go to computed-style.json (`emit.py --role computed-style`).
    if isinstance(env.get("speechVoices"), list):
        v["speech"] = {"voices": env["speechVoices"]}

    wg1 = (cap.get("webgl") or {}).get("webgl") or {}
    meta = ((cap.get("render") or {}).get("webgl") or {}).get("metadata") or {}
    name2num = _gl_name_to_num(cap)
    params_by_num = {}
    for name, val in (wg1.get("params") or {}).items():
        num = name2num.get(name)
        if num is not None:
            params_by_num[str(num)] = val
    wg2 = (cap.get("webgl") or {}).get("webgl2") or {}
    params_by_num_v2 = {}
    for name, val in (wg2.get("params") or {}).items():
        num = name2num.get(name)
        if num is not None and name not in ("VERSION", "SHADING_LANGUAGE_VERSION"):
            params_by_num_v2[str(num)] = val
    for num, val in (wg1.get("extParams") or {}).items():
        params_by_num.setdefault(str(num), val)
    for num, val in (wg2.get("extParams") or {}).items():
        params_by_num_v2.setdefault(str(num), val)
    canvas = (cap.get("render") or {}).get("canvas2d") or {}
    audio = (cap.get("render") or {}).get("audio") or {}
    oracle = {
        "webgl": {
            "unmaskedVendor": wg1.get("unmaskedVendor") or meta.get("UNMASKED_VENDOR") or "",
            "unmaskedRenderer": wg1.get("unmaskedRenderer") or meta.get("UNMASKED_RENDERER") or "",
            "vendor": meta.get("VENDOR") or "",
            "renderer": meta.get("RENDERER") or "",
            "versionV1": meta.get("VERSION") or "",
            "shadingV1": meta.get("SHADING_LANGUAGE_VERSION") or "",
            # THE WEBGL2 HALF. The engine reads `versionV2` / `shadingV2` for a WebGL2 context's
            # VERSION and SHADING_LANGUAGE_VERSION, and the capture records both -- and this promoter
            # emitted neither, so `gl2.getParameter(gl2.VERSION)` answered `undefined` from a bundle
            # whose raw capture said "WebGL 2.0 (OpenGL ES 3.0 Chromium)". Likewise the 50 params
            # only a WebGL2 context has (MAX_3D_TEXTURE_SIZE, MAX_DRAW_BUFFERS, ...): a fingerprinter's
            # parameter sweep is mostly those, and they were all `undefined` too.
            "versionV2": (wg2.get("params") or {}).get("VERSION") or "",
            "shadingV2": (wg2.get("params") or {}).get("SHADING_LANGUAGE_VERSION") or "",
            "paramsByNum": params_by_num,
            "paramsByNumV2": params_by_num_v2,
            "extensions": wg1.get("extensions") or [],
            "extensionsV2": wg2.get("extensions") or [],
            "contextAttributes": wg1.get("contextAttributes") or {},
            "precision": wg1.get("precision") or {},
            # Extension limits (asked after enabling every supported extension) by number, merged into
            # the two parameter tables a context reads, and WebGL 2's per-format sample counts.
            "internalformatSamples": wg2.get("internalformatSamples") or {},
        },
        "canvas": {"dataUrl": canvas.get("dataURL") or ""},
        "audio": {
            "sampleRate": ((cap.get("render") or {}).get("system") or {}).get("audioSampleRate")
                          or (audio.get("sampleRate") if isinstance(audio, dict) else None),
            "renderedHash": audio.get("renderedHash"),
            "classicSum": audio.get("classicSum"),
        },
    }
    v["deviceOracle"] = oracle
    return v


def render_manifest(cid: str, cap: dict, cond: dict, ki: list[dict]) -> str:
    ua = cond.get("ua", "")
    L = []
    L.append("# PERSONA BUNDLE — generated by plugins/korvus-persona/capture/promote.py. Edit `known_incoherence`\n"
             "# entries as gaps close; do not hand-edit `[capture]`, which is stamped from the capture\n"
             "# itself so it cannot claim a session nobody recorded.\n")
    L.append("[capture]")
    L.append(f"id          = {toml_str(cid)}")
    L.append('engine      = "chromium"')
    L.append(f'browser     = {toml_str(browser_of(cond))}')
    # `version` IS THE CHROMIUM MAJOR, and stays that way: every consumer compares it to a Chromium
    # major (`coherence::audit` against the UA's `Chrome/<n>`, P4 against the oracle's Chromium
    # version). `product_version` is the browser's own, which differs for exactly one member of this
    # family -- Opera 135 is Chromium 151 -- and without it a reader would think a manifest saying
    # `version = 151` contradicted the directory called `opera-135-...`.
    L.append(f"version     = {chrome_major(ua)}")
    L.append(f"product_version = {product_major(cond)}")
    L.append(f'os          = {toml_str(os_family(ua))}')
    L.append('cpu         = ""')
    # A capture taken through a page the user opened is headed by construction; `webdriver` says
    # whether it was automated, which is a different and equally important fact.
    L.append("headless    = false")
    farb = (cond.get("farbling") or {}).get("verdict") or "unknown"
    L.append(f"farbling    = {toml_str(str(farb))}")
    L.append(f'captured_at = {toml_str(cond.get("capturedAt", ""))}')
    L.append(f'captured_on = {toml_str(cond.get("href", ""))}')
    L.append(f"ua = {toml_str(ua)}")
    L.append(f"\n# Recorded so a later reader can tell a clean capture from a contaminated one.")
    L.append(f'# webdriver={cond.get("webdriver")}  devtools_api={cond.get("hasDevToolsApi")}  '
             f'secure_context={cond.get("isSecureContext")}')
    L.append("")
    for role, path, from_capture in ARTIFACTS:
        L.append("[[artifact]]")
        L.append(f"role = {toml_str(role)}")
        L.append(f"path = {toml_str(path.format(id=cid))}")
        L.append(f'provenance = {toml_str(cid if from_capture else "inherited from a previous bundle")}')
        L.append(f"matches_capture = {str(from_capture).lower()}")
        L.append("")
    L.append("[transport]")
    # Derived from the capture, not from a constant. See `tls_preset()`: the family comes from the
    # browser and the version is the newest the pinned wreq-util ships at or below the browser's own.
    L.append('emulation = "%s"' % tls_preset(cond))
    L.append("")
    for k in ki:
        L.append("[[known_incoherence]]")
        L.append(f"id = {toml_str(k['id'])}")
        L.append(f"layers = {toml_str(k['layers'])}")
        L.append(f"detail = {toml_str(k['detail'])}")
        L.append(f"why = {toml_str(k['why'])}")
        L.append(f"fix = {toml_str(k['fix'])}")
        L.append("")
    return "\n".join(L) + "\n"


def _leaf_keys(obj, prefix="") -> set:
    """Dotted paths of every leaf, for a --refresh diff. Lists count as one leaf each."""
    if isinstance(obj, dict):
        out = set()
        for k, v in obj.items():
            out |= _leaf_keys(v, f"{prefix}.{k}" if prefix else str(k))
        return out
    return {prefix}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("capture")
    ap.add_argument("--id", default="", help="capture id (default: derived from the capture itself)")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--refresh", action="store_true",
                    help="rewrite an EXISTING bundle's values.json from the same capture; the manifest "
                         "is left alone")
    ap.add_argument("--values-only", action="store_true",
                    help="write only values.json + manifest.toml; by default the whole directory is "
                         "written (emit.py --all runs on the new bundle, so every artifact the manifest "
                         "names exists and comes from this capture)")
    a = ap.parse_args()

    cap = json.load(open(a.capture))
    cond = cap.get("conditions") or {}
    if not cond.get("ua"):
        print("promote: this capture has no `conditions.ua` — it is not a collect.js capture",
              file=sys.stderr)
        return 2

    cid = a.id or suggest_id(cond)
    out = BUNDLES / cid
    if out.exists() and not a.dry_run and not a.refresh:
        print(f"promote: {out} already exists. A bundle records ONE session; re-promoting over it "
              f"would rewrite history other artifacts are keyed to. Use --id for a new one, or "
              f"--refresh to re-derive values.json from this same capture.",
              file=sys.stderr)
        return 1

    values = build_values(cap)
    if not isinstance(values, dict) or "nav" not in values:
        print("promote: the capture has no `persona` block with a `nav` — nothing to serve",
              file=sys.stderr)
        return 2

    # --refresh: THE SAME SESSION, PROMOTED FURTHER. `values.json` is derived from the capture; the
    # manifest records the session and what each artifact's provenance is, and `emit.py` flips
    # `matches_capture` there as artifacts land. When this promoter learns to carry another field
    # (the WebGL2 block was one), the bundle must pick it up WITHOUT rewriting the manifest -- a full
    # re-promote would reset every flip emit.py has made and re-declare the artifacts inherited.
    # So this rewrites values.json alone, and only for the capture the bundle was made from.
    if a.refresh:
        if not out.exists():
            print(f"promote: --refresh needs an existing bundle at {out}", file=sys.stderr)
            return 1
        manifest_text = (out / "manifest.toml").read_text()
        recorded_at = None
        for line in manifest_text.splitlines():
            if line.startswith("captured_at"):
                recorded_at = line.split("=", 1)[1].strip().strip('"')
        if recorded_at != cond.get("capturedAt", ""):
            print(f"promote: --refresh refused: the bundle records captured_at={recorded_at!r} and this "
                  f"capture is {cond.get('capturedAt')!r}. A different session is a new bundle.",
                  file=sys.stderr)
            return 1
        before = json.loads((out / "values.json").read_text())
        added = sorted(_leaf_keys(values) - _leaf_keys(before))
        removed = sorted(_leaf_keys(before) - _leaf_keys(values))
        print(f"  refresh        {cid}: +{len(added)} keys, -{len(removed)} keys, manifest untouched")
        for k in added[:12]:
            print(f"                 + {k}")
        if removed:
            for k in removed[:12]:
                print(f"                 - {k}")
        if not a.dry_run:
            (out / "values.json").write_text(json.dumps(values, indent=1, ensure_ascii=False) + "\n")
            print(f"\n  wrote {out}/values.json")
        return 0

    ki = known_incoherences(cap, cond)
    manifest = render_manifest(cid, cap, cond, ki)

    print(f"  capture id     {cid}")
    print(f"  browser        {browser_of(cond)} {product_major(cond)} on Chromium {chrome_major(cond['ua'])}")
    print(f"  tls preset     {tls_preset(cond)}   (derived from the capture, not a constant)")
    print(f"  ua             {cond['ua'][:78]}")
    print(f"  captured_at    {cond.get('capturedAt')}")
    print(f"  farbling       {(cond.get('farbling') or {}).get('verdict')}")
    print(f"  webdriver      {cond.get('webdriver')}   devtools={cond.get('hasDevToolsApi')}")
    _o = values.get("deviceOracle", {})
    print(f"  nav fields     {len(values.get('nav', {}))}")
    print(f"  webgl params   {len(_o.get('webgl', {}).get('paramsByNum', {}))} by number, "
          f"{len(_o.get('webgl', {}).get('extensions', []))} extensions")
    print(f"  renderer       {_o.get('webgl', {}).get('unmaskedRenderer', '')[:60]}")
    print(f"  fonts          {(values.get('fonts') or {}).get('count', 0)} present")
    print(f"  canvas         {len(_o.get('canvas', {}).get('dataUrl', ''))} chars of PNG data URL")
    print(f"  incoherences   {len(ki)}  ({', '.join(k['id'] for k in ki)})")
    if a.dry_run:
        print("\n--- manifest.toml (dry run) ---")
        print(manifest)
        return 0

    out.mkdir(parents=True)
    (out / "values.json").write_text(json.dumps(values, indent=1, ensure_ascii=False) + "\n")
    (out / "manifest.toml").write_text(manifest)
    print(f"\n  wrote {out}/values.json + manifest.toml")
    if not a.values_only:
        # THE WHOLE DIRECTORY: every artifact the manifest names, from this capture.
        #
        # AND ITS DERIVED RUST GOES NOWHERE NEAR THE TREE. `emit.py` compiles one of the bundle files
        # into Rust as well -- `core/korvus-page/src/font_table.rs` from `fonts/metrics.json` -- and it
        # is derived from the SHIPPED bundle, not from whichever bundle is being promoted. Left alone,
        # promoting an Edge capture rewrites the engine's font table from Edge while the shipped
        # persona is still Brave. `scripts/sync.sh --check` does catch it, one gate later and with
        # nothing in the failure that says where it came from.
        #
        # `codecs.json` was the second such file; the engine reads every bundle's own
        # grid now, so promoting a capture changes no Rust at all on that axis.
        #
        # L7a hit this and redirected by hand; a hand step in a promoter is the next thing to forget,
        # so the redirect is the promoter's now. `sync.sh` is what writes those two files, from the
        # shipped bundle, and it is already the next line this prints.
        import subprocess, tempfile
        env = dict(os.environ)
        env["KORVUS_SYNC_OUT_ROOT"] = tempfile.mkdtemp(prefix="korvus-promote-derived-")
        subprocess.run([sys.executable, str(ROOT / "plugins/korvus-persona/capture/emit.py"),
                        a.capture, "--all", "--bundle", str(out)], check=True, env=env)
    print(f"\n  wrote {out}/")
    print("  Next: point korvus-persona's include_str! at it (the shipped bundle), run scripts/sync.sh,")
    print("  and the gate.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
