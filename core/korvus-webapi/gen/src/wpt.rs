//! Per-interface WPT bucketing — the `webapi test` subcommand's bucketer.
//! (P3: ported from the since-removed `scripts/wpt_per_interface.py`; git history.)
//!
//! The runner (core/korvus-runtime/tests/wpt.rs) dumps per-file results to `$KORVUS_WPT_OUT`.
//! This classifies each file by the interface its name exercises (the `Interface-member.html` /
//! `Interface.any.js` convention) and aggregates pass/total per interface, so `webapi test <iface>`
//! (and, later, `webapi status`) can report one interface at a time. Pairs with the runner's
//! `KORVUS_WPT_FILTER`, which selects the same interface up front so the run is fast.

use std::collections::BTreeMap;

use serde::Deserialize;

/// One file's result, as written into the `$KORVUS_WPT_OUT` `"files"` array.
#[derive(Debug, Deserialize)]
pub struct FileResult {
    pub path: String,
    #[serde(default)]
    pub passed: usize,
    #[serde(default)]
    pub total: usize,
    /// Every subtest's name + status (0 = pass), from the runner's dump. Enables (C): attributing
    /// individual assertions to members inside a file, instead of one all-or-nothing file verdict.
    /// Absent in older dumps ⇒ empty ⇒ the file-level path is used unchanged.
    #[serde(default)]
    pub subtests: Vec<SubtestResult>,
    /// The failing subtests, with the harness's message. (E) These are handed to the implementer as
    /// the OPENING worklist: spec requirements upstream asked about that we did not have to think of.
    #[serde(default)]
    pub failures: Vec<FailureResult>,
}

/// One failing subtest.
#[derive(Debug, Deserialize)]
pub struct FailureResult {
    pub name: String,
    #[serde(default)]
    pub message: String,
}

/// One subtest as recorded by testharness.js.
#[derive(Debug, Deserialize)]
pub struct SubtestResult {
    pub name: String,
    #[serde(default)]
    pub status: i64,
}

impl SubtestResult {
    pub fn passed(&self) -> bool {
        self.status == 0
    }
}

#[derive(Debug, Deserialize)]
struct WptDump {
    files: Vec<FileResult>,
}

/// Parse the runner's `$KORVUS_WPT_OUT` json, returning its per-file results.
pub fn parse_dump(json: &str) -> serde_json::Result<Vec<FileResult>> {
    Ok(serde_json::from_str::<WptDump>(json)?.files)
}

