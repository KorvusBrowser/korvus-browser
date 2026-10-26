//! LIVE one-table gate for the four forms-family interfaces.
//!
//! Boots the real engine and asserts, against the shipped capture bundle and against the bundle's own
//! oracle answers, that the four `interface!` tables reproduce -- byte- and value-identically -- what
//! `dom/08w-form-element.js`, `dom/08y-output-fieldset.js`, `dom/08m-meter-element.js` and six lines of
//! `dom/08b-reflected.js` installed:
//!
//!   1. **Byte-identical prototypes.** `describe_own(Iface.prototype)` is the bundle's member list for
//!      that interface, in the capture's order, with the capture's descriptors, `constructor` LAST --
//!      which is also where Chrome 151 puts it (measured 2026-09-11 over loopback: every one of these
//!      four prototypes ends with `constructor`, and so does `HTMLInputElement`'s).
//!   2. **The interface objects and the chains.** All four chain to `HTMLElement`;
//!      `document.createElement(tag)` is an `instanceof` of the table's own interface object, which is
//!      what proves the tag map points at the TABLE's prototype (failure mode 1: an instance left on the
//!      cppgc class template's prototype); `new Iface()` throws `Illegal constructor`.
//!   3. **Every recorded oracle case**, read from the bundle at runtime -- 86 cases across the four --
//!      so the value half of the cutover is checked against the capture rather than against a value
//!      copied into this file.
//!   4. **The four ENUMERATED reflections** (`method`/`enctype`/`encoding`/`autocomplete`): the
//!      non-empty missing default, the invalid-keyword clamp, and that `enctype` and `encoding` are the
//!      SAME content attribute.
//!   5. **`action`'s URL rule**: absent and empty both read the document's URL; a relative value reads
//!      its absolute resolution. Chrome 151, measured.
//!   6. **`form.length` / `form.elements` / `checkValidity`** over a real form, including that the
//!      static validation does NOT short-circuit -- every failing control's `invalid` event fires.
//!   7. **`form.submit()`'s submission**: the entry list, the successful-control rules, the submission
//!      encoding, the three `enctype` serializers and the action's resolution, read off the navigation
//!      the engine queues (a form submission is a real navigation now).
//!   8. **`output.value` / `defaultValue`** and the spec's default-value override, in both orders.
//!   9. **The receiver checks** (failure mode 2): every body this lane wrote answers Chrome's
//!      `TypeError: Illegal invocation` for a foreign receiver.
//!  10. **The remaining `stub` row is honest** (`reset` answers `undefined`) and the real method rows
//!      are not.

use engine::Engine;

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

const IFACES: &[&str] = &["HTMLFormElement", "HTMLOutputElement", "HTMLFieldSetElement", "HTMLMeterElement"];

/// The four element interfaces and the tag that mints one.
const TAGS: &[(&str, &str)] = &[
    ("HTMLFormElement", "form"),
    ("HTMLOutputElement", "output"),
    ("HTMLFieldSetElement", "fieldset"),
    ("HTMLMeterElement", "meter"),
];

const DESCRIBE: &str = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
    const d = Object.getOwnPropertyDescriptor(o, n);
    return (d.get || d.set)
        ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
        : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
}).join("\n");"#;

/// The document URL every test below runs under. A real http URL rather than `about:blank`, because
/// `action`'s document-URL fallback and `submit()`'s action resolution are both defined against a base
/// that can be joined -- `about:blank` is a cannot-be-a-base URL and would make both vacuous.
const DOC_URL: &str = "https://forms.test/dir/page.html";

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.set_url(DOC_URL);
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

// -- 1. the four prototypes, byte-identical to the capture ---------------------------------------

#[tokio::test]
async fn every_l13_prototype_is_byte_identical_to_the_capture() {
    let mut e = page().await;
    for iface in IFACES {
        let src = format!(r#"(() => {{ {DESCRIBE} return desc({iface}.prototype); }})()"#);
        let got = str_of(&mut e, "l13-desc", src);
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != the capture");
    }
}

#[tokio::test]
async fn every_l13_interface_object_carries_the_capture_tag_length_and_name() {
    let mut e = page().await;
    let fx = bundle();
    for iface in IFACES {
        let src = format!(
            r#"(() => {{
                const C = {iface};
                const t = Object.getOwnPropertyDescriptor(C.prototype, Symbol.toStringTag);
                return [t.value, +t.writable, +t.enumerable, +t.configurable, C.length, C.name,
                        /\[native code\]/.test(Function.prototype.toString.call(C))].join("|");
            }})()"#
        );
        let got = str_of(&mut e, "l13-ctor", src);
        let want_len = fx["interfaces"][iface]["length"].as_u64().unwrap();
        assert_eq!(got, format!("{iface}|0|0|1|{want_len}|{iface}|true"), "{iface} interface object != the capture");
    }
}

