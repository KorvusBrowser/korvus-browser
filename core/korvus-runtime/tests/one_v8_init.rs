//! `v8_features::new_runtime` is this crate's ONLY `JsRuntime::new`.
//!
//! This is not a style rule. `src/v8_features.rs` turns `--js-defer-import-eval` and
//! `--js-source-phase-imports` back off AFTER `V8::initialize()`, which is only legal because it
//! asked V8 not to freeze its flag table BEFORE anything initialised V8. A `JsRuntime::new` that
//! bypasses the choke point can win that race — tests in this crate run as parallel threads in one
//! binary, so nothing about test order can promise the Engine is first — and then the flip lands on
//! a frozen flag table, which V8 answers with a FATAL (measured: SIGTRAP, not an error return).
//!
//! So the failure this guards against is not "a feature quietly comes back". It is "the process
//! dies, on a machine that is not this one, depending on thread scheduling". A grep is a cheap price.
//!
//! IT SCANS korvus-webapi TOO, AND THAT WAS NOT THE FIRST GUESS. korvus-webapi looks like a
//! different binary that never reaches the flip -- until you read its `Cargo.toml`, which
//! dev-depends on `engine` precisely so engine-booting tests can live there. So its `all` binary
//! contains both bare `JsRuntime::new` calls and `Engine::new` calls, on parallel threads, and
//! whichever wins decides whether the process survives. MEASURED: `cargo test -p korvus-webapi`
//! died with SIGTRAP, and the same binary with `--test-threads=1` passed all 50. A race, not a
//! failure -- which is the worst kind to leave to a reader.

/// Every `.rs` under `core/korvus-runtime/`, src and tests alike.
fn crate_sources() -> Vec<(std::path::PathBuf, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "rs") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    out.push((p, s));
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let webapi = root.parent().expect("core/").join("korvus-webapi");
    let mut out = Vec::new();
    for base in [root.to_path_buf(), webapi] {
        walk(&base.join("src"), &mut out);
        walk(&base.join("tests"), &mut out);
    }
    out
}

#[test]
fn new_runtime_is_the_only_js_runtime_constructor() {
    // `v8_features.rs` holds the one legitimate call; this file holds the string that finds it.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed = [root.join("src/v8_features.rs"), root.join("tests/one_v8_init.rs")];
    let mut offenders: Vec<String> = Vec::new();
    for (path, body) in crate_sources() {
        if allowed.contains(&path) {
            continue;
        }
        for (i, line) in body.lines().enumerate() {
            // The doc comments in v8_features.rs and in this file NAME the call, so only real code
            // counts: a line whose first non-space characters open a comment is prose.
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with("*") {
                continue;
            }
            if line.contains("JsRuntime::new(") || line.contains("JsRuntime::try_new(") {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a JsRuntime built outside `v8_features::new_runtime` can initialise V8 before the flag \
         table is unfrozen, and the later flag flip is then a FATAL -- as a RACE between parallel \
         test threads, which is how korvus-webapi's suite died with SIGTRAP while passing \
         single-threaded. Use `crate::new_runtime` (inside korvus-runtime) or \
         `engine::new_runtime` (anywhere else).\n  {}",
        offenders.join("\n  ")
    );
}

/// The other half: `init_platform` must be reachable and idempotent, so a caller that wants the
/// flags without a runtime (or calls twice) is safe.
#[test]
fn init_platform_is_idempotent() {
    engine::init_platform();
    engine::init_platform();
    engine::init_platform();
}
