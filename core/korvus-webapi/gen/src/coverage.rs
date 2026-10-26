//! Two-dimensional coverage of the webapi behavior scaffold — the `webapi coverage`
//! subcommand. (P2: ported from the since-removed `scripts/webapi_coverage.py`; git history.)
//!
//! STATIC (from source): for each `<Iface>Behavior` trait, the declared members are its
//! `fn … { stub!("Iface::member") }` defaults; the `label -> fn` map is learned directly from the
//! source (never guessed — renames like `toString` -> `to_string_js` would defeat guessing). A
//! member is implemented iff its trait fn is overridden in `impl <Iface>Behavior for <Iface>`.
//! "% implemented" = overridden / declared, computed without running anything.
//!
//! DYNAMIC (optional, from behavior_rt's `$KORVUS_COVERAGE_OUT` json): per member, calls +
//! stub_hits -> what a run TOUCHED, and which touched members are still stubs (the
//! implement-me-next signal).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

/// Runtime coverage for one member (the shape of a `$KORVUS_COVERAGE_OUT` json value).
#[derive(Debug, Default, Clone, Copy, Deserialize)]
pub struct MemberCov {
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub stub_hits: u64,
}

/// Static coverage for one interface: the declared members (`label -> trait fn name`, learned from
/// the trait's `stub!` defaults) and the set of fn names a behavior impl overrides.
#[derive(Debug, Default)]
pub struct InterfaceCoverage {
    pub declared: BTreeMap<String, String>,
    pub implemented: BTreeSet<String>,
}

impl InterfaceCoverage {
    pub fn decl_count(&self) -> usize {
        self.declared.len()
    }

    /// Labels whose trait fn is not overridden — these still throw `NotSupportedError`.
    pub fn missing(&self) -> Vec<&str> {
        self.declared
            .iter()
            .filter(|(_, f)| !self.implemented.contains(*f))
            .map(|(label, _)| label.as_str())
            .collect()
    }

    pub fn impl_count(&self) -> usize {
        self.decl_count() - self.missing().len()
    }
}

/// Static coverage across all scanned interfaces.
#[derive(Debug, Default)]
pub struct Coverage {
    pub interfaces: BTreeMap<String, InterfaceCoverage>,
}

impl Coverage {
    pub fn total_decl(&self) -> usize {
        self.interfaces.values().map(|i| i.decl_count()).sum()
    }

    pub fn total_impl(&self) -> usize {
        self.interfaces.values().map(|i| i.impl_count()).sum()
    }

    /// `(touched, stub_hit_labels)` for one interface given a dynamic-coverage map: how many of the
    /// interface's declared members were called, and which of those are still stubs.
    pub fn dynamic_for(&self, iface: &str, dynamic: &BTreeMap<String, MemberCov>) -> (usize, Vec<&str>) {
        let Some(ic) = self.interfaces.get(iface) else {
            return (0, Vec::new());
        };
        let mut touched = 0;
        let mut stubbed = Vec::new();
        for label in ic.declared.keys() {
            if let Some(c) = dynamic.get(label) {
                if c.calls > 0 {
                    touched += 1;
                }
                if c.stub_hits > 0 {
                    stubbed.push(label.as_str());
                }
            }
        }
        (touched, stubbed)
    }
}

/// Scan one source string for behavior traits + impls, folding into `cov`.
pub fn scan_str(src: &str) -> Coverage {
    let mut cov = Coverage::default();
    scan_into(src, &mut cov);
    cov
}

/// Recursively scan every `.rs` file under `root`.
pub fn scan_dir(root: &Path) -> std::io::Result<Coverage> {
    let mut cov = Coverage::default();
    scan_dir_into(root, &mut cov)?;
    Ok(cov)
}

/// Parse a behavior_rt dynamic-coverage json: `{ "Iface::member": {calls, stub_hits}, ... }`.
pub fn load_dynamic(json: &str) -> serde_json::Result<BTreeMap<String, MemberCov>> {
    serde_json::from_str(json)
}

fn scan_dir_into(dir: &Path, cov: &mut Coverage) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            scan_dir_into(&path, cov)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_into(&std::fs::read_to_string(&path)?, cov);
        }
    }
    Ok(())
}

