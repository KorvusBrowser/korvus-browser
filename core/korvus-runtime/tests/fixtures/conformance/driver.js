// Korvus conformance driver: runs a ported an agent-browser reference fixture's inline <script>s the way the
// an agent-browser reference test runner does (per-script document.currentScript), then calls testing.assertOk().
// Scripts are compiled as AsyncFunction bodies so `type=module` fixtures using top-level `await`
// (testing.async()) run correctly; each script is awaited in document order.
globalThis.__runFixture = async function () {
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const scripts = Array.from(document.getElementsByTagName("script"));
  for (const s of scripts) {
    if (s.getAttribute("src")) continue; // external scripts (e.g. the testing.js include) not fetched here
    // Anonymous inline scripts still run: fixtures use them for setup (e.g. defining window
    // helpers that later id'd scripts call). Only id'd scripts register testing.async() blocks.
    const src = s.textContent;
    try { Object.defineProperty(document, "currentScript", { value: s, configurable: true }); } catch (e) {}
    try {
      await new AsyncFunction(src)();
    } catch (e) {
      // expectEqual/expectError throw on failure; the observation is already recorded.
    }
  }
  try { Object.defineProperty(document, "currentScript", { value: null, configurable: true }); } catch (e) {}
  try {
    const ok = testing.assertOk();
    print("RESULT:" + (ok === true ? "PASS" : "PENDING"));
  } catch (e) {
    print("RESULT:FAIL:" + (e && e.message ? e.message : e));
  }
};
__runFixture();