/// The interface/member vocabulary a WPT path is resolved against (A).
///
/// WPT names files inconsistently: `dom/nodes/Element-closest.html` follows `Interface-member`,
/// `url/urlsearchparams-has.any.js` lowercases the interface, and `fetch/api/headers/headers-basic.any.js`
/// puts the interface in the DIRECTORY with a second segment (`basic`) that is not a member at all.
/// Matching case-sensitively on the basename head — the original rule — attributed only 198 of 1,129
/// interfaces; resolving case-insensitively, falling back to the directory name, and REQUIRING the
/// member segment to be a real WebIDL member takes it to 383 interfaces and 537 (interface, member)
/// pairs, while dropping non-members like `001`, `basic` and `labels` that used to be carried as
/// member keys matching nothing.
#[derive(Debug, Default)]
pub struct Vocab {
    /// lowercase interface name -> canonical name
    ifaces: std::collections::HashMap<String, String>,
    /// canonical interface -> (lowercase member -> canonical member)
    members: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl Vocab {
    /// Build from the merged `@webref` corpus (`merge::parse_corpus`).
    pub fn from_corpus(corpus: &BTreeMap<String, crate::spec::InterfaceSpec>) -> Self {
        use crate::spec::MemberSpec;
        let mut v = Vocab::default();
        for (name, spec) in corpus {
            v.ifaces.insert(name.to_ascii_lowercase(), name.clone());
            let e = v.members.entry(name.clone()).or_default();
            for m in &spec.members {
                let n = match m {
                    MemberSpec::Attribute { name, .. }
                    | MemberSpec::Operation { name, .. }
                    | MemberSpec::Const { name, .. } => name.as_str(),
                    // A constructor's file segment is spelled `constructor` by convention
                    // (`urlsearchparams-constructor.any.js`, `File-constructor.any.js`).
                    MemberSpec::Constructor { .. } => "constructor",
                    MemberSpec::Special { .. } => continue,
                };
                e.insert(n.to_ascii_lowercase(), n.to_string());
            }
        }
        v
    }

    /// Canonical interface name for a path token, ignoring ASCII case.
    pub fn iface(&self, token: &str) -> Option<&str> {
        self.ifaces.get(&token.to_ascii_lowercase()).map(String::as_str)
    }

    /// Canonical member name for a token on `iface`, ignoring ASCII case. `None` when the token is
    /// not a real member of that interface — which is what rejects `001` / `basic` / `labels`.
    pub fn member(&self, iface: &str, token: &str) -> Option<&str> {
        self.members.get(iface)?.get(&token.to_ascii_lowercase()).map(String::as_str)
    }
}

/// Strip the stacked WPT extensions (`.any.js`, `.window.html`, `.tentative.html`, …) off a basename.
fn strip_exts(mut base: &str) -> &str {
    const EXTS: [&str; 8] = ["html", "xhtml", "any", "window", "sub", "tentative", "js", "json"];
    loop {
        let mut changed = false;
        for ext in EXTS {
            if let Some(s) = base.strip_suffix(ext).and_then(|s| s.strip_suffix('.')) {
                base = s;
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    base
}

/// The interface a WPT file exercises, by the `Interface-member.html` naming convention: the
/// basename segment before the first '-', with stacked extensions stripped. Mirrors
/// the removed `wpt_per_interface.py`'s `basename_iface` and the runner's own `basename_iface`.
pub fn basename_iface(path: &str) -> String {
    let base = strip_exts(path.rsplit('/').next().unwrap_or(path));
    base.split('-').next().unwrap_or(base).split('.').next().unwrap_or(base).to_string()
}

/// The member a WPT file exercises, by the `Interface-member[-variant].ext` convention: the SECOND
/// '-'-segment of the basename. `None` for whole-interface files (`Interface.any.js`). A numeric
/// variant ("001") is returned as-is and simply matches no real WebIDL member downstream.
pub fn member_of(path: &str) -> Option<String> {
    let base = strip_exts(path.rsplit('/').next().unwrap_or(path));
    let mut segs = base.split('-');
    segs.next()?; // interface head
    let member = segs.next()?;
    let member = member.split('.').next().unwrap_or(member);
    (!member.is_empty()).then(|| member.to_string())
}

/// Per-(interface, member) WPT buckets: like [`bucket`] but split by the member segment, so the
/// quality report can grade each member's spec conformance individually. Whole-interface files (no
/// member segment) are omitted here (they still count in the interface-wide [`bucket`]).
pub fn bucket_by_member(files: &[FileResult]) -> BTreeMap<String, BTreeMap<String, Bucket>> {
    bucket_by_member_with(files, None)
}

/// [`bucket_by_member`], vocabulary-aware (A) and subtest-granular (C).
///
/// (A) The member segment must be a REAL WebIDL member of the resolved interface. Without that check
/// `DOMRect-001.html` contributed a bucket keyed `001` and `headers-basic.any.js` one keyed `basic` —
/// keys no member ever matches, so those files' results were silently unusable.
///
/// (C) When the dump carries subtests, a member's tally counts ASSERTIONS, not files. The old rule
/// summed whole files and the caller then demanded `passed >= total`, so one stray failure in a
/// 20-subtest file erased the other 19 — Node's corpus runs 86/92 overall yet handed out exactly one
/// verified member. Subtests whose name mentions ANOTHER member of the same interface are attributed
/// to that member instead, which is what lets a general file (`Node-properties.html`) credit the
/// members it actually exercises.
pub fn bucket_by_member_with(
    files: &[FileResult],
    vocab: Option<&Vocab>,
) -> BTreeMap<String, BTreeMap<String, Bucket>> {
    let mut out: BTreeMap<String, BTreeMap<String, Bucket>> = BTreeMap::new();
    for f in files {
        let iface = classify_with(&f.path, vocab);
        // The file's own member segment, validated against the corpus when we have one.
        let file_member = member_of(&f.path).and_then(|m| match vocab {
            Some(v) => v.member(&iface, &m).map(String::from),
            None => Some(m),
        });

        if f.subtests.is_empty() {
            // Older dump (no subtests): fall back to whole-file tallies.
            let Some(member) = file_member else { continue };
            let b = out.entry(iface).or_default().entry(member).or_default();
            b.passed += f.passed;
            b.total += f.total;
            b.files += 1;
            b.paths.insert(f.path.clone());
            b.rules.insert(classify_rule(&f.path, vocab));
            continue;
        }

        for st in &f.subtests {
            // Prefer a member named in the subtest itself; else the file's member segment.
            let member = vocab
                .and_then(|v| member_named_in(v, &iface, &st.name))
                .map(String::from)
                .or_else(|| file_member.clone());
            let Some(member) = member else { continue };
            let b = out.entry(iface.clone()).or_default().entry(member).or_default();
            b.total += 1;
            if st.passed() {
                b.passed += 1;
            }
            b.files = b.files.max(1);
            b.paths.insert(f.path.clone());
            b.rules.insert(classify_rule(&f.path, vocab));
        }
    }
    out
}

/// The member of `iface` a subtest name mentions, if exactly one does. Requires a word-ish boundary
/// so `has` does not match inside "hashchange", and ignores very short names (`a`, `x`) that would
/// match everywhere. Ambiguous names (two members mentioned) return `None` — the file segment then
/// decides, rather than crediting an arbitrary one.
fn member_named_in<'v>(vocab: &'v Vocab, iface: &str, subtest: &str) -> Option<&'v str> {
    let members = vocab.members.get(iface)?;
    let hay = subtest.to_ascii_lowercase();
    let bytes = hay.as_bytes();
    let mut hit: Option<&str> = None;
    for (lower, canon) in members {
        if lower.len() < 3 {
            continue;
        }
        let mut found = false;
        let mut from = 0usize;
        while let Some(i) = hay[from..].find(lower.as_str()) {
            let a = from + i;
            let b = a + lower.len();
            let before_ok = a == 0 || !bytes[a - 1].is_ascii_alphanumeric();
            let after_ok = b >= bytes.len() || !bytes[b].is_ascii_alphanumeric();
            if before_ok && after_ok {
                found = true;
                break;
            }
            from = a + 1;
        }
        if found {
            if hit.is_some() {
                return None; // ambiguous
            }
            hit = Some(canon.as_str());
        }
    }
    hit
}

/// The interface a WPT *spec-section directory* names, for paths that do not follow the
/// `Interface-member` basename convention (B).
///
/// `dom/` names its files after the interface, so `dom/nodes/Element-closest.html` resolves on the
/// basename alone. `html/` does not: it mirrors the HTML spec's own section structure. All 177
/// files that test `<input>` sit in `html/semantics/forms/the-input-element/`, and exactly ONE of
/// them is named `HTMLInputElement-*`. Head resolution finds `input`, no vocabulary entry matches
/// it, the directory fallback finds `the-input-element`, no entry matches that either -- so every
/// one of those files was attributed to a bucket no member ever reads, and HTMLInputElement scored
/// `wpt -` with 177 upstream tests sitting on disk.
///
/// Two directory shapes carry an interface:
///
/// ```text
/// the-<tag>-element[s]            -> HTML_TAG_IFACE      the-input-element    -> HTMLInputElement
/// the-<name>-interface | -object  -> <name>, verbatim    the-history-interface -> `history`
/// ```
///
/// The second shape is returned unresolved for the caller to look up case-insensitively, which is
/// also what validates it -- `the-windowproxy-exotic-object` yields a name no corpus carries and is
/// correctly dropped rather than inventing a bucket.
///
/// The NEAREST matching ancestor wins, so a subdirectory (`the-input-element/range/`) still lands
/// on the element that owns it.
fn section_iface(path: &str) -> Option<&str> {
    for seg in path.rsplit('/') {
        let Some(rest) = seg.strip_prefix("the-") else { continue };
        if let Some(tag) = rest.strip_suffix("-elements").or_else(|| rest.strip_suffix("-element")) {
            if let Some(iface) = crate::html_tags::html_iface_for_tag(tag) {
                return Some(iface);
            }
            continue;
        }
        if let Some(name) = rest.strip_suffix("-interface").or_else(|| rest.strip_suffix("-object")) {
            return Some(name);
        }
    }
    None
}

/// The spec-section directory names that resolve to `iface` -- the inverse of [`section_iface`],
/// used to widen the runner's `KORVUS_WPT_FILTER` (which matches on basenames only) so a filtered
/// run actually reaches these files. Candidates, not a promise: the caller matches them against
/// real paths, so a name no directory carries simply selects nothing.
pub fn section_dirs_for(iface: &str) -> Vec<String> {
    let mut out: Vec<String> = crate::html_tags::HTML_TAGS
        .iter()
        .filter(|(_, i)| i.eq_ignore_ascii_case(iface))
        .flat_map(|(t, _)| [format!("the-{t}-element"), format!("the-{t}-elements")])
        .collect();
    let low = iface.to_ascii_lowercase();
    out.push(format!("the-{low}-interface"));
    out.push(format!("the-{low}-object"));
    out
}

/// WebIDL mixins are not standalone interfaces; attribute their files to a representative interface
/// (as the removed `wpt_per_interface.py`'s MIXIN map did) so `ParentNode-*` etc. are not dropped.
fn mixin(head: &str) -> Option<&'static str> {
    match head {
        "ParentNode" | "ChildNode" | "NonDocumentTypeChildNode" | "Slottable" => Some("Element"),
        "NonElementParentNode" | "DocumentOrShadowRoot" => Some("Document"),
        "GlobalEventHandlers" => Some("Element"),
        "WindowEventHandlers" => Some("Window"),
        _ => None,
    }
}

/// The interface bucket for a WPT path: its basename interface, with mixins mapped to a
/// representative interface. (The `DIR_DEFAULT` url/encoding fallback and surface-set validation
/// from the Python tool arrive with the P5 status dashboard, which has the full interface list.)
pub fn classify(path: &str) -> String {
    classify_with(path, None)
}

/// The directory a WPT path sits in (`fetch/api/headers/headers-basic.any.js` -> `headers`).
fn dir_of(path: &str) -> Option<&str> {
    let (dir, _) = path.rsplit_once('/')?;
    Some(dir.rsplit('/').next().unwrap_or(dir))
}

/// [`classify`], resolved against a [`Vocab`] when one is available (A). Resolution order:
///   1. basename head, case-insensitively (`urlsearchparams-has` -> `URLSearchParams`)
///   2. the mixin map (`ParentNode-append` -> `Element`)
///   3. the spec-section directory (`html/semantics/forms/the-input-element/...` -> `HTMLInputElement`)
///   4. the containing directory, case-insensitively (`fetch/api/headers/...` -> `Headers`)
///   5. the raw basename head — the pre-vocabulary behavior, so nothing regresses without a corpus.
/// WHICH RULE ATTRIBUTED A FILE, because they are not equally good evidence.
///
/// `classify_with` is a ranked cascade and the ranks differ in strength by a lot. `basename` is a
/// file NAMED for its interface; `dir` is a file that merely SITS NEAR one. The audit of 2026-09-22
/// measured the weakest rank producing 624 of `Origin.from`'s 754 attributed files -- a whole
/// directory of `referrer-policy/gen/**/origin/*.html` charged to one member.
///
/// Rewriting attribution is a research problem. Recording which rule fired is not, and it turns
/// "this member fails" into "this member fails, on evidence of THIS strength" -- which is the
/// difference between a verdict and a number.
pub const RULE_BASENAME: &str = "basename";
pub const RULE_MIXIN: &str = "mixin";
pub const RULE_SECTION: &str = "section";
pub const RULE_DIR: &str = "dir";
pub const RULE_RAW: &str = "raw";

pub fn classify_rule(path: &str, vocab: Option<&Vocab>) -> &'static str {
    let head = basename_iface(path);
    match vocab {
        Some(v) => {
            if v.iface(&head).is_some() {
                RULE_BASENAME
            } else if mixin(&head).is_some() {
                RULE_MIXIN
            } else if section_iface(path).and_then(|t| v.iface(t)).is_some() {
                RULE_SECTION
            } else if dir_of(path).and_then(|d| v.iface(d)).is_some() {
                RULE_DIR
            } else {
                RULE_RAW
            }
        }
        None => {
            if mixin(&head).is_some() {
                RULE_MIXIN
            } else {
                RULE_RAW
            }
        }
    }
}

