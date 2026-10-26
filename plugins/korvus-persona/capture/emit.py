#!/usr/bin/env python3
"""Write the committed artifacts from one capture — what closes a `<role>-inherited` entry.

    plugins/korvus-persona/capture/emit.py <capture.json> --role render-reference
    plugins/korvus-persona/capture/emit.py <capture.json> --all --dry-run

WHY THIS IS SEPARATE FROM `promote.py`.

Promoting writes the bundle: the values a persona serves, and a manifest saying where every other
artifact came from. Emitting writes those OTHER artifacts. They are different acts with different
blast radii -- promoting a bundle changes what one process claims about itself, while emitting the
surface baseline changes the denominator every conformance number in this repo is measured against.
Keeping them apart is what lets a bundle land first and its artifacts follow one at a time, each
behind its own gate.

EVERY ROLE HERE CLOSES ONE ENTRY. A bundle's manifest lists eight artifacts; seven of them were
inherited from whatever session produced them last, and the count of foreign ones is asserted in a
test so it can only go down. `--role X` writes X from this capture and flips its `matches_capture`
to true; the entry then has nothing left to say and is deleted.
"""
import argparse, json, os, pathlib, re, sys


def _repo_root():
    """Walk up to the workspace root rather than counting `..` hops -- see the note in host.py."""
    d = pathlib.Path(__file__).resolve().parent
    for cand in [d, *d.parents]:
        m = cand / "Cargo.toml"
        if m.exists() and "[workspace]" in m.read_text(encoding="utf-8", errors="ignore"):
            return cand
    raise SystemExit("cannot find the workspace root above %s" % d)


ROOT = _repo_root()
sys.path.insert(0, str(ROOT / "tools/sync"))
import bundle as shipped  # noqa: E402  -- the shipped bundle: which one, and where its artifacts are

# Where each role lives INSIDE a bundle directory (the bundle is one directory, and its
# manifest's `[[artifact]] path`s are these). `promote.py` writes the same names into the manifest.
BUNDLE_FILES = {
    "surface":          "surface.json",
    "worker-surface":   "worker.surface.json",
    "render-reference": "render/reference.json",
    "font-metrics":     "fonts/metrics.json",
    "theme":            "theme.json",
    "computed-style":   "computed-style.json",
    "webrtc-offer":     "webrtc-offer.json",
    "webrtc-capabilities": "webrtc-capabilities.json",
    "css-named-properties": "css-named-properties.json",
    "oracle":           "oracle",
    "codecs":           "codecs.json",
    # WRITTEN BY `farbling.py`, NOT BY AN ENTRY IN `EMITTERS` BELOW — and the split is the point.
    # This table answers "what is in a bundle, and where", which must have exactly one answer;
    # `EMITTERS` answers "which roles this script can write from ONE capture file", and `farbling`
    # is not one of them. A farbling policy is not in any single capture: it is sampled across
    # dozens of fresh sessions, with an unfarbled control run beside it, which is a browser-driving
    # errand `emit.py` has no business growing. So `farbling.py` produces the file and
    # then stamps the manifest through THIS module's `mark_recorded`, so that a bundle's index still
    # has one writer. `--role farbling` is correctly rejected: the emitter for it is another program.
    "farbling":         "farbling.json",
}
# The artifact that also compiles into Rust. The bundle file is the SOURCE; the `.rs` is derived
# from it -- by this script (`--from-bundle`, which scripts/sync.sh runs, and whose `--check` proves the
# tree regenerates from the bundle byte-for-byte). `KORVUS_SYNC_OUT_ROOT` redirects the `.rs` output.
#
# `codecs` WAS HERE, deriving `core/korvus-webapi/src/core/dom/generated_media_codecs.rs`. It could not
# be both a generated `.rs` and per-bundle: four bundles' grids compiled into one `core/` file would
# have put four capture ids into engine source, which is the browser-family literal `scripts/persona_literals.py` forbids
# there. `codecs.json` is now read by the ACTIVE bundle's persona (`plugins/korvus-persona/src/codecs.rs`)
# the way `surface.json` already was, and the generated file is deleted.
#
# `font-metrics` is still derived from the SHIPPED bundle alone, and still differs 0 across all four
# captures -- because all four were taken on this one box. See `codec_grid_live.rs`'s font-metrics gate,
# which fails the day that stops being true, and says what to do about it.
DERIVED = {
    "font-metrics": "core/korvus-page/src/font_table.rs",
}
OUT_ROOT = pathlib.Path(os.environ.get("KORVUS_SYNC_OUT_ROOT", str(ROOT)))


def emit_surface(cap):
    """The window surface: which interfaces and members exist, their order and their descriptors.

    The capture's `surfaces.window` is a SUPERSET of the committed baseline's shape -- same `meta`,
    `globals`, `interfaces`, `singletons`, plus `documentKeys`, `fpToStringNative` and `instances`
    that the console-paste baseline never recorded. So it is written whole rather than narrowed: the
    extra keys are facts the consumers can ignore today and read tomorrow.

    THIS IS THE BIGGEST ONE. Every generated shell, every structural-diff number and the entire
    `present` axis are measured against this file."""
    w = (cap.get("surfaces") or {}).get("window")
    if not w or "interfaces" not in w:
        raise SystemExit("this capture has no surfaces.window")
    return json.dumps(w, indent=1, ensure_ascii=False) + "\n"


def emit_worker_surface(cap):
    w = (cap.get("surfaces") or {}).get("worker")
    if not w or "interfaces" not in w:
        raise SystemExit("this capture has no surfaces.worker")
    return json.dumps(w, indent=1, ensure_ascii=False) + "\n"


def emit_render_reference(cap):
    """The canvas/WebGL/audio calibration target.

    The committed one was captured on macOS with fingerprint protection disabled, and the pixel
    ratchet in `gate-plugin.sh` measures our rasteriser against it -- from Linux. The header of that
    script already says the number is a fact about the host as much as about our code. A reference
    taken on the machine that does the rasterising removes the part of the difference that was never
    ours to close."""
    r = cap.get("render")
    if not r or "canvas2d" not in r:
        raise SystemExit("this capture has no render block")
    return json.dumps(r, indent=1, ensure_ascii=False) + "\n"


def _rs_str(s):
    """A Rust string literal: JSON's escapes are Rust's but for a non-ASCII character, which Rust writes `\\u{..}` (a
    Korean family name, `굴림`); written as itself, the file stays readable."""
    return json.dumps(s, ensure_ascii=False)


def _rs_opt_em(px, size):
    """An `Option<f32>` in ems from a px-at-`size` reading that may be `null`."""
    if px is None:
        return "None"
    return "Some(%s)" % round(float(px) / size, 7)


