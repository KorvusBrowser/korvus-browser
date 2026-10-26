//! LIVE one-table gate for the six observer interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle, that the six `interface!`
//! tables reproduce -- byte- and value-identically -- what the deleted observer blocks of
//! `dom/08g-observers.js`, the two `globalThis.X =` lines of `dom/00-bootstrap.js` and
//! `worker-host.js`'s DUPLICATE PerformanceObserver installer used to build:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` last.
//!   2. **The interface objects.** The capture's `Symbol.toStringTag`, `length`, `name` and static
//!      list; NEWABILITY straight out of `surface.json` (all six record `"newable": false`, i.e. a
//!      zero-argument `new` throws); and every constructor reads `[native code]` -- the point of
//!      retiring the two `op_nativize_ctor` masks.
//!   3. **`PerformanceObserver.supportedEntryTypes`** is the capture's thirteen, from the row that
//!      replaced the `op_nativize` mask over a JS arrow.
//!   4. **A real delivery**: `new PerformanceObserver(fn)`, `observe({entryTypes:["mark"]})`,
//!      `performance.mark("x")`, and the callback receiving a `PerformanceObserverEntryList` -- an
//!      instance of the table's interface, wearing the table's prototype -- whose `getEntries()` has
//!      the mark.
//!   5. **An IntersectionObserver and a ResizeObserver** constructed, observed, unobserved and
//!      disconnected, with `root` / `thresholds` / `rootMargin` / `scrollMargin` / `delay` /
//!      `trackVisibility` read back, and the bundle's oracle answers for `observe()` / `takeRecords()`.
//!   6. **Instances wear the TABLE's prototype**, not the cppgc class template's -- the brief's
//!      failure mode (1).
//!   7. **The WORKER realm owns the same PerformanceObserver TABLE** (invariant I8) -- not a second
//!      install -- and its own timeline still delivers.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=t></div></body></html>";

/// The six this lane's tables own.
const IFACES: &[&str] = &[
    "PerformanceObserver",
    "PerformanceObserverEntryList",
    "IntersectionObserver",
    "IntersectionObserverEntry",
    "ResizeObserver",
    "ResizeObserverEntry",
];

/// The two the WORKER capture lists.
const WORKER_IFACES: &[&str] = &["PerformanceObserver", "PerformanceObserverEntryList"];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    e
}

fn bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap()
}

fn worker_bundle() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("worker-surface")).unwrap()).unwrap()
}

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        k => panic!("unexpected kind {k}"),
    }
}

/// The seat this REALM'S PERSONA SURFACE gives `iface`, which is the engine's own source rather than
/// a file this test picks. Both realms' artifacts record `ctorIndex` since the
/// worker surfaces were re-read from the browsers; before that the worker's came from a cross-realm
/// borrow, and this function read through the persona so that it followed whichever answer the
/// engine was using. It still does, which is the point: one source, both realms.
fn seat(realm: spec::SurfaceRealm, iface: &str, n: usize) -> usize {
    use spec::Persona;
    persona::CapturedPersona::default()
        .surface(realm)
        .and_then(|s| s.members_of(iface).and_then(|o| o.ctor_index))
        .unwrap_or(n)
        .min(n)
}

fn expected_proto(fx: &serde_json::Value, iface: &str, realm: spec::SurfaceRealm) -> Vec<String> {
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = seat(realm, iface, out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// Same, but for an expression that evaluates to a promise (the delivery cases are async).
async fn str_of_async(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value_settled(name, src, std::time::Duration::from_secs(10))
        .await
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

// ── 1. the six prototypes, byte-identical to the capture ────────────────────────────────────────

#[tokio::test]
async fn every_l11c_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l11c-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(&fx, iface, spec::SurfaceRealm::Window), "{iface}.prototype != the capture");
    }
}

// ── 2. the interface objects: tag, length, name, statics, newability, native-ness ───────────────

#[tokio::test]
async fn every_l11c_interface_object_matches_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                let newable = "yes";
                try {{ new C(); }} catch (err) {{ newable = "throws"; }}
                const native = /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(C));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name,
                        statics.join(","), newable, native,
                        Object.getPrototypeOf(C.prototype) === Object.prototype].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l11c-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        let want_statics: Vec<&str> = fx["interfaces"][iface]["statics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        // Every one of the six records `newable: false` -- each real constructor requires a callback,
        // and the three entry/list interfaces have none at all.
        assert!(!fx["interfaces"][iface]["newable"].as_bool().unwrap(), "{iface}: the capture changed");
        let want = format!(
            "{iface}|0|0|1|{want_len}|{iface}|{}|throws|true|true",
            want_statics.join(",")
        );
        assert_eq!(got, want, "{iface}: the interface object != the capture");
    }
}

