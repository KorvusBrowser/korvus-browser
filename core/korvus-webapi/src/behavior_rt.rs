//! Runtime for the generated webapi behavior scaffold (webapi refactor; see
//! docs/archive/WEBAPI-ARCHITECTURE.md "Behavior scaffold").
//!
//! THE TWO SWAPPABLE FUNCTIONS. Generated code never contains an inline panic/throw;
//! it only calls `stub!(label)` (trait defaults) and `dispatch!(label, expr)` (op2
//! bodies), which route here. The entire not-implemented mechanism -- coverage
//! recording, throw conversion, and even panic-vs-registry -- lives in `__stub` +
//! `__dispatch`; it can be changed by editing only this file, touching no generated
//! or behavior code.

use std::any::Any;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, Once, OnceLock};

use deno_error::JsErrorBox;

// ---- Coverage (the dynamic half of the two-dimensional coverage the scaffold gives us).
//
// Every member call routes through __dispatch/__dispatch_try, so recording here captures,
// per member label ("Iface::member"), how many times it was CALLED and how many of those
// hit the not-implemented STUB. Two facts fall out for free:
//   - which members a test suite actually exercises, and
//   - which of those are still stubs (a stub that gets called is a concrete "implement me
//     next, the tests need it" signal -- the implementation-priority data).
// The static half (declared members vs. overridden ones) is computed off the source by
// the `webapi coverage` subcommand; this runtime half tells you what was TOUCHED.
//
// Zero-config: recording is a couple of hashmap bumps (op-call granularity, negligible next
// to the op itself). A dump is written at process exit only when KORVUS_COVERAGE_OUT is set,
// mirroring KORVUS_WPT_OUT/KORVUS_SURFACE_OUT.

#[derive(Default, Clone, Copy)]
struct MemberCov {
    calls: u64,
    stub_hits: u64,
}

fn coverage() -> &'static Mutex<HashMap<&'static str, MemberCov>> {
    static COV: OnceLock<Mutex<HashMap<&'static str, MemberCov>>> = OnceLock::new();
    COV.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_call(label: &'static str) {
    #[cfg(feature = "analysis")]
    spec::analysis::record_bare(spec::analysis::Kind::Op, label);
    if let Ok(mut m) = coverage().lock() {
        m.entry(label).or_default().calls += 1;
    }
    if touch_enabled() {
        touch_label(label);
    }
}

/// [`record_call`] for the hot path, `dispatch!` / `dispatch_try!` (every op2 class member call): the call count
/// is kept only when someone will read it -- `KORVUS_COVERAGE_OUT` is set, so the run dumps it -- or under this
/// crate's unit tests. Everything else `record_call` does (the analysis recorder, the touch log) runs as before.
///
/// WHY: the count was a mutex and a SipHashed map entry per member call, for a map nothing dumps unless that
/// variable is set: 3 % of a `getBoundingClientRect()` loop's self time (release profile, 2026-10-23). The
/// stub paths (`record_stub_call`, a dispatch that hits a stub) still record unconditionally; they are rare, and
/// `coverage_json` callers (a test reads the D2 stub's hit) see them as they did.
#[inline(always)]
fn record_dispatch(label: &'static str) {
    if coverage_on() {
        record_call(label);
        return;
    }
    #[cfg(feature = "analysis")]
    spec::analysis::record_bare(spec::analysis::Kind::Op, label);
    if touch_enabled() {
        touch_label(label);
    }
}

/// Whether the per-call coverage count is kept ([`record_dispatch`]): 0 unread, 1 off, 2 on. The variable is read
/// once, on the first call -- the dump at exit reads the same environment.
static COVERAGE_ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[inline(always)]
fn coverage_on() -> bool {
    match COVERAGE_ON.load(std::sync::atomic::Ordering::Relaxed) {
        2 => true,
        1 => false,
        _ => coverage_on_read(),
    }
}

#[cold]
#[inline(never)]
fn coverage_on_read() -> bool {
    let on = cfg!(test) || std::env::var_os("KORVUS_COVERAGE_OUT").is_some();
    COVERAGE_ON.store(if on { 2 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
    on
}

// ── the impact recorder (P0 step 7) ─────────────────────────────────
// With KORVUS_TOUCH_LOG set, every interface whose member is CALLED -- through the receiver guard
// (`install::receiver`) or through an op2 class's `dispatch!` (above) -- is added to one process-wide
// set, which the WPT sweep empties before each file and reads after it. That per-file set is the
// impact map tier B selects WPT files by. It exists only with the `touch-log` feature: measured
// 2026-09-27, ANY hook here -- a `OnceLock` read, then a relaxed atomic -- moved the per-call bench by
// 5-12 % through code layout, because `dispatch!` runs on every op2 member call.
fn touched() -> &'static Mutex<std::collections::BTreeSet<String>> {
    static T: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()))
}