pub fn classify_with(path: &str, vocab: Option<&Vocab>) -> String {
    let head = basename_iface(path);
    if let Some(v) = vocab {
        if let Some(canon) = v.iface(&head) {
            return canon.to_string();
        }
        if let Some(m) = mixin(&head) {
            return m.to_string();
        }
        // (B) The HTML tree keys on spec sections, not basenames.
        if let Some(canon) = section_iface(path).and_then(|t| v.iface(t)) {
            return canon.to_string();
        }
        if let Some(canon) = dir_of(path).and_then(|d| v.iface(d)) {
            return canon.to_string();
        }
        return head;
    }
    mixin(&head).map(String::from).unwrap_or(head)
}

/// (E) The failing subtests for `iface`, as `file :: subtest — message` lines, newest-corpus-first.
/// Capped so a badly-failing interface cannot flood the agent's prompt.
pub fn failures_for(files: &[FileResult], iface: &str, vocab: Option<&Vocab>, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    for f in files {
        if !classify_with(&f.path, vocab).eq_ignore_ascii_case(iface) {
            continue;
        }
        let base = f.path.rsplit('/').next().unwrap_or(&f.path);
        for x in &f.failures {
            if out.len() >= cap {
                return out;
            }
            let msg = x.message.trim();
            let msg = if msg.is_empty() { String::new() } else { format!(" — {}", first_line(msg, 160)) };
            out.push(format!("{base} :: {}{}", x.name, msg));
        }
    }
    out
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() <= max {
        line.to_string()
    } else {
        line.chars().take(max).collect::<String>() + "…"
    }
}