// -- 2. the chains, the tag map, the constructors --------------------------------------------------

/// FAILURE MODE 1 of a table cutover: an instance left on the cppgc class template's prototype instead
/// of the table's. `Object.getPrototypeOf(document.createElement(tag)) === Iface.prototype` is the
/// probe that catches it.
#[tokio::test]
async fn every_l13_element_chains_and_owns_its_tag() {
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
        let got = str_of(&mut e, "l13-chain", src);
        let want_chain: Vec<&str> =
            fx["interfaces"][iface]["proto"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            got,
            // `new X()` carries Chrome's `Failed to construct 'X': ` construction context.
            format!("{}|true|true|[object {iface}]|TypeError:Failed to construct '{iface}': Illegal constructor", want_chain.join(",")),
            "{iface} (<{tag}>) chain / tag map / constructor"
        );
    }
}

// -- 3. every recorded oracle case -----------------------------------------------------------------

/// The bundle's own recorded answers for the four, read at RUNTIME from
/// `<bundle>/oracle/<Iface>.json`. Nothing is copied into this file: if the bundle is re-captured the
/// expectations move with it.
#[tokio::test]
async fn every_l13_oracle_case_answers_what_the_capture_recorded() {
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
                }} catch (err) {{ return "T" + (err && err.constructor ? err.constructor.name : String(err)); }} }})()"#
            );
            let got = str_of(&mut e, "l13-oracle", src);
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
    // A guard against the loop silently reading nothing (an empty or moved oracle directory).
    assert!(checked >= 80, "only {checked} oracle cases ran for the four L13 interfaces");
}

// -- 4. the four enumerated reflections ------------------------------------------------------------

/// `method`/`enctype`/`encoding`/`autocomplete` are ENUMERATED reflections with NON-EMPTY defaults: a
/// plain string reflection answers `""` where the browser answers `"get"`,
/// `"application/x-www-form-urlencoded"` and `"on"`, and an unrecognised keyword reads back the default
/// rather than what was assigned. `encoding` is a second IDL name for the `enctype` attribute, not a
/// second piece of state.
#[tokio::test]
async fn the_form_enumerated_reflections_carry_their_defaults() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-enum",
        r#"(() => {
            const f = document.createElement("form");
            const out = [];
            out.push("missing:" + [f.method, f.enctype, f.encoding, f.autocomplete].join(","));
            f.method = "POST"; out.push("method-post:" + f.method + ":" + f.getAttribute("method"));
            f.method = "x";    out.push("method-invalid:" + f.method + ":" + f.getAttribute("method"));
            f.enctype = "text/plain"; out.push("enctype:" + f.enctype + ":" + f.encoding);
            // ONE attribute, two IDL names -- assigning through `encoding` moves `enctype`.
            f.encoding = "multipart/form-data";
            out.push("encoding:" + f.enctype + ":" + f.encoding + ":" + f.getAttribute("enctype") + ":" + f.hasAttribute("encoding"));
            f.autocomplete = "off"; out.push("autocomplete:" + f.autocomplete);
            f.autocomplete = "zzz"; out.push("autocomplete-invalid:" + f.autocomplete);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "missing:get,application/x-www-form-urlencoded,application/x-www-form-urlencoded,on",
            "method-post:post:POST",
            "method-invalid:get:x",
            "enctype:text/plain:text/plain",
            "encoding:multipart/form-data:multipart/form-data:multipart/form-data:false",
            "autocomplete:off",
            "autocomplete-invalid:on",
        ]
        .join("\n")
    );
}