// ── 3. every member is [native code] ────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_l11c_member_is_native() {
    let mut e = page().await;
    let list = IFACES.join(",");
    let src = format!(
        r#"(() => {{
            const bad = [];
            for (const iface of "{list}".split(",")) {{
                const C = globalThis[iface];
                for (const [holder, tag] of [[C.prototype, "proto"], [C, "static"]]) {{
                    for (const n of Object.getOwnPropertyNames(holder)) {{
                        if (["length","name","prototype","constructor"].includes(n)) continue;
                        const d = Object.getOwnPropertyDescriptor(holder, n);
                        for (const f of [d.value, d.get, d.set]) {{
                            if (typeof f !== "function") continue;
                            if (!/\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f))) bad.push(iface + "." + n + ":" + tag);
                        }}
                    }}
                }}
            }}
            return bad.join(",");
        }})()"#
    );
    assert_eq!(str_of(&mut e, "l11c-native", src), "", "a member is not [native code]");
}

// ── 4. PerformanceObserver: the static, the constructor check, and a real delivery ──────────────

#[tokio::test]
async fn performance_observer_supported_entry_types_is_the_capture() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11c-set",
        r#"(() => {
            const t = PerformanceObserver.supportedEntryTypes;
            const d = Object.getOwnPropertyDescriptor(PerformanceObserver, "supportedEntryTypes");
            return t.join(",") + "|" + Array.isArray(t) + "|" + Object.isFrozen(t) + "|" +
                   (+d.enumerable) + (+d.configurable) + (typeof d.set) + "|" + d.get.name + "|" + d.get.length;
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "element,event,first-input,interaction-contentful-paint,largest-contentful-paint,layout-shift,\
long-animation-frame,longtask,mark,measure,navigation,paint,resource,soft-navigation,visibility-state\
|true|true|11undefined|get supportedEntryTypes|0"
    );
}

#[tokio::test]
async fn performance_observer_constructor_is_the_rust_one() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11c-po-ctor",
        r#"(() => {
            const out = [];
            try { new PerformanceObserver(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name + ":" + e.message); }
            const o = new PerformanceObserver(() => {});
            out.push(Object.getPrototypeOf(o) === PerformanceObserver.prototype);
            out.push(o instanceof PerformanceObserver);
            out.push(Object.prototype.toString.call(o));
            // The bundle's oracle: observe() with no options is a TypeError, disconnect() is
            // undefined, takeRecords() is [].
            try { o.observe(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            out.push(o.disconnect() === undefined);
            out.push(JSON.stringify(o.takeRecords()));
            out.push(PerformanceObserver.prototype.observe.length);
            // No foreign receiver: every row unwraps the cppgc brand.
            try { PerformanceObserver.prototype.takeRecords.call({}); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            return out.join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "TypeError:Failed to construct 'PerformanceObserver': 1 argument required, but only 0 present.\
|true|true|[object PerformanceObserver]|TypeError|true|[]|0|TypeError"
    );
}

#[tokio::test]
async fn a_performance_observer_receives_a_real_entry_list() {
    let mut e = page().await;
    let got = str_of_async(
        &mut e,
        "l11c-deliver",
        r#"new Promise((resolve) => {
            const o = new PerformanceObserver((list, obs) => {
                const es = list.getEntries();
                resolve([
                    Object.prototype.toString.call(list),
                    list instanceof PerformanceObserverEntryList,
                    Object.getPrototypeOf(list) === PerformanceObserverEntryList.prototype,
                    Object.getOwnPropertyNames(list).length,
                    es.map(x => x.entryType + ":" + x.name).join(","),
                    list.getEntriesByType("mark").length,
                    list.getEntriesByType("measure").length,
                    list.getEntriesByName("l11c-mark").length,
                    list.getEntriesByName("nope").length,
                    obs === o,
                ].join("|"));
            });
            o.observe({ entryTypes: ["mark"] });
            performance.mark("l11c-mark");
        })"#
            .to_string(),
    )
    .await;
    assert_eq!(got, "[object PerformanceObserverEntryList]|true|true|0|mark:l11c-mark|1|0|1|0|true");
}

// ── 5. IntersectionObserver / ResizeObserver: the constructors and the oracle's answers ─────────

