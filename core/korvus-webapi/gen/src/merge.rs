//! Fold the vendored `@webref` WebIDL corpus into one [`InterfaceSpec`] per interface — the merged
//! member list the "① present" axis of `webapi quality` grades against.
//!
//! A real interface's surface is *not* one `interface X {…}` block. It is spread across:
//!   - the base `interface X { … }` (often nearly empty — `interface Navigator {}` is just a comment),
//!   - many `partial interface X { … }` blocks (Navigator has ~37, scattered across dozens of files),
//!   - and `interface mixin M { … }` bodies pulled in by `X includes M;` statements (Navigator has ~19;
//!     e.g. `NavigatorConcurrentHardware` → `hardwareConcurrency`, `NavigatorDeviceMemory` → `deviceMemory`,
//!     `NavigatorUA` → `userAgentData`, each in a different file).
//!
//! [`crate::webidl::parse`] only ever sees the base block; every other kind falls through its `_ => {}`.
//! This module walks the *whole* corpus and handles all four relevant weedle definition kinds —
//! `Interface`, `PartialInterface`, `InterfaceMixin`, `IncludesStatement` — reusing `webidl`'s member
//! lowering so the normalized [`MemberSpec`] form is produced in exactly one place.

use std::collections::BTreeMap;
use regex::Regex;
use std::path::Path;

use crate::spec::{InterfaceSpec, MemberSpec};
use crate::webidl::{attributed_type_to_string, lower_args, lower_members, return_type_to_string};
use weedle::mixin::MixinMember;
use weedle::{Definition, Definitions, Parse};