/// `action` is the URL reflection with the document-URL fallback (HTML §4.10.3): missing OR empty reads
/// the document's URL. Chrome 151, measured over loopback 2026-09-11.
#[tokio::test]
async fn form_action_reflects_a_url_and_falls_back_to_the_document() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-action",
        r#"(() => {
            const f = document.createElement("form");
            const out = [];
            out.push("absent:" + f.action);
            f.setAttribute("action", ""); out.push("empty:" + f.action);
            f.setAttribute("action", "/x"); out.push("root:" + f.action);
            f.setAttribute("action", "y"); out.push("relative:" + f.action);
            f.setAttribute("action", "https://other.test/z"); out.push("absolute:" + f.action);
            f.action = "w"; out.push("setter:" + f.getAttribute("action") + ":" + f.action);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "absent:https://forms.test/dir/page.html",
            "empty:https://forms.test/dir/page.html",
            "root:https://forms.test/x",
            "relative:https://forms.test/dir/y",
            "absolute:https://other.test/z",
            "setter:w:https://forms.test/dir/w",
        ]
        .join("\n")
    );
}

// -- 5/6. length, elements, and the non-short-circuiting static validation --------------------------

#[tokio::test]
async fn form_length_elements_and_static_validation() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-validate",
        r#"(() => {
            const out = [];
            const host = document.createElement("div");
            host.innerHTML = '<form id="f"><input name="a" required><input name="b" required>' +
                             '<button name="d">x</button>' +
                             '<fieldset name="e"></fieldset><output name="g"></output></form>';
            document.body.appendChild(host);
            const f = document.getElementById("f");
            out.push("length:" + f.length + ":" + f.elements.length + ":" + (f.length === f.elements.length));
            out.push("elements:" + Object.prototype.toString.call(f.elements));
            // Two failing controls: BOTH `invalid` events must fire -- the static validation
            // deliberately does not short-circuit, which `every()` would.
            let fired = [];
            f.querySelectorAll("input").forEach(i => i.addEventListener("invalid", () => fired.push(i.name)));
            out.push("checkValidity:" + f.checkValidity() + " fired:" + fired.join(","));
            fired = [];
            out.push("reportValidity:" + f.reportValidity() + " fired:" + fired.join(","));
            f.querySelectorAll("input").forEach(i => i.value = "v");
            fired = [];
            out.push("valid:" + f.checkValidity() + " fired:" + fired.join(","));
            const empty = document.createElement("form");
            out.push("empty:" + empty.length + ":" + empty.checkValidity());
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "length:5:5:true",
            "elements:[object HTMLFormControlsCollection]",
            "checkValidity:false fired:a,b",
            "reportValidity:false fired:a,b",
            "valid:true fired:",
            "empty:0:true",
        ]
        .join("\n")
    );
}

/// THE DEFECT L13 RECORDED HERE IS CLOSED, by the lane it said would close it.
///
/// What it recorded: `form.checkValidity()` delegates to each listed control's own `checkValidity` --
/// which is what fires the `invalid` events where the spec puts them, and is a verbatim carry-over
/// from the deleted `KB.__formCheckValidity`. `HTMLSelectElement.checkValidity` was a registry D2 stub
/// answering `undefined`, so `!undefined` was truthy and ONE `<select>` anywhere in a form made the
/// whole form report invalid. Chrome 151, measured over loopback 2026-09-11 and written into this
/// test's expectation by L13 before the fix existed: `select.checkValidity()` is `true`,
/// `select.willValidate` is `true`, and the same form reports `true`.
///
/// `HTMLSelectElement` is an `interface!` table since L19, and its eight constraint-validation rows
/// are the mixin `<input>`, `<button>`, `<object>` and `<textarea>` already shared
/// (`install_form_control_shared`), so the delegation now reaches a real answer at both levels.
#[tokio::test]
async fn a_select_in_a_form_reports_the_form_valid() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-select-stub",
        r#"(() => {
            const host = document.createElement("div");
            host.innerHTML = '<form id="fsel"><select name="s"></select></form>';
            document.body.appendChild(host);
            const f = document.getElementById("fsel");
            return "select.checkValidity=" + f.elements[0].checkValidity() + " form=" + f.checkValidity();
        })()"#
            .to_string(),
    );
    assert_eq!(got, "select.checkValidity=true form=true");
}

// -- 7. the submission data path -------------------------------------------------------------------

