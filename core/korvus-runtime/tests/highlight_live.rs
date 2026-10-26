//! LIVE one-table gate for the CSS Custom Highlight API.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against answers recorded
//! from `google-chrome --headless=new --dump-dom` over a loopback server (2026-09-11), that the
//! `Highlight` and `HighlightRegistry` `interface!` tables reproduce -- byte- and value-identically --
//! what the deleted 100-line block of `dom/09-postshells.js` installed, and fix what it got wrong.
//!
//!   1. **Byte-identical prototypes**: the bundle's member list, in the capture's order, with the
//!      capture's descriptors.
//!   2. **The interface objects**: tag, length, name, statics, newability, `[native code]`, chain.
//!   3. **No row accepts a foreign receiver** -- the brief's failure mode 2. Chrome throws
//!      `Illegal invocation` from all 22; the JS threw from NONE of them.
//!   4. **The values**, including the two the JS had wrong (`get` on a missing key answered `null`
//!      where Chrome and the bundle's own oracle say `undefined`).
//!   5. **Instances wear the TABLE's prototype and own nothing** -- failure modes 1 and the
//!      instance-tell axis.
//!   6. **The iterator kind**: `[object Highlight Iterator]`, not a bare Set iterator.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body><div id=\"d\">x</div></body></html>";

const IFACES: &[&str] = &["HighlightRegistry", "Highlight"];

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
        "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
        "method" => format!("{n}:data:e=1:c=1:w=1"),
        k => panic!("unexpected kind {k}"),
    }
}

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "hl-probe", src)
}

// -- 1. the prototypes, byte-identical to the capture ---------------------------------------------

#[tokio::test]
async fn both_prototypes_are_byte_identical_to_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let mut want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap_or_else(|| panic!("{iface} absent from the surface"))
            .iter()
            .map(describe_line)
            .collect();
        // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member installed
        // at interface-definition time, so a member a PARTIAL interface adds lands AFTER it -- `Navigator`
        // 32 of 66, `Screen` 9 of 11 with `onchange` and `isExtended` behind it. Appending it was right on
        // 83 % of the surface and wrong on exactly the interfaces a fingerprinting script reads.
        let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(want.len()).min(want.len());
        want.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "hl-desc", src);
        // `constructor` is the registry's leading member and a hand table's trailing one, so compare
        // as a SET plus an order check over the capture's own names (which never include it).
        let mut got_sorted: Vec<&str> = got.lines().collect();
        let mut want_sorted: Vec<&str> = want.iter().map(String::as_str).collect();
        got_sorted.sort_unstable();
        want_sorted.sort_unstable();
        assert_eq!(got_sorted, want_sorted, "{iface}.prototype != the capture");
        let recorded: Vec<&str> =
            fx["interfaces"][iface]["members"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        let live_order: Vec<&str> =
            got.lines().map(|l| l.split(':').next().unwrap()).filter(|n| recorded.contains(n)).collect();
        assert_eq!(live_order, recorded, "{iface}.prototype is not in the capture's order");
    }
}

// -- 2. the interface objects ---------------------------------------------------------------------

#[tokio::test]
async fn both_interface_objects_match_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                let newable = "yes";
                try {{ new C(); }} catch (err) {{ newable = err.constructor.name + ":" + err.message; }}
                const native = /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(C));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name,
                        statics.join(","), newable, native,
                        Object.getPrototypeOf(C.prototype) === Object.prototype].join("|");
            }})()"#
        );
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
        let want_newable = if fx["interfaces"][iface]["newable"].as_bool().unwrap() {
            "yes".to_string()
        } else {
            format!("TypeError:Failed to construct '{iface}': Illegal constructor")
        };
        assert_eq!(
            str_of(&mut e, "hl-ctor", src),
            format!("{iface}|0|0|1|{want_len}|{iface}||{want_newable}|true|true"),
            "{iface} interface object != the capture"
        );
    }
}

// -- 3. no row accepts a foreign receiver ---------------------------------------------------------

