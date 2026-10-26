//! The PLAUSIBILITY axis of the stealth board (axis ⑦), as a standing gate.
//!
//! Six axes preceded this one. Five walk PROTOTYPES and grade what a member LOOKS like; the sixth
//! (`instance_tells`) reads live instances and grades whether a member is native, stable, branded and
//! unbound. None of them ever asks whether an ANSWER is PLAUSIBLE, and that is precisely how
//! `MediaSource.isTypeSupported` -- which returned `false` for every string on earth -- was VERIFIED
//! on this board: the recorded corpus asks it exactly one question, `isTypeSupported("x") == false`,
//! and a constant `false` passes it.
//!
//! This gate asks each capability site a SET of inputs and fails only when the answers have
//! cardinality one. It asserts nothing about WHAT any member returns -- writing an expected answer
//! here would be forging evidence -- only that a member which takes an input does not ignore it.
//!
//! See `fixtures/constant-answers.js` for the roster, the four question shapes, and the list of sites
//! deliberately left off because a real browser's discrimination there is a belief rather than
//! recorded evidence.
use engine::Engine;

const PROBE: &str = include_str!("fixtures/constant-answers.js");

/// The page the probe measures against. The probe provisions its own elements (and removes them
/// again), so this only has to be a real document with a body and a laid-out box.
const PAGE: &str = r#"<!doctype html><html><head><style>
    body { margin: 0; font: 16px monospace }
  </style></head><body><div id="root">constant-answers fixture</div></body></html>"#;

/// What the probe reports: the findings, plus the split of which roster sites were actually asked.
#[derive(serde::Deserialize)]
struct Report {
    findings: Vec<String>,
    graded: Vec<String>,
    skipped: Vec<String>,
}

async fn constant_answers() -> Report {
    // The REAL captured identity, not a default persona: half this roster (WebGL extensions, the
    // plugin/mimeType vocabularies, the media codec lists) reads values the profile plugin owns, and
    // grading them against a stub identity would measure the wrong browser.
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    let raw =
        e.eval_value("constant-answers", PROBE.to_string()).expect("constant-answers probe threw");
    let json = raw.as_str().expect("constant-answers.js must return a JSON string");
    serde_json::from_str::<Report>(json)
        .expect("constant-answers.js must return {findings, graded, skipped}")
}

/// The roster sites that must be LIVE for this gate to mean anything.
///
/// Without this the axis has the failure mode every gate on this board has been burned by: a probe
/// that silently stops measuring reads as a passing probe. If a refactor makes `MediaSource` or `CSS`
/// unreachable from the probe, the guard goes falsy, the finding disappears, and the gate goes green
/// on a defect that got worse. So the sites carrying a KNOWN entry -- the ones this gate exists to
/// watch -- are asserted to have actually been asked.
const MUST_BE_GRADED: &[&str] = &[
    "MediaSource.isTypeSupported",
    "MediaRecorder.isTypeSupported",
    "HTMLVideoElement.canPlayType",
    "HTMLAudioElement.canPlayType",
    "CSS.supports(1-arg)",
    "CSS.supports(2-arg)",
    "ClipboardItem.supports",
    "link.relList.supports",
    "Element.checkVisibility",
    "document.fonts.check",
    // document.fonts.check WAS HERE and was the axis's one FALSE POSITIVE. The implementation was
    // already right: it throws SyntaxError on a shorthand it cannot parse and answers true
    // otherwise, which is what Brave 151 on this machine does -- including `12px "KorvusNoSuchFace"`
    // -> true, because check() asks whether the needed fonts are READY, not whether the family
    // exists. A missing family falls back, and the fallback is ready.
    //
    // The roster was asking three inputs that a CORRECT implementation answers identically, so the
    // axis called a working member a constant. Fixed by adding the input that shows the
    // discrimination (an unparseable shorthand), not by relaxing the check. Worth keeping this
    // paragraph: the axis's own brief warned that a noisy gate is worse than no gate, and this is
    // what its noise looked like -- a roster that did not ask the distinguishing question, which is
    // the same defect the whole axis exists to find, pointed at itself.
];