/// The navigation `src` asks for: its URL, method and body, taken out of the engine's own queue.
///
/// READ FROM THE QUEUE, NOT FROM A FRAME'S `location`. These two used to submit into a
/// named `<iframe>` and read `frame.contentWindow.location.href` back synchronously, which worked
/// only because a form's "navigation" of a frame was a location write with no request behind it.
/// A form submission is a real navigation now, so the frame's location moves when its response
/// arrives -- and the thing worth asserting was never the frame's location anyway: it is the URL,
/// the method and the BODY the submission produces, which is exactly what the queue holds.
fn submitted(e: &mut Engine, name: &'static str, src: &str) -> String {
    e.eval_value(name, src.to_string()).unwrap();
    match e.take_nav_request() {
        None => "none".to_string(),
        Some(n) => match (&n.body, &n.content_type) {
            (Some(b), Some(ct)) => format!("{} {} {ct} {}", n.method, n.url, String::from_utf8_lossy(b)),
            _ => format!("{} {}", n.method, n.url),
        },
    }
}

const ENTRY_FORM: &str = r#"
    const host = document.createElement("div");
    host.innerHTML =
      '<form id="f" action="/go">' +
      '<input name="a" value="1">' +
      '<input name="" value="noname">' +
      '<input name="dis" value="d" disabled>' +
      '<input type="checkbox" name="cb1" checked>' +
      '<input type="checkbox" name="cb2">' +
      '<input type="checkbox" name="cb3" value="yes" checked>' +
      '<input type="radio" name="r" value="rv" checked>' +
      '<input type="submit" name="sub" value="s">' +
      '<input type="reset" name="res" value="r">' +
      '<button name="btn" value="b">x</button>' +
      '<textarea name="ta">t v</textarea>' +
      '<select name="sel"><option value="o1">a</option></select>' +
      '<fieldset name="fs"></fieldset><output name="op">o</output>' +
      '</form>';
    document.body.appendChild(host);
    const f = document.getElementById("f");"#;

/// `submit()` builds the entry list, chooses the submission encoding, serializes, resolves the action
/// and navigates the chosen browsing context -- which for a form with no `target` is the PAGE.
///
/// The successful-control rules are the assertion: an unnamed control, a disabled one, an unchecked
/// checkbox, a `<fieldset>`, an `<output>`, a `<button>` and the reset/submit inputs contribute
/// nothing, a checked checkbox with no `value` contributes `on`, and the rest contribute in tree order.
#[tokio::test]
async fn form_submit_serializes_the_entry_list_into_the_navigation_it_asks_for() {
    let mut e = page().await;
    let entries = "a=1&cb1=on&cb3=yes&r=rv&ta=t+v&sel=o1";
    let get = submitted(&mut e, "l13-submit-get", &format!(r#"(() => {{ {ENTRY_FORM} f.submit(); }})()"#));
    assert_eq!(get, format!("GET https://forms.test/go?{entries}"));

    // POST carries the entry list as its BODY, under the enctype's content type -- the limit the file
    // comment used to name ("a POST navigates to the bare action") is gone.
    let post = submitted(
        &mut e,
        "l13-submit-post",
        r#"(() => { const f = document.getElementById("f"); f.setAttribute("method", "post"); f.submit(); })()"#,
    );
    assert_eq!(post, format!("POST https://forms.test/go application/x-www-form-urlencoded {entries}"));

    // A missing action submits to the document's own URL.
    let noaction = submitted(
        &mut e,
        "l13-submit-noaction",
        r#"(() => { const f = document.getElementById("f");
            f.removeAttribute("method"); f.removeAttribute("action"); f.submit(); })()"#,
    );
    assert_eq!(noaction, format!("GET https://forms.test/dir/page.html?{entries}"));

    // A target that names no existing browsing context navigates nothing (see the file comment in
    // `core/html/html_form_element.rs`: Chrome's popup blocker answers the same way with no gesture).
    let blank = submitted(
        &mut e,
        "l13-submit-blank",
        r#"(() => { const f = document.getElementById("f"); f.setAttribute("target", "_blank"); f.submit(); })()"#,
    );
    assert_eq!(blank, "none");
}

