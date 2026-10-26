//! LIVE one-table gate for `DataTransfer`, `DataTransferItemList`, `DataTransferItem` and `FileList`.
//!
//! Boots the real engine and asserts:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the captured bundle's member
//!      list for that interface, in the capture's order, with the capture's descriptors, followed by
//!      the non-enumerable `constructor` -- the hand-table shape.
//!   2. **The interface objects.** The capture's `Symbol.toStringTag`, `length`, `name` and (empty)
//!      statics; `new DataTransfer()` works and the other three throw "Illegal constructor", which is
//!      what the bundle's oracle records and what `new FileList()` did NOT do before this lane.
//!   3. **The two iterators are `Array.prototype.values`** -- the SAME function object, which headless
//!      Chrome 150.0.7871.46 on this machine confirms for both (2026-09-10), where the deleted JS gave
//!      each a hand-built `op_*_iter` that snapshotted into a fresh Array.
//!   4. **A receiver check on every member** the four tables own: `Object.create(Iface.prototype)`
//!      gets a TypeError where the deleted layer answered `undefined`/`0`/`null` or did nothing.
//!   5. **The drag data store round trips**: items/files/length/add/remove/clear/getAsFile/
//!      getAsString/types/getData/setData/clearData, `[...dt.items]`, `Object.keys(list)`, and
//!      `input.files` as a real `FileList` instance.
//!   6. **The WORKER realm owns the same `FileList` table** (invariant I8) -- the one interface of the
//!      four the worker capture lists.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><input id=f type=file></body></html>";

/// The four this lane's tables own in the WINDOW.
const IFACES: &[&str] = &["FileList", "DataTransferItemList", "DataTransferItem", "DataTransfer"];

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

// ── 1. the four prototypes, byte-identical to the capture ───────────────────────────────────────

