//! THE installer (`install::installer`, reading
//! `InterfaceDef` directly) builds, on a bare runtime, exactly the surface the shell installer built --
//! for every interface of both realms, member by member -- and that surface is exactly its rows.
//!
//! Three checks per realm, one dump:
//!   1. **Golden.** `fixtures/installer-surface.{window,worker}.json` is the shell installer's surface
//!      (`install/shells_installer.rs` over the `ShellDef` adapter) captured at cbe3bc97, the commit
//!      that proved the two paths identical live (W2.2), just before the adapter path was deleted
//!      (W2.3). The dump records everything a page can observe of a member: descriptor flags,
//!      nativeness, function names and lengths, constant values (and `-0`), the prototype chain,
//!      `Symbol.toStringTag`, the constructor's `new`/call behaviour, and every member's BEHAVIOUR -- a
//!      zero-argument call (the arity guard), a call on a foreign receiver (the receiver check), the
//!      empty answer (`undefined` vs `null`), a getter read on a fresh instance. Zero differences.
//!      An INTENDED change to the generated surface (W7 unifies the stub) re-captures the golden with
//!      `KORVUS_UPDATE_INSTALLER_GOLDEN=1 cargo test --workspace --test installer_parity` and shows
//!      its diff in review; nothing else may move it.
//!   2. **Rows.** Every interface this run created is its table: constructor name/length/newability,
//!      tag, parent, the prototype's members in table order with their kind, flags, lengths, names and
//!      constant values, the interface object's statics likewise.
//!   3. **Ledger.** The install ledger (invariant I2) names exactly the own string-keyed members of
//!      every created prototype and interface object.

use std::collections::{BTreeMap, HashSet};

use deno_core::{v8, RuntimeOptions};
use webapi::install::generated_registry::{REGISTRY, WORKER_ORDER};
use webapi::install::installer::{self, Holder, InstallLedger};
use webapi::install::{ConstVal, Exposed, InterfaceDef, MemberDef, MemberKind, Model};

const GOLDEN_WINDOW: &str = include_str!("fixtures/installer-surface.window.json");
const GOLDEN_WORKER: &str = include_str!("fixtures/installer-surface.worker.json");

const DUMPER: &str = r#"(() => {
    const FP_TS = Function.prototype.toString;
    const isNative = (fn) => { try { return FP_TS.call(fn).includes("[native code]"); } catch (e) { return false; } };
    const ownNames = (o) => { try { return Object.getOwnPropertyNames(o); } catch (e) { return []; } };
    const shape = (r) => r === undefined ? "undefined" : r === null ? "null" : (typeof r === "object" || typeof r === "function") ? typeof r + ":" + Object.prototype.toString.call(r) : typeof r + ":" + String(r);
    const probe = (f) => { try { return shape(f()); } catch (e) { return "throw:" + (e && e.message); } };
    // A pre-existing ES builtin (PRE: on the global before the install) is owned by neither path;
    // its members are described but not CALLED -- `Date.now()` is a clock, not a surface.
    const pre = new Set(PRE);
    const member = (host, name, inst, behave) => {
        let d; try { d = Object.getOwnPropertyDescriptor(host, name); } catch (e) { return null; }
        if (!d) return null;
        const out = { name, enumerable: d.enumerable, configurable: d.configurable };
        if (d.get || d.set) {
            out.kind = "a"; out.get = !!d.get; out.set = !!d.set;
            out.native = (!d.get || isNative(d.get)) && (!d.set || isNative(d.set));
            if (d.get) {
                out.getName = d.get.name; out.getLength = d.get.length;
                if (behave) {
                    out.read = probe(() => d.get.call(inst));
                    out.readForeign = probe(() => d.get.call({}));
                }
            }
            if (d.set) { out.setName = d.set.name; out.setLength = d.set.length; }
        } else if (typeof d.value === "function") {
            out.kind = "m"; out.native = isNative(d.value); out.length = d.value.length; out.fnName = d.value.name; out.writable = d.writable;
            if (behave) {
                out.call0 = probe(() => d.value.call(inst));
                out.callN = probe(() => d.value.call(inst, 1, 2, 3, 4, 5, 6, 7, 8, 9));
                out.callForeign = probe(() => d.value.call({}, 1, 2, 3, 4, 5, 6, 7, 8, 9));
            }
        } else {
            out.kind = "v"; out.writable = d.writable; out.vtype = typeof d.value;
            if (d.value === null || typeof d.value !== "object") out.value = d.value;
            out.negz = Object.is(d.value, -0);
        }
        return out;
    };
    const skip = new Set(["length", "name", "prototype", "arguments", "caller"]);
    const out = {};
    for (const name of NAMES) {
        const C = globalThis[name];
        if (typeof C !== "function" || !C.prototype) { out[name] = C === undefined ? "absent" : "non-interface:" + typeof C; continue; }
        const proto = C.prototype;
        const inst = Object.create(proto);
        const behave = !pre.has(name);
        const pp = Object.getPrototypeOf(proto);
        const pd = Object.getOwnPropertyDescriptor(C, "prototype");
        const cd = Object.getOwnPropertyDescriptor(proto, "constructor");
        out[name] = {
            ctorName: C.name, ctorLength: C.length, ctorNative: isNative(C),
            newProbe: behave ? probe(() => new C()) : "(pre-existing)",
            callProbe: behave ? probe(() => C()) : "(pre-existing)",
            tag: Object.prototype.toString.call(inst),
            tagDesc: (() => { const t = Object.getOwnPropertyDescriptor(proto, Symbol.toStringTag); return t ? [t.value, t.writable, t.enumerable, t.configurable] : null; })(),
            parent: pp === null ? null : pp === Object.prototype ? "Object.prototype" : (pp.constructor && pp.constructor.name) + (pp.hasOwnProperty("constructor") ? "" : "(inherited ctor)"),
            parentOnGlobal: pp !== null && pp.constructor && globalThis[pp.constructor.name] === pp.constructor,
            protoDesc: pd ? [pd.writable, pd.enumerable, pd.configurable] : null,
            ctorDesc: cd ? [cd.writable, cd.enumerable, cd.configurable, cd.value === C] : null,
            // WHERE `constructor` SITS among the prototype's own keys. This dumper
            // filtered it out exactly as the capture rig did -- reasonably, since its descriptor is
            // identical on every interface of every browser -- and the POSITION went with it, so this
            // gate was blind by construction to the one thing `constructor` says. `ctorIndex` is the
            // index into the UNFILTERED sequence, which is how the bundles record it, so `members`
            // below still reads exactly what every other consumer of it read before.
            ctorIndex: ownNames(proto).indexOf("constructor"),
            members: ownNames(proto).filter((n) => n !== "constructor").map((n) => member(proto, n, inst, behave)).filter(Boolean),
            statics: ownNames(C).filter((n) => !skip.has(n)).map((n) => member(C, n, C, behave)).filter(Boolean),
            symbols: Object.getOwnPropertySymbols(proto).map(String),
        };
    }
    return JSON.stringify(out);
})()"#;