/// The three `enctype` serializers a POST can choose.
#[tokio::test]
async fn a_post_carries_the_body_its_enctype_serializes() {
    let mut e = page().await;
    let text = submitted(
        &mut e,
        "l13-textplain",
        r#"(() => {
            document.body.innerHTML = '<form id="f" action="/go" method="post" enctype="text/plain">' +
                '<input name="q" value="1"><input name="r" value="a b"></form>';
            document.getElementById("f").submit();
        })()"#,
    );
    assert_eq!(text, "POST https://forms.test/go text/plain q=1\r\nr=a b\r\n");

    let multi = submitted(
        &mut e,
        "l13-multipart",
        r#"(() => {
            document.body.innerHTML = '<form id="f" action="/go" method="post" enctype="multipart/form-data">' +
                '<input name="q" value="1"><input type="file" name="fi"></form>';
            document.getElementById("f").submit();
        })()"#,
    );
    let boundary = multi.split("boundary=").nth(1).and_then(|s| s.split(' ').next()).expect("a boundary").to_string();
    assert!(boundary.starts_with("----WebKitFormBoundary"), "{multi}");
    assert_eq!(
        multi,
        format!(
            "POST https://forms.test/go multipart/form-data; boundary={boundary} \
             --{boundary}\r\nContent-Disposition: form-data; name=\"q\"\r\n\r\n1\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"fi\"; filename=\"\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\r\n--{boundary}--\r\n"
        )
    );
}

/// The submission encoding: the form's `accept-charset`'s first SUPPORTED label, else the document's.
#[tokio::test]
async fn form_submit_honours_accept_charset() {
    let mut e = page().await;
    let build = r#"document.body.innerHTML = '<form id="f" action="/go"><input name="k" value="한"></form>';"#;
    let utf8 = submitted(&mut e, "l13-cs-utf8", &format!(r#"(() => {{ {build} document.getElementById("f").submit(); }})()"#));
    assert_eq!(utf8, "GET https://forms.test/go?k=%ED%95%9C");

    let euckr = submitted(
        &mut e,
        "l13-cs-euckr",
        r#"(() => { const f = document.getElementById("f");
            f.setAttribute("accept-charset", "euc-kr"); f.submit(); })()"#,
    );
    assert_eq!(euckr, "GET https://forms.test/go?k=%C7%D1");

    // An ordered set: the first label wins.
    let first = submitted(
        &mut e,
        "l13-cs-first",
        r#"(() => { const f = document.getElementById("f");
            f.setAttribute("accept-charset", "  shift_jis euc-kr "); f.submit(); })()"#,
    );
    assert_eq!(first, "GET https://forms.test/go?k=%26%2354620%3B");
}

// -- 8. output's value / defaultValue --------------------------------------------------------------

/// Neither is an attribute: both are the element's descendant TEXT content, and they diverge once
/// script assigns to `value`. The "default value override" starts null; `value`'s setter captures the
/// current text into the override BEFORE replacing the text.
#[tokio::test]
async fn output_value_and_default_value_track_the_override() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-output",
        r#"(() => {
            const out = [];
            const o = document.createElement("output");
            o.textContent = "markup";
            out.push("markup:" + o.value + "/" + o.defaultValue);
            o.value = "script";
            out.push("afterValue:" + o.value + "/" + o.defaultValue + "/" + o.textContent);
            o.defaultValue = "dv2";
            out.push("afterDefault:" + o.value + "/" + o.defaultValue + "/" + o.textContent);
            o.value = "again";
            out.push("secondValue:" + o.value + "/" + o.defaultValue);
            // The other order: with no override yet, assigning the default IS assigning the text.
            const p = document.createElement("output");
            p.textContent = "m2";
            p.defaultValue = "d2";
            out.push("defaultFirst:" + p.value + "/" + p.defaultValue + "/" + p.textContent);
            // The override is per element, not a shared side table.
            out.push("perElement:" + (o.defaultValue !== p.defaultValue));
            // A private slot, not an own property: the element's own-property surface is untouched.
            out.push("noOwnKeys:" + JSON.stringify(Object.getOwnPropertyNames(o)) +
                     JSON.stringify(Object.getOwnPropertySymbols(o).map(String)));
            // Non-Text children do not contribute.
            const q = document.createElement("output");
            q.innerHTML = "a<span>b</span>c";
            out.push("textOnly:" + q.value);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "markup:markup/markup",
            "afterValue:script/markup/script",
            "afterDefault:script/dv2/script",
            "secondValue:again/dv2",
            "defaultFirst:d2/d2/d2",
            "perElement:true",
            "noOwnKeys:[][]",
            "textOnly:ac",
        ]
        .join("\n")
    );
}