def _rs_char(c):
    """A Rust `char` literal. `json.dumps` gives a STRING literal, and the kerning table is
    `&[(char, char, f32)]` -- the first emitted table would not compile for exactly that reason."""
    if c == "'":
        return r"'\''"
    if c == "\\":
        return r"'\\'"
    return "'%s'" % c


def emit_font_metrics(cap):
    """`fonts/metrics.json` -- the capture's `fontMetrics` block, verbatim (advances and kerning in px at
    the measured `size`), plus the CARRIED line gaps.

    `line_gap` is NOT measurable from `measureText` -- it is a font-table fact, not a rendering one --
    so the values the bundle already carries move across unchanged (the first bundle derived them from
    a line-box measurement at 2048px), and the file says they were. `font_table.rs` is then DERIVED
    from this file by [`font_table_rs`] (`--from-bundle`, scripts/sync.sh)."""
    fm = cap.get("fontMetrics")
    if not fm:
        raise SystemExit("this capture has no fontMetrics (it predates captureVersion 2)")
    line_gap = {"Arial": 0.0327148, "Times New Roman": 0.0424805, "Courier New": 0.0}
    prev = BUNDLE_DIR / BUNDLE_FILES["font-metrics"] if BUNDLE_DIR else None
    if prev and prev.exists():
        line_gap.update(json.loads(prev.read_text(encoding="utf-8")).get("lineGap") or {})
    # THE AVERAGE AND MAXIMUM CHARACTER WIDTHS come from `layoutMetrics`, which reads
    # them back off text-field layout because no script API names them (see collect.js). Merged
    # into the font they describe, in px at that font's `size` like its advances; `null` is Blink's
    # own answer for a font it reads no usable average from, and is written as such. Only these two
    # keys are taken -- the raw readings stay in the capture, where they can be re-derived.
    layout_fonts = (cap.get("layoutMetrics") or {}).get("fonts")
    if not layout_fonts:
        raise SystemExit("this capture has no layoutMetrics.fonts (it predates captureVersion 5)")
    fm = {fam: dict(f) for fam, f in fm.items()}
    for fam, f in fm.items():
        lf = layout_fonts.get(fam)
        if lf is None:
            raise SystemExit("layoutMetrics has no entry for %r" % fam)
        if float(lf.get("size") or 0) != float(f.get("size") or 0):
            raise SystemExit("layoutMetrics measured %r at a different size than its advances" % fam)
        f["avgCharWidth"] = lf.get("avgCharWidth")
        f["maxCharWidth"] = lf.get("maxCharWidth")
    metrics = {
        "provenance": BUNDLE_ID,
        "note": "The capture's fontMetrics block, verbatim (advances and kerning in px at `size`; "
                "ascent/descent/xheight/zero likewise), with avgCharWidth/maxCharWidth from its "
                "layoutMetrics block (px at `size`; null = Blink reads no usable OS/2 average). "
                "lineGap is CARRIED from the previous capture: measureText cannot report the hhea "
                "line gap.",
        "lineGap": line_gap,
        "fonts": fm,
    }
    return json.dumps(metrics, indent=1, ensure_ascii=False) + "\n"


def _font_face_key(name):
    """A face's key: its first family's name, lower-cased, ASCII letters and digits; a name with none of those (a
    family named only in Hangul) is keyed by its UTF-8 bytes in hex, so two such names never share a key."""
    key = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
    return key or "u" + name.encode("utf-8").hex()


def emit_font_metrics_v2(prev, fcap, fcap_name):
    """`fonts/metrics.json` VERSION 2: the persona's whole font baseline, from a
    `fonts-probe.html` capture merged into the bundle's existing file.

    What a page can read of fonts -- which families are installed, what `sans-serif` is, the default
    family, and the width of any text in any of them, in four styles -- is the PERSONA's, so
    it comes from the persona's own browser: the probe page run in Brave 151 on this box. Kept from the
    previous file, verbatim: `lineGap` (the hhea line gap `measureText` cannot report) and the three
    legacy fonts' `fonts` block with their measured `avgCharWidth`/`maxCharWidth`.

    `faces` holds each distinct face ONCE: families whose four styles measure identically (Arial,
    Helvetica and Liberation Sans are one face on this box) share a key, and `families` / `generics`
    map names onto keys. A face's line gap is its capture's normal line box minus its content box
    (`lineHeights`, at 1000px), except where the previous file carried a precise one; a face whose
    line box is set by FALLBACK glyphs (Noto Color Emoji: 1448 against its own 1172) gets 0."""
    present = list(fcap["present"])
    styles = ("normal", "bold", "italic", "boldItalic")
    lh = fcap.get("lineHeights") or {}
    carried_gap = dict(prev.get("lineGap") or {})

    def style_block(src):
        keep = ("size", "advances", "kern", "extra", "ascent", "descent", "xheight", "capheight", "zero",
                "hangulAscent", "hangulDescent", "covered")
        return {k: src[k] for k in keep if k in src}

    def gap_for(names):
        for n in names:
            if n in carried_gap:
                return carried_gap[n]
        for n in names:
            v = lh.get('"%s"' % n) or lh.get(n)
            if v and "divH" in v and "spanH" in v:
                d = float(v["divH"]) - float(v["spanH"])
                return round(d / 1000.0, 6) if 0 <= d <= 100 else 0.0
        return 0.0

    faces, by_sig, families, generics, names = {}, {}, {}, {}, {}

    def face_for(name, getter):
        blocks = {st: style_block(getter(st)) for st in styles}
        sig = json.dumps(blocks, sort_keys=True)
        if sig not in by_sig:
            key = _font_face_key(name)
            while key in faces:
                key += "-2"
            faces[key] = blocks
            by_sig[sig] = key
        names.setdefault(by_sig[sig], []).append(name)
        return by_sig[sig]

    for fam in present:
        families[fam] = face_for(fam, lambda st, f=fam: fcap["styles"][st]["fonts"][f])
    for gen in fcap["styles"]["normal"].get("generics", {}):
        generics[gen] = face_for(gen, lambda st, g=gen: fcap["styles"][st]["generics"][g])
    # The line gap is looked up by EVERY name a face answers to: Times New Roman's carried gap is the
    # `times` face's, whichever name the face was keyed by.
    for key in faces:
        faces[key] = dict(faces[key], lineGap=gap_for(names[key]))
    default = (fcap.get("domDefault") or {}).get("computedFontFamily", "").strip().strip('"')
    if default not in families:
        raise SystemExit("the capture's default family %r is not among its installed ones" % default)
    out = dict(prev)
    out.update({
        "version": 2,
        "provenance": "%s; fonts: %s" % (BUNDLE_ID, fcap_name),
        "note": prev.get("note", "") + " VERSION 2 adds, from the fonts-probe capture: `default` (the "
                "family a document with no font-family uses), `installed`, `families` and `generics` "
                "(names -> face keys) and `faces` (each distinct face once: four styles of advances, "
                "kerning, sampled non-ASCII advances in `extra`, vertical metrics; px at `size`).",
        "default": default,
        "installed": present,
        "families": families,
        "generics": generics,
        "faces": faces,
    })
    return json.dumps(out, indent=1, ensure_ascii=False) + "\n"