#[tokio::test]
async fn the_intersection_observer_is_the_table() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11c-io",
        r#"(() => {
            const out = [];
            try { new IntersectionObserver(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name + ":" + e.message); }
            const o = new IntersectionObserver(() => {});
            out.push(Object.getPrototypeOf(o) === IntersectionObserver.prototype);
            out.push(o.root === null);
            out.push(o.rootMargin);
            out.push(o.scrollMargin);
            out.push(JSON.stringify(o.thresholds));
            out.push(o.delay);
            out.push(o.trackVisibility);
            const t = document.getElementById("t");
            out.push(o.observe(t) === undefined);
            out.push(o.unobserve(t) === undefined);
            out.push(o.disconnect() === undefined);
            out.push(JSON.stringify(o.takeRecords()));
            try { o.observe(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            try { o.unobserve(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            try { IntersectionObserver.prototype.disconnect.call({}); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            // The root the options carried, and the four-component margin.
            const r = new IntersectionObserver(() => {}, { root: t, rootMargin: "10px" });
            out.push(r.root === t);
            out.push(r.rootMargin);
            return out.join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "TypeError:Failed to construct 'IntersectionObserver': 1 argument required, but only 0 present.\
|true|true|0px 0px 0px 0px|0px 0px 0px 0px|[0]|0|false|true|true|true|[]|TypeError|TypeError|TypeError\
|true|10px 10px 10px 10px"
    );
}

#[tokio::test]
async fn the_resize_observer_is_the_table() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l11c-ro",
        r#"(() => {
            const out = [];
            try { new ResizeObserver(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name + ":" + e.message); }
            const o = new ResizeObserver(() => {});
            out.push(Object.getPrototypeOf(o) === ResizeObserver.prototype);
            out.push(Object.prototype.toString.call(o));
            const t = document.getElementById("t");
            out.push(o.observe(t) === undefined);
            out.push(o.unobserve(t) === undefined);
            out.push(o.disconnect() === undefined);
            try { o.observe(); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            try { ResizeObserver.prototype.observe.call({}, t); out.push("no-throw"); } catch (e) { out.push(e.constructor.name); }
            // The three entry/list interfaces have no constructor at all.
            for (const n of ["ResizeObserverEntry", "IntersectionObserverEntry", "PerformanceObserverEntryList"]) {
                try { new globalThis[n](); out.push("no-throw"); } catch (e) { out.push(e.constructor.name + ":" + e.message); }
            }
            return out.join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        // The three non-newable ones carry Chrome's construction context, per interface.
        "TypeError:Failed to construct 'ResizeObserver': 1 argument required, but only 0 present.\
|true|[object ResizeObserver]|true|true|true|TypeError|TypeError\
|TypeError:Failed to construct 'ResizeObserverEntry': Illegal constructor\
|TypeError:Failed to construct 'IntersectionObserverEntry': Illegal constructor\
|TypeError:Failed to construct 'PerformanceObserverEntryList': Illegal constructor"
    );
}

// ── 6. the WORKER realm owns the same PerformanceObserver TABLE ─────────────────────────────────

#[tokio::test]
async fn the_worker_realm_owns_the_same_performance_observer_table() {
    let fxw = worker_bundle();
    let mut expected: Vec<String> = Vec::new();
    for iface in WORKER_IFACES {
        expected.push(format!("{iface}\n{}", expected_proto(&fxw, iface, spec::SurfaceRealm::Worker).join("\n")));
    }
    let list = WORKER_IFACES.join(",");
    // The worker source is registered as a script, NOT built from a JS template literal in the page
    // realm: `DESCRIBE` carries `${...}` of its own, which such a literal would interpolate before the
    // worker ever saw it. A real worker isolate runs cross-thread, so the page is pumped until it
    // posts (the `l11b_live.rs` pattern).
    let worker_src = format!(
        r#"{DESCRIBE}
const out = [];
for (const iface of "{list}".split(",")) out.push(iface + "\n" + desc(globalThis[iface].prototype));
out.push("static=" + PerformanceObserver.supportedEntryTypes.length);
const o = new PerformanceObserver((l, ob) => {{
  out.push("delivered=" + Object.prototype.toString.call(l) + ":" + l.getEntries().map(x => x.name).join(",") +
           ":" + (l instanceof PerformanceObserverEntryList) + ":" + (ob === o));
  postMessage(out.join("\n@@\n"));
}});
o.observe({{ entryTypes: ["mark"] }});
performance.mark("wmark");
"#
    );

    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document(PAGE).unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './l11c.js': {} }};",
        serde_json::to_string(&worker_src).unwrap()
    );
    e.eval("l11c-worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "l11c-worker-driver",
        "const w = new Worker('./l11c.js'); w.onmessage = (ev) => print('L11C:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::new();
    let got = loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if let Some(l) = out.iter().find(|l| l.starts_with("L11C:")) {
            break l["L11C:".len()..].to_string();
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(20), "the worker did not post within 20s; output: {out:?}");
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    let mut want = expected.join("\n@@\n");
    // A worker lists the entry types ITS realm can produce -- mark, measure, resource (Chrome 150 headless,
    // measured 2026-10-07: `PerformanceObserver.supportedEntryTypes` in a dedicated worker is
    // ["mark","measure","resource"]; the worker capture has only those three entry interfaces). This
    // pinned 15, the window's Brave list the worker used to answer (korvus-analysis U-4).
    want.push_str("\n@@\nstatic=3");
    want.push_str("\n@@\ndelivered=[object PerformanceObserverEntryList]:wmark:true:true");
    assert_eq!(got, want, "the worker realm's two != the worker capture");
}
