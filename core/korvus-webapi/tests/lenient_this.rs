//! `[LegacyLenientThis]` IS DERIVED FROM webref, NOT WRITTEN DOWN.
//!
//! The installer's receiver check (`install::receiver`) rejects a foreign receiver for every row of
//! every table. WebIDL's one exception is `[LegacyLenientThis]`: the member answers `undefined`
//! instead of throwing, for the getter AND the setter. Measured on Chrome 150.0.7871.46
//! (`--headless=new --dump-dom`):
//!
//! ```text
//!   get Document.prototype.onreadystatechange .call({})   returned [object Undefined]
//!   set Document.prototype.onreadystatechange .call({},…) returned [object Undefined]
//!   get HTMLElement.prototype.onmouseenter    .call({})   returned [object Undefined]
//!   get HTMLElement.prototype.onmouseleave    .call({})   returned [object Undefined]
//!   get HTMLElement.prototype.onclick         .call({})   TypeError: Illegal invocation
//! ```
//!
//! The GENERATED registries carry the flag by construction: `tools/sync/gen_shells.py::lenient_this`
//! reads it from the same webref parse that answers nullability, and `scripts/sync.sh --check` proves
//! the output regenerates to itself. A HAND table spells it `lenient` in the row, and nothing else
//! would notice if a table forgot — which is what this test is for. It reads the extended attribute
//! out of `gen/webref/*.idl` and asserts every hand-table row that WebIDL declares lenient says so.
//!
//! It is a SOURCE scan, like `tests/exclusion.rs`'s inventory, because a hand `interface!` table is
//! Rust source and there is no Rust-side registry of them to walk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SRC_DIRS: &[&str] = &["core/korvus-webapi/src", "core/korvus-runtime/src"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every `[LegacyLenientThis]` attribute webref declares: member name -> the block that declares it,
/// and whether that block is an `interface mixin` (which any interface may `includes`) or a plain
/// `interface` (which only it and its subclasses get).
fn webref_lenient() -> BTreeMap<String, (String, bool)> {
    let dir = repo_root().join("core/korvus-webapi/gen/webref");
    let mut out = BTreeMap::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "idl"))
        .collect();
    files.sort();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        // Walk block headers so a hit can be attributed to the interface (or mixin) that declares it.
        let mut owner = String::new();
        let mut is_mixin = false;
        for line in src.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("interface mixin ") {
                owner = rest.split_whitespace().next().unwrap_or("").trim_end_matches('{').to_string();
                is_mixin = true;
            } else if let Some(rest) = t.strip_prefix("interface ") {
                owner = rest.split_whitespace().next().unwrap_or("").trim_end_matches('{').to_string();
                is_mixin = false;
            } else if t.starts_with("partial interface mixin ") {
                owner = t["partial interface mixin ".len()..].split_whitespace().next().unwrap_or("").trim_end_matches('{').to_string();
                is_mixin = true;
            } else if t.starts_with("partial interface ") {
                owner = t["partial interface ".len()..].split_whitespace().next().unwrap_or("").trim_end_matches('{').to_string();
                is_mixin = false;
            }
            if !t.contains("LegacyLenientThis") || !t.contains("attribute") {
                continue;
            }
            // `[LegacyLenientThis] attribute EventHandler onreadystatechange;`
            let after = t.split("attribute").nth(1).unwrap_or("");
            let name: String = after
                .trim_end_matches(';')
                .split_whitespace()
                .next_back()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() && !owner.is_empty() {
                out.insert(name, (owner.clone(), is_mixin));
            }
        }
    }
    assert!(
        !out.is_empty(),
        "no [LegacyLenientThis] attribute found in gen/webref -- the scanner is broken, not the IDL"
    );
    out
}

/// One hand `interface!` table: its name, its declared parent, and its member rows as
/// `(member name, whether the row carries the `lenient` modifier)`.
struct Table {
    name: String,
    parent: Option<String>,
    rows: Vec<(String, bool)>,
}

