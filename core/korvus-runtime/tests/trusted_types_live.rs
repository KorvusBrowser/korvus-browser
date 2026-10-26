//! `window.trustedTypes` on the LIVE surface, against the browsers' own answers.
//!
//! FP5 measured **eight sensor reads of `window.trustedTypes` in one a bot sensor page load** and refused
//! to seat the name, with the argument that became the unit contract's:
//!
//! > A page that finds `window.trustedTypes` and calls `createPolicy(n, o).createHTML(s)` **throws**
//! > on a stub. A page with no `trustedTypes` at all takes its fallback and works.
//!
//! So the load-bearing test here is [`a_policy_round_trips`]: `createPolicy` -> `createHTML` ->
//! `isHTML` is `true`, and the value an `innerHTML` sink is handed comes out the other side.
//!
//! Every expectation was read off Chrome 150.0.7871.46 and Brave 151.1.93.138 (headed over CDP under
//! Xvfb, two origins, 2026-09-19/20), and the type map off all four browsers.

use engine::Engine;
use std::time::Duration;

const URL: &str = "http://127.0.0.1:8123/page.html";
const DOC: &str = "<!doctype html><html><head><title>t</title></head><body>x</body></html>";

async fn page(js: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(DOC, URL, &[]).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval_value("tt", js.to_string()).expect("eval")
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array().expect("an array").iter().map(|x| x.as_str().expect("a string").to_string()).collect()
}

/// THE ROW THIS LANE EXISTS FOR: a policy that round-trips through its own type checks.
#[tokio::test]
async fn a_policy_round_trips() {
    let v = page(
        r#"(() => {
            const p = trustedTypes.createPolicy('fp55', {
                createHTML: (s) => s + '!', createScript: (s) => s, createScriptURL: (s) => s });
            const h = p.createHTML('x');
            const sc = p.createScript('1+1');
            const su = p.createScriptURL('/a.js');
            const sink = document.createElement('div');
            sink.innerHTML = h;
            return {
                name: p.name,
                policyTag: Object.prototype.toString.call(p),
                htmlTag: Object.prototype.toString.call(h),
                str: String(h), json: h.toJSON(), concat: 'p' + h,
                stringify: JSON.stringify({ h }),
                typeofH: typeof h,
                ownProps: Object.getOwnPropertyNames(h),
                isHTML: trustedTypes.isHTML(h),
                isScript: trustedTypes.isScript(h),
                isScriptURL: trustedTypes.isScriptURL(h),
                scriptIsScript: trustedTypes.isScript(sc),
                urlIsURL: trustedTypes.isScriptURL(su),
                scriptIsHTML: trustedTypes.isHTML(sc),
                plainIsHTML: trustedTypes.isHTML('x'),
                objIsHTML: trustedTypes.isHTML({}),
                fakeIsHTML: trustedTypes.isHTML(Object.create(TrustedHTML.prototype)),
                sink: sink.innerHTML,
                fresh: p.createHTML('x') === p.createHTML('x'),
            };
        })()"#,
    )
    .await;
    assert_eq!(v["name"], "fp55");
    assert_eq!(v["policyTag"], "[object TrustedTypePolicy]");
    assert_eq!(v["htmlTag"], "[object TrustedHTML]");
    assert_eq!(v["str"], "x!", "the policy's own callback ran and its return was wrapped");
    assert_eq!(v["json"], "x!");
    assert_eq!(v["concat"], "px!", "`+` reaches toString");
    assert_eq!(v["stringify"], r#"{"h":"x!"}"#, "JSON.stringify reaches toJSON");
    assert_eq!(v["typeofH"], "object");
    assert_eq!(strings(&v["ownProps"]), Vec::<String>::new(), "no own properties on any of the four");
    assert_eq!(v["isHTML"], true, "the whole lane: createPolicy -> createHTML -> isHTML");
    assert_eq!(v["isScript"], false);
    assert_eq!(v["isScriptURL"], false);
    assert_eq!(v["scriptIsScript"], true);
    assert_eq!(v["urlIsURL"], true);
    assert_eq!(v["scriptIsHTML"], false, "a TrustedScript is not a TrustedHTML");
    assert_eq!(v["plainIsHTML"], false);
    assert_eq!(v["objIsHTML"], false);
    // The brand is a private slot, not the prototype: borrowing the prototype does not make one.
    assert_eq!(v["fakeIsHTML"], false, "Object.create(TrustedHTML.prototype) is not a TrustedHTML");
    assert_eq!(v["sink"], "x!", "an innerHTML sink takes the value");
    assert_eq!(v["fresh"], false, "each call mints a new value");
}