def emit_theme(cap):
    """`theme.json` -- the platform's form-control and scrollbar geometry.

    What the capture's `layoutMetrics` read back off layout: a classic scrollbar's thickness for
    `scrollbar-width: auto` and `thin` (0 for an overlay-scrollbar platform), and a drop-down's
    theme paddings. None of these is a font fact or a script-visible value; each is a property of
    the platform theme the browser draws with, which is why a Windows or macOS persona may answer
    differently and why the engine reads them from the worn bundle rather than from a constant."""
    lm = cap.get("layoutMetrics") or {}
    bar = lm.get("scrollbar") or {}
    menu = lm.get("menuList") or {}
    if "auto" not in bar or "inlinePadding" not in menu:
        raise SystemExit("this capture has no layoutMetrics.scrollbar/menuList (it predates captureVersion 5)")
    for kind in ("auto", "thin", "none"):
        v = bar.get(kind) or {}
        if v.get("vertical") != v.get("horizontal"):
            raise SystemExit("scrollbar-width %s is %r: the two axes disagree, and theme.json has one "
                             "thickness per kind" % (kind, v))
    theme = {
        "provenance": BUNDLE_ID,
        "note": "The capture's layoutMetrics block: `offsetWidth - clientWidth` of an `overflow: scroll` "
                "box per `scrollbar-width`, and an empty `<select>` at `font-size: 0` for the theme's "
                "menu-list paddings. px.",
        "scrollbar": {
            "thickness": bar["auto"]["vertical"],
            "thinThickness": bar["thin"]["vertical"],
            "noneThickness": bar["none"]["vertical"],
        },
        "menuList": {
            "inlinePadding": menu["inlinePadding"],
            "blockPadding": menu["blockPadding"],
        },
    }
    return json.dumps(theme, indent=1, ensure_ascii=False) + "\n"


def emit_computed_style(cap):
    """`computed-style.json` -- the names `getComputedStyle` enumerates, in the browser's order (ACCESS
    2026-10-04): a surface fact (Chrome 150 lists 474, Chromium 151 adds `ruby-overhang`)."""
    names = (cap.get("environment") or {}).get("computedStyleProperties")
    if not isinstance(names, list) or not names:
        raise SystemExit("this capture has no environment.computedStyleProperties (collect.js predates 2026-10-04)")
    out = {
        "provenance": BUNDLE_ID,
        "note": "Array.from(getComputedStyle(document.documentElement)) in the browser's own order.",
        "computedStyleProperties": names,
    }
    return json.dumps(out, indent=1, ensure_ascii=False) + "\n"


def emit_webrtc_offer(cap):
    """`webrtc-offer.json` -- the device's own SDP offer for receive-only audio and video:
    which codecs, payload types, header extensions and feedback a section lists is the build's."""
    sdp = (cap.get("environment") or {}).get("webrtcOffer")
    if not isinstance(sdp, str) or "\r\nm=audio " not in sdp or "\r\nm=video " not in sdp or not sdp.endswith("\r\n"):
        raise SystemExit("this capture has no environment.webrtcOffer (collect.js predates 2026-10-07)")
    out = {
        "provenance": BUNDLE_ID,
        "note": "(await new RTCPeerConnection().createOffer({offerToReceiveAudio: 1, offerToReceiveVideo: 1})).sdp, "
                "verbatim, one line of the SDP per entry (each ends in CRLF there). The ICE credentials, the "
                "fingerprint and the session id are that one connection's; the engine writes its own connection's "
                "in their place.",
        # One entry per line, so an edit is a reviewable diff (tools/parity/tests/test_bundle_json_is_reviewable.py).
        "sdpLines": sdp[:-2].split("\r\n"),
    }
    return json.dumps(out, indent=1, ensure_ascii=False) + "\n"


def format_webrtc_capabilities(caps, provenance):
    """The `webrtc-capabilities.json` text for `caps` ({"RTCRtpReceiver.audio": RTCRtpCapabilities, ...}): every
    codec and header extension one entry, a JSON object with its keys sorted -- the order a WebIDL dictionary's
    members reach the page in -- so an edit is a reviewable diff (tools/parity/tests/test_bundle_json_is_reviewable.py)."""
    sides = {}
    for key in ("RTCRtpReceiver.audio", "RTCRtpReceiver.video", "RTCRtpSender.audio", "RTCRtpSender.video"):
        v = caps[key]
        sides[key] = {"codecs": [json.dumps(c, sort_keys=True) for c in v["codecs"]],
                      "headerExtensions": [json.dumps(e, sort_keys=True) for e in v["headerExtensions"]]}
    out = {
        "provenance": provenance,
        "note": "getCapabilities(kind) for each side, verbatim: one codec / header extension per entry, each a JSON "
                "object (keys sorted), in the browser's order",
        "capabilities": sides,
    }
    return json.dumps(out, indent=1, ensure_ascii=False) + "\n"


def emit_webrtc_capabilities(cap):
    """`webrtc-capabilities.json` -- RTCRtpReceiver/RTCRtpSender.getCapabilities for audio and video
    (2026-10-08): libwebrtc's decoder and encoder factories, the device's."""
    caps = (cap.get("environment") or {}).get("webrtcCapabilities")
    if not isinstance(caps, dict) or len(caps) != 4 or not all(isinstance(v, dict) and "codecs" in v for v in caps.values()):
        raise SystemExit("this capture has no environment.webrtcCapabilities (collect.js predates 2026-10-08)")
    return format_webrtc_capabilities(caps, BUNDLE_ID)


def format_css_named_properties(names, provenance):
    """The `css-named-properties.json` text for `names` (a CSSStyleDeclaration's named-property enumeration, in the
    browser's order): one name per entry, so an edit is a reviewable diff."""
    out = {
        "provenance": provenance,
        "note": "Object.getOwnPropertyNames(document.createElement('div').style), the indices left out: the JS name of "
                "every CSS property and alias the build exposes, in the order its named-property enumerator lists them",
        "names": list(names),
    }
    return json.dumps(out, indent=1, ensure_ascii=False) + "\n"