#[tokio::test]
async fn every_datatransfer_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l8-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_datatransfer_interface_object_carries_the_capture_tag_length_and_statics() {
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
        let got = str_of(&mut e, "l8-ctor", src);
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

// ── 2. newability and the chains ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn only_data_transfer_is_newable_and_nothing_inherits() {
    let mut e = page().await;
    // `new FileList()`, `new DataTransferItem()` and `new DataTransferItemList()` all WORKED before
    // this lane: `dom/00-bootstrap.js` put the raw cppgc class templates on the global, and their
    // `#[constructor]` minted an instance. The bundle's oracle records a TypeError for all three, and
    // headless Chrome 150 says "Failed to construct 'FileList': Illegal constructor" (2026-09-10).
    let got = str_of(
        &mut e,
        "l8-new",
        r#"(() => {
            const P = Object.getPrototypeOf;
            const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
            const native = (f) => Function.prototype.toString.call(f).includes("[native code]");
            const dt = new DataTransfer();
            return [
                throws(() => new FileList()),
                throws(() => new DataTransferItem()),
                throws(() => new DataTransferItemList()),
                Object.prototype.toString.call(dt),
                dt instanceof DataTransfer,
                dt.constructor === DataTransfer,
                P(DataTransfer.prototype) === Object.prototype,
                P(FileList.prototype) === Object.prototype,
                P(DataTransferItemList.prototype) === Object.prototype,
                P(DataTransferItem.prototype) === Object.prototype,
                P(DataTransfer) === Function.prototype,
                ["DataTransfer","DataTransferItem","DataTransferItemList","FileList"].every(n => native(globalThis[n])),
                Object.prototype.toString.call(dt.items),
                Object.prototype.toString.call(dt.files),
                dt.items instanceof DataTransferItemList,
                dt.files instanceof FileList,
                (class D extends DataTransfer {}) && Object.prototype.toString.call(new (class D extends DataTransfer {})()),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        // The message Chrome was already recorded as answering above is what the engine
        // answers now -- `Failed to construct 'FileList': Illegal constructor`, per interface.
        "TypeError:Failed to construct 'FileList': Illegal constructor|\
TypeError:Failed to construct 'DataTransferItem': Illegal constructor|\
TypeError:Failed to construct 'DataTransferItemList': Illegal constructor|\
[object DataTransfer]|true|true|true|true|true|true|true|true|\
[object DataTransferItemList]|[object FileList]|true|true|[object DataTransfer]",
        "the L8 interface objects / chains are not the capture's"
    );
}

// ── 3. the two iterators ARE Array.prototype.values ─────────────────────────────────────────────

#[tokio::test]
async fn both_lists_own_the_array_value_iterator() {
    let mut e = page().await;
    // Headless Chrome 150.0.7871.46, 2026-09-10:
    //   FileList.prototype[Symbol.iterator] === Array.prototype.values             -> true
    //   DataTransferItemList.prototype[Symbol.iterator] === Array.prototype.values -> true
    //   both descriptors: data, e=0, c=1, w=1
    // The deleted `op_fl_iter` / `op_dtil_iter` handed back a DIFFERENT function object per read.
    let got = str_of(
        &mut e,
        "l8-iter",
        r#"(() => {
            const one = (P) => {
                const d = Object.getOwnPropertyDescriptor(P, Symbol.iterator);
                return [P[Symbol.iterator] === Array.prototype.values, +d.enumerable, +d.configurable, +d.writable].join(",");
            };
            return [one(FileList.prototype), one(DataTransferItemList.prototype),
                    Object.getOwnPropertySymbols(DataTransfer.prototype).length,
                    Object.getOwnPropertySymbols(DataTransferItem.prototype).length].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "true,0,1,1|true,0,1,1|1|1", "the list iterators are not Array.prototype.values");
}

// ── 4. receiver checks ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_table_owned_member_refuses_a_foreign_receiver() {
    let mut e = page().await;
    // Before the cutover the op2 members answered `undefined` for any object at all and the raw
    // callbacks answered `0` / `null` / silently did nothing. Chrome throws "Illegal invocation" for
    // every one of them (probed 2026-09-10) -- except a PROMISE-returning one, which refuses by
    // rejecting (WebIDL): Chrome 150 returns a promise rejected with "Failed to execute
    // 'getAsFileSystemHandle' on 'DataTransferItem': Illegal invocation" (probed 2026-10-16; Brave, the
    // persona until then, has no `getAsFileSystemHandle`).
    let got = str_of(
        &mut e,
        "l8-recv",
        r#"(() => {
            const bad = [];
            globalThis.__recvAsync = []; globalThis.__recvAsyncN = 0;
            const probe = (label, f) => {
                let r;
                try { r = f(); } catch (e) { if (!(e instanceof TypeError)) bad.push(label + ":" + e.name); return; }
                if (r instanceof Promise) {
                    __recvAsyncN++;
                    r.then(() => __recvAsync.push(label + ":resolved"),
                           (e) => { if (!(e instanceof TypeError)) __recvAsync.push(label + ":" + e.name); });
                    return;
                }
                bad.push(label + ":no-throw");
            };
            const IF = { DataTransfer, DataTransferItemList, DataTransferItem, FileList };
            for (const name in IF) {
                const P = IF[name].prototype, foreign = Object.create(P);
                for (const m of Object.getOwnPropertyNames(P)) {
                    if (m === "constructor") continue;
                    const d = Object.getOwnPropertyDescriptor(P, m);
                    if (d.get) probe(name + "." + m + ".get", () => d.get.call(foreign));
                    if (d.set) probe(name + "." + m + ".set", () => d.set.call(foreign, "copy"));
                    if (typeof d.value === "function") probe(name + "." + m + "()", () => d.value.call(foreign, "x", "y", "z"));
                }
            }
            return bad.join(",");
        })()"#
            .to_string(),
    );
    assert_eq!(got, "", "a member answered instead of throwing for a foreign receiver");
    e.run().await.unwrap();
    let late = str_of(&mut e, "l8-recv-async", "__recvAsyncN + '|' + __recvAsync.join(',')".to_string());
    assert_eq!(late, "1|", "getAsFileSystemHandle, the one promise-returning member, must reject a foreign receiver");
}

