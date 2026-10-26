//! LIVE one-table gate for `Range` and `Selection`.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against the bundle's own
//! oracle answers, that the two `interface!` tables reproduce -- byte- and value-identically -- what
//! `dom/08i-range.js` installed with its four `Object.defineProperty` sites over 49 `op_range_*` /
//! `op_sel_*` factory ops:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` LAST --
//!      which is where Chrome 150 puts it (measured 2026-09-11 over loopback:
//!      `Object.getOwnPropertyNames(Range.prototype)` ends `...surroundContents, toString,
//!      constructor`, and `Selection.prototype`'s ends `...toString, constructor`). The JS layer put
//!      `constructor` FIRST -- the class body's position, which `generated_order.rs`'s sort could not
//!      move -- so this is an order tell the table closes.
//!   2. **The interface objects and the chains.** `Range` carries the capture's `length`, `name` and
//!      `Symbol.toStringTag`, its four comparison constants on BOTH holders with the capture's
//!      non-configurable descriptor, and the recorded `Range.prototype -> AbstractRange.prototype`
//!      chain; `Selection` likewise, with no statics.
//!   3. **Failure mode 1 -- no instance left on another prototype.**
//!      `Object.getPrototypeOf(document.createRange()) === Range.prototype`, the same for
//!      `new Range()` and for `getSelection()`, and `getSelection() === document.getSelection()`.
//!      All three hold in Chrome 150, measured.
//!   4. **Every recorded oracle case**, read from the bundle at RUNTIME -- 61 across the two -- so the
//!      value half of the cutover is checked against the capture, not against a value copied here.
//!   5. **Failure mode 3 -- the spec's validity checks still fire.** Nothing in this lane became
//!      newly reachable (all 49 bodies were already mounted by `08i-range.js`), and these cases pin
//!      that: the boundary rejections (`IndexSizeError` / `InvalidNodeTypeError` /
//!      `WrongDocumentError` / `NotSupportedError`) each still throw, with the DOMException NAME
//!      Chrome throws -- measured over loopback, recorded beside each case.
//!   6. **Failure mode 4 -- the install ORDER.** The one later writer of these prototypes was
//!      `BOOTSTRAP`'s "Range geometry" block (two `KB.__` slots behind `op_helper_method`), which
//!      this lane deletes after porting its approximation into `core/dom/range.rs`. So the tables are
//!      the SOLE owner now, and this pins the property that deletion had to preserve: a range's box
//!      still equals the box of the element it selects, and `getClientRects()` is a `DOMRectList`
//!      (the recorded oracle case `getClientRects` used to fail -- the JS body answered an Array).
//!   7. **The live-range registry still tracks DOM edits.** `KB.__ranges` lives in `host.js` (track
//!      W4's file, untouched here) and reaches a range only through the boundary SLOTS the table's
//!      bodies write; a cutover that changed how a boundary is stored would silently stop it.
//!   8. **The arity guards.** Every row whose registry `required` is non-zero throws `TypeError` when
//!      called with fewer arguments, and each function keeps the capture's `name` and `length`.
//!   9. **Failure mode 2 -- foreign receivers.** Every one of the 42 members of the two prototypes
//!      throws Chrome's `TypeError: Illegal invocation` for a receiver that is not a Range /
//!      Selection, and the receiver check runs BEFORE the arity check, as Chrome's does.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=host><p id=a>hello world</p><p id=b>second</p></div></body></html>";

const IFACES: &[&str] = &["Range", "Selection"];

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
        // A WebIDL constant: the capture spells it `kind: "value"`, non-configurable and
        // non-writable on both holders.
        "value" => format!(
            "{n}:data:e={}:c={}:w=0",
            m["enumerable"].as_bool().unwrap() as u8,
            m["configurable"].as_bool().unwrap() as u8
        ),
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

// -- 1. the two prototypes, byte-identical to the capture ------------------------------------------

#[tokio::test]
async fn every_l9_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l9-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

/// The interface OBJECTS: `length` / `name` / the prototype's `Symbol.toStringTag`, and -- for
/// `Range` -- the four statics the capture records on the constructor, in its order and with its
/// descriptor. `Selection` records no statics, so the constructor must carry none.
#[tokio::test]
async fn every_l9_interface_object_matches_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                {DESCRIBE}
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length", "name", "prototype"].includes(n));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name,
                        statics.map(n => {{ const d = Object.getOwnPropertyDescriptor(C, n);
                            return `${{n}}=${{d.value}}:e=${{+d.enumerable}}:c=${{+d.configurable}}:w=${{+d.writable}}`; }}).join(",")].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l9-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        let want_statics: Vec<String> = fx["interfaces"][iface]["statics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                format!(
                    "{}={}:e={}:c={}:w=0",
                    s["name"].as_str().unwrap(),
                    s["value"],
                    s["enumerable"].as_bool().unwrap() as u8,
                    s["configurable"].as_bool().unwrap() as u8
                )
            })
            .collect();
        assert_eq!(
            got,
            format!("{iface}|0|0|1|{want_len}|{iface}|{}", want_statics.join(",")),
            "{iface} interface object != the capture"
        );
    }
}

// -- 2/3. the chains and failure mode 1 ------------------------------------------------------------

