//! LIVE one-table gate for the eleven small element interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against the bundle's
//! own oracle answers, that the eleven `interface!` tables reproduce -- byte- and value-identically
//! -- what the nine deleted `dom/*.js` files installed:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list
//!      for that interface, in the capture's order, with the capture's descriptors, `constructor`
//!      last (the hand-table shape).
//!   2. **The interface objects and the chains.** The capture's `Symbol.toStringTag`, `length` and
//!      `name`; the nine element interfaces throw "Illegal constructor" for `new`; the two with real
//!      constructors construct; `document.createElement(tag)` is an `instanceof` of the table's own
//!      interface object, which is what proves the tag map points at the TABLE's prototype.
//!   3. **`<input>`**: `value`/`checked`/`defaultChecked`/`files`/`select()`/`setSelectionRange()`,
//!      `size`'s missing value of 20, `type`'s enumerated clamp, `stepUp`, the validity members.
//!   4. **`<textarea>`**: `defaultValue` over the child text, `textLength` over the API value, the
//!      selection members, the fixed `type`.
//!   5. **`<a>`**: the ten URL-decomposition components off `href`, plus `text` and the `toString`
//!      stringifier.
//!   6. **`<object>`/`<embed>`**: the reflections, the always-barred validation members,
//!      `getSVGDocument()`.
//!   7. **`<iframe>`/`<frame>`**: `contentWindow`/`contentDocument` still answer (W5.0d's bodies),
//!      `sandbox` is a live DOMTokenList, and `marginHeight`/`marginWidth` round-trip.
//!   8. **`<link>`**: the three DOMTokenLists, the enumerated reflections, and `sheet` -- which is a
//!      `stub` row whose body the kernel's CSSOM bootstrap supplies, so this asserts it still ANSWERS
//!      a stylesheet. That is the install-ORDER regression this lane's placement exists to prevent.
//!   9. **`SpeechSynthesisUtterance`**: the six attributes' defaults and round trips, and all seven
//!      `on<type>` handlers firing through a real `dispatchEvent`.
//!  10. **`SecurityPolicyViolationEvent`**: the twelve slots read back what the constructor's init
//!      dictionary put there, in both realms' shape.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

/// The eleven this lane's tables own, with the tag each mints (none for the last two).
const IFACES: &[&str] = &[
    "HTMLInputElement",
    "HTMLButtonElement",
    "HTMLObjectElement",
    "HTMLEmbedElement",
    "HTMLLinkElement",
    "HTMLIFrameElement",
    "HTMLFrameElement",
    "HTMLTextAreaElement",
    "HTMLAnchorElement",
    "SpeechSynthesisUtterance",
    "SecurityPolicyViolationEvent",
];

/// The nine element interfaces and the tag that mints one.
const TAGS: &[(&str, &str)] = &[
    ("HTMLInputElement", "input"),
    ("HTMLButtonElement", "button"),
    ("HTMLObjectElement", "object"),
    ("HTMLEmbedElement", "embed"),
    ("HTMLLinkElement", "link"),
    ("HTMLIFrameElement", "iframe"),
    ("HTMLFrameElement", "frame"),
    ("HTMLTextAreaElement", "textarea"),
    ("HTMLAnchorElement", "a"),
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

// ── 1. the eleven prototypes, byte-identical to the capture ─────────────────────────────────────

#[tokio::test]
async fn every_l1_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l1-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l1_interface_object_carries_the_capture_tag_length_and_name() {
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
        let got = str_of(&mut e, "l1-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        assert_eq!(got, format!("{iface}|0|0|1|{want_len}|{iface}|"), "{iface} interface object != the capture");
    }
}

// ── 2. the chains, the tag map, and the constructors ────────────────────────────────────────────

#[tokio::test]
async fn every_l1_element_chains_to_html_element_and_owns_its_tag() {
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
        let got = str_of(&mut e, "l1-chain", src);
        let want_chain: Vec<&str> = fx["interfaces"][iface]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            got,
            // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
            format!("{}|true|true|[object {iface}]|TypeError:Failed to construct '{iface}': Illegal constructor", want_chain.join(",")),
            "{iface} (<{tag}>) chain / tag map / constructor"
        );
    }
}

