#!/usr/bin/env python3
"""A Windows persona's fonts: the families a DEFAULT Windows 11 installation has, cut out of a capture taken on a
machine that has more (owner 2026-10-17: "기본 글꼴만" -- the default fonts only; the owner's PC also carries Office's and
Hancom Office's).

    default_fonts.py names                 the default family names, one per line
    default_fonts.py capture RAW OUT       a collect.js capture (host.py's) with its `fonts` block cut to the defaults
    default_fonts.py fonts FONTS OUT       a fonts_capture.py capture cut to the defaults

A family is DEFAULT when a file Windows 11 ships names it -- `data/windows-default-fonts.json`: Microsoft's list of the
files of every Windows 11 installation and of the Korean Supplemental Fonts (the feature-on-demand Windows installs with
Korean, the persona's language), each file read on the measuring PC for the GDI family names it gives in English and
Korean -- or when Blink answers the name with a default family's font (`AlternateFamilyName`: Courier, Times,
Helvetica). Every measurement stays the capture's own; only which families are installed changes, and a family that is
not default reads as a default installation reads it: ABSENT. In a collect.js capture that is `present` without it and
its width the one the capture measured for an absent family (its `"<name>", sans-serif` falls back: `generic`).
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, "data", "windows-default-fonts.json")


def default_names(data=None):
    """Every default family name: what the files give, plus Blink's alternates of the ones they give."""
    d = data or json.load(open(DATA, encoding="utf-8"))
    names = {n for v in d["files"].values() for n in v}
    alt = {k: v for k, v in d["alternates"].items() if not k.startswith("_")}
    names |= {a for a, target in alt.items() if target in names}
    return names


def cut_collect(cap, names):
    """A collect.js capture with its `fonts` block holding only `names`: an absent family's width is the capture's own
    measurement of an absent family -- `"<name>", sans-serif` is `sans-serif` (`generic`)."""
    out = dict(cap)
    f = dict(cap["fonts"])
    absent_w = f["generic"]["sans-serif"]
    gone = [n for n in f["present"] if n not in names]
    f["present"] = [n for n in f["present"] if n in names]
    f["count"] = len(f["present"])
    f["widths"] = {n: (absent_w if n in gone else w) for n, w in f["widths"].items()}
    out["fonts"] = f
    out["defaultFonts"] = {"tool": "plugins/korvus-persona/capture/default_fonts.py", "removed": gone}
    return out


def cut_fonts(cap, names):
    """A fonts_capture.py capture holding only `names`: `present`, the per-family advances of every style, the line
    heights. The candidates the page asked about stay; the defaults it found stay as found."""
    out = dict(cap)
    keep = [n for n in cap["present"] if n in names]
    gone = [n for n in cap["present"] if n not in names]
    out["present"] = keep
    out["fonts"] = {n: v for n, v in cap["fonts"].items() if n in names}
    out["styles"] = {st: dict(block, fonts={n: v for n, v in block["fonts"].items() if n in names})
                     for st, block in cap["styles"].items()}
    out["lineHeights"] = {k: v for k, v in (cap.get("lineHeights") or {}).items() if k.strip('"') in names}
    default = (cap.get("domDefault") or {}).get("computedFontFamily", "").strip().strip('"')
    if default and default not in keep:
        raise SystemExit("default_fonts: the capture's default family %r is not a default font" % default)
    out["defaultFonts"] = {"tool": "plugins/korvus-persona/capture/default_fonts.py", "removed": gone}
    return out


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "names":
        print("\n".join(sorted(default_names())))
        return
    if len(sys.argv) != 4 or sys.argv[1] not in ("capture", "fonts"):
        raise SystemExit(__doc__)
    kind, src, dst = sys.argv[1:]
    cap = json.load(open(src, encoding="utf-8"))
    out = (cut_collect if kind == "capture" else cut_fonts)(cap, default_names())
    json.dump(out, open(dst, "w", encoding="utf-8"), ensure_ascii=False)
    gone = out["defaultFonts"]["removed"]
    print("default_fonts: %d kept, %d removed -> %s" % (len(out["fonts"]["present"] if kind == "capture" else out["present"]), len(gone), dst))


if __name__ == "__main__":
    main()