type Surface = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Copy)]
enum Realm {
    Window,
    Worker,
}

impl Realm {
    fn registry(self) -> &'static [&'static InterfaceDef] {
        match self {
            // ONE table, two views: the window walks it in its own capture's order and the
            // worker walks `WORKER_ORDER`, the same rows permuted into the worker capture's. Both
            // filter every row and every member on `exposed`.
            Realm::Window => REGISTRY,
            Realm::Worker => WORKER_ORDER,
        }
    }
    fn exposed(self) -> Exposed {
        match self {
            Realm::Window => Exposed::Window,
            Realm::Worker => Exposed::Worker,
        }
    }
    fn surface_realm(self) -> spec::SurfaceRealm {
        match self {
            Realm::Window => spec::SurfaceRealm::Window,
            Realm::Worker => spec::SurfaceRealm::Worker,
        }
    }
    /// The rows THIS realm has -- the table filtered by each row's `exposed`. `REGISTRY` carries both
    /// realms since W6, so the window walk must not look for `DedicatedWorkerGlobalScope` and the
    /// worker walk must not look for `HTMLDivElement`: each is absent in the other realm by design,
    /// and a test that asks for it is reading its own input list as a defect.
    fn defs(self) -> Vec<&'static InterfaceDef> {
        self.registry().iter().copied().filter(|d| d.exposed.covers(self.exposed())).collect()
    }
    fn label(self) -> &'static str {
        match self {
            Realm::Window => "window",
            Realm::Worker => "worker",
        }
    }
    fn golden(self) -> &'static str {
        match self {
            Realm::Window => GOLDEN_WINDOW,
            Realm::Worker => GOLDEN_WORKER,
        }
    }
}

fn dump<'s>(scope: &mut v8::PinScope<'s, '_>, realm: Realm, pre: &HashSet<&'static str>) -> Surface {
    let names: Vec<&str> = realm.defs().iter().map(|d| d.name).collect();
    let mut pre: Vec<&str> = pre.iter().copied().collect();
    pre.sort_unstable();
    let src = DUMPER.replace("NAMES", &serde_json::to_string(&names).unwrap()).replace("PRE", &serde_json::to_string(&pre).unwrap());
    let s = v8::String::new(scope, &src).unwrap();
    let out = v8::Script::compile(scope, s, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    serde_json::from_str(&out).unwrap()
}

/// Names already on the bare global before any install (ES builtins the registry also lists).
fn pre_existing<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>, realm: Realm) -> HashSet<&'static str> {
    let mut pre = HashSet::new();
    for d in realm.defs() {
        let k = v8::String::new(scope, d.name).unwrap();
        if global.get(scope, k.into()).map(|v| !v.is_undefined()).unwrap_or(false) {
            pre.insert(d.name);
        }
    }
    pre
}

/// The SHIPPED bundle's surface for this realm, read through the persona exactly as the engine reads
/// it -- so this gate cannot grade the installer against a bundle it was not given.
fn shipped_surface(realm: Realm) -> Option<std::sync::Arc<spec::RealmSurface>> {
    use spec::Persona;
    persona::CapturedPersona::shipped().surface(match realm {
        Realm::Window => spec::SurfaceRealm::Window,
        Realm::Worker => spec::SurfaceRealm::Worker,
    })
}