/// The factory's own answers: the two empty values, the default policy, and what a policy with a
/// missing member does.
#[tokio::test]
async fn the_factory_answers_what_the_device_answers() {
    let v = page(
        r#"(() => {
            const tt = trustedTypes;
            const sync = (f) => { try { return f(); } catch (e) { return e.constructor.name + ': ' + e.message; } };
            const before = tt.defaultPolicy;
            const d = tt.createPolicy('default', { createHTML: (s) => s });
            const bare = tt.createPolicy('fp55-bare', {});
            const none = tt.createPolicy('fp55-none');
            return {
                emptyHTMLStr: String(tt.emptyHTML), emptyHTMLTag: Object.prototype.toString.call(tt.emptyHTML),
                emptyHTMLStable: tt.emptyHTML === tt.emptyHTML, emptyHTMLIsHTML: tt.isHTML(tt.emptyHTML),
                emptyScriptStr: String(tt.emptyScript), emptyScriptTag: Object.prototype.toString.call(tt.emptyScript),
                emptyScriptIsScript: tt.isScript(tt.emptyScript),
                defaultBefore: before, defaultAfter: tt.defaultPolicy === d, defaultName: tt.defaultPolicy.name,
                bareName: bare.name,
                bareCreate: sync(() => bare.createHTML('x')),
                noneCreate: sync(() => none.createHTML('x')),
                noneName: none.name,
                dupe: sync(() => tt.createPolicy('fp55-bare', {}).name),
                noArgs: sync(() => tt.createPolicy()),
                wrongArity: sync(() => d.createHTML()),
                throwing: sync(() => tt.createPolicy('fp55-t', { createHTML: () => { throw new Error('boom'); } }).createHTML('x')),
                nullReturn: sync(() => String(tt.createPolicy('fp55-n', { createHTML: () => null }).createHTML('x'))),
                undefReturn: sync(() => String(tt.createPolicy('fp55-u', { createHTML: () => undefined }).createHTML('x'))),
                numReturn: sync(() => String(tt.createPolicy('fp55-i', { createHTML: () => 42 }).createHTML('x'))),
                cbArgs: sync(() => { let seen; tt.createPolicy('fp55-a', { createHTML: function (...a) { seen = a.length + ':' + a.join(','); return 'y'; } }).createHTML('x', 1, 2); return seen; }),
                cbThis: sync(() => { let t; tt.createPolicy('fp55-h', { createHTML: function () { t = this === undefined ? 'undefined' : Object.prototype.toString.call(this); return 'y'; } }).createHTML('x'); return t; }),
                ttStable: window.trustedTypes === window.trustedTypes,
                ttOwn: Object.prototype.hasOwnProperty.call(window, 'trustedTypes'),
                ttDesc: (dd => [!!dd.get, !!dd.set, dd.enumerable, dd.configurable])(
                    Object.getOwnPropertyDescriptor(window, 'trustedTypes')),
            };
        })()"#,
    )
    .await;
    assert_eq!(v["emptyHTMLStr"], "");
    assert_eq!(v["emptyHTMLTag"], "[object TrustedHTML]");
    assert_eq!(v["emptyHTMLStable"], true, "one per realm — measured `===` true");
    assert_eq!(v["emptyHTMLIsHTML"], true);
    assert_eq!(v["emptyScriptStr"], "");
    assert_eq!(v["emptyScriptTag"], "[object TrustedScript]");
    assert_eq!(v["emptyScriptIsScript"], true);
    assert_eq!(v["defaultBefore"], serde_json::Value::Null, "null until a policy called `default` exists");
    assert_eq!(v["defaultAfter"], true, "and then THAT policy, by identity");
    assert_eq!(v["defaultName"], "default");
    assert_eq!(v["bareName"], "fp55-bare");
    assert_eq!(
        v["bareCreate"],
        "TypeError: Failed to execute 'createHTML' on 'TrustedTypePolicy': Policy fp55-bare's \
         TrustedTypePolicyOptions did not specify a 'createHTML' member."
    );
    assert_eq!(
        v["noneCreate"],
        "TypeError: Failed to execute 'createHTML' on 'TrustedTypePolicy': Policy fp55-none's \
         TrustedTypePolicyOptions did not specify a 'createHTML' member."
    );
    assert_eq!(v["noneName"], "fp55-none");
    assert_eq!(v["dupe"], "fp55-bare", "a duplicate policy name is not an error with no CSP");
    assert_eq!(
        v["noArgs"],
        "TypeError: Failed to execute 'createPolicy' on 'TrustedTypePolicyFactory': 1 argument required, but only 0 present."
    );
    assert_eq!(
        v["wrongArity"],
        "TypeError: Failed to execute 'createHTML' on 'TrustedTypePolicy': 1 argument required, but only 0 present."
    );
    assert_eq!(v["throwing"], "Error: boom", "a callback's throw goes through unchanged");
    // The reading an IDL would not have predicted: null and undefined both become "".
    assert_eq!(v["nullReturn"], "");
    assert_eq!(v["undefReturn"], "");
    assert_eq!(v["numReturn"], "42");
    assert_eq!(v["cbArgs"], "3:x,1,2", "every argument reaches the callback");
    assert_eq!(v["cbThis"], "[object Window]", "an undefined receiver, so a sloppy callback sees the global");
    assert_eq!(v["ttStable"], true);
    assert_eq!(v["ttOwn"], true);
    assert_eq!(v["ttDesc"], serde_json::json!([true, false, true, true]), "get only, e=1 c=1");
}