/// Failure mode 1: an instance left on some other prototype than the table's. Every way of minting a
/// `Range` -- `new Range()`, `document.createRange()`, `cloneRange()`, `Selection.getRangeAt()` --
/// must land on `Range.prototype`, and `getSelection()` on `Selection.prototype`. The recorded
/// `Range.prototype -> AbstractRange.prototype` chain (and the constructor-inheritance splice) rides
/// along. Chrome 150, measured over loopback: all true.
#[tokio::test]
async fn every_l9_instance_is_minted_on_the_tables_prototype() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-mint",
        r#"(() => {
            const P = Object.getPrototypeOf;
            const s = getSelection();
            s.selectAllChildren(document.getElementById("a"));
            const out = [];
            out.push("new:" + (P(new Range()) === Range.prototype));
            out.push("createRange:" + (P(document.createRange()) === Range.prototype));
            out.push("cloneRange:" + (P(document.createRange().cloneRange()) === Range.prototype));
            out.push("getRangeAt:" + (P(s.getRangeAt(0)) === Range.prototype));
            out.push("selection:" + (P(s) === Selection.prototype));
            out.push("one-selection:" + (getSelection() === document.getSelection()));
            // Chrome 152: `Range` extends `NodeRange` (`startContainer` / `endContainer`), which extends `AbstractRange`
            out.push("chain:" + (P(Range.prototype) === NodeRange.prototype && P(NodeRange.prototype) === AbstractRange.prototype) +
                     ":" + (P(Range) === NodeRange && P(NodeRange) === AbstractRange));
            out.push("instanceof:" + (document.createRange() instanceof Range) +
                     ":" + (document.createRange() instanceof AbstractRange));
            out.push("tag:" + Object.prototype.toString.call(document.createRange()) +
                     ":" + Object.prototype.toString.call(s));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "new:true",
            "createRange:true",
            "cloneRange:true",
            "getRangeAt:true",
            "selection:true",
            "one-selection:true",
            "chain:true:true",
            "instanceof:true:true",
            "tag:[object Range]:[object Selection]",
        ]
        .join("\n")
    );
}

/// Every member of both prototypes reports `[native code]`, and no `ownTell` -- the property the
/// whole one-table rule exists to keep. A row mounted as a JS function would show its source here.
#[tokio::test]
async fn every_l9_member_is_native() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-native",
        r#"(() => {
            const bad = [];
            for (const iface of ["Range", "Selection"]) {
                const P = globalThis[iface].prototype;
                for (const n of Object.getOwnPropertyNames(P)) {
                    // `constructor` is the interface OBJECT, whose own keys are the WebIDL statics
                    // (Range's four comparison constants) -- test 2 checks those against the capture.
                    if (n === "constructor") continue;
                    const d = Object.getOwnPropertyDescriptor(P, n);
                    for (const f of [d.value, d.get, d.set]) {
                        if (typeof f !== "function") continue;
                        const src = Function.prototype.toString.call(f);
                        if (!/\[native code\]/.test(src)) bad.push(iface + "." + n + ": " + src.slice(0, 60));
                        if (Object.getOwnPropertyNames(f).some(k => !["length", "name", "prototype"].includes(k)))
                            bad.push(iface + "." + n + ": own tell " + Object.getOwnPropertyNames(f));
                    }
                }
            }
            return bad.join("\n") || "clean";
        })()"#
            .to_string(),
    );
    assert_eq!(got, "clean");
}

// -- 4. every recorded oracle case ----------------------------------------------------------------

/// The bundle's own recorded answers for the two, read at RUNTIME from
/// `<bundle>/oracle/<Iface>.json`. Nothing is copied into this file: if the bundle is re-captured the
/// expectations move with it.
#[tokio::test]
async fn every_l9_oracle_case_answers_what_the_capture_recorded() {
    let mut e = page().await;
    let dir = persona::shipped_artifact("oracle");
    let mut checked = 0usize;
    for iface in IFACES {
        let raw = std::fs::read_to_string(dir.join(format!("{iface}.json"))).unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        for c in &cases {
            let expr = c["expr"].as_str().unwrap();
            let src = format!(
                r#"(() => {{ try {{ const v = ({expr});
                    return v === undefined ? "U" : "V" + JSON.stringify(v);
                }} catch (err) {{ return "T" + (err && err.name ? err.name
                    : err && err.constructor ? err.constructor.name : String(err)); }} }})()"#
            );
            let got = str_of(&mut e, "l9-oracle", src);
            let want = if let Some(t) = c["throws"].as_str() {
                format!("T{t}")
            } else if c["undefined"].as_bool() == Some(true) {
                "U".to_string()
            } else {
                format!("V{}", serde_json::to_string(&c["expected"]).unwrap())
            };
            assert_eq!(got, want, "{iface}.{} -- {expr}", c["member"].as_str().unwrap());
            checked += 1;
        }
    }
    assert!(checked >= 55, "only {checked} oracle cases ran for Range + Selection");
}

// -- 5. failure mode 3: the validity checks still fire ---------------------------------------------

