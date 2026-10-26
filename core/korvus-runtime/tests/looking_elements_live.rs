//! LIVE one-table gate for the five elements that LOOK AT OTHER ELEMENTS.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against Chrome 150's
//! own answers (measured over a loopback server with `--headless --dump-dom`; every expectation
//! below that is not in the bundle is quoted from that run), that the `HTMLAreaElement`,
//! `HTMLCanvasElement`, `HTMLLabelElement`, `HTMLMapElement` and `HTMLLegendElement` tables reproduce
//! -- byte- and value-identically -- what `dom/09c-small-elements.js` and the `<area>` half of
//! `dom/08c-collections.js`'s hyperlink loop installed, plus the four things those got WRONG.
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list
//!      for that interface, in the capture's order, with the capture's descriptors, `constructor`
//!      last (the hand-table shape).
//!   2. **The interface objects, the chains and the tag map.** The capture's `Symbol.toStringTag`,
//!      `length` and `name`; all five throw "Illegal constructor" for `new`; `document.createElement`
//!      of each tag is an `instanceof` of the table's own interface object, which is what proves the
//!      tag map points at the TABLE's prototype.
//!   3. **`<label>`, in BOTH directions.** The part a re-implementation gets wrong: an explicit `for`
//!      that HITS, an explicit `for` that MISSES and must NOT fall back to the descendants, an
//!      explicit `for` naming something UNLABELABLE, an empty `for`, and the implicit
//!      first-labelable-descendant rule (with `input type=hidden` skipped). Plus `label.form` being
//!      the CONTROL's form owner rather than the label's own ancestor.
//!   4. **`<legend>`**: the `<fieldset>` PARENT's form owner, and null anywhere else.
//!   5. **`<map>`**: `areas` is a LIVE `HTMLCollection` over the map's DESCENDANT `<area>`s, the same
//!      object every read, with `item`/`namedItem`. The deleted JS answered a dead `Array` with two
//!      own JS functions patched onto it.
//!   6. **`<area>`**: the ten URL components off `href`, the reflections, and the `toString`
//!      stringifier -- which is `HTMLAnchorElement`'s body, not a second copy.
//!   7. **`<canvas>`**: `width`/`height`'s 300/150 defaults, and that the four rows whose BODIES
//!      other lanes supply (`getContext`, `toDataURL`, `toBlob`, `captureStream`) still ANSWER. That
//!      is the install-ORDER regression this lane's placement in `generated_shells.js` exists to
//!      prevent.
//!   8. **Foreign receivers.** Every body this lane WRITES throws Chrome's `TypeError: Illegal
//!      invocation` on a receiver that is not its own element. Measured in Chrome 150; before this
//!      lane `HTMLMapElement.prototype.areas` on `{}` answered a list and both `toString`s answered
//!      `""`.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body>\
<form id=\"f1\"><input id=\"c1\"></form>\
<label id=\"lblFor\" for=\"c1\">x</label>\
<label id=\"lblMiss\" for=\"nosuch\"><input id=\"inner1\"></label>\
<label id=\"lblNonLabelable\" for=\"divx\"><input id=\"inner2\"></label>\
<div id=\"divx\"></div>\
<label id=\"lblImplicit\"><input id=\"inner3\"></label>\
<label id=\"lblEmptyFor\" for=\"\"><input id=\"inner4\"></label>\
<label id=\"lblHidden\"><input type=\"hidden\"><input id=\"inner5\"></label>\
<fieldset id=\"fs\"><legend id=\"lg\">L</legend></fieldset>\
<form id=\"f2\"><fieldset id=\"fs2\"><legend id=\"lg2\">L2</legend></fieldset></form>\
<map name=\"m1\"><area id=\"a1\" href=\"/x\" alt=\"a\"><area id=\"a2\" name=\"named\"></map>\
</body></html>";

/// The five this lane's tables own, and the tag each mints.
const TAGS: &[(&str, &str)] = &[
    ("HTMLAreaElement", "area"),
    ("HTMLCanvasElement", "canvas"),
    ("HTMLLabelElement", "label"),
    ("HTMLMapElement", "map"),
    ("HTMLLegendElement", "legend"),
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

// ── 1. the five prototypes, byte-identical to the capture ───────────────────────────────────────

#[tokio::test]
async fn every_l14_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for (iface, _) in TAGS {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l14-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l14_interface_object_carries_the_capture_tag_length_and_name() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, _) in TAGS {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                const statics = Object.getOwnPropertyNames(C).filter(n => !["length","name","prototype"].includes(n));
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name, statics.join(",")].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l14-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        assert_eq!(got, format!("{iface}|0|0|1|{want_len}|{iface}|"), "{iface} interface object != the capture");
    }
}

