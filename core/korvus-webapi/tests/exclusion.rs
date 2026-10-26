//! Table XOR every other layer -- the exclusivity ratchet, DERIVED from the tables themselves.
//!
//! An interface defined by an `interface!` table is defined in EXACTLY ONE place. If it is also emitted
//! by a generator (a row in the generated registry, a reflected attribute, an SVG or event-proto row) or
//! minted by the hand-written spine, two layers touch one prototype and "already there, so skip" decides
//! the winner -- precisely the drift the one-table rule forbids.
//!
//! Until 2026-09-09 this test held a hand-written `MIGRATED` list that had to "stay in lockstep" with
//! three more copies inside the generators, and it grepped `generated_shells.js` for `"n":"X"` keys that
//! file had stopped carrying -- the window half of the ratchet was vacuously true for a week. Nothing
//! here is a list any more: the names are READ FROM THE SOURCE TREE (every `interface! { "Name"` under
//! `core/*/src`, plus the aliases a table declares), and the generators read the same inventory through
//! `tools/sync/table_inventory.py`. A generator that forgets to skip a table-defined name fails this test;
//! a table whose name is spelled differently in the two scanners fails `the_two_scanners_agree`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use webapi::install::generated_registry::REGISTRY;
use webapi::install::Exposed;

const WEBAPI: &str = env!("CARGO_MANIFEST_DIR");

/// The two crates whose `src/` may hold a table. `macros/` and `tests/` are deliberately outside: the
/// macro's own docs spell the grammar and `tests/interface_macro.rs` defines throwaway tables.
const SRC_DIRS: [&str; 2] = ["core/korvus-webapi/src", "core/korvus-runtime/src"];

fn repo_root() -> PathBuf {
    Path::new(WEBAPI).join("..").join("..").canonicalize().expect("repo root")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Every `interface! { "Name" ... {` in `src`, with the aliases its header declares. Mirrors
/// `tools/sync/table_inventory.py` exactly: after `interface! {`, comment lines may precede the quoted
/// name (a `// interface-alias: X` line declares an alias); `alias "X"` may appear in the header
/// between the name and the `{` that opens the member rows.
fn scan(src: &str, found: &mut Vec<String>) {
    let mut from = 0;
    while let Some(i) = src[from..].find("interface!") {
        let at = from + i + "interface!".len();
        let Some(bo) = src[at..].find('{') else { break };
        let mut pos = at + bo + 1;
        let bytes = src.as_bytes();
        // Skip whitespace and `//` comment lines, harvesting `interface-alias:` markers.
        loop {
            while pos < src.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if src[pos..].starts_with("//") {
                let eol = src[pos..].find('\n').map(|e| pos + e).unwrap_or(src.len());
                let line = src[pos + 2..eol].trim();
                if let Some(alias) = line.strip_prefix("interface-alias:") {
                    // The identifier only -- the rest of the line is prose (mirrors the Python `[A-Za-z0-9_]+`).
                    let ident: String = alias.trim().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                    if !ident.is_empty() {
                        found.push(ident);
                    }
                }
                pos = eol;
                continue;
            }
            break;
        }
        if !src[pos..].starts_with('"') {
            from = pos.max(at);
            continue;
        }
        let Some(q) = src[pos + 1..].find('"') else { break };
        let name_end = pos + 1 + q;
        found.push(src[pos + 1..name_end].to_string());
        // The header runs to the `{` that opens the rows; `alias "X"` may appear in it.
        let Some(hb) = src[name_end..].find('{') else { break };
        let header = &src[name_end..name_end + hb];
        let mut h = 0;
        while let Some(a) = header[h..].find("alias") {
            let after = h + a + "alias".len();
            let Some(q1) = header[after..].find('"') else { break };
            let s0 = after + q1 + 1;
            let Some(q2) = header[s0..].find('"') else { break };
            found.push(header[s0..s0 + q2].to_string());
            h = s0 + q2;
        }
        from = name_end + hb;
    }
}

/// The inventory as (name -> number of definitions).
fn inventory() -> BTreeMap<String, usize> {
    let root = repo_root();
    let mut files = Vec::new();
    for d in SRC_DIRS {
        rs_files(&root.join(d), &mut files);
    }
    files.sort();
    let mut found = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        scan(&src, &mut found);
    }
    let mut counts = BTreeMap::new();
    for n in found {
        *counts.entry(n).or_insert(0) += 1;
    }
    counts
}