/// Every mutation member that rejects a boundary still rejects it, with the DOMException NAME Chrome
/// throws. Measured in Chrome 150 over loopback on this very page (2026-09-11); the engine's MESSAGE
/// text differs from Chrome's (`"offset out of range"` vs `"Failed to execute 'setStart' on 'Range':
/// The offset 999 is larger than the node's length (11)."`) -- a body-level fidelity gap this lane
/// does not touch, since the bodies did not move. The NAME and the throw/no-throw verdict are what
/// this pins, and they are Chrome's.
#[tokio::test]
async fn the_l9_boundary_rejections_still_throw_what_chrome_throws() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-validity",
        r#"(() => {
            const R = () => document.createRange();
            const t = document.getElementById("a").firstChild;
            const N = (fn) => { try { const v = fn(); return v === undefined ? "ok" : "ok:" + v; }
                                catch (err) { return err.name; } };
            const out = [];
            // Chrome: IndexSizeError -- the unsigned-long conversion makes -1 offset 4294967295.
            out.push("setStart-neg:" + N(() => R().setStart(t, -1)));
            out.push("setStart-big:" + N(() => R().setStart(t, 999)));
            out.push("setEnd-big:" + N(() => R().setEnd(t, 999)));
            // Chrome: InvalidNodeTypeError for a doctype boundary container.
            out.push("setStart-doctype:" + N(() => R().setStart(document.doctype, 0)));
            out.push("setEnd-doctype:" + N(() => R().setEnd(document.doctype, 0)));
            out.push("selectNodeContents-doctype:" + N(() => R().selectNodeContents(document.doctype)));
            // Chrome: InvalidNodeTypeError -- "the given Node has no parent".
            out.push("selectNode-doc:" + N(() => R().selectNode(document)));
            out.push("setStartBefore-doc:" + N(() => R().setStartBefore(document)));
            out.push("setStartAfter-doc:" + N(() => R().setStartAfter(document)));
            out.push("setEndBefore-doc:" + N(() => R().setEndBefore(document)));
            out.push("setEndAfter-doc:" + N(() => R().setEndAfter(document)));
            // Chrome: WrongDocumentError from comparePoint, but `false` (not a throw) from
            // isPointInRange -- the one place the two members disagree.
            const other = document.implementation.createHTMLDocument();
            out.push("comparePoint-foreign:" + N(() => R().comparePoint(other.body, 0)));
            out.push("isPointInRange-foreign:" + N(() => R().isPointInRange(other.body, 0)));
            out.push("comparePoint-doctype:" + N(() => R().comparePoint(document.doctype, 0)));
            out.push("comparePoint-big:" + N(() => R().comparePoint(t, 999)));
            // Chrome: NotSupportedError for a `how` outside 0..3 (ToUint16, so 65536 IS 0).
            out.push("cbp-4:" + N(() => { const a = R(), b = R(); return a.compareBoundaryPoints(4, b); }));
            out.push("cbp-65536:" + N(() => { const a = R(), b = R(); return a.compareBoundaryPoints(65536, b); }));
            // Chrome: InvalidStateError from Selection when there is no range at all.
            const s = getSelection();
            s.removeAllRanges();
            out.push("extend-empty:" + N(() => s.extend(document.body, 0)));
            out.push("collapseToStart-empty:" + N(() => s.collapseToStart()));
            out.push("collapseToEnd-empty:" + N(() => s.collapseToEnd()));
            out.push("getRangeAt-1:" + N(() => s.getRangeAt(1)));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "setStart-neg:IndexSizeError",
            "setStart-big:IndexSizeError",
            "setEnd-big:IndexSizeError",
            "setStart-doctype:InvalidNodeTypeError",
            "setEnd-doctype:InvalidNodeTypeError",
            "selectNodeContents-doctype:InvalidNodeTypeError",
            "selectNode-doc:InvalidNodeTypeError",
            "setStartBefore-doc:InvalidNodeTypeError",
            "setStartAfter-doc:InvalidNodeTypeError",
            "setEndBefore-doc:InvalidNodeTypeError",
            "setEndAfter-doc:InvalidNodeTypeError",
            "comparePoint-foreign:WrongDocumentError",
            "isPointInRange-foreign:ok:false",
            "comparePoint-doctype:InvalidNodeTypeError",
            "comparePoint-big:IndexSizeError",
            "cbp-4:NotSupportedError",
            "cbp-65536:ok:0",
            "extend-empty:InvalidStateError",
            "collapseToStart-empty:InvalidStateError",
            "collapseToEnd-empty:InvalidStateError",
            "getRangeAt-1:IndexSizeError",
        ]
        .join("\n")
    );
}

// -- 6. failure mode 4: the install ORDER ----------------------------------------------------------

