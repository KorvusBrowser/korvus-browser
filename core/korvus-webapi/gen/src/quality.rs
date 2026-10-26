//! The `ModuleQuality` aggregator — the single per-module quality report (the `webapi quality`
//! system).
//!
//! [`assess`] grades every WebIDL member of one interface on the five axes, diffing the runtime
//! surface [`SurfaceSnapshot`] against the three captured references, then rolls the per-member
//! verdicts up into one interface [`verdict`](ModuleQuality::verdict):
//!
//!   ① present  — the member is on Korvus's live prototype (`snapshot.member(iface,name).is_some()`).
//!   ② native   — `[native code]`, no shim tell, correct native fn length (ported from `surface_parity`
//!                / `nolies`).
//!   ③ shape    — kind / descriptor / length / enumerable / configurable == the Brave-151 baseline.
//!   ④ value    — the member's live value == the captured profile (only for mapped scalars).
//!   ⑤ wpt      — the interface's WPT bucket (per-interface, applied to each covered member).
//!
//! Verdict precedence: **Incomplete** (any member absent) ▸ **NotNative** (any present member fails ②)
//! ▸ **ValueDrift** (any mapped value differs) ▸ **LowQuality** (any shape/wpt failure) ▸ **Complete**.
//!
//! Anonymous specials (`getter DOMString (DOMString name);` — the named-property machinery) are not
//! prototype members, so the surface axes cannot see them: they are graded ONLY through the ⑥ behavior
//! axis, keyed by their synthetic names ("named getter" / "named setter" / "named deleter"). An
//! untested special contributes nothing (interfaces without cases keep their verdict); a passing case
//! is its presence proof AND its verification; a failing case drives behavior-fail.

use serde::Serialize;
use serde_json::Value;

use crate::profile::Profile;
use crate::snapshot::{CtorRec, MemberRec, SurfaceSnapshot};
use crate::spec::{InterfaceSpec, MemberSpec};
use crate::wpt::Bucket;

/// The one per-module report an Implement/Enhance agent (and the harness gate) consumes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModuleQuality {
    pub iface: String,
    pub members: Vec<MemberQuality>,
    /// Interface-level: the constructor's own nativeness (subsumes `nolies`'s ctor half).
    pub ctor: CtorAxis,
    /// Interface-level: extra/missing prototype members + ctor name / proto chain / toStringTag vs the
    /// baseline (subsumes `surface_parity`'s structural diff).
    pub structure: Structure,
    pub rollup: Rollup,
    /// `verified` (surface-complete + every member dynamically verified) | `complete` (surface-complete
    /// but some members behavior/value-UNVERIFIED) | `incomplete` | `not-native` | `behavior-fail` |
    /// `value-drift` | `low-quality`.
    pub verdict: String,
    pub worklist: Vec<String>,
    /// (E) Failing upstream WPT subtests for this interface, verbatim. Filled by the CLI (which holds
    /// the raw dump); `assess` itself only sees aggregated buckets. Handed to the implementer as its
    /// OPENING worklist so it starts from what the spec actually demands rather than from what it
    /// imagined to test — the blind spot that let `DOMRect` ship without string coercion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wpt_failures: Vec<String>,
    /// Blink's own implementation of this interface, `.cc` first — where to read the SEMANTICS that
    /// WebIDL does not carry (`sort()` exists, but what does it sort?). Filled by the CLI when
    /// `KORVUS_BLINK_ROOT` names a checkout. Reference only: read it, never translate it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blink_refs: Vec<String>,
}

/// The interface constructor's own nativeness axis: `[native code]` + a present `.name`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CtorAxis {
    /// The constructor is `[native code]`.
    pub native: bool,
    /// `ctor.name` is present (an empty name is the raw-`v8::Function` tell).
    pub name_ok: bool,
    /// `new X()` behaves per WebIDL: an argless-constructable interface constructs, and a
    /// non-constructable one throws (an extra constructability is a tell). `true` when n/a.
    pub newable_ok: bool,
    /// Native (not a lie) AND name present AND newable matches WebIDL. `true` when no ctor was walked.
    pub ok: bool,
}

/// The interface structure axis: the live prototype's extra/missing members plus the constructor's
/// name / prototype chain / `Symbol.toStringTag`, diffed against the baseline.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Structure {
    /// Live prototype members the baseline does NOT have (a leak/tell).
    pub extra_members: Vec<String>,
    /// Baseline prototype members absent from the live prototype.
    pub missing_members: Vec<String>,
    pub ctor_name: StrPair,
    pub proto_chain: ChainPair,
    pub to_string_tag: StrPair,
    /// The immediate WebIDL parent (`inherits`) vs the live prototype chain's first entry — the
    /// inheritance edge (Element → Node) must match the spec.
    pub inherits: StrPair,
    /// The ENUMERATION ORDER of members present in both Korvus and the baseline is identical (order is a
    /// fingerprint signal). `true` when there is no baseline order to compare.
    pub order_ok: bool,
    /// No extra/missing members AND ctor name / chain / tag / inheritance / order all match (where a
    /// reference exists — a `None`/empty reference is nothing to fault against).
    pub matches: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChainPair {
    pub ours: Vec<String>,
    #[serde(rename = "ref")]
    pub reference: Vec<String>,
}

/// The report-level globals scan (not per-interface): automation tells + korvus-only extra globals +
/// coverage gaps. Subsumes `automation_tells` and `surface_parity`'s global diff.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GlobalsReport {
    /// `globalThis` names matching a known automation-tell pattern (cdc_/$cdc_/webdriver/selenium/...).
    pub automation_tells: Vec<String>,
    /// `document` own-keys matching an automation-tell pattern (ChromeDriver injects cdc_ here too).
    pub document_tells: Vec<String>,
    /// Globals present in Korvus but not the baseline (korvus-only — a fingerprint tell).
    pub extra_globals: Vec<String>,
    /// Globals in the baseline but not Korvus (a surface coverage gap; informational).
    pub missing_globals: Vec<String>,
    /// `Function.prototype.toString` is itself native (a stealth override target).
    pub fp_tostring_native: bool,
    /// No automation tells (window or document) AND no extra globals AND FP.toString is native.
    pub ok: bool,
}

/// The full-surface tell scan (every dumped interface, independent of the @webref corpus).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TellsReport {
    /// `Iface.member` whose native fn owns prototype/arguments/caller (a JS-shim tell).
    pub own_tells: Vec<String>,
    /// `Iface.member` whose native fn reports an empty name (a raw `v8::Function` that skipped set_name).
    pub name_defects: Vec<String>,
    /// No own-tells and no name defects anywhere on the dumped surface.
    pub ok: bool,
}

