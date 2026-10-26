//! The one place V8 is initialised, and the two JavaScript features Chrome does not ship that
//! deno_core turns on.
//!
//! ## What was wrong
//!
//! `deno_core-0.410.0/runtime/setup.rs::v8_init` hard-codes a flag list for Deno's benefit and hands
//! it to `V8::set_flags_from_string` immediately before `V8::initialize()`. Two of those flags are
//! import-phase proposals that **Chrome 150 does not implement**:
//!
//! ```text
//!   --js-defer-import-eval      `import.defer(x)`
//!   --js-source-phase-imports   `import.source(x)` / `import source x from …`
//! ```
//!
//! MEASURED IN CHROME 150.0.7871.46, `--headless=new` over a loopback `python3 -m http.server`,
//! 2026-09-12 — `new Function("return import.defer('./dep.mjs')")` and the `import.source` form:
//!
//! | probe                    | Chrome 150                                  | korvus, before this |
//! |--------------------------|---------------------------------------------|---------------------|
//! | `import.defer(x)`        | `SyntaxError: Unexpected identifier 'defer'` | parses and runs     |
//! | `import.source(x)`       | `SyntaxError: Unexpected identifier 'source'`| parses and runs     |
//! | `using` / `Symbol.dispose` / `DisposableStack` | present                | present             |
//! | `Float16Array`, `Temporal`                    | present                | present             |
//!
//! So the OTHER three flags deno_core sets (`--js-explicit-resource-management`, `--js-float16array`,
//! `--harmony-temporal`) match Chrome and stay on. Only the two import-phase ones are turned off, and
//! the table above is why the line is drawn there rather than around deno_core's whole list.
//!
//! ## Why it mattered beyond fidelity: it aborted the process
//!
//! `--js-defer-import-eval` did not merely make korvus answer a question Chrome refuses. deno_core's
//! own Defer path is unsound. `ModuleMap::prepare_dyn_imports` instantiates a deferred module and
//! deliberately does NOT evaluate it; when its async dependencies are still pending it pushes a
//! `DynImportModEvaluate` onto `pending_dyn_mod_evaluations` — the same queue the ordinary
//! evaluation phase uses. `evaluate_dyn_imports` then drains that queue into
//! `dynamic_import_resolve`, whose first act is
//!
//! ```text
//! deno_core-0.410.0/modules/map.rs:2424
//!     assert_eq!(module.get_status(), v8::ModuleStatus::Evaluated);
//!         left: Instantiated   right: Evaluated
//! ```
//!
//! A deferred module is Instantiated by construction, so the assertion is guaranteed to fire — it
//! kills the whole process, not the page. `wpt-tiers.sh core` shard 3/11 died there every run, on
//! `html/semantics/scripting-1/the-script-element/module/dynamic-import/tla-deferred-evaluation.tentative.html`
//! (an `import.defer` of a module with a top-level-await dependency: exactly the pending-promise
//! shape), which is why no WPT floor could be recorded. Turning the flag off makes the syntax
//! unparseable, as it is in Chrome, so the unsound path is unreachable rather than merely untaken.
//!
//! ## Why the order below is load-bearing, and measured
//!
//! deno_core sets its flags INSIDE the `Once` that also calls `V8::initialize()`, so an embedder's
//! `v8_set_flags` before it is simply overwritten — there is no window. The only remaining route is
//! to flip the flag back off AFTER initialisation, and V8 refuses that by default: it freezes its
//! flag table at the end of `V8::Initialize()` and a later write is a FATAL, not an error.
//!
//! Measured, not assumed. With step 1 removed and steps 2-3 kept, the test binary died with
//! `SIGTRAP` inside V8's flag code; with step 1 in place the flip takes and `import.defer` becomes a
//! `SyntaxError`. Both halves are pinned by `tests/import_defer.rs`.
//!
//! ## Therefore: every JsRuntime in either kernel crate is created through here
//!
//! [`new_runtime`] is the only `JsRuntime::new` in korvus-runtime AND in korvus-webapi. That is not
//! tidiness — it is the invariant that keeps step 3 from being fatal. If ANY code initialises V8
//! before [`init_platform`] runs, step 1 never happened, V8's flags are frozen, and step 3 aborts
//! the process. `tests/one_v8_init.rs` fails the gate if another constructor appears in either.
//!
//! KORVUS-WEBAPI IS IN THAT RULE, AND I FIRST WROTE HERE THAT IT WAS NOT. The reasoning was that its
//! `#[cfg(test)]` runtimes are a different binary which never reaches step 3. Its `Cargo.toml`
//! dev-depends on `engine` — deliberately, so that engine-booting tests can live next to the tables
//! they check — so that binary holds bare `JsRuntime::new` calls and `Engine::new` calls at once, on
//! parallel threads. MEASURED, after the claim was written: `cargo test -p korvus-webapi` died with
//! SIGTRAP; the same binary at `--test-threads=1` passed all 50. The failure was a RACE, invisible
//! to the single-threaded run that would have been the natural way to check.

