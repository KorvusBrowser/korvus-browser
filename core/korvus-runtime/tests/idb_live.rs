//! LIVE one-table gate for the twelve IndexedDB interfaces.
//!
//! Boots the real engine and asserts, for `IDBFactory`, `IDBRequest`, `IDBOpenDBRequest`,
//! `IDBTransaction`, `IDBDatabase`, `IDBObjectStore`, `IDBIndex`, `IDBCursor`, `IDBCursorWithValue`,
//! `IDBKeyRange`, `IDBRecord` and `IDBVersionChangeEvent`:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the captured bundle's member
//!      list for that interface, in the capture's order, with the capture's descriptors, followed by the
//!      non-enumerable `constructor` -- the hand-table shape. Headless Chrome 150.0.7871.46 on this
//!      machine confirmed those orders member-for-member on 2026-09-10, `constructor` last on each.
//!   2. **The chains and the interface objects.** `IDBRequest`/`IDBTransaction`/`IDBDatabase` ->
//!      `EventTarget`, `IDBOpenDBRequest` -> `IDBRequest`, `IDBCursorWithValue` -> `IDBCursor`,
//!      `IDBVersionChangeEvent` -> `Event`, prototype chain AND constructor chain; every interface
//!      object is a native "Illegal constructor" thrower except `IDBVersionChangeEvent`, whose
//!      constructor is real.
//!   3. **A receiver check on every member.** The bundle's oracle records `TypeError` for a getter or
//!      method entered with `Object.create(Iface.prototype)`; before the cutover they answered
//!      `undefined`.
//!   4. **The whole round trip**, through the real dispatch machinery: open -> upgradeneeded
//!      (createObjectStore / createIndex / rename) -> put -> get / getKey / getAll / getAllRecords /
//!      count -> an openCursor walk -> delete / clear -> transaction complete, with `event.target` and
//!      `this` as Chrome answers them.
//!   5. **The WORKER realm owns the same twelve interface objects**, from the same tables (invariant
//!      I8) -- where the deleted worker shells gave them stub bodies.
//!
//! Every Chrome-sourced expectation below was read from headless Chrome on this machine (2026-09-10);
//! the values are quoted at their assertions.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The twelve, in the order the installer builds them.
const IFACES: &[&str] = &[
    "IDBFactory",
    "IDBRequest",
    "IDBOpenDBRequest",
    "IDBTransaction",
    "IDBDatabase",
    "IDBObjectStore",
    "IDBIndex",
    "IDBCursor",
    "IDBCursorWithValue",
    "IDBKeyRange",
    "IDBRecord",
    "IDBVersionChangeEvent",
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

/// The `describe_own` lines the capture's member list implies, with `constructor` at its recorded seat.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx = bundle();
    let members = fx["interfaces"][iface]["members"]
        .as_array()
        .unwrap_or_else(|| panic!("{iface} absent from the bundle surface"))
        .clone();
    let mut out: Vec<String> = members
        .iter()
        .map(|m| {
            let n = m["name"].as_str().unwrap();
            match m["kind"].as_str().unwrap() {
                "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
                "method" => format!("{n}:data:e=1:c=1:w=1"),
                k => panic!("unexpected kind {k}"),
            }
        })
        .collect();
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

// ── 1. the twelve prototypes ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_indexeddb_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "idb-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_indexeddb_interface_object_carries_the_capture_statics_tag_and_length() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        // `Symbol.toStringTag` on the prototype, the interface object's `length`, and its statics.
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(",")].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "idb-ctor", src);
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
    // The four `IDBKeyRange` statics carry the capture's WebIDL lengths, which the deleted JS glue got
    // wrong for three of them (it passed 4/2/2). Chrome 2026-09-10: bound 2, the other three 1.
    let got = str_of(
        &mut e,
        "idb-kr-len",
        r#"["bound","lowerBound","only","upperBound"].map(n => n + "=" + IDBKeyRange[n].length).join(",")"#.to_string(),
    );
    assert_eq!(got, "bound=2,lowerBound=1,only=1,upperBound=1");
}

// ── 2. the chains, the interface objects, the global ────────────────────────────────────────────

