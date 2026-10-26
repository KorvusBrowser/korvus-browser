//! LIVE one-table gate for the three small blocks of `dom/09-postshells.js`:
//! `HTMLTemplateElement`, `ClipboardItem`, and the seven ARIA ELEMENT reflections on `Element`.
//!
//! `HTMLTemplateElement` is the reason that file existed at all -- its header says so: the shell pass
//! created the prototype and a `content` stub only at the END of the bootstrap, so a native `content`
//! had to be installed after it. As an `interface!` table the ordering problem is GONE rather than
//! accommodated, and this file proves the interface came out the same shape.
//!
//! Every expectation is the BUNDLE's recorded surface or an answer read from
//! `google-chrome --headless=new --dump-dom` over a loopback server on 2026-09-11, named in the
//! comment where it matters.

use engine::Engine;

const PAGE: &str = r#"<!doctype html><html><body><div id="d">x</div><span id="c">y</span>
<template id="t"><b>hi</b></template></body></html>"#;

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

fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    str_of(e, "tca-probe", src)
}

fn assert_capture_shape(e: &mut Engine, iface: &'static str) {
    let fx = bundle();
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
    let got = str_of(e, "tca-desc", format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#));
    let mut g: Vec<&str> = got.lines().collect();
    let mut w: Vec<&str> = want.iter().map(String::as_str).collect();
    g.sort_unstable();
    w.sort_unstable();
    assert_eq!(g, w, "{iface}.prototype != the capture");
    let recorded: Vec<&str> = members.iter().map(|m| m["name"].as_str().unwrap()).collect();
    let live: Vec<&str> = got.lines().map(|l| l.split(':').next().unwrap()).filter(|n| recorded.contains(n)).collect();
    assert_eq!(live, recorded, "{iface}.prototype is not in the capture's order");
}

// -- HTMLTemplateElement ---------------------------------------------------------------------------

#[tokio::test]
async fn the_template_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    assert_capture_shape(&mut e, "HTMLTemplateElement");
    // The WebIDL chain the capture records: HTMLTemplateElement -> HTMLElement -> Element -> Node ->
    // EventTarget. Failure mode 1's sibling: a table spliced onto the wrong parent.
    let src = r#"(() => {
        let p = HTMLTemplateElement.prototype, out = [];
        for (let i = 0; i < 4; i++) { p = Object.getPrototypeOf(p); out.push(p === Object.prototype ? "Object" : p.constructor.name); }
        return out.join(",");
    })()"#;
    assert_eq!(str_of(&mut e, "tpl-chain", src.to_string()), "HTMLElement,Element,Node,EventTarget");
}

/// The `template` tag maps onto the TABLE's prototype, through the one `korvus_dom::tags` pass --
/// which is why the install has to run before `install::dom_chain::fill_dom_protos`.
#[tokio::test]
async fn a_template_element_wears_the_tables_prototype() {
    let mut e = page().await;
    let src = r#"(() => {
        const parsed = document.getElementById("t");
        const made = document.createElement("template");
        return [Object.getPrototypeOf(parsed) === HTMLTemplateElement.prototype,
                Object.getPrototypeOf(made) === HTMLTemplateElement.prototype,
                parsed instanceof HTMLTemplateElement,
                Object.prototype.toString.call(parsed)].join("|");
    })()"#;
    assert_eq!(str_of(&mut e, "tpl-tag", src.to_string()), "true|true|true|[object HTMLTemplateElement]");
}