/// The constant answers this engine still gives, each spelled out in full.
///
/// This is a RATCHET, not a tolerance -- the same shape as `instance_tells`'s KNOWN and
/// `worker_surface`'s ceilings. The gate below fails on any finding NOT in this list, and equally on
/// any entry in this list the engine no longer produces. So the list can only shrink, and it cannot
/// be padded to make a new defect pass: adding a line here is a visible, reviewable claim that a
/// specific member is knowingly answering a constant, and every line must name its evidence.
const KNOWN: &[&str] = &[
    // ── the defect that motivated the axis, caught in the act of being "fixed" ───────────────────
    // EVIDENCE: `tests/fixtures/behavior-cases/MediaSource.json` records
    //   `MediaSource.isTypeSupported("x") == false` (brave-151-linux-headless)
    // -- one degenerate question, and the ONLY one the corpus asks of this member.
    //
    // This line is the whole argument for the axis, so the measurement is worth writing down. At this
    // branch's merge-base (8801b432) the member answered `undefined` to all five inputs and the
    // recorded case FAILED (`ours=undefined want=false`), so it graded verified=false. Merging master
    // (232356bd, "merge fix-media-rtc") changes the answer to `boolean:false` -- for the recorded
    // input AND for `video/mp4; codecs="avc1.42E01E"`, `video/webm; codecs="vp8"`, `audio/mpeg` and a
    // MIME type that is registered nowhere. The recorded case now passes. The member flips to
    // VERIFIED. It is exactly as wrong as it was, and a browser that answers "no" to every codec on
    // earth is a fingerprint in its own right.
    //
    // That is the failure this axis was built for, reproduced end to end: the corpus cannot tell a
    // fix from a constant, because the only question it asks is one a constant answers correctly.
    // EVIDENCE, and this one is now recorded on BOTH sides: `behavior-cases/MediaRecorder.json`
    // records `isTypeSupported("x") == false` AND `isTypeSupported("video/webm") == true` AND
    // `isTypeSupported("video/webm;codecs=vp8") == true` (brave-151-linux-headless). The capture
    // itself proves the browser discriminates. Ours answers `undefined` to all of them (2/7 cases
    // passing at the time of writing), so the behavior gate sees this one too -- it is on this list
    // because the constant is the same defect, not because it is invisible.
    // EVIDENCE: `behavior-cases/ClipboardItem.json` records `ClipboardItem.supports("x") == false`.
    // Ours is `undefined`. The two types below (text/plain, image/png) are the ones the Clipboard API
    // is defined over; a `supports` that answers `undefined` to all three implements nothing.
    // FOUR ENTRIES WERE DELETED FROM THIS LIST, and the deletion is the point of the ratchet.
    // MediaSource.isTypeSupported, MediaRecorder.isTypeSupported and HTMLVideo/AudioElement.canPlayType
    // all answered one constant to every input when this axis was written. The stub-sweep unit landed
    // real implementations in the same wave, the engine stopped producing those four findings, and the
    // ratchet FAILED until they were removed -- which is how a list that can only shrink is supposed to
    // behave. The axis caught the codec stubs before the corpus could, and then recorded their repair.
    // CSS.supports WAS HERE and is not any more. It answered `true` to every well-formed
    // declaration, `korvus-not-a-prop: 1` included, because its body was a regex over the SHAPE of
    // the argument and its comment said "No layout/style engine". The comment was stale: the style
    // engine has named every property it understands for a long time, and `CSS.supports` now asks it
    // (core/css/css_supports.rs -> page::style::property::PropertyId::from_name, 478 entries, plus
    // shorthands and `--custom` properties).
    //
    // The roster still asks `color: not-a-color`, and that is deliberate: the engine has a property
    // NAME table and no per-property VALUE grammar, so an unknown property is now correctly `false`
    // while an invalid value is still `true`. Answering `false` to an unknown property is knowledge
    // we have; answering `false` to an unparseable value is not, and inventing it would be the same
    // guess this repair removed. Measured against Brave 151 on this machine, 7 of 9 probed inputs
    // now agree, and the two that do not are exactly those value cases.
    // Element.checkVisibility WAS HERE. It answered `true` for a visible element and for a
    // `display:none` one alike, because its body stopped at connectedness under a comment saying the
    // rest "needs layout (korvus-page)". That was true when it was written and had not been for a
    // long time: getComputedStyle resolves through page::Page::computed_value on this same DOM, and
    // checkVisibility now walks the element and its ancestors through the same handle.
    // Second stale "no engine for this" comment repaired in a row, after CSS.supports.
    // link.relList.supports WAS HERE. It threw TypeError for every token on every list, because the
    // implementation applied `classList`'s rule -- correctly, classList HAS no supported-token
    // definition -- to all of them. `rel` and `sandbox` do have one, and the two `rel` sets are
    // DISJOINT: <link> supports stylesheet/preload/prefetch/icon/alternate/dns-prefetch/
    // modulepreload/manifest and NOT noopener; <a>, <area> and <form> support noopener/noreferrer/
    // opener and none of the others. Probed on Brave 151 on this machine rather than read off a spec.
    // ClipboardItem.supports WAS HERE, answering `undefined` -- falsy -- to every media type,
    // because the generated shell gave it the right shape and no body. A page feature-detecting the
    // clipboard read that as "no type is supported at all". Now answers the mandatory list
    // (text/plain, text/html, image/png, image/svg+xml, plus the `web ` custom-format prefix),
    // probed on Brave 151 on this machine: image/jpeg, text/uri-list and application/json are all
    // false there, so the list is the browser's and not the spec's optimistic superset.
    // ── media type queries pinned to "" ──────────────────────────────────────────────────────────
    // EVIDENCE: the capture, and the implementation's own comment.
    //   `behavior-cases/HTMLMediaElement.json` now records
    //     video.canPlayType("video/mp4")                      == "maybe"
    //     video.canPlayType("application/x-korvus-nonsense")  == ""
    //   (brave-151-linux-headless) -- the recorded browser answers TWO different strings, which is
    //   the discrimination this line says is missing. And the implementation states the constant
    //   outright, core/korvus-webapi/src/js/dom/08z-media-element.js:196:
    //     "Korvus has no decoders, so "" ... is the truthful answer for every input"
    //     KB.__mediaCanPlayType = function () { return ""; };
    // Truthful about the decoder situation and a loud fingerprint all the same: `canPlayType` is one
    // of the oldest capability probes on the web. At this branch's merge-base the corpus recorded NO
    // canPlayType case at all -- `tested: false` -- so nothing on the board could see it; the axis
    // flagged it before the evidence for it existed.
    // ── CSS.supports answers yes to everything ───────────────────────────────────────────────────
    // EVIDENCE: the implementation says so in its own comment,
    //   core/korvus-webapi/src/js/dom/06-misc.js:45
    //   supports(prop, value) { // No layout/style engine; accept well-formed input conservatively.
    //     if (value === undefined) return /^[a-z-]+\s*:\s*.+$/i.test(String(prop));
    //     return typeof prop === "string" && prop.length > 0 && value != null && String(value).length > 0; }
    // Every syntactically well-formed declaration answers `true`, including `korvus-not-a-prop: 1`
    // and `color: korvus-not-a-color`. `CSS.supports` is the single most-called capability query on
    // the web and its entire contract is to say NO to things this UA cannot do; a UA that says yes to
    // a property that has never existed in any browser is trivially separable from every real one.
    // (The engine now HAS a style engine -- the `page` crate's cssparser pipeline -- so the comment's
    // premise is also out of date.)
    // ── checkVisibility ignores visibility ───────────────────────────────────────────────────────
    // EVIDENCE: `behavior-cases/Element.json` records
    //   `(() => { const o = document.createElement("div"); return o.checkVisibility(); })() == false`
    // -- the capture proves this method returns `false` for at least one receiver. Ours answers
    // `true` for an ATTACHED element and `true` again for one with `display:none`, which is the one
    // pair the method exists to separate. Whether ours also passes the recorded detached case is
    // beside the point: a `checkVisibility` that cannot see `display:none` is not implemented.
    // ── DOMTokenList.supports throws for every list ──────────────────────────────────────────────
    // EVIDENCE, and this is the WEAKEST entry on the list -- stated plainly so the next unit can
    // re-judge it. The capture is SILENT on `link.relList.supports`: the only recorded
    // `DOMTokenList.supports` cases are on `classList`, where a TypeError is CORRECT (classList has
    // no supported-token set) and where this engine is right. What the evidence does show is that
    // this engine's implementation never considered any other list:
    //   core/korvus-webapi/src/core/dom/dom_token_list.rs:145
    //   fn cb_dtl_supports(...) { // classList has no "supported tokens" definition, so
    //                             // DOMTokenList.supports() always throws TypeError (per spec)
    // -- it throws unconditionally, on a receiver whose `rel` attribute DOES define a supported token
    // set (HTML "the link element", the `rel` keyword registry), which is the only reason
    // `DOMTokenList.supports` exists. So: a knowing stub, generalised from the wrong receiver. If a
    // future capture records `link.relList.supports("stylesheet")` throwing, delete this line and the
    // roster entry -- THE RECORDING WINS.
    // ── document.fonts.check says yes to a font nothing has ──────────────────────────────────────
    // EVIDENCE: this site was SKIPPED at the merge-base (`document.fonts` did not exist) and became
    // reachable when master landed FontFaceSet -- so the finding is new surface, not a regression in
    // old surface. `FontFaceSet.check` is the sanctioned font-enumeration API. The capture's
    // `behavior-cases/FontFaceSet.json` records only the two MALFORMED calls -- `check("x")` throws
    // SyntaxError, `check()` throws TypeError -- and this engine passes both, which is what turned
    // the member verified=true/independent=true on master. Hand it three WELL-FORMED font shorthands
    // and it answers `true` to `12px monospace`, `12px sans-serif` AND `12px "KorvusNoSuchFace"`
    // alike: the corpus only ever asks how it rejects garbage, never what it says about a font. A UA
    // that claims every
    // font name ever asked for is separable from a real one in a single call, and font enumeration is
    // one of the highest-entropy signals a fingerprinter has. The `present` axis cannot see it (the
    // member is there and native) and the corpus cannot see it (one input).
];

