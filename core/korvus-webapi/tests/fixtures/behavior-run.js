// The behavior-oracle runner, shared verbatim by both evaluators:
//
//   - `tests/quality.rs`, which runs it inside Korvus to observe what WE do; and
//   - `plugins/korvus-persona/capture/collect.js`, which runs it inside the capture page a person opens
//     in the browser we target (host.py serves both this file and the corpus), to record what the
//     target does. The CDP-driven and hand-served oracles that used to be two more evaluators were
//     retired on 2026-09-07; one rig records every artifact of a persona in one session.
//
// It lives in one file because the two results are compared directly: any difference in how the value
// is evaluated, narrowed to JSON, or how a thrown error is named would show up as a behavior
// difference that is really a harness difference. `jsonSafe` mirrors surface-dump.js for the same
// reason.
//
// Input:  globalThis.__KORVUS_CASES = [{ id, expr, await? }, ...]  (assigned by the caller before this runs)
// Output: EITHER a JSON string, OR -- only when at least one case is marked `await: true` -- a PROMISE
//         of that JSON string. Every caller must handle both; see "THE TWO SHAPES" below.
//         The JSON maps id -> { value } | { undef: true } | { threw: "<error name>" }
//                            | { unsupported: true } | { timeout: true }
//
// `undefined` is its own outcome, not `unsupported`. JSON cannot hold it, but it is the single most
// common WebAPI return value -- every void method, every absent named property -- so an oracle that
// could not record it would be blind to most of the surface it is meant to capture.
//
// ── ASYNC: WHY IT IS OPT-IN, AND WHY THE SYNC CORPUS IS UNTOUCHED ──────────────────────────────────
//
// For most of this project the runner was `const v = (0, eval)(c.expr)` and nothing else, so every
// question had to be answerable synchronously. That single limit is why a large family of members has
// never been asked at all: a promise-returning member could only ever be asked about its WRAPPER
// (`Object.prototype.toString.call(o.getBattery())` -> "[object Promise]"), and a receiver that
// arrives as a promise (`navigator.xr.requestSession("inline")`) failed its `instanceof` probe and
// took every case filed under it down with it.
//
// The fix is deliberately NOT a global flip to `await`. Awaiting every expression would silently
// re-record the ~137 members currently answered "[object Promise]" as their settled values -- a
// corpus-wide semantic change wearing the clothes of a re-record, invisible to the provenance system
// because the questions did not change. So a case OPTS IN with `await: true`, and a case without the
// flag runs through the identical code path it always did.
//
// THE TWO SHAPES. A corpus with no awaited case returns a STRING, synchronously, from the same loop as
// before -- the ~10,000 recorded cases are not merely equivalent under the new runner, they do not
// reach any new code. A corpus with at least one awaited case returns a PROMISE, because there is no
// honest way to make an asynchronous answer arrive synchronously.
//
// ORDER. The awaited cases run in a SECOND PASS, after every synchronous case has finished, rather
// than interleaved in file order. This is not tidiness. `plugins/korvus-persona/capture/collect.js` depends on both
// of its determinism passes and the frame teardown happening in ONE TASK, because four corpus
// expressions assign to `location` and a browser answers those by queueing a navigation that commits
// at the end of the task. Keeping every synchronous case inside the first task preserves that
// property exactly as it was. The cost is that a synchronous case authored AFTER an awaited one in the
// same file no longer observes the awaited one's side effects; author accordingly.
//
// DEADLINE. Each awaited case gets AWAIT_MS to settle and is otherwise recorded as `{ timeout: true }`
// -- its OWN outcome, never silently `undefined` and never quietly dropped as `unsupported`, because
// "this never settled" is a fact about the member and the two other answers are not. A promise that
// never settles would otherwise hang the whole batch, the way a lone surrogate once killed a chunk of
// thirty cases and named none of them.
(() => {
  const cases = globalThis.__KORVUS_CASES || [];
  const MAX_STR = 4096, MAX_ARR = 64;
  // HOW LONG ONE AWAITED CASE MAY TAKE TO SETTLE, in milliseconds.
  //
  // 1500 ms, and the number is a compromise between two failure modes rather than a measurement.
  // Too low and a real answer that is merely slow (a GPU adapter request, a storage estimate on a
  // cold profile) is recorded as a timeout -- a lie that the determinism filter would not even catch,
  // because a consistently-slow member times out consistently. Too high and one hung member costs the
  // file: the capture rig evaluates every case twice, each pass in a fresh frame, so the worst
  // case for a file of N never-settling cases is 4 * N * AWAIT_MS.
  //
  // Measured on this box, the awaited members that DO settle (`navigator.xr.requestSession("inline")`,
  // `navigator.storage.estimate()`, `caches.keys()`, `navigator.permissions.query(...)`) settle in
  // single-digit milliseconds, so the margin here is three orders of magnitude. If a member is ever
  // found that legitimately needs longer, raise this and say so -- do not special-case the member.
  const AWAIT_MS = 1500;
  // A LONE SURROGATE IS NOT A RECORDABLE ANSWER, and one of them used to take the whole chunk with it.
  //
  // JS strings are UTF-16 and may hold an unpaired surrogate; `JSON.stringify` passes it through; the
  // Rust side then cannot read the result as UTF-8, so `raw.as_str()` is None and the harness dies with
  // "behavior runner must return a JSON string" -- naming no case, for a chunk of thirty. One case
  // assigning '\ud800x' to a reflected attribute did exactly that, and it only surfaced when the value
  // stopped being laundered through Rust (which replaces it with U+FFFD) on its way back.
  //
  // So it is `unsupported`, the same answer an over-long string or an object already gets. That is the
  // honest outcome for a value this transport cannot carry, and it costs one case instead of thirty.
  const lone = (s) => {
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff) {
        const n = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
        if (!(n >= 0xdc00 && n <= 0xdfff)) return true;
        i++;
      } else if (c >= 0xdc00 && c <= 0xdfff) return true;
    }
    return false;
  };
  const jsonSafe = (val) => {
    try {
      if (val === null) return { has: true, value: null };
      const t = typeof val;
      if (t === "boolean" || t === "number") return { has: true, value: val };
      if (t === "string") return (val.length <= MAX_STR && !lone(val)) ? { has: true, value: val } : { has: false };
      if (Array.isArray(val)) {
        if (val.length > MAX_ARR) return { has: false };
        const out = [];
        for (const el of val) {
          const et = typeof el;
          if (el === null || et === "boolean" || et === "number") out.push(el);
          else if (et === "string") { if (el.length > MAX_STR || lone(el)) return { has: false }; out.push(el); }
          else return { has: false };
        }
        return { has: true, value: out };
      }
      return { has: false };
    } catch (e) { return { has: false }; }
  };
  // The name a thrown value is recorded under. Shared by the sync and the awaited path so that a
  // rejection and a throw of the same error are indistinguishable in the corpus -- which is the point:
  // `await` routes a rejection into the same catch, so `wakeLock.request("screen")` falls out as
  // `throws: "NotAllowedError"` for free rather than being dropped.
  const errName = (e) => (e && e.name) || String(e);
  // A settled value, narrowed to what this transport can carry. Identical for both paths.
  const carry = (v) => {
    if (v === undefined) return { undef: true };
    const s = jsonSafe(v);
    return s.has ? { value: s.value } : { unsupported: true };
  };

  const out = {};
  const awaited = [];
  // PASS ONE -- every synchronous case, in file order, in a single task. Byte-for-byte the loop this
  // file has always had.
  for (const c of cases) {
    if (c.await === true) { awaited.push(c); continue; }
    try {
      const v = (0, eval)(c.expr);
      if (v === undefined) { out[c.id] = { undef: true }; continue; }
      const s = jsonSafe(v);
      out[c.id] = s.has ? { value: s.value } : { unsupported: true };
    } catch (e) { out[c.id] = { threw: errName(e) }; }
  }
  // No awaited case: the answer is a string and the caller never has to know this file changed.
  if (!awaited.length) return JSON.stringify(out);

  // Reading `.then` can itself throw (a Proxy, a getter). A value whose thenability cannot be
  // determined is treated as an ordinary value, which lands it in `carry` and, being an object, in
  // `unsupported` -- the same place it would have landed under the synchronous runner.
  const isThenable = (v) => {
    try {
      return v !== null && (typeof v === "object" || typeof v === "function") && typeof v.then === "function";
    } catch (e) { return false; }
  };
  // A sentinel the page cannot forge: `Promise.race` reports which side won by identity, not by a
  // flag, so a member that legitimately resolves to something truthy is never mistaken for a timeout.
  const TIMED_OUT = {};
  // A realm with no timers gets no per-case deadline; the caller's own budget is then the only bound,
  // and both callers have one. Every realm the corpus is actually evaluated in (window, worker) has
  // `setTimeout`, so this is a guard, not a code path anyone should be relying on.
  const canDeadline = typeof setTimeout === "function";

  // PASS TWO -- the awaited cases, SEQUENTIALLY. Not `Promise.all`: cases mutate the page, and running
  // them concurrently would let one case's side effects land in the middle of another's answer, which
  // is a non-determinism the filter would blame on the member.
  return (async () => {
    for (const c of awaited) {
      let v;
      try { v = (0, eval)(c.expr); } catch (e) { out[c.id] = { threw: errName(e) }; continue; }
      if (!isThenable(v)) { out[c.id] = carry(v); continue; }
      let timer = 0;
      try {
        let raced;
        if (canDeadline) {
          const deadline = new Promise((res) => { timer = setTimeout(() => res(TIMED_OUT), AWAIT_MS); });
          raced = await Promise.race([Promise.resolve(v), deadline]);
        } else {
          raced = await v;
        }
        out[c.id] = raced === TIMED_OUT ? { timeout: true } : carry(raced);
      } catch (e) {
        out[c.id] = { threw: errName(e) };
      } finally {
        if (timer) { try { clearTimeout(timer); } catch (e) {} }
      }
    }
    return JSON.stringify(out);
  })();
})()
