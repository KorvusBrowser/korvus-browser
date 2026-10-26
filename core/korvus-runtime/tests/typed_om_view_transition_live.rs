//! LIVE one-table gate for the Typed OM and View Transition pairs:
//! `StylePropertyMapReadOnly` + `CSSStyleValue` behind `Element.computedStyleMap()`, and
//! `ViewTransition` + `ViewTransitionTypeSet` behind `Element.startViewTransition()`.
//!
//! ~120 lines of `dom/09-postshells.js` behind five `op_nativize` masks become four `interface!`
//! tables and two `Element`-table row bodies. Every expectation below is the BUNDLE's recorded
//! surface, the bundle's oracle, or an answer read from `google-chrome --headless=new --dump-dom`
//! over a loopback server on 2026-09-11 -- named in the comment where it decides something.
//!
//! THE CHAIN REPAIR IS ASSERTED HERE and not only in the installer's golden: four interfaces left the
//! generated registry, and seven registry children inherit from two of them (`StylePropertyMap` from
//! `StylePropertyMapReadOnly`; `CSSUnparsedValue`, `CSSTransformValue`, `CSSKeywordValue`,
//! `CSSNumericValue`, `CSSImageValue` and `CSSPositionValue` from `CSSStyleValue`). The registry's own
//! chain pass ran during the ESM layer and could not find a parent that no longer exists there, so
//! `rechain_orphans` repairs it -- derived from the registry, never a list. If that sweep stopped
//! running, those seven prototypes would silently inherit `Object.prototype`.

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><head><style>#d { color: rgb(255, 0, 0) }</style></head>
<body><div id="d">x</div></body></html>"#;

const IFACES: &[&str] = &["StylePropertyMapReadOnly", "CSSStyleValue", "ViewTransitionTypeSet", "ViewTransition"];

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

fn str_of(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value(name, src).unwrap().as_str().unwrap().to_string()
}

async fn str_of_async(e: &mut Engine, name: &'static str, src: String) -> String {
    e.eval_value_settled(name, src, std::time::Duration::from_secs(10)).await.unwrap().as_str().unwrap().to_string()
}

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "tom-probe", src)
}

// -- 1. the four prototypes and interface objects --------------------------------------------------

#[tokio::test]
async fn every_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let members = fx["interfaces"][iface]["members"].as_array().unwrap_or_else(|| panic!("{iface} absent"));
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
        let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(want.len()).min(want.len());
        want.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
        let got = str_of(&mut e, "tom-desc", format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#));
        let mut g: Vec<&str> = got.lines().collect();
        let mut w: Vec<&str> = want.iter().map(String::as_str).collect();
        g.sort_unstable();
        w.sort_unstable();
        assert_eq!(g, w, "{iface}.prototype != the capture");
        let recorded: Vec<&str> = members.iter().map(|m| m["name"].as_str().unwrap()).collect();
        let live: Vec<&str> = got.lines().map(|l| l.split(':').next().unwrap()).filter(|n| recorded.contains(n)).collect();
        assert_eq!(live, recorded, "{iface}.prototype is not in the capture's order");
    }
}

#[tokio::test]
async fn every_interface_object_matches_the_capture() {
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
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(","),
                        newable, native, Object.getPrototypeOf(C.prototype) === Object.prototype].join("|");
            }})()"#
        );
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        let statics: Vec<&str> =
            fx["interfaces"][iface]["statics"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(
            str_of(&mut e, "tom-ctor", src),
            format!("{iface}|0|0|1|{want_len}|{iface}|{}|TypeError:Failed to construct '{iface}': Illegal constructor|true|true", statics.join(",")),
            "{iface} interface object != the capture"
        );
    }
}

/// The seven registry children whose parent left the registry with these tables.
#[tokio::test]
async fn the_registry_children_are_rechained_onto_the_tables() {
    let mut e = page().await;
    let src = r#"(() => {
        const pairs = [["StylePropertyMap", "StylePropertyMapReadOnly"],
                       ["CSSUnparsedValue", "CSSStyleValue"], ["CSSTransformValue", "CSSStyleValue"],
                       ["CSSKeywordValue", "CSSStyleValue"], ["CSSNumericValue", "CSSStyleValue"],
                       ["CSSImageValue", "CSSStyleValue"], ["CSSPositionValue", "CSSStyleValue"]];
        const bad = [];
        for (const [child, parent] of pairs) {
            const C = globalThis[child], P = globalThis[parent];
            if (!C || !P) { bad.push(child + "=absent"); continue; }
            if (Object.getPrototypeOf(C.prototype) !== P.prototype) bad.push(child + "!->" + parent);
            if (Object.getPrototypeOf(C) !== P) bad.push(child + ".ctor!->" + parent);
        }
        return bad.join(" ");
    })()"#;
    assert_eq!(str_of(&mut e, "tom-chain", src.to_string()), "", "a registry child lost its parent when the table left the registry");
}