use deno_core::{JsRuntime, RuntimeOptions};

/// The flags korvus turns back off after deno_core has had its say, with the Chrome measurement that
/// justifies each one in this module's header. `v8_set_flags` takes argv, so element 0 is a program
/// name V8 ignores and hands straight back.
const CHROME_DOES_NOT_SHIP: [&str; 2] = ["--no-js-defer-import-eval", "--no-js-source-phase-imports"];

/// Initialise V8 exactly once, with Chrome's JavaScript feature set rather than Deno's.
///
/// Idempotent and safe to call from any thread; every caller after the first is a no-op. Call it
/// before creating a `JsRuntime` — or better, do not call it at all and use [`new_runtime`].
pub fn init_platform() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // 1. Keep V8's flag table writable past `V8::Initialize()`. Without this, step 3 is a FATAL
        //    (measured: SIGTRAP). This must precede ANY V8 initialisation in the process.
        let mut argv = vec![String::from("korvus"), String::from("--no-freeze-flags-after-init")];
        let unknown = deno_core::v8_set_flags(argv);
        debug_assert_eq!(unknown.len(), 1, "V8 did not understand --no-freeze-flags-after-init: {unknown:?}");

        // 2. deno_core's own init: its flag list (defer + source phase ON), then `V8::Initialize()`.
        JsRuntime::init_platform(None);

        // 3. And now the two Chrome does not ship. `v8_set_flags` returns the arguments V8 did not
        //    understand; only the program name should come back. A V8 upgrade that renames either
        //    flag shows up here and, more loudly, as a red `tests/import_defer.rs`.
        argv = vec![String::from("korvus")];
        argv.extend(CHROME_DOES_NOT_SHIP.iter().map(|f| String::from(*f)));
        let unknown = deno_core::v8_set_flags(argv);
        debug_assert_eq!(unknown.len(), 1, "V8 did not understand one of {CHROME_DOES_NOT_SHIP:?}: {unknown:?}");

        // 4. The analysis build's pinned-session replay: KORVUS_ANALYSIS_RANDOM_SEED=<n> is
        //    Chrome's `--js-flags=--random-seed=<n>`. V8 starts every realm's Math.random from the flag
        //    (MathRandom::InitializeAndMaybeRefillCache), so a replay draws the numbers the recorded session drew, in
        //    both engines. A diagnostic of the analysis build; the engine itself never sets it.
        #[cfg(feature = "analysis")]
        if let Some(seed) = std::env::var("KORVUS_ANALYSIS_RANDOM_SEED").ok().and_then(|s| s.parse::<i32>().ok()) {
            let unknown = deno_core::v8_set_flags(vec![String::from("korvus"), format!("--random-seed={seed}")]);
            debug_assert_eq!(unknown.len(), 1, "V8 did not understand --random-seed: {unknown:?}");
        }

        // 5. F6 (plugin ABI 3 wave 1): a plugin may force the page's pseudo-random stream reproducible with
        //    `host->set_rng_seed`. When one has, apply it as V8's `--random-seed` so every realm's
        //    Math.random draws the same sequence across runs -- the SAME mechanism as the analysis replay
        //    above (V8 seeds each isolate's RNG from the flag in `MathRandom::InitializeAndMaybeRefillCache`),
        //    driven by the plugin instead of the environment. Plugins load before any engine is built, so the
        //    override is already set by the time V8 initialises here; the flag table stays writable (step 1),
        //    so this write takes. V8's flag is a 32-bit int, so a wide u64 seed keeps its low 32 bits.
        if let Some(seed) = plugin_host::rng_seed_override() {
            let unknown = deno_core::v8_set_flags(vec![String::from("korvus"), format!("--random-seed={}", seed as i32)]);
            debug_assert_eq!(unknown.len(), 1, "V8 did not understand --random-seed: {unknown:?}");
        }
    });
}