/// The table installer over the registry on a bare runtime, in the live bootstrap's pass order.
/// Returns the surface, the install ledger, and the names that pre-existed on the bare global.
fn build(realm: Realm) -> (Surface, InstallLedger, HashSet<&'static str>) {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);
    // THE BUNDLE THIS BARE RUNTIME WEARS. A `new_runtime` has no persona, so the
    // installer would otherwise install the whole UNION -- every bundle's rows at once -- and the
    // golden below would have to be re-captured to a surface no device has. Seeding the SHIPPED
    // bundle's own selector turns this file into the proof the union needs: golden BYTE-IDENTICAL,
    // captured at cbe3bc97 from the shell installer, means `union x brave-151's surface.json` is
    // exactly the surface that installer built. The two `only_now` classes it caught when the union
    // first landed were the twenty Chrome-only interfaces and `Image`'s three Chrome-only members.
    webapi::install::surface::seed(scope, realm.surface_realm(), shipped_surface(realm));
    let pre = pre_existing(scope, global, realm);
    let mut ledger = InstallLedger::new();
    let reg = realm.registry();
    match realm {
        Realm::Window => {
            installer::install_registry(scope, global, reg, realm.exposed(), &mut ledger);
            installer::finalize_shapes(scope, global, reg, realm.exposed());
            installer::backfill_members(scope, global, reg, realm.exposed(), &mut ledger);
            // `installer::install_arity_guards` WAS HERE: each create guards its own methods inside the
            // recording, and the pass found nothing left to guard, so it was deleted.
            // `installer::install_member_order` WAS HERE, between the two. It walked
            // `generated_order::ORDER` -- a second generated table of 407 interfaces x their recorded
            // member order -- and delete-and-redefined each prototype back into it. The golden below
            // records every interface's member order, so this file is the window realm's own
            // before/after: dropping the call leaves the golden byte-identical, because the registry
            // rows the installer walks were already IN that order.
            // the `constructor` seat pass (`seat_constructors_in_capture_order`) and the
            // constant seal (`seal_constants`) ran here, in the live bootstrap's positions, until
            // Both are deleted: the staged creates seat their own `constructor` inside the
            // recording, and the seal's only effect was B23's defect.
        }
        // NO SEAT PASS HERE, and that mirrors the live worker bootstrap, which does not make the call
        // either (with the measurement in `korvus-runtime/src/worker.rs`): with the
        // worker artifacts recording `ctorIndex`, `install_worker_registry`'s own creates already put
        // `constructor` where this realm's capture puts it, and adding the pass moved nothing --
        // this golden is byte-identical with and without it.
        Realm::Worker => installer::install_worker_registry(scope, global, reg, &mut ledger),
    }
    (dump(scope, realm, &pre), ledger, pre)
}

/// The first differing leaf between two JSON values, as a readable path.
fn first_diff(path: &str, a: &serde_json::Value, b: &serde_json::Value) -> Option<String> {
    use serde_json::Value::*;
    match (a, b) {
        (Object(x), Object(y)) => {
            for (k, va) in x {
                match y.get(k) {
                    None => return Some(format!("{path}.{k}: golden={va} now=<missing>")),
                    Some(vb) => {
                        if let Some(d) = first_diff(&format!("{path}.{k}"), va, vb) {
                            return Some(d);
                        }
                    }
                }
            }
            for k in y.keys() {
                if !x.contains_key(k) {
                    return Some(format!("{path}.{k}: golden=<missing> now={}", y[k]));
                }
            }
            None
        }
        (Array(x), Array(y)) => {
            if x.len() != y.len() {
                let nx: Vec<&str> = x.iter().filter_map(|m| m["name"].as_str()).collect();
                let ny: Vec<&str> = y.iter().filter_map(|m| m["name"].as_str()).collect();
                let only_golden: Vec<&&str> = nx.iter().filter(|n| !ny.contains(n)).collect();
                let only_now: Vec<&&str> = ny.iter().filter(|n| !nx.contains(n)).collect();
                return Some(format!("{path}: length golden={} now={} only_golden={only_golden:?} only_now={only_now:?}", x.len(), y.len()));
            }
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                let label = va["name"].as_str().map(|n| format!("[{n}]")).unwrap_or_else(|| format!("[{i}]"));
                if let Some(d) = first_diff(&format!("{path}{label}"), va, vb) {
                    return Some(d);
                }
            }
            None
        }
        _ => (a != b).then(|| format!("{path}: golden={a} now={b}")),
    }
}

fn golden_path(realm: Realm) -> String {
    format!("{}/tests/fixtures/installer-surface.{}.json", env!("CARGO_MANIFEST_DIR"), realm.label())
}

// ── 1. golden ────────────────────────────────────────────────────────────────────────────────

fn assert_golden(realm: Realm, now: &Surface) {
    let label = realm.label();
    if std::env::var_os("KORVUS_UPDATE_INSTALLER_GOLDEN").is_some() {
        let path = golden_path(realm);
        std::fs::write(&path, serde_json::to_string(now).unwrap()).unwrap_or_else(|e| panic!("write {path}: {e}"));
        println!("{label}: golden rewritten at {path}");
        return;
    }
    let golden: Surface = serde_json::from_str(realm.golden()).unwrap();
    let reg = realm.defs();
    let mut diffs = Vec::new();
    let mut exact = 0usize;
    for d in &reg {
        let (Some(g), Some(n)) = (golden.get(d.name), now.get(d.name)) else {
            diffs.push(format!("{}: missing from a dump (golden={} now={})", d.name, golden.contains_key(d.name), now.contains_key(d.name)));
            continue;
        };
        match first_diff(d.name, g, n) {
            None => exact += 1,
            Some(diff) => diffs.push(diff),
        }
    }
    assert!(
        diffs.is_empty(),
        "{label}: {} interfaces differ from the golden surface (first 40):\n  {}\n(an intended change re-captures it: KORVUS_UPDATE_INSTALLER_GOLDEN=1 cargo test --workspace --test installer_parity)",
        diffs.len(),
        diffs.iter().take(40).cloned().collect::<Vec<_>>().join("\n  ")
    );
    assert_eq!(exact, reg.len(), "{label}: every registry interface compared");
    assert_eq!(golden.len(), now.len(), "{label}: the golden lists exactly the registry");
    println!("{label}: {exact} interfaces identical to the golden");
}

// ── 2. rows ──────────────────────────────────────────────────────────────────────────────────