static TOUCH_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Read `KORVUS_TOUCH_LOG` once. The installer calls it before it mounts a single member, so no call
/// the recorder should see can come first.
pub fn touch_init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if std::env::var_os("KORVUS_TOUCH_LOG").is_some() {
            TOUCH_ON.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Whether the impact recorder is on: compiled in (`touch-log`) AND asked for (`KORVUS_TOUCH_LOG`).
/// Without the feature this is a constant `false`, and every hook folds away.
#[inline(always)]
pub fn touch_enabled() -> bool {
    cfg!(feature = "touch-log") && TOUCH_ON.load(std::sync::atomic::Ordering::Relaxed)
}

#[cold]
#[inline(never)]
fn touch_label(label: &str) {
    touch(label.split("::").next().unwrap_or(label));
}

/// Record that a member of interface `iface` was called.
pub fn touch(iface: &str) {
    if let Ok(mut t) = touched().lock() {
        if !t.contains(iface) {
            t.insert(iface.to_string());
        }
    }
}

/// The interfaces called since the last take, sorted; empties the set.
pub fn take_touched() -> Vec<String> {
    touched().lock().map(|mut t| std::mem::take(&mut *t).into_iter().collect()).unwrap_or_default()
}

fn record_stub(label: &'static str) {
    if let Ok(mut m) = coverage().lock() {
        m.entry(label).or_default().stub_hits += 1;
    }
}

/// Public entry for the one-table installer's D2 stub (`install::stub`): record that `label` was
/// CALLED and that the call hit a stub, into the SAME coverage registry the corpus reads via
/// `KORVUS_COVERAGE_OUT`. Unlike `__stub`, it does not unwind -- the D2 stub returns a shape-correct
/// empty value instead of throwing -- so a page's feature-detection sees "absent", not a crash.
pub fn record_stub_call(label: &'static str) {
    record_call(label);
    record_stub(label);
}

/// Snapshot the runtime coverage as JSON: `{ "Iface::member": {calls, stub_hits}, ... }`.
/// `stub_hits > 0` means the member was invoked but is still a stub.
pub fn coverage_json() -> String {
    let m = coverage().lock().unwrap();
    let mut rows: Vec<(&&str, &MemberCov)> = m.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let body: Vec<String> = rows
        .iter()
        .map(|(k, v)| format!("  {:?}: {{\"calls\": {}, \"stub_hits\": {}}}", k, v.calls, v.stub_hits))
        .collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

/// Write `coverage_json()` to `$KORVUS_COVERAGE_OUT` if set (no-op otherwise). Call at a
/// point where the run is done (e.g. end of a test); also safe to call repeatedly.
pub fn dump_coverage_if_requested() {
    if let Ok(path) = std::env::var("KORVUS_COVERAGE_OUT") {
        let _ = std::fs::write(path, coverage_json());
    }
}

// ---- The LAYER map (goal invariant I7: "one stub, exact ledger").
//
// WHAT WAS WRONG. `tools/status/data/status.json`'s `layer` was read off the SHELL INVENTORY -- the labels
// `make_shell_method` pushes when it builds a shape-only member. That inventory answers a narrower
// question than the file claimed: `install/stub.rs` (the D2 stub every HAND table's `= stub` row
// mounts) registers nothing into it, so a hand-table stub reported `layer: "implemented"`. It was
// wrong in the direction that flatters, and every W5.1 lane's "layer:shell fell by N" was measured
// against it.
//
// WHAT THIS IS. The one member mount (`install::installer::put_member`, invariant I2) knows each
// member's layer exactly, because the layer is a column of the row it is mounting. So it records it,
// here, for EVERY member it mounts -- not only the empty ones -- and `$KORVUS_LAYERS_OUT` dumps the
// result. A member absent from the dump was not mounted by the installer at all: it belongs to the
// JS glue or a native class template outside the one-table model, which is the bucket tracks W4/W5
// drive to zero, and `tools/status/build.py` reports it as `untabled` rather than guessing.
//
// The four layers, and why `slot` is not folded into either neighbour:
//   stub   the D2 stub: answers a shape-correct empty value and records a stub hit.
//   slot   real per-instance storage with no algorithm behind it -- a registry row's `on*` handler
//          pair or its writable-attribute slot accessor. It round-trips a value (`bc.onmessage = f`
//          reads back `f`), so calling it a stub is false; there is no implementation behind it, so
//          calling it native is false too.
//   native a row whose body is Rust (a kit or a hand-written `InstallFn`), and a WebIDL constant,
//          whose VALUE is its whole observable surface.
//   cppgc  a row a cppgc class template realises; the installer VERIFIES it rather than mounting it.

fn layer_map() -> &'static Mutex<HashMap<String, &'static str>> {
    static L: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How honest each layer is, lowest first. Used to resolve the one case where two contexts disagree
/// about a member (the same name mounted from a hand table in one realm and a registry row in
/// another): the WEAKEST answer wins, so the map can never flatter. It is the property this whole
/// unit exists to restore, and a pessimistic `layer` is a visible bug while an optimistic one is not.
fn layer_rank(l: &str) -> u8 {
    match l {
        "stub" => 0,
        "slot" => 1,
        "native" => 2,
        "cppgc" => 3,
        _ => 4,
    }
}

thread_local! {
    /// Nonzero while a FRAME realm's install runs: the layer/receiver maps, the install
    /// flags and the installed-by brands are the PAGE realm's diagnostics (the status grading and the
    /// ledger audit read them), and a frame's second copy of every row only cost install time.
    static BOOKKEEPING_OFF: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether the install diagnostics are being kept (see `BOOKKEEPING_OFF`).
pub fn bookkeeping_on() -> bool {
    BOOKKEEPING_OFF.with(|b| b.get() == 0)
}

/// Run `f` with the install diagnostics off -- a frame realm's install.
pub fn without_bookkeeping<R>(f: impl FnOnce() -> R) -> R {
    BOOKKEEPING_OFF.with(|b| b.set(b.get() + 1));
    let r = f();
    BOOKKEEPING_OFF.with(|b| b.set(b.get() - 1));
    r
}

/// Record the layer of one mounted member, keyed `Iface::member` -- the key `tools/status/build.py`
/// joins the grader on. Called from the one member mount for every row it mounts or verifies.
pub fn record_layer(iface: &str, member: &str, layer: &'static str) {
    if crate::install::templates::capturing() {
        let (iface, member) = (iface.to_string(), member.to_string());
        crate::install::templates::note_mark(crate::install::templates::Mark::Layer { iface, member, layer });
    }
    if !bookkeeping_on() {
        return;
    }
    let Ok(mut m) = layer_map().lock() else { return };
    let key = format!("{iface}::{member}");
    match m.get(&key) {
        Some(prev) if layer_rank(prev) <= layer_rank(layer) => {}
        _ => {
            m.insert(key, layer);
        }
    }
}

/// Snapshot the layer map as JSON: `{ "Iface::member": "stub"|"slot"|"native"|"cppgc", ... }`.
pub fn layers_json() -> String {
    let Ok(m) = layer_map().lock() else { return "{}\n".to_string() };
    let mut rows: Vec<(&String, &&str)> = m.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let body: Vec<String> = rows.iter().map(|(k, v)| format!("  {:?}: {:?}", k, v)).collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

// ── the receiver-policy ledger ─────────────────────────────────────────────────────
//
// The second thing the one member mount records about every row it mounts: which RECEIVER POLICY
// `install::receiver::policy` read off it. It exists so the rollout can be measured rather than
// hoped — `core/korvus-runtime/tests/receiver_sweep.rs` joins it against what each member actually
// does when it is handed a foreign receiver, which is how "how many rows would change answer" has a
// number before the check is switched on and "which rows still answer, and why" has one after.
//
// The values are `install::receiver::Policy`'s names: `throw` (Illegal invocation), `reject` (a
// promise rejected with that TypeError — WebIDL's rule for a promise-returning member), `lenient`
// (`undefined`, for `[LegacyLenientThis]`) and `none` (a static, a constant, an interceptor row, an
// interface-object member, a row whose function this engine did not build, or a row the installer
// mounted somewhere that is not a prototype).

fn receiver_map() -> &'static Mutex<HashMap<String, &'static str>> {
    static R: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the receiver policy of one mounted member, keyed `Iface::member`.
///
/// Like `record_layer`, the WEAKEST answer wins when two realms mount the same name differently: a
/// member that is un-checked in either realm is un-checked as far as a page is concerned, and a
/// ledger that flatters is the failure mode W7 spent a whole track undoing.
pub fn record_receiver(iface: &str, member: &str, policy: &'static str) {
    if crate::install::templates::capturing() {
        let (iface, member) = (iface.to_string(), member.to_string());
        crate::install::templates::note_mark(crate::install::templates::Mark::Receiver { iface, member, policy });
    }
    if !bookkeeping_on() {
        return;
    }
    let Ok(mut m) = receiver_map().lock() else { return };
    let key = format!("{iface}::{member}");
    match m.get(&key) {
        Some(prev) if *prev == "none" => {}
        _ => {
            m.insert(key, policy);
        }
    }
}

/// Snapshot the receiver-policy map as JSON: `{ "Iface::member": "throw"|"reject"|"lenient"|"none" }`.
pub fn receiver_json() -> String {
    let Ok(m) = receiver_map().lock() else { return "{}\n".to_string() };
    let mut rows: Vec<(&String, &&str)> = m.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let body: Vec<String> = rows.iter().map(|(k, v)| format!("  {:?}: {:?}", k, v)).collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

/// Whether the installer's receiver check is compiled ON, so a gate can say which side of the
/// rollout it is measuring instead of inferring it.
pub fn receiver_check_enabled() -> bool {
    crate::install::receiver::ENABLED
}

/// The layer of one member as this process installed it, for a test that wants to assert it.
pub fn layer_of(iface: &str, member: &str) -> Option<&'static str> {
    let m = layer_map().lock().ok()?;
    m.get(&format!("{iface}::{member}")).copied()
}

/// Write `layers_json()` to `$KORVUS_LAYERS_OUT` if set. This is what makes `status.json`'s `layer`
/// exact; see the section comment above for why the shell inventory could not.
pub fn dump_layers_if_requested() {
    if let Ok(path) = std::env::var("KORVUS_LAYERS_OUT") {
        let _ = std::fs::write(path, layers_json());
    }
}

/// Every dump, for a caller that just wants "record what this run learned".
///
/// WHY A BINARY CALLS THIS AND NOT ONLY A TEST. Until now `dump_coverage_if_requested` had exactly
/// one caller -- the WPT harness -- so the only pages that ever reported which members they touched
/// were conformance fixtures. That is the wrong corpus for deciding what to implement next: WPT
/// exercises the platform uniformly, and the open web does not. Wiring this into `korvus run|fetch|
/// probe` is what lets a real-site sweep answer "which unimplemented member did a production site call",
/// which is the ranking the whole implementation plan is supposed to run on.
pub fn dump_run_artifacts() {
    dump_coverage_if_requested();
    dump_layers_if_requested();
}

/// Unwind payload marking "this member has no behavior yet".
struct NotImplemented(&'static str);

/// Install, once, a panic hook that stays silent for `NotImplemented` unwinds (an
/// expected control-flow signal, not a crash) and delegates everything else to the
/// previous hook -- so stubbed members do not spam stderr during a test run.
fn ensure_quiet_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<NotImplemented>().is_none() {
                prev(info);
            }
        }));
    });
}

/// Signal that a member is not implemented. Diverges via unwind; `__dispatch` catches
/// it. Swap point: a registry model would make this unreachable and have `__dispatch`
/// short-circuit before ever calling the trait method (zero unwind).
#[cold]
pub fn __stub(label: &'static str) -> ! {
    ensure_quiet_hook();
    std::panic::panic_any(NotImplemented(label));
}

/// Wrap a behavior call: (record coverage,) run it, convert a not-implemented unwind
/// into a catchable JS exception, and convert any real panic into an error too so it
/// never crosses the op2 FFI frame (resume_unwind there would abort the process).
pub fn __dispatch<T>(label: &'static str, f: impl FnOnce() -> T) -> Result<T, JsErrorBox> {
    record_dispatch(label);
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => {
            if is_not_implemented(&payload) {
                record_stub(label);
            }
            Err(to_js_error(label, &payload))
        }
    }
}