#[tokio::test]
async fn the_chains_and_the_interface_objects_are_chromes() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "idb-chains",
        r#"(() => {
            const P = Object.getPrototypeOf;
            const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
            const native = (f) => Function.prototype.toString.call(f).includes("[native code]");
            return [
                P(IDBRequest.prototype) === EventTarget.prototype,
                P(IDBTransaction.prototype) === EventTarget.prototype,
                P(IDBDatabase.prototype) === EventTarget.prototype,
                P(IDBOpenDBRequest.prototype) === IDBRequest.prototype,
                P(IDBCursorWithValue.prototype) === IDBCursor.prototype,
                P(IDBVersionChangeEvent.prototype) === Event.prototype,
                P(IDBRequest) === EventTarget,
                P(IDBOpenDBRequest) === IDBRequest,
                P(IDBCursorWithValue) === IDBCursor,
                P(IDBVersionChangeEvent) === Event,
                throws(() => new IDBRequest()),
                throws(() => new IDBFactory()),
                throws(() => new IDBRecord()),
                throws(() => new IDBVersionChangeEvent()),
                ["IDBFactory","IDBRequest","IDBOpenDBRequest","IDBTransaction","IDBDatabase","IDBObjectStore",
                 "IDBIndex","IDBCursor","IDBCursorWithValue","IDBKeyRange","IDBRecord","IDBVersionChangeEvent"]
                    .every(n => native(globalThis[n])),
                indexedDB instanceof IDBFactory,
                Object.prototype.toString.call(indexedDB),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        // The three non-newable ones carry Chrome's construction context too, which is
        // what made the fourth line's prefix look like a special case rather than the rule.
        "true|true|true|true|true|true|true|true|true|true|\
TypeError:Failed to construct 'IDBRequest': Illegal constructor|\
TypeError:Failed to construct 'IDBFactory': Illegal constructor|\
TypeError:Failed to construct 'IDBRecord': Illegal constructor|\
TypeError:Failed to construct 'IDBVersionChangeEvent': 1 argument required, but only 0 present.|\
true|true|[object IDBFactory]",
        "the IndexedDB chains / interface objects are not Chrome's"
    );
}

// ── 3. receiver checks ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    // The bundle's oracle records `TypeError` for each of these (`.get.call(Object.create(P))` and
    // `P.method.call(Object.create(P), ...)`). Before the cutover they answered `undefined`.
    let got = str_of(
        &mut e,
        "idb-recv",
        r#"(() => {
            const bad = [];
            // a promise member refuses by REJECTING (WebIDL, Chrome): `databases()`, whose
            // receiver check is the brand; the rejection itself is the lock's and F-object-create-receivers'
            const promised = [];
            const probe = (label, f) => {
                let v;
                try { v = f(); } catch (e) { if (!(e instanceof TypeError)) bad.push(label + ":" + e.name); return; }
                if (v && typeof v.then === "function") { promised.push(label); v.catch(() => {}); return; }
                bad.push(label + ":no-throw");
            };
            const IF = { IDBRequest, IDBOpenDBRequest, IDBTransaction, IDBDatabase, IDBObjectStore, IDBIndex,
                         IDBCursor, IDBCursorWithValue, IDBKeyRange, IDBRecord, IDBVersionChangeEvent, IDBFactory };
            for (const name in IF) {
                const C = IF[name], P = C.prototype, foreign = Object.create(P);
                for (const m of Object.getOwnPropertyNames(P)) {
                    if (m === "constructor") continue;
                    const d = Object.getOwnPropertyDescriptor(P, m);
                    if (d.get) probe(name + "." + m + ".get", () => d.get.call(foreign));
                    if (d.set) probe(name + "." + m + ".set", () => d.set.call(foreign, null));
                    if (typeof d.value === "function") probe(name + "." + m + "()", () => d.value.call(foreign, "x", "y"));
                }
            }
            return bad.join(",") + "|promised=" + promised.join(",");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "|promised=IDBFactory.databases()", "a member answered instead of throwing for a foreign receiver");
}

// ── 4. IDBKeyRange and IDBVersionChangeEvent ────────────────────────────────────────────────────