/// Parse every `.idl` under `dir` and fold, per interface NAME, the base `interface X` + every
/// `partial interface X` + every `interface mixin M` pulled in by an `X includes M;` statement into
/// one [`InterfaceSpec`] carrying the UNION of all their members (deduplicated, first-seen order).
///
/// The caller passes the vendored corpus dir (`{CARGO_MANIFEST_DIR}/webref`). Unparseable files are
/// skipped (weedle PANICS on syntax it can't consume — modern `async iterable`, some ext-attrs, …),
/// mirroring `main.rs`'s `cmd_deps`, so one bad file never sinks the whole corpus.
pub fn parse_corpus(dir: &Path) -> BTreeMap<String, InterfaceSpec> {
    // Collect the .idl files (sorted, for deterministic member order across runs), like `cmd_deps`.
    let mut idl_files: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) == Some("idl") {
                idl_files.push(path);
            }
        }
    }
    idl_files.sort();

    // Accumulate in TWO passes' worth of state during one file walk, because a mixin `M` may be
    // defined in a *different* file than the `X includes M;` that pulls it in (and a base may follow
    // a partial in file order): we cannot resolve `includes` until every mixin body is known.
    let mut result: BTreeMap<String, InterfaceSpec> = BTreeMap::new();
    let mut mixins: BTreeMap<String, Vec<MemberSpec>> = BTreeMap::new();
    let mut includes: Vec<(String, String)> = Vec::new();
    // `[Exposed=...]` per interface, extracted from the raw source (weedle drops ext-attrs into a form
    // that is awkward to walk; a regex over the declaration is robust and simple).
    let mut exposed_map: BTreeMap<String, Vec<String>> = BTreeMap::new();

    // weedle can PANIC on constructs it can't consume, so catch each per-file unwind under a silenced
    // hook (the pattern proven in `main.rs`'s `cmd_deps`). But unlike `cmd_deps` we parse with
    // [`parse_defs_resync`] instead of `weedle::parse`: the whole-file `weedle::parse` asserts the
    // ENTIRE file consumed and thus loses every definition once one is unparseable — fatal here, since
    // `html.idl`'s base `Navigator` + the `NavigatorConcurrentHardware` mixin sit AFTER the first
    // construct weedle rejects. Resync recovers them.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    // Every `interface X` a source DECLARES, so the parse can be checked against it below.
    let mut declared: BTreeMap<String, String> = BTreeMap::new();
    for f in &idl_files {
        let Ok(src) = std::fs::read_to_string(f) else { continue };
        let src = normalize_for_weedle(&src);
        for name in declared_interfaces(&src) {
            declared.entry(name).or_insert_with(|| f.file_name().unwrap_or_default().to_string_lossy().into_owned());
        }
        for (name, globals) in extract_exposed(&src) {
            exposed_map.entry(name).or_insert(globals);
        }
        let defs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse_defs_resync(&src)))
            .unwrap_or_default();
        for def in &defs {
            match def {
                // Base `interface X { … }`: seed/extend X, and record its single inheritance parent.
                Definition::Interface(i) => {
                    let entry = entry_for(&mut result, i.identifier.0);
                    if let Some(inh) = &i.inheritance {
                        entry.inherits = Some(inh.identifier.0.to_string());
                    }
                    extend_unique(&mut entry.members, lower_members(&i.members.body));
                }
                // `partial interface X { … }`: add its members to X (never touches inheritance).
                Definition::PartialInterface(p) => {
                    let entry = entry_for(&mut result, p.identifier.0);
                    extend_unique(&mut entry.members, lower_members(&p.members.body));
                }
                // `interface mixin M { … }`: stash M's members; applied to interfaces via `includes`.
                Definition::InterfaceMixin(m) => {
                    let lowered = lower_mixin_members(&m.members.body);
                    extend_unique(mixins.entry(m.identifier.0.to_string()).or_default(), lowered);
                }
                // `X includes M;`: deferred until all mixin bodies are collected (see below).
                Definition::IncludesStatement(inc) => {
                    includes.push((inc.lhs_identifier.0.to_string(), inc.rhs_identifier.0.to_string()));
                }
                _ => {}
            }
        }
    }
    std::panic::set_hook(prev_hook);

    // Resolve `X includes M;` now that every mixin body is known. An interface referenced only by an
    // includes (its base/partials lived in an unparseable file) is still created, so the merge keeps
    // as many members as the corpus can yield.
    for (iface, mixin) in &includes {
        if let Some(members) = mixins.get(mixin) {
            let entry = entry_for(&mut result, iface);
            extend_unique(&mut entry.members, members.clone());
        }
    }

    // Attach the `[Exposed=...]` realms to the interfaces the corpus kept.
    for (name, globals) in exposed_map {
        if let Some(spec) = result.get_mut(&name) {
            spec.exposed = globals;
        }
    }

    // THE LOSS USED TO BE SILENT, AND THAT IS THE ACTUAL BUG.
    //
    // `parse_defs_resync` exists because weedle panics on constructs it cannot consume and a whole-file
    // parse would then lose every definition in the file. Resync recovers most of them -- but the ones
    // it does not recover simply never appear, the corpus is a little smaller, and the grader's answer
    // for those interfaces is not "wrong", it is ABSENT. `HTMLSelectElement`, `HTMLTextAreaElement`,
    // `ReadableStream` and 39 others sat in that hole; the summary read "859 interfaces graded" and
    // nobody was counting the ones that never arrived.
    //
    // So the parse is now checked against what the sources declare, and a shortfall is reported on
    // stderr with the names. It does not abort -- a corpus that lost one interface is still worth
    // grading with -- but it can no longer happen quietly.
    let lost: Vec<&String> = declared.keys().filter(|n| !result.contains_key(*n)).collect();
    if !lost.is_empty() {
        let mut names: Vec<&str> = lost.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        eprintln!(
            "webref corpus: {} of {} declared interfaces did not survive parsing: {}",
            names.len(),
            declared.len(),
            names.join(", ")
        );
    }

    result
}

/// The `interface X` names a source declares, from the raw text. Deliberately independent of weedle:
/// this is the control the parse is measured against, so it must not share the parse's failure modes.
///
/// `partial interface` and `interface mixin` are BOTH excluded, and for the same reason: neither is an
/// interface in its own right. A partial extends a base counted where the base is declared, and a mixin
/// is consumed by the `X includes M;` that pulls it in -- a mixin correctly does not survive as a
/// standalone entry, so counting one as lost would make the check cry wolf 90 times a run and be
/// ignored, which is the failure mode of every check nobody reads.
fn declared_interfaces(src: &str) -> Vec<String> {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?m)^\s*interface\s+([A-Za-z_]\w*)").unwrap());
    RE.captures_iter(src)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .filter(|n| n != "mixin")
        .collect()
}