#[test]
fn a_table_interface_is_defined_nowhere_else() {
    // (No `repo_root()` binding: this test stopped reading FILES at W9.1 — see the note below about
    // the two file scans whose haystacks had silently gone missing. It reads the `REGISTRY` constant.)
    let inv = inventory();
    // Sanity: the scan finds the tables that exist (a broken scanner would make this test vacuous).
    for known in ["DOMRect", "DOMRectReadOnly", "DOMPoint", "DOMPointReadOnly", "HTMLImageElement", "Navigator", "WorkerNavigator"] {
        assert!(inv.contains_key(known), "the inventory scan did not find the {known} table -- scanner broken?");
    }

    let mut offences = Vec::new();
    for (name, count) in &inv {
        if *count > 1 {
            offences.push(format!("{name}: defined by {count} tables (one interface = one table)"));
        }
    }

    let window: BTreeSet<&str> = REGISTRY.iter().filter(|d| d.exposed.covers(Exposed::Window)).map(|d| d.name).collect();
    let worker: BTreeSet<&str> = REGISTRY.iter().filter(|d| d.exposed.covers(Exposed::Worker)).map(|d| d.name).collect();
    // THE ONE GENERATED REGISTRY IS THE ONLY OTHER DEFINITION LAYER LEFT (W6 merged the worker's into
    // it; the realm is a column now), and it is a Rust CONSTANT this file imports: deleting it is a
    // compile error, so neither check can go vacuous.
    //
    // Until W9.1 this test also grepped two FILES, through a `read()` that swallowed a missing path
    // with `unwrap_or_default()` -- so a deleted layer became an empty haystack and every `contains`
    // against it passed. Both files were already gone:
    //
    //   * `core/events/generated_event_protos.rs` -- deleted by **W4.b** (`981a2133`, the event family
    //     became eighteen `interface!` tables). That half had been asserting NOTHING ever since;
    //   * `core/dom/generated_svg.rs` -- deleted by W9.1 (its `SVG_FACTORY` was a second copy of
    //     `korvus_dom::tags::SVG_TAGS`), which is what exposed the pattern.
    //
    // Neither is replaced by a new file scan. A layer that is a file is a layer this test can be
    // lied to about; a layer that is a `&[InterfaceDef]` is not.
    //
    // (dom/07-spine.js and dom/08a-tag-protos.js, the two JS layers that used to mint and map the
    // spine, were deleted by W5.0c; the spine is the tables and `fill_dom_protos`.)
    //
    // The TAG tables are NOT an interface-definition layer and are deliberately not scanned here --
    // they NAME interfaces for `fill_dom_protos` to look up on the global, so `HTMLImageElement`
    // appearing in both `HTML_TAGS` and its own `interface!` table is the design, not a duplicate.
    // `core/korvus-runtime/tests/dom_spine_live.rs` is what holds them: it asserts
    // `createElement(tag)` / `createElementNS(svg, tag)` for EVERY row of both tables.

    for name in inv.keys() {
        let n = name.as_str();
        if window.contains(n) {
            offences.push(format!("{n}: still a row in the generated WINDOW registry (gen_shells.py must skip the inventory)"));
        }
        if worker.contains(n) {
            offences.push(format!("{n}: still a row in the generated WORKER registry (gen_worker_shells.py must skip the inventory)"));
        }
    }
    assert!(
        offences.is_empty(),
        "a table-defined interface is ALSO defined by another layer:\n  {}",
        offences.join("\n  ")
    );
}