/// Failure mode 2. Recorded from Chrome 150 over loopback: all 22 members of the two prototypes --
/// every method, every getter, and both setters -- answer `TypeError: Illegal invocation` for an
/// object that wears the prototype but was never constructed. Measured on this tree BEFORE the
/// cutover: not one of the 22 threw. `HighlightRegistry.prototype.get.call({}, "k")` answered
/// `undefined`, and the JS `st2` helper ADOPTED anything whose chain reached `Highlight.prototype`,
/// so `Object.create(Highlight.prototype).add(r)` silently worked.
#[tokio::test]
async fn every_row_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let src = r#"(() => {
        const bad = [];
        const probe = (label, fn) => {
            try { fn(); bad.push(label + "=no-throw"); }
            catch (err) { if (String(err.message) !== "Illegal invocation") bad.push(label + "=" + err.message); }
        };
        for (const name of ["Highlight", "HighlightRegistry"]) {
            const P = globalThis[name].prototype;
            const foreign = Object.create(P);
            for (const m of Object.getOwnPropertyNames(P)) {
                if (m === "constructor") continue;
                const d = Object.getOwnPropertyDescriptor(P, m);
                if (d.get) probe(name + "." + m + ".get", () => d.get.call(foreign));
                if (d.set) probe(name + "." + m + ".set", () => d.set.call(foreign, "x"));
                if (typeof d.value === "function") probe(name + "." + m + "()", () => d.value.call(foreign, "x", "y"));
            }
        }
        return bad.join(" ");
    })()"#;
    // NO RESIDUAL LEFT. `highlightsFromPoint` -- the one row of the two tables with no Rust
    // body, an honest D2 `stub` (it answers the highlights PAINTED over a point, and this engine paints none)
    // -- answered a foreign receiver while the check was the prototype chain, which `Object.create(P)`
    // satisfies. The receiver's brand is the check now, and a fake has none, so every member of the two
    // prototypes refuses, as Chrome 150's do.
    assert_eq!(
        str_of(&mut e, "hl-foreign", src.to_string()),
        "",
        "a row answered for a foreign receiver where Chrome 150 throws Illegal invocation"
    );
}

// -- 4. the values --------------------------------------------------------------------------------

/// Every expectation here is the bundle's recorded oracle (`oracle/Highlight.json`, 24 cases;
/// `oracle/HighlightRegistry.json`, 21) or a Chrome 150 probe named in the comment.
#[tokio::test]
async fn the_highlight_values_are_the_recorded_ones() {
    let mut e = page().await;
    // The setlike, and the two attributes.
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(new Highlight())"), "ok:[object Highlight]");
    assert_eq!(probe(&mut e, "new Highlight().priority"), "ok:0");
    assert_eq!(probe(&mut e, "new Highlight().type"), "ok:highlight");
    assert_eq!(probe(&mut e, "new Highlight().size"), "ok:0");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.priority = '1'; return o.priority; })()"), "ok:1");
    // Chrome 150: WebIDL `long` truncates and maps NaN to 0.
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.priority = 2.7; return o.priority; })()"), "ok:2");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.priority = NaN; return o.priority; })()"), "ok:0");
    // Chrome 150: an unrecognised enum value is IGNORED, not stored and not thrown on.
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.type = 'nope'; return o.type; })()"), "ok:highlight");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.type = 'spelling-error'; return o.type; })()"), "ok:spelling-error");
    // The set: identity dedupe, `add` returns the set, `delete`/`has` are booleans.
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.add(new Range()); return o.size; })()"), "ok:1");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); const r = new Range(); o.add(r); o.add(r); return o.size; })()"), "ok:1");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); const r = new Range(); return o.add(r) === o; })()"), "ok:true");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); const r = new Range(); o.add(r); return o.has(r); })()"), "ok:true");
    assert_eq!(probe(&mut e, "new Highlight().has(new Range())"), "ok:false");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); const r = new Range(); o.add(r); return o.delete(r); })()"), "ok:true");
    assert_eq!(probe(&mut e, "new Highlight().delete(new Range())"), "ok:false");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.add(new Range()); o.clear(); return o.size; })()"), "ok:0");
    assert_eq!(probe(&mut e, "new Highlight().clear()"), "ok:undefined");
    assert_eq!(probe(&mut e, "(() => { const o = new Highlight(); o.add(new Range()); let n = 0; o.forEach(function () { n++; }); return n; })()"), "ok:1");
    // Chrome 150: `new Highlight(r)` is variadic and adds its arguments.
    assert_eq!(probe(&mut e, "(() => { const r = new Range(); return new Highlight(r).size; })()"), "ok:1");
}

