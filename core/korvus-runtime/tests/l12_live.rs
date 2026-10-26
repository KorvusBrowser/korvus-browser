//! LIVE one-table gate for the ten abort / style / blob / fetch interfaces.
//!
//! Boots the real engine and asserts, for `AbortSignal`, `AbortController`, `CSSStyleDeclaration`,
//! `Blob`, `File`, `Response`, `Request`, `PerformanceEntry`, `Notification` and `AbstractRange`:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the captured bundle's member
//!      list for that interface, in the capture's order, with the capture's descriptors, followed by
//!      the non-enumerable `constructor` -- the hand-table shape.
//!   2. **The interface objects**: the capture's `Symbol.toStringTag`, `length`, `name` and statics;
//!      `new AbortSignal()` is "Illegal constructor" and `Object.getPrototypeOf(AbortSignal)` is
//!      `EventTarget`.
//!   3. **Real behaviour through the real machinery**: an `AbortController` abort that fires `onabort`
//!      exactly ONCE through dispatch beside an `addEventListener` listener, `throwIfAborted`,
//!      `AbortSignal.abort`/`timeout`/`any`, an `el.style` round trip, a `Blob`/`File`, a `fetch`ed
//!      `Response`, a `Request`, `performance.getEntries()[0].toJSON()` and a `Notification`.
//!   4. **The WORKER realm owns the same tables** (invariant I8) for the eight the worker capture lists.
//!
//! Nothing here checks a foreign receiver: these ten are JS-class-backed (the classes stay until
//! W4.b/W4.c), so unlike L7/L10 there is no brand to check -- see `install::live::install_window_abort_style`.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><p id=\"t\" style=\"color: red\">hi</p></body></html>";

/// The ten this lane's tables own in the WINDOW, in the order the installer builds them.
const IFACES: &[&str] = &[
    "AbortSignal",
    "AbortController",
    "CSSStyleDeclaration",
    "Blob",
    "File",
    "Response",
    "Request",
    "PerformanceEntry",
    "Notification",
    "AbstractRange",
];

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

fn describe_line(m: &serde_json::Value) -> String {
    let n = m["name"].as_str().unwrap();
    match m["kind"].as_str().unwrap() {
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        // A WebIDL constant: non-writable, non-configurable, enumerable.
        "value" => format!(
            "{n}:data:e={}:c={}:w=0",
            m["enumerable"].as_bool().unwrap() as u8,
            m["configurable"].as_bool().unwrap() as u8
        ),
        k => panic!("unexpected kind {k}"),
    }
}

/// The `describe_own` lines the capture's member list implies, with `constructor` at its recorded seat.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx = bundle();
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the bundle surface"))
        .clone();
    let mut out: Vec<String> = members.iter().map(describe_line).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

/// Pump the event loop until `globalThis.__done` is true (the async cases: dispatch runs on
/// setTimeout/microtask tasks), then read `globalThis.__out`.
async fn pump_for(e: &mut Engine, name: &'static str, script: &str) -> String {
    e.eval(name, script.to_string()).unwrap();
    let start = std::time::Instant::now();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        let done = e
            .eval_value("l12-done", "!!globalThis.__done".to_string())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if done || start.elapsed() > std::time::Duration::from_secs(15) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    str_of(e, "l12-out", "String(globalThis.__out)".to_string())
}

// ── 1. the ten prototypes, byte-identical to the capture ────────────────────────────────────────

#[tokio::test]
async fn every_l12_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l12-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l12_interface_object_carries_the_capture_tag_length_and_statics() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(",")].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l12-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        let want_statics: Vec<&str> = fx["interfaces"][iface]["statics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            got,
            format!("{iface}|0|0|1|{want_len}|{iface}|{}", want_statics.join(",")),
            "{iface} interface object != the capture"
        );
    }
}