/// The two geometry rows, whose bodies moved OUT of `BOOTSTRAP` and into `core/dom/range.rs` in the
/// same commit that made the tables the sole owner. A range's box is what it covers (Blink's
/// `Range::GetBorderAndTextQuads`): over a paragraph's contents, its text -- from the paragraph's corner, as
/// tall, narrower than the block -- and over the paragraph itself, the paragraph's box (Chrome 150). And
/// `getClientRects()` must be a `DOMRectList`, the type the bundle's oracle records and the JS body did not
/// produce for a collapsed range.
#[tokio::test]
async fn the_table_owns_the_two_geometry_bodies() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-geometry",
        r#"(() => {
            const el = document.getElementById("a");
            const r = document.createRange();
            r.selectNodeContents(el);
            const a = r.getBoundingClientRect(), b = el.getBoundingClientRect();
            const fmt = (x) => [x.x, x.y, x.width, x.height].join(",");
            const whole = document.createRange();
            whole.selectNode(el);
            return [
                "text:" + (a.x === b.x && a.y === b.y && a.height === b.height && a.width > 0 && a.width < b.width),
                "node:" + (fmt(whole.getBoundingClientRect()) === fmt(b)),
                "rects:" + r.getClientRects().length + ":" + whole.getClientRects().length,
                // A COLLAPSED range covers nothing -- but still answers the platform TYPES.
                "collapsed:" + fmt(document.createRange().getBoundingClientRect()),
                "type:" + Object.prototype.toString.call(r.getClientRects()) +
                    ":" + Object.prototype.toString.call(document.createRange().getClientRects()),
                "notarray:" + Array.isArray(document.createRange().getClientRects()),
                "native:" + /\[native code\]/.test(Function.prototype.toString.call(Range.prototype.getBoundingClientRect)),
            ].join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "text:true",
            "node:true",
            "rects:1:2",
            "collapsed:0,0,0,0",
            "type:[object DOMRectList]:[object DOMRectList]",
            "notarray:false",
            "native:true",
        ]
        .join("\n")
    );
}

// -- 7. the live-range registry -------------------------------------------------------------------

/// `KB.__ranges` (in `core/korvus-runtime/src/js/host.js`, track W4's file and untouched by this lane)
/// adjusts every live range's boundary offsets when character data moves under it. It reaches a range
/// only through the private boundary SLOTS the table's bodies write, so a cutover that changed how a
/// boundary is stored would silently stop it -- with no surface diff to show for it.
#[tokio::test]
async fn the_live_range_registry_still_tracks_dom_edits() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-live",
        r#"(() => {
            const t = document.getElementById("a").firstChild;   // "hello world"
            const r = document.createRange();
            r.setStart(t, 6); r.setEnd(t, 11);
            const out = ["before:" + r.startOffset + "/" + r.endOffset + ":" + r.toString()];
            t.insertData(0, "XY");
            out.push("insert:" + r.startOffset + "/" + r.endOffset + ":" + r.toString());
            t.deleteData(0, 2);
            out.push("delete:" + r.startOffset + "/" + r.endOffset + ":" + r.toString());
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(got, ["before:6/11:world", "insert:8/13:world", "delete:6/11:world"].join("\n"));
}

// -- 8. the arity guards ---------------------------------------------------------------------------

/// Every row the registry gave a non-zero `required` throws `TypeError` below that count, and every
/// function keeps the capture's `name` and `length`. The guards used to come from `08i-range.js`'s
/// `guard()` helper and then from the registry's arity pass; now they come from the row's own
/// `required`, through the one `installer` mount.
#[tokio::test]
async fn every_l9_required_argument_is_guarded() {
    let mut e = page().await;
    let fx = bundle();
    let got = str_of(
        &mut e,
        "l9-arity",
        r#"(() => {
            const need = {
                Range: { compareBoundaryPoints: 2, comparePoint: 2, createContextualFragment: 1, insertNode: 1,
                         intersectsNode: 1, isPointInRange: 2, selectNode: 1, selectNodeContents: 1, setEnd: 2,
                         setEndAfter: 1, setEndBefore: 1, setStart: 2, setStartAfter: 1, setStartBefore: 1,
                         surroundContents: 1 },
                Selection: { addRange: 1, collapse: 1, containsNode: 1, extend: 1, getRangeAt: 1, removeRange: 1,
                             selectAllChildren: 1, setBaseAndExtent: 4, setPosition: 1 },
            };
            const recv = { Range: () => document.createRange(), Selection: () => getSelection() };
            const bad = [];
            for (const iface of ["Range", "Selection"]) {
                for (const [m, n] of Object.entries(need[iface])) {
                    const args = new Array(n - 1).fill(undefined);
                    try { recv[iface]()[m](...args); bad.push(iface + "." + m + ": no throw at " + (n - 1)); }
                    catch (err) { if (err.name !== "TypeError") bad.push(iface + "." + m + ": " + err.name); }
                }
            }
            return bad.join("\n") || "clean";
        })()"#
            .to_string(),
    );
    assert_eq!(got, "clean");

    // The capture's `name` and `length` on every method of both prototypes.
    for iface in IFACES {
        let src = format!(
            r#"(() => Object.getOwnPropertyNames({iface}.prototype).map(n => {{
                const d = Object.getOwnPropertyDescriptor({iface}.prototype, n);
                return typeof d.value === "function" ? `${{n}}:${{d.value.name}}/${{d.value.length}}`
                     : d.get ? `${{n}}:${{d.get.name}}/${{d.get.length}}` : null;
            }}).filter(Boolean).join("\n"))()"#
        );
        let got = str_of(&mut e, "l9-names", src);
        let want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| {
                let n = m["name"].as_str().unwrap();
                match m["kind"].as_str().unwrap() {
                    "method" => Some(format!("{n}:{}/{}", m["fnName"].as_str().unwrap(), m["length"].as_u64().unwrap())),
                    "accessor" => Some(format!("{n}:{}/{}", m["getName"].as_str().unwrap(), m["getLength"].as_u64().unwrap())),
                    _ => None,
                }
            })
            .chain(std::iter::once("constructor:{}/0".replace("{}", iface)))
            .collect();
        assert_eq!(got.lines().collect::<Vec<_>>(), want, "{iface} member name/length != the capture");
    }
}

