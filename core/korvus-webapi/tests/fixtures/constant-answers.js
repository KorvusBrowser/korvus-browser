//! `constant-answers.js` -- the PLAUSIBILITY axis of the stealth board (axis ⑦).
//!
//! WHY THIS EXISTS. Six axes now grade this engine. Five walk PROTOTYPES and ask what a member LOOKS
//! like (`scan_tells`: own-tells, name-defects, non-native constructors, cross-layer incoherences,
//! structural diffs). The sixth (`instance-tells.js`) reads LIVE INSTANCES and asks whether a member
//! is native, stable, branded and unbound. **Not one of the six asks whether an ANSWER is PLAUSIBLE.**
//!
//! That is how `MediaSource.isTypeSupported` sat on the board as VERIFIED while returning `false`
//! unconditionally. The recorded corpus asks it exactly one question --
//!
//!     MediaSource.isTypeSupported("x")   =>  false      (brave-151-linux-headless)
//!
//! -- and a function that answers `false` to every string on earth passes that case. The member is
//! green on every axis we own, and a browser that claims to support no media type at all is itself a
//! loud fingerprint. The same one-degenerate-question shape is all the corpus has for
//! `MediaRecorder.isTypeSupported("x")`, `HTMLScriptElement.supports("x")` and
//! `ClipboardItem.supports("x")` -- three more predicates a constant `false` satisfies.
//!
//! THE QUESTION THIS AXIS ASKS, and the reason it is allowed to exist. The campaign rule is absolute:
//! **you may write the QUESTION, you may never write the ANSWER.** So this probe asserts nothing
//! about what any member returns. It asks each site a SET of inputs and reports only when the answers
//! have CARDINALITY ONE -- when a member that takes an input ignores it. "These N answers are not all
//! the same" is a structural claim, not a recorded expectation: no `expected` is written down here,
//! and the probe stays correct even if every real answer changes.
//!
//! Four shapes of question, per the axis's charter:
//!   P. PREDICATE   a support/capability query, asked over inputs a browser certainly separates
//!                  (a codec it plays and a MIME type that does not exist).
//!   R. RECEIVER    the same getter read from receivers that DIFFER in the property being read.
//!   D. DERIVED     a measurement whose whole purpose is to vary (text metrics, layout, entropy).
//!   V. VOCABULARY  a lookup over a name a browser has and a name nothing has.
//!
//! WHAT THIS AXIS IS NOT. It is not a correctness oracle -- it cannot tell you WHICH answer is right,
//! only that a member is not answering at all. And it is deliberately NARROW: every site on the roster
//! below is one where a real browser's discrimination is not in doubt. A site that answers a genuine
//! constant does not belong on the roster at all, and the header of each entry says why the browser
//! must discriminate there. Entries removed for exactly that reason are recorded at the bottom of the
//! roster so nobody re-adds them.
//!
//! ITS TWO LIMITS, stated so nobody trusts it further than it goes.
//!   1. Cardinality one is coarse. A site can be badly wrong and still vary -- `checkVisibility` here
//!      DOES separate a detached element from an attached one (the recorded case passes) and is blind
//!      to `display:none`, and it only registers because the two roster inputs are the pair the method
//!      exists to separate. Adding a third input that happens to vary would SILENCE a real finding, so
//!      inputs are chosen to isolate one discrimination, not to cover a member.
//!   2. It only reaches what somebody put on the roster. The roster's growth path is the corpus's own
//!      blind spots: a sweep of `tests/fixtures/behavior-cases/` finds 44 members whose name says they
//!      answer a capability question and whose recorded cases call them with exactly ONE distinct
//!      argument (usually the literal `"x"`). Those 44 are the places where a constant answer is
//!      VERIFIED by construction, and they are where the next entries should come from.
//!
//! Returns JSON `{ findings: [...], graded: [...], skipped: [...] }`.
//!   `findings`  one string per constant-answer site. Empty is the passing state; the gate that reads
//!               this (`tests/constant_answers.rs`) carries the ratchet of what is still outstanding.
//!   `graded`    every roster site whose guard was live, so the probe actually asked it. A gate that
//!               cannot say what it MEASURED is a gate that can silently shrink to nothing -- the
//!               failure `instance-tells.js` calls out in its own header ("provision instead of
//!               skip"). Here a site CAN legitimately be skipped (the interface is absent, which is
//!               the `present` axis's finding), so the probe reports the split instead of hiding it.
//!   `skipped`   roster sites whose guard was falsy; nothing is asserted about them.
(function () {
  const tells = [];
  const graded = [];
  const skipped = [];

  // ── serialisation ────────────────────────────────────────────────────────────────────────────
  // Two answers are "the same" iff this renders them identically. Primitives compare by type+value;
  // objects compare by brand plus JSON content where JSON can see any (DOMRect, a capabilities
  // dictionary, an array), and by brand alone where it cannot -- so an object-vs-null or an
  // object-vs-different-brand difference always registers, and two opaque instances of the same brand
  // read as equal. That asymmetry is deliberate: this axis only ever fires on ZERO variation, so a
  // coarse comparator can lose a finding but can never invent one.
  const tag = (v) => Object.prototype.toString.call(v);
  const repr = (v) => {
    if (v === null) return "null";
    const t = typeof v;
    if (t === "undefined") return "undefined";
    if (t === "number") return "number:" + (Object.is(v, -0) ? "-0" : String(v));
    if (t !== "object" && t !== "function") return t + ":" + String(v);
    let body = "";
    try {
      const s = JSON.stringify(v, (k, x) => (typeof x === "bigint" ? "bigint:" + String(x) : x));
      if (typeof s === "string") body = s;
    } catch (e) {}
    return tag(v) + body;
  };
  const answer = (thunk) => {
    try {
      return repr(thunk());
    } catch (e) {
      // The error's NAME only. A message can carry the input verbatim ("'x' is not a valid MIME
      // type"), which would make a uniformly-throwing stub look like it discriminates.
      return "throws " + ((e && e.name) || String(e));
    }
  };

  // ── the fixture the probe measures against ───────────────────────────────────────────────────
  // Self-provisioning, exactly like `instance-tells.js`: the probe runs against whatever page the
  // caller loaded, and a site that silently skipped because the page lacked an element is the failure
  // mode this whole board exists to avoid. Everything is created here and torn down at the end.
  let fx = null, narrow = null, wide = null, hidden = null, req = null, link = null, canvas = null;
  try {
    fx = document.createElement("div");
    fx.setAttribute("id", "korvus-constant-answers-fixture");
    fx.setAttribute("style", "position:absolute;left:0;top:0;width:800px;font:16px monospace");
    fx.innerHTML =
      '<span id="ca-narrow" style="display:inline-block;width:10px;height:10px;color:rgb(255,0,0)"></span>' +
      '<span id="ca-wide" style="display:inline-block;width:300px;height:40px;color:rgb(0,0,255)"></span>' +
      '<span id="ca-hidden" style="display:none"></span>' +
      '<form><input id="ca-req" type="text" required></form>';
    (document.body || document.documentElement).appendChild(fx);
    narrow = document.getElementById("ca-narrow");
    wide = document.getElementById("ca-wide");
    hidden = document.getElementById("ca-hidden");
    req = document.getElementById("ca-req");
    link = document.createElement("link");
    canvas = document.createElement("canvas");
    canvas.width = 200;
    canvas.height = 50;
  } catch (e) {
    return JSON.stringify({
      findings: ["fixture: could not be provisioned -- " + e],
      graded: [],
      skipped: [],
    });
  }
  const gl = (kind) => {
    try {
      const c = document.createElement("canvas");
      c.width = 16; c.height = 16;
      return c.getContext(kind);
    } catch (e) { return null; }
  };
  const ctx2d = (() => { try { return canvas.getContext("2d"); } catch (e) { return null; } })();

  // ── THE ROSTER ───────────────────────────────────────────────────────────────────────────────
  // [shape, label, guard, [[inputLabel, thunk], ...]]
  //   `guard` returns falsy when the SITE ITSELF is absent. Absence is the `present` axis's finding,
  //   not this one's, and a probe that fired on a missing constructor would duplicate 7,376 members'
  //   worth of grading into this list. A site that EXISTS and throws uniformly is a finding.
  const roster = [

    // P ── media capability. Every one of these is a codec query. A browser that ships a media stack
    // answers `true` for at least one of the containers/codecs below and `false` for a MIME type that
    // is not registered anywhere; the corpus's single `("x")` case cannot see the difference.
    ["P", "MediaSource.isTypeSupported", () => typeof MediaSource === "function" && typeof MediaSource.isTypeSupported === "function", [
      ['"x"', () => MediaSource.isTypeSupported("x")],
      ['video/mp4 avc1', () => MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"')],
      ['video/webm vp8', () => MediaSource.isTypeSupported('video/webm; codecs="vp8"')],
      ['audio/mpeg', () => MediaSource.isTypeSupported("audio/mpeg")],
      ['application/x-not-a-type', () => MediaSource.isTypeSupported("application/x-not-a-type")],
    ]],
    ["P", "MediaRecorder.isTypeSupported", () => typeof MediaRecorder === "function" && typeof MediaRecorder.isTypeSupported === "function", [
      ['"x"', () => MediaRecorder.isTypeSupported("x")],
      ['video/webm vp8', () => MediaRecorder.isTypeSupported('video/webm;codecs=vp8')],
      ['audio/webm opus', () => MediaRecorder.isTypeSupported('audio/webm;codecs=opus')],
      ['application/x-not-a-type', () => MediaRecorder.isTypeSupported("application/x-not-a-type")],
    ]],
    ["P", "HTMLVideoElement.canPlayType", () => typeof document.createElement("video").canPlayType === "function", [
      ['video/mp4 avc1', () => document.createElement("video").canPlayType('video/mp4; codecs="avc1.42E01E"')],
      ['video/webm vp8', () => document.createElement("video").canPlayType('video/webm; codecs="vp8"')],
      ['application/x-not-a-type', () => document.createElement("video").canPlayType("application/x-not-a-type")],
      ['"x"', () => document.createElement("video").canPlayType("x")],
    ]],
    ["P", "HTMLAudioElement.canPlayType", () => typeof document.createElement("audio").canPlayType === "function", [
      ['audio/mpeg', () => document.createElement("audio").canPlayType("audio/mpeg")],
      ['audio/ogg vorbis', () => document.createElement("audio").canPlayType('audio/ogg; codecs="vorbis"')],
      ['application/x-not-a-type', () => document.createElement("audio").canPlayType("application/x-not-a-type")],
    ]],

    // P ── CSS support. `CSS.supports` is the single most-called capability query on the web. A UA
    // that answers the same thing to `color: red` and to a property that has never existed cannot
    // render the pages it claims to.
    ["P", "CSS.supports(1-arg)", () => typeof CSS === "object" && CSS && typeof CSS.supports === "function", [
      ['color: red', () => CSS.supports("color: red")],
      ['display: grid', () => CSS.supports("display: grid")],
      ['korvus-not-a-prop: 1', () => CSS.supports("korvus-not-a-prop: 1")],
      ['color: not-a-color', () => CSS.supports("color: korvus-not-a-color")],
    ]],
    ["P", "CSS.supports(2-arg)", () => typeof CSS === "object" && CSS && typeof CSS.supports === "function", [
      ['(color, red)', () => CSS.supports("color", "red")],
      ['(display, grid)', () => CSS.supports("display", "grid")],
      ['(korvus-not-a-prop, 1)', () => CSS.supports("korvus-not-a-prop", "1")],
      ['(color, korvus-not-a-color)', () => CSS.supports("color", "korvus-not-a-color")],
    ]],

    // P ── script types. The corpus records only `HTMLScriptElement.supports("x") == false`. The three
    // type strings below are the whole vocabulary the method exists to report on; a browser that runs
    // a classic script answers `true` to at least one.
    ["P", "HTMLScriptElement.supports", () => typeof HTMLScriptElement === "function" && typeof HTMLScriptElement.supports === "function", [
      ['classic', () => HTMLScriptElement.supports("classic")],
      ['module', () => HTMLScriptElement.supports("module")],
      ['importmap', () => HTMLScriptElement.supports("importmap")],
      ['"x"', () => HTMLScriptElement.supports("x")],
    ]],

    // P ── clipboard payload types. Same shape: the corpus records only the `("x")` case.
    ["P", "ClipboardItem.supports", () => typeof ClipboardItem === "function" && typeof ClipboardItem.supports === "function", [
      ['text/plain', () => ClipboardItem.supports("text/plain")],
      ['image/png', () => ClipboardItem.supports("image/png")],
      ['"x"', () => ClipboardItem.supports("x")],
    ]],

    // V ── token vocabularies. `DOMTokenList.supports` is defined ONLY for a list with a supported
    // token set, which is why the corpus's `classList.supports` cases all record a TypeError -- and
    // exactly why `relList`/`sandbox` (which DO have one) are the sites that carry information.
    ["V", "link.relList.supports", () => link && link.relList && typeof link.relList.supports === "function", [
      ['stylesheet', () => link.relList.supports("stylesheet")],
      ['preload', () => link.relList.supports("preload")],
      ['korvus-not-a-rel', () => link.relList.supports("korvus-not-a-rel")],
    ]],
    ["V", "navigator.plugins.namedItem", () => navigator.plugins && typeof navigator.plugins.namedItem === "function", [
      ['PDF Viewer', () => navigator.plugins.namedItem("PDF Viewer")],
      ['Chrome PDF Viewer', () => navigator.plugins.namedItem("Chrome PDF Viewer")],
      ['korvus-not-a-plugin', () => navigator.plugins.namedItem("korvus-not-a-plugin")],
    ]],
    ["V", "navigator.mimeTypes.namedItem", () => navigator.mimeTypes && typeof navigator.mimeTypes.namedItem === "function", [
      ['application/pdf', () => navigator.mimeTypes.namedItem("application/pdf")],
      ['korvus/not-a-type', () => navigator.mimeTypes.namedItem("korvus/not-a-type")],
    ]],
    // V ── the rest of the corpus's one-argument capability members. A sweep of
    // `tests/fixtures/behavior-cases/` finds 44 members whose name says they answer a capability
    // question and whose recorded cases call them with exactly ONE distinct argument (usually the
    // literal `"x"`) -- 44 places a constant answer is VERIFIED by construction. These are the ones
    // whose second input can be supplied without asserting anything about the first.
    ["V", "Element.matches", () => narrow && typeof narrow.matches === "function", [
      ['own tag', () => narrow.matches("span")],
      ['own id', () => narrow.matches("#ca-narrow")],
      ['korvus-no-such-tag', () => narrow.matches("korvus-no-such-tag")],
      ['"x"', () => narrow.matches("x")],
    ]],
    ["V", "Element.hasAttribute", () => narrow && typeof narrow.hasAttribute === "function", [
      ['id (present)', () => narrow.hasAttribute("id")],
      ['style (present)', () => narrow.hasAttribute("style")],
      ['korvus-absent', () => narrow.hasAttribute("korvus-absent")],
    ]],
    ["V", "Storage.getItem", () => typeof localStorage === "object" && localStorage && typeof localStorage.getItem === "function", [
      ['a key that was set', () => { localStorage.setItem("korvus-ca", "v"); return localStorage.getItem("korvus-ca"); }],
      ['a key that was not', () => localStorage.getItem("korvus-ca-absent")],
    ]],
    ["V", "HTMLCanvasElement.getContext", () => typeof document.createElement("canvas").getContext === "function", [
      ['2d', () => { const c = document.createElement("canvas"); return c.getContext("2d"); }],
      ['webgl', () => { const c = document.createElement("canvas"); return c.getContext("webgl"); }],
      ['korvus-not-a-context', () => { const c = document.createElement("canvas"); return c.getContext("korvus-not-a-context"); }],
    ]],
    // R ── `getParameter` is the WebGL fingerprint surface: four different pnames, four different
    // strings, all owned by the profile plugin. One string for all four is a stub AND a tell.
    ["R", "WebGLRenderingContext.getParameter", () => !!gl("webgl"), [
      ['VENDOR', () => { const g = gl("webgl"); return g.getParameter(g.VENDOR); }],
      ['RENDERER', () => { const g = gl("webgl"); return g.getParameter(g.RENDERER); }],
      ['VERSION', () => { const g = gl("webgl"); return g.getParameter(g.VERSION); }],
      ['SHADING_LANGUAGE_VERSION', () => { const g = gl("webgl"); return g.getParameter(g.SHADING_LANGUAGE_VERSION); }],
    ]],
    ["V", "URL.canParse", () => typeof URL === "function" && typeof URL.canParse === "function", [
      ['https://example.com/', () => URL.canParse("https://example.com/")],
      ['not a url', () => URL.canParse("::::")],
    ]],
    ["V", "document.fonts.check", () => document.fonts && typeof document.fonts.check === "function", [
      ['12px monospace', () => document.fonts.check("12px monospace")],
      ['12px sans-serif', () => document.fonts.check("12px sans-serif")],
      ['12px KorvusNoSuchFace', () => document.fonts.check('12px "KorvusNoSuchFace"')],
      // The input that shows the discrimination. `check()` answers TRUE for a font family that does
      // not exist -- verified on Brave 151 on this machine -- because it asks whether the fonts
      // needed are READY, not whether the family exists; a missing family falls back and is ready.
      // The only thing it distinguishes is a shorthand it cannot parse, which is a SyntaxError.
      // Without this row the roster asked three inputs that a correct implementation answers
      // identically, and the axis reported the member as a constant when it is not one.
      ['not-a-font-shorthand', () => document.fonts.check("not-a-font-shorthand")],
    ]],
    ["V", "WebGLRenderingContext.getExtension", () => !!gl("webgl"), [
      ['WEBGL_debug_renderer_info', () => { const g = gl("webgl"); return g.getExtension("WEBGL_debug_renderer_info"); }],
      ['OES_texture_float', () => { const g = gl("webgl"); return g.getExtension("OES_texture_float"); }],
      ['KORVUS_not_an_extension', () => { const g = gl("webgl"); return g.getExtension("KORVUS_not_an_extension"); }],
    ]],

    // R ── the same getter, receivers that differ in the property being read. A `getPropertyValue`
    // that returns one literal regardless of which property it was handed is the CSSOM's version of
    // the `isTypeSupported` defect, and no prototype scan can see it.
    ["R", "getComputedStyle().getPropertyValue", () => typeof getComputedStyle === "function" && narrow && wide, [
      ['narrow.color', () => getComputedStyle(narrow).getPropertyValue("color")],
      ['wide.color', () => getComputedStyle(wide).getPropertyValue("color")],
      ['narrow.display', () => getComputedStyle(narrow).getPropertyValue("display")],
      ['hidden.display', () => getComputedStyle(hidden).getPropertyValue("display")],
    ]],
    ["R", "element.style.getPropertyValue", () => narrow && narrow.style, [
      ['width', () => narrow.style.getPropertyValue("width")],
      ['height', () => narrow.style.getPropertyValue("height")],
      ['color', () => narrow.style.getPropertyValue("color")],
      ['korvus-not-a-prop', () => narrow.style.getPropertyValue("korvus-not-a-prop")],
    ]],
    ["R", "Element.checkVisibility", () => wide && typeof wide.checkVisibility === "function", [
      ['visible', () => wide.checkVisibility()],
      ['display:none', () => hidden.checkVisibility()],
    ]],
    ["R", "HTMLInputElement.checkValidity", () => req && typeof req.checkValidity === "function", [
      ['required + empty', () => { req.value = ""; return req.checkValidity(); }],
      ['required + filled', () => { req.value = "filled"; return req.checkValidity(); }],
      ['type=email + garbage', () => { const i = document.createElement("input"); i.type = "email"; i.value = "nope"; return i.checkValidity(); }],
    ]],

    // D ── measurements whose entire reason to exist is to vary. A stub that pins them is both a
    // correctness defect and, because these are the classic fingerprinting reads, a stealth defect.
    ["D", "CanvasRenderingContext2D.measureText().width", () => ctx2d && typeof ctx2d.measureText === "function", [
      ['"i"', () => ctx2d.measureText("i").width],
      ['"mmmmmmmmmm"', () => ctx2d.measureText("mmmmmmmmmm").width],
      ['""', () => ctx2d.measureText("").width],
    ]],
    ["D", "Element.getBoundingClientRect().width", () => narrow && wide, [
      ['10px span', () => narrow.getBoundingClientRect().width],
      ['300px span', () => wide.getBoundingClientRect().width],
      ['display:none span', () => hidden.getBoundingClientRect().width],
    ]],
    ["D", "matchMedia().matches", () => typeof matchMedia === "function", [
      ['(min-width: 1px)', () => matchMedia("(min-width: 1px)").matches],
      ['(min-width: 999999px)', () => matchMedia("(min-width: 999999px)").matches],
      ['(min-width: 1px) and (max-width: 0px)', () => matchMedia("(min-width: 1px) and (max-width: 0px)").matches],
    ]],
    ["D", "crypto.getRandomValues", () => typeof crypto === "object" && crypto && typeof crypto.getRandomValues === "function", [
      // Two draws from a CSPRNG collide with probability 2^-256. A constant here is a stub.
      ['draw #1', () => Array.from(crypto.getRandomValues(new Uint8Array(32))).join(",")],
      ['draw #2', () => Array.from(crypto.getRandomValues(new Uint8Array(32))).join(",")],
    ]],
    ["D", "canvas.toDataURL", () => ctx2d && typeof canvas.toDataURL === "function", [
      ['blank', () => { const c = document.createElement("canvas"); c.width = 40; c.height = 20; return c.toDataURL().length + ":" + c.toDataURL().slice(0, 64); }],
      ['painted', () => { const c = document.createElement("canvas"); c.width = 40; c.height = 20; const x = c.getContext("2d"); x.fillStyle = "#f00"; x.fillRect(0, 0, 40, 20); return c.toDataURL().length + ":" + c.toDataURL().slice(0, 64); }],
    ]],

    // ── SITES CONSIDERED AND DELIBERATELY LEFT OFF THE ROSTER ────────────────────────────────────
    // Each of these looks like it belongs and does not. Recorded so the next unit does not re-add it.
    //
    //   `Notification.permission`, `navigator.cookieEnabled`, `navigator.pdfViewerEnabled`,
    //   `document.implementation.hasFeature` -- there is no input to vary. A single-valued read
    //   cannot be "constant across inputs"; whether its one value is right is the behavior corpus's
    //   job, and the corpus records all four.
    //
    //   `document.queryCommandSupported` -- the ONLY recorded evidence is `new Document()`, where the
    //   capture records InvalidStateError for every argument, including a valid command. On the live
    //   document the capture is silent, so "it must discriminate here" is a belief, not evidence.
    //   Leaving it off; adding it would be this axis's first hand-written answer.
    //
    //   `performance.now()` read twice -- Brave COARSENS the clock, so two reads legitimately return
    //   the same number. A gate that fires on that is a gate everyone learns to ignore.
    //
    //   `speechSynthesis.getVoices()`, `PerformanceObserver.supportedEntryTypes`,
    //   `PushManager.supportedContentEncodings` -- one input each, and the capture already records
    //   the exact list, so the behavior corpus grades them properly. Nothing for this axis to add.
  ];

  for (const entry of roster) {
    const shape = entry[0], label = entry[1], guard = entry[2], inputs = entry[3];
    let live = false;
    try { live = !!guard(); } catch (e) { live = false; }
    if (!live) { skipped.push(label); continue; } // absent site -> the `present` axis's finding.
    graded.push(label);

    const answers = inputs.map((p) => answer(p[1]));
    const distinct = new Set(answers);
    if (distinct.size > 1) continue;

    const only = answers[0];
    tells.push(
      shape + " " + label + ": one answer " + (only.length > 90 ? only.slice(0, 90) + "…" : only) +
      " for all " + inputs.length + " inputs (" + inputs.map((p) => p[0]).join(", ") + ")"
    );
  }

  try { if (fx && fx.parentNode) fx.parentNode.removeChild(fx); } catch (e) {}

  return JSON.stringify({ findings: tells, graded: graded, skipped: skipped });
})()