fn scan_into(raw: &str, cov: &mut Coverage) {
    // Drop `#[cfg(test)] mod … { }` blocks first, so test-only helper traits (e.g. behavior_rt's
    // Demo/Cov dispatch-test traits) are not mistaken for real interfaces.
    let stripped = strip_test_modules(raw);
    let src = stripped.as_str();
    // `<Iface>Behavior` trait -> {label: fn} learned from each `fn … { stub!("Iface::member") }`.
    for m in trait_re().captures_iter(src) {
        let iface = m.get(1).unwrap().as_str().to_string();
        let body = block(src, m.get(0).unwrap().end() - 1);
        let ic = cov.interfaces.entry(iface).or_default();
        for fm in trait_fn_re().captures_iter(body) {
            ic.declared.insert(fm[2].to_string(), fm[1].to_string());
        }
    }
    // `impl <Iface>Behavior for <Iface>` -> the set of overridden fn names.
    for m in impl_re().captures_iter(src) {
        let iface = m.get(1).unwrap().as_str().to_string();
        let body = block(src, m.get(0).unwrap().end() - 1);
        let ic = cov.interfaces.entry(iface).or_default();
        for fm in fn_re().captures_iter(body) {
            ic.implemented.insert(fm[1].to_string());
        }
    }
}

/// Remove every `#[cfg(test)] mod <name> { … }` block. Test modules can define helper `…Behavior`
/// traits (behavior_rt's dispatch tests do) that are not real interfaces; excluding them keeps the
/// coverage dashboard honest.
fn strip_test_modules(src: &str) -> String {
    let re = test_mod_re();
    let mut out = String::new();
    let mut idx = 0;
    while let Some(m) = re.find_at(src, idx) {
        out.push_str(&src[idx..m.start()]);
        idx = block_end(src, m.end() - 1); // skip past the matching `}`
    }
    out.push_str(&src[idx..]);
    out
}

/// Index just past the `}` matching the `{` at/after `brace_pos`.
fn block_end(text: &str, brace_pos: usize) -> usize {
    let bytes = text.as_bytes();
    let open = brace_pos + bytes[brace_pos..].iter().position(|&b| b == b'{').unwrap();
    let mut depth = 1i32;
    let mut i = open + 1;
    while i < bytes.len() && depth != 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    i
}

/// Body between the braces of the block whose opening `{` is at/after `brace_pos` (mirrors
/// `webapi_coverage.py`'s `block`). ASCII-brace depth counting; source is Rust so bytes suffice.
fn block(text: &str, brace_pos: usize) -> &str {
    let bytes = text.as_bytes();
    let open = brace_pos + bytes[brace_pos..].iter().position(|&b| b == b'{').unwrap();
    let mut depth = 1i32;
    let mut i = open + 1;
    let start = i;
    while i < bytes.len() && depth != 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    &text[start..i - 1]
}

// The four patterns from webapi_coverage.py, compiled once. `\w+?` is lazy so `<Iface>Behavior`
// captures the interface name; `trait_fn_re` learns the exact label->fn from each stub default.
fn trait_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"trait\s+(\w+?)Behavior\s*\{").unwrap())
}

fn trait_fn_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?s)fn\s+(\w+)\s*\([^;{]*?\{\s*stub!\("([^"]+)"\)"#).unwrap())
}

fn impl_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"impl\s+(\w+?)Behavior\s+for\s+\w+\s*\{").unwrap())
}

fn fn_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s*fn\s+(\w+)\s*\(").unwrap())
}