/// One WebIDL member graded on the five axes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemberQuality {
    pub name: String,
    /// ① in the merged WebIDL AND on the live prototype.
    pub present: bool,
    /// ② `[native code]` + no shim tell + native fn length correct.
    pub native: NativeAxis,
    /// ③ descriptor/length/kind == the Brave baseline.
    pub shape: ShapeAxis,
    /// ④ live value == captured profile (mapped members only).
    pub value: ValueAxis,
    /// ⑤ `pass | fail | untested` (this member's WPT bucket, else the interface's).
    pub wpt: String,
    /// THE FILES THAT PRODUCED ⑤, so the verdict can be checked rather than believed.
    ///
    /// A `wpt` string with no evidence behind it cannot be cross-examined: `tools/status/data/status.json`
    /// said `CharacterData::insertData` was `fail` while `scripts/wpt-tiers.baseline` recorded
    /// `dom/nodes/CharacterData-insertData.html` as a floor `pass`, and nothing could notice,
    /// because one speaks in members and the other in files. This is the join.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wpt_from: Vec<String>,
    /// HOW those files were attributed to this member (`basename` / `mixin` / `section` / `dir` /
    /// `raw`). `dir` means the file merely sits near the interface; it is the weakest rank and the
    /// one the audit measured charging 624 files to a single member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wpt_rules: Vec<String>,
    /// ⑥ behavior — the member's oracle `expr` cases actually produce the correct output.
    pub behavior: BehaviorAxis,
    /// ⑦ spec — the live shape matches the WebIDL declaration (kind / readonly / const value).
    pub spec: SpecAxis,
    /// Positive DYNAMIC verification exists for this member (a passing behavior case, a passing WPT, a
    /// value matching the profile, or a const matching its WebIDL value) — not merely surface-present.
    /// `false` = surface-only: the member exists and looks right, but its behavior/value is UNVERIFIED.
    pub verified: bool,
    /// (D) The verification is EXTERNAL — a dedicated upstream WPT file passes for this member. The
    /// complement (`verified && !independent`) is self-proof: our own behavior case, which was written
    /// by whoever wrote the implementation and so cannot catch a shared misreading of the spec.
    pub independent: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NativeAxis {
    /// `[native code]` (for accessors: both present halves native).
    pub native: bool,
    /// The member fn owns `prototype`/`arguments`/`caller` — a JS-shim tell.
    pub own_tell: bool,
    /// The native fn's length/name is wrong (getter len 0, setter len 1, method arity vs baseline;
    /// an empty fn.name is the raw-`v8::Function` tell) — collapses `surface_parity`'s NAME+LEN defects.
    pub length_defect: bool,
    /// The axis verdict: native, no tell, no length defect (n/a for non-callable members ⇒ `true`).
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ShapeAxis {
    pub length: LengthPair,
    pub descriptor: StrPair,
    pub kind: StrPair,
    /// kind + descriptor + length + enumerable + configurable all equal the baseline (`true` when the
    /// baseline has no record for the member — nothing to fault against).
    pub matches: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LengthPair {
    pub ours: Option<i64>,
    #[serde(rename = "ref")]
    pub reference: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StrPair {
    pub ours: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValueAxis {
    pub ours: Option<Value>,
    pub profile: Option<Value>,
    pub from_profile: bool,
}

/// ⑥ behavior — does the member actually WORK? The harness evals the oracle's `expr` cases in Korvus and
/// diffs against the captured/known-correct output; this is the per-member pass/total tally.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BehaviorAxis {
    pub pass: usize,
    pub total: usize,
    /// A behavioral case existed for this member.
    pub tested: bool,
    /// All cases passed (n/a when untested ⇒ `true`).
    pub ok: bool,
    /// At least one PASSING case was judged against an answer recorded from a real browser
    /// (the capture rig) rather than one the implementer wrote. This is what lets the
    /// behavior axis count as INDEPENDENT proof for the 721 of 1,127 interfaces upstream WPT has no
    /// file for -- which can otherwise never rise above `self-verified`, however correct they are.
    pub external: bool,
    pub detail: Option<String>,
}

/// ⑦ spec — the member's live shape matches its WebIDL DECLARATION (independent of the Brave baseline):
/// the right kind (attribute→accessor, operation→method, const→value), `readonly` attributes have no
/// setter, and a `const`'s live value equals its WebIDL-declared value. All universal (no per-case).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpecAxis {
    /// Live kind matches the WebIDL member type.
    pub kind_ok: bool,
    /// A WebIDL `readonly` attribute exposes no setter.
    pub readonly_ok: bool,
    /// A WebIDL `const`'s live value equals its declared value.
    pub const_ok: bool,
    /// All applicable spec checks pass (n/a checks ⇒ `true`).
    pub ok: bool,
    pub detail: Option<String>,
}

/// Per-axis `passed/total` ratio strings (the interface's roll-up of its member verdicts).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rollup {
    pub present: String,
    pub native: String,
    pub shape: String,
    pub value: String,
    pub wpt: String,
    pub behavior: String,
    pub spec: String,
    /// Members with positive dynamic verification / present members — how much is actually verified vs
    /// merely surface-present.
    pub verified: String,
    /// (D) Of those, how many rest on evidence the implementer did NOT author -- an upstream WPT pass,
    /// or a behavior case whose expectation was recorded from a real browser
    /// (the capture rig). `independent == 0` with `verified > 0` means the interface is
    /// entirely self-graded: the same agent wrote the code and the answer that judges it.
    pub independent: String,
    /// Verified CONSTANTS / declared constants. A const verifies off its WebIDL value alone, so this
    /// is the part of `verified` that required no behavior and no implementation.
    pub consts: String,
    /// Verified NON-const members / non-const members — the part that actually had to work.
    pub behavioral: String,
}

/// Grade one interface: for every unique named WebIDL member, resolve it in the snapshot and diff it
/// against the baseline (shape/native) and the profile (value); apply the interface's WPT bucket.
pub fn assess(
    iface: &str,
    webidl: &InterfaceSpec,
    snapshot: &SurfaceSnapshot,
    baseline: &Value,
    profile: &Profile,
    wpt: Option<&Bucket>,
    wpt_members: Option<&std::collections::BTreeMap<String, Bucket>>,
    behavior: Option<&std::collections::BTreeMap<String, crate::oracle::BehaviorObs>>,
) -> ModuleQuality {
    let base_members = baseline_members(baseline, iface);
    // THE CAPTURE IS THE DENOMINATOR, NOT THE WEBREF CORPUS.
    //
    // `named_members` walks the webref WebIDL, which describes the standard — including a long tail of
    // members Chrome has never shipped. Grading against that tail is not merely noise, it points the
    // work the wrong way: measured 2026-08-29, of the 547 members this graded "not present", only 28
    // exist in Brave 151. Implementing the other 519 would ADD a prototype member the reference browser
    // does not have, i.e. make the fingerprint worse, and the campaign daemon dutifully emitted them as
    // `impl N missing WebIDL member(s)` worklist lines (`Element: impl 44 missing ...
    // getSpatialNavigationContainer, focusableAreas`) and spent rounds on them.
    //
    // So a member the capture does not record is not work. It is skipped entirely — not counted absent,
    // which would leave `present` with an unreachable ceiling (~92.9%) that reads as remaining work.
    //
    // Scoped carefully, because two kinds of member legitimately have no capture record:
    //   * interfaces the capture never walked at all -> `capture_known` is false, grade everything;
    //   * named-property specials -> exotic, nothing on the prototype to record (`DOMStringMap.members`
    //     is `[]`), graded through the behavior axis alone.
    let base_statics = baseline_statics(baseline, iface);
    let capture_known = !base_members.is_empty() || !base_statics.is_empty();
    let wpt_status = wpt_interface_status(wpt);
    // (B) The interface-wide FAIL signal, taken straight from the bucket rather than smeared across
    // members. A failing file anywhere in the interface is a real defect signal and still drives the
    // verdict + worklist; it just no longer masquerades as a per-member result.
    let wpt_iface_fail = wpt_status == "fail";
    let mut behavior_fail_any = false;
    let (mut behavior_total, mut behavior_ok) = (0usize, 0usize);
    let mut behavior_bad: Vec<String> = Vec::new();
    let spec_index = spec_by_name(webidl);
    let mut spec_fail_any = false;
    let (mut spec_total, mut spec_ok_n) = (0usize, 0usize);
    let mut spec_bad: Vec<String> = Vec::new();
    // Dynamic-verification tally: present members that carry positive behavior/WPT/value/const evidence.
    let (mut verified_total, mut verified_ok) = (0usize, 0usize);
    // (D) Of the verified members, how many rest on an external WPT pass rather than our own cases.
    let mut independent_ok = 0usize;
    // Constants are a MEMBER KIND apart: a `const` is fully implemented exactly when its value is
    // right, so it verifies statically off the WebIDL declaration with no behavior at all. They are a
    // quarter of the whole surface (1,714 of them are WebGL enum values), so folding them into one
    // `VERIFIED x/y` makes a shelled interface look most of the way done — `WebGL2RenderingContext`
    // would read 559/790 while not one of its 231 methods does anything. Counted separately so the
    // report can say `const 559/559 · behavioral 0/231`, which is the true state.
    let (mut const_total, mut const_ok_n) = (0usize, 0usize);

    let mut members: Vec<MemberQuality> = Vec::new();
    // rollup accumulators
    let (mut present_total, mut present_ok) = (0usize, 0usize);
    let (mut native_total, mut native_ok) = (0usize, 0usize);
    let (mut shape_total, mut shape_ok) = (0usize, 0usize);
    let (mut value_total, mut value_ok) = (0usize, 0usize);
    // worklist inputs
    let mut missing: Vec<String> = Vec::new();
    let mut native_bad: Vec<String> = Vec::new();
    let mut value_bad: Vec<String> = Vec::new();
    let mut shape_bad: Vec<String> = Vec::new();

    // THE OTHER HALF OF THE SAME RULE, and a blind spot the first version created. Iterating the WebIDL
    // corpus alone loses a member the CAPTURE has and webref does not declare -- `ReadableStream`'s six
    // (cancel/locked/pipeThrough/pipeTo/tee/values) and `Range.expand` were invisible for exactly that
    // reason, and a direct capture-vs-live diff found them while this reported the gap as closed. If the
    // capture is the denominator then it is the denominator in both directions.
    //
    // A capture-only member has no WebIDL declaration, so the ⑦ spec axis has nothing to check it
    // against and is skipped for it; every other axis reads the same as for any other member.
    let mut graded: Vec<(String, bool, bool, bool)> = named_members(webidl)
        .into_iter()
        .map(|(n, s, sp)| (n, s, sp, true))
        .collect();
    if capture_known {
        for name in base_members.keys() {
            if !graded.iter().any(|(n, _, _, _)| n == name) {
                graded.push((name.clone(), false, false, false));
            }
        }
        for name in base_statics.iter() {
            if !graded.iter().any(|(n, _, _, _)| n == name) {
                graded.push((name.clone(), true, false, false));
            }
        }
    }
    for (name, is_static, declared_special, in_webidl) in graded {
        if capture_known
            && !declared_special
            && !base_members.contains_key(name.as_str())
            && !base_statics.contains(name.as_str())
        {
            continue; // webref declares it; Chrome 151 does not ship it. Not our work — see above.
        }
        let rec = if is_static {
            snapshot.static_member(iface, &name)
        } else {
            snapshot.member(iface, &name)
        };
        let base: Option<&Value> = base_members.get(name.as_str()).copied();

        // ⑤ per-member WPT, in two strengths — they are NOT interchangeable:
        //   `own`      this member has its OWN dedicated WPT file (`Interface-member*.html`). The only
        //              form that is per-member PROOF, so the only form that may verify (below).
        //   `member_wpt`  `own` when it exists, else the interface-wide bucket. Fine for the report
        //              column and for the FAIL signal (a failing file anywhere in the interface is a
        //              real defect signal), but it must never verify: the interface-wide bucket would
        //              spread one file's pass across every member of the interface — e.g. Document's
        //              corpus only exercises getElementById/getElementsByTagName, yet the fallback
        //              would hand all 59 present members a `verified`, including `cookie` and `title`
        //              that no WPT file touches. That is the ⑥-axis self-grading problem restated as a
        //              ⑤-axis free ride, so the credit is deliberately narrower than the display.
        let member_wpt_own = member_wpt_status(wpt_members, &name);
        // (B) The column shows only what WPT actually says ABOUT THIS MEMBER. It used to fall back to
        // the interface-wide bucket, which printed `fail` next to members no WPT file touches — Node
        // reported all 27 members failing when its corpus runs 86/92 and only one member has a file.
        // That is a false worklist item: it sends the agent hunting a bug that does not exist. The
        // interface-wide FAIL signal is still raised, just from the bucket directly (below).
        let member_wpt = member_wpt_own.unwrap_or("untested");
        // Computed HERE, before `name` is moved into the row below.
        let member_wpt_files = member_wpt_paths(wpt_members, &name);
        let member_wpt_rule_names = member_wpt_rules(wpt_members, &name);

        // ⑥ behavior: the harness-observed pass/total for this member's oracle cases (Step 2).
        let beh = behavior_axis(behavior.and_then(|b| b.get(&name)));

        // A named-property special (anonymous getter/setter/deleter) is exotic-object behavior, not a
        // prototype member — the surface axes (①②③④) cannot see it. It is graded ONLY dynamically: a
        // behavior case keyed by its synthetic name is the presence proof. Untested ⇒ contributes
        // nothing at all (no false "missing member" for the many special-bearing interfaces without
        // cases yet, and no free verification either).
        let is_special = matches!(spec_index.get(name.as_str()), Some(MemberSpec::Special { .. }));
        if is_special && !beh.tested {
            continue;
        }

        present_total += 1;

        if beh.tested {
            behavior_total += 1;
            if beh.ok {
                behavior_ok += 1;
            } else {
                behavior_fail_any = true;
                if let Some(d) = &beh.detail {
                    behavior_bad.push(format!("{iface}.{d}"));
                }
            }
        }

        if is_special {
            // Tested (guarded above): the case ran against the live exotic, so the machinery is
            // present; the case outcome is the verified bit, and a failure already drove
            // behavior-fail. Every surface axis is n/a-true by construction.
            present_ok += 1;
            verified_total += 1;
            if beh.ok {
                verified_ok += 1;
            }
            members.push(MemberQuality {
                name,
                present: true,
                native: NativeAxis { native: true, own_tell: false, length_defect: false, ok: true },
                shape: ShapeAxis {
                    length: LengthPair { ours: None, reference: None },
                    descriptor: StrPair { ours: None, reference: None },
                    kind: StrPair { ours: Some("special".to_string()), reference: None },
                    matches: true,
                },
                value: ValueAxis { ours: None, profile: None, from_profile: false },
                wpt: member_wpt.to_string(),
                wpt_from: member_wpt_files.clone(),
                wpt_rules: member_wpt_rule_names.clone(),
                behavior: beh.clone(),
                spec: SpecAxis { kind_ok: true, readonly_ok: true, const_ok: true, ok: true, detail: None },
                verified: beh.ok,
                // A named special has no prototype member and so no `Interface-member` WPT file;
                // its only possible proof is a behavior case. Always self-proof, never independent.
                independent: false,
            });
            continue;
        }

        // ⑦ spec: the live shape matches the WebIDL declaration (kind / readonly / const value). Graded
        // only for present members (an absent member is already axis ①).
        let spec = spec_axis(spec_index.get(name.as_str()).copied(), rec.as_ref());
        // `in_webidl` false = the capture has this member and webref does not declare it, so there is
        // no declaration for ⑦ to compare against. It is graded on every other axis and left out of
        // this tally rather than given a free pass that inflates the denominator.
        if rec.is_some() && in_webidl {
            spec_total += 1;
            if spec.ok {
                spec_ok_n += 1;
            } else {
                spec_fail_any = true;
                if let Some(d) = &spec.detail {
                    spec_bad.push(format!("{iface}.{name}: {d}"));
                }
            }
        }

        let Some(rec) = rec else {
            // Absent from Korvus's live surface — axis ① fails; the other axes are n/a but we still
            // surface the profile expectation so a value-bearing gap is visible.
            missing.push(name.clone());
            members.push(MemberQuality {
                present: false,
                native: NativeAxis { native: false, own_tell: false, length_defect: false, ok: false },
                shape: ShapeAxis {
                    length: LengthPair { ours: None, reference: base.and_then(|b| b["length"].as_i64()) },
                    descriptor: StrPair { ours: None, reference: base.map(descriptor_sig_value) },
                    kind: StrPair { ours: None, reference: base.and_then(|b| b["kind"].as_str()).map(str::to_string) },
                    matches: false,
                },
                value: ValueAxis {
                    ours: None,
                    profile: profile.expected(iface, &name),
                    from_profile: false,
                },
                wpt: member_wpt.to_string(),
                wpt_from: member_wpt_files.clone(),
                wpt_rules: member_wpt_rule_names.clone(),
                behavior: beh.clone(),
                spec: spec.clone(),
                verified: false,
                independent: false,
                name,
            });
            continue;
        };
        present_ok += 1;

        // ② native — only callable members carry a native fn; data properties are n/a (ok).
        let native = native_axis(&rec, base);
        if rec.is_callable() {
            native_total += 1;
            if native.ok {
                native_ok += 1;
            } else {
                native_bad.push(format!("{iface}.{name} ({})", native_reason(&native)));
            }
        }

        // ③ shape — graded only where the baseline has a reference record.
        let shape = shape_axis(&rec, base);
        if base.is_some() {
            shape_total += 1;
            if shape.matches {
                shape_ok += 1;
            } else {
                shape_bad.push(format!("{iface}.{name} ({})", shape_reason(&shape)));
            }
        }

        // ④ value — graded only for mapped scalars we could both expect AND measure.
        let expected = profile.expected(iface, &name);
        let ours = rec.live_value.clone();
        let from_profile = matches!((&ours, &expected), (Some(o), Some(e)) if value_eq(o, e));
        if expected.is_some() && ours.is_some() {
            value_total += 1;
            if from_profile {
                value_ok += 1;
            } else {
                value_bad.push(format!(
                    "{iface}.{name} ours={} want={}",
                    compact(ours.as_ref()),
                    compact(expected.as_ref())
                ));
            }
        }

        // A member is dynamically VERIFIED (not merely surface-present) when some positive check confirms
        // its value/behavior: a passing behavior case, a passing WPT, a value matching the profile, or a
        // const matching its WebIDL value.
        let is_const = matches!(spec_index.get(name.as_str()), Some(MemberSpec::Const { .. }));

        // (D) WPT OUTRANKS the self-authored behavior axis, in both directions.
        //
        // Down: a member whose own WPT file FAILS is not verified, whatever our cases say. This used
        // to be the purest form of the self-grading problem — `URLSearchParams.delete` reported
        // `wpt fail` and `verified yes` in the same row, because our own case passed. The upstream
        // corpus is the independent authority; our case agreeing with our code proves nothing against it.
        //
        // Up: a passing dedicated file is INDEPENDENT proof, tracked separately from self-proof so the
        // rollup can say how much of `verified` is actually externally checked. Today that number is
        // the honest headline: an interface can be "verified" entirely on evidence it wrote itself.
        //
        // A RECORDED behavior case is independent for the same reason a WPT pass is: the implementer did
        // not author the answer. It is weaker in one way (we chose which expression to run) and stronger
        // in another (the answer is the target browser's own, quirks included, where WPT is the spec's).
        // Both beat a case whose expectation and implementation came from the same reading of a spec.
        let independent = member_wpt_own == Some("pass") || (beh.tested && beh.ok && beh.external);
        let self_proof = (beh.tested && beh.ok) || from_profile || (is_const && spec.const_ok);
        let verified = member_wpt_own != Some("fail") && (independent || self_proof);
        verified_total += 1;
        if is_const {
            const_total += 1;
        }
        if verified {
            verified_ok += 1;
            if independent {
                independent_ok += 1;
            }
            if is_const {
                const_ok_n += 1;
            }
        }

        members.push(MemberQuality {
            name,
            present: true,
            native,
            shape,
            value: ValueAxis { ours, profile: expected, from_profile },
            wpt: member_wpt.to_string(),
                wpt_from: member_wpt_files.clone(),
                wpt_rules: member_wpt_rule_names.clone(),
            behavior: beh,
            spec,
            verified,
            independent,
        });
    }

    // Interface-level axes (Phase A): the constructor's nativeness and the structural diff.
    // WebIDL constructor shape for the newable check: does the interface declare a constructor, and can
    // one be called with no args (all-optional/variadic)?
    // `[HTMLConstructor]` is NOT a way to construct anything: every HTML element interface declares
    // it, and outside a custom-element upgrade it throws "Illegal constructor" -- which is what the
    // recorded browser answers say for all of them. Counting it here judged every HTML element as
    // argless-constructable and put a spurious `fix constructor` line on each one's worklist, which
    // an implementer could only "fix" by breaking ground truth. `spec.rs` already parses the flag and
    // `gencases.rs` already honours it; this was the one reader that did not.
    let webidl_ctors: Vec<&Vec<crate::spec::Arg>> = webidl
        .members
        .iter()
        .filter_map(|m| match m {
            MemberSpec::Constructor { args, html_constructor: false } => Some(args),
            _ => None,
        })
        .collect();
    let has_ctor = !webidl_ctors.is_empty();
    let argless_ctor = webidl_ctors.iter().any(|args| args.iter().all(|a| a.optional || a.variadic));
    let ctor = ctor_axis(snapshot.ctor(iface), baseline_interface(baseline, iface), has_ctor, argless_ctor);
    let structure = structure_axis(iface, snapshot, baseline, webidl);

    // Verdict precedence.
    let present_gap = present_ok < present_total;
    let native_fail = !native_bad.is_empty() || !ctor.ok;
    let value_fail = !value_bad.is_empty();
    let shape_fail = !shape_bad.is_empty();
    // (B) Straight from the interface bucket: any failing file in the interface is a real defect
    // signal, independent of whether it could be attributed to a member.
    let wpt_fail = wpt_iface_fail;
    let structure_fail = !structure.matches;
    let verdict = if present_gap {
        "incomplete"
    } else if native_fail {
        "not-native"
    } else if behavior_fail_any {
        "behavior-fail"
    } else if value_fail {
        "value-drift"
    } else if shape_fail || wpt_fail || structure_fail || spec_fail_any || !ctor.newable_ok {
        "low-quality"
    } else if verified_total > 0 && verified_ok == verified_total {
        // Surface-complete AND every present member has positive dynamic verification. (D) splits this
        // by WHERE the proof came from: `verified` is reserved for surfaces with at least one member
        // proven by the external corpus; a surface proven only by cases we wrote ourselves is
        // `self-verified` — a real milestone, but not the same claim, and the campaign metric must not
        // conflate them. Interfaces upstream has no tests for can only ever reach `self-verified`.
        if independent_ok > 0 {
            "verified"
        } else {
            "self-verified"
        }
    } else {
        // Surface-complete + no failures, but some members are surface-only (behavior/value UNVERIFIED).
        "complete"
    };

    let mut worklist = build_worklist(
        iface, &missing, &native_bad, &value_bad, &shape_bad, wpt_fail, wpt,
    );
    for entry in behavior_bad.iter().take(8) {
        worklist.push(format!("fix behavior: {entry}"));
    }
    for entry in spec_bad.iter().take(8) {
        worklist.push(format!("fix spec: {entry}"));
    }
    if !ctor.ok {
        worklist.insert(0, format!("nativize constructor: {iface} ({})", ctor_reason(&ctor)));
    }
    if !ctor.newable_ok {
        worklist.push(format!("fix constructor: {iface} (newable != WebIDL — `new {iface}()` behaves wrong)"));
    }
    if structure_fail {
        worklist.push(format!("fix structure: {iface} ({})", structure_reason(&structure)));
    }

    ModuleQuality {
        iface: iface.to_string(),
        members,
        ctor,
        structure,
        rollup: Rollup {
            present: format!("{present_ok}/{present_total}"),
            native: format!("{native_ok}/{native_total}"),
            shape: format!("{shape_ok}/{shape_total}"),
            value: format!("{value_ok}/{value_total}"),
            wpt: wpt.map(|b| format!("{}/{}", b.passed, b.total)).unwrap_or_else(|| "-".to_string()),
            behavior: format!("{behavior_ok}/{behavior_total}"),
            spec: format!("{spec_ok_n}/{spec_total}"),
            verified: format!("{verified_ok}/{verified_total}"),
            independent: format!("{independent_ok}/{verified_total}"),
            consts: format!("{const_ok_n}/{const_total}"),
            behavioral: format!(
                "{}/{}",
                verified_ok - const_ok_n,
                verified_total - const_total
            ),
        },
        verdict: verdict.to_string(),
        wpt_failures: Vec::new(), // filled by the CLI, which holds the raw dump
        blink_refs: Vec::new(),   // filled by the CLI when KORVUS_BLINK_ROOT is set
        worklist,
    }
}

/// The unique named WebIDL members to grade, in source order — `(name, is_static)`. Constructors have
/// no name (they are graded via the interface's `ctorNative`, not here) and are skipped; anonymous
/// specials grade under their synthetic names ("named getter", …); duplicate names (operation
/// overloads, a base member redeclared) collapse to their first occurrence.
fn named_members(webidl: &InterfaceSpec) -> Vec<(String, bool, bool)> {
    let mut out: Vec<(String, bool, bool)> = Vec::new();
    for m in &webidl.members {
        let Some(name) = graded_name(m) else {
            continue;
        };
        let is_static = matches!(
            m,
            MemberSpec::Attribute { is_static: true, .. } | MemberSpec::Operation { is_static: true, .. }
        );
        // Named-property specials are exotic-object behavior, not prototype members: the capture
        // records `DOMStringMap.members` as `[]` because there is nothing on its prototype to record.
        // They are graded through the behavior axis alone, so the capture filter in `assess` must not
        // see them — hence the third field.
        let is_special = matches!(m, MemberSpec::Special { .. });
        if !out.iter().any(|(n, _, _)| *n == name) {
            out.push((name, is_static, is_special));
        }
    }
    out
}

/// The graded name of one member: its WebIDL identifier, or the synthetic special name for an
/// anonymous getter/setter/deleter. `None` for constructors (graded via the ctor axis).
fn graded_name(m: &MemberSpec) -> Option<String> {
    match m {
        MemberSpec::Attribute { name, .. }
        | MemberSpec::Operation { name, .. }
        | MemberSpec::Const { name, .. } => Some(name.clone()),
        MemberSpec::Special { special } => Some(special.member_name().to_string()),
        MemberSpec::Constructor { .. } => None,
    }
}

/// `baseline.interfaces[iface].statics` as a name set. Separate from `members` in the capture (statics
/// live on the interface object, prototype members on the prototype), so the capture filter has to look
/// in both — a member WebIDL calls static and Chrome exposes as an instance member, or the reverse, must
/// not be mistaken for a member Chrome does not ship at all.
fn baseline_statics(baseline: &Value, iface: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    if let Some(statics) = baseline
        .get("interfaces")
        .and_then(|i| i.get(iface))
        .and_then(|r| r.get("statics"))
        .and_then(Value::as_array)
    {
        for m in statics {
            let name = m.get("name").and_then(Value::as_str).or_else(|| m.as_str());
            if let Some(name) = name {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// `baseline.interfaces[iface].members` keyed by name.
fn baseline_members<'a>(baseline: &'a Value, iface: &str) -> std::collections::BTreeMap<String, &'a Value> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(members) = baseline
        .get("interfaces")
        .and_then(|i| i.get(iface))
        .and_then(|r| r.get("members"))
        .and_then(Value::as_array)
    {
        for m in members {
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                out.entry(name.to_string()).or_insert(m);
            }
        }
    }
    out
}

/// The raw `baseline.interfaces[iface]` record (its interface-level ctor fields: ctorNative / ctorName
/// / proto / toStringTag / members), for the ctor + structure axes.
fn baseline_interface<'a>(baseline: &'a Value, iface: &str) -> Option<&'a Value> {
    baseline.get("interfaces").and_then(|i| i.get(iface))
}