// -- the fieldset collection and the shared always-barred rule -------------------------------------

#[tokio::test]
async fn fieldset_and_output_are_always_barred_and_fieldset_lists_its_controls() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-barred",
        r#"(() => {
            const out = [];
            const host = document.createElement("div");
            host.innerHTML = '<fieldset id="fs"><input name="a" required><select name="b"></select>' +
                             '<textarea name="c"></textarea></fieldset>';
            document.body.appendChild(host);
            const fs = document.getElementById("fs");
            out.push("elements:" + Object.prototype.toString.call(fs.elements) + ":" + fs.elements.length);
            // No attribute makes either of them validate: `required` on the fieldset is inert markup.
            fs.setAttribute("required", "");
            for (const el of [fs, document.createElement("output")]) {
                el.setCustomValidity("nope");
                out.push(el.tagName + ":" + el.willValidate + ":" + el.checkValidity() + ":" +
                         JSON.stringify(el.validationMessage) + ":" + el.validity.valid + ":" +
                         (el.validity === el.validity));
            }
            out.push("type:" + fs.type + ":" + document.createElement("output").type);
            out.push("disabled:" + fs.disabled);
            fs.disabled = true;
            out.push("disabledSet:" + fs.disabled + ":" + fs.hasAttribute("disabled"));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "elements:[object HTMLCollection]:3",
            // `validity.customError` is set even on a barred element, so `validity.valid` is false --
            // while `willValidate`, `checkValidity()` and `validationMessage` all ignore it, because a
            // non-candidate is trivially valid and shows no message. Chrome 151 answers exactly this
            // (measured over loopback 2026-09-11).
            "FIELDSET:false:true:\"\":false:true",
            "OUTPUT:false:true:\"\":false:true",
            "type:fieldset:output",
            "disabled:false",
            "disabledSet:true:true",
        ]
        .join("\n")
    );
}

// -- the meter clamp -------------------------------------------------------------------------------

#[tokio::test]
async fn meter_clamps_against_its_siblings_and_rejects_non_finite() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-meter",
        r#"(() => {
            const out = [];
            const m = document.createElement("meter");
            const read = (t) => t + ":" + [m.value, m.min, m.max, m.low, m.high, m.optimum].join(",");
            out.push(read("defaults"));
            m.setAttribute("min", "2"); m.setAttribute("max", "10");
            m.setAttribute("value", "5abc");   // the lenient float parse: trailing garbage is fine
            out.push(read("clamped"));
            m.setAttribute("value", "1");      // below min -> pinned to min
            out.push(read("belowMin"));
            m.setAttribute("value", "+3");     // a leading `+` is not a recognised sign -> unparseable
            out.push(read("plusSign"));
            m.setAttribute("max", "1");        // max cannot fall below min
            out.push(read("maxBelowMin"));
            m.removeAttribute("min"); m.removeAttribute("max"); m.removeAttribute("value");
            m.value = 0.25;
            out.push("setter:" + m.value + ":" + m.getAttribute("value"));
            let threw = "no-throw";
            try { m.value = Infinity; } catch (e) { threw = e.constructor.name + ":" + e.message; }
            out.push("infinite:" + threw + ":" + m.value);
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "defaults:0,0,1,0,1,0.5",
            "clamped:5,2,10,2,10,6",
            "belowMin:2,2,10,2,10,6",
            "plusSign:2,2,10,2,10,6",
            "maxBelowMin:2,2,2,2,2,2",
            "setter:0.25:0.25",
            "infinite:TypeError:Failed to set the 'value' property on 'HTMLMeterElement': The provided double value is non-finite.:0.25",
        ]
        .join("\n")
    );
}

// -- 9. the receiver checks ------------------------------------------------------------------------