#[tokio::test]
async fn key_range_and_version_change_event_answer_chromes_values() {
    let mut e = page().await;
    // Chrome 2026-09-10: only(1) -> lower/upper 1, both closed, includes(1) true;
    // bound(1,5,true).includes(5) is TRUE (the third argument is lowerOpen) and
    // bound(1,5,true,true).includes(5) is false.
    let got = str_of(
        &mut e,
        "idb-kr",
        r#"(() => {
            const o = IDBKeyRange.only(1);
            return [Object.prototype.toString.call(o), o.lower, o.upper, o.lowerOpen, o.upperOpen, o.includes(1),
                    IDBKeyRange.bound(1, 5, true).includes(5),
                    IDBKeyRange.bound(1, 5, true, true).includes(5),
                    IDBKeyRange.lowerBound(1, true).lowerOpen,
                    IDBKeyRange.upperBound(1).upperOpen,
                    o instanceof IDBKeyRange].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "[object IDBKeyRange]|1|1|false|false|true|true|false|true|false|true");

    // Chrome 2026-09-10: a fresh IDBVersionChangeEvent is oldVersion 0 / newVersion null /
    // dataLoss "none" / dataLossMessage ""; the init's EventInit half is DROPPED (bubbles stays
    // false) while its own members are read; the instance's only own property is `isTrusted`;
    // a subclass keeps its own prototype; an invalid `dataLoss` is a TypeError.
    let got = str_of(
        &mut e,
        "idb-vce",
        r#"(() => {
            const T = (v) => Object.prototype.toString.call(v);
            const a = new IDBVersionChangeEvent("x");
            const b = new IDBVersionChangeEvent("v", { oldVersion: 1, newVersion: 2, bubbles: true, dataLoss: "total" });
            class M extends IDBVersionChangeEvent {}
            const m = new M("z");
            let enumErr = "no-throw";
            try { new IDBVersionChangeEvent("v", { dataLoss: "bogus" }); } catch (e) { enumErr = e.constructor.name; }
            return [T(a), a.oldVersion, String(a.newVersion), a.dataLoss, JSON.stringify(a.dataLossMessage),
                    a.type, a.bubbles, a.isTrusted, Object.getOwnPropertyNames(a).join("+"),
                    Object.getPrototypeOf(a) === IDBVersionChangeEvent.prototype,
                    b.oldVersion, b.newVersion, b.bubbles, b.dataLoss,
                    Object.getPrototypeOf(m) === M.prototype, T(m), m.oldVersion,
                    enumErr, IDBVersionChangeEvent.length].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "[object IDBVersionChangeEvent]|0|null|none|\"\"|x|false|false|isTrusted|true|1|2|false|total|true|[object IDBVersionChangeEvent]|0|TypeError|1"
    );
}

/// REQUEST EVENTS ARE TASKS, AND THE ACTIVE FLAG OUTLIVES THE CHECKPOINT.
///
/// This is the regression gate for the scheduling defect that made
/// `IndexedDB/event-dispatch-active-flag.any.js` run forever. Two properties, both spec, both broken
/// before:
///
/// 1. **A success handler that starts another request does not starve the task queue.** IndexedDB
///    says "queue a task" to fire a request's event; Korvus queued a MICROTASK, and V8 drains
///    microtasks to exhaustion before returning to the task queue — so `spin()` below, which re-arms a
///    request from its own success handler (this is `IndexedDB/resources/support.js`'s `keep_alive`,
///    reduced), pinned the checkpoint open forever. The `setTimeout(…, 0)` never ran, the script never
///    set `__idbDone`, and memory grew at a measured 13.2 MB/s. If that comes back, `run_flow` below
///    exhausts its 200 pumps and the log has no `task:` line at all — which is what this asserts.
/// 2. **The transaction is still active in the microtasks the handler schedules.** The active flag is
///    unset when control returns to the event loop, i.e. AFTER that task's microtask checkpoint;
///    clearing it on the line after `dispatchEvent` showed a `Promise.resolve().then()` an already
///    inactive transaction (all four subtests of that WPT file failed on exactly that assertion).
#[tokio::test]
async fn a_success_handler_that_re_arms_a_request_does_not_starve_the_task_queue() {
    let mut e = page().await;
    let got = run_flow(
        &mut e,
        r#"
        globalThis.__idbLog = [];
        const log = globalThis.__idbLog;
        const state = (tx) => {
          try { tx.objectStore('store').get(0); return 'active'; } catch (err) { return err.name; }
        };
        const open = indexedDB.open('w918-starve', 1);
        open.onupgradeneeded = () => { open.result.createObjectStore('store'); };
        open.onsuccess = () => {
          const tx = open.result.transaction('store', 'readonly');
          const os = tx.objectStore('store');
          let spinning = true;
          const spin = () => { if (!spinning) return; os.get(0).onsuccess = spin; };
          spin();
          os.get(0).onsuccess = () => {
            log.push('handler:' + state(tx));
            Promise.resolve().then(() => log.push('microtask:' + state(tx)));
            setTimeout(() => {
              log.push('task-ran');
              spinning = false;
              globalThis.__idbDone = true;
            }, 0);
          };
        };
        "#,
    )
    .await;
    assert_eq!(got, "handler:active\nmicrotask:active\ntask-ran");
}

