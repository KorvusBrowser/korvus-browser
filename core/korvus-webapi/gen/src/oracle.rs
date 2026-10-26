//! The behavioral capture oracle (Pillar B of the correctness tester): per-member
//! `expr -> expected output` cases, captured from real Chrome / known spec behavior (self-owned
//! fixtures). The harness evals each `expr` in Korvus and folds the outcome (via [`BehaviorObs::observe`])
//! into a per-member pass/total tally, which the quality report turns into the "does this member actually
//! WORK?" axis — the piece present/native/shape can't answer.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The emitted per-(interface, member) behavior tally: what the harness writes and the `webapi quality`
/// CLI reads, so the CLI's per-member report carries the ⑥ behavior column too.
pub type ObsMap = BTreeMap<String, BTreeMap<String, BehaviorObs>>;

/// Read an emitted [`ObsMap`] from a path (missing/invalid ⇒ empty ⇒ behavior simply ungraded).
pub fn read_obs(path: &Path) -> ObsMap {
    std::fs::read_to_string(path).ok().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
}

/// One behavioral case. Eval `expr` in Korvus: its result must equal `expected`, OR (when `undefined`
/// is set) be `undefined`, OR (when `throws` is set) throw an error whose `.name` is `throws`. A case
/// with none of the three — i.e. the `expected` key is ABSENT, not present-and-`null` — just asserts
/// `expr` does not throw.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BehaviorCase {
    pub iface: String,
    pub member: String,
    pub expr: String,
    /// The value the expression must evaluate to.
    ///
    /// `None` -- the `expected` key is **absent**: the case makes no value claim and asserts only that
    /// `expr` does not throw.
    /// `Some(Value::Null)` -- the key is **present and `null`**: the member must actually RETURN `null`.
    ///
    /// Those are different assertions and `#[serde(default)]` alone cannot tell them apart: `Option`'s
    /// own deserializer maps a JSON `null` to `None`, so a recorded `"expected": null` collapsed into
    /// "no expectation" and the case degraded to *does not throw* -- it would have passed on `0`, `""`
    /// or any other value the member happened to return. [`de_expected`] separates the two without
    /// changing a byte on disk: `default` supplies `None` for an absent key, and the custom
    /// deserializer -- which serde invokes only when the key IS present -- reads a full [`Value`]
    /// (where JSON `null` is `Value::Null`) and wraps it in `Some`.
    #[serde(default, deserialize_with = "de_expected")]
    pub expected: Option<Value>,
    #[serde(default)]
    pub throws: Option<String>,
    /// Where the expectation came from.
    ///
    /// `None` -- the implementer wrote it. That is self-proof: the same agent authored the code AND
    /// the answer that judges it, so a misread spec yields a wrong implementation and a matching wrong
    /// expectation, and they agree.
    ///
    /// `Some("brave-151-linux-headless")` -- recorded from a real browser by
    /// the capture rig (collect.js), which evaluates this very `expr` with this very runner. The agent
    /// still chooses what to test; it no longer decides what the right answer is.
    ///
    /// The tag names the BOX it was recorded on, not just the browser, because the persona is
    /// `the shipped bundle` and a capture taken on Linux headless is not automatically valid for it. Most
    /// WebIDL semantics are platform-independent -- a capture on another platform re-evaluates
    /// the corpus under macOS UA/platform/timezone emulation and currently reports 0 of 639 answers
    /// moving -- but the tag must not quietly imply the two are the same capture.
    ///
    /// Only a `chrome-<major>` head counts (see `is_recorded`); an arbitrary string cannot promote a
    /// case to external evidence.
    #[serde(default)]
    pub source: Option<String>,
    /// The expression must evaluate to `undefined`. JSON has no `undefined`, and `null` is a different
    /// answer that a real browser distinguishes, so it needs its own flag rather than an encoding.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub undefined: bool,
    /// The answer arrives ASYNCHRONOUSLY: `expr` evaluates to a promise, and the recorded `expected`
    /// is what that promise SETTLES to (or `throws` is the name of what it rejects with).
    ///
    /// Opt-in per case, and deliberately so. `behavior-run.js` awaits only the cases carrying this
    /// flag; every other case runs through the identical synchronous loop it always did. A global
    /// flip to `await` would instead re-record the ~137 members currently answered "[object Promise]"
    /// as their settled values -- a corpus-wide semantic change that the provenance system could not
    /// flag, because the questions would not have changed.
    ///
    /// Spelled `await` on disk (the JS keyword the flag is about); `awaited` in Rust because `await`
    /// is reserved there too, and `r#await` reads worse than it explains.
    #[serde(default, rename = "await", skip_serializing_if = "std::ops::Not::not")]
    pub awaited: bool,
}