def emit_css_named_properties(cap):
    """`css-named-properties.json` -- a CSSStyleDeclaration's named-property enumeration."""
    names = (cap.get("environment") or {}).get("cssNamedProperties")
    if not isinstance(names, list) or not names or not all(isinstance(n, str) for n in names):
        raise SystemExit("this capture has no environment.cssNamedProperties (collect.js predates 2026-10-08)")
    return format_css_named_properties(names, BUNDLE_ID)


# The script classes whose advance is ONE number per face and style on this capture (measured: every
# Hangul syllable, jamo, CJK ideograph, kana and CJK punctuation sample of a face-style has the same
# advance), in the order `FontMetrics::class_advances` holds them. Emoji are kept per character -- four
# face-styles give two different emoji advances -- and the class value is the fallback for an emoji the
# probe did not sample.
_FONT_CLASSES = ("hangul", "jamo", "cjk", "kana", "cjkpunct", "emoji")


def _font_class(ch):
    if len(ch) != 1:
        return "seq"
    o = ord(ch)
    if 0xAC00 <= o <= 0xD7A3:
        return "hangul"
    if 0x1100 <= o <= 0x11FF or 0x3130 <= o <= 0x318F:
        return "jamo"
    if 0x4E00 <= o <= 0x9FFF or 0x3400 <= o <= 0x4DBF:
        return "cjk"
    if 0x3040 <= o <= 0x30FF:
        return "kana"
    if 0x3000 <= o <= 0x303F or 0xFF00 <= o <= 0xFFEF:
        return "cjkpunct"
    if o >= 0x1F000 or 0x2600 <= o <= 0x27BF:
        return "emoji"
    return "other"


def _rs_ident(key):
    return re.sub(r"[^A-Z0-9]+", "_", key.upper()).strip("_")


def _units_per_em(adv, size):
    """The design grid a face's ASCII advances sit on (1000 or 2048 units per em), or None."""
    vals = [float(v) for v in adv.values()]
    for upem in (1000, 2048):
        if vals and all(abs(v * upem / size - round(v * upem / size)) < 0.02 for v in vals):
            return upem
    return None


def _em(v, size, upem):
    """A measured px-at-`size` value in ems, SNAPPED to the face's design units when it sits on them.

    The browser measures at 1000px with a float artifact -- Noto Sans CJK KR's `m` reads 880.9984 for 881
    units of a 1000-unit em -- and scaling that noise down to another size lands a few thousandths of a
    pixel off the browser's own answer there: a 35-character string at 72px read 1799.493 where Chrome
    says 1799.4967 (the `generic` key of V-font-metrics). From whole design units it is
    1799.496. A value that is not on the grid -- a bitmap emoji's advance -- is kept as measured."""
    v = float(v)
    for u in ([upem] if upem else []) + [1000, 2048]:
        x = v * u / size
        if abs(x - round(x)) < 0.02:
            return round(round(x) / u, 9)
    return round(v / size, 9)


def _rs_table(pool, kind, rust_type, rows):
    """A table field's value: inline, or -- with a `pool` -- the name of one shared static holding these rows.

    The 93 families Chrome detects on the persona are 74 faces in four styles, and most of a
    face's tables are the platform fallback's (a Noto script face has only its space), so two thirds of the rows
    repeated; a pool compiles each distinct table once."""
    if pool is None:
        return ['&['] + rows + ['    ]']
    key = (kind, tuple(rows))
    if key not in pool:
        pool[key] = ("%s_%d" % (kind, len(pool)), rust_type)
    return [pool[key][0]]


def _rs_pool(pool):
    out = []
    for (kind, rows), (name, rust_type) in pool.items():
        out += ['static %s: &[%s] = &[' % (name, rust_type)] + [r[4:] for r in rows] + ['];', '']
    return out


def _font_metrics_rs(name, family, f, line_gap, avg=None, mx=None, size_for_opt=None, pool=None):
    size = float(f.get("size") or 1000)
    adv = f.get("advances") or {}
    upem = _units_per_em(adv, size)
    widths = [_em(adv.get(chr(c), 0.0), size, upem) for c in range(0x20, 0x7F)]
    kern = f.get("kern") or {}
    pairs = sorted((k, _em(v, size, upem)) for k, v in kern.items() if len(k) == 2)
    extra, classes = [], {}
    for ch, v in (f.get("extra") or {}).items():
        c = _font_class(ch)
        if c == "seq":
            continue
        if c in ("other", "emoji"):
            extra.append((ch, _em(v, size, upem)))
        if c in _FONT_CLASSES and c not in classes:
            classes[c] = _em(v, size, upem)
    if "\U0001F600" in (f.get("extra") or {}):
        classes["emoji"] = _em(f["extra"]["\U0001F600"], size, upem)
    extra.sort()
    out = [
        'pub static %s: FontMetrics = FontMetrics {' % name,
        '    family: %s,' % _rs_str(family),
        '    ascent: %s,' % round(float(f.get("ascent") or 0) / size, 6),
        '    descent: %s,' % round(float(f.get("descent") or 0) / size, 6),
        '    line_gap: %s,' % line_gap,
        '    x_height: %s,' % round(float(f.get("xheight") or 0) / size, 6),
        '    cap_height: %s,' % round(float(f.get("capheight") or 0) / size, 6),
        '    zero_advance: %s,' % _em(f.get("zero") or 0, size, upem),
        '    avg_char_width: %s,' % _rs_opt_em(avg, size_for_opt or size),
        '    max_char_width: %s,' % _rs_opt_em(mx, size_for_opt or size),
    ]
    def field(label, kind, rust_type, rows):
        value = _rs_table(pool, kind, rust_type, rows)
        out.append('    %s: %s' % (label, value[0]))
        out.extend(value[1:-1])
        if len(value) > 1:
            out.append(value[-1])
        out[-1] += ','
    field('advances', 'ADVANCES', 'f32',
          ['        ' + ' '.join('%s,' % w for w in widths[i:i + 8]) for i in range(0, len(widths), 8)])
    field('kerning', 'KERNING', '(char, char, f32)',
          ['        (%s, %s, %s),' % (_rs_char(k[0]), _rs_char(k[1]), v) for k, v in pairs])
    field('extra', 'EXTRA', '(char, f32)', ['        (%s, %s),' % (_rs_char(ch), v) for ch, v in extra])
    out += ['    class_advances: [%s],' % ", ".join(str(classes.get(c, 0.0)) for c in _FONT_CLASSES)]
    # The printable-ASCII characters the face has a glyph for, bit `c - 0x20` -- a capture
    # records `covered` only for a face that lacks some (a Noto script face has its space and nothing else)
    covered = f.get("covered")
    mask = (1 << 95) - 1 if covered is None else sum(1 << (ord(ch) - 0x20) for ch in set(covered) if 0x20 <= ord(ch) < 0x7F)
    out += ['    covered: 0x%x,' % mask, '};', '']
    return out