/// The ctor axis: the constructor is `[native code]` (not a lie vs the baseline) and its `.name` is
/// present. `None` ours (no constructor walked) ⇒ n/a ⇒ ok.
fn ctor_axis(ours: Option<&CtorRec>, base: Option<&Value>, has_ctor: bool, argless_ctor: bool) -> CtorAxis {
    let Some(ours) = ours else {
        return CtorAxis { native: false, name_ok: true, newable_ok: true, ok: true };
    };
    let base_native = base.and_then(|b| b.get("ctorNative")).and_then(Value::as_bool).unwrap_or(false);
    let native = ours.native;
    // Only an EMPTY name is a tell (a raw v8::Function that skipped set_name); an absent name (unit
    // fixtures) is not faulted.
    let name_ok = ours.name.as_deref() != Some("");
    let lie = base_native && !native;
    // Newability vs WebIDL: an argless-constructable interface must construct; a non-constructable one
    // must throw (constructing it anyway is a tell). Arg-requiring constructors + unprobed snapshots pass.
    let newable_ok = match (ours.newable, has_ctor, argless_ctor) {
        (Some(false), true, true) => false, // WebIDL: argless-constructable, but `new X()` threw
        (Some(true), false, _) => false,    // WebIDL: no constructor, but `new X()` worked
        _ => true,
    };
    // `ok` covers only the NATIVE-tell concerns (a lie about native, an empty name); a newable mismatch
    // is a constructability GAP, not a tell, so it is graded as low-quality (via `newable_ok`), not
    // not-native.
    CtorAxis { native, name_ok, newable_ok, ok: !lie && name_ok }
}