// -- 2. no row accepts a foreign receiver ---------------------------------------------------------

/// Chrome 150: every member of the four throws `Illegal invocation` for a foreign receiver -- EXCEPT
/// `ViewTransition`'s three promise attributes, which hand back a REJECTED PROMISE instead (Blink
/// converts an exception in a promise-typed attribute into a rejection). That exception is asserted on
/// its own below, so this sweep skips exactly those three and nothing else.
#[tokio::test]
async fn every_row_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let src = r#"(() => {
        const PROMISE_ATTRS = new Set(["finished", "ready", "updateCallbackDone"]);
        const bad = [];
        const probe = (label, fn) => {
            try { fn(); bad.push(label + "=no-throw"); }
            catch (err) { if (String(err.message) !== "Illegal invocation") bad.push(label + "=" + err.message); }
        };
        for (const name of ["StylePropertyMapReadOnly", "CSSStyleValue", "ViewTransitionTypeSet", "ViewTransition"]) {
            const P = globalThis[name].prototype;
            const foreign = Object.create(P);
            for (const m of Object.getOwnPropertyNames(P)) {
                if (m === "constructor") continue;
                if (name === "ViewTransition" && PROMISE_ATTRS.has(m)) continue;
                const d = Object.getOwnPropertyDescriptor(P, m);
                if (d.get) probe(name + "." + m + ".get", () => d.get.call(foreign));
                if (typeof d.value === "function") probe(name + "." + m + "()", () => d.value.call(foreign));
            }
        }
        return bad.join(" ");
    })()"#;
    // Every row, `waitUntil` included. It was pinned here as the one exception: an honest `stub` row whose
    // arity guard, wrapped round the stub by `put_d2_stub`, counted before any receiver check could run --
    // "1 argument required". Chrome 150 refuses the receiver first (`Illegal invocation`; the receiver
    // sweep's `ViewTransition.waitUntil` row, `fixtures/foreign-receivers/sweep-chrome-152.json`), and since
    // The stub counts its own arguments after its receiver check
    // (`install::stub::stub_get_body`). (`transitionRoot` was the second stub.)
    assert_eq!(str_of(&mut e, "tom-foreign", src.to_string()), "");
}

/// The exception Chrome makes: a promise-typed attribute answers a REJECTED PROMISE, with Blink's own
/// sentence, rather than throwing. The JS threw from all three.
#[tokio::test]
async fn the_promise_attributes_reject_for_a_foreign_receiver() {
    let mut e = page().await;
    let src = r#"(async () => {
        const out = [];
        for (const n of ["finished", "ready", "updateCallbackDone"]) {
            const g = Object.getOwnPropertyDescriptor(ViewTransition.prototype, n).get;
            let p;
            try { p = g.call(Object.create(ViewTransition.prototype)); }
            catch (err) { out.push(n + "=THREW"); continue; }
            if (Object.prototype.toString.call(p) !== "[object Promise]") { out.push(n + "=not-a-promise"); continue; }
            try { await p; out.push(n + "=resolved"); }
            catch (err) { out.push(n + "=" + err.constructor.name + ":" + err.message); }
        }
        return out.join(" ");
    })()"#;
    assert_eq!(
        str_of_async(&mut e, "tom-vt-foreign", src.to_string()).await,
        "finished=TypeError:Failed to read the 'finished' property from 'ViewTransition': Illegal invocation \
         ready=TypeError:Failed to read the 'ready' property from 'ViewTransition': Illegal invocation \
         updateCallbackDone=TypeError:Failed to read the 'updateCallbackDone' property from 'ViewTransition': Illegal invocation"
    );
}

// -- 3. computedStyleMap ---------------------------------------------------------------------------