def font_map_key(bundle_id):
    """FNV-1a 64 of the bundle id: the key a persona hands the engine (`spec::font_map_key`) to pick
    its own font map out of `FONT_MAPS`. The two implementations must agree bit for bit."""
    h = 0xcbf29ce484222325
    for b in bundle_id.encode("utf-8"):
        h ^= b
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def font_table_rs(bundles):
    """`font_table.rs` from EVERY bundle's `fonts/metrics.json` -- advances in EMS, and the pairs that
    actually kern.

    The capture measures at 1000px and everything here is divided by that, because `FontMetrics::
    advance` returns ems and its callers multiply by the font size directly. A VERSION 2 file
    adds every face the persona has, in four styles, and the name tables that pick them.

    `bundles` is `[(bundle_id, metrics)]`, the SHIPPED bundle first. Font metrics are a property of the
    DEVICE the persona was captured on, so every bundle gets its own map (`FONT_MAPS`) over one set of
    faces: a face two bundles measured identically is compiled once, and a key two DEVICES measured
    differently (a Linux capture's `arial` is Liberation Sans, a Windows one's Arial) is the later
    bundle's own, `<key>@b<n>`. Each map carries its bundle's default family; the three legacy tables
    (`ARIAL`, `TIMES_NEW_ROMAN`, `COURIER_NEW`) are the shipped bundle's."""
    bundle_id, metrics = bundles[0]
    fm = metrics["fonts"]
    LINE_GAP = dict(metrics.get("lineGap") or {})
    legacy = {"Arial": "ARIAL", "Times New Roman": "TIMES_NEW_ROMAN", "Courier New": "COURIER_NEW"}
    out = ['//! Font metrics, captured from a real browser. GENERATED by',
           '//! `plugins/korvus-persona/capture/emit.py --role font-metrics` out of one capture --',
           '//! the same visit that produced the persona bundle, so the advances and the device agree.',
           '//!',
           '//! Advances are in EMS: a 16px advance is `advance * 16.0`. They scale linearly with the',
           '//! font size, which is why one capture at 1000px serves every size.',
           '//!',
           '//! VERSION 2: every face the persona has, in four styles (normal, bold, italic,',
           '//! bold italic), measured by `plugins/korvus-persona/capture/fonts-probe.html` in the persona\'s',
           '//! own browser; `FAMILIES` and `GENERICS` name them, `DEFAULT_FAMILY` is the family a document',
           '//! with no `font-family` uses. Non-ASCII text: the sampled characters that vary are in `extra`,',
           '//! and one advance per script class the capture found uniform is in `class_advances`.',
           '',
           'use crate::font::{FontFace, FontMap, FontMetrics};',
           '',
           '/// Printable ASCII, the range these tables cover. Anything outside it falls back.',
           'pub const FIRST_CHAR: u32 = 0x20;',
           'pub const LAST_CHAR: u32 = 0x7e;',
           '']
    for bid, m in bundles:
        if not m.get("faces"):
            raise SystemExit("%s: fonts/metrics.json has no `faces`: emit a VERSION 2 file first (--font-capture)" % bid)
        if m.get("default") not in m["families"]:
            raise SystemExit("%s: the default family %r is not among its families" % (bid, m.get("default")))
    # Every face once. A key two bundles both have is one face when they measured it alike, else the later
    # bundle's own key; `maps` is each bundle's names onto the union's keys.
    faces = dict(metrics["faces"])
    maps = [(bundle_id, metrics, dict(metrics["families"]), dict(metrics["generics"]))]
    for n, (bid, m) in enumerate(bundles[1:], start=1):
        rename = {}
        for key, face in m["faces"].items():
            if key not in faces:
                faces[key] = face
            elif faces[key] != face:
                own = "%s@b%d" % (key, n)
                faces[own] = face
                rename[key] = own
        maps.append((bid, m, {f: rename.get(k, k) for f, k in m["families"].items()},
                     {g: rename.get(k, k) for g, k in m["generics"].items()}))
    styles = ("normal", "bold", "italic", "boldItalic")
    fam_of_face = {}
    for _, _, families, generics in maps:
        for fam, key in families.items():
            fam_of_face.setdefault(key, fam)
        for gen, key in generics.items():
            fam_of_face.setdefault(key, gen)
    # Each face's `avgCharWidth` / `maxCharWidth`, from the bundle that contributed the face -- the two
    # OS/2-derived numbers a text control is sized in (`FontMetrics::avg_char_width` / `max_char_width`).
    # They belong to a face whatever bundle it came from, not only the shipped legacy three, so a Windows
    # Arial carries its own. A zero `avgCharWidth` is the DirectWrite case -- the platform reports no
    # average -- and becomes `None`, the engine's signal to fall back to the advance of 'x' as Blink does;
    # `maxCharWidth` is kept regardless, since Blink keeps it (`layout/replaced.rs::char_widths`).
    avgmax = {}
    for _bid, m, families, _generics in maps:
        mfonts = m.get("fonts") or {}
        for fam, key in families.items():
            fd = mfonts.get(fam)
            if fd and key not in avgmax:
                avgmax[key] = (fd.get("avgCharWidth") or None, fd.get("maxCharWidth") or None, float(fd.get("size") or 1000))
    face_names, keys = {}, list(faces)
    pool = {}
    for key in keys:
        face = faces[key]
        legacy_fam = next((lf for lf in legacy if metrics["families"].get(lf) == key), None)
        am = avgmax.get(key)
        names = []
        for st in styles:
            # avg/max size a text control and belong to the NORMAL style only; the other three take None.
            avg, mx, amsize = (am if (st == "normal" and am) else (None, None, 1000.0))
            if st == "normal" and legacy_fam:
                name = legacy[legacy_fam]
                out += _font_metrics_rs(name, legacy_fam, face[st], LINE_GAP.get(legacy_fam, face.get("lineGap", 0.0)),
                                        avg, mx, amsize, pool)
            else:
                name = "FACE_%s_%s" % (_rs_ident(key), _rs_ident(st))
                out += _font_metrics_rs(name, legacy_fam or fam_of_face[key], face[st], face.get("lineGap", 0.0),
                                        avg, mx, amsize, pool)
            names.append(name)
        face_names[key] = names
    out += ['/// The faces\' tables, each distinct one once.', '']
    out += _rs_pool(pool)
    out += ['/// Every face, once, with its four styles: normal, bold, italic, bold italic.', 'pub static FACES: &[FontFace] = &[']
    for key in keys:
        # `.key` is the face's key IN A BUNDLE'S MAP (what `fonts/metrics.json` names, asserted by
        # codec_grid_live.rs); the `@b<n>` the loop above appends is only this array's cross-bundle
        # uniqueness marker (a later device's `arial` differs from the first's), never part of the
        # bundle-local key a persona looks up, so it is stripped from the reported key.
        out.append('    FontFace { key: %s, styles: [%s] },' % (_rs_str(re.sub(r"@b\d+$", "", key)), ", ".join("&" + n for n in face_names[key])))
    out += ['];', '']
    idx = {k: i for i, k in enumerate(keys)}
    default = metrics["default"]
    rows_out = []
    for n, (bid, m, families, generics) in enumerate(maps):
        names = []
        for label, table in (("FAMILIES", families), ("GENERICS", generics)):
            rows = sorted((name.lower(), idx[key]) for name, key in table.items())
            what = "installed family" if label == "FAMILIES" else "generic family keyword"
            name = "%s_%d" % (label, n)
            out += ['/// %s: each %s (lowercase), sorted, to its face in [`FACES`].' % (bid, what), 'static %s: &[(&str, u16)] = &[' % name]
            out += ['    (%s, %d),' % (_rs_str(nm), i) for nm, i in rows]
            out += ['];', '']
            names.append(name)
        rows_out.append('    FontMap { bundle: %s, key: 0x%016x, families: %s, generics: %s, default_family: %s, default_face: %d },'
                        % (_rs_str(bid), font_map_key(bid), names[0], names[1], _rs_str(m["default"]), idx[families[m["default"]]]))
    out += ['/// Every bundle\'s names for the faces, the SHIPPED bundle first: a persona picks its own by',
            '/// `key` (`spec::font_map_key` of its bundle id), and a key no map has gets the first.',
            'pub static FONT_MAPS: &[FontMap] = &[']
    out += rows_out
    out += ['];', '']
    out += ['/// The shipped bundle\'s standard font, the family a document with no `font-family` uses (each map',
            '/// carries its own: `FontMap::default_family`).',
            'pub const DEFAULT_FAMILY: &str = %s;' % _rs_str(default),
            'pub const DEFAULT_FACE: usize = %d;' % idx[metrics["families"][default]],
            '']
    out += ['/// The three legacy fonts, as earlier tables named them.',
            'pub static ALL: &[&FontMetrics] = &[%s];' % ", ".join("&" + n for n in legacy.values()),
            '']
    return "\n".join(out)