#[tokio::test]
async fn the_registry_values_are_the_recorded_ones() {
    let mut e = page().await;
    assert_eq!(probe(&mut e, "Object.prototype.toString.call(CSS.highlights)"), "ok:[object HighlightRegistry]");
    assert_eq!(probe(&mut e, "CSS.highlights instanceof HighlightRegistry"), "ok:true");
    // `[SameObject]`: the realm has ONE registry.
    assert_eq!(probe(&mut e, "CSS.highlights === CSS.highlights"), "ok:true");
    assert_eq!(probe(&mut e, "CSS.highlights.size"), "ok:0");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; const r = o.set('k', new Highlight()) === o; o.clear(); return r; })()"), "ok:true");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; o.set('k', new Highlight()); const n = o.size; o.clear(); return n; })()"), "ok:1");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; const h = new Highlight(); o.set('k', h); const r = o.get('k') === h; o.clear(); return r; })()"), "ok:true");
    // THE DEFECT THIS LANE FIXED: the JS answered `null` here. The bundle's own oracle records the
    // case as `"undefined": true`, and Chrome 150 confirms it.
    assert_eq!(probe(&mut e, "CSS.highlights.get('nope')"), "ok:undefined");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; o.set('k', new Highlight()); const r = o.has('k'); o.clear(); return r; })()"), "ok:true");
    assert_eq!(probe(&mut e, "CSS.highlights.has('nope')"), "ok:false");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; o.set('k', new Highlight()); const r = o.delete('k'); o.clear(); return r; })()"), "ok:true");
    assert_eq!(probe(&mut e, "CSS.highlights.delete('nope')"), "ok:false");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; o.set('k', new Highlight()); const r = [...o.keys()][0]; o.clear(); return r; })()"), "ok:k");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; const h = new Highlight(); o.set('k', h); const r = [...o.values()][0] === h; o.clear(); return r; })()"), "ok:true");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; const h = new Highlight(); o.set('k', h); const e2 = [...o.entries()][0]; o.clear(); return e2[0] === 'k' && e2[1] === h; })()"), "ok:true");
    assert_eq!(probe(&mut e, "(() => { const o = CSS.highlights; o.set('k', new Highlight()); let n = 0; o.forEach(function () { n++; }); o.clear(); return n; })()"), "ok:1");
    // Chrome 150: `CSS.highlights` is NOT receiver-checked -- the one member of this lane that
    // answers for a foreign `this`.
    assert_eq!(
        probe(&mut e, "Object.prototype.toString.call(Object.getOwnPropertyDescriptor(CSS, 'highlights').get.call({}))"),
        "ok:[object HighlightRegistry]"
    );
}

/// The arity TypeErrors, verbatim. The capture-derived registry rows carry `required: 0` for every
/// maplike/setlike member (webref has no count for a declaration), so these come from the BODIES --
/// and the receiver check runs FIRST, which is Chrome's order.
#[tokio::test]
async fn the_arity_messages_are_chromes() {
    let mut e = page().await;
    for (expr, iface, member, need) in [
        ("new Highlight().add()", "Highlight", "add", 1),
        ("new Highlight().delete()", "Highlight", "delete", 1),
        ("new Highlight().has()", "Highlight", "has", 1),
        ("new Highlight().forEach()", "Highlight", "forEach", 1),
        ("CSS.highlights.set()", "HighlightRegistry", "set", 2),
        ("CSS.highlights.get()", "HighlightRegistry", "get", 1),
        ("CSS.highlights.has()", "HighlightRegistry", "has", 1),
        ("CSS.highlights.delete()", "HighlightRegistry", "delete", 1),
        ("CSS.highlights.forEach()", "HighlightRegistry", "forEach", 1),
        ("CSS.highlights.highlightsFromPoint()", "HighlightRegistry", "highlightsFromPoint", 2),
    ] {
        let plural = if need == 1 { "argument" } else { "arguments" };
        assert_eq!(
            probe(&mut e, expr),
            format!("throw:TypeError:Failed to execute '{member}' on '{iface}': {need} {plural} required, but only 0 present."),
            "{iface}.{member}"
        );
    }
    // ...and the two that take none do not throw.
    assert_eq!(probe(&mut e, "new Highlight().clear()"), "ok:undefined");
    assert_eq!(probe(&mut e, "CSS.highlights.clear()"), "ok:undefined");
}

// -- 5. instances: the table's prototype, and no own property -------------------------------------