/// A CYCLIC ARRAY IS A `DataError`, NOT A STACK OVERFLOW.
///
/// `var a = []; a[0] = a;` is a value `IDBKeyRange.includes()` must reject, and
/// `IndexedDB/idbkeyrange-includes.any.js` constructs it on purpose to check exactly that. Korvus's
/// `js_to_key` recursed on array elements with no `seen` list, so the value took the whole PROCESS
/// down -- `fatal runtime error: stack overflow, aborting`, which no watchdog can interrupt and which
/// ended the scoreboard sweep at file 103 of 220 (measured 2026-09-11 at master `00316613`). The spec's
/// "convert a value to a key" carries that `seen` list for this reason.
///
/// The cycle is checked at three depths because the guard has to be carried DOWN the recursion, not
/// just consulted at the top; and a diamond (the same array twice as siblings, no cycle) must still be
/// a perfectly good key, which is what separates a `seen` STACK from a naive visited set.
#[tokio::test]
async fn a_self_referential_array_is_rejected_as_a_key_instead_of_eating_the_stack() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "idb-cyclic-key",
        r#"(() => {
            const range = IDBKeyRange.bound(12, 34);
            const err = (f) => { try { f(); return "no-throw"; } catch (e) { return e.name; } };
            const direct = []; direct[0] = direct;
            const nested = []; nested[0] = [[nested]];
            const shared = [1, 2];
            return [err(() => range.includes(direct)),
                    err(() => range.includes(nested)),
                    err(() => range.includes([1, [2, [3]]])),
                    err(() => range.includes([shared, shared])),
                    err(() => range.includes({}))].join("|");
        })()"#
            .to_string(),
    );
    // WPT `IndexedDB/idbkeyrange-includes.any.js` (11/11 with this fix): an invalid key is a
    // `DataError`. A finite nested array and a diamond are VALID keys, so `includes` answers rather
    // than throwing -- both are out of the [12, 34] range, so the answer is `false` and not a throw.
    assert_eq!(got, "DataError|DataError|no-throw|no-throw|DataError");
}

// ── 5. the whole round trip ─────────────────────────────────────────────────────────────────────