// ── 2. the chains, the tag map, and the constructors ────────────────────────────────────────────

#[tokio::test]
async fn every_l14_element_chains_to_html_element_and_owns_its_tag() {
    let mut e = page().await;
    let fx = bundle();
    for (iface, tag) in TAGS {
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
        let got = str_of(&mut e, "l14-chain", src);
        let want_chain: Vec<&str> = fx["interfaces"][iface]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            got,
            // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
            format!("{}|true|true|[object {iface}]|TypeError:Failed to construct '{iface}': Illegal constructor", want_chain.join(",")),
            "{iface} (<{tag}>) chain / tag map / constructor"
        );
    }
}

// ── 3. <label>: the rule that does not compose, pinned in BOTH directions ───────────────────────

#[tokio::test]
async fn the_label_control_rule_does_not_fall_back_from_for_to_the_descendants() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l14-label",
        r#"(() => {
            const out = [];
            const $ = (i) => document.getElementById(i);
            const idOf = (x) => x === null ? "null" : (x.id || "(no id)");
            // Explicit `for` that HITS: the control is the named element, wherever it sits.
            out.push("hit:" + idOf($("lblFor").control));
            // Explicit `for` that MISSES: NO control. It does NOT fall back to the <input> inside.
            out.push("miss:" + idOf($("lblMiss").control));
            // Explicit `for` naming something UNLABELABLE (a <div>): also no control, no fallback.
            out.push("unlabelable:" + idOf($("lblNonLabelable").control));
            // An EMPTY `for` still counts as present: no control.
            out.push("empty:" + idOf($("lblEmptyFor").control));
            // No `for` at all: the first labelable DESCENDANT.
            out.push("implicit:" + idOf($("lblImplicit").control));
            // `input type=hidden` is NOT labelable, so the second input wins.
            out.push("hidden:" + idOf($("lblHidden").control));
            // A detached label whose `for` names an id in the DOCUMENT is not in that tree: no control.
            const d = document.createElement("label");
            d.htmlFor = "c1";
            out.push("detached:" + idOf(d.control));
            out.push("bare:" + idOf(document.createElement("label").control));
            // form is the CONTROL's form owner -- this label is OUTSIDE the form it reports.
            out.push("form:" + idOf($("lblFor").form));
            out.push("formMiss:" + idOf($("lblMiss").form));
            // htmlFor is the plain `for` reflection.
            out.push("htmlFor:" + $("lblFor").htmlFor + ":" + document.createElement("label").htmlFor);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "hit:c1",
            "miss:null",
            "unlabelable:null",
            "empty:null",
            "implicit:inner3",
            "hidden:inner5",
            "detached:null",
            "bare:null",
            "form:f1",
            "formMiss:null",
            "htmlFor:c1:",
        ]
        .join("\n")
    );
}

// ── 4. <legend>: the fieldset PARENT's form owner ───────────────────────────────────────────────

#[tokio::test]
async fn the_legend_form_is_its_fieldset_parents_form_owner() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l14-legend",
        r#"(() => {
            const out = [];
            const idOf = (x) => x === null ? "null" : (x.id || "(no id)");
            // A fieldset outside any form: the legend has no form either.
            out.push("noform:" + idOf(document.getElementById("lg").form));
            // A fieldset inside a form: the legend reports THAT form, not the fieldset.
            out.push("inform:" + idOf(document.getElementById("lg2").form));
            // Only a DIRECT <fieldset> parent counts.
            const l = document.createElement("legend");
            const d = document.createElement("div");
            d.appendChild(l);
            out.push("notfieldset:" + idOf(l.form));
            out.push("detached:" + idOf(document.createElement("legend").form));
            // align is the plain reflection, and works on a detached legend.
            const a = document.createElement("legend");
            out.push("align0:" + a.align);
            a.align = "left";
            out.push("align1:" + a.align + ":" + a.getAttribute("align"));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        ["noform:null", "inform:f2", "notfieldset:null", "detached:null", "align0:", "align1:left:left"].join("\n")
    );
}

// ── 5. <map>: a LIVE HTMLCollection of DESCENDANT areas, the same object every read ─────────────