/// Rewrite the ONE extended-attribute form weedle 0.13 cannot consume.
///
/// WebIDL's `ExtendedAttributeIdent` is `identifier = identifier`. Blink also emits
/// `[ReflectDefault=1.0]` (and `=0`, `=2`, `=20`), a NUMBER on the right, which webref carries through
/// verbatim -- and weedle panics on it with "There is redundant raw data after parsing", taking the
/// declaration with it. Measured: 10 occurrences, and they cost `HTMLSelectElement`,
/// `HTMLTextAreaElement`, `HTMLProgressElement` and `HTMLMarqueeElement` plus everything the resync
/// then skipped past.
///
/// The value is dropped, not the attribute: nothing in this crate reads `ReflectDefault`, and keeping
/// the name means a future reader still sees that the declaration had one. Deliberately NARROW -- a
/// general "strip anything weird" would hide the next unparseable form instead of letting the
/// declared-vs-parsed check above report it.
///
/// `[Reflect="accept-charset"]` (73 occurrences) and `[LegacyFactoryFunction=Audio(...)]` (3) were
/// tested against the same parser and are consumed fine; they are left alone.
fn normalize_for_weedle(src: &str) -> String {
    static REFLECT_DEFAULT: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"ReflectDefault\s*=\s*-?\d+(?:\.\d+)?").unwrap());
    // The other two, both from WebIDL's async-iteration additions, which weedle 0.13 predates:
    //
    //   `async_iterable<any>(optional Opts o = {});`     ReadableStream
    //   `async_iterable<USVString, FileSystemHandle>;`   FileSystemDirectoryHandle
    //   `static X from(async_sequence<any> it);`         ReadableStream
    //
    // Each panics the parser and takes its whole interface with it. The declaration is removed rather
    // than translated: `async iterable<K,V>` implies auto-members (`values`/`entries`/`keys`/
    // `Symbol.asyncIterator`), and inventing them here would be this file guessing at a shape instead
    // of reading one. It does not cost coverage -- the CAPTURE is the denominator and lists them
    // (`ReadableStream.values` is in it), so they are graded from there. What would have cost coverage
    // is what happened before: losing the entire interface.
    static ASYNC_ITERABLE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?m)^\s*async_iterable\s*<[^;]*;\s*$").unwrap()
    });
    static ASYNC_SEQUENCE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"async_sequence\s*<\s*([^>]*)>").unwrap());
    let out = REFLECT_DEFAULT.replace_all(src, "ReflectDefault");
    let out = ASYNC_ITERABLE.replace_all(&out, "");
    // `async_sequence<T>` appears in an ARGUMENT, where dropping the member would lose a real one
    // (`ReadableStream.from`). It is the async form of `sequence<T>` and the grader only reads argument
    // types for arity and marshalling, so `sequence<T>` is the faithful stand-in.
    ASYNC_SEQUENCE.replace_all(&out, "sequence<$1>").into_owned()
}

/// Extract `(interface name, exposed globals)` from the `[... Exposed=... ] interface Name` declarations
/// in a raw `.idl` source. Handles `Exposed=Window`, `Exposed=(Window,Worker)`, and `Exposed=*` (all
/// realms). Only base declarations carry `[Exposed]`; partials/mixins never do.
fn extract_exposed(src: &str) -> Vec<(String, Vec<String>)> {
    use regex::Regex;
    // Reuse compiled regexes across calls.
    thread_local! {
        static DECL: Regex = Regex::new(r"\[([^\]]*)\]\s*interface\s+(\w+)").unwrap();
        static EXP: Regex = Regex::new(r"Exposed\s*=\s*(?:\(([^)]*)\)|(\*)|(\w+))").unwrap();
    }
    DECL.with(|decl| {
        EXP.with(|exp| {
            let mut out = Vec::new();
            for cap in decl.captures_iter(src) {
                let attrs = &cap[1];
                let name = cap[2].to_string();
                if let Some(m) = exp.captures(attrs) {
                    let globals: Vec<String> = if let Some(list) = m.get(1) {
                        list.as_str().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                    } else if m.get(2).is_some() {
                        vec!["*".to_string()]
                    } else {
                        vec![m[3].to_string()]
                    };
                    if !globals.is_empty() {
                        out.push((name, globals));
                    }
                }
            }
            out
        })
    })
}

