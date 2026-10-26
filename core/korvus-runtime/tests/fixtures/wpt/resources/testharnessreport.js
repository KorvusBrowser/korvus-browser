// Korvus WPT report hook (NOT vendored WPT content).
//
// wptserve serves its own resources/testharnessreport.js to post results back to the wpt runner;
// an embedder replaces it with a hook that captures results in-process. This file is that hook: it
// runs immediately after resources/testharness.js (every WPT test loads both, in that order), so the
// `setup`/`add_result_callback`/`add_completion_callback` globals it uses already exist.
//
// It (a) disables the harness's own multi-second auto-timeout via setup({explicit_timeout:true}) so
// the Rust runner owns finalization deterministically; (b) captures every settled subtest
// incrementally (blackhole-proof) onto globalThis.__wptpartial; (c) records the authoritative
// completion payload as a JSON string on globalThis.__wptresults. It NEVER fabricates a result — a
// subtest's status is whatever testharness reports.
(() => {
  // `output: false` disables testharness's in-page results TABLE. We never read it -- results come
  // through add_completion_callback -- and rendering it walks DOM APIs an incomplete engine may not
  // have, where a throw aborts completion and strands the whole file with no result at all. wptrunner
  // runs with output off for the same reason, and so does the reference benchmark's overlay, which says so
  // explicitly. Leaving it on cost us files that had nothing to do with the table.
  try { setup({ explicit_timeout: true, output: false }); } catch (e) {}
  globalThis.__wptpartial = [];
  try {
    add_result_callback(function (t) {
      try { globalThis.__wptpartial.push({ name: t.name, status: t.status, message: t.message || null }); } catch (e) {}
    });
  } catch (e) {}
  // OBSERVATION ONLY (worker-url-loading unit, per a peer session's instrumentation ask): how many
  // DISTINCT tests testharness.js ever registered (test()/promise_test()/async_test() calls that
  // reached the harness), whether or not any of them ever finished. `add_test_state_callback` fires on
  // every phase transition of a test, not just registration, so dedupe by `test.index` (stable, set once
  // when a test is first pushed into the registry) to count each test exactly once. Distinguishes "this
  // file registered 50 tests and none finished" from "this file registered nothing" for a no-verdict
  // file, where `__wptpartial`/`__wptresults` alone say nothing (both stay empty either way). Never
  // read by anything that decides pass/fail -- purely additive, same guarantee as `__wpterrors` below.
  globalThis.__wptregistered = 0;
  try {
    var __wptSeenIdx = Object.create(null);
    add_test_state_callback(function (t) {
      try {
        var k = (t && t.index !== undefined && t.index !== null) ? t.index : (t && t.name);
        if (k !== undefined && k !== null && !(k in __wptSeenIdx)) {
          __wptSeenIdx[k] = true;
          globalThis.__wptregistered++;
        }
      } catch (e) {}
    });
  } catch (e) {}
  try {
    add_completion_callback(function (tests, status) {
      try {
        globalThis.__wptresults = JSON.stringify({
          harness: status.status,
          tests: tests.map(function (t) { return { name: t.name, status: t.status, message: t.message || null }; })
        });
      } catch (e) {}
    });
  } catch (e) {}

  // Capture uncaught errors and unhandled rejections onto globalThis.__wpterrors.
  //
  // This is the diagnostic that turns "no verdict" from a dead end into a worklist. A file whose
  // harness loaded but never completed has almost always thrown during setup -- typically on a DOM or
  // JS API that does not exist here -- and without the message there is nothing to act on but the
  // filename. 2,698 non-encoding files reach no verdict; bucketed by this message they become a
  // ranked list of what to implement. a reference engine's overlay captures the same thing for the same reason.
  //
  // Passive: never preventDefault, never rethrow, so testharness's own bookkeeping is untouched.
  globalThis.__wpterrors = [];
  var MAXE = 20;
  function rec(line) {
    try {
      if (globalThis.__wpterrors.length < MAXE) {
        var s = String(line);
        globalThis.__wpterrors.push(s.length > 500 ? s.slice(0, 500) + "..." : s);
      }
    } catch (e) {}
  }
  var viaListener = false;
  try {
    globalThis.addEventListener("error", function (ev) {
      viaListener = true;
      var where = ev && ev.filename ? " (" + ev.filename + ":" + (ev.lineno || 0) + ")" : "";
      rec(((ev && ev.message) || "uncaught error") + where);
    }, true);
  } catch (e) {}
  try {
    globalThis.addEventListener("unhandledrejection", function (ev) {
      var r = ev && ev.reason !== undefined ? ev.reason : "unhandled rejection";
      rec("unhandledrejection: " + ((r && r.message) || r));
    }, true);
  } catch (e) {}
  // onerror fallback, for the path where addEventListener itself is what is missing.
  try {
    var prior = globalThis.onerror;
    globalThis.onerror = function (msg, src, line) {
      // Only when the listener path is not doing the job -- otherwise every error is recorded twice
      // and the buckets double-count.
      if (!viaListener) rec(String(msg) + (src ? " (" + src + ":" + (line || 0) + ")" : ""));
      if (typeof prior === "function") { try { return prior.apply(this, arguments); } catch (e) {} }
      return false;
    };
  } catch (e) {}
})();