/// The bundle's ten recorded `StylePropertyMapReadOnly` cases, every one on a DETACHED element -- an
/// empty map that answers falsy for everything.
#[tokio::test]
async fn the_recorded_style_property_map_cases_pass() {
    let mut e = page().await;
    let m = "document.createElement('div').computedStyleMap()";
    assert_eq!(probe(&mut e, &format!("{m}.size")), "ok:0");
    assert_eq!(probe(&mut e, &format!("{m}.get('x')")), "ok:undefined");
    assert_eq!(probe(&mut e, &format!("Object.prototype.toString.call({m}.get('x'))")), "ok:[object Undefined]");
    assert_eq!(probe(&mut e, &format!("{m}.has('x')")), "ok:false");
    assert_eq!(probe(&mut e, &format!("JSON.stringify({m}.getAll('x'))")), "ok:[]");
    assert_eq!(probe(&mut e, &format!("Object.prototype.toString.call({m}.getAll('x'))")), "ok:[object Array]");
    assert_eq!(probe(&mut e, "new StylePropertyMapReadOnly()"), "throw:TypeError:Failed to construct 'StylePropertyMapReadOnly': Illegal constructor");
    // The arity TypeErrors, Chrome's sentences, with the receiver check FIRST.
    for member in ["get", "getAll", "has"] {
        assert_eq!(
            probe(&mut e, &format!("{m}.{member}()")),
            format!("throw:TypeError:Failed to execute '{member}' on 'StylePropertyMapReadOnly': 1 argument required, but only 0 present.")
        );
    }
}

/// A CSS-wide keyword is a `CSSKeywordValue` for EVERY longhand -- Typed-OM-supported or not, a colour
/// property too -- as Blink's `CreateStyleValueWithoutProperty` makes it before the `typedom_types` gate. A colour that
/// is not a keyword stays a `CSSStyleValue` (`F-attribute-style-map`), and a SHORTHAND is never reified
/// (`GetShorthandProperty`: Chrome 150 reads `margin-block: initial` as `CSSStyleValue`).
///
/// Every row is Chrome 150.0.7871.46's own, byte for byte: stock Chrome, three runs alike, over the same loop
/// (ac, `r1617/tomprobe/tom.html` -> `chrome.txt`, 50 rows; these are 23 of them). The keyword is lower-cased and
/// trimmed, a shorthand's text too.
#[tokio::test]
async fn a_css_wide_keyword_reifies_for_every_longhand_but_no_shorthand() {
    const CHROME: [&str; 23] = [
        r#"color="initial" -> CSSKeywordValue:initial"#,
        r#"color="inherit" -> CSSKeywordValue:inherit"#,
        r#"color="unset" -> CSSKeywordValue:unset"#,
        r#"color="revert" -> CSSKeywordValue:revert"#,
        r#"color="revert-layer" -> CSSKeywordValue:revert-layer"#,
        r#"color="INITIAL" -> CSSKeywordValue:initial"#,
        r#"color=" unset " -> CSSKeywordValue:unset"#,
        r#"background-repeat="initial" -> CSSKeywordValue:initial"#,
        r#"background-repeat="inherit" -> CSSKeywordValue:inherit"#,
        r#"background-repeat="unset" -> CSSKeywordValue:unset"#,
        r#"background-repeat="revert" -> CSSKeywordValue:revert"#,
        r#"background-repeat="revert-layer" -> CSSKeywordValue:revert-layer"#,
        r#"background-repeat="INITIAL" -> CSSKeywordValue:initial"#,
        r#"background-repeat=" unset " -> CSSKeywordValue:unset"#,
        r#"border-block-start-width="unset" -> CSSKeywordValue:unset"#,
        r#"accent-color="INITIAL" -> CSSKeywordValue:initial"#,
        r#"mask-image="revert-layer" -> CSSKeywordValue:revert-layer"#,
        r#"width="initial" -> CSSKeywordValue:initial"#,
        r#"margin-block="initial" -> CSSStyleValue:initial"#,
        r#"margin-block="INITIAL" -> CSSStyleValue:initial"#,
        r#"margin-block=" unset " -> CSSStyleValue:unset"#,
        r#"margin-block="revert-layer" -> CSSStyleValue:revert-layer"#,
        r#"color=red -> CSSStyleValue"#,
    ];
    let mut e = page().await;
    let got = probe(
        &mut e,
        r#"(() => { const d = document.createElement('div'); const out = []; const kws = ['initial', 'inherit', 'unset', 'revert', 'revert-layer', 'INITIAL', ' unset '];
          const rows = [...kws.map((k) => ['color', k]), ...kws.map((k) => ['background-repeat', k]),
                        ['border-block-start-width', 'unset'], ['accent-color', 'INITIAL'], ['mask-image', 'revert-layer'], ['width', 'initial'],
                        ['margin-block', 'initial'], ['margin-block', 'INITIAL'], ['margin-block', ' unset '], ['margin-block', 'revert-layer']];
          for (const [p, k] of rows) {
            d.removeAttribute('style'); d.style.setProperty(p, k);
            let a; try { const v = d.attributeStyleMap.get(p); a = v === null ? 'null' : v === undefined ? 'undef' : v.constructor.name + ':' + String(v); } catch (e) { a = 'throw ' + e.name; }
            out.push(p + '=' + JSON.stringify(k) + ' -> ' + a);
          }
          d.removeAttribute('style'); d.style.setProperty('color', 'red'); out.push('color=red -> ' + d.attributeStyleMap.get('color').constructor.name);
          return out.join('\n'); })()"#,
    );
    assert_eq!(got, format!("ok:{}", CHROME.join("\n")));
}