/// Parse as many top-level definitions from `src` as weedle can, RESYNCING across the ones it can't,
/// by parsing each definition IN ISOLATION.
///
/// `weedle::parse` is all-or-nothing: it runs `many0(Definition)` (stops at the first definition it
/// can't parse) and then `assert!`s the remainder is empty — so one unsupported construct (modern
/// `async iterable`, an ext-attribute weedle lacks, …) discards the whole file, base `Navigator` and
/// all. Nor can we simply resync by re-running `many0` on the suffix: reaching deep into a large file
/// like `html.idl`, weedle's nom parsers exhibit catastrophic backtracking on some constructs and hang.
///
/// So we split `src` into one-definition chunks at brace-depth-0 `;` (via [`def_end`]) and parse each
/// chunk on its own. Each `Definitions::parse` then sees a SINGLE small definition — no cross-neighbour
/// loss, no long-input backtracking — and we keep whatever it accepts (ignoring any trailing remainder
/// rather than asserting on it). Linear in the source length, and only the unparseable definitions drop.
fn parse_defs_resync(src: &str) -> Vec<Definition<'_>> {
    let mut all: Vec<Definition<'_>> = Vec::new();
    let mut input = src;
    while !input.trim_start().is_empty() {
        let (chunk, rest) = match def_end(input) {
            Some(end) => input.split_at(end),
            None => (input, ""), // trailing text with no terminator — try it whole, then stop.
        };
        if let Ok((_, mut defs)) = Definitions::parse(chunk) {
            all.append(&mut defs);
        }
        if rest.len() >= input.len() {
            break; // no forward progress — stop rather than spin.
        }
        input = rest;
    }
    all
}

/// Byte index just past the first brace-depth-0 `;` in `s` — the end of exactly one top-level WebIDL
/// definition (the `;` after a braced block's `}`, or the bare terminator of `includes;`/`typedef …;`),
/// or `None` if none is found. Brace-depth aware, and skips `//` / `/* */` comments and `"…"` string
/// literals so their braces and semicolons never miscount the terminator. `[…]` ext-attribute lists and
/// `(…)` unions/args carry no top-level `;`, so tracking only `{}` suffices.
///
/// Only used to slice chunk boundaries; the definitions themselves still come from weedle's own strict
/// parser, so an imperfect boundary can at worst MISS a definition, never admit a malformed one.
fn def_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut depth: i32 = 0;
    let mut i = 0usize;
    while i < n {
        match bytes[i] {
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                i += 2;
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2; // consume the closing `*/` (harmless overshoot if unterminated).
            }
            b'"' => {
                i += 1;
                while i < n && bytes[i] != b'"' {
                    i += 1;
                }
                i += 1; // consume the closing quote.
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            // `;` is ASCII, so `i + 1` is a valid char boundary.
            b';' if depth <= 0 => return Some((i + 1).min(n)),
            _ => i += 1,
        }
    }
    None
}

/// Get X's spec, creating an empty one (name set, no inheritance, no members) on first sight. A base,
/// a partial, or an includes-target may each be the first thing seen for a given name.
fn entry_for<'a>(map: &'a mut BTreeMap<String, InterfaceSpec>, name: &str) -> &'a mut InterfaceSpec {
    map.entry(name.to_string()).or_insert_with(|| InterfaceSpec {
        name: name.to_string(),
        inherits: None,
        exposed: Vec::new(),
        members: Vec::new(),
    })
}

/// Append `incoming` to `members`, skipping any member structurally equal to one already present —
/// the "UNION" of `parse_corpus`. Exact `MemberSpec` equality is the right key: it drops only true
/// duplicates (a mixin included by two parsed files, a base member redeclared) while preserving
/// operation overloads (same name, different args/return) and every distinct constructor.
fn extend_unique(members: &mut Vec<MemberSpec>, incoming: Vec<MemberSpec>) {
    for m in incoming {
        if !members.contains(&m) {
            members.push(m);
        }
    }
}