#[tokio::test]
async fn the_map_areas_is_a_live_same_object_html_collection() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l14-map",
        r#"(() => {
            const out = [];
            const m = document.querySelector("map");
            out.push("kind:" + Object.prototype.toString.call(m.areas));
            out.push("same:" + (m.areas === m.areas));
            out.push("len:" + m.areas.length);
            out.push("item:" + m.areas.item(0).id + ":" + m.areas[1].id + ":" + (m.areas.item(9) === null));
            // namedItem matches the id OR an HTML element's `name` content attribute.
            out.push("named:" + m.areas.namedItem("named").id + ":" + m.areas.namedItem("a1").id +
                     ":" + (m.areas.namedItem("zz") === null));
            // LIVE: the same collection object sees a later append.
            const list = m.areas;
            const extra = document.createElement("area");
            m.appendChild(extra);
            out.push("live:" + list.length + ":" + m.areas.length);
            extra.remove();
            out.push("back:" + list.length);
            // DESCENDANTS, not children.
            const n = document.createElement("map");
            n.innerHTML = "<div><area id=q></div>";
            out.push("nested:" + n.areas.length + ":" + n.areas[0].id);
            out.push("empty:" + document.createElement("map").areas.length);
            // name is the plain reflection.
            out.push("name:" + m.name + ":" + document.createElement("map").name);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "kind:[object HTMLCollection]",
            "same:true",
            "len:2",
            "item:a1:a2:true",
            "named:a2:a1:true",
            "live:3:3",
            "back:2",
            "nested:1:q",
            "empty:0",
            "name:m1:",
        ]
        .join("\n")
    );
}

// ── 6. <area>: the URL decomposition, the reflections, and the SHARED stringifier ───────────────

#[tokio::test]
async fn the_area_element_decomposes_its_href_and_stringifies_to_it() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l14-area",
        r#"(() => {
            const out = [];
            const a = document.createElement("area");
            // The spec's null-URL branch: every component "" except protocol, which is ":".
            out.push("null:" + [a.origin, a.protocol, a.username, a.password, a.host, a.hostname,
                                a.port, a.pathname, a.search, a.hash].join("|"));
            out.push("nullStr:" + a.toString() + ":" + String(a));
            a.href = "https://u:p@example.com:8443/one/two?q=1#frag";
            out.push("parts:" + [a.origin, a.protocol, a.username, a.password, a.host, a.hostname,
                                 a.port, a.pathname, a.search, a.hash].join("|"));
            // The stringifier IS the href getter -- the same body <a> uses.
            out.push("str:" + (a.toString() === a.href) + ":" + (String(a) === a.href));
            // A setter re-serialises through href.
            a.protocol = "http";
            out.push("setproto:" + a.protocol + ":" + (a.href.indexOf("http://") === 0));
            a.hash = "other";
            out.push("sethash:" + a.hash);
            // href is [ReflectURL] -- the SAME row kind <a> uses, so a relative attribute reads back
            // through the SAME parser on both. (Whether it resolves to an absolute URL depends on the
            // document's base, which a test page loaded from memory does not have; what this pins is
            // that <area> and <a> answer identically, which is the one-parser-two-callers property.)
            const r = document.createElement("area");
            const ra = document.createElement("a");
            r.setAttribute("href", "/rel");
            ra.setAttribute("href", "/rel");
            out.push("relurl:" + (r.href === ra.href) + ":" + (r.getAttribute("href") === "/rel") +
                     ":" + (r.toString() === r.href) + ":" + (r.toString() === ra.toString()));
            // The reflections.
            const b = document.createElement("area");
            out.push("refl0:" + [b.alt, b.coords, b.shape, b.download, b.ping, b.rel, b.target].join("|"));
            b.alt = "A"; b.coords = "1,2"; b.shape = "rect"; b.download = "d"; b.ping = "p"; b.rel = "noopener nofollow"; b.target = "_blank";
            out.push("refl1:" + [b.alt, b.coords, b.shape, b.download, b.ping, b.rel, b.target].join("|"));
            out.push("relList:" + Object.prototype.toString.call(b.relList) + ":" + b.relList.length +
                     ":" + b.relList.contains("noopener") + ":" + (b.relList === b.relList));
            // noHref is the boolean reflection of the obsolete `nohref`.
            out.push("noHref0:" + b.noHref);
            b.noHref = true;
            out.push("noHref1:" + b.noHref + ":" + (b.getAttribute("nohref") === ""));
            // referrerPolicy is enumerated: "" for both missing and invalid.
            out.push("rp0:" + JSON.stringify(b.referrerPolicy));
            b.referrerPolicy = "nonsense";
            out.push("rp1:" + JSON.stringify(b.referrerPolicy) + ":" + b.getAttribute("referrerpolicy"));
            b.referrerPolicy = "origin";
            out.push("rp2:" + b.referrerPolicy);
            // interestForElement is an attr-element reference over `interestfor`.
            out.push("interest:" + (document.createElement("area").interestForElement === null));
            out.push("len:" + HTMLAreaElement.prototype.toString.length + ":" + HTMLAreaElement.prototype.toString.name);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            // Ten components, nine separators: every one "" except `protocol`, which is ":".
            "null:|:||||||||",
            "nullStr::",
            "parts:https://example.com:8443|https:|u|p|example.com:8443|example.com|8443|/one/two|?q=1|#frag",
            "str:true:true",
            "setproto:http::true",
            "sethash:#other",
            "relurl:true:true:true:true",
            "refl0:||||||",
            "refl1:A|1,2|rect|d|p|noopener nofollow|_blank",
            "relList:[object DOMTokenList]:2:true:true",
            "noHref0:false",
            "noHref1:true:true",
            "rp0:\"\"",
            "rp1:\"\":nonsense",
            "rp2:origin",
            "interest:true",
            "len:0:toString",
        ]
        .join("\n")
    );
}