fn is_not_implemented(payload: &Box<dyn Any + Send>) -> bool {
    payload.downcast_ref::<NotImplemented>().is_some()
}

fn to_js_error(label: &'static str, payload: &Box<dyn Any + Send>) -> JsErrorBox {
    if let Some(ni) = payload.downcast_ref::<NotImplemented>() {
        JsErrorBox::new("NotSupportedError", format!("{} is not implemented", ni.0))
    } else {
        JsErrorBox::new("Error", format!("internal error in {label}"))
    }
}

/// Like `__dispatch`, but for a fallible behavior method that returns `Result<T, JsErrorBox>`
/// itself (a member that can throw a real spec error, e.g. an invalid-matrix DOMException).
/// The inner `Result` is flattened through; a not-implemented unwind still becomes an `Err`.
pub fn __dispatch_try<T>(label: &'static str, f: impl FnOnce() -> Result<T, JsErrorBox>) -> Result<T, JsErrorBox> {
    record_dispatch(label);
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            if is_not_implemented(&payload) {
                record_stub(label);
            }
            Err(to_js_error(label, &payload))
        }
    }
}

/// `stub!("Iface::member")` -- trait default body signalling not-implemented.
#[macro_export]
macro_rules! stub {
    ($label:literal) => {
        $crate::behavior_rt::__stub($label)
    };
}