// -- 9. the four legacy Selection aliases ----------------------------------------------------------

/// `baseNode` / `baseOffset` / `extentNode` / `extentOffset` are the legacy WebKit spellings of
/// `anchorNode` / `anchorOffset` / `focusNode` / `focusOffset`. Chrome 150, measured over loopback on
/// a live selection: each `===` its modern twin, and on an empty selection they read `null` / `0`.
///
/// Before this lane they were the generated registry's D2 stub installed with `Flavor::Shell`, whose
/// receiver check wants a brand the `host.js` `Selection` instance does not carry -- so reading
/// `getSelection().baseNode` threw `TypeError: Illegal invocation` on the ONE real selection in the
/// realm. The table gives all four the anchor/focus bodies, which is what Chrome answers.
#[tokio::test]
async fn the_legacy_selection_aliases_answer_their_modern_twins() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-aliases",
        r#"(() => {
            const s = getSelection();
            s.removeAllRanges();
            const out = ["empty:" + [s.baseNode, s.baseOffset, s.extentNode, s.extentOffset].join(",")];
            s.selectAllChildren(document.getElementById("a"));
            out.push("live:" + [s.baseNode === s.anchorNode, s.baseOffset === s.anchorOffset,
                                s.extentNode === s.focusNode, s.extentOffset === s.focusOffset].join(","));
            out.push("values:" + s.baseNode.nodeName + "," + s.baseOffset + "," +
                     s.extentNode.nodeName + "," + s.extentOffset);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(got, ["empty:,0,,0", "live:true,true,true,true", "values:P,0,P,1"].join("\n"));
}

// -- 10. failure mode 2: foreign receivers ---------------------------------------------------------

/// Chrome 150, measured over loopback (2026-09-11): EVERY member of both prototypes -- all 25 Range
/// methods, `commonAncestorContainer`'s getter, all 17 Selection methods and all 12 Selection getters
/// -- answers `TypeError: Illegal invocation` for a receiver that is not one of its platform objects,
/// and `Object.create(Range.prototype).toString()` throws it too.
///
/// Measured on this tree BEFORE the cutover: not one of the 42 threw. They answered `undefined`,
/// `false`, `""` or `0` -- and `Range.prototype.createContextualFragment.call({}, "<b>x</b>")`
/// returned a real DocumentFragment, `Range.prototype.getBoundingClientRect.call({})` a real DOMRect.
/// The table mounts every row through `install::live::branded_method_row` / `branded_getter_row`.
///
/// The receiver check runs BEFORE the arity check, which is the order Chrome uses:
/// `Range.prototype.setStart.call({})` is `Illegal invocation`, never "2 arguments required".
#[tokio::test]
async fn every_l9_member_rejects_a_foreign_receiver() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-receiver",
        r#"(() => {
            // Receivers a page or a detector actually produces. `Object.create(Iface.prototype)` has
            // the prototype in its chain and is still not an instance -- Chrome rejects it, and so
            // does the brand (the interface's own storage is missing).
            const foreign = (iface) => [
                {}, globalThis, document, document.body, "s", 7, [],
                Object.create(globalThis[iface].prototype),
                globalThis[iface].prototype,
                iface === "Range" ? getSelection() : document.createRange(),
            ];
            const bad = [];
            for (const iface of ["Range", "Selection"]) {
                const P = globalThis[iface].prototype;
                for (const n of Object.getOwnPropertyNames(P)) {
                    if (n === "constructor") continue;
                    const d = Object.getOwnPropertyDescriptor(P, n);
                    const fn = typeof d.value === "function" ? d.value : d.get;
                    if (typeof fn !== "function") continue;   // the four Range constants
                    for (const recv of foreign(iface)) {
                        let verdict;
                        try {
                            const v = fn.call(recv, document.body, 0, document.body, 0);
                            verdict = "returned " + Object.prototype.toString.call(v);
                        } catch (err) { verdict = err.name + ": " + err.message; }
                        if (verdict !== "TypeError: Illegal invocation")
                            bad.push(iface + "." + n + " on " + Object.prototype.toString.call(recv) + " -> " + verdict);
                    }
                }
            }
            // The receiver check is OUTSIDE the arity guard, as in Chrome.
            try { Range.prototype.setStart.call({}); }
            catch (err) { if (err.message !== "Illegal invocation") bad.push("order: " + err.message); }
            // And a REAL receiver still reaches the body.
            const r = document.createRange();
            if (Range.prototype.toString.call(r) !== "") bad.push("real Range receiver rejected");
            if (Selection.prototype.toString.call(getSelection()) !== "") bad.push("real Selection receiver rejected");
            return bad.join("\n") || "clean";
        })()"#
            .to_string(),
    );
    assert_eq!(got, "clean");
}