/// Deserialize a PRESENT `expected` key, keeping `null` as an expectation rather than losing it.
///
/// Paired with `#[serde(default)]` on the field: serde calls this only for a key that is actually in
/// the JSON, so `None` on the field means "absent" and can no longer be produced by a `null`.
fn de_expected<'de, D>(d: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(d).map(Some)
}

impl BehaviorCase {
    /// True when `expected`/`throws` was RECORDED from a real browser rather than authored here.
    ///
    /// Deliberately narrow: `<engine>-<major>-<os>[-headless]` is the shape the capture ids
    /// writes and nothing else is honoured, so a case cannot claim independence by inventing a
    /// provenance string. The corpus is agent-writable, so this is a shape check, not a signature --
    /// the gate additionally strips any `source` an agent introduces (see `quality-gate.ts`).
    ///
    /// `brave` and `chrome` are both engines we record from. They are NOT interchangeable -- Brave
    /// farbles some members -- but which one a case came from is provenance for a human to read, not
    /// something the grader can adjudicate.
    pub fn is_recorded(&self) -> bool {
        let Some(s) = self.source.as_deref() else { return false };
        let rest = match s.split_once('-') {
            Some(("brave" | "chrome", rest)) => rest,
            _ => return false,
        };
        let major = rest.split('-').next().unwrap_or("");
        !major.is_empty() && major.chars().all(|c| c.is_ascii_digit())
    }
}

/// Load the case corpus from a JSON array.
pub fn load(json: &str) -> serde_json::Result<Vec<BehaviorCase>> {
    serde_json::from_str(json)
}

/// Load the case corpus from a file (missing file ⇒ empty corpus, so behavior is simply ungraded).
pub fn from_path(path: &Path) -> Vec<BehaviorCase> {
    std::fs::read_to_string(path).ok().and_then(|j| load(&j).ok()).unwrap_or_default()
}

/// Load the case corpus from a DIRECTORY of per-interface files (`<Iface>.json`, each a JSON array) —
/// the parallel-safe layout: one file per interface means N campaign workers authoring cases for
/// different interfaces never touch the same path, so their worktrees merge without conflict.
///
/// Read at RUNTIME (not `include_str!`), so adding or editing cases does not force a rebuild of the
/// harness — a behavior-case-only round skips the korvus-webapi recompile entirely.
///
/// A missing directory ⇒ empty corpus (behavior simply ungraded). A single unparseable/unreadable file
/// is skipped rather than poisoning the whole corpus, but its name is returned so the caller can
/// surface it — a silently-dropped interface would read as "no cases" and hand out a free `complete`.
pub fn from_dir(dir: &Path) -> (Vec<BehaviorCase>, Vec<String>) {
    let mut cases = Vec::new();
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return (cases, bad) };
    // Sort for a deterministic corpus order (read_dir order is filesystem-dependent).
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        match std::fs::read_to_string(&path).ok().and_then(|j| load(&j).ok()) {
            Some(mut v) => cases.append(&mut v),
            None => bad.push(name),
        }
    }
    (cases, bad)
}