/// Aggregated pass/total/file-count for one interface.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Bucket {
    pub passed: usize,
    pub total: usize,
    pub files: usize,
    /// THE EVIDENCE, not a count of it. `files` is how MANY files fed this bucket; this is WHICH.
    ///
    /// Without it a member's `wpt` verdict is unfalsifiable: `tools/status/data/status.json` can say
    /// `CharacterData::insertData` is `fail` while `scripts/wpt-tiers.baseline` records
    /// `dom/nodes/CharacterData-insertData.html` as a floor `pass`, and NO CODE CAN NOTICE, because
    /// one speaks in members and the other in files and nothing joins them. That contradiction was
    /// found by hand on 2026-09-23 and had been silent for the whole goal.
    ///
    /// A verdict carries the evidence that produced it, or it is not a measurement.
    pub paths: std::collections::BTreeSet<String>,
    /// WHICH attribution rules produced this bucket (see `classify_rule`). A bucket built only from
    /// `dir` is far weaker evidence than one built from `basename`, and a consumer that cannot see
    /// the difference treats them alike -- which is how a directory of 624 files was charged to one
    /// member and read as a measurement.
    pub rules: std::collections::BTreeSet<&'static str>,
}

impl Bucket {
    pub fn rate(&self) -> Option<f64> {
        (self.total > 0).then(|| 100.0 * self.passed as f64 / self.total as f64)
    }
}