#[tokio::test]
async fn the_arity_guards_are_chromes() {
    let mut e = page().await;
    // Every message here is headless Chrome 150's, verbatim (2026-09-10). `setDragImage()` and
    // `getData()` used to answer `undefined` -- the op2 members had no arity guard, and the two
    // `Object.defineProperty(fn, "length", ...)` corrections in `dom/08d-datatransfer.js` only fixed
    // the reported `.length`.
    let got = str_of(
        &mut e,
        "l8-arity",
        r#"(() => {
            const dt = new DataTransfer();
            const msg = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + ":" + e.message; } };
            return [
                msg(() => dt.getData()),
                msg(() => dt.setData()),
                msg(() => dt.setDragImage()),
                msg(() => dt.items.add()),
                msg(() => dt.items.remove()),
                msg(() => dt.files.item()),
                String(dt.clearData()),
                String(dt.items.clear()),
                dt.clearData.length + ":" + dt.setDragImage.length + ":" + dt.getData.length + ":" + dt.setData.length,
                dt.items.add.length + ":" + dt.items.remove.length + ":" + dt.items.clear.length + ":" + dt.files.item.length,
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "TypeError:Failed to execute 'getData' on 'DataTransfer': 1 argument required, but only 0 present.|\
TypeError:Failed to execute 'setData' on 'DataTransfer': 2 arguments required, but only 0 present.|\
TypeError:Failed to execute 'setDragImage' on 'DataTransfer': 3 arguments required, but only 0 present.|\
TypeError:Failed to execute 'add' on 'DataTransferItemList': 1 argument required, but only 0 present.|\
TypeError:Failed to execute 'remove' on 'DataTransferItemList': 1 argument required, but only 0 present.|\
TypeError:Failed to execute 'item' on 'FileList': 1 argument required, but only 0 present.|\
undefined|undefined|0:3:1:2|1:1:0:1",
        "the L8 arity guards are not Chrome's"
    );
}

// ── 5. the drag data store round trips ──────────────────────────────────────────────────────────

#[tokio::test]
async fn the_drag_data_store_round_trips_through_the_table_bodies() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l8-store",
        r#"(() => {
            const dt = new DataTransfer();
            const out = [];
            out.push(dt.dropEffect + ":" + dt.effectAllowed);
            dt.dropEffect = "copy"; dt.dropEffect = "bogus";
            dt.effectAllowed = "copyMove"; dt.effectAllowed = "nope";
            out.push(dt.dropEffect + ":" + dt.effectAllowed);
            dt.setData("text/plain", "hello");
            out.push(dt.getData("text") + ":" + dt.getData("text/plain") + ":" + dt.getData("application/json"));
            dt.setData("url", "https://example.com");
            out.push(dt.getData("text/uri-list"));
            out.push(JSON.stringify(dt.types) + ":" + Object.prototype.toString.call(dt.types));
            const f = new File(["abc"], "a.txt", { type: "text/plain" });
            const item = dt.items.add(f);
            out.push(item.kind + ":" + item.type + ":" + (item.getAsFile() === f));
            out.push(dt.files.length + ":" + dt.files[0].name + ":" + (dt.files[0] instanceof File) + ":" + dt.files.item(0).name);
            out.push(JSON.stringify(dt.types));
            out.push(dt.items.length + ":" + JSON.stringify(Object.keys(dt.items)) + ":" + JSON.stringify(Object.keys(dt.files)));
            // An index property is Chrome's `{enumerable, configurable, NOT writable}` (probed
            // 2026-09-10); `set_index` used to make them writable.
            const di = Object.getOwnPropertyDescriptor(dt.items, "0"), df = Object.getOwnPropertyDescriptor(dt.files, "0");
            out.push([+di.enumerable, +di.configurable, +di.writable, +df.enumerable, +df.configurable, +df.writable].join(""));
            out.push([...dt.items].map(i => i.kind + "/" + i.type).join(","));
            let got = null;
            dt.items[0].getAsString((s) => { got = s; });
            out.push(String(got));
            out.push(String(dt.items[0].getAsFile()) + ":" + String(dt.items[0].webkitGetAsEntry()));
            dt.items.remove(0);
            out.push(dt.items.length + ":<" + dt.getData("text/plain") + ">:" + dt.files.length);
            dt.setData("text/plain", "again");
            dt.clearData();
            out.push("<" + dt.getData("text/plain") + ">:" + dt.files.length);
            dt.items.clear();
            out.push(dt.items.length + ":" + dt.files.length + ":" + JSON.stringify(dt.types));
            return out.join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "none:none|copy:copyMove|hello:hello:|https://example.com|\
[\"text/plain\",\"text/uri-list\"]:[object Array]|file:text/plain:true|1:a.txt:true:a.txt|\
[\"text/plain\",\"text/uri-list\",\"Files\"]|3:[\"0\",\"1\",\"2\"]:[\"0\"]|110110|\
string/text/plain,string/text/uri-list,file/text/plain|hello|null:null|\
2:<>:1|<>:1|0:0:[]",
        "the drag data store does not round trip"
    );
}