#[tokio::test]
async fn the_two_constructible_l1_interfaces_construct_and_chain() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-newable",
        r#"(() => {
            const out = [];
            const u = new SpeechSynthesisUtterance("hi");
            out.push("u:" + (u instanceof SpeechSynthesisUtterance) + ":" + (u instanceof EventTarget) + ":" +
                     Object.prototype.toString.call(u) + ":" + u.text);
            out.push("uproto:" + (Object.getPrototypeOf(SpeechSynthesisUtterance.prototype) === EventTarget.prototype));
            out.push("uctor:" + (Object.getPrototypeOf(SpeechSynthesisUtterance) === EventTarget));
            const v = new SecurityPolicyViolationEvent("securitypolicyviolation", { blockedURI: "b", lineNumber: 7 });
            out.push("v:" + (v instanceof SecurityPolicyViolationEvent) + ":" + (v instanceof Event) + ":" +
                     Object.prototype.toString.call(v) + ":" + v.type);
            out.push("vproto:" + (Object.getPrototypeOf(SecurityPolicyViolationEvent.prototype) === Event.prototype));
            out.push("native:" + /\[native code\]/.test(Function.prototype.toString.call(SecurityPolicyViolationEvent)) +
                     ":" + /\[native code\]/.test(Function.prototype.toString.call(SpeechSynthesisUtterance)));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "u:true:true:[object SpeechSynthesisUtterance]:hi",
            "uproto:true",
            "uctor:true",
            "v:true:true:[object SecurityPolicyViolationEvent]:securitypolicyviolation",
            "vproto:true",
            "native:true:true",
        ]
        .join("\n")
    );
}

// ── 3. <input> ──────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_input_element_round_trips_value_checked_files_and_selection() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-input",
        r#"(() => {
            const out = [];
            const i = document.createElement("input");
            // `size`'s missing value is 20, not 0 -- the one reflected unsigned long in HTML that is not 0.
            out.push("size:" + i.size);
            // `type` is "limited to only known values": an absent OR unrecognised attribute IS "text",
            // and the content attribute keeps what was written.
            i.type = "x";
            out.push("type:" + i.type + ":" + i.getAttribute("type"));
            i.type = "number";
            out.push("number:" + i.type);
            // `defaultValue` reflects the `value` content attribute; `value` is the API value.
            i.defaultValue = "1";
            out.push("v0:" + i.value + ":" + i.defaultValue + ":" + i.getAttribute("value"));
            i.value = "42";
            out.push("v1:" + i.value + ":" + i.defaultValue);
            out.push("asNumber:" + i.valueAsNumber);
            i.stepUp();
            out.push("stepUp:" + i.value);
            // checkedness, not the attribute.
            const c = document.createElement("input");
            c.type = "checkbox";
            out.push("chk0:" + c.checked + ":" + c.defaultChecked);
            c.checked = true;
            out.push("chk1:" + c.checked + ":" + c.defaultChecked + ":" + (c.getAttribute("checked") === null));
            c.indeterminate = true;
            out.push("indet:" + c.indeterminate);
            // files is a real (empty) FileList on type=file.
            const f = document.createElement("input");
            f.type = "file";
            out.push("files:" + Object.prototype.toString.call(f.files) + ":" + f.files.length);
            // the selection mixin.
            const t = document.createElement("input");
            t.value = "hello world";
            t.setSelectionRange(2, 5);
            out.push("sel:" + t.selectionStart + ":" + t.selectionEnd + ":" + t.selectionDirection);
            t.select();
            out.push("select:" + t.selectionStart + ":" + t.selectionEnd);
            t.setRangeText("XY");
            out.push("range:" + t.value);
            // the constraint-validation mixin.
            const r = document.createElement("input");
            r.required = true;
            out.push("valid:" + r.willValidate + ":" + r.checkValidity() + ":" + r.validity.valueMissing);
            r.setCustomValidity("nope");
            out.push("custom:" + r.validationMessage + ":" + r.validity.customError);
            out.push("labels:" + Object.prototype.toString.call(r.labels));
            // the enumerated reflections the generator left as stubs.
            const g = document.createElement("input");
            g.formMethod = "x";
            out.push("formMethod:" + g.formMethod);
            g.popoverTargetAction = "x";
            out.push("popover:" + g.popoverTargetAction);
            // arity: a table row with `required` carries the WebIDL guard.
            let threw = "no-throw";
            try { t.setSelectionRange(); } catch (err) { threw = err.constructor.name; }
            out.push("arity:" + threw);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "size:20",
            "type:text:x",
            "number:number",
            "v0:1:1:1",
            // The API value moves; `defaultValue` -- the `value` content attribute -- does not.
            // Chrome 150.0.7871.46 answers `42:1` (this engine used to write the attribute).
            "v1:42:1",
            "asNumber:42",
            "stepUp:43",
            "chk0:false:false",
            "chk1:true:false:true",
            "indet:true",
            "files:[object FileList]:0",
            "sel:2:5:forward",
            "select:0:11",
            "range:XY",
            "valid:true:false:true",
            "custom:nope:true",
            "labels:[object NodeList]",
            "formMethod:get",
            "popover:toggle",
            "arity:TypeError",
        ]
        .join("\n")
    );
}