#[tokio::test]
async fn no_capability_query_ignores_its_input() {
    let report = constant_answers().await;
    let findings = &report.findings;
    println!(
        "  constant-answers: {} (asked {} roster sites, skipped {}: {:?})",
        findings.len(),
        report.graded.len(),
        report.skipped.len(),
        report.skipped
    );
    println!("{findings:#?}");
    let asked: std::collections::BTreeSet<&str> =
        report.graded.iter().map(String::as_str).collect();
    let unasked: Vec<&&str> = MUST_BE_GRADED.iter().filter(|s| !asked.contains(**s)).collect();
    assert!(
        unasked.is_empty(),
        "{} roster site(s) this gate exists to watch were NOT asked -- their guard went falsy, so \
         the probe measured nothing and would have read GREEN on a worse engine: {:#?}\n\
         Either the site moved (fix its guard in fixtures/constant-answers.js) or the interface \
         disappeared (that is the `present` axis's finding, and this list should shrink with it).",
        unasked.len(),
        unasked
    );
    let known: std::collections::BTreeSet<&str> = KNOWN.iter().copied().collect();
    let got: std::collections::BTreeSet<&str> = findings.iter().map(String::as_str).collect();
    let fresh: Vec<&&str> = got.difference(&known).collect();
    let gone: Vec<&&str> = known.difference(&got).collect();
    assert!(
        fresh.is_empty(),
        "{} NEW constant-answer finding(s): {:#?}\n\
         Each line is a member that was handed inputs a real browser separates and returned the same \
         answer to all of them. That is the `MediaSource.isTypeSupported` defect: green on all six \
         other axes, VERIFIED by a corpus that only ever asked one degenerate question, and wrong.\n\
         Implement the discrimination. Do NOT add the line to KNOWN unless you are recording a \
         knowingly-outstanding defect with its evidence -- that list is a ratchet that only shrinks. \
         If you believe the site legitimately answers a constant, the fix is to DELETE it from the \
         roster in fixtures/constant-answers.js with a comment saying why, next to the other sites \
         that were left off for the same reason.",
        fresh.len(),
        fresh
    );
    assert!(
        gone.is_empty(),
        "KNOWN lists {} finding(s) the engine no longer produces -- delete them from KNOWN so the \
         ratchet stays tight: {:#?}",
        gone.len(),
        gone
    );
}