#[tokio::test]
async fn template_content_and_shadow_root_mode_answer_what_chrome_answers() {
    let mut e = page().await;
    // `content` is a real DocumentFragment, the SAME object on every read, carrying the parsed markup.
    assert_eq!(
        probe(&mut e, "(() => { const t = document.getElementById('t'); return Object.prototype.toString.call(t.content) + '|' + (t.content === t.content) + '|' + t.content.childNodes.length; })()"),
        "ok:[object DocumentFragment]|true|1"
    );
    // Chrome 150: a foreign receiver is `Illegal invocation`.
    assert_eq!(
        probe(&mut e, "Object.getOwnPropertyDescriptor(HTMLTemplateElement.prototype, 'content').get.call(Object.create(HTMLTemplateElement.prototype))"),
        "throw:TypeError:Illegal invocation"
    );
    // `shadowRootMode` does NOT round-trip: the setter writes the attribute verbatim and the getter
    // limits it to the keywords. All four answers recorded from Chrome 150.
    assert_eq!(probe(&mut e, "document.createElement('template').shadowRootMode"), "ok:");
    assert_eq!(
        probe(&mut e, "(() => { const t = document.createElement('template'); t.shadowRootMode = 'open'; return t.shadowRootMode + '|' + t.getAttribute('shadowrootmode'); })()"),
        "ok:open|open"
    );
    assert_eq!(
        probe(&mut e, "(() => { const t = document.createElement('template'); t.shadowRootMode = 'OPEN'; return t.shadowRootMode + '|' + t.getAttribute('shadowrootmode'); })()"),
        "ok:open|OPEN"
    );
    assert_eq!(
        probe(&mut e, "(() => { const t = document.createElement('template'); t.shadowRootMode = 'zz'; return JSON.stringify(t.shadowRootMode) + '|' + t.getAttribute('shadowrootmode'); })()"),
        "ok:\"\"|zz"
    );
    // The five rows this lane did NOT change still answer what the registry's kits answered.
    assert_eq!(
        probe(&mut e, "(() => { const t = document.createElement('template'); return [t.shadowRootDelegatesFocus, t.shadowRootClonable, t.shadowRootSerializable].join(','); })()"),
        "ok:false,false,false"
    );
    assert_eq!(
        probe(&mut e, "(() => { const t = document.createElement('template'); t.shadowRootClonable = true; return t.shadowRootClonable + '|' + t.hasAttribute('shadowrootclonable'); })()"),
        "ok:true|true"
    );
    assert_eq!(probe(&mut e, "JSON.stringify(document.createElement('template').shadowRootCustomElementRegistry)"), "ok:\"\"");
}

// -- ClipboardItem -----------------------------------------------------------------------------

#[tokio::test]
async fn the_clipboard_item_shape_is_the_captures() {
    let mut e = page().await;
    assert_capture_shape(&mut e, "ClipboardItem");
    let fx = bundle();
    let src = r#"(() => {
        const C = ClipboardItem;
        const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
        const d = Object.getOwnPropertyDescriptor(C, "supports");
        return [C.length, C.name, statics.join(","), +d.writable, +d.enumerable, +d.configurable,
                d.value.name, d.value.length,
                /\{\s*\[native code\]\s*\}/.test(Function.prototype.toString.call(d.value))].join("|");
    })()"#;
    let want_len = fx["interfaces"]["ClipboardItem"]["length"].as_u64().unwrap();
    assert_eq!(str_of(&mut e, "ci-shape", src.to_string()), format!("{want_len}|ClipboardItem|supports|1|1|1|supports|1|true"));
}

/// Nineteen inputs, every one read from Chrome 150 over loopback on 2026-09-11. FOUR of them are
/// answers the previous implementation got wrong: it lowercased, it trimmed, and it rejected every
/// `web `-prefixed CUSTOM format -- which is the entire point of that prefix.
#[tokio::test]
async fn clipboard_item_supports_answers_what_chrome_answers() {
    let mut e = page().await;
    for (ty, want) in [
        ("text/plain", "true"),
        ("text/html", "true"),
        ("image/png", "true"),
        ("image/svg+xml", "true"),
        ("image/jpeg", "false"),
        ("text/uri-list", "false"),
        ("application/json", "false"),
        ("x", "false"),
        ("", "false"),
        // the four the old body got wrong
        ("TEXT/PLAIN", "false"),
        (" text/plain ", "false"),
        ("web x/y", "true"),
        ("web image/jpeg", "true"),
        // the `web ` clause's own edges
        ("web text/plain", "true"),
        ("web ", "false"),
        ("web", "false"),
        ("web foo", "false"),
        ("web a/b/c", "false"),
        ("WEB TEXT/PLAIN", "false"),
    ] {
        assert_eq!(
            probe(&mut e, &format!("ClipboardItem.supports({})", serde_json::to_string(ty).unwrap())),
            format!("ok:{want}"),
            "supports({ty:?})"
        );
    }
    // WebIDL requires the argument; a static has no receiver, so the arity guard is the whole check.
    assert_eq!(
        probe(&mut e, "ClipboardItem.supports()"),
        "throw:TypeError:Failed to execute 'supports' on 'ClipboardItem': 1 argument required, but only 0 present."
    );
    // ...and Chrome answers for a null `this`, so the static must not be receiver-checked.
    assert_eq!(probe(&mut e, "ClipboardItem.supports.call(null, 'text/plain')"), "ok:true");
}