# THERE IS NO `ua-stylesheet` EMITTER, AND THE REASON IS NOT LAZINESS.
#
# The capture's `uaDefaults` block reads one computed style per tag inside an empty iframe, and that
# is a genuine, provenance-carrying record of the browser's defaults. It is NOT a stylesheet, and the
# difference is not cosmetic. Written out as rules it broke 62 of korvus-page's 344 tests, for two
# reasons that a bigger tag list would not fix:
#
#   * A UA SHEET IS MOSTLY CONDITIONAL. The committed one selects on
#     `[type="checkbox" i]`, `details[open] > summary:first-of-type`, `dialog:not([open])`,
#     `audio:not([controls])`, `[popover]:where(:not(:popover-open))`, `abbr[title]`,
#     `body:focus-visible`... A computed style is one element in one state, so every conditional rule
#     collapses to whichever branch that element happened to be in, and the others vanish.
#   * A COMPUTED VALUE IS RESOLVED AGAINST THE HOST. `font-family` came back
#     `"Noto Sans CJK KR"` -- this machine's fontconfig default, not a browser default -- which is
#     precisely the host value the bundle's `fonts-host` entry exists to keep out of the persona.
#
# So `uaDefaults` stays in the capture, where it is a useful cross-check against whatever sheet is
# committed, and `ua-stylesheet` stays inherited until something can emit a sheet rather than a
# projection of one.

# Set by main(): the capture's path (codecs.mjs reads the file itself) and the bundle the run is
# closing artifacts for (its directory, and the `source` every re-recorded oracle entry carries).
CAPTURE_PATH = None
BUNDLE_ID = None
BUNDLE_DIR = None


def emit_oracle(cap):
    """`behavior-cases/<Iface>.json`, re-recorded from the capture's `behavior.answers`.

    The corpus the collector runs IS this directory -- host.py serves it, collect.js answers every
    expression twice, and the capture keeps the ones that agreed. So closing this role is a join,
    not a translation: for each entry the capture answered, the answer becomes `expected` /
    `throws` / `undefined` and `source` becomes this bundle's id.

    TWO KINDS OF ENTRY ARE LEFT ALONE, visibly. An entry with no `source` AND an answer was written
    by a person (345 of them) and is never overwritten -- that is the one-table rule: a body someone
    wrote is theirs. And an entry the capture did not answer keeps its old tag: host.py withholds the
    expressions that would open a native dialog (`navigator.share`, `requestMIDIAccess`, ...) and
    the collector drops the ones that disagreed with themselves, so those still say which older
    session recorded them rather than claiming this one did.

    A SKELETON IS FILLED. `webapi gencases` and `gen_receiver_identity_cases.mjs` write
    `{iface, member, expr}` with no answer at all -- a question waiting for the browser. Those used
    to go through a separate CDP oracle's `--fill`; now they sit in the corpus directory, the next
    capture answers them like any other case, and this fills them. A question is nobody's body.

    Returns {filename: text}; main() writes a directory for a dict body."""
    answers = ((cap.get("behavior") or {}).get("answers") or {})
    if not answers:
        raise SystemExit("this capture has no behavior.answers")
    # The QUESTIONS are the shipped bundle's corpus (what host.py served the collector); a new bundle
    # starts from the same questions and records its own answers.
    src = pathlib.Path(shipped.artifact("oracle"))
    out, n_re, n_human, n_unanswered, n_changed, n_realm, n_filled = {}, 0, 0, 0, 0, 0, 0
    for f in sorted(src.glob("*.json")):
        entries = json.loads(f.read_text(encoding="utf-8"))
        for e in entries:
            has_answer = any(k in e for k in ("expected", "throws", "undefined"))
            if e.get("source") is None and has_answer:
                n_human += 1
                continue
            if (e["iface"], e["member"]) in REALM_ARTIFACTS:
                n_realm += 1
                continue
            a = answers.get(e["iface"] + "\0" + e["member"] + "\0" + e["expr"])
            if a is None or "unsupported" in a:
                n_unanswered += 1
                continue
            before = {k: e.get(k) for k in ("expected", "throws", "undefined")}
            # Rebuilt in the file's own key order (iface, member, expr, answer, source), so a re-emit
            # that changes nothing diffs as nothing. The first version popped the answer keys and
            # re-added them, which moved every answer behind `source` and turned 26 real changes
            # into a 29,000-line diff.
            answer = {}
            if "value" in a:
                answer["expected"] = a["value"]
            elif "threw" in a:
                answer["throws"] = a["threw"]
            elif "undef" in a:
                answer["undefined"] = True
            rebuilt = {k: e[k] for k in ("iface", "member", "expr") if k in e}
            rebuilt.update(answer)
            for k, v in e.items():
                if k not in rebuilt and k not in ("expected", "throws", "undefined", "source"):
                    rebuilt[k] = v
            rebuilt["source"] = BUNDLE_ID
            e.clear()
            e.update(rebuilt)
            n_re += 1
            n_filled += not has_answer
            n_changed += has_answer and before != {k: e.get(k) for k in ("expected", "throws", "undefined")}
        out[f.name] = json.dumps(entries, indent=2, ensure_ascii=False) + "\n"
    print("  oracle: %d entries re-recorded (%d with a different answer, %d skeletons filled), "
          "%d human-owned kept, %d unanswered by this capture kept with their old tag, %d realm "
          "artifacts kept with their old tag" % (n_re, n_changed, n_filled, n_human, n_unanswered, n_realm))
    return out


