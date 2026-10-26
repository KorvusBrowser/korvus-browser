//! NAMES THE DOM REFUSES, AND WHERE AN UNCAUGHT ERROR SAYS IT HAPPENED -- against Chrome's own answers.
//!
//! # What was wrong (found by korvus-analysis's recorder runs, 2026-10-05)
//!
//! * `document.createElement` made an element of ANY string: `"1bad"`, `""`, `"a b"`. Chrome throws
//!   `InvalidCharacterError` for a name that is not a valid element local name (WHATWG DOM, 2025).
//!   Its attribute siblings were wrong both ways: `setAttribute` / `toggleAttribute` / `setAttributeNS`
//!   took `""`, `"a b"` and `"a=b"`, while `createAttribute` / `createAttributeNS` refused `1bad`, `-x` and
//!   `a@b`, which the current rules accept.
//! * An uncaught error a PLATFORM call threw -- `document.querySelector('<<<')` in a timer -- reached
//!   `onerror` as `"Uncaught "` at line 0, column 0. Three causes, one symptom each:
//!   - a DOMException the platform throws had no `stack` (Blink captures one), so nothing recorded
//!     where it came from; a page-made one thrown from a timer had no location either;
//!   - V8's message for a thrown non-Error object is the bare `"Uncaught "`;
//!   - an inline script's lines counted from its own first line, not from the document's, so EVERY
//!     inline script's error line and `stack` line was short by the script's position.
//!   Also the selector methods said `'<<<' is not a valid selector` without Blink's
//!   `Failed to execute 'querySelector' on 'Document': ` and final period.
//!
//! # The oracle: stock Chrome 150.0.7871.46, 2026-10-05
//!
//! Every expected string below was read off Chrome on the same markup;
//! the line numbers are this file's markup, laid out exactly as it was there.
use engine::Engine;
use std::time::Duration;

async fn run(html: &str, read: &str) -> serde_json::Value {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.expect("boot");
    e.load_page(html, "http://127.0.0.1:8765/t.html", &[]).await.expect("load_page");
    e.pump_until_loaded(Duration::from_secs(5)).await.expect("pump");
    e.pump(Duration::from_millis(300)).await.expect("pump 2");
    let v = e.eval_value("read", read.to_string()).expect("eval");
    serde_json::from_str(v.as_str().expect("a JSON string")).expect("json")
}

const NAMES: &str = r#"JSON.stringify((() => {
  const m = (f) => { try { return 'ok:' + f(); } catch (x) { return x.name + ': ' + x.message; } };
  const b = document.body;
  return {
    ce_digit: m(() => document.createElement('1bad').localName),
    ce_empty: m(() => document.createElement('')),
    ce_space: m(() => document.createElement('a b')),
    ce_colon: m(() => document.createElement(':a').localName),
    ce_eq: m(() => document.createElement('a=b').localName),
    ce_nonascii_first: m(() => document.createElement('·a').localName),
    sa_empty: m(() => { b.setAttribute('', ''); return 'set'; }),
    sa_eq: m(() => { b.setAttribute('a=b', ''); return 'set'; }),
    sa_digit: m(() => { b.setAttribute('1bad', ''); return 'set'; }),
    ta_space: m(() => b.toggleAttribute('a b')),
    ca_digit: m(() => document.createAttribute('1bad').name),
    ca_at: m(() => document.createAttribute('a@b').name),
    ca_space: m(() => document.createAttribute('a b').name),
    can_empty: m(() => document.createAttributeNS('urn:x', '').name),
    can_prefix: m(() => document.createAttributeNS('urn:x', ':a').name),
    can_digit: m(() => document.createAttributeNS('urn:x', '1bad').name),
    san_space: m(() => { b.setAttributeNS('urn:x', 'a b', ''); return 'set'; }),
    san_xmlns: m(() => { b.setAttributeNS('urn:x', 'xmlns', ''); return 'set'; }),
    qs: m(() => document.querySelector('<<<')),
    qs_empty: m(() => document.querySelector('')),
    eqsa: m(() => b.querySelectorAll('<<<')),
    fqs: m(() => document.createDocumentFragment().querySelector('<<<')),
    wms: m(() => b.webkitMatchesSelector('<<<')),
  };
})())"#;