// ── 4. <textarea> ───────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_textarea_element_round_trips_default_value_text_length_and_selection() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-textarea",
        r#"(() => {
            const out = [];
            const t = document.createElement("textarea");
            t.appendChild(document.createTextNode("hello"));
            // defaultValue is the CHILD TEXT CONTENT, not an attribute; value follows it until dirtied.
            out.push("dv:" + t.defaultValue + ":" + t.value + ":" + t.textLength);
            t.defaultValue = "abc";
            out.push("set:" + t.defaultValue + ":" + t.childNodes.length + ":" + t.value);
            t.value = "abcdef";
            out.push("val:" + t.value + ":" + t.textLength + ":" + t.defaultValue);
            // `type` is a FIXED IDL value, get-only.
            const d = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "type");
            out.push("type:" + t.type + ":" + (d.set === undefined));
            // cols/rows have non-zero missing values.
            out.push("cols:" + t.cols + ":" + t.rows);
            // the selection mixin, shared with <input>.
            t.setSelectionRange(1, 3);
            out.push("sel:" + t.selectionStart + ":" + t.selectionEnd + ":" + t.selectionDirection);
            t.select();
            out.push("select:" + t.selectionStart + ":" + t.selectionEnd);
            // the constraint-validation mixin, shared with <input>.
            out.push("valid:" + t.willValidate + ":" + t.checkValidity() + ":" + t.validity.valid);
            // autocomplete reads back the AUTOFILL field name.
            t.autocomplete = "x";
            out.push("ac:" + t.autocomplete + ":" + t.getAttribute("autocomplete"));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "dv:hello:hello:5",
            "set:abc:1:abc",
            "val:abcdef:6:abc",
            "type:textarea:true",
            "cols:20:2",
            "sel:1:3:forward",
            "select:0:6",
            "valid:true:true:true",
            "ac::x",
        ]
        .join("\n")
    );
}

// ── 5. <a>: URL decomposition, `text` and the stringifier ───────────────────────────────────────

#[tokio::test]
async fn the_anchor_element_decomposes_its_href_and_stringifies() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-anchor",
        r#"(() => {
            const out = [];
            const a = document.createElement("a");
            a.href = "https://user:pw@example.com:8080/p/q?x=1#frag";
            out.push([a.protocol, a.username, a.password, a.host, a.hostname, a.port, a.pathname,
                      a.search, a.hash, a.origin].join("|"));
            out.push("str:" + String(a) + ":" + (a.toString() === a.href) + ":" +
                     (Object.getOwnPropertyDescriptor(HTMLAnchorElement.prototype, "toString").value.length));
            a.protocol = "http:";
            a.port = "9090";
            a.hash = "z";
            out.push("mut:" + a.href);
            // `text` is the child text content, with WebIDL's ToString on the way in.
            a.appendChild(document.createTextNode("click"));
            out.push("text:" + a.text);
            a.text = null;
            out.push("null:" + a.text + ":" + a.textContent);
            // relList is a live DOMTokenList over `rel`, PutForwards=value.
            a.rel = "noopener noreferrer";
            out.push("rel:" + Object.prototype.toString.call(a.relList) + ":" + a.relList.length +
                     ":" + a.relList.contains("noopener") + ":" + (a.relList === a.relList));
            a.relList.add("nofollow");
            out.push("relAdd:" + a.rel);
            // referrerPolicy: "" for both missing and invalid.
            a.referrerPolicy = "x";
            out.push("rp:" + a.referrerPolicy);
            a.referrerPolicy = "origin";
            out.push("rp2:" + a.referrerPolicy);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "https:|user|pw|example.com:8080|example.com|8080|/p/q|?x=1|#frag|https://example.com:8080",
            "str:https://user:pw@example.com:8080/p/q?x=1#frag:true:0",
            "mut:http://user:pw@example.com:9090/p/q?x=1#z",
            "text:click",
            "null:null:null",
            "rel:[object DOMTokenList]:2:true:true",
            "relAdd:noopener noreferrer nofollow",
            "rp:",
            "rp2:origin",
        ]
        .join("\n")
    );
}

