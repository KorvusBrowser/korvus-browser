//! LIVE one-table gate for the six `<table>` interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against headless
//! Chrome 150's recorded answers (probed on this machine 2026-09-11; see the lane's notes):
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` last --
//!      the hand-table shape.
//!   2. **The interface objects and the chain.** The capture's `Symbol.toStringTag`, `length`, `name`
//!      and (empty) statics; `new HTMLTableElement()` throws "Illegal constructor"; the prototype chain
//!      is `HTMLElement -> Element -> Node -> EventTarget`; `document.createElement("table")` is an
//!      `instanceof` of the table's own interface object, which is what proves the tag map points at
//!      the TABLE's prototype and not at some other one.
//!   3. **The section algorithms.** `createTHead()` twice returns the SAME element and puts it after
//!      any caption/colgroup; the setters REPLACE; a non-`<caption>` is Chrome's TypeError and a
//!      `<tbody>` handed to `tHead` is Chrome's `HierarchyRequestError`.
//!   4. **The index rules.** `insertRow(-1)` appends, `insertRow(99)` and `insertRow(-2)` are
//!      `IndexSizeError` with Chrome's two different messages, `deleteRow(-1)` removes the last row and
//!      is a NO-OP on an empty table (the deleted JS threw), `deleteRow()` is the WebIDL arity
//!      TypeError.
//!   5. **The collections are live and are `HTMLCollection`s**, a `<tr>` in a nested table is not this
//!      table's row, and `rowIndex`/`sectionRowIndex`/`cellIndex` follow a move.
//!   6. **A MutationObserver sees an `insertRow`** -- the proof the algorithm mutates through the Node
//!      choke points rather than poking the arena.
//!   7. **Every member the four bodied tables own refuses a foreign receiver** with a TypeError.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The six this lane's tables own.
const IFACES: &[&str] = &[
    "HTMLTableElement",
    "HTMLTableRowElement",
    "HTMLTableSectionElement",
    "HTMLTableCellElement",
    "HTMLTableColElement",
    "HTMLTableCaptionElement",
];

/// The four with hand-written bodies -- the ones whose rows must refuse a foreign receiver. The other
/// two are reflections only, mounted by the shared `reflect_*` kits (which have no per-interface
/// receiver check anywhere on the surface; see the lane's REMAINS).
const BODIED: &[(&str, &str)] = &[
    ("HTMLTableElement", "table"),
    ("HTMLTableRowElement", "tr"),
    ("HTMLTableSectionElement", "tbody"),
    ("HTMLTableCellElement", "td"),
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
        k => panic!("unexpected kind {k}"),
    }
}

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

// ── 1. the six prototypes, byte-identical to the capture ────────────────────────────────────────

#[tokio::test]
async fn every_table_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l2-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_table_interface_object_carries_the_capture_tag_length_and_statics() {
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
        let got = str_of(&mut e, "l2-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        assert_eq!(got, format!("{iface}|0|0|1|{want_len}|{iface}|"), "{iface} interface object != the capture");
    }
}

// ── 2. the chain, the tag map, and "Illegal constructor" ────────────────────────────────────────

#[tokio::test]
async fn every_table_interface_chains_to_html_element_and_owns_its_tag() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, tag) in [
        ("HTMLTableElement", "table"),
        ("HTMLTableRowElement", "tr"),
        ("HTMLTableSectionElement", "tbody"),
        ("HTMLTableSectionElement", "thead"),
        ("HTMLTableSectionElement", "tfoot"),
        ("HTMLTableCellElement", "td"),
        ("HTMLTableCellElement", "th"),
        ("HTMLTableColElement", "col"),
        ("HTMLTableColElement", "colgroup"),
        ("HTMLTableCaptionElement", "caption"),
    ] {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const chain = [];
                let p = Object.getPrototypeOf(C.prototype);
                while (p && p.constructor && p.constructor.name && p !== Object.prototype) {{ chain.push(p.constructor.name); p = Object.getPrototypeOf(p); }}
                const el = document.createElement("{tag}");
                let ctorThrew = "no-throw";
                try {{ new C(); }} catch (err) {{ ctorThrew = err.constructor.name + ":" + err.message; }}
                return [chain.join(","), el instanceof C, Object.getPrototypeOf(el) === C.prototype,
                        Object.prototype.toString.call(el), ctorThrew].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l2-chain", src);
        let want_chain: Vec<&str> = fx["interfaces"][iface]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            got,
            // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
            format!("{}|true|true|[object {iface}]|TypeError:Failed to construct '{iface}': Illegal constructor", want_chain.join(",")),
            "{iface} (<{tag}>) chain / tag map / constructor"
        );
    }
}

// ── 3. the section algorithms ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_section_accessors_replace_create_and_reject_what_chrome_rejects() {
    let mut e = page().await;
    // Every expected value below is headless Chrome 150's answer on this machine (2026-09-11).
    let got = str_of(
        &mut e,
        "l2-sections",
        r#"(() => {
            const mk = (t) => document.createElement(t);
            // Fixtures are BUILT rather than written as `innerHTML` markup, because that is the tree
            // these algorithms actually walk. (Both work now: `innerHTML` gained its fragment-parsing
            // CONTEXT element in W9.11, so table markup survives either way.) Serialization (reading
            // `innerHTML`) is what asserts here.
            const el = (t, id, kids) => { const e = mk(t); if (id) e.id = id; for (const k of (kids || [])) e.appendChild(k); return e; };
            const tbl = (kids) => el("table", null, kids);
            const err = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + "|" + (e.name || "") + "|" + e.message; } };
            const out = [];
            // createTHead twice is the SAME element, and it lands after caption/colgroup.
            {
                const x = mk("table");
                x.appendChild(el("caption")); x.appendChild(el("colgroup")); x.appendChild(el("tbody", null, [el("tr")]));
                const a = x.createTHead(), b = x.createTHead();
                out.push("same:" + (a === b));
                out.push("place:" + x.innerHTML);
            }
            out.push("createTBody:" + (() => { const x = mk("table"); x.createTBody(); x.createTBody(); return x.innerHTML; })());
            out.push("createTFoot:" + (() => { const x = tbl([el("tbody", null, [el("tr")])]); x.createTFoot(); return x.innerHTML; })());
            out.push("createCaption:" + (() => { const x = tbl([el("tbody")]); x.createCaption(); return x.innerHTML; })());
            // The setters REPLACE the existing one, and null clears it.
            out.push("replace:" + (() => { const x = mk("table"); x.createCaption().id = "a"; const c = mk("caption"); c.id = "b"; x.caption = c; return x.innerHTML; })());
            out.push("clear:" + (() => { const x = mk("table"); x.createTHead(); x.tHead = null; return x.innerHTML + "/" + x.tHead; })());
            out.push("undef:" + (() => { const x = mk("table"); x.createCaption(); x.caption = undefined; return x.innerHTML; })());
            // A non-<caption> ELEMENT fails the IDL conversion -- a TypeError, not a DOMException.
            out.push("cap=div:" + err(() => { mk("table").caption = mk("div"); }));
            out.push("thead=div:" + err(() => { mk("table").tHead = mk("div"); }));
            // A <tbody> IS an HTMLTableSectionElement, so it reaches the algorithm: HierarchyRequestError.
            out.push("thead=tbody:" + err(() => { mk("table").tHead = mk("tbody"); }));
            out.push("tfoot=thead:" + err(() => { mk("table").tFoot = mk("thead"); }));
            out.push("delete:" + (() => { const x = mk("table"); x.createTHead(); x.createTFoot(); x.createCaption(); x.deleteTHead(); x.deleteTFoot(); x.deleteCaption(); return x.innerHTML + "/" + x.deleteCaption(); })());
            return out.join("\n");
        })()"#
            .to_string(),
    );
    let want = [
        "same:true",
        "place:<caption></caption><colgroup></colgroup><thead></thead><tbody><tr></tr></tbody>",
        "createTBody:<tbody></tbody><tbody></tbody>",
        "createTFoot:<tbody><tr></tr></tbody><tfoot></tfoot>",
        "createCaption:<caption></caption><tbody></tbody>",
        "replace:<caption id=\"b\"></caption>",
        "clear:/null",
        "undef:",
        "cap=div:TypeError|TypeError|Failed to set the 'caption' property on 'HTMLTableElement': Failed to convert value to 'HTMLTableCaptionElement'.",
        "thead=div:TypeError|TypeError|Failed to set the 'tHead' property on 'HTMLTableElement': Failed to convert value to 'HTMLTableSectionElement'.",
        "thead=tbody:DOMException|HierarchyRequestError|Failed to set the 'tHead' property on 'HTMLTableElement': Not a thead element.",
        "tfoot=thead:DOMException|HierarchyRequestError|Failed to set the 'tFoot' property on 'HTMLTableElement': Not a tfoot element.",
        "delete:/undefined",
    ];
    assert_eq!(got.lines().collect::<Vec<_>>(), want, "the section algorithms disagree with Chrome");
}

// ── 4. the index rules ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_index_rules_are_chromes() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l2-index",
        r#"(() => {
            const mk = (t) => document.createElement(t);
            // Fixtures are BUILT rather than written as `innerHTML` markup, because that is the tree
            // these algorithms actually walk. (Both work now: `innerHTML` gained its fragment-parsing
            // CONTEXT element in W9.11, so table markup survives either way.) Serialization (reading
            // `innerHTML`) is what asserts here.
            const el = (t, id, kids) => { const e = mk(t); if (id) e.id = id; for (const k of (kids || [])) e.appendChild(k); return e; };
            const tbl = (kids) => el("table", null, kids);
            const err = (f) => { try { const r = f(); return r === undefined ? "undefined" : String(r); } catch (e) { return (e.name || e.constructor.name) + "|" + e.message; } };
            const out = [];
            out.push("append:" + (() => { const x = tbl([el("tbody", null, [el("tr", "a")])]); x.insertRow(-1); return x.innerHTML; })());
            out.push("empty:" + (() => { const x = mk("table"); x.insertRow(); return x.innerHTML; })());
            out.push("emptyThead:" + (() => { const x = tbl([el("thead")]); x.insertRow(); return x.innerHTML; })());
            out.push("at0:" + (() => { const x = tbl([el("tbody", null, [el("tr", "a"), el("tr", "b")])]); x.insertRow(0); return x.innerHTML; })());
            out.push("frac:" + (() => { const x = tbl([el("tbody", null, [el("tr", "a"), el("tr", "b")])]); x.insertRow(1.7); return x.innerHTML; })());
            out.push("nan:" + (() => { const x = tbl([el("tbody", null, [el("tr", "a")])]); x.insertRow("abc"); return x.innerHTML; })());
            out.push("i99:" + err(() => mk("table").insertRow(99)));
            out.push("i-2:" + err(() => mk("table").insertRow(-2)));
            out.push("dellast:" + (() => { const x = tbl([el("tbody", null, [el("tr", "a"), el("tr", "b")])]); x.deleteRow(-1); return x.innerHTML; })());
            out.push("delempty:" + err(() => mk("table").deleteRow(-1)));
            out.push("del1:" + err(() => mk("table").deleteRow(1)));
            out.push("del-2:" + err(() => mk("table").deleteRow(-2)));
            out.push("delnoarg:" + err(() => mk("table").deleteRow()));
            out.push("cellappend:" + (() => { const x = el("tr", null, [el("td", "a")]); x.insertCell(-1); return x.innerHTML; })());
            out.push("cell0:" + (() => { const x = el("tr", null, [el("td", "a")]); x.insertCell(0); return x.innerHTML; })());
            out.push("cell9:" + err(() => mk("tr").insertCell(9)));
            out.push("cell-2:" + err(() => mk("tr").insertCell(-2)));
            out.push("delcell:" + (() => { const x = el("tr", null, [el("td", "a"), el("th", "b")]); x.deleteCell(-1); return x.innerHTML; })());
            out.push("delcellempty:" + err(() => mk("tr").deleteCell(-1)));
            out.push("delcell1:" + err(() => mk("tr").deleteCell(1)));
            out.push("delcellnoarg:" + err(() => mk("tr").deleteCell()));
            out.push("sect0:" + (() => { const x = el("tbody", null, [el("tr", "a")]); x.insertRow(0); return x.innerHTML; })());
            out.push("sect-2:" + err(() => mk("tbody").insertRow(-2)));
            out.push("sectdel1:" + err(() => mk("tbody").deleteRow(1)));
            out.push("sectdelempty:" + err(() => mk("tbody").deleteRow(-1)));
            out.push("sectdelnoarg:" + err(() => mk("tbody").deleteRow()));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    let want = [
        "append:<tbody><tr id=\"a\"></tr><tr></tr></tbody>",
        "empty:<tbody><tr></tr></tbody>",
        "emptyThead:<thead></thead><tbody><tr></tr></tbody>",
        "at0:<tbody><tr></tr><tr id=\"a\"></tr><tr id=\"b\"></tr></tbody>",
        "frac:<tbody><tr id=\"a\"></tr><tr></tr><tr id=\"b\"></tr></tbody>",
        "nan:<tbody><tr></tr><tr id=\"a\"></tr></tbody>",
        "i99:IndexSizeError|Failed to execute 'insertRow' on 'HTMLTableElement': The index provided (99) is greater than the number of rows in the table (0).",
        "i-2:IndexSizeError|Failed to execute 'insertRow' on 'HTMLTableElement': The index provided (-2) is less than -1.",
        "dellast:<tbody><tr id=\"a\"></tr></tbody>",
        "delempty:undefined",
        "del1:IndexSizeError|Failed to execute 'deleteRow' on 'HTMLTableElement': The index provided (1) is greater than the number of rows in the table (0).",
        "del-2:IndexSizeError|Failed to execute 'deleteRow' on 'HTMLTableElement': The index provided (-2) is less than -1.",
        "delnoarg:TypeError|Failed to execute 'deleteRow' on 'HTMLTableElement': 1 argument required, but only 0 present.",
        "cellappend:<td id=\"a\"></td><td></td>",
        "cell0:<td></td><td id=\"a\"></td>",
        "cell9:IndexSizeError|Failed to execute 'insertCell' on 'HTMLTableRowElement': The value provided (9) is outside the range [-1, 0].",
        "cell-2:IndexSizeError|Failed to execute 'insertCell' on 'HTMLTableRowElement': The value provided (-2) is outside the range [-1, 0].",
        "delcell:<td id=\"a\"></td>",
        "delcellempty:undefined",
        "delcell1:IndexSizeError|Failed to execute 'deleteCell' on 'HTMLTableRowElement': The value provided (1) is outside the range [0, 0).",
        "delcellnoarg:TypeError|Failed to execute 'deleteCell' on 'HTMLTableRowElement': 1 argument required, but only 0 present.",
        "sect0:<tr></tr><tr id=\"a\"></tr>",
        "sect-2:IndexSizeError|Failed to execute 'insertRow' on 'HTMLTableSectionElement': The provided index (-2 is outside the range [-1, 0].",
        "sectdel1:IndexSizeError|Failed to execute 'deleteRow' on 'HTMLTableSectionElement': The provided index (1 is outside the range [-1, 0].",
        "sectdelempty:undefined",
        "sectdelnoarg:TypeError|Failed to execute 'deleteRow' on 'HTMLTableSectionElement': 1 argument required, but only 0 present.",
    ];
    assert_eq!(got.lines().collect::<Vec<_>>(), want, "the index rules disagree with Chrome");
}

// ── 5. the collections and the positional getters ───────────────────────────────────────────────

#[tokio::test]
async fn the_collections_are_live_html_collections_and_the_indices_follow_a_move() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l2-live",
        r##"(() => {
            const mk = (t) => document.createElement(t);
            // Fixtures are BUILT rather than written as `innerHTML` markup, because that is the tree
            // these algorithms actually walk. (Both work now: `innerHTML` gained its fragment-parsing
            // CONTEXT element in W9.11, so table markup survives either way.) Serialization (reading
            // `innerHTML`) is what asserts here.
            const el = (t, id, kids) => { const e = mk(t); if (id) e.id = id; for (const k of (kids || [])) e.appendChild(k); return e; };
            const tbl = (kids) => el("table", null, kids);
            const out = [];
            // Live: a mutation shows without re-reading the accessor.
            const x = mk("table");
            const rows = x.rows, bodies = x.tBodies;
            out.push("start:" + rows.length + "," + bodies.length);
            const r = x.insertRow();
            out.push("after:" + rows.length + "," + bodies.length);
            out.push("kind:" + (rows instanceof HTMLCollection) + "," + (bodies instanceof HTMLCollection) + "," + Object.prototype.toString.call(rows));
            const c = r.cells;
            r.insertCell();
            out.push("cells:" + c.length + "," + (c instanceof HTMLCollection));
            // A nested table's <tr>/<tbody> is not this table's.
            const n = mk("table");
            n.appendChild(el("tbody", null, [el("tr", null, [el("td", null, [tbl([el("tbody", null, [el("tr")])])])])]));
            out.push("nested:" + n.rows.length + "," + n.tBodies.length + "," + n.rows[0].cells.length);
            // A <tr> that is a direct child of the table still counts.
            const d = mk("table"); d.appendChild(mk("tr"));
            out.push("directTr:" + d.rows.length);
            // rowIndex / sectionRowIndex / cellIndex, before and after a move.
            const t = mk("table");
            t.appendChild(el("tfoot", null, [el("tr", "f")])); t.appendChild(el("tbody", null, [el("tr", "b")]));
            const f = t.querySelector("#f");
            out.push("index:" + f.rowIndex + "," + f.sectionRowIndex);
            t.querySelector("tbody").appendChild(f);
            out.push("moved:" + f.rowIndex + "," + f.sectionRowIndex);
            out.push("orphan:" + (() => { const q = mk("tr"); return q.rowIndex + "," + q.sectionRowIndex; })());
            out.push("cellIndex:" + (() => { const q = el("tr", null, [el("td"), el("th", "h")]); const h = q.querySelector("#h"); const before = h.cellIndex; q.insertBefore(mk("td"), q.firstChild); return before + "," + h.cellIndex + "," + mk("td").cellIndex; })());
            return out.join("\n");
        })()"##
            .to_string(),
    );
    let want = [
        "start:0,0",
        "after:1,1",
        "kind:true,true,[object HTMLCollection]",
        "cells:1,true",
        "nested:1,1,1",
        "directTr:1",
        "index:1,0",
        "moved:1,1",
        "orphan:-1,-1",
        "cellIndex:1,2,-1",
    ];
    assert_eq!(got.lines().collect::<Vec<_>>(), want, "the live collections / positional getters are wrong");
}

// ── 6. the mutation reaches the choke points ────────────────────────────────────────────────────

#[tokio::test]
async fn a_mutation_observer_sees_an_insert_row() {
    let mut e = page().await;
    e.eval_value(
        "l2-mo-arm",
        r#"(() => {
            globalThis.__l2 = [];
            const t = document.createElement("table");
            document.body.appendChild(t);
            globalThis.__l2t = t;
            new MutationObserver((recs) => { for (const r of recs) globalThis.__l2.push(r.type + ":" + r.target.nodeName + ":+" + r.addedNodes.length + ":-" + r.removedNodes.length); })
                .observe(t, { childList: true, subtree: true });
            const r = t.insertRow();
            r.insertCell();
            t.deleteRow(-1);
            return "armed";
        })()"#
            .to_string(),
    )
    .unwrap();
    e.run().await.unwrap();
    let got = str_of(&mut e, "l2-mo-read", "globalThis.__l2.join(\"|\")".to_string());
    assert_eq!(
        got,
        "childList:TABLE:+1:-0|childList:TBODY:+1:-0|childList:TR:+1:-0|childList:TBODY:+0:-1",
        "MutationObserver did not see the table algorithms' mutations"
    );
}

// ── 7. receiver checks ──────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_table_owned_body_refuses_a_foreign_receiver() {
    let mut e = page().await;
    // The deleted helpers answered an empty collection, `null` or `-1` for any element at all;
    // headless Chrome 150 throws "Illegal invocation" for every one of them (probed 2026-09-11) --
    // for a plain object, for a `<div>`, and for an element of a SIBLING table interface.
    let list = BODIED.iter().map(|(i, t)| format!("[{i}, \"{t}\"]")).collect::<Vec<_>>().join(",");
    let src = format!(
        r#"(() => {{
            const bad = [];
            const probe = (label, f) => {{ try {{ f(); bad.push(label + ":no-throw"); }} catch (e) {{ if (!(e instanceof TypeError)) bad.push(label + ":" + e.name); }} }};
            const own = new Set(["caption","tHead","tFoot","tBodies","rows","cellIndex","rowIndex","sectionRowIndex","cells",
                                 "createCaption","createTBody","createTFoot","createTHead","deleteCaption","deleteRow",
                                 "deleteTFoot","deleteTHead","insertRow","insertCell","deleteCell"]);
            for (const [C, tag] of [{list}]) {{
                const P = C.prototype;
                const foreigners = [Object.create(P), document.createElement("div"), document.createElement(tag === "table" ? "tr" : "table")];
                for (const m of Object.getOwnPropertyNames(P)) {{
                    if (!own.has(m)) continue;
                    const d = Object.getOwnPropertyDescriptor(P, m);
                    for (const foreign of foreigners) {{
                        if (d.get) probe(C.name + "." + m + ".get", () => d.get.call(foreign));
                        if (d.set) probe(C.name + "." + m + ".set", () => d.set.call(foreign, null));
                        if (typeof d.value === "function") probe(C.name + "." + m + "()", () => d.value.call(foreign, 0));
                    }}
                }}
            }}
            return bad.join(",");
        }})()"#
    );
    let got = str_of(&mut e, "l2-recv", src);
    assert_eq!(got, "", "a table row answered instead of throwing for a foreign receiver");
}