/// The structure axis: extra/missing live prototype members + ctor name / proto chain / toStringTag,
/// diffed against the baseline (name/chain/tag faulted only where the baseline carries a reference).
fn structure_axis(iface: &str, snap: &SurfaceSnapshot, baseline: &Value, webidl: &InterfaceSpec) -> Structure {
    let ours_members: std::collections::BTreeSet<String> =
        snap.member_names(iface).into_iter().map(str::to_string).collect();
    let base = baseline_interface(baseline, iface);
    let base_members: std::collections::BTreeSet<String> = base
        .and_then(|b| b.get("members"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let extra_members: Vec<String> = ours_members.difference(&base_members).cloned().collect();
    let missing_members: Vec<String> = base_members.difference(&ours_members).cloned().collect();

    let ours_ctor = snap.ctor(iface);
    let ctor_name = StrPair {
        ours: ours_ctor.and_then(|c| c.name.clone()),
        reference: base.and_then(|b| b.get("ctorName")).and_then(Value::as_str).map(str::to_string),
    };
    let proto_chain = ChainPair {
        ours: ours_ctor.map(|c| c.proto_chain.clone()).unwrap_or_default(),
        reference: base
            .and_then(|b| b.get("proto"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    };
    let to_string_tag = StrPair {
        ours: ours_ctor.and_then(|c| c.to_string_tag.clone()),
        reference: base.and_then(|b| b.get("toStringTag")).and_then(Value::as_str).map(str::to_string),
    };

    // The immediate WebIDL parent vs the live prototype chain's first entry (Element inherits Node).
    let inherits = StrPair {
        ours: proto_chain.ours.first().cloned(),
        reference: webidl.inherits.clone(),
    };

    // Enumeration order: the members present in BOTH must appear in the same relative order (the order
    // itself is a fingerprint signal). Compare the two order-filtered-to-shared sequences.
    let base_names: Vec<&str> = base
        .and_then(|b| b.get("members"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| m.get("name").and_then(Value::as_str)).collect())
        .unwrap_or_default();
    let base_set: std::collections::BTreeSet<&str> = base_names.iter().copied().collect();
    let ours_seq: Vec<&str> =
        snap.ordered_member_names(iface).iter().map(String::as_str).filter(|m| base_set.contains(*m)).collect();
    let base_seq: Vec<&str> = base_names.iter().copied().filter(|m| ours_members.contains(*m)).collect();
    let order_ok = base_names.is_empty() || ours_seq == base_seq;

    let name_mismatch = ctor_name.reference.is_some() && ctor_name.ours != ctor_name.reference;
    let chain_mismatch = !proto_chain.reference.is_empty() && proto_chain.ours != proto_chain.reference;
    let tag_mismatch = to_string_tag.reference.is_some() && to_string_tag.ours != to_string_tag.reference;
    let inherits_mismatch = inherits.reference.is_some() && inherits.ours != inherits.reference;
    let matches = extra_members.is_empty()
        && missing_members.is_empty()
        && !name_mismatch
        && !chain_mismatch
        && !tag_mismatch
        && !inherits_mismatch
        && order_ok;
    Structure {
        extra_members,
        missing_members,
        ctor_name,
        proto_chain,
        to_string_tag,
        inherits,
        order_ok,
        matches,
    }
}

fn ctor_reason(c: &CtorAxis) -> String {
    let mut r = Vec::new();
    if !c.native {
        r.push("not [native code]".to_string());
    }
    if !c.name_ok {
        r.push("empty ctor.name".to_string());
    }
    r.join("; ")
}

fn structure_reason(s: &Structure) -> String {
    let mut parts = Vec::new();
    if !s.extra_members.is_empty() {
        parts.push(format!("extra members: {}", head(&s.extra_members, 6)));
    }
    if !s.missing_members.is_empty() {
        parts.push(format!("missing members: {}", head(&s.missing_members, 6)));
    }
    if s.ctor_name.reference.is_some() && s.ctor_name.ours != s.ctor_name.reference {
        parts.push(format!("ctor.name {}→{}", opt(&s.ctor_name.ours), opt(&s.ctor_name.reference)));
    }
    if !s.proto_chain.reference.is_empty() && s.proto_chain.ours != s.proto_chain.reference {
        parts.push(format!("proto {:?}→{:?}", s.proto_chain.ours, s.proto_chain.reference));
    }
    if s.to_string_tag.reference.is_some() && s.to_string_tag.ours != s.to_string_tag.reference {
        parts.push(format!("toStringTag {}→{}", opt(&s.to_string_tag.ours), opt(&s.to_string_tag.reference)));
    }
    if s.inherits.reference.is_some() && s.inherits.ours != s.inherits.reference {
        parts.push(format!("inherits {}→{}", opt(&s.inherits.ours), opt(&s.inherits.reference)));
    }
    if !s.order_ok {
        parts.push("enumeration order differs".to_string());
    }
    parts.join("; ")
}

/// The report-level globals scan (not per-interface): automation tells, korvus-only extra globals, and
/// baseline-only missing globals. `ok` when there are no tells and no extra globals.
pub fn scan_globals(snapshot: &SurfaceSnapshot, baseline: &Value) -> GlobalsReport {
    let ours = snapshot.globals();
    let automation_tells: Vec<String> =
        ours.iter().filter(|g| is_automation_tell(g)).cloned().collect();
    let document_tells: Vec<String> =
        snapshot.document_keys().iter().filter(|g| is_automation_tell(g)).cloned().collect();
    let base: std::collections::BTreeSet<&str> = baseline
        .get("globals")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let ours_set: std::collections::BTreeSet<&str> = ours.iter().map(String::as_str).collect();
    let extra_globals: Vec<String> =
        ours.iter().filter(|g| !base.contains(g.as_str())).cloned().collect();
    let missing_globals: Vec<String> =
        base.iter().filter(|g| !ours_set.contains(*g)).map(|g| g.to_string()).collect();
    // Unprobed (old snapshot) ⇒ assume native so we don't false-positive.
    let fp_tostring_native = snapshot.fp_tostring_native().unwrap_or(true);
    let ok = automation_tells.is_empty()
        && document_tells.is_empty()
        && extra_globals.is_empty()
        && fp_tostring_native;
    GlobalsReport { automation_tells, document_tells, extra_globals, missing_globals, fp_tostring_native, ok }
}

/// The full-surface tell scan (every dumped interface, not just the corpus subset): native member fns
/// that own prototype/arguments/caller, or that report an empty name. Subsumes surface_parity's
/// OWN-TELLS + NAME-DEFECT columns across the whole surface.
pub fn scan_tells(snapshot: &SurfaceSnapshot) -> TellsReport {
    let mut own_tells = Vec::new();
    let mut name_defects = Vec::new();
    for iface in snapshot.interface_names() {
        for name in snapshot.member_names(iface) {
            let Some(rec) = snapshot.member(iface, name) else {
                continue;
            };
            if !rec.is_callable() {
                continue;
            }
            if rec.own_tell {
                own_tells.push(format!("{iface}.{name}"));
            }
            if rec.native && native_name_empty(&rec) {
                name_defects.push(format!("{iface}.{name}"));
            }
        }
    }
    let ok = own_tells.is_empty() && name_defects.is_empty();
    TellsReport { own_tells, name_defects, ok }
}

/// A native member fn (method, or an accessor half) whose name is the empty string — the raw
/// `v8::Function` tell (a real native fn is named "get x"/"set x"/the method name).
fn native_name_empty(rec: &MemberRec) -> bool {
    match rec.kind.as_str() {
        "method" => rec.fn_name.as_deref() == Some(""),
        "accessor" => {
            (rec.get && rec.get_name.as_deref() == Some(""))
                || (rec.set && rec.set_name.as_deref() == Some(""))
        }
        _ => false,
    }
}

/// The Exposed-realm cross-check (report-level): each interface must live in the realm its WebIDL
/// `[Exposed=...]` declares. A Window-only interface appearing in the worker realm is a leak/tell; a
/// Worker-exposed interface missing from the worker realm is a coverage gap.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExposedReport {
    /// WebIDL exposes it to Worker (or `*`) and Korvus has it in the window, but NOT in the worker realm.
    pub missing_in_worker: Vec<String>,
    /// WebIDL exposes it to Window only, but Korvus leaks it into the worker realm (a tell).
    pub leaked_to_worker: Vec<String>,
    /// No leaks (the hard invariant; `missing_in_worker` is a coverage gap, not a tell).
    pub ok: bool,
}

/// Cross-check every corpus interface's `[Exposed]` realms against Korvus's window + worker surfaces.
pub fn check_exposed(
    corpus: &std::collections::BTreeMap<String, InterfaceSpec>,
    window: &SurfaceSnapshot,
    worker_ifaces: &std::collections::BTreeSet<String>,
) -> ExposedReport {
    let mut missing_in_worker = Vec::new();
    let mut leaked_to_worker = Vec::new();
    for (name, spec) in corpus {
        if spec.exposed.is_empty() || !window.has_interface(name) {
            continue; // unknown exposure, or not implemented in the window realm — skip
        }
        // Any worker realm counts (Worker / DedicatedWorker / SharedWorker / ServiceWorker), plus `*`.
        let worker_exposed = spec.exposed.iter().any(|g| g == "*" || g.contains("Worker"));
        let in_worker = worker_ifaces.contains(name);
        if worker_exposed && !in_worker {
            missing_in_worker.push(name.clone());
        } else if !worker_exposed && in_worker {
            leaked_to_worker.push(name.clone());
        }
    }
    let ok = leaked_to_worker.is_empty();
    ExposedReport { missing_in_worker, leaked_to_worker, ok }
}

/// Whether a `globalThis` name matches a known headless-automation / driver injection tell. Matched
/// case-insensitively so mixed-case injections (`calledSelenium`, `__$webdriverAsyncExecutor`) are caught.
fn is_automation_tell(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("cdc_")
        || n.starts_with("$cdc_")
        || n.contains("webdriver")
        || n.contains("selenium")
        || n.contains("nightmare")
        || n.contains("phantom")
        || n.contains("domautomation")
        || n.starts_with("__driver")
        || n.starts_with("__fxdriver")
        || n.contains("watir")
        || n.contains("puppeteer")
        || n.contains("playwright")
}

/// The interface-level WPT status — the fallback for members without a dedicated WPT file: all subtests
/// pass ⇒ `pass`, some fail ⇒ `fail`, no bucket / no subtests ⇒ `untested`.
fn wpt_interface_status(wpt: Option<&Bucket>) -> &'static str {
    match wpt {
        Some(b) if b.total == 0 => "untested",
        Some(b) if b.passed >= b.total => "pass",
        Some(_) => "fail",
        None => "untested",
    }
}

/// This member's own WPT status from its per-member bucket, or `None` when it has no dedicated file
/// (the caller then falls back to the interface-wide status).
/// The files behind this member's ⑤ verdict. Empty when the member has no bucket of its own -- which
/// is a different fact from "no files failed", and the caller must not conflate them.
fn member_wpt_paths(members: Option<&std::collections::BTreeMap<String, Bucket>>, name: &str) -> Vec<String> {
    members
        .and_then(|m| m.get(name))
        .map(|b| b.paths.iter().cloned().collect())
        .unwrap_or_default()
}

/// The attribution rules behind this member's evidence -- how strong the join is, not just that
/// there is one.
fn member_wpt_rules(members: Option<&std::collections::BTreeMap<String, Bucket>>, name: &str) -> Vec<String> {
    members
        .and_then(|m| m.get(name))
        .map(|b| b.rules.iter().map(|r| r.to_string()).collect())
        .unwrap_or_default()
}

fn member_wpt_status(members: Option<&std::collections::BTreeMap<String, Bucket>>, name: &str) -> Option<&'static str> {
    let b = members?.get(name)?;
    Some(if b.total == 0 {
        "untested"
    } else if b.passed >= b.total {
        "pass"
    } else {
        "fail"
    })
}

/// The ⑥ behavior axis for one member from the harness-observed pass/total tally. No observation ⇒
/// untested ⇒ n/a (`ok`, `tested = false`).
fn behavior_axis(obs: Option<&crate::oracle::BehaviorObs>) -> BehaviorAxis {
    match obs {
        None => BehaviorAxis { pass: 0, total: 0, tested: false, ok: true, external: false, detail: None },
        Some(o) => BehaviorAxis {
            pass: o.pass,
            total: o.total,
            tested: o.total > 0,
            ok: o.total == 0 || o.pass == o.total,
            external: o.external > 0,
            detail: o.fail_detail.clone(),
        },
    }
}

/// `member name -> &MemberSpec` (first-seen), for the ⑦ spec checks (and the special-member
/// detection in `assess`). Anonymous specials are keyed by their synthetic names.
fn spec_by_name(webidl: &InterfaceSpec) -> std::collections::BTreeMap<&str, &MemberSpec> {
    let mut out = std::collections::BTreeMap::new();
    for m in &webidl.members {
        let name: &str = match m {
            MemberSpec::Attribute { name, .. }
            | MemberSpec::Operation { name, .. }
            | MemberSpec::Const { name, .. } => name,
            MemberSpec::Special { special } => special.member_name(),
            MemberSpec::Constructor { .. } => continue,
        };
        out.entry(name).or_insert(m);
    }
    out
}

/// ⑦ the spec axis: live kind matches the WebIDL member type (attribute→accessor, operation→method,
/// const→value), a `readonly` attribute exposes no setter, and a `const`'s live value equals its
/// declared value. `None` mspec/rec ⇒ n/a (ok).
fn spec_axis(mspec: Option<&MemberSpec>, rec: Option<&MemberRec>) -> SpecAxis {
    let na = SpecAxis { kind_ok: true, readonly_ok: true, const_ok: true, ok: true, detail: None };
    let (Some(mspec), Some(rec)) = (mspec, rec) else {
        return na;
    };
    let mut kind_ok = true;
    let mut readonly_ok = true;
    let mut const_ok = true;
    let mut detail: Option<String> = None;
    match mspec {
        MemberSpec::Attribute { readonly, put_forwards, .. } => {
            kind_ok = rec.kind == "accessor";
            if !kind_ok {
                detail = Some(format!("kind {} want accessor", rec.kind));
            }
            // `[PutForwards=value]` keeps the IDL `readonly` and still installs a setter — the
            // recorded browser reports `set: true` for relList/sizes/blocking. Faulting that setter
            // would be faulting the implementation for agreeing with the browser.
            if *readonly && !*put_forwards && rec.set {
                readonly_ok = false;
                detail.get_or_insert_with(|| "readonly attribute has a setter".to_string());
            }
        }
        MemberSpec::Operation { .. } => {
            kind_ok = rec.kind == "method";
            if !kind_ok {
                detail = Some(format!("kind {} want method", rec.kind));
            }
        }
        MemberSpec::Const { value, ty, .. } => {
            kind_ok = rec.kind == "value";
            if !kind_ok {
                detail = Some(format!("kind {} want value(const)", rec.kind));
            }
            let want = parse_const(value, ty);
            let ours = rec.live_value.clone().or_else(|| rec.value.clone());
            const_ok = ours.as_ref().map(|o| value_eq(o, &want)).unwrap_or(false);
            if !const_ok {
                detail.get_or_insert_with(|| format!("const {ours:?} want {want}"));
            }
        }
        MemberSpec::Constructor { .. } => {}
        // A special has no prototype shape to check — it is graded dynamically (⑥), never here.
        MemberSpec::Special { .. } => {}
    }
    let ok = kind_ok && readonly_ok && const_ok;
    SpecAxis { kind_ok, readonly_ok, const_ok, ok, detail }
}

/// Parse a WebIDL const literal (`"1"`, `"true"`, `"0x1F"`, `"-3.5"`) into a JSON value.
fn parse_const(value: &str, _ty: &str) -> Value {
    let v = value.trim();
    match v {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        if let Ok(n) = i64::from_str_radix(hex, 16) {
            return Value::from(n);
        }
    }
    if let Ok(n) = v.parse::<i64>() {
        return Value::from(n);
    }
    if let Ok(n) = v.parse::<f64>() {
        return Value::from(n);
    }
    Value::from(v)
}

/// ② the native axis for one member (ported from `surface_parity`/`nolies`). A "lie" is Korvus being
/// non-native where the baseline IS native; tells are an owned `prototype`/`arguments`/`caller` or a
/// native fn whose length/name is wrong.
fn native_axis(rec: &MemberRec, base: Option<&Value>) -> NativeAxis {
    if !rec.is_callable() {
        // Data property (const/value) — no native fn to grade.
        return NativeAxis { native: true, own_tell: false, length_defect: false, ok: true };
    }
    let base_native = base.map(|b| b["native"].as_bool().unwrap_or(true)).unwrap_or(true);
    let native = rec.native;
    let own_tell = rec.own_tell;
    let length_defect = native_length_defect(rec, base);
    let lie = base_native && !native;
    NativeAxis { native, own_tell, length_defect, ok: !lie && !own_tell && !length_defect }
}

/// Whether a native member fn's shape is wrong — the port of `surface_parity`'s NAME+LEN defects: a
/// native getter must have length 0 and a name, a setter length 1 and a name, a method its baseline
/// arity and a non-empty name (an empty name is the raw-`v8::Function` tell). Non-native fns are not
/// judged here (that is the `native`/lie signal).
fn native_length_defect(rec: &MemberRec, base: Option<&Value>) -> bool {
    if !rec.native {
        return false;
    }
    match rec.kind.as_str() {
        "method" => {
            let name_empty = rec.fn_name.as_deref() == Some("");
            let len_bad = match (rec.length, base.and_then(|b| b["length"].as_i64())) {
                (Some(kl), Some(bl)) if bl >= 0 => kl != bl,
                _ => false,
            };
            name_empty || len_bad
        }
        "accessor" => {
            let get_bad = rec.get
                && (rec.get_name.as_deref() == Some("") || rec.get_length.unwrap_or(0) != 0);
            let set_bad = rec.set
                && (rec.set_name.as_deref() == Some("") || rec.set_length.unwrap_or(1) != 1);
            get_bad || set_bad
        }
        _ => false,
    }
}

/// ③ the shape axis for one member, diffed against the baseline record (`None` ref ⇒ nothing to
/// fault, `matches = true`).
fn shape_axis(rec: &MemberRec, base: Option<&Value>) -> ShapeAxis {
    let (len_ours, len_ref) = if rec.kind == "method" {
        (rec.length, base.and_then(|b| b["length"].as_i64()))
    } else {
        (None, None)
    };
    let kind_ref = base.and_then(|b| b["kind"].as_str()).map(str::to_string);
    let desc_ref = base.map(|b| descriptor_sig_value(b));
    let matches = match base {
        None => true,
        Some(b) => shape_matches(rec, b),
    };
    ShapeAxis {
        length: LengthPair { ours: len_ours, reference: len_ref },
        descriptor: StrPair { ours: Some(descriptor_sig(rec)), reference: desc_ref },
        kind: StrPair { ours: Some(rec.kind.clone()), reference: kind_ref },
        matches,
    }
}

/// kind + enumerable + configurable + (accessor: get/set) + (method: length) all equal the baseline.
fn shape_matches(rec: &MemberRec, base: &Value) -> bool {
    if rec.kind != base["kind"].as_str().unwrap_or("") {
        return false;
    }
    if rec.enumerable != base["enumerable"].as_bool().unwrap_or(rec.enumerable) {
        return false;
    }
    if rec.configurable != base["configurable"].as_bool().unwrap_or(rec.configurable) {
        return false;
    }
    match rec.kind.as_str() {
        "accessor" => {
            rec.get == base["get"].as_bool().unwrap_or(rec.get)
                && rec.set == base["set"].as_bool().unwrap_or(rec.set)
        }
        "method" => match (rec.length, base["length"].as_i64()) {
            (Some(k), Some(b)) => k == b,
            _ => true,
        },
        _ => match (&rec.value, base.get("value")) {
            (Some(k), Some(b)) => value_eq(k, b),
            _ => true,
        },
    }
}

/// A compact descriptor signature: an accessor's present halves (`get` / `set` / `get set`), else the
/// member kind (`method` / `value`). Matches the doc's `descriptor: {ours:"get", ref:"get"}`.
fn descriptor_sig(rec: &MemberRec) -> String {
    match rec.kind.as_str() {
        "accessor" => accessor_sig(rec.get, rec.set),
        "method" => "method".to_string(),
        other => other.to_string(),
    }
}

fn descriptor_sig_value(v: &Value) -> String {
    match v["kind"].as_str().unwrap_or("") {
        "accessor" => accessor_sig(v["get"].as_bool().unwrap_or(false), v["set"].as_bool().unwrap_or(false)),
        "method" => "method".to_string(),
        other => other.to_string(),
    }
}

fn accessor_sig(get: bool, set: bool) -> String {
    match (get, set) {
        (true, true) => "get set".to_string(),
        (true, false) => "get".to_string(),
        (false, true) => "set".to_string(),
        (false, false) => "accessor".to_string(),
    }
}

/// Value equality with numeric coercion (JSON `8` vs `8.0`, `2` vs `2.0`) so integer/float spelling
/// never spuriously drifts.
fn value_eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

fn native_reason(n: &NativeAxis) -> String {
    let mut r = Vec::new();
    if !n.native {
        r.push("not [native code]");
    }
    if n.own_tell {
        r.push("owns prototype/arguments/caller");
    }
    if n.length_defect {
        r.push("native fn length/name defect");
    }
    if r.is_empty() {
        "ok".to_string()
    } else {
        r.join("; ")
    }
}

fn shape_reason(s: &ShapeAxis) -> String {
    if s.kind.ours != s.kind.reference {
        return format!("kind {}→{}", opt(&s.kind.ours), opt(&s.kind.reference));
    }
    if s.descriptor.ours != s.descriptor.reference {
        return format!("descriptor {}→{}", opt(&s.descriptor.ours), opt(&s.descriptor.reference));
    }
    if s.length.ours != s.length.reference && s.length.reference.is_some() {
        return format!("length {:?}→{:?}", s.length.ours, s.length.reference);
    }
    "enumerable/configurable mismatch".to_string()
}

fn opt(s: &Option<String>) -> String {
    s.clone().unwrap_or_else(|| "∅".to_string())
}

/// Render a JSON value compactly for the worklist (`"MacIntel"`, `10`, `["en-US","en"]`).
fn compact(v: Option<&Value>) -> String {
    match v {
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "?".to_string()),
        None => "∅".to_string(),
    }
}