/// A longhand's name is ASCII case-insensitive in every map: `get('wIdTh')` and `getAll('wIdTh')` reify
/// `width` as `width` does. WPT `the-stylepropertymap/{inline/getAll,computed/get,declared/get}`, which Chrome 150
/// passes: `CSSUnitValue` 10px each. Since the `typedom_types` gate, the name met the gate's lower-case table as
/// written and the value fell through to `CSSStyleValue`.
#[tokio::test]
async fn a_property_name_reifies_whatever_its_case() {
    let mut e = page().await;
    let got = probe(
        &mut e,
        r#"(() => {
          const d = document.createElement('div'); d.id = 'r23'; d.style.cssText = 'height: 20px; width: 10px';
          document.body.appendChild(d);
          const st = document.createElement('style'); st.textContent = '#r23x { height: 20px; width: 10px; }';
          document.head.appendChild(st);
          const s = (v) => v ? v.constructor.name + ':' + String(v) : String(v);
          const r = [s(d.attributeStyleMap.get('wIdTh')), d.attributeStyleMap.getAll('wIdTh').map(s).join(','),
                     s(d.computedStyleMap().get('wIdTh')), s(st.sheet.cssRules[0].styleMap.get('wIdTh'))];
          d.remove(); st.remove();
          return r.join(' '); })()"#,
    );
    assert_eq!(got, "ok:CSSUnitValue:10px CSSUnitValue:10px CSSUnitValue:10px CSSUnitValue:10px");
}

