#!/usr/bin/env python3
"""Measure each installed browser's CSSStyleDeclaration NAMED properties -- the names its named-property
enumerator lists (`Object.getOwnPropertyNames(document.createElement('div').style)`) -- into its bundle's
`css-named-properties.json`.

usage: plugins/korvus-persona/capture/css_named_properties.py [bundle-id ...]   (default: every bundle with a binary)

Blink's `CSSStyleDeclaration::NamedPropertyEnumerator` lists the JS name of every web-exposed CSS property and
alias, sorted by code unit -- the BUILD's table (Chrome 150: 744 names, 151 `webkit*`, 9 `epub*`), larger than
any snapshot of css_properties.json5 filtered to stable flags, and different on each Chromium. Each browser runs
headed under its own Xvfb with a fresh profile, the page served from loopback; the names are written as listed.
"""
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import emit  # noqa: E402  -- the one formatter, shared with `emit.py --role css-named-properties`
from webrtc_capabilities import BINARIES, BUNDLES, REPO, browser_answer, browser_version  # noqa: E402

PAGE = b"""<!doctype html><script>
const names = Object.getOwnPropertyNames(document.createElement('div').style).filter(n => !/^[0-9]+$/.test(n));
fetch('/result', {method: 'POST', body: JSON.stringify({names})});
</script>"""


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
        names = got.get("names")
        if not names:
            print(f"{bid}: no answer from {version}")
            continue
        out = BUNDLES / bid / "css-named-properties.json"
        out.write_text(emit.format_css_named_properties(names, f"measured on the installed {version}, headed under "
                       "Xvfb (plugins/korvus-persona/capture/css_named_properties.py) -- not this bundle's capture session"))
        print(f"{bid}: {version} -> {out.relative_to(REPO)} {len(names)} names")


if __name__ == "__main__":
    main()