/// THE HEAP CONSTRAINTS CHROME GIVES EVERY ISOLATE IT MAKES. Window, frame and worker
/// isolates are all made from `gin::IsolateHolder::getDefaultIsolateParams`, which calls
/// `ResourceConstraints::ConfigureDefaults(SysInfo::AmountOfTotalPhysicalMemory(), SysInfo::AmountOfVirtualMemory())`;
/// this engine passed none, so V8 sized its heap for a machine of unknown memory: a 1,568,669,696-byte limit (1.4 GB of
/// old generation) where Chrome 150 on the same machine allows 4,395,630,592 (4 GB, the cap from 8 GB of memory up --
/// the persona's `jsHeapSizeLimit`, captured there), so a page that holds 2 GB died here and lived in Chrome. The
/// memory is the machine's, as Chrome reads it: `sysconf(_SC_PHYS_PAGES) * _SC_PAGESIZE` is `MemTotal`; the virtual
/// limit is `RLIMIT_DATA`, 0 when unlimited. A machine that answers neither keeps V8's defaults.
fn chrome_heap_constraints(options: &mut RuntimeOptions) {
    if options.create_params.is_some() {
        return;
    }
    let physical = std::fs::read_to_string("/proc/meminfo").ok().and_then(|m| {
        let kb = m.lines().find(|l| l.starts_with("MemTotal:"))?.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        Some(kb * 1024)
    });
    let Some(physical) = physical else { return };
    let virtual_limit = std::fs::read_to_string("/proc/self/limits")
        .ok()
        .and_then(|l| {
            let soft = l.lines().find(|l| l.starts_with("Max data size"))?.split_whitespace().nth(3)?.to_string();
            Some(soft.parse::<u64>().unwrap_or(0))
        })
        .unwrap_or(0);
    options.create_params = Some(deno_core::v8::CreateParams::default().heap_limits_from_system_memory(physical, virtual_limit));
}

/// The only `JsRuntime::new` in korvus-runtime or korvus-webapi: [`init_platform`] first, always, and Chrome's heap
/// constraints ([`chrome_heap_constraints`]).
#[cfg(not(feature = "analysis-v8"))]
pub fn new_runtime(mut options: RuntimeOptions) -> JsRuntime {
    init_platform();
    chrome_heap_constraints(&mut options);
    JsRuntime::new(options)
}

/// [`new_runtime`] for the process that makes the page realm's snapshot (`crate::snapshot`): the same
/// V8 flags and heap constraints -- V8 refuses a snapshot made under other flags -- in a runtime that can be
/// snapshotted.
#[cfg(not(feature = "analysis"))]
pub(crate) fn new_snapshot_runtime(mut options: RuntimeOptions) -> deno_core::JsRuntimeForSnapshot {
    init_platform();
    chrome_heap_constraints(&mut options);
    // V8 gives an ordinary isolate a C++ heap of its own, and a snapshot creator's none: the install's cppgc wrappers
    // (deno_core `wrap_object`: the window singletons minted from Rust state) need one. They are unseated, so collected,
    // before V8 serialises (`webapi::snapshot::prepare_page_snapshot`).
    if let Some(params) = options.create_params.take() {
        let heap = deno_core::v8::cppgc::Heap::create(deno_core::v8::V8::get_current_platform(), deno_core::v8::cppgc::HeapCreateParams::default());
        options.create_params = Some(params.cpp_heap(heap));
    }
    deno_core::JsRuntimeForSnapshot::new(options)
}

/// [`new_runtime`] on the patched V8: it reports builtins and constants when the
/// recorder is on, and the first runtime measures what its post-call hook costs (goal T). A function of
/// its own, so the release body above stays exactly as it was (A3).
#[cfg(feature = "analysis-v8")]
pub fn new_runtime(mut options: RuntimeOptions) -> JsRuntime {
    init_platform();
    chrome_heap_constraints(&mut options);
    webapi::install_builtin_trace();
    let mut rt = JsRuntime::new(options);
    webapi::calibrate_hook_exits(&mut rt);
    rt
}