# (iface, member) pairs whose answer in a capture is a fact about the RIG the corpus ran in, not
# about the browser -- so the capture's answer is not promoted and the entry keeps its earlier tag.
#
# EMPTY, and meant to stay so. The first Linux capture filled it with nineteen entries: the corpus
# ran in a hidden 1x1 iframe, so `frameElement` was an element, `getInstalledRelatedApps` threw,
# the navigation entry was a 404 text/plain page after a `location.pathname = "x"` case had
# committed, `elementsFromPoint` found nothing, and `Notification.permission` was a grant the user
# had once given 127.0.0.1:8900. Each was closed in the rig rather than excused here:
#
#   collect.js  captureVersion 3+  the navigating cases run last in a disposable frame;
#                                  the corpus frame is 1024x768
#               captureVersion 4+  the cases a frame cannot answer as a tab (frameElement,
#                                  getInstalledRelatedApps, the navigation entry) run in the
#                                  top page; pass 2 starts from a fresh frame
#   host.py                        a random port per run, so no origin state carries over;
#                                  the frame gets a bare document at "/" (Sec-Fetch-Dest)
#
# An entry belongs here only while the rig cannot yet ask the question right, and its line must
# say what would remove it. A list that grows is a rig that is not being fixed.
REALM_ARTIFACTS = set()


def emit_codecs(cap):
    """`codecs.json` -- the recorded answer grid, by the ONE program that knows its questions.

    codecs.mjs drove a headless browser over CDP for its 9,104 answers; the collector asks the same
    grid in the visited browser and records it under `codecs.answers`. This does not re-implement the
    grid in Python -- two writers of one file is exactly the split this repo keeps removing -- it hands
    the capture to codecs.mjs in `--from-capture --raw` mode and takes the grid it read.

    NOTHING IS DERIVED FROM IT AT SYNC TIME ANY MORE. The grid used to be compiled into
    `generated_media_codecs.rs` from the shipped bundle, so every device answered Brave's codecs; the
    engine now reads the ACTIVE bundle's `codecs.json` through the persona, and this file is the whole
    artifact."""
    import subprocess, tempfile
    if not (cap.get("codecs") or {}).get("answers"):
        raise SystemExit("this capture has no codecs.answers (captureVersion < 2)")
    with tempfile.TemporaryDirectory() as d:
        raw = pathlib.Path(d) / "codecs.json"
        subprocess.run(["node", str(ROOT / "plugins/korvus-persona/capture/codecs.mjs"),
                        "--from-capture", str(CAPTURE_PATH), "--source", BUNDLE_ID, "--raw", str(raw)],
                       check=True)
        return raw.read_text(encoding="utf-8")


def derive_font_metrics(bundle_dir: pathlib.Path):
    """`font_table.rs` from EVERY bundle's `fonts/metrics.json` (into OUT_ROOT), the shipped bundle's
    first whichever bundle `bundle_dir` names -- the table is the same file from any of them."""
    del bundle_dir
    first = pathlib.Path(shipped.BUNDLE_DIR).resolve()
    dirs = sorted(d.resolve() for d in (ROOT / "plugins/korvus-persona/bundles").iterdir() if (d / "manifest.toml").is_file())
    dirs = [first] + [d for d in dirs if d != first]
    bundles = [(d.name, json.loads((d / BUNDLE_FILES["font-metrics"]).read_text(encoding="utf-8"))) for d in dirs]
    out = OUT_ROOT / DERIVED["font-metrics"]
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(font_table_rs(bundles), encoding="utf-8")
    return out


# `derive_codecs` WAS HERE. It ran `codecs.mjs --from-bundle` to compile the shipped
# bundle's grid into `generated_media_codecs.rs`; the engine reads every bundle's own grid now, so
# there is nothing to derive and no generated file to keep in step.


DERIVERS = {"font-metrics": derive_font_metrics}


EMITTERS = {
    "surface": emit_surface,
    "oracle": emit_oracle,
    "codecs": emit_codecs,
    "worker-surface": emit_worker_surface,
    "render-reference": emit_render_reference,
    "font-metrics": emit_font_metrics,
    "theme": emit_theme,
    "computed-style": emit_computed_style,
    "webrtc-offer": emit_webrtc_offer,
    "webrtc-capabilities": emit_webrtc_capabilities,
    "css-named-properties": emit_css_named_properties,
}


