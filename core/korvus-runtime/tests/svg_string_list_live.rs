//! LIVE one-table gate for `SVGStringList` and its two content-attribute views.
//!
//! The interface is one `interface!` table (`core/dom/svg_string_list.rs`); the two attributes that
//! hand a list out are REGISTRY rows naming `kits::svg_string_list_attr`, claimed by
//! `tools/sync/gen_shells.py` from the capture itself (every shell whose recorded member list has
//! either name), so the four interfaces that carry them are derived, never written down.
//!
//! What is asserted, and where each expectation comes from:
//!
//!   1. **Byte-identical prototype** and interface object -- the BUNDLE.
//!   2. **No row accepts a foreign receiver**, with the receiver check BEFORE the arity check --
//!      Chrome 150 over loopback, 2026-09-11 (`SVGStringList.prototype.getItem.call(foreign)` with no
//!      arguments is `Illegal invocation`, not "1 argument required").
//!   3. **The list IS the content attribute**, in both directions, with the right separator per
//!      attribute -- Chrome 150. The JS this replaces kept a DETACHED array in a `WeakMap`, so
//!      `setAttribute` and the list could not see each other at all.
//!   4. **The bundle's sixteen recorded cases** (`oracle/SVGStringList.json`), which run on a fresh
//!      `<svg>` and which the detached version also passed -- kept so the cutover cannot lose them.
//!   5. **The four interfaces that expose the pair**, and only those four.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><body></body></html>";

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

/// A fresh `<svg>` in the document, and its `requiredExtensions` list.
const MK: &str = r#"const mk = () => { const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
                   document.body.appendChild(s); return s; };"#;

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

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ {MK} try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "sl-probe", src)
}

// -- 1. the shape ---------------------------------------------------------------------------------

#[tokio::test]
async fn the_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    let members = fx["interfaces"]["SVGStringList"]["members"].as_array().unwrap();
    let mut want: Vec<String> = members
        .iter()
        .map(|m| {
            let n = m["name"].as_str().unwrap();
            match m["kind"].as_str().unwrap() {
                "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap_or(false) as u8),
                _ => format!("{n}:data:e=1:c=1:w=1"),
            }
        })
        .collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member installed
    // at interface-definition time, so a member a PARTIAL interface adds lands AFTER it -- `Navigator`
    // 32 of 66, `Screen` 9 of 11 with `onchange` and `isExtended` behind it. Appending it was right on
    // 83 % of the surface and wrong on exactly the interfaces a fingerprinting script reads.
    let at = fx["interfaces"]["SVGStringList"]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(want.len()).min(want.len());
    want.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    let got = str_of(&mut e, "sl-desc", format!(r#"(() => {{ {DESCRIBE} return desc(SVGStringList.prototype); }})()"#));
    let mut g: Vec<&str> = got.lines().collect();
    let mut w: Vec<&str> = want.iter().map(String::as_str).collect();
    g.sort_unstable();
    w.sort_unstable();
    assert_eq!(g, w, "SVGStringList.prototype != the capture");
    let recorded: Vec<&str> = members.iter().map(|m| m["name"].as_str().unwrap()).collect();
    let live: Vec<&str> = got.lines().map(|l| l.split(':').next().unwrap()).filter(|n| recorded.contains(n)).collect();
    assert_eq!(live, recorded, "SVGStringList.prototype is not in the capture's order");
}

#[tokio::test]
async fn the_interface_object_matches_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    let src = r#"(() => {
        const C = SVGStringList;
        const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
        const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
        let newable = "yes";
        try { new C(); } catch (err) { newable = err.constructor.name + ":" + err.message; }
        const native = /\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(C));
        return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(","),
                newable, native, Object.getPrototypeOf(C.prototype) === Object.prototype].join("|");
    })()"#;
    let want_len = fx["interfaces"]["SVGStringList"]["length"].as_u64().unwrap();
    assert_eq!(
        str_of(&mut e, "sl-ctor", src.to_string()),
        format!("SVGStringList|0|0|1|{want_len}|SVGStringList||TypeError:Failed to construct 'SVGStringList': Illegal constructor|true|true")
    );
}