/// An ATTACHED element's map is the SAME resolved values `getComputedStyle` returns -- the two views
/// cannot disagree, which is the whole reason this reads through that one function.
#[tokio::test]
async fn an_attached_elements_map_agrees_with_get_computed_style() {
    let mut e = page().await;
    let src = r#"(() => {
        const el = document.getElementById("d");
        const m = el.computedStyleMap(), cs = getComputedStyle(el);
        const out = [Object.prototype.toString.call(m), m.size === cs.length, m.size > 0];
        // `get` is a CSSStyleValue whose String() is the computed value, on CSSStyleValue.prototype.
        const v = m.get("color");
        out.push(Object.prototype.toString.call(v));
        out.push(String(v) === cs.getPropertyValue("color"));
        out.push(Object.getPrototypeOf(v) === CSSStyleValue.prototype);
        out.push(v.constructor === CSSStyleValue);
        out.push(JSON.stringify(Object.getOwnPropertyNames(v)));
        out.push(m.has("color"));
        out.push(m.getAll("color").length);
        // keys/values/entries/forEach, and the iterator KIND.
        out.push(Object.prototype.toString.call(m.keys()));
        // `cs.item(i)`, and `cs[i]` agrees with it: the declaration had no V8 INDEXED interceptor
        // when this was written, so `getComputedStyle(el)[0]` was `undefined` where Chrome 150 says
        // "accent-color". A later change added one; the assertion below is where that is pinned, and it
        // reads through `item(i)` here either way.
        out.push([...m.keys()][0] === cs.item(0));
        out.push(Array.isArray([...m.values()][0]));
        const first = [...m.entries()][0];
        out.push(first[0] === cs.item(0) && Array.isArray(first[1]) && String(first[1][0]) === cs.getPropertyValue(cs.item(0)));
        let seen = null;
        m.forEach((val, key, map) => { if (seen === null) seen = [key === cs.item(0), Array.isArray(val), map === m]; });
        out.push(JSON.stringify(seen));
        out.push([...m].length === m.size);
        out.push(StylePropertyMapReadOnly.prototype[Symbol.iterator] === StylePropertyMapReadOnly.prototype.entries);
        return out.join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "tom-spm", src.to_string()),
        "[object StylePropertyMapReadOnly]|true|true|[object CSSStyleValue]|true|true|true|[]|true|1|\
         [object StylePropertyMapReadOnly Iterator]|true|true|true|[true,true,true]|true|true"
    );
    // `computedStyleMap` refuses a foreign receiver (Chrome 150).
    assert_eq!(
        probe(&mut e, "Element.prototype.computedStyleMap.call(Object.create(Element.prototype))"),
        "throw:TypeError:Illegal invocation"
    );
    // CLOSED LATER, and this assertion is what told them -- it was written as
    // `"ok:undefined"` with a comment saying so. `CSSStyleDeclaration` had no V8 INDEXED interceptor,
    // so `getComputedStyle(el)[0]` answered `undefined` while `.item(0)` and `.length` were both
    // right: the named interceptor carried a digit branch for it that could never run, because V8
    // canonicalises `s[0]` and `s["0"]` to an ARRAY INDEX and routes them to the indexed handler.
    // There is one now (`css_style_declaration.rs::cb_style_indexed_get`), and Chrome 150 answers
    // `"accent-color"` for `s[0]` and `"--brand"` for `s[s.length - 1]` -- both measured over CDP.
    assert_eq!(probe(&mut e, "getComputedStyle(document.getElementById('d'))[0]"), "ok:accent-color");
    // Past the end is `undefined`, not `""`: `item` and the indexed getter differ there, and both
    // answers are Chrome's (measured).
    assert_eq!(probe(&mut e, "(s => s[s.length])(getComputedStyle(document.getElementById('d')))"), "ok:undefined");
    assert_eq!(probe(&mut e, "getComputedStyle(document.getElementById('d')).item(0)"), "ok:accent-color");
}

/// The bundle's five `CSSStyleValue` cases, and Chrome's message for the two statics.
#[tokio::test]
async fn the_recorded_css_style_value_cases_pass() {
    let mut e = page().await;
    assert_eq!(probe(&mut e, "new CSSStyleValue()"), "throw:TypeError:Failed to construct 'CSSStyleValue': Illegal constructor");
    for m in ["parse", "parseAll"] {
        assert_eq!(
            probe(&mut e, &format!("CSSStyleValue.{m}('x', 'x')")),
            format!("throw:TypeError:Failed to execute '{m}' on 'CSSStyleValue': The value provided ('x') could not be parsed as a 'x'."),
        );
        assert_eq!(
            probe(&mut e, &format!("CSSStyleValue.{m}()")),
            format!("throw:TypeError:Failed to execute '{m}' on 'CSSStyleValue': 2 arguments required, but only 0 present.")
        );
    }
    assert_eq!(
        probe(&mut e, "CSSStyleValue.prototype.toString.call(Object.create(CSSStyleValue.prototype))"),
        "throw:TypeError:Illegal invocation"
    );
}

// -- 4. startViewTransition -------------------------------------------------------------------------

