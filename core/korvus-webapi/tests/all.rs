//! Every integration test of this crate, in ONE binary.
//!
//! WHY. Cargo compiles each `tests/*.rs` into its own executable, each statically linking the whole
//! dependency graph (deno_core and V8 included). Measured 2026-09-11: 20 binaries for this crate,
//! 3.0 GB of linker output for one `cargo test`, against 218 MB as a single binary. Across the
//! workspace 97 % of 55.6 GB of test binaries was integration tests; the autonomous loop runs full
//! builds all day, and that write volume is what no consumer NVMe survives.
//!
//! THE FILES DO NOT MOVE. `#[path]` keeps every test where it is, which matters because `include_str!`
//! resolves relative to the file containing it -- 86 such call sites across this repo keep working with
//! no edit. Do not "tidy" these into a subdirectory; that would break every one of them.
//!
//! ADDING A TEST. Drop the file in `tests/` and add a line below. `every_test_file_is_in_this_suite`
//! fails if you forget, because a test file that is never compiled looks exactly like coverage and is
//! not.
#[path = "behavior_runner_async.rs"] mod behavior_runner_async;
#[path = "constant_answers.rs"] mod constant_answers;
#[path = "dom_parse.rs"] mod dom_parse;
#[path = "exclusion.rs"] mod exclusion;
#[path = "lenient_this.rs"] mod lenient_this;
#[path = "exotic_interceptor.rs"] mod exotic_interceptor;
#[path = "generated_registry.rs"] mod generated_registry;
#[path = "install_skeleton.rs"] mod install_skeleton;
#[path = "installer_parity.rs"] mod installer_parity;
#[path = "instance_tells.rs"] mod instance_tells;
#[path = "interface_macro.rs"] mod interface_macro;
#[path = "js_strings.rs"] mod js_strings;
#[path = "overwrite_prototype.rs"] mod overwrite_prototype;
#[path = "plain_html_tags.rs"] mod plain_html_tags;
#[path = "quality.rs"] mod quality;
#[path = "registry.rs"] mod registry;
#[path = "shells_arity_parity.rs"] mod shells_arity_parity;
#[path = "shells_worker_echo_probe.rs"] mod shells_worker_echo_probe;
#[path = "slice_probe.rs"] mod slice_probe;
#[path = "symbol_rows.rs"] mod symbol_rows;
#[path = "template_spike.rs"] mod template_spike;
#[path = "worker_surface.rs"] mod worker_surface;

/// A test file that is not listed above is never compiled, and the absence of a test looks exactly like
/// a passing test. This reads the directory and this file's own source, so forgetting a line fails here.
#[test]
fn every_test_file_is_in_this_suite() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let me = std::fs::read_to_string(dir.join("all.rs")).expect("read tests/all.rs");
    const SEPARATE: &[&str] = &[];
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read tests/") {
        let name = entry.expect("dir entry").file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".rs") else { continue };
        if stem == "all" || SEPARATE.contains(&stem) {
            continue;
        }
        if !me.contains(&format!("#[path = \"{stem}.rs\"]")) {
            missing.push(stem.to_string());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "these test files are not in tests/all.rs, so they are NEVER COMPILED OR RUN: {missing:?}\n\
         add `#[path = \"<name>.rs\"] mod <name>;` for each, or list it in SEPARATE with a reason."
    );
}