// -- 2. the foreign receiver, before the arity check ----------------------------------------------

#[tokio::test]
async fn every_row_refuses_a_foreign_receiver_before_checking_arity() {
    let mut e = page().await;
    let src = r#"(() => {
        const bad = [];
        const P = SVGStringList.prototype;
        const foreign = Object.create(P);
        const probe = (label, fn) => {
            try { fn(); bad.push(label + "=no-throw"); }
            catch (err) { if (String(err.message) !== "Illegal invocation") bad.push(label + "=" + err.message); }
        };
        for (const m of Object.getOwnPropertyNames(P)) {
            if (m === "constructor") continue;
            const d = Object.getOwnPropertyDescriptor(P, m);
            if (d.get) probe(m + ".get", () => d.get.call(foreign));
            // ZERO arguments on purpose: Chrome reports Illegal invocation, not the arity TypeError,
            // so a guard applied OUTSIDE the body would fail here.
            if (typeof d.value === "function") probe(m + "()", () => d.value.call(foreign));
        }
        return bad.join(" ");
    })()"#;
    assert_eq!(str_of(&mut e, "sl-foreign", src.to_string()), "");
}

/// The arity messages, verbatim, on a REAL list -- and the receiver check does not swallow them.
#[tokio::test]
async fn the_arity_messages_are_chromes() {
    let mut e = page().await;
    for (member, need) in
        [("appendItem", 1), ("getItem", 1), ("initialize", 1), ("removeItem", 1), ("insertItemBefore", 2), ("replaceItem", 2)]
    {
        let plural = if need == 1 { "argument" } else { "arguments" };
        assert_eq!(
            probe(&mut e, &format!("mk().requiredExtensions.{member}()")),
            format!("throw:TypeError:Failed to execute '{member}' on 'SVGStringList': {need} {plural} required, but only 0 present."),
            "{member}"
        );
    }
    assert_eq!(probe(&mut e, "mk().requiredExtensions.clear()"), "ok:undefined");
}

// -- 3. the list IS the content attribute ---------------------------------------------------------