/// The Python scanner the generators use and this Rust scanner must read the tree identically --
/// otherwise a generator could skip a name this test does not know, or vice versa.
#[test]
fn the_two_scanners_agree() {
    let root = repo_root();
    let out = std::process::Command::new("python3")
        .arg(root.join("tools/sync/table_inventory.py"))
        .output()
        .expect("python3 is required (the gate and the generators use it)");
    assert!(out.status.success(), "table_inventory.py failed: {}", String::from_utf8_lossy(&out.stderr));
    let py: BTreeSet<String> = String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let rs: BTreeSet<String> = inventory().into_keys().collect();
    assert_eq!(rs, py, "tools/sync/table_inventory.py and tests/exclusion.rs disagree about which interfaces have tables");
}

/// The SELECTOR half of the one-table rule (docs/ONE-TABLE-ARCHITECTURE.md D3/D4):
/// the tree may DEFINE an interface this bundle does not EXPOSE, and when it does, NOTHING installs it.
///
/// `HTMLPortalElement` and `HTMLModelElement` were `.js` files deliberately left out of `dom.js`'s
/// import list -- a decision expressed by an ABSENCE, which reads exactly like a forgotten import and
/// was silent for one of the two until 2026-09-09. Each is a table now, listed in
/// `webapi::UNEXPOSED_BY_THIS_BUNDLE`, and this asserts the two halves of the decision that a later
/// lane could otherwise undo without noticing: the capture records the name in neither realm, and no
/// `install/live/*.rs` mounts the table.
#[test]
fn a_table_this_bundle_does_not_expose_is_installed_by_nothing() {
    let root = repo_root();
    assert!(
        !webapi::UNEXPOSED_BY_THIS_BUNDLE.is_empty(),
        "the unexposed list is empty -- delete this test rather than leave it vacuous"
    );
    // The names have to BE tables, which is what keeps them out of the generated registries.
    let inv = inventory();

    // Every `install/live/*.rs` source, concatenated: naming a table's static there MOUNTS it.
    let mut files = Vec::new();
    rs_files(&root.join("core/korvus-webapi/src/install/live"), &mut files);
    assert!(!files.is_empty(), "no install/live/*.rs found -- has the install surface moved?");
    let live_src: String = files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).collect();

    // The bundle's own global lists -- the denominator (goal §"truth sources" 1).
    let globals = |role: &str| -> BTreeSet<String> {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact(role)).expect("read the bundle surface"))
                .expect("parse the bundle surface");
        v["globals"].as_array().map(|a| a.iter().filter_map(|g| g.as_str().map(str::to_string)).collect()).unwrap_or_default()
    };
    let realms = [("window", globals("surface")), ("worker", globals("worker-surface"))];
    assert!(realms[0].1.contains("HTMLBodyElement"), "the window surface list did not parse -- the shape of surface.json changed");

    let mut offences = Vec::new();
    for def in webapi::UNEXPOSED_BY_THIS_BUNDLE {
        let n = def.name;
        if !inv.contains_key(n) {
            offences.push(format!("{n}: listed as unexposed but has no `interface!` table"));
        }
        // SCREAMING_SNAKE is the static the macro emits from the table's name.
        if live_src.contains(&n.to_uppercase()) {
            offences.push(format!("{n}: named in install/live/ -- this build would EXPOSE a global Brave has not"));
        }
        for (realm, names) in &realms {
            if names.contains(n) {
                offences.push(format!("{n}: the capture DOES record it in the {realm} realm -- install it instead of listing it here"));
            }
        }
    }
    assert!(offences.is_empty(), "the unexposed-table list disagrees with the tree:\n  {}", offences.join("\n  "));
}

