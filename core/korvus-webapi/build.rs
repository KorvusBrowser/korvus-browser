//! Build-time emission of the exposure registry (see src/binding/exposure.rs).
//!
//! This file used to ALSO run the `gen` crate's WebIDL->Rust scaffold over gen/idl/geometry.idl and
//! write one `<family>.gen.rs` per interface family into OUT_DIR "until a domain opts in via
//! `include!`". No domain ever did: the one-table model (`interface!` tables) replaced the scaffold as
//! the way an interface's shape reaches Rust, and the loop ran on every build of the largest crate in
//! the tree with its output discarded. Deleted (the emitter is in git history).

use std::path::{Path, PathBuf};

fn main() {
    emit_registry();
    compile_codegen_shim();
    // Chrome's Skia static library references but does not define a few libwebp/expat symbols on Windows;
    // supply them so the final MSVC link of korvus.exe resolves (see shim/win_skia_stubs.c).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=shim/win_skia_stubs.c");
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        cc::Build::new().file(manifest.join("shim/win_skia_stubs.c")).compile("korvus_win_skia_stubs");
    }
}

// ---------------------------------------------------------------------------------------------
// `shim/codegen.cc`, the two V8 calls rusty_v8 does not bind (the human chose a shim over a
// V8 source build, 2026-09-26) -- and, by the same rule, `shim/console.cc`, the six a console message's
// text needs. Compiled against the headers the `v8` crate ships, the version the workspace
// lockfile resolves -- the same V8 the prebuilt static library was built from.
// ---------------------------------------------------------------------------------------------

fn compile_codegen_shim() {
    println!("cargo:rerun-if-changed=shim/codegen.cc");
    println!("cargo:rerun-if-changed=shim/console.cc");
    println!("cargo:rerun-if-changed=shim/realm.cc");
    println!("cargo:rerun-if-changed=shim/cached_accessor.cc");
    println!("cargo:rerun-if-changed=shim/stream.cc");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let lock = std::fs::read_to_string(manifest.join("../../Cargo.lock")).expect("the workspace Cargo.lock");
    let version = v8_version(&lock).expect("the lockfile resolves a `v8` crate");
    let include = v8_include(&version).unwrap_or_else(|| panic!("the v8-{version} crate's v8/include was not found in the cargo registry"));
    let mut build = cc::Build::new();
    // A fetched script compiled off the page's thread (`core::html::script_stream`): Linux only, the one platform its
    // call of the prebuilt's StreamedSource constructor by symbol has been checked on.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        build.file(manifest.join("shim/stream.cc"));
    }
    build
        .cpp(true)
        .file(manifest.join("shim/codegen.cc"))
        // The calls V8's inspector makes building a console message (modules/console_ns.rs).
        .file(manifest.join("shim/console.cc"))
        // The incumbent realm (scope_timers' map of active timers, `webapi::incumbent_realm`).
        .file(manifest.join("shim/realm.cc"))
        // A getter V8's inline caches may answer from a private (`FunctionTemplate::NewWithCache`, the window singletons).
        .file(manifest.join("shim/cached_accessor.cc"))
        .include(&include)
        .std("c++20")
        // MSVC keeps __cplusplus at 199711L even under -std:c++20 unless told otherwise, and v8config.h
        // checks __cplusplus >= 202002L; /Zc:__cplusplus makes it report the real value. clang/gcc skip it.
        .flag_if_supported("/Zc:__cplusplus")
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .warnings(false)
        // The shim uses no C++ standard library; do not add a dynamic libstdc++ to the binary.
        .cpp_link_stdlib(None)
        .compile("korvus_codegen_shim");
}

/// `version = "..."` of the `[[package]]` whose `name = "v8"`.
fn v8_version(lock: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"v8\"" {
            let v = lines.next()?;
            return v.trim().strip_prefix("version = \"")?.strip_suffix('"').map(str::to_string);
        }
    }
    None
}

