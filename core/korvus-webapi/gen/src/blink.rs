//! Blink source cross-reference — "which Chromium file implements this interface?"
//!
//! WebIDL declares that `URLSearchParams` has a `sort()` returning `undefined`. It does not say what
//! `sort()` sorts, or how it orders equal keys. That semantic half lives in prose (WHATWG) and in the
//! engines. For Korvus the engine is the better reference of the two: the goal is to be
//! indistinguishable from Chrome, so Chrome's actual behavior — quirks included — is the target, and
//! prose the spec authors wrote is only a proxy for it.
//!
//! Blink names files after the interface: every JS-visible interface has an `<name>.idl` with sibling
//! `<name>.cc` / `<name>.h`. Rather than guess the path from the interface name (Blink has enough
//! exceptions that guessing misses), this indexes the `.idl` files and reads the declared interface
//! name out of each, so the mapping comes from Blink's own declarations.
//!
//! The checkout is a git-ignored sparse clone; `KORVUS_BLINK_ROOT` points at it:
//!
//! ```text
//! git clone --depth=1 --filter=blob:none --no-checkout \
//!     https://chromium.googlesource.com/chromium/src reference/chromium
//! cd reference/chromium && git sparse-checkout set third_party/blink/renderer && git checkout main
//! ```
//!
//! **Reference, not source.** These paths are handed to an implementer to read for SEMANTICS. Blink is
//! mixed BSD-3 / LGPL-2.1 (the core DOM files carry the KHTML/WebKit inheritance), and a line-by-line
//! translation would make Korvus a derivative work of it. Nothing here copies Blink; it only says
//! where to look.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where an interface is implemented in Blink.
#[derive(Debug, Clone, PartialEq)]
pub struct BlinkRef {
    /// Path to the `.idl`, relative to the Blink root.
    pub idl: String,
    /// Sibling implementation, when present (`element.idl` -> `element.cc`).
    pub cc: Option<String>,
    /// Sibling header, when present.
    pub h: Option<String>,
}

impl BlinkRef {
    /// The files worth reading, implementation first — the order to put in a prompt.
    pub fn paths(&self) -> Vec<&str> {
        let mut v = Vec::new();
        if let Some(cc) = &self.cc {
            v.push(cc.as_str());
        }
        if let Some(h) = &self.h {
            v.push(h.as_str());
        }
        v.push(self.idl.as_str());
        v
    }
}

/// The Blink checkout root from `KORVUS_BLINK_ROOT`, if it is set and exists.
pub fn root_from_env() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("KORVUS_BLINK_ROOT").ok()?);
    p.is_dir().then_some(p)
}

/// Index every interface Blink declares, by walking `.idl` files under `root` and reading the
/// declared name out of each. A `partial interface` never overwrites the base declaration, so
/// `Element` resolves to `core/dom/element.idl`, not to whichever partial was walked last.
pub fn index(root: &Path) -> BTreeMap<String, BlinkRef> {
    let mut out: BTreeMap<String, BlinkRef> = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Generated output and test data are not the implementation.
                let name = e.file_name().to_string_lossy().to_string();
                if !matches!(name.as_str(), "build" | "testing" | ".git") {
                    stack.push(p);
                }
                continue;
            }
            if p.extension().and_then(|x| x.to_str()) != Some("idl") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&p) else { continue };
            for (name, is_partial) in declared_interfaces(&src) {
                let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                let dir_rel = rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
                let sibling = |ext: &str| {
                    let cand = p.with_file_name(format!("{stem}.{ext}"));
                    cand.is_file().then(|| {
                        if dir_rel.is_empty() {
                            format!("{stem}.{ext}")
                        } else {
                            format!("{dir_rel}/{stem}.{ext}")
                        }
                    })
                };
                let entry = BlinkRef { idl: rel, cc: sibling("cc"), h: sibling("h") };
                match out.get(&name) {
                    // A base declaration always wins over a partial.
                    Some(_) if is_partial => {}
                    _ => {
                        out.insert(name, entry);
                    }
                }
            }
        }
    }
    out
}