def mark_recorded(bundle_dir: pathlib.Path, role: str):
    """Flip the artifact to `matches_capture = true` and delete its `<role>-inherited` entry.

    The manifest is the record: an artifact that now comes from this capture must stop saying it does
    not, and the entry explaining why it did not has nothing left to explain.

    THE ENTRY IS FOUND BY SPLITTING ON THE TABLE HEADER, not by matching the whole block with one
    regular expression. The first version did the latter and deleted one of two entries -- the
    trailing-blank-line the pattern needed was there for `render-reference` and absent for
    `font-metrics`, so a run that closed both left one behind and the ratchet caught it. Splitting on
    `[[known_incoherence]]` cannot care how a block is spaced."""
    man = bundle_dir / "manifest.toml"
    s = man.read_text(encoding="utf-8")

    # A role this manifest has never listed (`theme`) is ADDED rather than skipped: an
    # artifact written into the bundle directory that its index does not name is exactly the
    # unrecorded state `Bundle::self_check` exists to prevent. It goes where `promote.py` puts it, in
    # the shape `promote.py` writes.
    if not re.search(r'\[\[artifact\]\]\nrole = "%s"\n' % re.escape(role), s):
        entry = ('[[artifact]]\nrole = "%s"\npath = "%s"\nprovenance = "%s"\nmatches_capture = true\n\n'
                 % (role, BUNDLE_FILES[role], bundle_dir.name))
        # Before the inherited UA sheet when there is one, which is `promote.py`'s order.
        at = s.find('[[artifact]]\nrole = "ua-stylesheet"')
        at = at if at >= 0 else s.find("[transport]")
        if at < 0:
            raise SystemExit("%s has no [transport] table to insert the %s artifact before" % (man, role))
        s = s[:at] + entry + s[at:]

    s = re.sub(r'(role = "%s"\npath = [^\n]+\n)provenance = "[^"]*"\nmatches_capture = false'
               % re.escape(role),
               lambda m: m.group(1) + 'provenance = "%s"\nmatches_capture = true' % bundle_dir.name,
               s)

    head, sep, rest = s.partition("[[known_incoherence]]")
    if sep:
        blocks = [b for b in rest.split("[[known_incoherence]]")]
        kept = [b for b in blocks if ('id = "%s-inherited"' % role) not in b]
        s = head + "".join(sep + b for b in kept) if kept else head.rstrip() + "\n"
    man.write_text(s, encoding="utf-8")


def _bundle_dir(arg: str) -> pathlib.Path:
    """`--bundle`/`--from-bundle`: a directory, a bundle id, or `shipped` (the one the engine wears)."""
    if not arg or arg == "shipped":
        return pathlib.Path(shipped.BUNDLE_DIR)
    p = pathlib.Path(arg)
    if p.is_dir():
        return p.resolve()
    cand = ROOT / "plugins/korvus-persona/bundles" / arg
    if cand.is_dir():
        return cand
    raise SystemExit("emit: no bundle directory at %s" % arg)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("capture", nargs="?", help="a collect.js capture (omit with --from-bundle)")
    ap.add_argument("--role", action="append", default=[], choices=sorted(EMITTERS))
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--bundle", default="", help="bundle dir or id to write into (default: the shipped bundle)")
    ap.add_argument("--from-bundle", default="",
                    help="no capture: regenerate the DERIVED Rust file (font-metrics) from this "
                         "bundle's committed artifacts -- what scripts/sync.sh runs; `shipped` = the engine's")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--font-capture", default="",
                    help="a fonts-probe.html capture: write fonts/metrics.json VERSION 2 from it "
                         "and the bundle's current file, then derive font_table.rs")
    a = ap.parse_args()

    global BUNDLE_ID, BUNDLE_DIR, CAPTURE_PATH
    if a.font_capture:
        BUNDLE_DIR = _bundle_dir(a.bundle)
        BUNDLE_ID = BUNDLE_DIR.name
        target = BUNDLE_DIR / BUNDLE_FILES["font-metrics"]
        prev = json.loads(target.read_text(encoding="utf-8"))
        fpath = pathlib.Path(a.font_capture)
        body = emit_font_metrics_v2(prev, json.load(open(fpath)), fpath.name)
        print("  %-17s %-24s %8d -> %8d bytes" % ("font-metrics", BUNDLE_FILES["font-metrics"], target.stat().st_size, len(body.encode())))
        if a.dry_run:
            return 0
        target.write_text(body, encoding="utf-8")
        out = DERIVERS["font-metrics"](BUNDLE_DIR)
        print("  %-17s derived -> %s" % ("", out.relative_to(OUT_ROOT)))
        # The VERSION 2 file is this bundle's own recording now: the manifest says so, as every other role's
        # emit does (the 150 -> 152 update found a fresh bundle still calling its font metrics inherited).
        mark_recorded(BUNDLE_DIR, "font-metrics")
        return 0
    if a.from_bundle:
        BUNDLE_DIR = _bundle_dir(a.from_bundle)
        BUNDLE_ID = BUNDLE_DIR.name
        roles = [r for r in (a.role or sorted(DERIVERS)) if r in DERIVERS]
        for role in roles:
            out = DERIVERS[role](BUNDLE_DIR)
            print("  %-17s %s -> %s" % (role, BUNDLE_FILES[role], out.relative_to(OUT_ROOT)))
        return 0

    if not a.capture:
        print("emit: a capture is required unless --from-bundle is given", file=sys.stderr)
        return 2
    cap = json.load(open(a.capture))
    CAPTURE_PATH = pathlib.Path(a.capture).resolve()
    roles = sorted(EMITTERS) if a.all else a.role
    if not roles:
        print("emit: name at least one --role, or --all", file=sys.stderr)
        return 2

    BUNDLE_DIR = _bundle_dir(a.bundle)
    BUNDLE_ID = BUNDLE_DIR.name

    for role in roles:
        target = BUNDLE_DIR / BUNDLE_FILES[role]
        body = EMITTERS[role](cap)
        if isinstance(body, dict):
            # A directory role: one file per key, written in place. Files the emitter did not
            # return (none today) would be left as they are, never deleted.
            total = sum(len(v.encode()) for v in body.values())
            print("  %-17s %-24s %8d files, %8d bytes" % (role, BUNDLE_FILES[role], len(body), total))
            if a.dry_run:
                continue
            target.mkdir(parents=True, exist_ok=True)
            for name, text in body.items():
                (target / name).write_text(text, encoding="utf-8")
            mark_recorded(BUNDLE_DIR, role)
            continue
        before = target.stat().st_size if target.exists() else 0
        print("  %-17s %-24s %8d -> %8d bytes" % (role, BUNDLE_FILES[role], before, len(body.encode())))
        if a.dry_run:
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")
        if role in DERIVERS:
            out = DERIVERS[role](BUNDLE_DIR)
            print("  %-17s derived -> %s" % ("", out.relative_to(OUT_ROOT)))
        mark_recorded(BUNDLE_DIR, role)
    if not a.dry_run:
        print("\n  marked in %s/manifest.toml" % BUNDLE_DIR.name)
        print("  Re-run the gate: the surface roles move every conformance number in the repo.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