#[tokio::test]
async fn the_abort_signal_interface_object_is_native_and_refuses_new() {
    let mut e = page().await;
    // WebIDL declares no constructor on AbortSignal: Chrome answers "Failed to construct
    // 'AbortSignal': Illegal constructor" (measured over loopback, 2026-09-12 -- a later change gave the
    // shared `cb_illegal_ctor` the interface name and the construction-context prefix), where the
    // deleted `op_stub_ctor` produced V8's "AbortSignal is not a constructor". The class stays the
    // internal factory, so every signal still wears this prototype and inherits from EventTarget.
    let got = str_of(
        &mut e,
        "l12-as-ctor",
        r#"(() => {
            const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
            const native = (f) => Function.prototype.toString.call(f).includes("[native code]");
            const s = new AbortController().signal;
            return [
                throws(() => new AbortSignal()),
                native(AbortSignal),
                Object.getPrototypeOf(AbortSignal) === EventTarget,
                Object.getPrototypeOf(AbortSignal.prototype) === EventTarget.prototype,
                s instanceof AbortSignal,
                s.constructor === AbortSignal,
                Object.prototype.toString.call(s),
                typeof s.addEventListener,
                Object.keys(s).length,
                ["abort","any","timeout"].every(n => native(AbortSignal[n])),
                native(Object.getOwnPropertyDescriptor(AbortSignal.prototype, "aborted").get),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "TypeError:Failed to construct 'AbortSignal': Illegal constructor|true|true|true|true|true|[object AbortSignal]|function|0|true|true",
        "the AbortSignal interface object is not the installer's"
    );
}

// ── 2. abort behaviour ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_abort_fires_onabort_exactly_once_through_dispatch() {
    let mut e = page().await;
    // `onabort` is the ONE event-handler store (W5.0b): it must run as a registered listener, beside
    // an addEventListener listener, in registration order, ONCE -- a hand-call from the fire path
    // would double-fire it.
    let got = str_of(
        &mut e,
        "l12-abort",
        r#"(() => {
            const seen = [];
            const c = new AbortController();
            const s = c.signal;
            const before = [s.aborted, String(s.reason), String(s.onabort)].join(",");
            s.addEventListener("abort", (ev) => seen.push("listener:" + ev.type + ":" + (ev.target === s)));
            s.onabort = (ev) => seen.push("onabort:" + ev.type);
            s.addEventListener("abort", () => seen.push("listener2"));
            c.abort("x");
            c.abort("y");
            const thrown = (() => { try { s.throwIfAborted(); return "no-throw"; } catch (e) { return String(e); } })();
            return [before, s.aborted, s.reason, typeof s.onabort, seen.join("|"), thrown].join(" / ");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "false,undefined,null / true / x / function / listener:abort:true|onabort:abort|listener2 / x",
        "the abort fire path"
    );
}

/// ONE expectation here changed: `AbortSignal.timeout(1.5)` was recorded as a TypeError and
/// is a real signal now. `[EnforceRange] unsigned long long` TRUNCATES a fractional value -- only NaN,
/// the infinities and an out-of-range value are TypeErrors -- and `google-chrome --headless=new`
/// 150.0.7871.46 answers `[object AbortSignal]` for it (measured with tests/fixtures/abort-probe.js).
/// The `host.js` class threw, and the expectation was the recording of that.
#[tokio::test]
async fn the_three_abort_signal_statics_still_see_the_real_argument_list() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l12-statics",
        r#"(() => {
            const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name; } };
            const a = AbortSignal.abort();
            const b = AbortSignal.abort("boom");
            const c1 = new AbortController(), c2 = new AbortController();
            c1.abort("first");
            const any = AbortSignal.any([c1.signal, c2.signal]);
            return [
                a instanceof AbortSignal, a.aborted, a.reason instanceof DOMException, a.reason.name,
                b.reason,
                throws(() => AbortSignal.timeout()),
                throws(() => AbortSignal.timeout(1.5)),  // Chrome: no throw, truncates to 1
                any instanceof AbortSignal, any.aborted, any.reason,
                AbortSignal.timeout(5) instanceof AbortSignal,
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|true|true|AbortError|boom|TypeError|no-throw|true|true|first|true",
        "the AbortSignal statics no longer reach the class"
    );
}

#[tokio::test]
async fn abort_signal_timeout_aborts_on_its_timer() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const s = AbortSignal.timeout(1);
        const seen = ["start:" + s.aborted];
        s.onabort = () => {
            seen.push("abort:" + s.aborted + ":" + (s.reason instanceof DOMException) + ":" + s.reason.name);
            globalThis.__out = seen.join("|"); globalThis.__done = true;
        };
    "#;
    let got = pump_for(&mut e, "l12-timeout", script).await;
    assert_eq!(got, "start:false|abort:true:true:TimeoutError", "AbortSignal.timeout");
}