/// The actionable work-list — the concrete next steps an Implement/Enhance agent runs, worst axis
/// first (missing members ▸ native ▸ value ▸ shape ▸ wpt).
fn build_worklist(
    iface: &str,
    missing: &[String],
    native_bad: &[String],
    value_bad: &[String],
    shape_bad: &[String],
    wpt_fail: bool,
    wpt: Option<&Bucket>,
) -> Vec<String> {
    let mut out = Vec::new();
    if !missing.is_empty() {
        out.push(format!("impl {} missing WebIDL member(s): {}", missing.len(), head(missing, 6)));
    }
    for entry in native_bad.iter().take(8) {
        out.push(format!("nativize: {entry}"));
    }
    for entry in value_bad.iter().take(8) {
        out.push(format!("fix value: {entry}"));
    }
    for entry in shape_bad.iter().take(8) {
        out.push(format!("fix shape: {entry}"));
    }
    if wpt_fail {
        if let Some(b) = wpt {
            out.push(format!("wpt: {}/{} subtests passing for {iface}", b.passed, b.total));
        }
    }
    out
}

/// The first `n` items joined, with a `+k more` tail when truncated.
fn head(items: &[String], n: usize) -> String {
    let shown = items.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if items.len() > n {
        format!("{shown} +{} more", items.len() - n)
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{InterfaceSpec, MemberSpec};

    // A tiny Navigator slice: one clean member (hardwareConcurrency — present/native/shape/value all
    // green) and one missing member (getBattery), plus the baseline + profile they grade against.
    fn navigator_webidl() -> InterfaceSpec {
        InterfaceSpec {
            name: "Navigator".to_string(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Attribute {
                    name: "hardwareConcurrency".to_string(),
                    ty: "unsigned long long".to_string(),
                    readonly: true,
                    is_static: false,
                    inherited: false,
                put_forwards: false,
                },
                MemberSpec::Operation {
                    name: "getBattery".to_string(),
                    ret: "Promise".to_string(),
                    args: vec![],
                    is_static: false,
                },
            ],
        }
    }

    const SNAP: &str = r#"{
      "meta": { "engine": "korvus" },
      "interfaces": { "Navigator": { "members": [
        { "name": "hardwareConcurrency", "kind": "accessor", "get": true, "set": false,
          "native": true, "ownTell": false, "enumerable": true, "configurable": true,
          "getName": "get hardwareConcurrency", "getLength": 0 }
      ]}},
      "singletons": { "navigator": { "ctor": "Navigator", "members": [
        { "name": "hardwareConcurrency", "liveValue": 10 }
      ]}},
      "instances": {}
    }"#;

    // `getBattery` is in the BASELINE as well as the WebIDL, which is what makes it a GAP: the
    // reference browser exposes it and we do not. A member that appears only in the WebIDL corpus is a
    // different thing entirely — Chrome never shipped it — and is skipped rather than graded
    // (see `a_webidl_member_the_capture_does_not_record_is_not_graded` below).
    const BASELINE: &str = r#"{ "interfaces": { "Navigator": { "members": [
      { "name": "hardwareConcurrency", "kind": "accessor", "get": true, "set": false,
        "native": true, "enumerable": true, "configurable": true },
      { "name": "getBattery", "kind": "method", "native": true,
        "enumerable": true, "configurable": true, "length": 0 }
    ]}}}"#;

    const PROFILE: &str = r#"{ "nav": { "hardwareConcurrency": 10 } }"#;

    #[test]
    fn navigator_slice_grades_present_native_shape_value_and_flags_the_gap() {
        let webidl = navigator_webidl();
        let snap = SurfaceSnapshot::from_json(SNAP).unwrap();
        let baseline: Value = serde_json::from_str(BASELINE).unwrap();
        let profile = Profile::from_json(PROFILE).unwrap();
        let bucket = Bucket { passed: 40, total: 40, files: 3, paths: Default::default(), rules: Default::default() };

        let q = assess("Navigator", &webidl, &snap, &baseline, &profile, Some(&bucket), None, None);

        // The clean member is green on every axis.
        let hc = q.members.iter().find(|m| m.name == "hardwareConcurrency").unwrap();
        assert!(hc.present);
        assert!(hc.native.ok && hc.native.native && !hc.native.own_tell && !hc.native.length_defect);
        assert!(hc.shape.matches);
        assert_eq!(hc.shape.descriptor, StrPair { ours: Some("get".into()), reference: Some("get".into()) });
        assert_eq!(hc.value, ValueAxis { ours: Some(Value::from(10)), profile: Some(Value::from(10)), from_profile: true });
        // (B) No `Navigator-hardwareConcurrency` file exists in this fixture, so WPT says nothing
        // about this member — it no longer borrows the interface bucket's verdict.
        assert_eq!(hc.wpt, "untested");

        // The missing member drives axis ① and the verdict.
        let gb = q.members.iter().find(|m| m.name == "getBattery").unwrap();
        assert!(!gb.present);

        assert_eq!(q.rollup, Rollup {
            present: "1/2".into(),
            native: "1/1".into(),
            shape: "1/1".into(),
            value: "1/1".into(),
            wpt: "40/40".into(),
            behavior: "0/0".into(),
            spec: "1/1".into(),
            verified: "1/1".into(),
            // (D) hardwareConcurrency is proven by the PROFILE, not by a WPT file — self-proof.
            independent: "0/1".into(),
            consts: "0/0".into(),
            behavioral: "1/1".into(),
        });
        assert_eq!(q.verdict, "incomplete");
        assert!(q.worklist.iter().any(|w| w.contains("getBattery")), "worklist: {:?}", q.worklist);
    }

    // THE CAPTURE IS THE DENOMINATOR. The webref corpus describes the standard, which includes a long
    // tail Chrome never shipped; grading against that tail told the campaign to ADD prototype members
    // the reference browser does not have — 519 of the 547 it once reported "missing" were of this kind,
    // and implementing any of them makes the fingerprint worse, not better.
    //
    // So such a member is skipped outright rather than counted absent: counted absent, it would leave
    // `present` with an unreachable ceiling that reads as remaining work forever.
    #[test]
    fn a_webidl_member_the_capture_does_not_record_is_not_graded() {
        let mut webidl = navigator_webidl();
        // A real example: webref declares it, Chrome 151 does not expose it.
        webidl.members.push(MemberSpec::Attribute {
            name: "cpuPerformance".into(),
            ty: "unsigned long".into(),
            readonly: true,
            is_static: false,
            inherited: false,
            put_forwards: false,
        });
        let snap = SurfaceSnapshot::from_json(SNAP).unwrap();
        let baseline: Value = serde_json::from_str(BASELINE).unwrap();
        let profile = Profile::from_json(PROFILE).unwrap();

        let q = assess("Navigator", &webidl, &snap, &baseline, &profile, None, None, None);

        assert!(
            !q.members.iter().any(|m| m.name == "cpuPerformance"),
            "a member absent from the capture must not be graded at all: {:?}",
            q.members.iter().map(|m| &m.name).collect::<Vec<_>>()
        );
        assert!(
            !q.worklist.iter().any(|w| w.contains("cpuPerformance")),
            "and it must never reach the worklist: {:?}",
            q.worklist
        );
        // getBattery IS in the capture, so the real gap still drives ① — the filter narrows the
        // denominator, it does not blunt the signal.
        assert_eq!(q.rollup.present, "1/2");
    }

    // THE DENOMINATOR IS THE CAPTURE IN BOTH DIRECTIONS. The first version of the capture filter only
    // narrowed -- it dropped webref members Chrome does not ship -- and so it lost the opposite case: a
    // member the CAPTURE has that webref does not declare was never graded at all. `ReadableStream`'s
    // six (cancel/locked/pipeThrough/pipeTo/tee/values) and `Range.expand` sat in that hole while the
    // rollup reported the gap closed; a direct capture-vs-live diff found them.
    #[test]
    fn a_capture_member_webidl_does_not_declare_is_still_graded() {
        // WebIDL declares only hardwareConcurrency; the capture also has getBattery, which we lack.
        let webidl = InterfaceSpec {
            name: "Navigator".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: "hardwareConcurrency".into(),
                ty: "unsigned long".into(),
                readonly: true,
                is_static: false,
                inherited: false,
                put_forwards: false,
            }],
        };
        let snap = SurfaceSnapshot::from_json(SNAP).unwrap();
        let baseline: Value = serde_json::from_str(BASELINE).unwrap();
        let profile = Profile::from_json(PROFILE).unwrap();

        let q = assess("Navigator", &webidl, &snap, &baseline, &profile, None, None, None);

        assert_eq!(q.rollup.present, "1/2", "the capture-only member is in the denominator");
        assert!(
            q.members.iter().any(|m| m.name == "getBattery" && !m.present),
            "and it is graded as absent: {:?}",
            q.members.iter().map(|m| (&m.name, m.present)).collect::<Vec<_>>()
        );
        assert!(q.worklist.iter().any(|w| w.contains("getBattery")), "worklist: {:?}", q.worklist);
    }

    // The filter needs a capture record to act on. An interface the capture never walked (or one whose
    // prototype it recorded as empty, like the `[LegacyUnforgeable]` `Location`) has no basis for
    // exclusion, so everything the WebIDL declares is still graded.
    #[test]
    fn with_no_capture_record_every_webidl_member_is_still_graded() {
        let webidl = navigator_webidl();
        let snap = SurfaceSnapshot::from_json(SNAP).unwrap();
        let baseline: Value = serde_json::from_str(r#"{ "interfaces": {} }"#).unwrap();
        let profile = Profile::from_json(PROFILE).unwrap();

        let q = assess("Navigator", &webidl, &snap, &baseline, &profile, None, None, None);

        assert_eq!(q.rollup.present, "1/2", "both WebIDL members graded when the capture is silent");
        assert!(q.members.iter().any(|m| m.name == "getBattery"));
    }

    #[test]
    fn a_non_native_present_member_is_not_native_not_incomplete() {
        // Both members present, but one lies about being native -> verdict not-native (present is full).
        let webidl = InterfaceSpec {
            name: "Navigator".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: "hardwareConcurrency".into(),
                ty: "unsigned long".into(),
                readonly: true,
                is_static: false,
                inherited: false,
            put_forwards: false,
            }],
        };
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Navigator": { "members": [
                { "name": "hardwareConcurrency", "kind": "accessor", "get": true, "set": false,
                  "native": false, "ownTell": false, "enumerable": true, "configurable": true,
                  "getName": "get hardwareConcurrency", "getLength": 0 } ]}},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        // This fixture's own baseline, not the shared BASELINE: the shared one also records
        // `getBattery`, and the capture is the denominator in BOTH directions now, so that member would
        // correctly count as missing here and turn this into an `incomplete` -- which is a real rule,
        // just not the one this test is about.
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Navigator": { "members": [
              { "name": "hardwareConcurrency", "kind": "accessor", "get": true, "set": false,
                "native": true, "enumerable": true, "configurable": true } ]}}}"#,
        )
        .unwrap();
        let profile = Profile::from_json(PROFILE).unwrap();

        let q = assess("Navigator", &webidl, &snap, &baseline, &profile, None, None, None);
        assert_eq!(q.rollup.present, "1/1");
        assert_eq!(q.rollup.native, "0/1");
        assert_eq!(q.verdict, "not-native");
        assert!(q.worklist.iter().any(|w| w.contains("not [native code]")), "{:?}", q.worklist);
    }

    // Phase A: the interface constructor's own nativeness is an axis of its own (subsumes `nolies`'s
    // ctor half). A ctor that is not `[native code]` where the baseline IS drives the not-native verdict.
    fn one_attr(iface: &str, name: &str) -> InterfaceSpec {
        InterfaceSpec {
            name: iface.into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: name.into(),
                ty: "DOMString".into(),
                readonly: true,
                is_static: false,
                inherited: false,
            put_forwards: false,
            }],
        }
    }

    #[test]
    fn ctor_axis_flags_a_non_native_constructor() {
        let webidl = one_attr("Event", "type");
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Event": {
                "ctorNative": false, "ctorName": "Event", "length": 1, "proto": ["Object"], "toStringTag": "Event",
                "members": [ { "name": "type", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get type", "getLength": 0 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Event": {
                "ctorNative": true, "ctorName": "Event", "proto": ["Object"], "toStringTag": "Event",
                "members": [ { "name": "type", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let q = assess("Event", &webidl, &snap, &baseline, &profile, None, None, None);
        assert!(!q.ctor.native);
        assert!(!q.ctor.ok);
        assert_eq!(q.verdict, "not-native");
        assert!(q.worklist.iter().any(|w| w.contains("constructor")), "{:?}", q.worklist);
    }

    // Phase A: the structure axis diffs the live prototype's extra/missing members + ctor name + proto
    // chain + toStringTag against the baseline (subsumes `surface_parity`'s structural diff). An extra
    // member is a leak/tell; a wrong toStringTag is a shape lie.
    #[test]
    fn structure_axis_flags_an_extra_member_and_tag_mismatch() {
        let webidl = one_attr("Event", "type");
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Event": {
                "ctorNative": true, "ctorName": "Event", "proto": ["Object"], "toStringTag": "WrongTag",
                "members": [
                  { "name": "type", "kind": "accessor", "get": true, "set": false, "native": true,
                    "enumerable": true, "configurable": true, "getName": "get type", "getLength": 0 },
                  { "name": "__korvusLeak", "kind": "method", "native": true, "enumerable": true,
                    "configurable": true, "length": 0, "fnName": "__korvusLeak" } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Event": {
                "ctorNative": true, "ctorName": "Event", "proto": ["Object"], "toStringTag": "Event",
                "members": [ { "name": "type", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let q = assess("Event", &webidl, &snap, &baseline, &profile, None, None, None);
        assert!(q.structure.extra_members.iter().any(|m| m == "__korvusLeak"), "{:?}", q.structure);
        assert_eq!(
            q.structure.to_string_tag,
            StrPair { ours: Some("WrongTag".into()), reference: Some("Event".into()) }
        );
        assert!(!q.structure.matches);
        // present full + ctor ok, but the structural mismatch drops it to low-quality.
        assert_eq!(q.verdict, "low-quality");
        assert!(q.worklist.iter().any(|w| w.contains("__korvusLeak")), "{:?}", q.worklist);
    }

    // Phase A: the full-surface tell scan (subsumes surface_parity's OWN-TELLS + NAME-DEFECT across ALL
    // dumped interfaces, not just the @webref-corpus intersection). A member fn owning prototype/
    // arguments/caller, or a native fn with an empty name, is a JS-shim tell — flagged wherever it lives.
    #[test]
    fn scan_tells_flags_own_tells_across_the_full_surface() {
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": {
                "VendorThing": { "members": [
                  { "name": "doIt", "kind": "method", "native": true, "ownTell": true,
                    "enumerable": true, "configurable": true, "length": 0, "fnName": "doIt" } ] },
                "RawThing": { "members": [
                  { "name": "raw", "kind": "method", "native": true, "ownTell": false,
                    "enumerable": true, "configurable": true, "length": 0, "fnName": "" } ] },
                "CleanThing": { "members": [
                  { "name": "ok", "kind": "method", "native": true, "ownTell": false,
                    "enumerable": true, "configurable": true, "length": 0, "fnName": "ok" } ] } },
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let report = scan_tells(&snap);
        assert!(report.own_tells.iter().any(|t| t.contains("VendorThing.doIt")), "{:?}", report);
        assert!(report.name_defects.iter().any(|t| t.contains("RawThing.raw")), "{:?}", report);
        assert!(!report.own_tells.iter().any(|t| t.contains("CleanThing")), "{:?}", report);
        assert!(!report.ok);
    }

    // Phase A: the report-level globals scan (subsumes `automation_tells` + surface_parity's global
    // diff). Automation tells (cdc_/$cdc_/__webdriver/...) and korvus-only extra globals both fail it.
    #[test]
    fn scan_globals_flags_automation_tells_and_extra_globals() {
        let snap = SurfaceSnapshot::from_json(
            r#"{ "globals": ["Event", "URL", "cdc_adoQpoasnfa76pfcZLmcfl_Array", "__korvusOnly"],
                "documentKeys": ["title", "$cdc_asdjflasutopfhvcZLmcfl_"], "fpToStringNative": false,
                "interfaces": {}, "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(r#"{ "globals": ["Event", "URL", "Navigator"] }"#).unwrap();
        let report = scan_globals(&snap, &baseline);
        assert!(report.automation_tells.iter().any(|g| g.starts_with("cdc_")), "{:?}", report);
        assert!(report.document_tells.iter().any(|g| g.starts_with("$cdc_")), "{:?}", report);
        assert!(!report.fp_tostring_native, "FP.toString reported non-native");
        assert!(report.extra_globals.iter().any(|g| g == "__korvusOnly"), "{:?}", report);
        assert!(report.missing_globals.iter().any(|g| g == "Navigator"), "{:?}", report);
        assert!(!report.ok);
    }

    // Group 3 (universal): `new X()` must behave per WebIDL — an argless-constructable interface that
    // throws is a behavioral tell.
    #[test]
    fn ctor_newable_matches_webidl_constructor() {
        let webidl = InterfaceSpec {
            name: "Ev".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Constructor { args: vec![], html_constructor: false },
                MemberSpec::Attribute { name: "type".into(), ty: "T".into(), readonly: true, is_static: false, inherited: false, put_forwards: false },
            ],
        };
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Ev": {
                "ctorNative": true, "ctorName": "Ev", "newable": false, "proto": [],
                "members": [ { "name": "type", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get type", "getLength": 0 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Ev": { "ctorNative": true, "proto": [], "members": [
                { "name": "type", "kind": "accessor", "get": true, "set": false } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let q = assess("Ev", &webidl, &snap, &baseline, &profile, None, None, None);
        assert!(!q.ctor.newable_ok, "argless-constructable but new X() threw");
        assert!(q.ctor.ok, "native + named ctor -> the tell axis is clean; newable is a separate gap");
        // A newable gap is a constructability quality issue (low-quality), not a native-tell (not-native).
        assert_eq!(q.verdict, "low-quality");
        assert!(q.worklist.iter().any(|w| w.contains("newable")), "{:?}", q.worklist);
    }

    // Group 4 (universal): each interface must live in the realm its WebIDL [Exposed] declares — a
    // Window-only interface in the worker is a LEAK (tell); a Worker-exposed one missing is a gap.
    #[test]
    fn check_exposed_flags_leaks_and_gaps() {
        let mk = |name: &str, exposed: Vec<&str>| InterfaceSpec {
            name: name.into(),
            inherits: None,
            exposed: exposed.iter().map(|s| s.to_string()).collect(),
            members: vec![],
        };
        let mut corpus: std::collections::BTreeMap<String, InterfaceSpec> = std::collections::BTreeMap::new();
        corpus.insert("WinOnly".into(), mk("WinOnly", vec!["Window"]));
        corpus.insert("Both".into(), mk("Both", vec!["Window", "DedicatedWorker"]));
        corpus.insert("WorkerGap".into(), mk("WorkerGap", vec!["Window", "Worker"]));
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "WinOnly": { "members": [] }, "Both": { "members": [] }, "WorkerGap": { "members": [] } },
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        // Korvus's worker exposes WinOnly (a LEAK) + Both (ok per DedicatedWorker), but not WorkerGap.
        let worker: std::collections::BTreeSet<String> =
            ["WinOnly", "Both"].iter().map(|s| s.to_string()).collect();
        let r = check_exposed(&corpus, &snap, &worker);
        assert_eq!(r.leaked_to_worker, vec!["WinOnly".to_string()]);
        assert_eq!(r.missing_in_worker, vec!["WorkerGap".to_string()]);
        assert!(!r.ok);
    }

    // Step 2: a present + native member whose oracle cases produce the WRONG output is `behavior-fail`
    // (the member exists and looks native, but doesn't actually work).
    #[test]
    fn a_recorded_expectation_is_external_proof_but_an_authored_one_is_not() {
        // Same interface, same passing case. The only difference is who decided the right answer.
        let webidl = one_attr("URL", "protocol");
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "URL": { "ctorNative": true, "ctorName": "URL", "members": [
                { "name": "protocol", "kind": "accessor", "get": true, "set": true, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get protocol", "getLength": 0,
                  "setName": "set protocol", "setLength": 1 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "URL": { "ctorNative": true, "ctorName": "URL", "members": [
                { "name": "protocol", "kind": "accessor", "get": true, "set": true, "native": true,
                  "enumerable": true, "configurable": true } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();

        let mut authored = std::collections::BTreeMap::new();
        authored.insert(
            "protocol".to_string(),
            crate::oracle::BehaviorObs { pass: 1, total: 1, external: 0, fail_detail: None },
        );
        let q = assess("URL", &webidl, &snap, &baseline, &profile, None, None, Some(&authored));
        let m = q.members.iter().find(|m| m.name == "protocol").unwrap();
        assert!(m.verified, "a passing case still proves the member works");
        assert!(!m.independent, "but the implementer wrote the answer -- that is self-proof");
        assert_eq!(q.rollup.independent, "0/1");

        let mut recorded = std::collections::BTreeMap::new();
        recorded.insert(
            "protocol".to_string(),
            crate::oracle::BehaviorObs { pass: 1, total: 1, external: 1, fail_detail: None },
        );
        let q = assess("URL", &webidl, &snap, &baseline, &profile, None, None, Some(&recorded));
        let m = q.members.iter().find(|m| m.name == "protocol").unwrap();
        assert!(m.verified);
        assert!(m.independent, "a browser-recorded answer is proof the implementer did not author");
        assert_eq!(q.rollup.independent, "1/1");
    }

    #[test]
    fn behavior_axis_drives_the_verdict_when_output_is_wrong() {
        let webidl = one_attr("URL", "protocol");
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "URL": { "ctorNative": true, "ctorName": "URL", "members": [
                { "name": "protocol", "kind": "accessor", "get": true, "set": true, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get protocol", "getLength": 0,
                  "setName": "set protocol", "setLength": 1 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "URL": { "ctorNative": true, "ctorName": "URL", "members": [
                { "name": "protocol", "kind": "accessor", "get": true, "set": true, "native": true,
                  "enumerable": true, "configurable": true } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();

        let mut obs = std::collections::BTreeMap::new();
        obs.insert(
            "protocol".to_string(),
            crate::oracle::BehaviorObs {
                pass: 1,
                total: 2,
                external: 0,
                fail_detail: Some("protocol ours=\"http:\" want=\"https:\"".into()),
            },
        );

        let q = assess("URL", &webidl, &snap, &baseline, &profile, None, None, Some(&obs));
        let p = q.members.iter().find(|m| m.name == "protocol").unwrap();
        assert!(p.behavior.tested);
        assert_eq!((p.behavior.pass, p.behavior.total), (1, 2));
        assert!(!p.behavior.ok);
        assert_eq!(q.rollup.behavior, "0/1");
        assert_eq!(q.verdict, "behavior-fail");
        assert!(q.worklist.iter().any(|w| w.contains("fix behavior")), "{:?}", q.worklist);
    }

    // Group 1 (universal spec conformance, no per-case): live kind matches the WebIDL member type,
    // readonly attributes have no setter, const live values match the declaration, and the inheritance
    // edge matches `inherits`.
    #[test]
    fn spec_axis_checks_kind_readonly_const_and_inheritance() {
        let webidl = InterfaceSpec {
            name: "Node".into(),
            inherits: Some("EventTarget".into()),
            exposed: vec![],
            members: vec![
                MemberSpec::Const { name: "ELEMENT_NODE".into(), ty: "unsigned short".into(), value: "1".into() },
                MemberSpec::Attribute {
                    name: "nodeName".into(),
                    ty: "DOMString".into(),
                    readonly: true,
                    is_static: false,
                    inherited: false,
                put_forwards: false,
                },
            ],
        };
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Node": {
                "ctorNative": true, "ctorName": "Node", "proto": ["EventTarget"],
                "members": [
                  { "name": "ELEMENT_NODE", "kind": "value", "type": "number", "value": 1,
                    "enumerable": true, "configurable": false },
                  { "name": "nodeName", "kind": "accessor", "get": true, "set": true, "native": true,
                    "enumerable": true, "configurable": true, "getName": "get nodeName", "getLength": 0,
                    "setName": "set nodeName", "setLength": 1 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Node": { "proto": ["EventTarget"], "members": [
                { "name": "ELEMENT_NODE", "kind": "value" },
                { "name": "nodeName", "kind": "accessor", "get": true, "set": true } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let q = assess("Node", &webidl, &snap, &baseline, &profile, None, None, None);

        let nn = q.members.iter().find(|m| m.name == "nodeName").unwrap();
        assert!(nn.spec.kind_ok, "accessor kind ok");
        assert!(!nn.spec.readonly_ok, "readonly attr with a setter must fail");
        assert!(!nn.spec.ok);
        let el = q.members.iter().find(|m| m.name == "ELEMENT_NODE").unwrap();
        assert!(el.spec.const_ok, "const live value 1 == WebIDL 1");
        assert_eq!(
            q.structure.inherits,
            StrPair { ours: Some("EventTarget".into()), reference: Some("EventTarget".into()) }
        );
        // present full + native ok + structure ok, but the readonly violation drops it to low-quality.
        assert_eq!(q.verdict, "low-quality");
        assert!(q.worklist.iter().any(|w| w.contains("readonly")), "{:?}", q.worklist);
    }

    // Group 2 (universal): the same members exposed in a DIFFERENT enumeration order than the baseline
    // (order is itself a fingerprint signal) fails the structure order check.
    #[test]
    fn structure_order_axis_flags_reordered_members() {
        let webidl = InterfaceSpec {
            name: "X".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Attribute { name: "a".into(), ty: "T".into(), readonly: true, is_static: false, inherited: false, put_forwards: false },
                MemberSpec::Attribute { name: "b".into(), ty: "T".into(), readonly: true, is_static: false, inherited: false, put_forwards: false },
            ],
        };
        // Korvus dumps them b, a — the baseline order is a, b.
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "X": { "ctorNative": true, "ctorName": "X", "members": [
                { "name": "b", "kind": "accessor", "get": true, "set": false, "native": true, "enumerable": true, "configurable": true, "getName": "get b", "getLength": 0 },
                { "name": "a", "kind": "accessor", "get": true, "set": false, "native": true, "enumerable": true, "configurable": true, "getName": "get a", "getLength": 0 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "X": { "members": [
                { "name": "a", "kind": "accessor", "get": true, "set": false },
                { "name": "b", "kind": "accessor", "get": true, "set": false } ] }}}"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let q = assess("X", &webidl, &snap, &baseline, &profile, None, None, None);
        assert!(!q.structure.order_ok, "reordered members must fail the order check");
        assert!(!q.structure.matches);
        assert_eq!(q.verdict, "low-quality");
        assert!(q.worklist.iter().any(|w| w.contains("order")), "{:?}", q.worklist);
    }

    // The honest complete/verified split: a surface-perfect interface with NO dynamic coverage is only
    // `complete` (its value/behavior is UNVERIFIED); once a positive check confirms it, it is `verified`.
    #[test]
    fn verdict_splits_complete_vs_verified_by_dynamic_coverage() {
        let webidl = one_attr("Screen", "width");
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Screen": { "ctorNative": true, "ctorName": "Screen", "proto": [],
                "members": [ { "name": "width", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get width", "getLength": 0 } ] }},
                "singletons": { "screen": { "ctor": "Screen", "members": [ { "name": "width", "liveValue": 1512 } ] } },
                "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Screen": { "ctorNative": true, "proto": [], "members": [
                { "name": "width", "kind": "accessor", "get": true, "set": false } ] }}}"#,
        )
        .unwrap();

        // Empty profile -> width's value is unmeasured -> surface-only -> complete.
        let empty = Profile::from_json("{}").unwrap();
        let q1 = assess("Screen", &webidl, &snap, &baseline, &empty, None, None, None);
        assert_eq!(q1.verdict, "complete", "surface-complete but width's value UNVERIFIED");
        assert_eq!(q1.rollup.verified, "0/1");

        // Profile carries screen.width -> the value is confirmed -> verified.
        let prof = Profile::from_json(r#"{ "screen": { "width": 1512 } }"#).unwrap();
        let q2 = assess("Screen", &webidl, &snap, &baseline, &prof, None, None, None);
        assert_eq!(q2.rollup.verified, "1/1", "width's value matches the profile -> dynamically verified");
        // (D) …but the proof is ours, not upstream's, so the verdict says so.
        assert_eq!(q2.verdict, "self-verified");
        assert_eq!(q2.rollup.independent, "0/1");
    }

    // A passing INTERFACE-WIDE WPT bucket must not verify members that have no WPT file of their own.
    // Regression guard for the free-ride this fallback used to grant: Document's real corpus exercises
    // only getElementById/getElementsByTagName, yet the interface bucket handed all 59 of its present
    // members a `verified` -- `cookie`, `title` and the rest included.
    #[test]
    fn interface_wide_wpt_does_not_verify_members_without_their_own_file() {
        let webidl = one_attr("Screen", "width");
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Screen": { "ctorNative": true, "ctorName": "Screen", "proto": [],
                "members": [ { "name": "width", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get width", "getLength": 0 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Screen": { "ctorNative": true, "proto": [], "members": [
                { "name": "width", "kind": "accessor", "get": true, "set": false } ] }}}"#,
        )
        .unwrap();
        let empty = Profile::from_json("{}").unwrap();
        let green = Bucket { passed: 9, total: 9, files: 1, paths: Default::default(), rules: Default::default() };

        // Interface bucket fully green, but no `Screen-width` file. (B) The column must say `untested`,
        // not borrow the interface's result: printing `pass`/`fail` next to a member no file touches is
        // a fabricated per-member claim either way.
        let q = assess("Screen", &webidl, &snap, &baseline, &empty, Some(&green), None, None);
        assert_eq!(q.members[0].wpt, "untested", "no dedicated file ⇒ WPT says nothing about this member");
        assert!(!q.members[0].verified, "an interface-wide pass is not per-member proof");
        assert_eq!(q.rollup.verified, "0/1");
        assert_eq!(q.verdict, "complete");

        // The member's OWN dedicated file passing IS proof — and (D) it is EXTERNAL proof.
        let mut own = std::collections::BTreeMap::new();
        own.insert("width".to_string(), green.clone());
        let q2 = assess("Screen", &webidl, &snap, &baseline, &empty, Some(&green), Some(&own), None);
        assert!(q2.members[0].verified, "a dedicated passing WPT file verifies the member");
        assert!(q2.members[0].independent, "a WPT pass is external proof, not self-proof");
        assert_eq!(q2.rollup.verified, "1/1");
        assert_eq!(q2.rollup.independent, "1/1");
        assert_eq!(q2.verdict, "verified");
    }

    // (D) A failing dedicated WPT file OUTRANKS a passing behavior case. This is the self-grading
    // problem in its purest form: `URLSearchParams.delete` reported `wpt fail` and `verified yes` in
    // the same row, because the case we wrote agreed with the code we wrote.
    #[test]
    fn a_failing_wpt_file_outranks_our_own_passing_case() {
        let webidl = InterfaceSpec {
            name: "Screen".into(),
            inherits: None,
            exposed: vec![],
            members: vec![MemberSpec::Attribute {
                name: "width".into(),
                ty: "long".into(),
                readonly: true,
                is_static: false,
                inherited: false,
            put_forwards: false,
            }],
        };
        let snap = SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "Screen": { "ctorNative": true, "ctorName": "Screen", "proto": [],
                "members": [ { "name": "width", "kind": "accessor", "get": true, "set": false, "native": true,
                  "enumerable": true, "configurable": true, "getName": "get width", "getLength": 0 } ] }},
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "Screen": { "ctorNative": true, "proto": [], "members": [
                { "name": "width", "kind": "accessor", "get": true, "set": false } ] }}}"#,
        )
        .unwrap();
        let empty = Profile::from_json("{}").unwrap();
        let red = Bucket { passed: 3, total: 9, files: 1, paths: Default::default(), rules: Default::default() };
        let mut own = std::collections::BTreeMap::new();
        own.insert("width".to_string(), red.clone());

        // Our own case passes...
        let mut obs = std::collections::BTreeMap::new();
        obs.insert(
            "width".to_string(),
            crate::oracle::BehaviorObs { pass: 3, total: 3, external: 0, fail_detail: None },
        );

        let q = assess("Screen", &webidl, &snap, &baseline, &empty, Some(&red), Some(&own), Some(&obs));
        assert_eq!(q.members[0].wpt, "fail");
        assert!(q.members[0].behavior.ok, "our own case passes");
        assert!(!q.members[0].verified, "...but upstream says it is wrong, so it is NOT verified");
        assert_eq!(q.rollup.verified, "0/1");
    }

    // --- Anonymous specials (the named-property machinery): graded ONLY through the ⑥ behavior axis.
    // A special-bearing interface with no cases keeps its verdict (no false missing member); passing
    // cases make it verified; failing cases drive behavior-fail with an actionable worklist entry. ---

    fn domstringmap_webidl() -> InterfaceSpec {
        use crate::spec::SpecialOp;
        InterfaceSpec {
            name: "DOMStringMap".into(),
            inherits: None,
            exposed: vec![],
            members: vec![
                MemberSpec::Special { special: SpecialOp::NamedGetter },
                MemberSpec::Special { special: SpecialOp::NamedSetter },
                MemberSpec::Special { special: SpecialOp::NamedDeleter },
            ],
        }
    }

    fn domstringmap_snap() -> SurfaceSnapshot {
        SurfaceSnapshot::from_json(
            r#"{ "interfaces": { "DOMStringMap": { "ctorNative": true, "ctorName": "DOMStringMap",
                "newable": false, "proto": [], "toStringTag": "DOMStringMap", "members": [] } },
                "singletons": {}, "instances": {} }"#,
        )
        .unwrap()
    }

    #[test]
    fn untested_specials_contribute_nothing() {
        let webidl = domstringmap_webidl();
        let snap = domstringmap_snap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "DOMStringMap": { "ctorNative": true, "proto": [], "members": [] } } }"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let q = assess("DOMStringMap", &webidl, &snap, &baseline, &profile, None, None, None);
        assert!(q.members.is_empty(), "untested specials add no rows: {:?}", q.members);
        assert_eq!(q.rollup.present, "0/0");
        assert_eq!(q.rollup.verified, "0/0");
        assert_eq!(q.verdict, "complete");
        assert!(q.worklist.is_empty());
    }

    #[test]
    fn passing_special_cases_verify_the_interface() {
        let webidl = domstringmap_webidl();
        let snap = domstringmap_snap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "DOMStringMap": { "ctorNative": true, "proto": [], "members": [] } } }"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let mut obs = std::collections::BTreeMap::new();
        obs.insert("named getter".to_string(), crate::oracle::BehaviorObs { pass: 4, total: 4, external: 0, fail_detail: None });
        obs.insert("named setter".to_string(), crate::oracle::BehaviorObs { pass: 3, total: 3, external: 0, fail_detail: None });
        obs.insert("named deleter".to_string(), crate::oracle::BehaviorObs { pass: 2, total: 2, external: 0, fail_detail: None });
        let q = assess("DOMStringMap", &webidl, &snap, &baseline, &profile, None, None, Some(&obs));
        assert_eq!(q.members.len(), 3);
        assert!(q.members.iter().all(|m| m.present && m.verified && m.native.ok && m.shape.matches && m.spec.ok));
        assert_eq!(q.rollup.present, "3/3");
        assert_eq!(q.rollup.behavior, "3/3");
        assert_eq!(q.rollup.verified, "3/3");
        // (D) A named special has no `Interface-member` WPT file by construction, so behavior cases are
        // its ONLY possible proof — such an interface can never be more than `self-verified`.
        assert_eq!(q.verdict, "self-verified");
        assert_eq!(q.rollup.independent, "0/3");
    }

    #[test]
    fn failing_special_cases_drive_behavior_fail() {
        let webidl = domstringmap_webidl();
        let snap = domstringmap_snap();
        let baseline: Value = serde_json::from_str(
            r#"{ "interfaces": { "DOMStringMap": { "ctorNative": true, "proto": [], "members": [] } } }"#,
        )
        .unwrap();
        let profile = Profile::from_json("{}").unwrap();
        let mut obs = std::collections::BTreeMap::new();
        obs.insert(
            "named getter".to_string(),
            crate::oracle::BehaviorObs { pass: 1, total: 2, external: 0, fail_detail: Some("named getter ours=undefined want=x".into()) },
        );
        let q = assess("DOMStringMap", &webidl, &snap, &baseline, &profile, None, None, Some(&obs));
        assert_eq!(q.rollup.present, "1/1");
        assert_eq!(q.rollup.verified, "0/1");
        assert_eq!(q.verdict, "behavior-fail");
        assert!(
            q.worklist.iter().any(|w| w.contains("named getter")),
            "worklist should point at the failing special: {:?}",
            q.worklist
        );
    }
}