// -- 4. the DOM §4.4 ALGORITHMS, against Chrome ----------------------------------------

/// The unit that moved these bodies also changed what they COMPUTE, so the gate that matters is not a
/// shape gate: it is the spec's own worked cases, with the answers taken from the real browser.
///
/// EVERY expected string below is a RECORDING, produced by running the identical script under
/// `google-chrome --headless=new --dump-dom` on 2026-09-11 and pasting what it printed. Nothing here
/// was derived from this engine's output. The cases are chosen to be the ones an approximation gets
/// wrong -- a range that ENTERS an element without swallowing it, which is what
/// `extractContents`/`cloneContents` had no representation for until W4.e, plus the refusals and the
/// collapse points that came with them.
///
/// WHY IT IS HERE AND NOT IN THE WPT SCOREBOARD: the five WPT files that cover these members
/// (`Range-extractContents.html`, `-cloneContents.html`, `-surroundContents.html`,
/// `-deleteContents.html`, `-insertNode.html`) all drive an iframe, and the runner skips any file
/// that creates a nested browsing context. So the suite's dom/ranges number, which this unit moved by
/// 157 subtests, contains NO direct coverage of the recursion at all; without this test the headline
/// algorithm change would be graded only through the four shadow-DOM files that happen to exercise it.
#[tokio::test]
async fn the_range_algorithms_answer_what_chrome_answers() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l9-algorithms",
        r#"(() => {
            const out = [];
            const mk = (html) => { const d = document.createElement("div"); d.innerHTML = html; document.body.appendChild(d); return d; };
            const fragHtml = (f) => [...f.childNodes].map((n) => n.outerHTML !== undefined ? n.outerHTML : JSON.stringify(n.nodeValue)).join("|");

            // The partially-contained recursion: a range from inside one <p> into the next.
            { const d = mk("<p>abcd</p><p>efgh</p>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild, 2); r.setEnd(d.lastChild.firstChild, 2);
              out.push("clone-recurse = " + fragHtml(r.cloneContents()));
              out.push("clone-src-unchanged = " + d.innerHTML); }
            { const d = mk("<p>abcd</p><p>efgh</p>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild, 2); r.setEnd(d.lastChild.firstChild, 2);
              out.push("extract-frag = " + fragHtml(r.extractContents()));
              out.push("extract-left = " + d.innerHTML);
              out.push("extract-collapse = " + (r.startContainer === d ? "div" : r.startContainer.nodeName) + "," + r.startOffset + " collapsed=" + r.collapsed); }
            // Two levels of it, so a single level of special-casing cannot pass.
            { const d = mk("<div><b>xy</b></div><div><i>zw</i></div>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild.firstChild, 1); r.setEnd(d.lastChild.firstChild.firstChild, 1);
              out.push("deep-clone = " + fragHtml(r.cloneContents())); }
            // deleteContents collapses OUTSIDE the subtree it emptied, not to the start boundary.
            { const d = mk("<p>abcd</p><p>efgh</p>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild, 2); r.setEnd(d.lastChild.firstChild, 2);
              r.deleteContents();
              out.push("delete-left = " + d.innerHTML);
              out.push("delete-collapse = " + (r.startContainer === d ? "div" : r.startContainer.nodeName) + "," + r.startOffset); }
            // surroundContents: the two refusals, and the emptying of newParent.
            { const d = mk("<p>ab<b>cd</b></p>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild, 1); r.setEnd(d.firstChild.lastChild.firstChild, 1);
              try { r.surroundContents(document.createElement("u")); out.push("surround-partial = NO-THROW"); }
              catch (err) { out.push("surround-partial = " + err.name); } }
            { const d = mk("<p>abcd</p>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild, 1); r.setEnd(d.firstChild.firstChild, 3);
              try { r.surroundContents(document.createDocumentFragment()); out.push("surround-frag = NO-THROW"); }
              catch (err) { out.push("surround-frag = " + err.name); } }
            { const d = mk("<p>abcd</p>"); const r = document.createRange();
              r.setStart(d.firstChild.firstChild, 1); r.setEnd(d.firstChild.firstChild, 3);
              const u = document.createElement("u"); u.appendChild(document.createTextNode("DROPME"));
              r.surroundContents(u);
              out.push("surround-ok = " + d.innerHTML);
              out.push("surround-sel = " + r.startContainer.nodeName + "," + r.startOffset + ".." + r.endOffset); }
            // The live registry: splitText detached (clamp) and attached (the parent bump).
            { const t = document.createTextNode("Uvwxyzab"); const r = document.createRange();
              r.setStart(t, 1); r.setEnd(t, 3); t.splitText(1);
              out.push("split-detached = start:" + JSON.stringify(r.startContainer.nodeValue) + "," + r.startOffset +
                       " end:" + JSON.stringify(r.endContainer.nodeValue) + "," + r.endOffset); }
            { const d = mk("<p>ab</p>"); const t = d.firstChild.firstChild; const r = document.createRange();
              r.setStart(d.firstChild, 1); r.setEnd(d.firstChild, 1); t.splitText(1);
              out.push("split-parent-bump = " + r.startOffset); }
            // A move is a remove THEN an insert: a range inside the moved node comes out to its old place.
            { const d = mk("<p>a</p><p>b</p>"); const r = document.createRange();
              r.setStart(d.lastChild, 0); r.setEnd(d.lastChild, 0);
              d.appendChild(d.lastChild);
              out.push("move-out = " + (r.startContainer === d ? "div" : r.startContainer.nodeName) + "," + r.startOffset); }
            // A doctype among the contained children is refused before anything moves.
            { const doc = document.implementation.createHTMLDocument("t"); const r = doc.createRange();
              r.setStart(doc, 0); r.setEnd(doc, doc.childNodes.length);
              try { r.extractContents(); out.push("doctype = NO-THROW"); } catch (err) { out.push("doctype = " + err.name); } }
            // "replace data" moves a boundary that sat inside the replaced run to its start.
            { const t = document.createTextNode("0123456789"); document.body.appendChild(t);
              const r = document.createRange(); r.setStart(t, 4); r.setEnd(t, 8);
              t.replaceData(2, 4, "XY");
              out.push("replace-data = " + r.startOffset + ".." + r.endOffset); }
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "clone-recurse = <p>cd</p>|<p>ef</p>",
            "clone-src-unchanged = <p>abcd</p><p>efgh</p>",
            "extract-frag = <p>cd</p>|<p>ef</p>",
            "extract-left = <p>ab</p><p>gh</p>",
            "extract-collapse = div,1 collapsed=true",
            "deep-clone = <div><b>y</b></div>|<div><i>z</i></div>",
            "delete-left = <p>ab</p><p>gh</p>",
            "delete-collapse = div,1",
            "surround-partial = InvalidStateError",
            "surround-frag = InvalidNodeTypeError",
            "surround-ok = <p>a<u>bc</u>d</p>",
            "surround-sel = P,1..2",
            "split-detached = start:\"U\",1 end:\"U\",1",
            "split-parent-bump = 2",
            "move-out = div,1",
            "doctype = HierarchyRequestError",
            "replace-data = 2..6",
        ]
        .join("\n"),
        "a DOM §4.4 algorithm disagrees with the recorded Chrome 150 answer"
    );
}


// -- 10. `AbstractRange` and `StaticRange`: the two tables W4.j added beside these ------------------

/// The interface OBJECTS, which is the half `host.js` still owned.
///
/// `AbstractRange` was a JS `class` with a `collapsed` getter whose PROTOTYPE the `AbstractRange`
/// table then took over through `take_prototype`, and `StaticRange` was
/// `class StaticRange extends globalThis.AbstractRange`. The members were therefore already right
/// (they are the same slots and the same native accessors `Range` uses, asserted above); the
/// IDENTITIES were not, and everything below was measured on `google-chrome --headless=new`
/// 150.0.7871.46 over a `file:` URL rather than reasoned about.
#[tokio::test]
async fn the_two_abstract_range_interfaces_are_native_not_js_classes() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "w4j-ifaces",
        r#"(() => {
        const out = [];
        const err = (f) => { try { f(); return "OK"; } catch (e) { return e.constructor.name + "|" + e.name + "|" + e.message; } };
        for (const n of ["AbstractRange", "StaticRange"]) {
            const C = globalThis[n];
            out.push(n + ".native=" + String(C).includes("[native code]") + " name=" + C.name + " length=" + C.length);
            const pd = Object.getOwnPropertyDescriptor(C, "prototype");
            out.push(n + ".prototype=w" + (+pd.writable) + "e" + (+pd.enumerable) + "c" + (+pd.configurable)
                + " tag=" + Object.prototype.toString.call(C.prototype));
        }
        // `StaticRange.prototype`'s ONLY own key is `constructor`: every boundary member belongs to
        // `AbstractRange.prototype`, which is what makes the table row-less.
        out.push("SR.protoKeys=" + Object.getOwnPropertyNames(StaticRange.prototype).join(","));
        // Chrome 152: through `NodeRange`, as `Range`'s chain goes
        out.push("SR.chain=" + (Object.getPrototypeOf(StaticRange.prototype) === NodeRange.prototype)
            + "," + (Object.getPrototypeOf(StaticRange) === NodeRange));
        // `AbstractRange` is `[interface]`: WebIDL declares no constructor.
        out.push("AR.new=" + err(() => new AbstractRange()));
        out.push("SR.nonew=" + err(() => StaticRange({})));
        return out.join("\n");
    })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "AbstractRange.native=true name=AbstractRange length=0",
            "AbstractRange.prototype=w0e0c0 tag=[object AbstractRange]",
            "StaticRange.native=true name=StaticRange length=1",
            "StaticRange.prototype=w0e0c0 tag=[object StaticRange]",
            "SR.protoKeys=constructor",
            "SR.chain=true,true",
            // A JS class with a getter is CONSTRUCTIBLE, so this used to answer "OK".
            "AR.new=TypeError|TypeError|Failed to construct 'AbstractRange': Illegal constructor",
            // ...and this used to be V8's "Class constructor StaticRange cannot be invoked without 'new'".
            "SR.nonew=TypeError|TypeError|Failed to construct 'StaticRange': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
        ],
        "an AbstractRange/StaticRange interface object disagrees with Chrome 152"
    );
}