fn test_mod_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn geometry_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/core/geometry")
    }

    /// The label->fn map is learned FROM the source, so a renamed fn (trait `to_string_js` behind
    /// label `Widget::toString`) is still credited when that fn is overridden — the false-negative
    /// the Python tool's `TRAIT_FN_RE` fixed. And an un-overridden member is counted missing.
    #[test]
    fn scan_learns_label_to_fn_and_counts_overrides() {
        let src = r#"
            pub(crate) trait WidgetBehavior {
                fn size(&self) -> f64 { stub!("Widget::size") }
                fn to_string_js(&self) -> String { stub!("Widget::toString") }
                fn missing(&self) -> f64 { stub!("Widget::missing") }
            }
            impl WidgetBehavior for Widget {
                fn size(&self) -> f64 { 1.0 }
                fn to_string_js(&self) -> String { String::new() }
            }
        "#;
        let cov = scan_str(src);
        let w = cov.interfaces.get("Widget").expect("Widget scanned");
        assert_eq!(w.decl_count(), 3);
        assert_eq!(w.declared.get("Widget::toString").map(String::as_str), Some("to_string_js"));
        assert_eq!(w.impl_count(), 2);
        assert_eq!(w.missing(), vec!["Widget::missing"]);
    }

    /// The P2 gate: the committed geometry behavior scaffold reads 154/154 = 100%, per interface,
    /// over `core/korvus-webapi/src/core/geometry`.
    ///
    /// The number that matters is the EQUALITY on each line -- declared == implemented, i.e. not one
    /// member of the family is still a `stub!`. The totals themselves move whenever the family
    /// legitimately gains a member, the way OPS_AT_MIGRATION does: 151 -> 154 when DOMMatrix gained
    /// scale3dSelf, rotateFromVectorSelf and rotateAxisAngleSelf. Update them with the members, never
    /// to make a red test green.
    #[test]
    fn geometry_source_reads_154_of_154() {
        let cov = scan_dir(&geometry_dir()).expect("scan geometry source");
        let counts = |iface: &str| {
            let i = cov.interfaces.get(iface).unwrap_or_else(|| panic!("no {iface}"));
            (i.impl_count(), i.decl_count())
        };
        assert_eq!(counts("DOMMatrix"), (60, 60));
        assert_eq!(counts("DOMMatrixReadOnly"), (46, 46));
        assert_eq!(counts("DOMPoint"), (10, 10));
        assert_eq!(counts("DOMPointReadOnly"), (8, 8));
        assert_eq!(counts("DOMQuad"), (9, 9));
        assert_eq!(counts("DOMRect"), (10, 10));
        assert_eq!(counts("DOMRectReadOnly"), (11, 11));
        assert_eq!((cov.total_impl(), cov.total_decl()), (154, 154), "matches webapi_coverage.py");
    }

    /// A `…Behavior` trait inside a `#[cfg(test)] mod` (like behavior_rt's dispatch-test traits) is
    /// NOT a real interface and must be excluded from coverage.
    #[test]
    fn cfg_test_modules_are_excluded() {
        let src = r#"
            pub(crate) trait RealBehavior {
                fn go(&self) -> f64 { stub!("Real::go") }
            }
            impl RealBehavior for Real { fn go(&self) -> f64 { 1.0 } }

            #[cfg(test)]
            mod tests {
                trait DemoBehavior { fn ok(&self) -> f64 { stub!("Demo::ok") } }
                impl DemoBehavior for Demo {}
            }
        "#;
        let cov = scan_str(src);
        assert!(cov.interfaces.contains_key("Real"), "the real interface is scanned");
        assert!(!cov.interfaces.contains_key("Demo"), "the #[cfg(test)] trait is excluded");
    }

    /// Dynamic merge: over an interface's declared members, count those the run called and flag the
    /// called-but-still-stub ones (implement-me-next).
    #[test]
    fn dynamic_merge_marks_touched_and_stub_hits() {
        let src = r#"
            pub(crate) trait WidgetBehavior {
                fn size(&self) -> f64 { stub!("Widget::size") }
                fn missing(&self) -> f64 { stub!("Widget::missing") }
            }
            impl WidgetBehavior for Widget { fn size(&self) -> f64 { 1.0 } }
        "#;
        let cov = scan_str(src);
        let dynamic = load_dynamic(
            r#"{ "Widget::size": {"calls": 3, "stub_hits": 0}, "Widget::missing": {"calls": 2, "stub_hits": 2} }"#,
        )
        .unwrap();
        let (touched, stubbed) = cov.dynamic_for("Widget", &dynamic);
        assert_eq!(touched, 2, "both declared members were called");
        assert_eq!(stubbed, vec!["Widget::missing"], "only the unimplemented member hit a stub");
    }
}