/// Lower a mixin's members (weedle `MixinMember` — a *different* enum than interface members) into the
/// normalized [`MemberSpec`] form. Mixin attributes/operations can be neither `static` nor `inherit`,
/// so both flags are always false; the heavy lifting (argument + type stringification) is reused from
/// [`crate::webidl`] verbatim, so nothing is duplicated. `const` is skipped, matching `lower_members`.
fn lower_mixin_members(members: &[MixinMember]) -> Vec<MemberSpec> {
    let mut out = Vec::new();
    for member in members {
        match member {
            MixinMember::Attribute(a) => {
                out.push(MemberSpec::Attribute {
                    name: a.identifier.0.to_string(),
                    ty: attributed_type_to_string(&a.type_),
                    readonly: a.readonly.is_some(),
                    is_static: false,
                    inherited: false,
                    // A mixin attribute can carry [PutForwards] too -- `LinkStyle` does not, but
                    // `HTMLHyperlinkElementUtils` and friends are lowered through here as well.
                    put_forwards: crate::webidl::has_put_forwards(&a.attributes),
                });
            }
            MixinMember::Operation(o) => {
                let name = match &o.identifier {
                    Some(id) => id.0.to_string(),
                    // Anonymous operations in a MIXIN: weedle gives mixins no `special` field (WebIDL
                    // forbids specials there), so this is unreachable for real corpus files.
                    None => continue,
                };
                out.push(MemberSpec::Operation {
                    name,
                    ret: return_type_to_string(&o.return_type),
                    args: lower_args(&o.args.body.list),
                    is_static: false,
                });
            }
            // A bare `stringifier;` is a `toString() -> DOMString` op (as `lower_members` synthesizes).
            MixinMember::Stringifier(_) => {
                out.push(MemberSpec::Operation {
                    name: "toString".to_string(),
                    ret: "DOMString".to_string(),
                    args: Vec::new(),
                    is_static: false,
                });
            }
            // Mixin consts matter disproportionately: WebGL declares its ~1,714 `gl.TRIANGLES`-style
            // enum values on `WebGL2RenderingContextBase` / `WebGLRenderingContextBase`, not on the
            // interface, so dropping them here dropped 82% of WebGL's surface from the denominator —
            // `WebGL2RenderingContext` graded `present 231/231` against a real 1,349.
            MixinMember::Const(c) => {
                out.push(MemberSpec::Const {
                    name: c.identifier.0.to_string(),
                    ty: crate::webidl::const_type_to_string(&c.const_type),
                    value: crate::webidl::const_value_to_string(&c.const_value),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member_name(m: &MemberSpec) -> Option<&str> {
        match m {
            MemberSpec::Attribute { name, .. }
            | MemberSpec::Operation { name, .. }
            | MemberSpec::Const { name, .. } => Some(name.as_str()),
            MemberSpec::Constructor { .. } | MemberSpec::Special { .. } => None,
        }
    }

    #[test]
    fn extract_exposed_parses_every_form() {
        let src = r#"
            [Exposed=Window] interface A {};
            [Exposed=(Window,Worker)] interface B {};
            [Exposed=*] interface C {};
            [SecureContext, Exposed=Worker] interface D {};
            interface E {};
        "#;
        let ex: BTreeMap<String, Vec<String>> = extract_exposed(src).into_iter().collect();
        assert_eq!(ex["A"], vec!["Window"]);
        assert_eq!(ex["B"], vec!["Window", "Worker"]);
        assert_eq!(ex["C"], vec!["*"]);
        assert_eq!(ex["D"], vec!["Worker"]);
        assert!(!ex.contains_key("E"), "no [Exposed] -> not recorded");
    }

    /// The whole point of the merge: `Navigator`'s member list must fold in members that live NOWHERE
    /// in its base block — they arrive only via `Navigator includes <Mixin>;` across three separate
    /// files: `hardwareConcurrency` (NavigatorConcurrentHardware, html.idl), `deviceMemory`
    /// (NavigatorDeviceMemory, device-memory.idl), `userAgentData` (NavigatorUA, ua-client-hints.idl).
    #[test]
    fn merges_navigator_partials_and_mixins() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("webref");
        let specs = parse_corpus(&dir);

        let nav = specs.get("Navigator").expect("Navigator present in the merged corpus");
        let names: Vec<&str> = nav.members.iter().filter_map(member_name).collect();

        for wanted in ["hardwareConcurrency", "deviceMemory", "userAgentData"] {
            assert!(
                names.contains(&wanted),
                "merged Navigator should include mixin member `{wanted}` (pulled in via `Navigator includes …;`); \
                 got {} members: {names:?}",
                nav.members.len()
            );
        }

        // The merge must yield far more than the (empty) base block alone — sanity that partials +
        // the other ~16 mixins folded in too, not just the three asserted above.
        assert!(
            nav.members.len() > 20,
            "expected Navigator to fold in many partial/mixin members, got only {}",
            nav.members.len()
        );

        eprintln!("merged Navigator member count = {} (of which named: {})", nav.members.len(), names.len());
    }
}