// ── 6. <object> / <embed> ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_plugin_elements_reflect_and_are_always_barred_from_validation() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-object",
        r#"(() => {
            const out = [];
            const o = document.createElement("object");
            o.border = "x";
            o.hspace = 3;
            o.declare = true;
            out.push("refl:" + o.border + ":" + o.hspace + ":" + o.declare + ":" + o.vspace);
            // always barred from constraint validation, with no attribute that can change it.
            o.setCustomValidity("nope");
            out.push("barred:" + o.willValidate + ":" + o.checkValidity() + ":" + o.reportValidity() +
                     ":" + o.validationMessage);
            out.push("frame:" + o.contentDocument + ":" + o.contentWindow + ":" + o.getSVGDocument());
            const b = document.createElement("embed");
            b.src = "";
            b.type = "image/svg+xml";
            b.width = "10";
            out.push("embed:" + b.type + ":" + b.width + ":" + b.getSVGDocument() + ":" + b.name);
            out.push("embedLen:" + HTMLEmbedElement.prototype.getSVGDocument.length);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "refl:x:3:true:0",
            "barred:false:true:true:",
            "frame:null:null:null",
            "embed:image/svg+xml:10:null:",
            "embedLen:0",
        ]
        .join("\n")
    );
}

// ── 7. <iframe> / <frame> ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_frame_elements_keep_their_browsing_context_members() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-iframe",
        r#"(() => {
            const out = [];
            const f = document.createElement("iframe");
            document.body.appendChild(f);
            // W5.0d's bodies: an attached <iframe> has a nested browsing context.
            out.push("ctx:" + (f.contentWindow !== null) + ":" + (f.contentDocument !== null));
            f.sandbox = "allow-scripts allow-forms";
            out.push("sandbox:" + Object.prototype.toString.call(f.sandbox) + ":" + f.sandbox.length +
                     ":" + f.sandbox.contains("allow-forms"));
            f.marginHeight = "x";
            f.marginWidth = "y";
            f.csp = "z";
            out.push("legacy:" + f.marginHeight + ":" + f.marginWidth + ":" + f.csp);
            f.loading = "x";
            out.push("loading:" + f.loading);
            f.credentialless = true;
            out.push("cred:" + f.credentialless);
            out.push("svg:" + f.getSVGDocument());
            // <frame>: the same shared getters, and the two margin cases the shell used to fail.
            //
            // A DETACHED frame element has no content navigable, so both answer null -- as stock
            // Chrome does, on <frame> and on a detached <iframe> alike (2026-10-08, the
            // Chrome oracle ratchet; before, `frame_ctx_for` handed a detached element a live Window).
            const g = document.createElement("frame");
            g.marginHeight = "x";
            g.marginWidth = "y";
            out.push("frame:" + Object.prototype.toString.call(g.contentDocument) + ":" +
                     Object.prototype.toString.call(g.contentWindow) + ":" + g.marginHeight + ":" + g.marginWidth);
            g.noResize = true;
            out.push("frame2:" + g.noResize + ":" + g.src + ":" + g.longDesc);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "ctx:true:true",
            "sandbox:[object DOMTokenList]:2:true",
            "legacy:x:y:z",
            "loading:auto",
            "cred:true",
            "svg:null",
            "frame:[object Null]:[object Null]:x:y",
            "frame2:true::",
        ]
        .join("\n")
    );
}

// ── 8. <link>, and the `sheet` row the kernel's bootstrap bodies ────────────────────────────────