/// Failure mode 1 (an instance on the wrong prototype) and the instance-tell axis in one: the state is
/// `v8::Private` slots, so `Object.getOwnPropertyNames` sees nothing -- recorded `[]` from Chrome 150
/// for both a `new Highlight()` and `CSS.highlights`.
#[tokio::test]
async fn instances_wear_the_tables_prototype_and_own_nothing() {
    let mut e = page().await;
    let src = r#"(() => {
        const h = new Highlight();
        h.add(new Range());
        h.priority = 3;
        const r = CSS.highlights;
        return [Object.getPrototypeOf(h) === Highlight.prototype,
                Object.getPrototypeOf(r) === HighlightRegistry.prototype,
                JSON.stringify(Object.getOwnPropertyNames(h)),
                JSON.stringify(Object.getOwnPropertyNames(r)),
                JSON.stringify(Object.getOwnPropertySymbols(h).map(String)),
                h.constructor === Highlight].join("|");
    })()"#;
    assert_eq!(str_of(&mut e, "hl-inst", src.to_string()), "true|true|[]|[]|[]|true");
}

// -- 6. the iterator kind --------------------------------------------------------------------------

/// A WebIDL maplike/setlike iterator is its interface's OWN kind, which the capture records three
/// times per interface. A bare Set iterator would read `[object Set Iterator]`.
#[tokio::test]
async fn the_iterators_are_the_interfaces_own_kind() {
    let mut e = page().await;
    let src = r#"(() => {
        const h = new Highlight();
        const r = CSS.highlights;
        const t = (o) => Object.prototype.toString.call(o);
        const out = [t(h.keys()), t(h.values()), t(h.entries()), t(r.keys()), t(r.values()), t(r.entries())];
        // ...and they are real iterators: %IteratorPrototype% is in the chain, so spread works, and
        // `@@iterator` is the same function object the setlike/maplike declaration gives.
        out.push(String(Highlight.prototype[Symbol.iterator] === Highlight.prototype.values));
        out.push(String(HighlightRegistry.prototype[Symbol.iterator] === HighlightRegistry.prototype.entries));
        const rr = new Range();
        h.add(rr);
        out.push(String([...h][0] === rr));
        out.push(String(h.keys().next().value === rr));
        // Two calls share ONE iterator prototype -- a fresh one per call is an identity a page can see.
        out.push(String(Object.getPrototypeOf(h.keys()) === Object.getPrototypeOf(h.values())));
        return out.join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "hl-iter", src.to_string()),
        "[object Highlight Iterator]|[object Highlight Iterator]|[object Highlight Iterator]|\
         [object HighlightRegistry Iterator]|[object HighlightRegistry Iterator]|[object HighlightRegistry Iterator]|\
         true|true|true|true|true"
    );
}

/// Every member reads `[native code]` and carries its own name and the capture's length -- the
/// `op_nativize` masks this lane deleted were the reason that was ever in doubt.
#[tokio::test]
async fn every_member_is_native_with_the_captures_length() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const P = {iface}.prototype, out = [];
                for (const n of Object.getOwnPropertyNames(P)) {{
                    if (n === "constructor") continue;
                    const d = Object.getOwnPropertyDescriptor(P, n);
                    for (const [k, f] of [["", d.value], ["get ", d.get], ["set ", d.set]]) {{
                        if (typeof f !== "function") continue;
                        if (!/\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f))) out.push(n + ":not-native");
                        if (f.name !== k + n) out.push(n + ":name=" + f.name);
                        const own = Object.getOwnPropertyNames(f).filter(x => x !== "name" && x !== "length");
                        if (own.length) out.push(n + ":own=" + own.join("+"));
                        out.push(k + n + "=" + f.length);
                    }}
                }}
                return out.join(" ");
            }})()"#
        );
        let got = str_of(&mut e, "hl-native", src);
        let want: Vec<String> = fx["interfaces"][iface]["members"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| {
                let n = m["name"].as_str().unwrap();
                match m["kind"].as_str().unwrap() {
                    "accessor" => {
                        let mut v = vec![format!("get {n}={}", m["getLength"].as_u64().unwrap())];
                        if m["set"].as_bool().unwrap_or(false) {
                            v.push(format!("set {n}={}", m["setLength"].as_u64().unwrap()));
                        }
                        v
                    }
                    _ => vec![format!("{n}={}", m["length"].as_u64().unwrap())],
                }
            })
            .collect();
        assert_eq!(got, want.join(" "), "{iface}: a member is not native, is misnamed, or has the wrong length");
    }
}