/// FAILURE MODE 2 of a table cutover: a row that accepts a foreign receiver. Chrome 151 answers
/// `TypeError: Illegal invocation` for every one of these (measured over loopback 2026-09-11); before
/// this lane `HTMLOutputElement.prototype.type` answered `"output"` on a plain object,
/// `HTMLMeterElement.prototype.value` answered `0`, and `HTMLFieldSetElement.prototype.elements`
/// answered `undefined`.
#[tokio::test]
async fn every_l13_body_rejects_a_foreign_receiver() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-foreign",
        r#"(() => {
            const P = (label, f) => { try { return label + ":" + String(f()); }
                                      catch (e) { return label + ":" + e.constructor.name + ":" + e.message; } };
            const g = (I, n) => Object.getOwnPropertyDescriptor(globalThis[I].prototype, n).get;
            const s = (I, n) => Object.getOwnPropertyDescriptor(globalThis[I].prototype, n).set;
            const out = [];
            // A plain object, and an object WEARING the prototype (the harder case).
            for (const recv of [{}, Object.create(HTMLFormElement.prototype)]) {
                out.push(P("form.length", () => g("HTMLFormElement", "length").call(recv)));
                out.push(P("form.elements", () => g("HTMLFormElement", "elements").call(recv)));
                out.push(P("form.action", () => g("HTMLFormElement", "action").call(recv)));
                out.push(P("form.checkValidity", () => HTMLFormElement.prototype.checkValidity.call(recv)));
                out.push(P("form.submit", () => HTMLFormElement.prototype.submit.call(recv)));
            }
            out.push(P("output.type", () => g("HTMLOutputElement", "type").call({})));
            out.push(P("output.value", () => g("HTMLOutputElement", "value").call({})));
            out.push(P("output.value=", () => s("HTMLOutputElement", "value").call({}, "x")));
            out.push(P("output.defaultValue", () => g("HTMLOutputElement", "defaultValue").call({})));
            out.push(P("output.defaultValue=", () => s("HTMLOutputElement", "defaultValue").call({}, "x")));
            out.push(P("fieldset.type", () => g("HTMLFieldSetElement", "type").call({})));
            out.push(P("fieldset.elements", () => g("HTMLFieldSetElement", "elements").call({})));
            for (const n of ["value", "min", "max", "low", "high", "optimum"]) {
                out.push(P("meter." + n, () => g("HTMLMeterElement", n).call({})));
                out.push(P("meter." + n + "=", () => s("HTMLMeterElement", n).call({}, 1)));
            }
            // A <div> wearing the right prototype is still not a <form>.
            const d = document.createElement("div");
            Object.setPrototypeOf(d, HTMLFormElement.prototype);
            out.push(P("div-as-form.length", () => g("HTMLFormElement", "length").call(d)));
            return out.join("\n");
        })()"#
            .to_string(),
    );
    for line in got.lines() {
        assert!(line.ends_with(":TypeError:Illegal invocation"), "foreign receiver accepted: {line}");
    }
    assert_eq!(got.lines().count(), 5 * 2 + 7 + 12 + 1);
}

// -- 10. the one stub row is honest, the real ones are not ----------------------------------------

/// `reset` is the table's last D2 `stub` row: it answers `undefined` (its IDL return type) and nothing
/// else. `requestSubmit` LEFT that set and is a body now, and `submit`, `checkValidity`
/// and `reportValidity` never were stubs. Both halves matter -- a stub that answered `null` would be a
/// shape tell, and a "stub" that had quietly become a body would be an unrecorded behaviour change
/// (failure mode 3). All five still answer `undefined` for a form with no controls and no action, so
/// what this pins is the SHAPE: the arity, the name and `[native code]`.
#[tokio::test]
async fn the_stub_row_answers_undefined_and_every_row_is_native() {
    let mut e = page().await;
    let got = str_of(
        &mut e,
        "l13-stubs",
        r#"(() => {
            const out = [];
            const f = document.createElement("form");
            const nat = (n) => /\[native code\]/.test(Function.prototype.toString.call(HTMLFormElement.prototype[n]));
            for (const n of ["reset", "requestSubmit", "submit", "checkValidity", "reportValidity"]) {
                out.push(n + ":" + HTMLFormElement.prototype[n].length + ":" + nat(n) + ":" +
                         HTMLFormElement.prototype[n].name);
            }
            out.push("reset()=" + f.reset());
            out.push("requestSubmit()=" + f.requestSubmit());
            out.push("submit()=" + f.submit());
            return out.join("\n");
        })()"#
            .to_string(),
    );
    assert_eq!(
        got,
        [
            "reset:0:true:reset",
            "requestSubmit:0:true:requestSubmit",
            "submit:0:true:submit",
            "checkValidity:0:true:checkValidity",
            "reportValidity:0:true:reportValidity",
            "reset()=undefined",
            "requestSubmit()=undefined",
            "submit()=undefined",
        ]
        .join("\n")
    );
}