// ── 7. <canvas>: the two rows this lane bodies, and the four whose bodies still arrive ──────────

#[tokio::test]
async fn the_canvas_defaults_are_300_by_150_and_the_other_lanes_bodies_still_answer() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l14-canvas",
        r#"(() => {
            const out = [];
            const c = document.createElement("canvas");
            // The spec's missing values, NOT zero -- and no own properties on the element.
            out.push("default:" + c.width + ":" + c.height);
            out.push("own:" + Object.getOwnPropertyNames(c).length);
            c.width = 1; c.height = "2";
            out.push("set:" + c.width + ":" + c.height + ":" + c.getAttribute("width"));
            // The four rows whose BODIES other passes supply must still ANSWER. This is the
            // install-ORDER gate: the table is honest `stub` rows for these, and the oracle
            // (getContext/toDataURL/toBlob) and install_window_media_rtc (captureStream) run after it.
            const d = document.createElement("canvas");
            out.push("ctx:" + Object.prototype.toString.call(d.getContext("2d")) +
                     ":" + (d.getContext("2d") === d.getContext("2d")) +
                     ":" + (d.getContext("x") === null));
            out.push("dataurl:" + d.toDataURL().slice(0, 22));
            let threw = "no-throw";
            try { d.getContext(); } catch (err) { threw = err.constructor.name; }
            out.push("arity:" + threw);
            let blobThrew = "no-throw";
            try { d.toBlob("not a function"); } catch (err) { blobThrew = err.constructor.name; }
            out.push("toBlob:" + blobThrew + ":" + (d.toBlob(function () {}) === undefined));
            out.push("capture:" + Object.prototype.toString.call(d.captureStream()));
            // transferControlToOffscreen is an honest D2 stub in this table, as it was in the registry.
            out.push("offscreen:" + typeof d.transferControlToOffscreen);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "default:300:150",
            "own:0",
            "set:1:2:1",
            "ctx:[object CanvasRenderingContext2D]:true:true",
            "dataurl:data:image/png;base64,",
            "arity:TypeError",
            "toBlob:TypeError:true",
            "capture:[object MediaStream]",
            "offscreen:function",
        ]
        .join("\n")
    );
}

// ── 8. foreign receivers: every body this lane writes is Chrome's Illegal invocation ────────────

#[tokio::test]
async fn every_l14_body_refuses_a_foreign_receiver() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l14-receiver",
        r#"(() => {
            const out = [];
            const call = (label, f) => {
                try { const v = f(); out.push(label + ":no-throw:" + String(v)); }
                catch (err) { out.push(label + ":" + err.constructor.name + ":" + err.message); }
            };
            const get = (C, n) => Object.getOwnPropertyDescriptor(C.prototype, n).get;
            call("areas", () => get(HTMLMapElement, "areas").call({}));
            call("control", () => get(HTMLLabelElement, "control").call({}));
            call("labelForm", () => get(HTMLLabelElement, "form").call({}));
            call("legendForm", () => get(HTMLLegendElement, "form").call({}));
            call("areaToString", () => HTMLAreaElement.prototype.toString.call({}));
            call("anchorToString", () => HTMLAnchorElement.prototype.toString.call({}));
            // The check is aimed at the INTERFACE, not at the shared mixin: an <a> has an href and
            // still must not satisfy <area>'s stringifier (Chrome 150, measured).
            call("areaOnAnchor", () => {
                const a = document.createElement("a");
                a.href = "/zz";
                return HTMLAreaElement.prototype.toString.call(a);
            });
            call("anchorOnArea", () => {
                const r = document.createElement("area");
                r.href = "/zz";
                return HTMLAnchorElement.prototype.toString.call(r);
            });
            // A wrong-tag ELEMENT is refused too, not just a plain object.
            call("areasOnDiv", () => get(HTMLMapElement, "areas").call(document.createElement("div")));
            call("controlOnDiv", () => get(HTMLLabelElement, "control").call(document.createElement("div")));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    for line in got.lines() {
        assert!(
            line.ends_with(":TypeError:Illegal invocation"),
            "a foreign receiver must be Chrome's Illegal invocation, got: {line}\nall:\n{got}"
        );
    }
    assert_eq!(got.lines().count(), 10, "every probe must have produced a line:\n{got}");
}