#[tokio::test]
async fn an_input_type_file_hands_out_a_real_file_list_instance() {
    let mut e = page().await;
    // `input.files` is `[SameObject]`: one FileList, created once and kept. It is minted in Rust
    // (`core/html/html_input_element.rs`) through the table's mint site, so it carries the TABLE's
    // prototype -- before this lane it carried the cppgc class template's, which was the same object
    // only because `globalThis.FileList` WAS that class template.
    let got = str_of(
        &mut e,
        "l8-input",
        r#"(() => {
            const i = document.getElementById("f");
            const t = document.createElement("input");
            return [
                i.files instanceof FileList,
                i.files === i.files,
                i.files.length,
                Object.prototype.toString.call(i.files),
                i.files.constructor === FileList,
                Object.getPrototypeOf(i.files) === FileList.prototype,
                JSON.stringify([...i.files]),
                String(i.files.item(0)),
                String(t.files),
            ].join("|");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        "true|true|0|[object FileList]|true|true|[]|null|null",
        "input.files is not a real FileList on the table's prototype"
    );
}

// ── 6. the worker realm ─────────────────────────────────────────────────────────────────────────

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
async fn the_worker_realm_owns_the_same_file_list_table() {
    // `FileList` is `[Exposed=(Window,Worker)]` and the bundle's `worker.surface.json` lists it with a
    // member list byte-identical to the window's. The other three are window-only in BOTH the worker
    // capture and the worker registry, so the worker must not grow them.
    let body = r#"
        const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
            const d = Object.getOwnPropertyDescriptor(o, n);
            return (d.get || d.set) ? `${n}:a:s=${+!!d.set}` : `${n}:d:w=${+d.writable}`;
        }).join(",");
        const throws = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name; } };
        const parts = [
            typeof FileList,
            desc(FileList.prototype),
            FileList.prototype[Symbol.toStringTag],
            FileList.prototype[Symbol.iterator] === Array.prototype.values,
            throws(() => new FileList()),
            typeof globalThis.DataTransfer,
            typeof globalThis.DataTransferItem,
            typeof globalThis.DataTransferItemList,
        ];
        self.postMessage(parts.join("|"));
    "#;
    let got = worker_probe(body).await;
    assert_eq!(
        got,
        "function|length:a:s=0,item:d:w=1,constructor:d:w=1|FileList|true|TypeError|undefined|undefined|undefined",
        "the worker realm's FileList is not the window's table"
    );
}