/// `dispatch_try!("Iface::member", <Self as Trait>::member(self, args))` -- op2 body for a
/// behavior method that itself returns `Result<T, JsErrorBox>` (fallible / can throw).
#[macro_export]
macro_rules! dispatch_try {
    ($label:literal, $call:expr) => {
        $crate::behavior_rt::__dispatch_try($label, || $call)
    };
}

/// `dispatch!("Iface::member", <Self as Trait>::member(self, args))` -- op2 body.
#[macro_export]
macro_rules! dispatch {
    ($label:literal, $call:expr) => {
        $crate::behavior_rt::__dispatch($label, || $call)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny stand-in for a generated behavior trait: `ok` stays the throwing default,
    // `imp` is "implemented".
    trait DemoBehavior {
        fn ok(&self) -> f64 {
            stub!("Demo::ok")
        }
        fn imp(&self) -> f64 {
            stub!("Demo::imp")
        }
    }
    struct Demo;
    impl DemoBehavior for Demo {
        fn imp(&self) -> f64 {
            42.0
        }
    }

    #[test]
    fn implemented_returns_ok() {
        let d = Demo;
        let r: Result<f64, JsErrorBox> = dispatch!("Demo::imp", <Demo as DemoBehavior>::imp(&d));
        assert_eq!(r.unwrap(), 42.0);
    }

    #[test]
    fn unimplemented_returns_err_not_crash() {
        let d = Demo;
        let r: Result<f64, JsErrorBox> = dispatch!("Demo::ok", <Demo as DemoBehavior>::ok(&d));
        assert!(r.is_err(), "unimplemented member must surface as a catchable Err");
    }

    // Coverage: an implemented member records a call and NO stub hit; an unimplemented one
    // records both. (Unique labels so this test is independent of the two above.)
    trait CovBehavior {
        fn real(&self) -> f64 { stub!("Cov::real") }
        fn missing(&self) -> f64 { stub!("Cov::missing") }
    }
    struct Cov;
    impl CovBehavior for Cov {
        fn real(&self) -> f64 { 1.0 }
    }

    #[test]
    fn coverage_records_calls_and_stub_hits() {
        let c = Cov;
        let _: Result<f64, JsErrorBox> = dispatch!("Cov::real", <Cov as CovBehavior>::real(&c));
        let _: Result<f64, JsErrorBox> = dispatch!("Cov::missing", <Cov as CovBehavior>::missing(&c));
        let _: Result<f64, JsErrorBox> = dispatch!("Cov::missing", <Cov as CovBehavior>::missing(&c));

        let m = coverage().lock().unwrap();
        let real = m.get("Cov::real").copied().unwrap_or_default();
        let missing = m.get("Cov::missing").copied().unwrap_or_default();
        assert_eq!((real.calls, real.stub_hits), (1, 0), "implemented: called, not a stub");
        assert_eq!((missing.calls, missing.stub_hits), (2, 2), "unimplemented: called and stubbed each time");
    }
}