// ── 3. CSSStyleDeclaration ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn el_style_is_the_tables_css_style_declaration() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l12-style",
        r#"(() => {
            const el = document.getElementById("t");
            const s = el.style;
            const before = s.getPropertyValue("color");
            s.setProperty("background-color", "blue");
            const cs = getComputedStyle(el);
            return [
                s instanceof CSSStyleDeclaration,
                Object.getPrototypeOf(s) === CSSStyleDeclaration.prototype,
                Object.prototype.toString.call(s),
                s === el.style,
                before,
                s.getPropertyValue("background-color"),
                s.cssText.includes("background-color"),
                s.length >= 2,
                typeof s.item(0),
                s.removeProperty("background-color"),
                s.getPropertyValue("background-color"),
                String(s.parentRule),
                cs instanceof CSSStyleDeclaration,
                (() => { try { new CSSStyleDeclaration(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } })(),
                (() => { try { s.setProperty(); return "no-throw"; } catch (e) { return e.constructor.name; } })(),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|true|[object CSSStyleDeclaration]|true|red|blue|true|true|string|blue|\
|null|true|TypeError:Failed to construct 'CSSStyleDeclaration': Illegal constructor|TypeError",
        "el.style is not the table's CSSStyleDeclaration"
    );
}

// ── 4. Blob / File ──────────────────────────────────────────────────────────────────────────────

/// Nothing here was renamed and ONE expectation changed: `Object.keys(b)` was `_bytes,_type`
/// while `Blob` was a JS class keeping its state in own properties, and it is `""` now that the
/// constructor is native and the state is in v8 privates. Chrome answers `[]` there
/// (`Object.getOwnPropertyNames(new Blob(["a"]))`, measured on `google-chrome --headless=new`
/// 150.0.7871.46), so the engine was right and the expectation was the recording of a defect.
#[tokio::test]
async fn blob_and_file_read_their_class_internals_through_the_table() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const b = new Blob(["ab"], { type: "TEXT/PLAIN" });
        const f = new File(["xy"], "n.txt", { lastModified: 1234 });
        const parts = [
            b.size, b.type, b.slice(1).size, f.size, f.name, f.lastModified,
            f.lastModifiedDate instanceof Date, f.webkitRelativePath === "",
            f instanceof Blob, Object.prototype.toString.call(b),
            Object.getPrototypeOf(File.prototype) === Blob.prototype,
            Object.keys(b).join(","),
        ];
        b.text().then((t) => {
            parts.push("text:" + t);
            globalThis.__out = parts.join("|"); globalThis.__done = true;
        }, (err) => { globalThis.__out = "REJECTED:" + err; globalThis.__done = true; });
    "#;
    let got = pump_for(&mut e, "l12-blob", script).await;
    assert_eq!(
        got,
        "2|text/plain|1|2|n.txt|1234|true|true|true|[object Blob]|true||text:ab",
        "Blob / File over the class internals"
    );
}

// ── 5. Response / Request ───────────────────────────────────────────────────────────────────────

/// TWO expectations here changed, both because CHROME says so (measured with
/// `tests/fixtures/fetch-types-probe.js` on `google-chrome --headless=new` 150.0.7871.46):
/// `new Response(...).type` is `"default"` for a CONSTRUCTED response — `"basic"` is what a
/// same-origin fetch produces, and the native constructor still answers that for the internal
/// `op_fetch` result — and `Object.keys(r)` is `""` because the body is a v8 private now rather than
/// the own `__body` the JS class left on every instance (Chrome's `getOwnPropertyNames` is `[]`).
///
/// A later change made a THIRD: `String(r.body)` for `new Response('{"k":1}')` is
/// `[object ReadableStream]` in Chrome 150 (measured: `tests/fixtures/bodies-probe.chrome-150.txt`,
/// "Response body tag, identity"), not the `null` the engine answered because `body` was installed as
/// a null getter. A GET `Request` has no body, so `String(q.body)` stays `null` (same fixture,
/// "Request body tag").
#[tokio::test]
async fn a_response_and_a_request_read_their_slots_through_the_table() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const r = new Response('{"k":1}', { status: 201, statusText: "Created" });
        const q = new Request("http://example.test/x", { method: "POST" });
        const parts = [
            r.status, r.statusText, r.ok, r.type, r.redirected, r.bodyUsed, String(r.body),
            Object.prototype.toString.call(r), Object.keys(r).join(","),
            q.url, q.method, q.credentials, q.mode, q.cache, q.redirect, q.referrer, String(q.body),
            q.clone() instanceof Request, q.clone().method,
            Object.prototype.toString.call(q),
        ];
        r.json().then((j) => {
            parts.push("json:" + j.k);
            globalThis.__out = parts.join("|"); globalThis.__done = true;
        }, (err) => { globalThis.__out = "REJECTED:" + err; globalThis.__done = true; });
    "#;
    let got = pump_for(&mut e, "l12-fetch", script).await;
    assert_eq!(
        got,
        "201|Created|true|default|false|false|[object ReadableStream]|[object Response]||\
http://example.test/x|POST|same-origin|cors|default|follow|about:client|null|true|POST|[object Request]|json:1",
        "Response / Request over their slots"
    );
}

