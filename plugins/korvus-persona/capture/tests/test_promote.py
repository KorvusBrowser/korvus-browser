#!/usr/bin/env python3
"""The promoter's identity derivation — who a capture is, and which wire that browser speaks.

    python3 -m unittest discover -s plugins/korvus-persona/capture/tests -p 'test_*.py' -v

WHY THIS FILE EXISTS. `promote.py` used to hold `TLS_PRESET = "chrome_149"` as a module constant and
stamp it into every manifest it wrote. Two Chromium captures could not tell that apart from a
derivation -- Brave rides Chrome's wire, and Chrome is Chrome -- so the defect was invisible until a
third browser arrived. Edge and Opera are that third and fourth browser, and these cases are the ones that
would have caught it on the first promotion rather than the third.

The two existing bundles are in here as REGRESSION anchors: their manifests are committed, and a
derivation that changed what they declare would be rewriting history other artifacts are keyed to.
"""
import importlib.util
import pathlib
import unittest

CAPTURE_DIR = pathlib.Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("promote", CAPTURE_DIR / "promote.py")
promote = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(promote)

# The four UAs this goal's personas actually report, page-read rather than taken off a version
# string. Opera is the one that matters: its product version and its Chromium version differ.
BRAVE = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36"
CHROME = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36"
EDGE = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
        "Chrome/151.0.0.0 Safari/537.36 Edg/151.0.0.0")
OPERA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
         "Chrome/151.0.0.0 Safari/537.36 OPR/135.0.0.0")


def cond(ua, is_brave=False, at="2026-09-13T12:00:00.000Z"):
    return {"ua": ua, "isBrave": is_brave, "capturedAt": at}


class WhoTheCaptureIs(unittest.TestCase):
    def test_brave_is_told_from_chrome_by_isBrave_alone(self):
        # Brave's UA is byte-identical to Chrome's of the same major. If this ever starts reading the
        # UA for Brave, it is reading something Brave does not say.
        self.assertEqual(promote.browser_of(cond(BRAVE, is_brave=True)), "brave")
        self.assertEqual(promote.browser_of(cond(BRAVE, is_brave=False)), "chrome")

    def test_edge_and_opera_are_told_by_their_ua_token(self):
        self.assertEqual(promote.browser_of(cond(EDGE)), "edge")
        self.assertEqual(promote.browser_of(cond(OPERA)), "opera")

    def test_a_plain_chrome_ua_is_chrome(self):
        self.assertEqual(promote.browser_of(cond(CHROME)), "chrome")

    def test_the_chrome_token_in_every_ua_does_not_win(self):
        # Every UA here contains `Chrome/151`. A derivation that checked Chrome first would call all
        # four of them Chrome and nothing downstream would notice.
        for ua in (EDGE, OPERA):
            self.assertIn("Chrome/", ua)
            self.assertNotEqual(promote.browser_of(cond(ua)), "chrome")


class WhichVersion(unittest.TestCase):
    def test_product_and_chromium_agree_except_for_opera(self):
        for ua in (BRAVE, EDGE):
            self.assertEqual(promote.product_major(cond(ua)), promote.chrome_major(ua), ua)
        self.assertEqual(promote.product_major(cond(OPERA)), 135)
        self.assertEqual(promote.chrome_major(OPERA), 151)

    def test_the_bundle_id_carries_the_product_version(self):
        # `opera-151-linux-...` would name a version of Opera that has never existed.
        self.assertEqual(promote.suggest_id(cond(OPERA)), "opera-135-linux-20260913")
        self.assertEqual(promote.suggest_id(cond(EDGE)), "edge-151-linux-20260913")