/// The per-member behavior tally the harness fills (each case's eval result vs its expectation) and the
/// quality report reads. Multiple cases for one member aggregate into pass/total.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BehaviorObs {
    pub pass: usize,
    pub total: usize,
    /// Of the PASSING cases, how many were judged against a recorded browser answer rather than one the
    /// implementer wrote. Non-zero is what lets the behavior axis count as external proof.
    #[serde(default)]
    pub external: usize,
    /// The first failing case's detail, for the worklist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_detail: Option<String>,
}

impl BehaviorObs {
    /// Fold one case's outcome in. `ours` = the eval result (`None` if the expr threw); `threw` = the
    /// thrown error's name (`None` if it returned a value).
    /// Fold one case's outcome in, for a caller that cannot distinguish `undefined` (the old contract).
    pub fn observe(&mut self, case: &BehaviorCase, ours: Option<&Value>, threw: Option<&str>) {
        self.observe_full(case, ours, threw, false);
    }

    /// `ours` = the eval result (`None` if the expr threw OR returned undefined/non-JSON-safe);
    /// `threw` = the thrown error's name; `undef` = the expr returned `undefined`.
    pub fn observe_full(&mut self, case: &BehaviorCase, ours: Option<&Value>, threw: Option<&str>, undef: bool) {
        self.total += 1;
        let ok = match (&case.throws, case.undefined, &case.expected) {
            (Some(want), _, _) => threw == Some(want.as_str()),
            (None, true, _) => threw.is_none() && undef,
            // An expectation of a VALUE is not met by `undefined`, and vice versa -- `null` and
            // `undefined` are different answers and the browser tells them apart.
            (None, false, Some(exp)) => threw.is_none() && !undef && ours.map(|o| value_eq(o, exp)).unwrap_or(false),
            (None, false, None) => threw.is_none(),
        };
        if ok {
            self.pass += 1;
            if case.is_recorded() {
                self.external += 1;
            }
        } else if self.fail_detail.is_none() {
            // What we DID, in the same vocabulary as what was wanted -- this line is the agent's whole
            // worklist entry for the member, so "(no throw)" for an expectation of `undefined` was
            // actively misleading: it named the wrong difference.
            let got = match (ours, threw, undef) {
                (_, Some(t), _) => format!("threw {t}"),
                (_, None, true) => "undefined".to_string(),
                (Some(v), None, false) => v.to_string(),
                (None, None, false) => "(unrepresentable)".to_string(),
            };
            let want = match (&case.throws, case.undefined, &case.expected) {
                (Some(t), _, _) => format!("throws {t}"),
                (None, true, _) => "undefined".to_string(),
                (None, false, Some(v)) => v.to_string(),
                (None, false, None) => "(not to throw)".to_string(),
            };
            self.fail_detail = Some(format!("{} ours={got} want={want}", case.member));
        }
    }
}