/// No deno_core `objects = [..]` class is left in the engine. Such a class is a second way to
/// build a page-visible object -- deno_core makes its template and prototype, outside the builder -- and P6.2
/// rewrote the last of them as the builder's own classes (`#[webapi(own)]`). Every `src/` of `core/` is
/// scanned, the macro's arguments with and without spaces, comment lines aside; `tests/` may keep a
/// throwaway class (`tests/template_spike.rs` probes one).
#[test]
fn no_deno_core_object_class_is_left() {
    let root = repo_root();
    let mut files = Vec::new();
    let Ok(crates) = std::fs::read_dir(root.join("core")) else { panic!("read core/") };
    for c in crates.flatten() {
        rs_files(&c.path().join("src"), &mut files);
    }
    assert!(files.len() > 100, "the scan found {} files -- the tree layout changed", files.len());
    let mut offences = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        for (i, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let squeezed: String = code.chars().filter(|c| !c.is_whitespace()).collect();
            if squeezed.contains("objects=[") {
                offences.push(format!("{}:{}: {}", f.strip_prefix(&root).unwrap_or(f).display(), i + 1, line.trim()));
            }
        }
    }
    assert!(offences.is_empty(), "a deno_core `objects = [..]` class is back -- make it the builder's own class:\n  {}", offences.join("\n  "));
}

/// For INSTANCES: every page-visible object is made from its interface's template, which the
/// builder makes (`install/templates.rs`), with what its instances carry beyond their members -- the
/// interceptors, V8's marks -- set there too (`install/instance_shapes.rs`). A `v8::ObjectTemplate::new` or
/// `v8::FunctionTemplate::new` anywhere else is a second way to make an object. This counts them per file
/// against the list below, which may only shrink: a new one fails, and so does one that went without its
/// line being deleted here. Each line says why its object is not an interface instance, or what it waits for.
#[test]
fn no_hand_template_outside_the_builder() {
    const KNOWN: &[(&str, usize, &str)] = &[
        // the fallback when an interface has no template in this isolate (`mint_wrapper_on`, which reports it
        // as MINT-FALLBACK under KORVUS_TPL_REPORT; measured 0 on the lock and the fixtures)
        ("core/korvus-webapi/src/binding/protos.rs", 1, "anon_wrapper_object"),
        // V8's intrinsics (`%AsyncIteratorPrototype%`, `%IteratorPrototype%`, ...) read off a throwaway holder:
        // the embedder API has no other way to name one, and the holder never reaches a page
        ("core/korvus-webapi/src/core/dom/webidl_iter.rs", 1, "intrinsic reader"),
        ("core/korvus-webapi/src/core/streams/readable_stream.rs", 1, "intrinsic reader"),
        // test-only: `tests/exotic_interceptor.rs` builds a named-getter object from a throwaway table
        ("core/korvus-webapi/src/install/installer.rs", 1, "install_named_getter_object"),
    ];
    let root = repo_root();
    let mut files = Vec::new();
    let Ok(crates) = std::fs::read_dir(root.join("core")) else { panic!("read core/") };
    for c in crates.flatten() {
        rs_files(&c.path().join("src"), &mut files);
    }
    assert!(files.len() > 100, "the scan found {} files -- the tree layout changed", files.len());
    let mut found: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        if rel.ends_with("core/korvus-webapi/src/install/templates.rs") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        for (i, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("ObjectTemplate::new(") || code.contains("FunctionTemplate::new(") || code.contains("FunctionTemplate::builder") {
                found.entry(rel.clone()).or_default().push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    let mut offences = Vec::new();
    for (file, lines) in &found {
        match KNOWN.iter().find(|(f, _, _)| f == file) {
            Some((_, n, _)) if lines.len() == *n => {}
            Some((_, n, why)) => offences.push(format!("{file}: {} hand template(s), {n} known ({why}):\n    {}", lines.len(), lines.join("\n    "))),
            None => offences.push(format!("{file}: a hand template -- make the object from its interface's template:\n    {}", lines.join("\n    "))),
        }
    }
    for (file, n, why) in KNOWN {
        if !found.contains_key(*file) {
            offences.push(format!("{file}: {n} known hand template(s) ({why}) are gone -- delete the line"));
        }
    }
    assert!(offences.is_empty(), "hand-made V8 templates outside the builder:\n  {}", offences.join("\n  "));
}