/// The IndexedDB flow is microtask-driven; pump the loop until the script says it is done.
async fn run_flow(e: &mut Engine, script: &str) -> String {
    e.eval("idb-flow", script.to_string()).unwrap();
    for _ in 0..200 {
        e.run().await.unwrap();
        let done = e.eval_value("idb-done", "!!globalThis.__idbDone".to_string()).unwrap();
        if done.as_bool() == Some(true) {
            break;
        }
    }
    str_of(e, "idb-log", r#"(globalThis.__idbLog || ["NO LOG"]).join("\n")"#.to_string())
}

#[tokio::test]
async fn the_whole_round_trip_runs_through_the_tables() {
    let mut e = page().await;
    let script = r#"
        globalThis.__idbLog = [];
        const L = (s) => globalThis.__idbLog.push(s);
        const T = (v) => Object.prototype.toString.call(v);
        const req = indexedDB.open("live", 1);
        L("open=" + T(req) + " readyState=" + req.readyState);
        req.onupgradeneeded = function (ev) {
            L("upgrade=" + T(ev) + " this===req " + (this === req) + " target===req " + (ev.target === req) +
              " old/new " + ev.oldVersion + "/" + ev.newVersion + " own=" + Object.getOwnPropertyNames(ev).join("+"));
            const db = req.result, tx = req.transaction;
            L("db=" + T(db) + " name=" + db.name + " version=" + db.version + " osn=" + T(db.objectStoreNames));
            L("tx=" + T(tx) + " mode=" + tx.mode + " durability=" + tx.durability + " db===db " + (tx.db === db) +
              " error=" + String(tx.error) + " osn=" + T(tx.objectStoreNames));
            const st = db.createObjectStore("s", { keyPath: "id" });
            const ix = st.createIndex("byV", "v", {});
            L("store=" + T(st) + " keyPath=" + st.keyPath + " autoIncrement=" + st.autoIncrement +
              " tx===tx " + (st.transaction === tx) + " indexNames=" + T(st.indexNames) +
              " contains=" + st.indexNames.contains("byV"));
            L("index=" + T(ix) + " name=" + ix.name + " keyPath=" + ix.keyPath + " unique=" + ix.unique +
              " multiEntry=" + ix.multiEntry + " objectStore===st " + (ix.objectStore === st));
            st.name = "renamed";
            L("renamed=" + st.name + " contains=" + db.objectStoreNames.contains("renamed"));
            st.put({ id: 1, v: "a" }); st.put({ id: 2, v: "b" }); st.put({ id: 3, v: "c" });
        };
        req.onsuccess = function () {
            const db = req.result;
            L("done readyState=" + req.readyState + " result=" + T(db) + " source=" + String(req.source));
            const tx = db.transaction("renamed", "readwrite");
            const st = tx.objectStore("renamed");
            L("tx2 mode=" + tx.mode + " durability=" + tx.durability + " identity=" + (tx.objectStore("renamed") === st));
            try { st.name = "nope"; L("rename-outside=NO-THROW"); } catch (e) { L("rename-outside=" + e.name); }
            const g = st.get(2); g.onsuccess = () => L("get(2)=" + JSON.stringify(g.result));
            const gk = st.getKey(3); gk.onsuccess = () => L("getKey(3)=" + gk.result);
            const ga = st.getAll(); ga.onsuccess = () => L("getAll=" + JSON.stringify(ga.result));
            const gr = st.getAllRecords();
            gr.onsuccess = () => {
                const r = gr.result;
                L("getAllRecords n=" + r.length + " T0=" + T(r[0]) + " own0=[" + Object.getOwnPropertyNames(r[0]).join("+") +
                  "] proto0=" + (Object.getPrototypeOf(r[0]) === IDBRecord.prototype) +
                  " key0=" + r[0].key + " pk0=" + r[0].primaryKey + " val0=" + JSON.stringify(r[0].value));
            };
            const c1 = st.count(); c1.onsuccess = () => L("count=" + c1.result);
            const walk = [];
            const cur = st.openCursor();
            cur.onsuccess = function (ev) {
                const c = ev.target.result;
                if (c) {
                    walk.push(T(c) + ":" + c.key + ":" + c.primaryKey + ":" + JSON.stringify(c.value) + ":" + c.direction +
                              ":" + (c.request === cur) + ":" + (c.source === st));
                    if (walk.length === 1) c.advance(1); else c.continue();
                } else {
                    L("cursor=" + walk.join(" | ") + " end=" + String(c));
                    const d = st.delete(1);
                    d.onsuccess = () => {
                        const c2 = st.count();
                        c2.onsuccess = () => { L("after-delete=" + c2.result); st.clear(); };
                    };
                }
            };
            tx.oncomplete = function (ev) {
                L("complete=" + T(ev) + " type=" + ev.type + " target===tx " + (ev.target === tx) + " this===tx " + (this === tx));
                const t3 = db.transaction("renamed", "readonly");
                const c3 = t3.objectStore("renamed").count();
                c3.onsuccess = () => { L("after-clear=" + c3.result); globalThis.__idbDone = true; };
            };
            tx.onerror = function () { L("TX ERROR"); globalThis.__idbDone = true; };
        };
        req.onerror = function () { L("OPEN ERROR"); globalThis.__idbDone = true; };
    "#;
    let got = run_flow(&mut e, script).await;
    let want = "\
open=[object IDBOpenDBRequest] readyState=pending
upgrade=[object IDBVersionChangeEvent] this===req true target===req true old/new 0/1 own=isTrusted
db=[object IDBDatabase] name=live version=1 osn=[object DOMStringList]
tx=[object IDBTransaction] mode=versionchange durability=default db===db true error=null osn=[object DOMStringList]
store=[object IDBObjectStore] keyPath=id autoIncrement=false tx===tx true indexNames=[object DOMStringList] contains=true
index=[object IDBIndex] name=byV keyPath=v unique=false multiEntry=false objectStore===st true
renamed=renamed contains=true
done readyState=done result=[object IDBDatabase] source=null
tx2 mode=readwrite durability=default identity=true
rename-outside=InvalidStateError
get(2)={\"id\":2,\"v\":\"b\"}
getKey(3)=3
getAll=[{\"id\":1,\"v\":\"a\"},{\"id\":2,\"v\":\"b\"},{\"id\":3,\"v\":\"c\"}]
getAllRecords n=3 T0=[object IDBRecord] own0=[] proto0=true key0=1 pk0=1 val0={\"id\":1,\"v\":\"a\"}
count=3
cursor=[object IDBCursorWithValue]:1:1:{\"id\":1,\"v\":\"a\"}:next:true:true | [object IDBCursorWithValue]:2:2:{\"id\":2,\"v\":\"b\"}:next:true:true | [object IDBCursorWithValue]:3:3:{\"id\":3,\"v\":\"c\"}:next:true:true end=null
after-delete=2
complete=[object Event] type=complete target===tx true this===tx true
after-clear=0";
    assert_eq!(got, want, "the IndexedDB round trip is not Chrome's");
}

// ── 6. the worker realm ─────────────────────────────────────────────────────────────────────────

/// Spawn a worker that probes the twelve and posts a pipe-joined summary back; pump until RESULT:.
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
async fn the_worker_realm_owns_the_same_twelve_interfaces() {
    // The worker's twelve come from the SAME tables (invariant I8): same prototypes, same member
    // order, the same chains and the same "Illegal constructor". The bundle's worker.surface.json
    // records the twelve interfaces and NO `indexedDB` global, so that is what the worker exposes.
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const names = ["IDBFactory","IDBRequest","IDBOpenDBRequest","IDBTransaction","IDBDatabase",
                       "IDBObjectStore","IDBIndex","IDBCursor","IDBCursorWithValue","IDBKeyRange",
                       "IDBRecord","IDBVersionChangeEvent"];
        const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name; } };
        const parts = [
            names.every((n) => typeof globalThis[n] === "function"),
            names.every((n) => globalThis[n].prototype[Symbol.toStringTag] === n),
            desc(IDBRequest.prototype),
            desc(IDBKeyRange.prototype),
            Object.getPrototypeOf(IDBRequest.prototype) === EventTarget.prototype,
            Object.getPrototypeOf(IDBOpenDBRequest.prototype) === IDBRequest.prototype,
            Object.getPrototypeOf(IDBCursorWithValue.prototype) === IDBCursor.prototype,
            Object.getPrototypeOf(IDBVersionChangeEvent.prototype) === Event.prototype,
            throws(() => new IDBRequest()),
            // a REAL body in the worker, where the deleted shell had a stub
            Object.prototype.toString.call(IDBKeyRange.only(7)) + ":" + IDBKeyRange.only(7).lower,
            new IDBVersionChangeEvent("x", { oldVersion: 4 }).oldVersion,
            // `indexedDB` in a worker is an INHERITED `WorkerGlobalScope` row, not an own global, and
            // it answers a real `IDBFactory`. Chrome agrees -- measured on
            // `google-chrome --headless=new` 150 with `tests/fixtures/worker-scope-probe.js` in a real
            // worker: `indexedDB  OK:object`. It read `undefined` here until the scope table landed,
            // because the capture's row was a D2 stub and nothing published a factory.
            typeof globalThis.indexedDB,
            Object.prototype.hasOwnProperty.call(globalThis, "indexedDB"),
        ];
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    let req = "result:a:s=0,error:a:s=0,source:a:s=0,transaction:a:s=0,readyState:a:s=0,onsuccess:a:s=1,onerror:a:s=1,constructor:d:w=1";
    let kr = "lower:a:s=0,upper:a:s=0,lowerOpen:a:s=0,upperOpen:a:s=0,includes:d:w=1,constructor:d:w=1";
    assert_eq!(
        got,
        format!("true|true|{req}|{kr}|true|true|true|true|TypeError|[object IDBKeyRange]:7|4|object|false"),
        "the worker realm's IndexedDB interfaces are not the window's tables"
    );
}

/// A SEQUENCE key path on an object store: YouTube keys its `ResponseStore` by `["key", "clientName"]`, which korvus
/// took for no key path at all, so every `put` threw `DataError` where Chrome's stored. Read on Chrome 150.0.7871.46
/// (2026-10-24): the key path reads back as the same array each time, on the store and on an index; the key is the
/// array of the parts' values; autoIncrement over a sequence is `InvalidAccessError`, an invalid path `SyntaxError`,
/// a value missing a part `DataError`.
#[tokio::test]
async fn a_sequence_key_path_keys_a_store_as_chrome_does() {
    let mut e = page().await;
    let script = r#"
        globalThis.__idbLog = [];
        const L = (s) => globalThis.__idbLog.push(s);
        const req = indexedDB.open("seq", 1);
        req.onupgradeneeded = () => {
            const db = req.result;
            const st = db.createObjectStore("r", { keyPath: ["key", "clientName"] });
            L("keyPath=" + JSON.stringify(st.keyPath) + " isArray=" + Array.isArray(st.keyPath) + " same=" + (st.keyPath === st.keyPath));
            const ix = st.createIndex("byPair", ["clientName", "x"]);
            L("ixKeyPath=" + JSON.stringify(ix.keyPath) + " ixSame=" + (ix.keyPath === ix.keyPath));
            try { db.createObjectStore("bad", { keyPath: ["a", "b"], autoIncrement: true }); L("auto=NO-THROW"); } catch (e) { L("auto=" + e.name); }
            try { db.createObjectStore("bad2", { keyPath: "a..b" }); L("syntax=NO-THROW"); } catch (e) { L("syntax=" + e.name); }
            st.put({ key: "a", clientName: 1, x: 5 });
            try { st.put({ key: "b" }); L("missing=NO-THROW"); } catch (e) { L("missing=" + e.name); }
        };
        req.onsuccess = () => {
            const st = req.result.transaction("r").objectStore("r");
            const g = st.get(["a", 1]);
            g.onsuccess = () => {
                L("get=" + JSON.stringify(g.result));
                const gi = st.index("byPair").get([1, 5]);
                gi.onsuccess = () => {
                    L("index=" + JSON.stringify(gi.result));
                    const k = st.getAllKeys();
                    k.onsuccess = () => { L("keys=" + JSON.stringify(k.result)); globalThis.__idbDone = true; };
                };
            };
        };
        req.onerror = () => { L("OPEN ERROR"); globalThis.__idbDone = true; };
    "#;
    let got = run_flow(&mut e, script).await;
    assert_eq!(
        got,
        [
            r#"keyPath=["key","clientName"] isArray=true same=true"#,
            r#"ixKeyPath=["clientName","x"] ixSame=true"#,
            "auto=InvalidAccessError",
            "syntax=SyntaxError",
            "missing=DataError",
            r#"get={"key":"a","clientName":1,"x":5}"#,
            r#"index={"key":"a","clientName":1,"x":5}"#,
            r#"keys=[["a",1]]"#,
        ]
        .join("\n")
    );
}