/// Aggregate per-file results into per-interface buckets.
pub fn bucket(files: &[FileResult]) -> BTreeMap<String, Bucket> {
    bucket_with(files, None)
}

/// [`bucket`], vocabulary-aware (A): the same case-insensitive + directory resolution as
/// [`classify_with`], so `fetch/api/headers/*` lands on `Headers` rather than a `headers` bucket
/// nothing ever looks up.
pub fn bucket_with(files: &[FileResult], vocab: Option<&Vocab>) -> BTreeMap<String, Bucket> {
    let mut out: BTreeMap<String, Bucket> = BTreeMap::new();
    for f in files {
        let b = out.entry(classify_with(&f.path, vocab)).or_default();
        b.passed += f.passed;
        b.total += f.total;
        b.files += 1;
        b.paths.insert(f.path.clone());
        b.rules.insert(classify_rule(&f.path, vocab));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_iface_follows_the_naming_convention() {
        assert_eq!(basename_iface("css/geometry/DOMRect-001.html"), "DOMRect");
        assert_eq!(basename_iface("css/geometry/DOMMatrix-a-f-alias.html"), "DOMMatrix");
        assert_eq!(basename_iface("dom/nodes/Node-childNodes.any.js"), "Node");
        assert_eq!(basename_iface("DOMPointReadOnly-002.html"), "DOMPointReadOnly");
    }

    #[test]
    fn classify_maps_mixins_but_keeps_real_interfaces() {
        assert_eq!(classify("dom/nodes/ParentNode-querySelector-All.html"), "Element");
        assert_eq!(classify("css/geometry/DOMRect-001.html"), "DOMRect");
    }

    /// Bucketing reproduces the DOMRect numbers the runner just measured (26/30 + 28/32 = 54/62).
    /// THE EVIDENCE IS CARRIED, not counted. A bucket that knows only HOW MANY files fed it cannot
    /// be cross-checked against `scripts/wpt-tiers.baseline`, which speaks in file paths -- and that
    /// is exactly how `status.json` could report a member `fail` while the floor recorded its file
    /// as a `pass`, in silence, for the whole goal.
    #[test]
    fn a_bucket_names_the_files_that_produced_it() {
        let files = vec![
            FileResult { path: "css/geometry/DOMRect-001.html".into(), passed: 26, total: 30, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "css/geometry/DOMRect-002.html".into(), passed: 28, total: 32, subtests: Vec::new(), failures: Vec::new() },
        ];
        let b = bucket(&files);
        let got: Vec<&str> = b["DOMRect"].paths.iter().map(String::as_str).collect();
        assert_eq!(got, vec!["css/geometry/DOMRect-001.html", "css/geometry/DOMRect-002.html"],
                   "the bucket must name its evidence, not just count it");
        assert_eq!(b["DOMRect"].paths.len(), b["DOMRect"].files,
                   "one path per file that fed the bucket");
    }

    #[test]
    fn bucket_aggregates_per_interface() {
        let files = vec![
            FileResult { path: "css/geometry/DOMRect-001.html".into(), passed: 26, total: 30, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "css/geometry/DOMRect-002.html".into(), passed: 28, total: 32, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "css/geometry/DOMPoint-001.html".into(), passed: 10, total: 14, subtests: Vec::new(), failures: Vec::new() },
        ];
        let b = bucket(&files);
        assert_eq!((b["DOMRect"].passed, b["DOMRect"].total, b["DOMRect"].files), (54, 62, 2));
        assert_eq!(b["DOMPoint"].files, 1);
        assert!((b["DOMRect"].rate().unwrap() - 87.096).abs() < 0.01);
    }

    #[test]
    fn parse_dump_reads_the_files_array() {
        let json = r#"{ "overall_passed": 26, "files": [ {"path":"css/geometry/DOMRect-001.html","passed":26,"total":30} ] }"#;
        let files = parse_dump(json).expect("parse");
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].passed, files[0].total), (26, 30));
    }

    /// A small corpus for the vocabulary tests, parsed from real WebIDL so the member sets are
    /// produced by the same path production uses.
    fn vocab() -> Vocab {
        let idl = r#"
            interface URLSearchParams {
              constructor();
              undefined append(DOMString name, DOMString value);
              boolean has(DOMString name);
              undefined sort();
            };
            interface Headers {
              undefined append(DOMString name, DOMString value);
              undefined delete(DOMString name);
            };
            interface DOMRect {
              attribute unrestricted double x;
            };
            interface Node {
              attribute DOMString nodeValue;
              Node cloneNode(optional boolean deep);
              boolean contains(Node? other);
            };
            interface HTMLInputElement {
              attribute DOMString value;
              attribute boolean checked;
            };
            interface History {
              undefined back();
            };
        "#;
        let specs = crate::webidl::parse(idl).expect("parse test idl");
        let corpus: BTreeMap<String, crate::spec::InterfaceSpec> =
            specs.into_iter().map(|s| (s.name.clone(), s)).collect();
        Vocab::from_corpus(&corpus)
    }

    // (A) WPT lowercases the interface in several areas; resolution must ignore ASCII case.
    #[test]
    fn classify_resolves_a_lowercased_interface_name() {
        let v = vocab();
        assert_eq!(classify_with("url/urlsearchparams-has.any.js", Some(&v)), "URLSearchParams");
        // Unchanged for names that already match.
        assert_eq!(classify_with("dom/nodes/Node-cloneNode.html", Some(&v)), "Node");
    }

    // (A) `fetch/api/headers/headers-basic.any.js` carries the interface in the DIRECTORY.
    #[test]
    fn classify_falls_back_to_the_directory_name() {
        let v = vocab();
        assert_eq!(classify_with("fetch/api/headers/header-values.any.js", Some(&v)), "Headers");
    }

    // (A) Without a vocabulary nothing changes — the pre-existing behavior is the fallback.
    #[test]
    fn classify_without_a_vocabulary_keeps_the_raw_head() {
        assert_eq!(classify_with("url/urlsearchparams-has.any.js", None), "urlsearchparams");
        assert_eq!(classify_with("dom/nodes/ParentNode-append.html", None), "Element"); // mixin map
    }

    // (A) The member segment must be a REAL member: `001` and `basic` used to become bucket keys.
    #[test]
    fn member_buckets_reject_segments_that_are_not_members() {
        let v = vocab();
        let files = vec![
            FileResult { path: "css/geometry/DOMRect-001.html".into(), passed: 26, total: 30, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "url/urlsearchparams-has.any.js".into(), passed: 4, total: 4, subtests: Vec::new(), failures: Vec::new() },
        ];
        let b = bucket_by_member_with(&files, Some(&v));
        assert!(b.get("DOMRect").is_none(), "`001` is not a DOMRect member and must not bucket");
        assert_eq!((b["URLSearchParams"]["has"].passed, b["URLSearchParams"]["has"].total, b["URLSearchParams"]["has"].files), (4, 4, 1));
    }

    // (C) A member's tally counts ASSERTIONS. One stray failure must not erase the rest of the file.
    #[test]
    fn subtests_are_counted_per_member_not_per_file() {
        let v = vocab();
        let files = vec![FileResult {
            path: "url/urlsearchparams-sort.any.js".into(),
            passed: 2,
            total: 3,
            subtests: vec![
                SubtestResult { name: "sorts basic".into(), status: 0 },
                SubtestResult { name: "sorts unicode".into(), status: 0 },
                SubtestResult { name: "sorts with holes".into(), status: 1 },
            ],
            failures: Vec::new(),
        }];
        let b = bucket_by_member_with(&files, Some(&v));
        assert_eq!((b["URLSearchParams"]["sort"].passed, b["URLSearchParams"]["sort"].total, b["URLSearchParams"]["sort"].files), (2, 3, 1));
    }

    // (C) A subtest that names another member of the same interface credits THAT member, so a general
    // file spreads its assertions over the members it actually exercises.
    #[test]
    fn a_subtest_naming_another_member_is_attributed_to_it() {
        let v = vocab();
        let files = vec![FileResult {
            path: "dom/nodes/Node-cloneNode.html".into(),
            passed: 2,
            total: 2,
            subtests: vec![
                SubtestResult { name: "cloneNode keeps attributes".into(), status: 0 },
                SubtestResult { name: "contains() after cloning".into(), status: 0 },
            ],
            failures: Vec::new(),
        }];
        let b = bucket_by_member_with(&files, Some(&v));
        assert_eq!(b["Node"]["cloneNode"].total, 1);
        assert_eq!(b["Node"]["contains"].total, 1, "the second subtest names `contains`");
    }

    // (C) A subtest naming two members is ambiguous: fall back to the file's own member segment
    // rather than crediting an arbitrary one.
    #[test]
    fn an_ambiguous_subtest_falls_back_to_the_file_member() {
        let v = vocab();
        let files = vec![FileResult {
            path: "dom/nodes/Node-cloneNode.html".into(),
            passed: 1,
            total: 1,
            subtests: vec![SubtestResult { name: "cloneNode then contains".into(), status: 0 }],
            failures: Vec::new(),
        }];
        let b = bucket_by_member_with(&files, Some(&v));
        assert_eq!(b["Node"]["cloneNode"].total, 1);
        assert!(b["Node"].get("contains").is_none());
    }

    // (C) A short member name must not match inside a longer word.
    #[test]
    fn member_matching_respects_word_boundaries() {
        let v = vocab();
        let files = vec![FileResult {
            path: "url/urlsearchparams-append.any.js".into(),
            passed: 1,
            total: 1,
            // "appended" must NOT be read as the member `append`; the file segment decides.
            subtests: vec![SubtestResult { name: "values are appended".into(), status: 0 }],
            failures: Vec::new(),
        }];
        let b = bucket_by_member_with(&files, Some(&v));
        assert_eq!((b["URLSearchParams"]["append"].passed, b["URLSearchParams"]["append"].total, b["URLSearchParams"]["append"].files), (1, 1, 1));
    }

    // (B) The whole point: `html/` keys on spec sections. 177 files test `<input>` and one is named
    // after the interface, so without this every one of them was unattributed.
    #[test]
    fn classify_resolves_a_spec_section_directory() {
        let v = vocab();
        assert_eq!(
            classify_with("html/semantics/forms/the-input-element/input-type-number.html", Some(&v)),
            "HTMLInputElement"
        );
    }

    // (B) The nearest matching ancestor wins, so a variant subdirectory still lands on its element.
    #[test]
    fn classify_resolves_a_nested_spec_section_directory() {
        let v = vocab();
        assert_eq!(
            classify_with("html/semantics/forms/the-input-element/range/foo.html", Some(&v)),
            "HTMLInputElement"
        );
    }

    // (B) The `-interface` shape carries the name directly; the vocabulary canonicalises the case.
    #[test]
    fn classify_resolves_the_interface_directory_shape() {
        let v = vocab();
        assert_eq!(classify_with("html/browsers/history/the-history-interface/001.html", Some(&v)), "History");
    }

    // (B) A section naming something the corpus does not carry must NOT invent a bucket -- it falls
    // through to the old behavior rather than attributing files to an interface nobody grades.
    #[test]
    fn an_unknown_spec_section_does_not_invent_a_bucket() {
        let v = vocab();
        // `the-marquee-element` is a real Blink tag, but HTMLMarqueeElement is not in this vocabulary.
        assert_eq!(classify_with("html/semantics/the-marquee-element/x.html", Some(&v)), "x");
        // Nor does the `-object` shape, whose name is unresolved by design.
        assert_eq!(
            classify_with("html/browsers/windows/the-windowproxy-exotic-object/y.html", Some(&v)),
            "y"
        );
    }

    // (B) The inverse map is what lets a FILTERED run reach those files at all: the runner matches
    // basenames, and no basename under the-input-element/ says HTMLInputElement.
    #[test]
    fn section_dirs_invert_the_tag_map() {
        let d = section_dirs_for("HTMLInputElement");
        assert!(d.contains(&"the-input-element".to_string()));
        // Case-insensitive, and the `-interface` candidates are always offered.
        assert!(section_dirs_for("history").contains(&"the-history-interface".to_string()));
        // An interface with no element spelling still yields only its own candidates, never another's.
        assert!(!section_dirs_for("HTMLDivElement").contains(&"the-input-element".to_string()));
    }


    // Step 1 (per-member WPT): the member is the SECOND '-'-segment of the basename.
    #[test]
    fn member_of_extracts_the_member_segment() {
        assert_eq!(member_of("dom/nodes/Node-childNodes.any.js"), Some("childNodes".to_string()));
        assert_eq!(member_of("dom/nodes/ParentNode-querySelector-All.html"), Some("querySelector".to_string()));
        assert_eq!(member_of("css/geometry/DOMRect-001.html"), Some("001".to_string()));
        assert_eq!(member_of("dom/nodes/Node.any.js"), None);
        assert_eq!(member_of("css/geometry/DOMMatrix-a-f-alias.html"), Some("a".to_string()));
    }

    #[test]
    fn bucket_by_member_groups_per_interface_member() {
        let files = vec![
            FileResult { path: "dom/nodes/Node-childNodes.any.js".into(), passed: 8, total: 10, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "dom/nodes/Node-childNodes-2.html".into(), passed: 2, total: 2, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "dom/nodes/Node-nodeType.html".into(), passed: 5, total: 5, subtests: Vec::new(), failures: Vec::new() },
            FileResult { path: "dom/nodes/Node.any.js".into(), passed: 3, total: 4, subtests: Vec::new(), failures: Vec::new() },
        ];
        let b = bucket_by_member(&files);
        assert_eq!((b["Node"]["childNodes"].passed, b["Node"]["childNodes"].total, b["Node"]["childNodes"].files), (10, 12, 2));
        assert_eq!((b["Node"]["nodeType"].passed, b["Node"]["nodeType"].total, b["Node"]["nodeType"].files), (5, 5, 1));
        // The whole-interface file (no member) is not attributed to any member bucket.
        assert!(!b["Node"].contains_key(""));
    }
}