// ── 6. PerformanceEntry / Notification / AbstractRange ──────────────────────────────────────────

#[tokio::test]
async fn a_performance_entry_serialises_through_the_table() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l12-perf",
        r#"(() => {
            performance.mark("l12");
            const m = performance.getEntriesByName("l12")[0];
            const j = m.toJSON();
            return [
                m instanceof PerformanceEntry, m.name, m.entryType, typeof m.startTime, m.duration,
                // Chrome 152: the document's navigation id, a number from 100 to 10000 (random per document)
                m.navigationId >= 100 && m.navigationId <= 10000, Object.keys(m).length,
                Object.keys(j).join(","), j.name, j.entryType,
                (() => { try { new PerformanceEntry(); return "no-throw"; } catch (e) { return e.constructor.name; } })(),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|l12|mark|number|0|true|0|name,entryType,startTime,duration,navigationId|l12|mark|TypeError",
        "PerformanceEntry.toJSON"
    );
}

#[tokio::test]
async fn a_notification_is_a_shape_with_honest_stubs() {
    let mut e = page().await;
    let script = r#"
        globalThis.__out = "pending"; globalThis.__done = false;
        const n = new Notification("t", { body: "b", tag: "g", silent: true });
        const parts = [
            n instanceof Notification, n instanceof EventTarget,
            Object.prototype.toString.call(n), Object.keys(n).length,
            n.title, n.body, n.tag, n.dir, n.silent, n.requireInteraction,
            n.actions.length, typeof n.timestamp,
            String(n.onclick), typeof n.close, String(n.close()),
            Notification.permission, Notification.maxActions,
        ];
        // The request opens a prompt nobody answers: Brave 151 and Chrome 150 leave it PENDING
        // (2026-10-01), so after a task it has still not settled.
        let settled = "pending";
        Notification.requestPermission().then((p) => { settled = "resolved:" + p; }, () => { settled = "rejected"; });
        setTimeout(() => {
            parts.push("perm:" + settled);
            globalThis.__out = parts.join("|"); globalThis.__done = true;
        }, 50);
    "#;
    let got = pump_for(&mut e, "l12-notif", script).await;
    assert_eq!(
        got,
        "true|true|[object Notification]|0|t|b|g|auto|true|false|0|number|null|function|undefined|default|2|perm:pending",
        "the Notification shape"
    );
}

#[tokio::test]
async fn a_static_range_answers_collapsed_through_the_table() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l12-range",
        r#"(() => {
            const p = document.getElementById("t");
            const t = p.firstChild;
            const a = new StaticRange({ startContainer: t, startOffset: 0, endContainer: t, endOffset: 0 });
            const b = new StaticRange({ startContainer: t, startOffset: 0, endContainer: t, endOffset: 2 });
            const d = Object.getOwnPropertyDescriptor(AbstractRange.prototype, "collapsed");
            return [
                a instanceof AbstractRange, a.collapsed, b.collapsed,
                a.startContainer === t, a.endOffset, b.endOffset,
                d.get.name, d.get.length, !!d.set, d.enumerable, d.configurable,
                Function.prototype.toString.call(d.get).includes("[native code]"),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|true|false|true|0|2|get collapsed|0|false|true|true|true",
        "AbstractRange.collapsed"
    );
}