/// Every line is a Chrome 150 answer recorded over loopback on 2026-09-11. The JS this replaces
/// failed ALL of them: its list was a detached array, so nothing here round-tripped.
#[tokio::test]
async fn the_list_is_the_content_attribute_in_both_directions() {
    let mut e = page().await;
    // attribute -> list, with the right separator per attribute and NOT the other one.
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('requiredExtensions', 'p  q'); return JSON.stringify([...Array(s.requiredExtensions.length)].map((_, i) => s.requiredExtensions.getItem(i))); })()"),
        r#"ok:["p","q"]"#
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('requiredExtensions', 'a,b'); return s.requiredExtensions.length + '|' + s.requiredExtensions.getItem(0); })()"),
        "ok:1|a,b"
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('systemLanguage', 'en, fr'); return s.systemLanguage.length + '|' + s.systemLanguage.getItem(0) + '|' + s.systemLanguage.getItem(1); })()"),
        "ok:2|en|fr"
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('systemLanguage', 'en fr'); return s.systemLanguage.length + '|' + s.systemLanguage.getItem(0); })()"),
        "ok:1|en fr"
    );
    // list -> attribute, same separators.
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.requiredExtensions.appendItem('a'); s.requiredExtensions.appendItem('b'); return s.getAttribute('requiredExtensions'); })()"),
        "ok:a b"
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.systemLanguage.appendItem('en'); s.systemLanguage.appendItem('fr'); return s.getAttribute('systemLanguage'); })()"),
        "ok:en,fr"
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.requiredExtensions.appendItem('a'); s.requiredExtensions.initialize('z'); return s.getAttribute('requiredExtensions'); })()"),
        "ok:z"
    );
    // `clear()` REMOVES the attribute -- not an empty one.
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.requiredExtensions.appendItem('a'); s.requiredExtensions.clear(); return s.getAttribute('requiredExtensions'); })()"),
        "ok:null"
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('requiredExtensions', 'a b'); s.requiredExtensions.removeItem(0); return s.getAttribute('requiredExtensions'); })()"),
        "ok:b"
    );
    // An empty attribute is an empty list, and an absent one too.
    assert_eq!(probe(&mut e, "(() => { const s = mk(); s.setAttribute('requiredExtensions', ''); return s.requiredExtensions.length; })()"), "ok:0");
    assert_eq!(probe(&mut e, "mk().requiredExtensions.length"), "ok:0");
    // `[SameObject]`, and it survives an attribute write.
    assert_eq!(probe(&mut e, "(() => { const s = mk(); return s.requiredExtensions === s.requiredExtensions; })()"), "ok:true");
    assert_eq!(probe(&mut e, "(() => { const s = mk(); return s.requiredExtensions !== s.systemLanguage; })()"), "ok:true");
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); const L = s.requiredExtensions; s.setAttribute('requiredExtensions', 'x y'); return (L === s.requiredExtensions) + '|' + L.length; })()"),
        "ok:true|2"
    );
    // A list is not a set.
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.requiredExtensions.appendItem('a'); s.requiredExtensions.appendItem('a'); return s.requiredExtensions.length; })()"),
        "ok:2"
    );
    // `insertItemBefore` CLAMPS past the end.
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('requiredExtensions', 'a'); const r = s.requiredExtensions.insertItemBefore('z', 99); return r + '|' + s.getAttribute('requiredExtensions'); })()"),
        "ok:z|a z"
    );
    // A DETACHED element's list works too (Chrome: no document required).
    assert_eq!(
        probe(&mut e, "(() => { const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); s.setAttribute('requiredExtensions', 'a b'); return s.requiredExtensions.length; })()"),
        "ok:2"
    );
}

/// The IndexSizeError sentence, verbatim -- including the bound being the list's LENGTH and an
/// `unsigned long` coercion that turns `-1` into 4294967295. The JS said "greater than or EQUAL TO".
#[tokio::test]
async fn an_out_of_range_index_is_chromes_index_size_error() {
    let mut e = page().await;
    assert_eq!(
        probe(&mut e, "mk().requiredExtensions.getItem(1)"),
        "throw:DOMException:Failed to execute 'getItem' on 'SVGStringList': The index provided (1) is greater than the maximum bound (0)."
    );
    assert_eq!(
        probe(&mut e, "mk().requiredExtensions.replaceItem('x', 1)"),
        "throw:DOMException:Failed to execute 'replaceItem' on 'SVGStringList': The index provided (1) is greater than the maximum bound (0)."
    );
    assert_eq!(
        probe(&mut e, "mk().requiredExtensions.removeItem(1)"),
        "throw:DOMException:Failed to execute 'removeItem' on 'SVGStringList': The index provided (1) is greater than the maximum bound (0)."
    );
    assert_eq!(
        probe(&mut e, "(() => { const s = mk(); s.setAttribute('requiredExtensions', 'a b'); return s.requiredExtensions.getItem(-1); })()"),
        "throw:DOMException:Failed to execute 'getItem' on 'SVGStringList': The index provided (4294967295) is greater than the maximum bound (2)."
    );
}

// -- 4. the bundle's recorded cases ---------------------------------------------------------------