/// EVERY NAME RULE AND EVERY MESSAGE IS CHROME'S.
#[tokio::test]
async fn names_are_refused_and_accepted_as_chrome_does_in_its_words() {
    let r = run("<!doctype html><body>", NAMES).await;
    let want: &[(&str, &str)] = &[
        ("ce_digit", "InvalidCharacterError: Failed to execute 'createElement' on 'Document': The tag name provided ('1bad') is not a valid name."),
        ("ce_empty", "InvalidCharacterError: Failed to execute 'createElement' on 'Document': The tag name provided ('') is not a valid name."),
        ("ce_space", "InvalidCharacterError: Failed to execute 'createElement' on 'Document': The tag name provided ('a b') is not a valid name."),
        ("ce_colon", "ok::a"),
        ("ce_eq", "ok:a=b"),
        ("ce_nonascii_first", "ok:\u{b7}a"),
        ("sa_empty", "InvalidCharacterError: Failed to execute 'setAttribute' on 'Element': '' is not a valid attribute name."),
        ("sa_eq", "InvalidCharacterError: Failed to execute 'setAttribute' on 'Element': 'a=b' is not a valid attribute name."),
        ("sa_digit", "ok:set"),
        ("ta_space", "InvalidCharacterError: Failed to execute 'toggleAttribute' on 'Element': 'a b' is not a valid attribute name."),
        ("ca_digit", "ok:1bad"),
        ("ca_at", "ok:a@b"),
        ("ca_space", "InvalidCharacterError: Failed to execute 'createAttribute' on 'Document': The localName provided ('a b') contains an invalid character."),
        ("can_empty", "InvalidCharacterError: Failed to execute 'createAttributeNS' on 'Document': The qualified name provided is empty."),
        ("can_prefix", "InvalidCharacterError: Failed to execute 'createAttributeNS' on 'Document': The qualified name provided (':a') has an empty namespace prefix."),
        ("can_digit", "ok:1bad"),
        ("san_space", "InvalidCharacterError: Failed to execute 'setAttributeNS' on 'Element': The qualified name provided ('a b') contains the invalid character ' '."),
        ("san_xmlns", "NamespaceError: Failed to execute 'setAttributeNS' on 'Element': 'urn:x' is an invalid namespace for attributes."),
        ("qs", "SyntaxError: Failed to execute 'querySelector' on 'Document': '<<<' is not a valid selector."),
        ("qs_empty", "SyntaxError: Failed to execute 'querySelector' on 'Document': The provided selector is empty."),
        ("eqsa", "SyntaxError: Failed to execute 'querySelectorAll' on 'Element': '<<<' is not a valid selector."),
        ("fqs", "SyntaxError: Failed to execute 'querySelector' on 'DocumentFragment': '<<<' is not a valid selector."),
        ("wms", "SyntaxError: Failed to execute 'webkitMatchesSelector' on 'Element': '<<<' is not a valid selector."),
    ];
    let bad: Vec<String> = want
        .iter()
        .filter(|(k, v)| r.get(*k).and_then(|x| x.as_str()) != Some(*v))
        .map(|(k, v)| format!("{k}: got {:?}, Chrome {v:?}", r.get(*k)))
        .collect();
    assert!(bad.is_empty(), "differs from Chrome 150:\n  {}", bad.join("\n  "));
}

// The markup's own line numbers are what both engines report: the second <script> starts on line 4.
const UNCAUGHT: &str = "<!DOCTYPE html><html><body><script>
window.T = [];
window.onerror = function (msg, src, line, col, err) { T.push([msg, line, col, err && err.name]); return true; };
</script><script>
function f() { try { document.querySelector('<<<'); } catch (e) { return e; } }
setTimeout(function () { document.querySelector('<<<'); }, 0);
setTimeout(function () { throw new DOMException('mine', 'SyntaxError'); }, 0);
setTimeout(function () { null.x; }, 0);
window.S = [f().stack, Object.prototype.hasOwnProperty.call(f(), 'stack'), new DOMException('m').stack === undefined];
</script><script>document.querySelector('<<<');</script></body></html>";

/// AN UNCAUGHT ERROR NAMES WHAT CHROME NAMES, WHERE CHROME SAYS, and a platform DOMException carries a
/// stack of its caller's frames, numbered from the document's first line.
#[tokio::test]
async fn an_uncaught_error_reports_chromes_message_line_and_column() {
    let r = run(UNCAUGHT, "JSON.stringify([window.T, window.S])").await;
    let qs = "Uncaught SyntaxError: Failed to execute 'querySelector' on 'Document': '<<<' is not a valid selector.";
    let want = serde_json::json!([
        [
            [qs, 10, 27, "SyntaxError"],
            [qs, 6, 35, "SyntaxError"],
            ["Uncaught SyntaxError: mine", 7, 26, "SyntaxError"],
            ["Uncaught TypeError: Cannot read properties of null (reading 'x')", 8, 31, "TypeError"]
        ],
        [
            "SyntaxError: Failed to execute 'querySelector' on 'Document': '<<<' is not a valid selector.\n    at f (http://127.0.0.1:8765/t.html:5:31)\n    at http://127.0.0.1:8765/t.html:9:13",
            true,
            true
        ]
    ]);
    assert_eq!(r, want);
}