/// The half a page depends on: the callback RUNS, and the three promises settle the way Chrome 150
/// settles them. Three of these are answers the JS got wrong.
#[tokio::test]
async fn a_view_transition_runs_its_callback_and_settles_like_chrome() {
    let mut e = page().await;
    let src = r#"(async () => {
        const out = [];
        // 1. the callback runs, and `updateCallbackDone` / `finished` resolve with UNDEFINED --
        //    not with the callback's return value, which is what the JS did.
        let ran = 0;
        const vt = document.getElementById("d").startViewTransition(() => { ran++; return 42; });
        out.push(Object.prototype.toString.call(vt));
        out.push(vt instanceof ViewTransition);
        out.push(ran);
        out.push("ucd=" + String(await vt.updateCallbackDone));
        out.push("fin=" + String(await vt.finished));
        // 2. `ready` rejects with Chrome's own AbortError / "Transition was skipped".
        try { await vt.ready; out.push("ready=resolved"); }
        catch (err) { out.push("ready=" + err.constructor.name + ":" + err.name + ":" + err.message); }
        // 3. a callback that THROWS rejects BOTH -- the JS resolved `finished` either way, so a
        //    page's `.catch` on it never fired.
        const vt2 = document.getElementById("d").startViewTransition(() => { throw new Error("boom"); });
        vt2.ready.catch(() => {});
        try { await vt2.updateCallbackDone; out.push("ucd2=resolved"); } catch (err) { out.push("ucd2=" + err.message); }
        try { await vt2.finished; out.push("fin2=resolved"); } catch (err) { out.push("fin2=" + err.message); }
        // 4. an ASYNC callback is awaited, and its rejection reaches both.
        const vt3 = document.getElementById("d").startViewTransition(async () => { await null; return 7; });
        vt3.ready.catch(() => {});
        out.push("ucd3=" + String(await vt3.updateCallbackDone));
        const vt4 = document.getElementById("d").startViewTransition(async () => { await null; throw new Error("late"); });
        vt4.ready.catch(() => {});
        try { await vt4.updateCallbackDone; out.push("ucd4=resolved"); } catch (err) { out.push("ucd4=" + err.message); }
        try { await vt4.finished; out.push("fin4=resolved"); } catch (err) { out.push("fin4=" + err.message); }
        // 5. no callback at all: nothing to run, both resolve.
        const vt5 = document.getElementById("d").startViewTransition();
        vt5.ready.catch(() => {});
        out.push("ucd5=" + String(await vt5.updateCallbackDone));
        // 6. the instance owns nothing, and `skipTransition()` answers undefined.
        out.push(JSON.stringify(Object.getOwnPropertyNames(vt)));
        out.push("skip=" + String(vt.skipTransition()));
        return out.join("|");
    })()"#;
    assert_eq!(
        str_of_async(&mut e, "tom-vt", src.to_string()).await,
        "[object ViewTransition]|true|1|ucd=undefined|fin=undefined|\
         ready=DOMException:AbortError:Transition was skipped|ucd2=boom|fin2=boom|ucd3=undefined|\
         ucd4=late|fin4=late|ucd5=undefined|[]|skip=undefined"
    );
}

/// `types` is a `[SameObject]` real setlike. `add` returns UNDEFINED here and the SET on
/// `Highlight` -- both measured in Chrome 150, and the difference is why neither is guessed.
#[tokio::test]
async fn the_view_transition_type_set_is_a_real_setlike() {
    let mut e = page().await;
    let src = r#"(() => {
        const vt = document.getElementById("d").startViewTransition(() => {});
        vt.ready.catch(() => {});
        const S = vt.types;
        const out = [Object.prototype.toString.call(S), S === vt.types, S.size];
        out.push("add=" + String(S.add("a")));
        out.push(S.size);
        S.add("a");
        out.push("dup=" + S.size);
        out.push("has=" + S.has("a"));
        S.add(7);
        out.push(JSON.stringify([...S]));
        out.push("del=" + S.delete("a") + "/" + S.size);
        out.push("delmiss=" + S.delete("zz"));
        out.push(Object.prototype.toString.call(S.keys()));
        out.push(JSON.stringify([...S.entries()][0]));
        let seen = null;
        S.forEach((v, k, s) => { if (seen === null) seen = [v, k, s === S]; });
        out.push(JSON.stringify(seen));
        out.push(ViewTransitionTypeSet.prototype[Symbol.iterator] === ViewTransitionTypeSet.prototype.values);
        S.clear();
        out.push("clear=" + S.size);
        out.push(JSON.stringify(Object.getOwnPropertyNames(S)));
        return out.join("|");
    })()"#;
    assert_eq!(
        str_of(&mut e, "tom-vts", src.to_string()),
        "[object ViewTransitionTypeSet]|true|0|add=undefined|1|dup=1|has=true|[\"a\",\"7\"]|del=true/1|\
         delmiss=false|[object ViewTransitionTypeSet Iterator]|[\"7\",\"7\"]|[\"7\",\"7\",true]|true|clear=0|[]"
    );
}