#[tokio::test]
async fn the_link_element_keeps_its_token_lists_and_its_stylesheet() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-link",
        r#"(() => {
            const out = [];
            const l = document.createElement("link");
            l.rel = "stylesheet preload";
            out.push("relList:" + Object.prototype.toString.call(l.relList) + ":" + l.relList.length);
            l.sizes = "16x16 32x32";
            out.push("sizes:" + l.sizes.length + ":" + l.sizes.contains("32x32"));
            l.blocking = "render";
            out.push("blocking:" + l.blocking.length);
            out.push("crossOrigin:" + l.crossOrigin);
            l.crossOrigin = "use-credentials";
            out.push("crossOrigin2:" + l.crossOrigin);
            l.as = "x";
            out.push("as:" + l.as);
            l.as = "style";
            out.push("as2:" + l.as);
            l.fetchPriority = "x";
            out.push("fp:" + l.fetchPriority);
            // `sheet` is a `stub` ROW whose body the kernel's CSSOM bootstrap supplies. If this lane
            // ever installs AFTER that bootstrap, the stub wins and this line reads "undefined".
            //
            // A DISCONNECTED element has no CSS style sheet, so `l.sheet` above is null and cannot
            // tell the live getter from the stub. Chrome 150 headless over loopback, measured:
            // `document.createElement("link").sheet`, `createElement("style").sheet` and
            // `createElementNS(svgNS,"style").sheet` are all null, an APPENDED `<style>` answers a
            // CSSStyleSheet, and removing it makes it null again -- which is what this asserts now.
            // (It read `[object CSSStyleSheet]` for the detached link until the getter stopped
            // minting a sheet for a disconnected element; the bundle's own recorded case for this
            // member has always said null.)
            const d = Object.getOwnPropertyDescriptor(HTMLLinkElement.prototype, "sheet");
            const cs = document.createElement("style");
            cs.textContent = "p{color:red}";
            document.head.appendChild(cs);
            const attached = Object.prototype.toString.call(cs.sheet);
            cs.remove();
            out.push("sheet:" + (typeof d.get === "function") + ":" + (d.set === undefined) + ":" +
                     Object.prototype.toString.call(l.sheet) + ":" + attached + ":" +
                     Object.prototype.toString.call(cs.sheet));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "relList:[object DOMTokenList]:2",
            "sizes:2:true",
            "blocking:1",
            "crossOrigin:null",
            "crossOrigin2:use-credentials",
            "as:",
            "as2:style",
            "fp:auto",
            "sheet:true:true:[object Null]:[object CSSStyleSheet]:[object Null]",
        ]
        .join("\n")
    );
}

// ── 9. SpeechSynthesisUtterance ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_utterance_stores_its_state_and_its_seven_handlers_fire() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-utterance",
        r#"(() => {
            const out = [];
            const u = new SpeechSynthesisUtterance();
            out.push("defaults:" + [u.text, u.lang, String(u.voice), u.volume, u.rate, u.pitch].join("|"));
            u.text = "hi"; u.lang = "en"; u.volume = 0.5; u.rate = 2; u.pitch = 0.25;
            out.push("set:" + [u.text, u.lang, u.volume, u.rate, u.pitch].join("|"));
            const fired = [];
            for (const t of ["start", "end", "error", "pause", "resume", "mark", "boundary"]) {
                u["on" + t] = () => fired.push(t);
                u.dispatchEvent(new Event(t));
            }
            out.push("fired:" + fired.join(","));
            out.push("readback:" + (typeof u.onstart) + ":" + (u.onerror === null));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "defaults:||null|1|1|1",
            "set:hi|en|0.5|2|0.25",
            "fired:start,end,error,pause,resume,mark,boundary",
            "readback:function:false",
        ]
        .join("\n")
    );
}

// ── 10. SecurityPolicyViolationEvent ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_violation_event_reads_back_its_twelve_slots() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l1-spve",
        r#"(() => {
            const M = ["documentURI","referrer","blockedURI","violatedDirective","effectiveDirective",
                       "originalPolicy","disposition","sourceFile","statusCode","lineNumber",
                       "columnNumber","sample"];
            const out = [];
            const d = new SecurityPolicyViolationEvent("securitypolicyviolation");
            out.push("defaults:" + M.map(m => JSON.stringify(d[m])).join("|"));
            const v = new SecurityPolicyViolationEvent("securitypolicyviolation", {
                documentURI: "du", referrer: "rf", blockedURI: "bu", violatedDirective: "vd",
                effectiveDirective: "ed", originalPolicy: "op", disposition: "report",
                sourceFile: "sf", statusCode: 200, lineNumber: 7, columnNumber: 9, sample: "sm",
            });
            out.push("set:" + M.map(m => String(v[m])).join("|"));
            // The twelve slots are hidden v8 Privates: the ONE own property is `isTrusted`, which
            // WebIDL marks [LegacyUnforgeable]. Headless Chrome on this machine (2026-09-10) answers
            // `Object.getOwnPropertyNames(v)` -> ["isTrusted"] for the same construction.
            out.push("own:" + Object.getOwnPropertyNames(v).join(","));
            let threw = "no-throw";
            try { new SecurityPolicyViolationEvent(); } catch (err) { threw = err.constructor.name; }
            out.push("arity:" + threw);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            r#"defaults:""|""|""|""|""|""|"enforce"|""|0|0|0|"""#,
            "set:du|rf|bu|vd|ed|op|report|sf|200|7|9|sm",
            "own:isTrusted",
            "arity:TypeError",
        ]
        .join("\n")
    );
}