/// `oracle/SVGStringList.json`, the sixteen cases the capture rig recorded. They run on a fresh
/// `<svg>` with no attribute, which is why the detached-array version passed them too.
#[tokio::test]
async fn the_recorded_oracle_cases_still_pass() {
    let mut e = page().await;
    let svg = "document.createElementNS('http://www.w3.org/2000/svg', 'svg').requiredExtensions";
    assert_eq!(probe(&mut e, &format!("{svg}.length")), "ok:0");
    assert_eq!(probe(&mut e, &format!("{svg}.numberOfItems")), "ok:0");
    assert_eq!(probe(&mut e, &format!("{svg}.clear()")), "ok:undefined");
    assert_eq!(probe(&mut e, &format!("{svg}.initialize('x')")), "ok:x");
    assert_eq!(probe(&mut e, &format!("{svg}.appendItem('x')")), "ok:x");
    assert_eq!(probe(&mut e, &format!("{svg}.insertItemBefore('x', 1)")), "ok:x");
    for m in ["getItem", "removeItem"] {
        assert!(
            probe(&mut e, &format!("{svg}.{m}(1)")).starts_with("throw:DOMException:Failed to execute"),
            "{m}(1) on an empty list must be an IndexSizeError"
        );
    }
    assert!(probe(&mut e, &format!("{svg}.replaceItem('x', 1)")).starts_with("throw:DOMException:"));
    assert_eq!(probe(&mut e, "new SVGStringList()"), "throw:TypeError:Failed to construct 'SVGStringList': Illegal constructor");
}

// -- 5. the four interfaces that expose the pair --------------------------------------------------

/// Recorded from Chrome 150: `requiredExtensions` and `systemLanguage` are own members of exactly
/// `SVGGraphicsElement`, `SVGPatternElement`, `SVGMaskElement` and `SVGAnimationElement` -- not of
/// `SVGElement`, `SVGSVGElement`, `SVGClipPathElement`, `SVGFilterElement` or
/// `SVGForeignObjectElement`. `gen_shells.py` derives that list from the capture, so this asserts the
/// derivation landed where the browser puts it.
#[tokio::test]
async fn exactly_four_interfaces_expose_the_pair() {
    let mut e = page().await;
    let src = r#"(() => {
        const out = [];
        for (const n of ["SVGGraphicsElement","SVGPatternElement","SVGMaskElement","SVGAnimationElement",
                         "SVGElement","SVGSVGElement","SVGClipPathElement","SVGFilterElement","SVGForeignObjectElement"]) {
            const C = globalThis[n];
            const own = C ? Object.getOwnPropertyNames(C.prototype).filter(m => m === "requiredExtensions" || m === "systemLanguage") : ["ABSENT"];
            out.push(n + "=" + (own.join(",") || "-"));
        }
        return out.join(" ");
    })()"#;
    assert_eq!(
        str_of(&mut e, "sl-ifaces", src.to_string()),
        "SVGGraphicsElement=requiredExtensions,systemLanguage SVGPatternElement=requiredExtensions,systemLanguage \
         SVGMaskElement=requiredExtensions,systemLanguage SVGAnimationElement=requiredExtensions,systemLanguage \
         SVGElement=- SVGSVGElement=- SVGClipPathElement=- SVGFilterElement=- SVGForeignObjectElement=-"
    );
    // The accessor's own shape, and the receiver check on it: Chrome throws `Illegal invocation` for
    // a non-element receiver.
    let src = r#"(() => {
        const d = Object.getOwnPropertyDescriptor(SVGGraphicsElement.prototype, "requiredExtensions");
        let foreign = "no-throw";
        try { d.get.call({}); } catch (err) { foreign = err.constructor.name + ":" + err.message; }
        return [!!d.get, !!d.set, d.enumerable, d.configurable, d.get.name, d.get.length,
                /\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(d.get)), foreign].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "sl-attr-desc", src.to_string()),
        "true|false|true|true|get requiredExtensions|0|true|TypeError:Illegal invocation"
    );
    // The list an element hands out wears the TABLE's prototype and owns nothing (failure mode 1 and
    // the instance-tell axis).
    let src = r#"(() => {
        const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
        s.setAttribute("requiredExtensions", "a");
        const L = s.requiredExtensions;
        return [Object.getPrototypeOf(L) === SVGStringList.prototype,
                Object.prototype.toString.call(L),
                JSON.stringify(Object.getOwnPropertyNames(L)),
                JSON.stringify(Object.getOwnPropertyNames(s)),
                L.constructor === SVGStringList].join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "sl-inst", src.to_string()),
        "true|[object SVGStringList]|[]|[]|true"
    );
}