// -- the six ARIA element reflections Chrome 150 has -----------------------------------------------
// (the table carries a seventh, `ariaActionsElements`, a Brave 151 member the Chrome 150 capture does
// not record, so the Chrome persona does not expose it)

const ARIA: &[&str] = &[
    "ariaControlsElements",
    "ariaDescribedByElements",
    "ariaDetailsElements",
    "ariaErrorMessageElements",
    "ariaFlowToElements",
    "ariaLabelledByElements",
];

/// The accessor pair's own shape, per row, and the receiver check -- all from Chrome 150.
#[tokio::test]
async fn every_aria_element_row_has_chromes_descriptor_and_refuses_a_foreign_receiver() {
    let mut e = page().await;
    for name in ARIA {
        let src = format!(
            r#"(() => {{
                const d = Object.getOwnPropertyDescriptor(Element.prototype, "{name}");
                let foreign = "no-throw";
                try {{ d.get.call(Object.create(Element.prototype)); }} catch (err) {{ foreign = err.constructor.name + ":" + err.message; }}
                let foreignSet = "no-throw";
                try {{ d.set.call(Object.create(Element.prototype), []); }} catch (err) {{ foreignSet = err.constructor.name + ":" + err.message; }}
                const nat = (f) => /\{{\s*\[native code\]\s*\}}/.test(Function.prototype.toString.call(f));
                return [!!d.get, !!d.set, d.enumerable, d.configurable, d.get.name, d.get.length,
                        d.set.name, d.set.length, nat(d.get), nat(d.set), foreign, foreignSet].join("|");
            }})()"#
        );
        assert_eq!(
            str_of(&mut e, "aria-desc", src),
            format!("true|true|true|true|get {name}|0|set {name}|1|true|true|TypeError:Illegal invocation|TypeError:Illegal invocation"),
            "{name}"
        );
    }
}

/// The values, including the two the JS had wrong: Chrome's array is FROZEN, and two reads are the
/// SAME object (the JS rebuilt a fresh `slice()` per read).
#[tokio::test]
async fn the_aria_element_reflections_answer_what_chrome_answers() {
    let mut e = page().await;
    for name in ARIA {
        // Unset is `null` -- not `undefined`, which is what the shell answered.
        assert_eq!(probe(&mut e, &format!("document.getElementById('d').{name}")), "ok:null", "{name} unset");
    }
    let src = r#"(() => {
        const el = document.getElementById("d"), c = document.getElementById("c");
        el.ariaLabelledByElements = [c];
        const a = el.ariaLabelledByElements, b = el.ariaLabelledByElements;
        const out = [Array.isArray(a), a.length, a[0] === c, Object.isFrozen(a), a === b];
        // A frozen COPY, not the caller's array: a later push must not change what the element reports.
        const src2 = [c];
        el.ariaDetailsElements = src2;
        src2.push(el);
        out.push(el.ariaDetailsElements.length);
        // Assigning null clears it back to null...
        el.ariaLabelledByElements = null;
        out.push(String(el.ariaLabelledByElements));
        // ...and the element still owns NOTHING (the store is a v8::Private).
        out.push(JSON.stringify(Object.getOwnPropertyNames(el)));
        // The seven lists do not share a slot.
        el.ariaControlsElements = [c];
        el.ariaFlowToElements = [el];
        out.push((el.ariaControlsElements[0] === c) + "/" + (el.ariaFlowToElements[0] === el));
        return out.join("|");
    })()"#;
    assert_eq!(str_of(&mut e, "aria-values", src.to_string()), "true|1|true|true|true|1|null|[]|true/true");
}