/// `new StaticRange(init)`: the WebIDL dictionary conversion, every branch of it.
///
/// The JS constructor had ONE check -- "is each of the four `undefined` or `null`" -- and one message
/// per outcome. Each line below is Chrome's answer to the same call, and each was a difference.
#[tokio::test]
async fn the_static_range_constructor_converts_its_dictionary_like_chrome() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "w4j-sr-ctor",
        r#"(() => {
        const out = [];
        const err = (label, f) => { try { f(); out.push(label + "=OK"); } catch (e) { out.push(label + "=" + e.name + "|" + e.message); } };
        const b = document.body, host = document.getElementById("host");
        err("noargs", () => new StaticRange());
        err("null", () => new StaticRange(null));
        err("num", () => new StaticRange(5));
        // WebIDL reads dictionary members in LEXICOGRAPHIC order, so an empty init blames
        // `endContainer` -- not `startContainer`, which is what boundary order would blame.
        err("empty", () => new StaticRange({}));
        err("startOnly", () => new StaticRange({ startContainer: b, startOffset: 0 }));
        err("noEndOffset", () => new StaticRange({ startContainer: b, startOffset: 0, endContainer: b }));
        err("nullNode", () => new StaticRange({ startContainer: null, startOffset: 0, endContainer: b, endOffset: 0 }));
        err("numNode", () => new StaticRange({ startContainer: 5, startOffset: 0, endContainer: b, endOffset: 0 }));
        err("doctype", () => new StaticRange({ startContainer: document.doctype, startOffset: 0, endContainer: b, endOffset: 0 }));
        // A REAL `Attr` (nodeType 2), off an element's own attribute map. NOT
        // `document.createAttribute("x")`, which answers `null` in this engine -- a gap of its own,
        // outside this table, and one that would have made this line assert the wrong reason
        // ("Failed to convert value to 'Node'", because `null` is not one).
        const attr = (() => { const d = document.createElement("div"); d.setAttribute("q", "1"); return d.attributes[0]; })();
        err("attr", () => new StaticRange({ startContainer: attr, startOffset: 0, endContainer: b, endOffset: 0 }));
        // `unsigned long` with neither [EnforceRange] nor [Clamp] is the MODULAR conversion: none of
        // these throw, and none of them keep the value they were given.
        const off = (v) => new StaticRange({ startContainer: b, startOffset: v, endContainer: b, endOffset: 0 }).startOffset;
        out.push("offsets=" + [off(-1), off(4294967296), off(1.7), off(NaN), off(9999)].join(","));
        // An instance owns NOTHING: the four members are private slots read by
        // `AbstractRange.prototype`'s accessors, shared with `Range`.
        const r = new StaticRange({ startContainer: host, startOffset: 1, endContainer: host, endOffset: 1 });
        out.push("own=[" + Object.getOwnPropertyNames(r).join(",") + "]");
        out.push("vals=" + (r.startContainer === host) + "," + r.startOffset + "," + (r.endContainer === host)
            + "," + r.endOffset + "," + r.collapsed);
        out.push("inst=" + (r instanceof StaticRange) + "," + (r instanceof AbstractRange)
            + " proto=" + (Object.getPrototypeOf(r) === StaticRange.prototype));
        return out.join("\n");
    })()"#
            .to_string(),
    );
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        vec![
            "noargs=TypeError|Failed to construct 'StaticRange': 1 argument required, but only 0 present.",
            "null=TypeError|Failed to construct 'StaticRange': The provided value is not of type 'StaticRangeInit'.",
            "num=TypeError|Failed to construct 'StaticRange': The provided value is not of type 'StaticRangeInit'.",
            "empty=TypeError|Failed to construct 'StaticRange': Failed to read the 'endContainer' property from 'StaticRangeInit': Required member is undefined.",
            "startOnly=TypeError|Failed to construct 'StaticRange': Failed to read the 'endContainer' property from 'StaticRangeInit': Required member is undefined.",
            "noEndOffset=TypeError|Failed to construct 'StaticRange': Failed to read the 'endOffset' property from 'StaticRangeInit': Required member is undefined.",
            "nullNode=TypeError|Failed to construct 'StaticRange': Failed to read the 'startContainer' property from 'StaticRangeInit': Failed to convert value to 'Node'.",
            "numNode=TypeError|Failed to construct 'StaticRange': Failed to read the 'startContainer' property from 'StaticRangeInit': Failed to convert value to 'Node'.",
            "doctype=InvalidNodeTypeError|Failed to construct 'StaticRange': Neither startContainer nor endContainer can be a DocumentType or Attribute node.",
            "attr=InvalidNodeTypeError|Failed to construct 'StaticRange': Neither startContainer nor endContainer can be a DocumentType or Attribute node.",
            // -1 -> 2^32-1, 2^32 -> 0, 1.7 -> 1, NaN -> 0, and an offset beyond the container's
            // length is NOT clamped: a static range is a snapshot, not a validated position.
            "offsets=4294967295,0,1,0,9999",
            "own=[]",
            "vals=true,1,true,1,true",
            "inst=true,true proto=true",
        ],
        "the StaticRange constructor disagrees with Chrome 150"
    );
}