fn kind_letter(m: &MemberDef) -> &'static str {
    match m.kind {
        MemberKind::Attr { .. } | MemberKind::StaticAttr { .. } | MemberKind::Handler => "a",
        MemberKind::Method | MemberKind::StaticMethod => "m",
        MemberKind::Const => "v",
        MemberKind::NamedGetter | MemberKind::IndexedGetter => "",
    }
}

fn accessor_flags(m: &MemberDef) -> (bool, bool) {
    match m.kind {
        MemberKind::Attr { readonly } | MemberKind::StaticAttr { readonly } => (true, !readonly),
        MemberKind::Handler => (true, true),
        _ => (false, false),
    }
}

/// The JSON a const row's value must read back as (the installer's `cval_v8`: an integral in-range
/// number is an Integer, any finite number a Number, non-finite/null/none `null`). An integral value
/// is compared as a JSON integer, which is how `JSON.stringify` prints it.
fn cval_json(cv: ConstVal) -> serde_json::Value {
    match cv {
        ConstVal::Num(n) if n.is_finite() && n.fract() == 0.0 && n.abs() < 9007199254740992.0 => serde_json::json!(n as i64),
        ConstVal::Num(n) if n.is_finite() => serde_json::json!(n),
        ConstVal::Num(_) | ConstVal::None | ConstVal::Null => serde_json::Value::Null,
        ConstVal::Bool(b) => serde_json::json!(b),
        ConstVal::Str(s) => serde_json::json!(s),
    }
}

fn check_members<'a>(iface: &str, half: &str, live: &[serde_json::Value], rows: impl Iterator<Item = &'a MemberDef>, bad: &mut Vec<String>) {
    let rows: Vec<&MemberDef> = rows.collect();
    if live.len() != rows.len() {
        let ln: Vec<&str> = live.iter().filter_map(|m| m["name"].as_str()).collect();
        let rn: Vec<&str> = rows.iter().map(|m| m.name).collect();
        bad.push(format!("{iface}.{half}: count live={} rows={} | live={ln:?} rows={rn:?}", live.len(), rows.len()));
        return;
    }
    for (lv, m) in live.iter().zip(rows) {
        let at = format!("{iface}.{half}.{}", m.name);
        if lv["name"].as_str() != Some(m.name) {
            bad.push(format!("{at}: order, got {:?}", lv["name"]));
            continue;
        }
        let k = kind_letter(m);
        if lv["kind"].as_str() != Some(k) {
            bad.push(format!("{at}: kind got {:?} want {k}", lv["kind"]));
            continue;
        }
        if lv["enumerable"].as_bool() != Some(m.enumerable) {
            bad.push(format!("{at}: enumerable got {:?} want {}", lv["enumerable"], m.enumerable));
        }
        if lv["configurable"].as_bool() != Some(m.configurable) {
            bad.push(format!("{at}: configurable got {:?} want {}", lv["configurable"], m.configurable));
        }
        match k {
            "m" => {
                if lv["native"].as_bool() != Some(true) {
                    bad.push(format!("{at}: not native"));
                }
                if lv["fnName"].as_str() != Some(m.name) {
                    bad.push(format!("{at}: fnName got {:?}", lv["fnName"]));
                }
                if lv["length"].as_u64() != Some(m.length as u64) {
                    bad.push(format!("{at}: length got {:?} want {}", lv["length"], m.length));
                }
                if lv["writable"].as_bool() != Some(true) {
                    bad.push(format!("{at}: method not writable"));
                }
            }
            "a" => {
                let (g, s) = accessor_flags(m);
                if lv["native"].as_bool() != Some(true) {
                    bad.push(format!("{at}: not native"));
                }
                if lv["get"].as_bool() != Some(g) || lv["set"].as_bool() != Some(s) {
                    bad.push(format!("{at}: get/set got {:?}/{:?} want {g}/{s}", lv["get"], lv["set"]));
                }
                if g && (lv["getName"].as_str() != Some(&format!("get {}", m.name)) || lv["getLength"].as_u64() != Some(0)) {
                    bad.push(format!("{at}: getter got {:?}/{:?}", lv["getName"], lv["getLength"]));
                }
                if s && (lv["setName"].as_str() != Some(&format!("set {}", m.name)) || lv["setLength"].as_u64() != Some(1)) {
                    bad.push(format!("{at}: setter got {:?}/{:?}", lv["setName"], lv["setLength"]));
                }
            }
            "v" => {
                if lv["writable"].as_bool() != Some(false) {
                    bad.push(format!("{at}: const writable"));
                }
                let want = cval_json(m.cval);
                if lv["value"] != want {
                    bad.push(format!("{at}: value got {} want {want}", lv["value"]));
                }
            }
            _ => bad.push(format!("{at}: an interceptor row is not a prototype member")),
        }
    }
}