/// Interface names declared in one `.idl`, each flagged as a `partial` declaration or not.
///
/// Blink does not write `interface X` at the start of a line. Its formatter puts the extended-attribute
/// block on its own lines and closes it on the declaration line:
///
/// ```text
/// [
///     Exposed=(Window,Worker,ShadowRealm)
/// ] interface URLSearchParams {
/// ```
///
/// so the keyword is scanned for anywhere on the line (after comments are stripped) rather than
/// anchored to the start. Comments are stripped first because spec links contain the word —
/// `// https://dom.spec.whatwg.org/#interface-element` must not register an interface named `element`.
fn declared_interfaces(src: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        if code.is_empty() {
            continue;
        }
        let Some(at) = find_keyword(code, "interface") else { continue };
        let is_partial = code[..at].trim_end().ends_with("partial");
        let rest = code[at + "interface".len()..].trim_start();
        // `interface mixin Foo` — the mixin body, still worth pointing at.
        let rest = match rest.strip_prefix("mixin") {
            Some(r) if r.starts_with(|c: char| c.is_whitespace()) => r.trim_start(),
            _ => rest,
        };
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if name.is_empty() {
            continue;
        }
        // A declaration continues into a body or a base: `interface X {` / `interface X : Y {`.
        // This rejects prose and forward references that merely mention the word.
        let tail = rest[name.len()..].trim_start();
        if !(tail.starts_with('{') || tail.starts_with(':') || tail.is_empty()) {
            continue;
        }
        out.push((name, is_partial));
    }
    out
}

/// Byte offset of `kw` in `hay` as a standalone word (not a substring of a longer identifier).
fn find_keyword(hay: &str, kw: &str) -> Option<usize> {
    let b = hay.as_bytes();
    let mut from = 0usize;
    while let Some(i) = hay[from..].find(kw) {
        let a = from + i;
        let e = a + kw.len();
        let before_ok = a == 0 || !b[a - 1].is_ascii_alphanumeric() && b[a - 1] != b'_';
        let after_ok = e >= b.len() || !b[e].is_ascii_alphanumeric() && b[e] != b'_';
        if before_ok && after_ok {
            return Some(a);
        }
        from = a + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Blink's actual formatting: the extended-attribute block closes on the declaration line, and
    // spec URLs in comments contain the word "interface".
    #[test]
    fn declared_interfaces_handles_blinks_bracket_prefixed_form() {
        let src = "\
// https://url.spec.whatwg.org/#interface-urlsearchparams\n\
[\n\
    Exposed=(Window,Worker,ShadowRealm) // TODO: should be Exposed=*\n\
] interface URLSearchParams {\n\
    void sort();\n\
};\n\
] interface Element : Node {\n\
};\n";
        assert_eq!(
            declared_interfaces(src),
            vec![("URLSearchParams".to_string(), false), ("Element".to_string(), false)],
            "the comment must not register an interface named `urlsearchparams`"
        );
    }

    #[test]
    fn declared_interfaces_reads_base_partial_and_mixin() {
        let src = r#"
            // interface NotThisOne {};
            [Exposed=Window]
            interface URLSearchParams {
              undefined sort();
            };
            partial interface Navigator {
              readonly attribute long hardwareConcurrency;
            };
            interface mixin ParentNode {
              readonly attribute unsigned long childElementCount;
            };
        "#;
        let got = declared_interfaces(src);
        assert_eq!(
            got,
            vec![
                ("URLSearchParams".to_string(), false),
                ("Navigator".to_string(), true),
                ("ParentNode".to_string(), false),
            ]
        );
    }

    #[test]
    fn paths_put_the_implementation_first() {
        let r = BlinkRef {
            idl: "core/url/url_search_params.idl".into(),
            cc: Some("core/url/url_search_params.cc".into()),
            h: Some("core/url/url_search_params.h".into()),
        };
        assert_eq!(
            r.paths(),
            vec![
                "core/url/url_search_params.cc",
                "core/url/url_search_params.h",
                "core/url/url_search_params.idl",
            ]
        );
    }

    #[test]
    fn paths_degrade_when_siblings_are_absent() {
        let r = BlinkRef { idl: "core/dom/node.idl".into(), cc: None, h: None };
        assert_eq!(r.paths(), vec!["core/dom/node.idl"]);
    }
}