/// Numeric-tolerant value equality (JSON `8` vs `8.0`), matching the quality value axis.
fn value_eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn case(member: &str, expr: &str, expected: Option<Value>, throws: Option<&str>) -> BehaviorCase {
        BehaviorCase {
            iface: "URL".into(),
            member: member.into(),
            expr: expr.into(),
            expected,
            throws: throws.map(str::to_string),
            source: None,
            undefined: false,
            awaited: false,
        }
    }

    /// The same case, but with the expectation recorded from a real browser.
    fn recorded(member: &str, expr: &str, expected: Option<Value>, throws: Option<&str>) -> BehaviorCase {
        BehaviorCase { source: Some("brave-151-linux-headless".into()), ..case(member, expr, expected, throws) }
    }

    #[test]
    fn only_a_recorded_source_counts_as_external() {
        // Authored -> self-proof, never external, however confident the author was.
        let mut o = BehaviorObs::default();
        o.observe(&case("protocol", "e", Some(Value::from("https:")), None), Some(&Value::from("https:")), None);
        assert_eq!((o.pass, o.external), (1, 0));

        // Recorded from a browser -> external.
        let mut o = BehaviorObs::default();
        o.observe(&recorded("protocol", "e", Some(Value::from("https:")), None), Some(&Value::from("https:")), None);
        assert_eq!((o.pass, o.external), (1, 1));

        // A made-up provenance string is not evidence.
        let mut c = case("protocol", "e", Some(Value::from("https:")), None);
        c.source = Some("i-checked-the-spec".into());
        let mut o = BehaviorObs::default();
        o.observe(&c, Some(&Value::from("https:")), None);
        assert_eq!((o.pass, o.external), (1, 0));

        // A FAILING recorded case is not external proof of anything.
        let mut o = BehaviorObs::default();
        o.observe(&recorded("protocol", "e", Some(Value::from("https:")), None), Some(&Value::from("http:")), None);
        assert_eq!((o.pass, o.external), (0, 0));
    }

    #[test]
    fn value_case_passes_when_output_matches() {
        let c = case("protocol", "new URL('https://a.b/').protocol", Some(json!("https:")), None);
        let mut obs = BehaviorObs::default();
        obs.observe(&c, Some(&json!("https:")), None);
        assert_eq!(obs, BehaviorObs { pass: 1, total: 1, external: 0, fail_detail: None });
    }

    #[test]
    fn value_case_fails_and_records_detail_on_mismatch() {
        let c = case("protocol", "new URL('https://a.b/').protocol", Some(json!("https:")), None);
        let mut obs = BehaviorObs::default();
        obs.observe(&c, Some(&json!("http:")), None);
        assert_eq!(obs.pass, 0);
        assert_eq!(obs.total, 1);
        assert!(obs.fail_detail.as_deref().unwrap().contains("want"));
    }

    #[test]
    fn throws_case_passes_only_on_the_right_error() {
        let c = case("ctor", "new URL('not a url')", None, Some("TypeError"));
        let mut ok = BehaviorObs::default();
        ok.observe(&c, None, Some("TypeError"));
        assert_eq!(ok.pass, 1);
        let mut bad = BehaviorObs::default();
        bad.observe(&c, None, Some("SyntaxError")); // threw, but the wrong error
        assert_eq!(bad.pass, 0);
        let mut none = BehaviorObs::default();
        none.observe(&c, Some(&json!("x")), None); // did not throw at all
        assert_eq!(none.pass, 0);
    }

    /// A PRESENT `"expected": null` survives deserialization as an expectation, and an ABSENT key
    /// still means "no expectation". This is the whole defect: serde's `Option` deserializer maps a
    /// JSON `null` to `None`, so both used to arrive as `None` and 990 recorded cases silently
    /// downgraded to "does not throw".
    #[test]
    fn present_null_and_absent_expected_are_different_cases() {
        let present =
            load(r#"[{"iface":"X","member":"m","expr":"e","expected":null}]"#).unwrap().pop().unwrap();
        assert_eq!(present.expected, Some(Value::Null), "a present `null` is an expectation of null");

        let absent = load(r#"[{"iface":"X","member":"m","expr":"e"}]"#).unwrap().pop().unwrap();
        assert_eq!(absent.expected, None, "an absent key is still no expectation");

        // The distinction has to survive `throws`/`undefined` siblings too.
        let with_undef =
            load(r#"[{"iface":"X","member":"m","expr":"e","expected":null,"undefined":false}]"#)
                .unwrap()
                .pop()
                .unwrap();
        assert_eq!(with_undef.expected, Some(Value::Null));
    }

    /// DONE WHEN #1: a `"expected": null` case asserts the member RETURNS `null` — it passes on `null`
    /// and fails on `undefined`, `0`, `""`, `false`, a throw, and an unrepresentable value.
    #[test]
    fn a_null_expectation_is_met_only_by_null() {
        let c = load(r#"[{"iface":"X","member":"m","expr":"e","expected":null}]"#).unwrap().pop().unwrap();

        let mut ok = BehaviorObs::default();
        ok.observe_full(&c, Some(&Value::Null), None, false);
        assert_eq!((ok.pass, ok.total), (1, 1), "null is the answer the case wants");

        // `undefined` is a DIFFERENT answer; the browser tells them apart and so must the grader.
        let mut undef = BehaviorObs::default();
        undef.observe_full(&c, None, None, true);
        assert_eq!(undef.pass, 0, "undefined must not satisfy an expectation of null");
        assert!(undef.fail_detail.as_deref().unwrap().contains("ours=undefined want=null"));

        for wrong in [json!(0), json!(""), json!(false), json!([]), json!("null")] {
            let mut o = BehaviorObs::default();
            o.observe_full(&c, Some(&wrong), None, false);
            assert_eq!(o.pass, 0, "{wrong} must not satisfy an expectation of null");
        }

        let mut threw = BehaviorObs::default();
        threw.observe_full(&c, None, Some("TypeError"), false);
        assert_eq!(threw.pass, 0, "a throw must not satisfy an expectation of null");

        // Unrepresentable (an object/too-long string the transport dropped) is not null either.
        let mut unrep = BehaviorObs::default();
        unrep.observe_full(&c, None, None, false);
        assert_eq!(unrep.pass, 0, "an unrepresentable value must not satisfy an expectation of null");
    }

    /// The other half of DONE WHEN #1: a case with NO `expected` key keeps its old, weaker meaning.
    #[test]
    fn an_absent_expectation_still_only_asserts_no_throw() {
        let c = load(r#"[{"iface":"X","member":"m","expr":"e"}]"#).unwrap().pop().unwrap();
        for (ours, undef) in
            [(Some(json!(0)), false), (Some(json!("")), false), (Some(Value::Null), false), (None, true)]
        {
            let mut o = BehaviorObs::default();
            o.observe_full(&c, ours.as_ref(), None, undef);
            assert_eq!(o.pass, 1, "no `expected` key ⇒ any non-throwing outcome passes");
        }
        let mut threw = BehaviorObs::default();
        threw.observe_full(&c, None, Some("TypeError"), false);
        assert_eq!(threw.pass, 0);
    }

    /// A recorded `"expected": null` is external evidence like any other recorded answer — the fix must
    /// not quietly demote 983 recorded nulls out of the `independent` count while it repairs them.
    #[test]
    fn a_recorded_null_expectation_still_counts_as_external() {
        let c = load(
            r#"[{"iface":"X","member":"m","expr":"e","expected":null,"source":"brave-151-linux-headless"}]"#,
        )
        .unwrap()
        .pop()
        .unwrap();
        let mut o = BehaviorObs::default();
        o.observe_full(&c, Some(&Value::Null), None, false);
        assert_eq!((o.pass, o.external), (1, 1));
    }

    #[test]
    fn numeric_tolerance_and_load() {
        let c = case("x", "1+1", Some(json!(2)), None);
        let mut obs = BehaviorObs::default();
        obs.observe(&c, Some(&json!(2.0)), None); // 2 == 2.0
        assert_eq!(obs.pass, 1);

        let corpus = load(r#"[{"iface":"URL","member":"protocol","expr":"e","expected":"https:"}]"#).unwrap();
        assert_eq!(corpus.len(), 1);
        assert_eq!(corpus[0].member, "protocol");
    }

    /// The `await` flag is read off disk, and its ABSENCE means synchronous.
    ///
    /// The second half is the one that matters: ~10,000 recorded cases have no such key, and if the
    /// default ever became `true` every one of them would start being awaited -- which for the ~137
    /// promise-returning members would silently replace `"[object Promise]"` with a settled value
    /// while the question on disk looked untouched.
    #[test]
    fn the_await_flag_is_read_and_defaults_to_synchronous() {
        let c = load(r#"[{"iface":"StorageManager","member":"estimate","expr":"e","await":true,"expected":1}]"#)
            .unwrap();
        assert!(c[0].awaited);

        let c = load(r#"[{"iface":"URL","member":"protocol","expr":"e","expected":"https:"}]"#).unwrap();
        assert!(!c[0].awaited, "a case with no await key is synchronous");

        let c = load(r#"[{"iface":"URL","member":"protocol","expr":"e","await":false,"expected":"x"}]"#).unwrap();
        assert!(!c[0].awaited);
    }
}