fn tables() -> Vec<Table> {
    let root = repo_root();
    let mut files = Vec::new();
    for d in SRC_DIRS {
        rs_files(&root.join(d), &mut files);
    }
    files.sort();
    let mut out = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        let mut from = 0;
        while let Some(i) = src[from..].find("interface!") {
            let at = from + i + "interface!".len();
            let Some(bo) = src[at..].find('{') else { break };
            // The header: `"Name" : "Parent" [model, …] {`. The name is the first quoted string after
            // the opening brace (comment lines may precede it); the parent is the second, when the
            // header has a `:` before the `[`.
            let rest = &src[at + bo + 1..];
            let Some(hb) = rest.find('{') else { break };
            let header = &rest[..hb];
            let quoted: Vec<&str> = header.match_indices('"').collect::<Vec<_>>().chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| &header[c[0].0 + 1..c[1].0])
                .collect();
            let Some(name) = quoted.first().map(|s| s.to_string()) else {
                from = at + bo + 1;
                continue;
            };
            let parent = if header.contains(':') { quoted.get(1).map(|s| s.to_string()) } else { None };
            // The rows run to the matching close brace of the member block.
            let body_start = at + bo + 1 + hb + 1;
            let mut depth = 1usize;
            let bytes = src.as_bytes();
            let mut p = body_start;
            while p < src.len() && depth > 0 {
                match bytes[p] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                p += 1;
            }
            let body = &src[body_start..p.min(src.len())];
            let mut rows = Vec::new();
            for line in body.lines() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                let Some(kind_end) = t.find(char::is_whitespace) else { continue };
                if !matches!(&t[..kind_end], "attr" | "handler" | "static_attr") {
                    continue;
                }
                let after = &t[kind_end..];
                let Some(q1) = after.find('"') else { continue };
                let Some(q2) = after[q1 + 1..].find('"') else { continue };
                let member = after[q1 + 1..q1 + 1 + q2].to_string();
                // The modifiers run between the closing quote and the `=`.
                let mods = after[q1 + 1 + q2 + 1..].split('=').next().unwrap_or("");
                rows.push((member, mods.split_whitespace().any(|w| w == "lenient")));
            }
            out.push(Table { name, parent, rows });
            from = p;
        }
    }
    assert!(out.len() > 100, "only {} hand tables found -- the scanner is broken", out.len());
    out
}

#[test]
fn every_hand_table_row_webidl_declares_lenient_says_so() {
    let lenient = webref_lenient();
    let tables = tables();
    let parents: BTreeMap<&str, Option<&str>> =
        tables.iter().map(|t| (t.name.as_str(), t.parent.as_deref())).collect();

    // Does `iface` (or an ancestor the tables themselves declare) reach `owner`?
    let reaches = |iface: &str, owner: &str| -> bool {
        let mut cur = Some(iface);
        let mut hops = 0;
        while let Some(c) = cur {
            if c == owner {
                return true;
            }
            hops += 1;
            if hops > 16 {
                return false;
            }
            cur = parents.get(c).copied().flatten();
        }
        false
    };

    let mut missing = Vec::new();
    let mut wrong = Vec::new();
    let mut marked = 0usize;
    for t in &tables {
        for (member, has) in &t.rows {
            let applies = match lenient.get(member) {
                // A mixin is `includes`d by anything, so any interface carrying the member gets it.
                Some((_, true)) => true,
                // A plain interface gives it only to itself and its subclasses -- which is the whole
                // reason this cannot be a name check: `Document.onreadystatechange` is lenient and
                // `XMLHttpRequest.onreadystatechange` is not.
                Some((owner, false)) => reaches(&t.name, owner),
                None => false,
            };
            match (applies, has) {
                (true, false) => missing.push(format!(
                    "{}::{member}: WebIDL declares it [LegacyLenientThis] (on `{}`) -- the row needs the `lenient` modifier",
                    t.name,
                    lenient[member].0
                )),
                (false, true) => wrong.push(format!(
                    "{}::{member}: the row says `lenient` but no webref declaration gives it [LegacyLenientThis]",
                    t.name
                )),
                (true, true) => marked += 1,
                _ => {}
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // Both halves must be non-empty or the assertions above are vacuous: webref really does declare
    // some, and the tables really do carry some.
    assert_eq!(lenient.len(), 3, "webref's [LegacyLenientThis] set changed: {lenient:?}");
    assert!(marked >= 7, "only {marked} hand-table rows are marked lenient -- the scanner is not seeing the rows");
    eprintln!("[LegacyLenientThis]: {} webref declarations, {marked} hand-table rows marked", lenient.len());
}