class WhichWire(unittest.TestCase):
    def test_each_browser_gets_its_own_family(self):
        self.assertTrue(promote.tls_preset(cond(EDGE)).startswith("edge_"))
        self.assertTrue(promote.tls_preset(cond(OPERA)).startswith("opera_"))
        self.assertTrue(promote.tls_preset(cond(CHROME)).startswith("chrome_"))

    def test_brave_rides_chromes_wire_and_that_is_deliberate(self):
        # Not a fallback: Brave ships Chromium's network stack unmodified, so `chrome_*` is the
        # correct answer for Brave and `wreq-util` ships no brave_* preset to choose instead.
        self.assertTrue(promote.tls_preset(cond(BRAVE, is_brave=True)).startswith("chrome_"))

    def test_the_preset_is_never_newer_than_the_browser(self):
        for c in (cond(BRAVE, is_brave=True), cond(CHROME), cond(EDGE), cond(OPERA)):
            preset = promote.tls_preset(c)
            self.assertLessEqual(int(preset.rsplit("_", 1)[-1]), promote.product_major(c), preset)

    def test_the_two_committed_bundles_still_declare_what_they_declare(self):
        # A derivation that changed these would rewrite two sessions' history.
        self.assertEqual(promote.tls_preset(cond(BRAVE, is_brave=True)), "chrome_149")
        self.assertEqual(promote.tls_preset(cond(CHROME)), "chrome_149")

    def test_the_preset_list_comes_from_the_pinned_crate_and_is_not_empty(self):
        # The transport's own doc comment records what the pinned library ships -- 133 presets, "the
        # whole Chromium family (41 chrome_*, 19 edge_*, 16 opera_*) included". This parse reads the
        # same table the engine reads through serde, so the two must agree; a shrunken list here
        # would make every case above vacuous while still looking like a pass.
        presets = promote._wreq_util_presets()
        self.assertGreater(len(presets), 100, "a tiny preset list makes every case above vacuous")
        for family, count in (("chrome", 41), ("edge", 19), ("opera", 16)):
            self.assertEqual(len([p for p in presets if p.startswith(family + "_")]), count, family)


class WhatTheManifestAdmits(unittest.TestCase):
    def test_every_bundle_quotes_the_one_ja4_they_all_now_emit(self):
        # UNTIL FP1 THIS TEST WAS THE OPPOSITE: L7d had read ONE pairing off a socket, so quoting its
        # JA4 digests under an Edge or Opera manifest would have been a generator fabricating a
        # measurement, and those entries had to say `NOT MEASURED`. That distinction is gone, and not
        # by relaxing it: `the_four_presets_emit_the_browser_familys_own_ja4` reads chrome_149,
        # edge_148 AND opera_131 off the same sink and all three emit this one string, which is also
        # what the five real browsers emit. One wire, measured for every pairing, so every entry may
        # quote it.
        for c in (cond(BRAVE, is_brave=True), cond(CHROME), cond(EDGE), cond(OPERA)):
            entry = next(k for k in promote.known_incoherences({}, c) if k["id"] == "tls-version")
            self.assertIn(promote.TLS_FAMILY_JA4, entry["detail"], c["ua"])
            self.assertIn(promote.TLS_FAMILY_SIGALGS, entry["detail"], c["ua"])
            # And the entry must not still claim the gap it no longer has.
            self.assertNotIn("d8a2da3f94cd with", entry["detail"], c["ua"])

    def test_only_operas_entry_mentions_the_alps_codepoint(self):
        # The second difference was Opera's alone. A generator that copied it onto a Chrome bundle
        # would be fabricating a measurement in exactly the way the old `NOT MEASURED` branch guarded
        # against, so the guard moved rather than went away.
        opera = next(k for k in promote.known_incoherences({}, cond(OPERA)) if k["id"] == "tls-version")
        self.assertIn("0x4469", opera["detail"])
        for c in (cond(BRAVE, is_brave=True), cond(CHROME), cond(EDGE)):
            entry = next(k for k in promote.known_incoherences({}, c) if k["id"] == "tls-version")
            self.assertNotIn("0x4469", entry["detail"], c["ua"])

    def test_every_capture_here_declares_the_gap_at_all(self):
        # No preset in the pinned library NAMES any of these four browsers' versions, so all four must
        # carry the entry -- `coherence::audit` reads the preset's trailing integer, has no socket,
        # and refuses to boot a bundle whose disagreement is undeclared. Measured on 2026-09-19 by
        # deleting the entries: `Engine::assemble` refused all four. The bytes agreeing does not help;
        # the audit cannot see them.
        for c in (cond(BRAVE, is_brave=True), cond(CHROME), cond(EDGE), cond(OPERA)):
            ids = [k["id"] for k in promote.known_incoherences({}, c)]
            self.assertIn("tls-version", ids, c["ua"])


if __name__ == "__main__":
    unittest.main()