/// The five interface objects and their prototypes, exactly as the captures record them.
#[tokio::test]
async fn the_five_interfaces_are_the_browsers() {
    let v = page(
        r#"({
            factory: Object.getOwnPropertyNames(TrustedTypePolicyFactory.prototype),
            policy: Object.getOwnPropertyNames(TrustedTypePolicy.prototype),
            html: Object.getOwnPropertyNames(TrustedHTML.prototype),
            script: Object.getOwnPropertyNames(TrustedScript.prototype),
            url: Object.getOwnPropertyNames(TrustedScriptURL.prototype),
            tags: ['TrustedTypePolicyFactory','TrustedTypePolicy','TrustedHTML','TrustedScript','TrustedScriptURL']
                .map(n => window[n].prototype[Symbol.toStringTag]),
            lengths: ['createPolicy','getAttributeType','getPropertyType','getTypeMapping','isHTML','isScript','isScriptURL']
                .map(n => TrustedTypePolicyFactory.prototype[n].length),
            policyLengths: ['createHTML','createScript','createScriptURL'].map(n => TrustedTypePolicy.prototype[n].length),
            news: ['TrustedTypePolicyFactory','TrustedTypePolicy','TrustedHTML','TrustedScript','TrustedScriptURL']
                .map(n => { try { new window[n](); return 'no throw'; } catch (e) { return e.message; } }),
            foreign: [
                (() => { try { TrustedHTML.prototype.toString.call({}); return 'no throw'; } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
                (() => { try { TrustedTypePolicy.prototype.createHTML.call({}, 'x'); return 'no throw'; } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
                (() => { try { TrustedTypePolicyFactory.prototype.isHTML.call({}, 'x'); return 'no throw'; } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
                (() => { try { Object.getOwnPropertyDescriptor(TrustedTypePolicy.prototype, 'name').get.call({}); return 'no throw'; } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
                (() => { try { Object.getOwnPropertyDescriptor(TrustedTypePolicyFactory.prototype, 'emptyHTML').get.call({}); return 'no throw'; } catch (e) { return e.constructor.name + ': ' + e.message; } })(),
            ],
            nameDesc: (d => [!!d.get, !!d.set, d.enumerable, d.configurable])(
                Object.getOwnPropertyDescriptor(TrustedTypePolicy.prototype, 'name')),
        })"#,
    )
    .await;
    // The captures' order, with `constructor` at the index each records (10 / 4 / 2 / 2 / 2).
    assert_eq!(
        strings(&v["factory"]),
        ["emptyHTML", "emptyScript", "defaultPolicy", "createPolicy", "getAttributeType",
         "getPropertyType", "getTypeMapping", "isHTML", "isScript", "isScriptURL", "constructor"]
    );
    assert_eq!(strings(&v["policy"]), ["name", "createHTML", "createScript", "createScriptURL", "constructor"]);
    assert_eq!(strings(&v["html"]), ["toJSON", "toString", "constructor"]);
    assert_eq!(strings(&v["script"]), ["toJSON", "toString", "constructor"]);
    assert_eq!(strings(&v["url"]), ["toJSON", "toString", "constructor"]);
    assert_eq!(
        strings(&v["tags"]),
        ["TrustedTypePolicyFactory", "TrustedTypePolicy", "TrustedHTML", "TrustedScript", "TrustedScriptURL"]
    );
    assert_eq!(v["lengths"], serde_json::json!([1, 2, 2, 0, 1, 1, 1]));
    assert_eq!(v["policyLengths"], serde_json::json!([1, 1, 1]));
    for (i, n) in ["TrustedTypePolicyFactory", "TrustedTypePolicy", "TrustedHTML", "TrustedScript", "TrustedScriptURL"]
        .iter()
        .enumerate()
    {
        assert_eq!(v["news"][i], format!("Failed to construct '{n}': Illegal constructor"));
    }
    // None of these rows returns a promise, so every one of them THROWS the bare sentence.
    for i in 0..5 {
        assert_eq!(v["foreign"][i], "TypeError: Illegal invocation", "foreign receiver {i}");
    }
    assert_eq!(v["nameDesc"], serde_json::json!([true, false, true, true]));
}

/// The TYPE MAP, which is a measurement rather than a rule — and the three members that read it.
#[tokio::test]
async fn the_type_map_is_the_measured_one() {
    let v = page(
        r#"(() => {
            const tt = trustedTypes;
            const HTML = 'http://www.w3.org/1999/xhtml', SVG = 'http://www.w3.org/2000/svg';
            const m = tt.getTypeMapping();
            return {
                keys: Object.keys(m),
                scriptAttrs: Object.keys(m.script.attributes),
                scriptProps: Object.keys(m.script.properties),
                objectAttrs: JSON.stringify(m.object),
                embed: JSON.stringify(m.embed),
                iframe: JSON.stringify(m.iframe),
                starKeys: Object.keys(m['*']),
                starProps: JSON.stringify(m['*'].properties),
                starCount: Object.keys(m['*'].attributes).length,
                starFirst: Object.keys(m['*'].attributes).slice(0, 3),
                starType: m['*'].attributes.onclick,
                fresh: tt.getTypeMapping() !== tt.getTypeMapping(),
                plainProto: Object.getPrototypeOf(m) === Object.prototype,
                mapEmptyNs: tt.getTypeMapping('') === null ? 'null' : 'map',
                mapHtmlNs: tt.getTypeMapping(HTML),
                mapSvgNs: tt.getTypeMapping(SVG),
                attrs: {
                    scriptSrc: tt.getAttributeType('script', 'src'),
                    scriptSrcSvg: tt.getAttributeType('script', 'src', SVG),
                    scriptHref: tt.getAttributeType('script', 'href'),
                    scriptHrefSvg: tt.getAttributeType('script', 'href', SVG),
                    iframeSrcdoc: tt.getAttributeType('iframe', 'srcdoc'),
                    iframeSrcdocSvg: tt.getAttributeType('iframe', 'srcdoc', SVG),
                    objectCodebase: tt.getAttributeType('object', 'codebase'),
                    divOnclick: tt.getAttributeType('div', 'onclick'),
                    divOnclickSvg: tt.getAttributeType('div', 'onclick', SVG),
                    divOnbogus: tt.getAttributeType('div', 'onbogus'),
                    upper: tt.getAttributeType('SCRIPT', 'SRC'),
                    mixed: tt.getAttributeType('div', 'onClick'),
                    unknownEl: tt.getAttributeType('bogus-element', 'onclick'),
                    attrNs: tt.getAttributeType('script', 'src', HTML, HTML),
                    bogusNs: tt.getAttributeType('script', 'src', 'urn:x'),
                    divId: tt.getAttributeType('div', 'id'),
                    oneArg: (() => { try { tt.getAttributeType('script'); return 'no throw'; }
                                     catch (e) { return e.constructor.name + ': ' + e.message; } })(),
                },
                props: {
                    divInnerHTML: tt.getPropertyType('div', 'innerHTML'),
                    divInnerHTMLSvg: tt.getPropertyType('div', 'innerHTML', SVG),
                    divUpper: tt.getPropertyType('div', 'INNERHTML'),
                    objectCodeBase: tt.getPropertyType('object', 'codeBase'),
                    objectCodebase: tt.getPropertyType('object', 'codebase'),
                    scriptText: tt.getPropertyType('script', 'text'),
                    scriptSrcSvg: tt.getPropertyType('script', 'src', SVG),
                    scriptHrefSvg: tt.getPropertyType('script', 'href', SVG),
                    divOnclick: tt.getPropertyType('div', 'onclick'),
                },
            };
        })()"#,
    )
    .await;
    assert_eq!(strings(&v["keys"]), ["embed", "iframe", "object", "script", "*"]);
    // The browsers' own order inside an entry — `src` before `href`, which is not alphabetical and
    // is why the measured file's key order has to survive the read.
    assert_eq!(strings(&v["scriptAttrs"]), ["src", "href"]);
    assert_eq!(strings(&v["scriptProps"]), ["innerText", "src", "text", "textContent", "href"]);
    assert_eq!(
        v["objectAttrs"],
        r#"{"attributes":{"codebase":"TrustedScriptURL","data":"TrustedScriptURL"},"properties":{"codeBase":"TrustedScriptURL","data":"TrustedScriptURL"}}"#
    );
    assert_eq!(v["embed"], r#"{"attributes":{"src":"TrustedScriptURL"},"properties":{"src":"TrustedScriptURL"}}"#);
    assert_eq!(v["iframe"], r#"{"attributes":{"srcdoc":"TrustedHTML"},"properties":{"srcdoc":"TrustedHTML"}}"#);
    assert_eq!(strings(&v["starKeys"]), ["attributes", "properties"]);
    assert_eq!(v["starProps"], r#"{"innerHTML":"TrustedHTML","outerHTML":"TrustedHTML"}"#);
    // 321 is what all four browsers list in common; Edge lists four more (`onwidget*`), which
    // `tools/sync/data/trusted_types_type_map.json` records in `not_in_all_four` and this engine does
    // not publish. That is the shortfall, and it is named rather than averaged away.
    assert_eq!(v["starCount"], 321);
    assert_eq!(strings(&v["starFirst"]), ["onabort", "onabortpayment", "onactivate"]);
    assert_eq!(v["starType"], "TrustedScript");
    assert_eq!(v["fresh"], true, "a new object every call — measured `!==` on both browsers");
    assert_eq!(v["plainProto"], true);
    assert_eq!(v["mapEmptyNs"], "map");
    assert_eq!(v["mapHtmlNs"], serde_json::Value::Null, "even the XHTML namespace answers null");
    assert_eq!(v["mapSvgNs"], serde_json::Value::Null);

    let a = &v["attrs"];
    assert_eq!(a["scriptSrc"], "TrustedScriptURL");
    assert_eq!(a["scriptSrcSvg"], serde_json::Value::Null);
    assert_eq!(a["scriptHref"], serde_json::Value::Null);
    assert_eq!(a["scriptHrefSvg"], "TrustedScriptURL");
    assert_eq!(a["iframeSrcdoc"], "TrustedHTML");
    assert_eq!(a["iframeSrcdocSvg"], serde_json::Value::Null);
    assert_eq!(a["objectCodebase"], "TrustedScriptURL");
    assert_eq!(a["divOnclick"], "TrustedScript");
    assert_eq!(a["divOnclickSvg"], "TrustedScript", "the `*` rows apply in both namespaces");
    assert_eq!(a["divOnbogus"], serde_json::Value::Null, "the handler set is a LIST, not `starts with on`");
    assert_eq!(a["upper"], "TrustedScriptURL", "tag and attribute are ASCII-lowercased");
    assert_eq!(a["mixed"], "TrustedScript");
    assert_eq!(a["unknownEl"], "TrustedScript");
    assert_eq!(a["attrNs"], serde_json::Value::Null, "a non-null attrNs answers null, even the XHTML one");
    assert_eq!(a["bogusNs"], serde_json::Value::Null);
    assert_eq!(a["divId"], serde_json::Value::Null);
    assert_eq!(
        a["oneArg"],
        "TypeError: Failed to execute 'getAttributeType' on 'TrustedTypePolicyFactory': 2 arguments required, but only 1 present."
    );

    let p = &v["props"];
    assert_eq!(p["divInnerHTML"], "TrustedHTML");
    assert_eq!(p["divInnerHTMLSvg"], "TrustedHTML");
    assert_eq!(p["divUpper"], serde_json::Value::Null, "a PROPERTY name keeps its case");
    assert_eq!(p["objectCodeBase"], "TrustedScriptURL");
    assert_eq!(p["objectCodebase"], serde_json::Value::Null);
    assert_eq!(p["scriptText"], "TrustedScript");
    assert_eq!(p["scriptSrcSvg"], serde_json::Value::Null);
    assert_eq!(p["scriptHrefSvg"], "TrustedScriptURL");
    assert_eq!(p["divOnclick"], serde_json::Value::Null, "a handler is not a property");
}

/// The document delivered under one `Content-Security-Policy` header, then `js` run after `load`.
async fn delivered(csp: &str, js: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    let headers = [("content-type".to_string(), "text/html; charset=utf-8".to_string()), ("content-security-policy".to_string(), csp.to_string())];
    e.load_page_delivered(DOC, URL, &[], &headers).await.expect("load_page");
    let _ = e.pump_until_loaded(Duration::from_secs(5)).await;
    e.eval_value("tt", js.to_string()).expect("eval")
}

/// A STRING BECOMING CODE asks the realm's Trusted Types and CSP, in Blink's order (korvus-analysis
/// U-1: a bot-challenge frame's `eval` called the default policy's `createScript` in Chrome and
/// never in korvus). Every row is Chrome 150.0.7871.46's answer for the same header and script,
/// measured 2026-10-07 (`google-chrome --headless=new --dump-dom`):
///
/// - `eval` / `Function`: the default policy is asked with sink "eval" or "Function" (read off the
///   source V8 builds); refused (EvalError) with no policy, a null answer, a throw, or a CHANGED answer;
///   `'trusted-types-eval'` lets a string through untouched; then `'unsafe-eval'` decides.
/// - a string timer: sink "Window setTimeout" / "Window setInterval", a changed answer IS the script, no
///   policy throws a TypeError; then without `'unsafe-eval'` -- `'trusted-types-eval'` or not -- it
///   answers 0 and never runs.
#[tokio::test]
async fn a_string_becoming_code_asks_trusted_types_then_csp() {
    const PROBE: &str = r#"(() => {
        const r = [];
        const t = (n, f) => { try { r.push(n + ':' + JSON.stringify(f())) } catch (e) { r.push(n + ':' + e.name + (e instanceof TypeError ? ':' + e.message : '')) } };
        if (globalThis.__policy) trustedTypes.createPolicy('default', { createScript(s, a, b) {
            r.push('cs:' + [s, a, b].join('|'));
            return /nul/.test(s) ? null : /thr/.test(s) ? (() => { throw 1 })() : /chg/.test(s) ? s + '+1' : s } });
        t('eval', () => eval('1+1'));
        t('nul', () => eval('"nul"'));
        t('thr', () => eval('"thr"'));
        t('chg', () => eval('"chg"'));
        t('fn', () => new Function('return 5')());
        t('obj', () => typeof eval({}));
        t('timer', () => setTimeout('0', 0) > 0);
        t('emptytimer', () => setTimeout('', 0));
        return r;
    })()"#;
    let with_policy = |js: &str| format!("globalThis.__policy = true; {js}");
    let tt = "require-trusted-types-for 'script'";
    let rows: &[(&str, &str, bool, &[&str])] = &[
        ("no policy at all", "img-src *", false, &["eval:2", "nul:\"nul\"", "thr:\"thr\"", "chg:\"chg\"", "fn:5", "obj:\"object\"", "timer:true", "emptytimer:0"]),
        ("Trusted Types, no default policy", tt, false, &[
            "eval:EvalError", "nul:EvalError", "thr:EvalError", "chg:EvalError", "fn:EvalError", "obj:\"object\"",
            "timer:TypeError:Failed to execute 'setTimeout' on 'Window': This document requires 'TrustedScript' assignment.",
            "emptytimer:TypeError:Failed to execute 'setTimeout' on 'Window': This document requires 'TrustedScript' assignment.",
        ]),
        ("Trusted Types with a default policy", tt, true, &[
            "cs:1+1|TrustedScript|eval", "eval:2",
            "cs:\"nul\"|TrustedScript|eval", "nul:EvalError",
            "cs:\"thr\"|TrustedScript|eval", "thr:EvalError",
            "cs:\"chg\"|TrustedScript|eval", "chg:EvalError",
            "cs:(function anonymous(\n) {\nreturn 5\n})|TrustedScript|Function", "fn:5",
            "obj:\"object\"",
            "cs:0|TrustedScript|Window setTimeout", "timer:true",
            "cs:|TrustedScript|Window setTimeout", "emptytimer:0",
        ]),
        ("no 'unsafe-eval'", "script-src 'unsafe-inline'", false, &["eval:EvalError", "nul:EvalError", "thr:EvalError", "chg:EvalError", "fn:EvalError", "obj:\"object\"", "timer:false", "emptytimer:0"]),
        ("'trusted-types-eval' lets eval through, not a timer", "require-trusted-types-for 'script'; script-src 'unsafe-inline' 'trusted-types-eval'", true, &[
            "eval:2", "nul:\"nul\"", "thr:\"thr\"", "chg:\"chg\"", "fn:5", "obj:\"object\"",
            "cs:0|TrustedScript|Window setTimeout", "timer:false",
            "cs:|TrustedScript|Window setTimeout", "emptytimer:0",
        ]),
    ];
    let mut bad = Vec::new();
    for (label, csp, policy, want) in rows {
        let js = if *policy { with_policy(PROBE) } else { PROBE.to_string() };
        let got = strings(&delivered(csp, &js).await);
        if got != *want {
            bad.push(format!("{label}:\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