fn assert_rows(realm: Realm, now: &Surface, pre: &HashSet<&'static str>) {
    let label = realm.label();
    let mut bad = Vec::new();
    let mut checked = 0usize;
    // The bundle this bare run wears, read ONCE (the persona re-parses its values on every
    // construction, and this loop runs 672 times).
    let surface = shipped_surface(realm);
    for d in realm.defs() {
        if pre.contains(d.name) {
            continue;
        }
        let Some(rec) = now.get(d.name).filter(|v| v.is_object()) else { continue };
        checked += 1;
        let n = d.name;
        if rec["ctorName"].as_str() != Some(n) {
            bad.push(format!("{n}: ctor name {:?}", rec["ctorName"]));
        }
        let cl = d.ctor.as_ref().map(|c| c.length as u64).unwrap_or(0);
        if rec["ctorLength"].as_u64() != Some(cl) {
            bad.push(format!("{n}: ctor length {:?} want {cl}", rec["ctorLength"]));
        }
        if rec["ctorNative"].as_bool() != Some(true) {
            bad.push(format!("{n}: ctor not native"));
        }
        // THE TWO CALL SHAPES OF AN INTERFACE OBJECT, AND WHY THEY ARE TWO RULES.
        //
        // MEASURED, not derived: `google-chrome --headless=new` 150.0.7871.46 over a loopback server,
        // 2026-09-12, probing `new X()` AND `X()` for every uppercase-initial global of a page (949)
        // and of a real worker (294). The two shapes DISAGREE, so one message for both would be wrong
        // for one of them:
        //
        //   model                     new X()                                      X()
        //   ------------------------- -------------------------------------------- ----------------------------------
        //   Interface / Singleton     Failed to construct 'X': Illegal constructor  Illegal constructor
        //   Element ([HTMLConstructor]) Failed to construct 'X': Illegal constructor  Failed to construct 'X': Please
        //                                                                            use the 'new' operator, this DOM
        //                                                                            object constructor cannot be
        //                                                                            called as a function.
        //   Value, ctor.length 0      an instance                                   the same "Please use the 'new'…"
        //   Value, ctor.length N > 0  Failed to construct 'X': N argument(s)        the same "Please use the 'new'…"
        //                             required, but only 0 present.
        //
        // The discriminator for the prefix is `new.target`, not the syntax: `Reflect.construct(Selection,
        // [])` gets it and `Reflect.apply(Selection, null, [])` does not. The discriminator for the
        // bare-call MESSAGE is whether WebIDL declares a constructor at all: an `[HTMLConstructor]`
        // interface has one (it is how a custom element is upgraded), so "you forgot new" is the
        // truthful answer; an interface with none has nothing to construct.
        //
        // This rule ASSERTED THE OPPOSITE until W9.20 -- `new` "C is not a constructor" (V8's, naming
        // the dumper's own binding) and a call "Illegal constructor" -- which was a faithful record of
        // what this engine did and a record of nothing Chrome does. MEASURED: run the rule below
        // against the pre-W9.20 engine (this file's change alone, `git checkout <base> -- src`) and it
        // reports 751 differences -- 598 window, 153 worker. It bites.
        //
        // UNLESS THE ROW CARRIES A BODY. `install::ctors::row_ctor` names a Rust
        // constructor for twelve generated rows, and a body ANSWERS FOR ITSELF: it validates its
        // arguments, so `new OffscreenCanvas()` with no arguments throws rather than minting an
        // instance. That is not this engine deviating from its rows -- it is the capture: the bundle
        // records `newable: false` for `OffscreenCanvas`, `PeriodicWave`, `AudioWorkletNode`, both
        // channel nodes and the three `GPUError`s (window AND worker), and headless Chrome
        // 150.0.7871.46 on this machine throws "Failed to construct 'X': N argument(s) required, but
        // only 0 present." for every one of them (measured 2026-09-12, over a loopback server). The
        // rule below asserted the OPPOSITE for those eight, and passed only because the body used to
        // be applied by a later pass this bare runtime never runs -- the swap, since deleted.
        //
        // What still holds for a row with a body, and is what is checked: the interface object has a
        // `[[Construct]]`, i.e. `new` does not fail in V8 before the callback runs. The MESSAGE the
        // body then throws is recorded leaf by leaf in the golden above, not restated here.
        let new_probe = rec["newProbe"].as_str().unwrap_or("");
        let call_probe = rec["callProbe"].as_str().unwrap_or("");
        let illegal_new = format!("throw:Failed to construct '{n}': Illegal constructor");
        let use_new = format!(
            "throw:Failed to construct '{n}': Please use the 'new' operator, this DOM object constructor cannot be called as a function."
        );
        if webapi::install::ctors::has_row_ctor(n) {
            // The MESSAGE is the body's own (an arity error, or a mint); what the model still fixes is
            // that there is a `[[Construct]]` to enter at all.
            if new_probe == "throw:C is not a constructor" {
                bad.push(format!("{n}: its row names a constructor body, but the interface object has no [[Construct]]"));
            }
        } else if d.model == Model::Value {
            // ARITY FIRST. A `Model::Value` row whose `ctor.length` is non-zero has a
            // constructor that REQUIRES arguments, and the dumper calls it with none -- so WebIDL's
            // overload-resolution step throws before any body runs, exactly as Chrome 150 does for
            // 206 window and 67 worker interface objects (`new CloseEvent()` ->
            // `Failed to construct 'CloseEvent': 1 argument required, but only 0 present.`). The
            // count is the row's own `length`, which is what the capture recorded off the browser and
            // what the browser's own message quotes: verified equal in all 273 (2026-09-12).
            let want_new = if cl > 0 {
                format!(
                    "throw:Failed to construct '{n}': {cl} argument{} required, but only 0 present.",
                    if cl == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            if cl > 0 {
                if new_probe != want_new {
                    bad.push(format!("{n}: newable with {cl} required argument(s), but `new` gave {new_probe:?} want {want_new:?}"));
                }
            } else if !new_probe.starts_with("object:") {
                bad.push(format!("{n}: newable, but `new` gave {new_probe:?}"));
            }
            if call_probe != use_new {
                bad.push(format!("{n}: newable, but a bare call gave {call_probe:?} want {use_new:?}"));
            }
        } else {
            let want_call = if d.model == Model::Element { use_new } else { "throw:Illegal constructor".to_string() };
            if new_probe != illegal_new || call_probe != want_call {
                bad.push(format!(
                    "{n}: not newable ({:?}), but `new` gave {new_probe:?} want {illegal_new:?} and a call gave {call_probe:?} want {want_call:?}",
                    d.model
                ));
            }
        }
        match (d.tag, rec["tagDesc"].as_array()) {
            (Some(t), Some(td)) => {
                if td.first().and_then(|v| v.as_str()) != Some(t) || td[1] != false || td[2] != false || td[3] != true {
                    bad.push(format!("{n}: tag {td:?} want [{t:?}, false, false, true]"));
                }
            }
            (Some(t), None) => bad.push(format!("{n}: no own toStringTag, want {t:?}")),
            (None, Some(td)) => bad.push(format!("{n}: tag {td:?} on a tagless row")),
            (None, None) => {}
        }
        if rec["protoDesc"] != serde_json::json!([false, false, false]) {
            bad.push(format!("{n}: .prototype descriptor {}", rec["protoDesc"]));
        }
        if rec["ctorDesc"] != serde_json::json!([true, false, true, true]) {
            bad.push(format!("{n}: .constructor descriptor {}", rec["ctorDesc"]));
        }
        if let Some(p) = d.parent {
            // A parent this realm exposes (or, in the worker, a phantom for a base it does not) must be
            // the chain's next prototype; a parent that is neither is left unlinked at Object.prototype.
            let parent = rec["parent"].as_str().unwrap_or("");
            let parent_present = now.get(p).map(|v| v.is_object()).unwrap_or(false);
            if (parent_present || matches!(realm, Realm::Worker)) && parent != p {
                bad.push(format!("{n}: parent {parent:?} want {p:?}"));
            }
        }
        let members = rec["members"].as_array().unwrap();
        let statics = rec["statics"].as_array().unwrap();
        // Rows this realm exposes AND THIS BUNDLE SELECTS, in the bundle's order. A row marked for the
        // other realm is not part of this realm's surface and must not be looked for on it
        // (`DOMMatrix` has `setMatrixValue` in the window capture and not in the worker's) -- and since
        // The same is true of a row belonging to ANOTHER BUNDLE: the table carries the
        // Chromium family's union, and `Image.browsingTopics` is Chrome's, so a run wearing
        // `brave-151-linux-20260907T1006` must not mount it and this check must not ask for it.
        //
        // The selector is applied by the same function the installer uses, not re-derived here: two
        // implementations of "which rows does this device have" is two answers waiting to disagree,
        // and the one that matters is the installer's.
        let ex = realm.exposed();
        let selected: HashSet<String> = match surface.as_deref().and_then(|s| s.members_of(n)) {
            Some(o) => o.members.iter().chain(o.statics.iter()).cloned().collect(),
            // The capture does not speak for this name: the table's own rows stand, which is what the
            // installer does too.
            None => d.members.iter().map(|m| m.name.to_string()).collect(),
        };
        // STRING-keyed rows against `members` (the dump's `getOwnPropertyNames`); a SYMBOL-keyed row (`@@iterator`,
        // which the installer appends whatever the bundle selects) against `symbols` (the first
        // registry rows keyed on a symbol -- until then this filter's `@@` clause matched nothing).
        let keeps = |m: &&webapi::install::MemberDef| m.exposed.covers(ex) && selected.contains(m.name) && !m.name.starts_with("@@");
        check_members(n, "prototype", members, d.members.iter().filter(|m| !m.static_member).filter(keeps), &mut bad);
        check_members(n, "static", statics, d.members.iter().filter(|m| m.static_member).filter(keeps), &mut bad);
        let symbols: Vec<&str> = rec["symbols"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        for m in d.members.iter().filter(|m| !m.static_member && m.exposed.covers(ex) && m.name.starts_with("@@")) {
            let want = format!("Symbol(Symbol.{})", &m.name[2..]);
            if !symbols.contains(&want.as_str()) {
                bad.push(format!("{n}.prototype.{}: no own {want} (symbols {symbols:?})", m.name));
            }
        }
        // No early break (W9.20): the failure MESSAGE still shows only the first 40, but the COUNT is
        // the whole registry's, so "how far off is the tree?" is answerable from a red run instead of
        // needing a hand-edited cap. It costs nothing on a green one, and it is how this unit read its
        // own before/after number off the pre-fix engine.
    }
    assert!(
        bad.is_empty(),
        "{label}: the built surface disagrees with its rows -- {} differences, first 40:\n  {}",
        bad.len(),
        bad.iter().take(40).cloned().collect::<Vec<_>>().join("\n  ")
    );
    assert!(checked > realm.defs().len() / 2, "{label}: the bare run created the bulk of the registry ({checked})");
    println!("{label}: {checked} created interfaces are their rows");
}

// ── 3. ledger ────────────────────────────────────────────────────────────────────────────────

fn assert_ledger(realm: Realm, now: &Surface, ledger: &InstallLedger, pre: &HashSet<&'static str>) {
    let label = realm.label();
    let mut bad = Vec::new();
    let mut created = 0usize;
    for d in realm.defs() {
        if pre.contains(d.name) {
            continue;
        }
        let Some(rec) = now.get(d.name).filter(|v| v.is_object()) else { continue };
        created += 1;
        // and the symbol-keyed rows, as `@@name`; the tag is the installer's own, not a row.
        let proto_have: Vec<String> = std::iter::once("constructor".to_string())
            .chain(rec["members"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap().to_string()))
            .chain(
                rec["symbols"].as_array().into_iter().flatten().filter_map(|v| v.as_str())
                    .filter_map(|s| s.strip_prefix("Symbol(Symbol.").and_then(|r| r.strip_suffix(')')))
                    .filter(|n| *n != "toStringTag")
                    .map(|n| format!("@@{n}")),
            )
            .collect();
        let ctor_have: Vec<String> = rec["statics"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap().to_string()).collect();
        let proto_ledger: Vec<&str> = ledger.entries().iter().filter(|e| e.iface == d.name && e.holder == Holder::Prototype).map(|e| e.member).collect();
        let ctor_ledger: Vec<&str> = ledger.entries().iter().filter(|e| e.iface == d.name && e.holder == Holder::InterfaceObject).map(|e| e.member).collect();
        let ph: HashSet<&str> = proto_have.iter().map(String::as_str).collect();
        let pl: HashSet<&str> = proto_ledger.iter().copied().collect();
        let ch: HashSet<&str> = ctor_have.iter().map(String::as_str).collect();
        let cl: HashSet<&str> = ctor_ledger.iter().copied().collect();
        if ph != pl || proto_ledger.len() != pl.len() {
            bad.push(format!("{}: prototype own-not-ledgered={:?} ledgered-not-own={:?}", d.name, ph.difference(&pl).collect::<Vec<_>>(), pl.difference(&ph).collect::<Vec<_>>()));
        }
        if ch != cl || ctor_ledger.len() != cl.len() {
            bad.push(format!("{}: interface object own-not-ledgered={:?} ledgered-not-own={:?}", d.name, ch.difference(&cl).collect::<Vec<_>>(), cl.difference(&ch).collect::<Vec<_>>()));
        }
    }
    assert!(bad.is_empty(), "{label}: install ledger disagrees with the built surface (first 20):\n  {}", bad.iter().take(20).cloned().collect::<Vec<_>>().join("\n  "));
    println!("{label}: ledger covers {created} created interfaces, {} entries", ledger.len());
}

// ── 4. `constructor`'s seat ──────────────────────────────────────────────────────────────────

/// WHERE `constructor` SITS on every prototype, against the worn bundle.
///
/// `Object.getOwnPropertyNames(X.prototype).indexOf("constructor")` is a number a fingerprinting
/// script gets for free while walking a prototype, and on 635 of brave-151's 930 interfaces this
/// engine answered 0 where the device answers last. Nothing saw it because the capture rig and the
/// dumper above both FILTERED `constructor` out of the member list, so the position went with it.
///
/// **THE RULE THIS ASSERTS, which is not "constructor is last":** `constructor` is the last member
/// installed at interface-definition time, so a member a PARTIAL interface adds lands *after* it.
/// That is why the capture reads last on `Element` (152 of 152) and strictly inside on everything a
/// partial extends — `Navigator`, `Performance`, `StorageManager`, `Screen`, `Document`. An
/// implementation that hard-codes "last" is right on ~83 % of the surface and wrong on exactly the
/// interfaces a sensor reads, so THE PARTIAL FAMILY IS ASSERTED NON-EMPTY below: a constant cannot
/// pass this function, and neither can a test that only looks at the easy 83 %.
///
/// The expected position is never written down here. It is the bundle's `ctorIndex`, read through the
/// persona exactly as the engine reads it, counted in the keys THIS realm has — a member the capture
/// records and this engine does not must not push `constructor` past the end of a shorter list.
fn assert_constructor_seat(realm: Realm, now: &Surface) {
    let label = realm.label();
    let Some(surface) = shipped_surface(realm) else {
        panic!("{label}: no shipped surface to grade `constructor`'s seat against");
    };
    let mut bad = Vec::new();
    let (mut checked, mut partial, mut identical, mut unrecorded) = (0usize, 0usize, 0usize, 0usize);
    for d in realm.defs() {
        let Some(order) = surface.members_of(d.name) else { continue };
        let Some(rec) = now.get(d.name).filter(|v| v.is_object()) else { continue };
        let Some(ci) = order.ctor_index else {
            unrecorded += 1;
            continue;
        };
        let live = rec["ctorIndex"].as_i64().expect("the dumper records ctorIndex");
        let names: Vec<&str> = rec["members"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        let before: HashSet<&str> = order.members.iter().take(ci).map(String::as_str).collect();
        let want = names.iter().filter(|n| before.contains(*n)).count() as i64;
        checked += 1;
        if ci < order.members.len() {
            partial += 1;
        }
        if names.len() == order.members.len() && names.iter().zip(&order.members).all(|(a, b)| a == b) {
            identical += 1;
        }
        if live != want {
            bad.push(format!("{}: constructor at {live}, the capture seats it at {want} (ctorIndex {ci} of {} recorded members, {} here)", d.name, order.members.len(), names.len()));
        }
    }
    assert!(
        bad.is_empty(),
        "{label}: {} interfaces mount `constructor` somewhere the capture does not (first 40):\n  {}",
        bad.len(),
        bad.iter().take(40).cloned().collect::<Vec<_>>().join("\n  ")
    );
    match realm {
        Realm::Window => {
            // NON-VACUITY, BOTH HALVES. An empty comparison passes any assertion, and a comparison
            // made only of interfaces the capture puts `constructor` LAST on cannot tell a rule from
            // a constant. Both floors are far below what this tree carries (930 recorded interfaces,
            // 47 of them partial-extended on brave-151) and are here to fail if a capture stops
            // recording the position rather than to pin a number.
            assert!(checked >= 500, "[{label}] only {checked} interfaces carry a recorded `ctorIndex`; the seat below is not being graded");
            assert!(
                partial >= 20,
                "[{label}] only {partial} of {checked} interfaces have `constructor` strictly INSIDE their member list. \
                 That family is what distinguishes the rule (constructor is last at interface-definition time, partials land after it) \
                 from the observation (constructor is last); with it empty this gate would pass a hard-coded constant."
            );
            assert_eq!(unrecorded, 0, "[{label}] the window capture records a position for every interface it records");
            // THE PARTIAL FAMILY, BY NAME. These are the rows a "constructor is last" constant gets
            // wrong, so they are asserted to BE strictly inside rather than assumed to be — if a
            // re-capture ever made all four last, the `partial` floor above would still pass on the
            // rest of the surface and this gate would quietly stop discriminating.
            //
            // The six FP5 measured and left unasserted (`BarProp`, `External`, `VisualViewport`,
            // `Scheduler`, `Viewport`, `CrashReportContext`) are NOT here: half of them are hand
            // tables that this bare-runtime build does not create, so they are named in
            // `korvus-runtime/tests/persona_surface.rs`, which reads the LIVE global under all four
            // bundles and does see them.
            let mut witnesses = Vec::new();
            for iface in ["Navigator", "Performance", "StorageManager", "Screen", "Element"] {
                let order = surface.members_of(iface).unwrap_or_else(|| panic!("[{label}] the bundle records {iface}"));
                let ci = order.ctor_index.unwrap_or_else(|| panic!("[{label}] the bundle records {iface}'s ctorIndex"));
                let inside = ci > 0 && ci < order.members.len();
                assert!(
                    inside || iface == "Element",
                    "[{label}] {iface} is this gate's proof that `constructor` is not simply last: the capture must put it strictly inside its {} members, and it is at {ci}",
                    order.members.len()
                );
                if let Some(rec) = now.get(iface).filter(|v| v.is_object()) {
                    witnesses.push(format!("{iface} {}/{}", rec["ctorIndex"], rec["members"].as_array().unwrap().len()));
                }
            }
            println!("{label}: seat witnesses {}", witnesses.join(" · "));
        }
        Realm::Worker => {
            // THE GAP IS CLOSED, AND THIS IS WHAT REPLACED IT. FP12's branch here
            // counted the interfaces the worker realm could NOT grade, because `ctorIndex` was a
            // window-only field and `persona::surface` bridged it by BORROWING the window's index
            // wherever the two captures listed the same members (R22). The worker artifacts were
            // re-read from the four browsers, every interface they record carries its own seat, the
            // borrow is deleted, and the tripwire that said "turn red when that happens" did.
            //
            // So the worker realm is graded exactly as the window is, against its own capture, and
            // `unrecorded` is 0 rather than a counted remainder.
            assert!(checked >= 100, "[{label}] only {checked} worker interfaces carry a recorded `ctorIndex`; the seat above is not being graded");
            assert_eq!(unrecorded, 0, "[{label}] the worker capture records a position for every interface it records");
            assert!(
                partial >= 20,
                "[{label}] only {partial} of {checked} interfaces have `constructor` anywhere but last. \
                 That family is what distinguishes the rule (constructor is last at interface-definition time, partials land after it) \
                 from the observation (constructor is last); with it empty this gate would pass a hard-coded constant."
            );
            // THE WITNESSES ARE THE ONES NO BORROW COULD HAVE SUPPLIED, which is the property that
            // says this realm is read from its own artifact rather than from the window's. Each is
            // asserted to be a seat the window capture CANNOT give -- the interface is absent there,
            // or its member list differs, which is precisely where FP12's borrow declined -- and to
            // sit strictly inside its member list, which is where a "constructor is last" constant
            // fails. Nothing here is a literal index; both halves are read off the two captures.
            let window = shipped_surface(Realm::Window).expect("the bundle ships a window surface");
            let mut witnesses = Vec::new();
            for iface in ["WorkerNavigator", "WorkerGlobalScope", "StorageManager", "ServiceWorkerRegistration"] {
                let order = surface.members_of(iface).unwrap_or_else(|| panic!("[{label}] the worker capture records {iface}"));
                let ci = order.ctor_index.unwrap_or_else(|| panic!("[{label}] the worker capture records {iface}'s ctorIndex"));
                let borrowable = window.members_of(iface).is_some_and(|w| w.members == order.members);
                assert!(
                    !borrowable,
                    "[{label}] {iface} is one of this gate's witnesses because the window capture cannot supply its seat, \
                     and it now can -- pick a witness the two captures still describe differently"
                );
                assert!(
                    ci > 0 && ci < order.members.len(),
                    "[{label}] {iface} is this gate's proof that `constructor` is not simply last: the worker capture must put it \
                     strictly inside its {} members, and it is at {ci}",
                    order.members.len()
                );
                if let Some(rec) = now.get(iface).filter(|v| v.is_object()) {
                    witnesses.push(format!("{iface} {}/{}", rec["ctorIndex"], rec["members"].as_array().unwrap().len()));
                }
            }
            println!("{label}: seat witnesses {}", witnesses.join(" · "));
        }
    }
    println!("{label}: constructor seat matches the capture on {checked} interfaces ({partial} with it strictly inside, {identical} whose member list is the capture's exactly, {unrecorded} with no recorded position)");
}

fn check_realm(realm: Realm) {
    let (now, ledger, pre) = build(realm);
    assert_golden(realm, &now);
    assert_rows(realm, &now, &pre);
    assert_ledger(realm, &now, &ledger, &pre);
    assert_constructor_seat(realm, &now);
}

// A tokio context must exist: creating ~926 objects trips a V8 GC that posts a delayed task, and V8
// aborts the process if it posts one with no runtime to honour it (exactly as the engine boots inside
// tokio).
#[tokio::test]
async fn the_window_registry_builds_the_golden_surface_its_rows_and_its_ledger() {
    check_realm(Realm::Window);
}

#[tokio::test]
async fn the_worker_registry_builds_the_golden_surface_its_rows_and_its_ledger() {
    check_realm(Realm::Worker);
}