fn v8_include(version: &str) -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    let registry = home.join("registry/src");
    for entry in std::fs::read_dir(&registry).ok()?.flatten() {
        let candidate = entry.path().join(format!("v8-{version}")).join("v8/include");
        if candidate.join("v8-isolate.h").exists() {
            return Some(candidate);
        }
    }
    None
}


// ---------------------------------------------------------------------------------------------
// The exposure registry (REFACTOR-BLUEPRINT STEP 4). See src/binding/exposure.rs for the rationale.
//
// Walk src/ for files that DECLARE their own native registration (`webapi_ops!`; `webapi_objects!` went)
// and emit one OUT_DIR file calling each. That is what replaces the 463-symbol `ops = [...]` line
// every agent used to have to edit: registration now lives in the file that defines the op, and this
// list is rebuilt from the tree on every build so it cannot drift.
//
// Detection is a literal token match, deliberately: 58 of the ops are produced by local `macro_rules!`
// rather than a visible `#[op2] fn`, so a scanner that looked for op definitions would have to model
// nine macros and would silently drop an op the day a tenth appears. A dropped op is not a compile
// error — it is a member missing from the JS surface at runtime — so the detection has to be
// unmissable, and tests/registry.rs pins the resulting count.
// ---------------------------------------------------------------------------------------------

fn emit_registry() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let src = PathBuf::from(&manifest).join("src");
    // Any .rs edit can add or remove a declaration, so the registry must be rebuilt when the tree
    // changes. Cargo watches a directory recursively, so the crate root is enough.
    println!("cargo:rerun-if-changed=src");

    let mut ops_mods: Vec<String> = Vec::new();
    collect(&src, &src, &mut ops_mods);
    ops_mods.sort(); // deterministic order: the emitted file must be stable across builds

    let mut out = String::from(
        "// GENERATED by build.rs from the `webapi_ops!` declarations in src/.\n         // DO NOT EDIT and DO NOT COMMIT — rebuilt on every `cargo build`.\n",
    );
    out.push_str("fn collected_ops() -> Vec<OpDecl> {\n    let mut v = Vec::new();\n");
    for m in &ops_mods {
        out.push_str(&format!("    v.extend({m}::__webapi_ops());\n"));
    }
    out.push_str("    v\n}\n\n");
    // The module NAMES the registry was built from, so the manifest gate (tests/registry.rs) can say
    // WHICH declaration was lost in a refactor (a lost declaration is not a compile error -- it would
    // otherwise only show up as a missing JS member).
    let quoted = |v: &[String]| v.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ");
    out.push_str(&format!(
        "/// Module paths that declared ops (sorted), as seen by build.rs.\n         pub(crate) const DECLARING_OP_MODULES: &[&str] = &[{}];\n",
        quoted(&ops_mods)
    ));

    let dst = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("webapi_registry.rs");
    std::fs::write(&dst, out).unwrap_or_else(|e| panic!("write {}: {e}", dst.display()));
}

/// Recurse `dir`, recording the module path of every file that declares ops.
fn collect(root: &Path, dir: &Path, ops: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, ops);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // The file that DEFINES the macros also spells them (in `macro_rules!` and in its docs), so it
        // would otherwise register itself and emit a call to a `__webapi_ops` it does not have.
        if path.ends_with("binding/exposure.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // Match the INVOCATION form specifically — `crate::webapi_ops![`. Requiring the path prefix
        // keeps a doc-comment mention of the bare macro name from registering a module by accident.
        if !text.contains("crate::webapi_ops![") {
            continue;
        }
        let Some(module) = module_path(root, &path) else { continue };
        ops.push(module);
    }
}

/// `src/core/dom/node.rs` -> `crate::core::dom::node`; `src/core/dom/mod.rs` -> `crate::core::dom`;
/// `src/lib.rs` -> `crate` (which cannot declare, but is handled rather than mis-mapped).
fn module_path(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = rel
        .parent()?
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let stem = rel.file_stem()?.to_string_lossy().into_owned();
    if stem != "mod" && stem != "lib" {
        parts.push(stem);
    }
    let mut path = String::from("crate");
    for p in parts {
        path.push_str("::");
        path.push_str(&p);
    }
    Some(path)
}