#[tokio::test]
async fn the_registry_children_of_the_ten_still_chain_to_them() {
    let mut e = page().await;
    // `install_registry`'s chain pass runs during the ESM layer and skips a parent the global does
    // not name yet. Nine of the ten still have a JS class, so it found the very prototype the table
    // then takes over. `CSSStyleDeclaration` has none -- its two registry children were left on
    // `Object.prototype` until `install::live::rechain_orphans` put them back.
    let got = str_of(
        &mut e,
        "l12-chains",
        r#"(() => {
            const P = Object.getPrototypeOf;
            return [
                P(CSSPositionTryDescriptors.prototype) === CSSStyleDeclaration.prototype,
                P(CSSFunctionDescriptors.prototype) === CSSStyleDeclaration.prototype,
                P(PerformanceMark.prototype) === PerformanceEntry.prototype,
                P(PerformanceMeasure.prototype) === PerformanceEntry.prototype,
                P(PerformanceResourceTiming.prototype) === PerformanceEntry.prototype,
                P(Range.prototype) === NodeRange.prototype,
                P(StaticRange.prototype) === NodeRange.prototype,
                P(TaskSignal.prototype) === AbortSignal.prototype,
                P(TaskController.prototype) === AbortController.prototype,
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "true|true|true|true|true|true|true|true|true", "a registry child lost its table parent");
}

// ── 7. the worker realm ─────────────────────────────────────────────────────────────────────────

async fn worker_probe(worker_body: &str) -> String {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_document(PAGE).unwrap();
    let reg = format!(
        "globalThis.__workerScripts = {{ './probe.js': {} }};",
        serde_json::to_string(worker_body).unwrap()
    );
    e.eval("worker-scripts", reg).unwrap();
    e.run().await.unwrap();
    e.eval(
        "driver",
        "const w = new Worker('./probe.js'); w.onmessage = (ev) => print('RESULT:' + ev.data);".into(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let mut out = Vec::new();
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), e.run()).await;
        out.extend(e.take_output());
        if out.iter().any(|l| l.starts_with("RESULT:")) {
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(20) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    out.iter()
        .find(|l| l.starts_with("RESULT:"))
        .map(|l| l["RESULT:".len()..].to_string())
        .unwrap_or_else(|| format!("NONE (output: {out:?})"))
}

#[tokio::test]
async fn the_worker_realm_owns_the_same_tables() {
    // The worker's eight come from the SAME tables (invariant I8): same member order, same chains,
    // same `Symbol.toStringTag`. `CSSStyleDeclaration` and `AbstractRange` are NOT there --
    // `worker.surface.json` has neither, and the bundle is the denominator. `Notification` walks the
    // worker SELECTOR, so `requestPermission` (`[Exposed=Window]`) is absent here too.
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const names = ["AbortSignal","AbortController","Blob","File","Response","Request",
                       "PerformanceEntry","Notification"];
        const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name; } };
        const parts = [
            names.every((n) => typeof globalThis[n] === "function"),
            names.every((n) => globalThis[n].prototype[Symbol.toStringTag] === n),
            desc(AbortSignal.prototype),
            desc(Blob.prototype),
            desc(File.prototype),
            Object.getPrototypeOf(AbortSignal.prototype) === EventTarget.prototype,
            Object.getPrototypeOf(File.prototype) === Blob.prototype,
            Object.getPrototypeOf(Notification.prototype) === EventTarget.prototype,
            throws(() => new AbortSignal()),
            Object.getOwnPropertyNames(Notification).filter(n => !["length","name","prototype"].includes(n)).join(","),
            typeof globalThis.CSSStyleDeclaration,
            typeof globalThis.AbstractRange,
            new Response("hi").status,
            Object.getPrototypeOf(TaskSignal.prototype) === AbortSignal.prototype,
            Object.getPrototypeOf(TaskController.prototype) === AbortController.prototype,
            Object.getPrototypeOf(PerformanceMark.prototype) === PerformanceEntry.prototype,
        ];
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    let sig = "aborted:a:s=0,reason:a:s=0,onabort:a:s=1,throwIfAborted:d:w=1,constructor:d:w=1";
    let blob = "size:a:s=0,type:a:s=0,arrayBuffer:d:w=1,slice:d:w=1,stream:d:w=1,text:d:w=1,bytes:d:w=1,textStream:d:w=1,constructor:d:w=1";
    let file = "name:a:s=0,lastModified:a:s=0,lastModifiedDate:a:s=0,webkitRelativePath:a:s=0,constructor:d:w=1";
    assert_eq!(
        got,
        format!("true|true|{sig}|{blob}|{file}|true|true|true|TypeError|permission,maxActions|undefined|undefined|200|true|true|true"),
        "the worker realm's L12 interfaces are not the window's tables"
    );
}
