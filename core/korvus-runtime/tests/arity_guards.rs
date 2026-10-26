//! WebIDL arity guards (unit: arity-guards).
//!
//! A WebIDL operation with required arguments RAISES `TypeError` when called with too few, and one
//! whose arguments are ALL optional does not. Both halves are asserted here because only one of them
//! is a bug when it is wrong: a missing guard fails a recorded case, an EXTRA guard makes a legal
//! call throw and breaks the page. `document.getElementById()` must throw and
//! `document.createRange()` must not, and that pair is the whole contract in one line.
//!
//! RAISES, not "throws", and the difference is the last test in this file: a PROMISE-returning
//! operation delivers that same `TypeError` as a REJECTION of the promise it already committed to
//! returning. Every case in the first three tests is a non-promise member, so a guard that threw for
//! everything passed all of them -- which is how the promise half stayed wrong (ARITY1).
//!
//! The third assertion is the one nothing else would catch: a member's `length` is the count of its
//! required arguments and is graded on its own axis, so wrapping a member in a guard must leave it
//! exactly as it was.
use engine::Engine;
async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(r#"<!doctype html><html><body><div id="d">x</div></body></html>"#).unwrap();
    e
}

/// `expr` evaluated to a string, so a thrown error and a returned value are both reportable.
fn probe(e: &mut Engine, expr: &str) -> String {
    let src = format!(
        r#"(() => {{ try {{ const v = ({expr}); return "ok:" + String(v); }}
                     catch (err) {{ return "throw:" + err.constructor.name + ":" + err.message; }} }})()"#
    );
    e.eval_value("arity-probe", src)
        .unwrap_or_else(|err| panic!("eval failed: {err}"))
        .as_str()
        .expect("probe returns a string")
        .to_string()
}

#[tokio::test]
async fn a_required_argument_member_throws_and_an_optional_one_does_not() {
    let mut e = page().await;

    // REQUIRED. `Element? getElementById(DOMString elementId)` -- one required argument, and Chrome's
    // message is reproduced verbatim (binding/protos.rs::require_args), pluralisation and trailing
    // period included, because a page can read `e.message`.
    let got = probe(&mut e, "document.getElementById()");
    assert_eq!(
        got,
        "throw:TypeError:Failed to execute 'getElementById' on 'Document': 1 argument required, but only 0 present.",
        "document.getElementById() must throw the WebIDL arity TypeError"
    );
    // ...and the same member with its argument still works.
    assert_eq!(probe(&mut e, "document.getElementById('d').id"), "ok:d");

    // ALL-OPTIONAL. `[NewObject] Range createRange()` takes no arguments at all. Guarding it would
    // make a legal call throw -- a worse defect than the one this unit fixes, and one that breaks
    // pages rather than tests.
    let got = probe(&mut e, "Object.prototype.toString.call(document.createRange())");
    assert_eq!(got, "ok:[object Range]", "document.createRange() must NOT throw");

    // An optional argument with a default is not required either: `getElementsByTagName` takes one,
    // but `Range.prototype.cloneContents()` and `Element.prototype.getBoundingClientRect()` take none.
    assert_eq!(
        probe(&mut e, "Object.prototype.toString.call(document.getElementById('d').getBoundingClientRect())"),
        "ok:[object DOMRect]"
    );

    // Window's members are own properties of the global object, not of Window.prototype, so they are
    // guarded through their own list -- this is the half a per-interface pass would have missed.
    assert_eq!(
        probe(&mut e, "window.btoa()"),
        "throw:TypeError:Failed to execute 'btoa' on 'Window': 1 argument required, but only 0 present."
    );
    assert_eq!(probe(&mut e, "window.btoa('ab')"), "ok:YWI=");

    // A member with two required arguments reports two, and one argument is still too few.
    assert_eq!(
        probe(&mut e, "document.body.addEventListener('x')"),
        "throw:TypeError:Failed to execute 'addEventListener' on 'EventTarget': 2 arguments required, but only 1 present."
    );

    // A VARIADIC argument is not a required one: `undefined write(DOMString... text)` accepts zero. Last,
    // because on a loaded page the call implies `document.open()`, which empties the document, as in Chrome.
    assert!(
        !probe(&mut e, "document.write()").starts_with("throw:TypeError:Failed to execute"),
        "document.write() must not raise an arity TypeError"
    );
}

#[tokio::test]
async fn the_guard_does_not_change_a_member_s_length() {
    let mut e = page().await;
    // `length` IS the count of required arguments and is graded on its own axis. The guard is built
    // with the length of the function it replaces, so these are the pre-guard values by construction.
    for (expr, want) in [
        ("document.getElementById.length", "ok:1"),
        ("document.createRange.length", "ok:0"),
        ("document.createElement.length", "ok:1"),
        ("Node.prototype.appendChild.length", "ok:1"),
        ("Node.prototype.insertBefore.length", "ok:2"),
        ("EventTarget.prototype.addEventListener.length", "ok:2"),
        ("window.btoa.length", "ok:1"),
        ("window.setTimeout.length", "ok:1"),
        ("Element.prototype.getAttribute.length", "ok:1"),
        ("Element.prototype.setAttribute.length", "ok:2"),
    ] {
        assert_eq!(probe(&mut e, expr), want, "{expr}");
    }
}

#[tokio::test]
async fn a_guarded_member_is_still_a_native_function() {
    let mut e = page().await;
    // The guard is `build_fn_data`, a real v8::Function -- not a JS wrapper. `own-tells` and
    // `name-defects` are the stealth axes that would catch the difference, so assert both directly:
    // `[native code]`, the member's own name, and no own `prototype`/`arguments`/`caller`.
    assert_eq!(
        probe(&mut e, "Function.prototype.toString.call(document.getElementById).includes('[native code]')"),
        "ok:true"
    );
    assert_eq!(probe(&mut e, "document.getElementById.name"), "ok:getElementById");
    assert_eq!(
        probe(
            &mut e,
            "Object.getOwnPropertyNames(document.getElementById).filter(n => n !== 'name' && n !== 'length').join(',')"
        ),
        "ok:"
    );
}

/// A PROMISE-RETURNING operation REJECTS the arity refusal; it does not throw it.
///
/// The half of the rule this file did not assert, and the reason it could be wrong for as long as it
/// was: every case above is a non-promise member, so a guard that threw for everything passed them
/// all. WebIDL turns a `TypeError` raised while invoking a promise-returning operation into a
/// rejection of the promise the call already committed to returning, and Chrome does that for the
/// ARGUMENT COUNT as much as for the receiver.
///
/// MEASURED, Google Chrome 150.0.7871.46 `--headless=new --dump-dom` over a loopback server (a
/// secure context), 2026-09-23, AWAITING each promise rather than inferring from
/// `typeof r.then === "function"`:
///
/// ```text
///   customElements.whenDefined()      REJECT TypeError: Failed to execute 'whenDefined' on
///                                       'CustomElementRegistry': 1 argument required, but only 0 present.
///   document.fonts.load()             REJECT TypeError: Failed to execute 'load' on 'FontFaceSet':
///                                       1 argument required, but only 0 present.
///   VideoDecoder.isConfigSupported()  REJECT TypeError: Failed to execute 'isConfigSupported' on
///                                       'VideoDecoder': 1 argument required, but only 0 present.
///   document.getElementById()         THROW  <- the control, and it is why the rest proves
///                                       anything: this probe CAN see a synchronous throw.
/// ```
///
/// 94 of the registry's 96 `Promise`-typed rows with `required >= 1` answer REJECT in that browser,
/// and 258 non-promise rows with a required argument answer THROW. So the pair below is the whole
/// contract: drop the control and a guard that rejected EVERYTHING would pass.
#[tokio::test]
async fn a_promise_returning_member_rejects_a_missing_argument_and_a_plain_one_throws() {
    let mut e = page().await;
    // Phase 1: call each member, register the handlers, record the SYNCHRONOUS outcome at once.
    let src = r#"(() => {
        globalThis.__arity = [];
        const push = (s) => globalThis.__arity.push(s);
        const one = (label, f) => {
            let r;
            try { r = f(); }
            catch (err) { push(label + "=throw:" + err.constructor.name + ":" + err.message); return; }
            if (r && typeof r.then === "function") {
                r.then(v  => push(label + "=resolve:" + Object.prototype.toString.call(v)),
                       err => push(label + "=reject:" + err.constructor.name + ":" + err.message));
            } else {
                push(label + "=value:" + Object.prototype.toString.call(r));
            }
        };
        // Two HAND-TABLE rows (`install::live::method_row`) and one GENERATED-REGISTRY stub row
        // (`installer::put_d2_stub`) -- the two mounts that build the guard, and 96 of the rows this
        // rule governs are the second kind.
        one("whenDefined", () => customElements.whenDefined());
        one("fontsLoad", () => document.fonts.load());
        one("videoDecoder", () => VideoDecoder.isConfigSupported());
        // THE CONTROL: a non-promise member with a required argument must still THROW.
        one("getElementById", () => document.getElementById());
        return "probed";
    })()"#;
    assert_eq!(probe(&mut e, src), "ok:probed");
    // Phase 2: the microtask checkpoint, so every rejection above has been observed.
    e.pump(std::time::Duration::from_millis(50)).await.expect("pump");
    let got = probe(&mut e, "globalThis.__arity.slice().sort().join('|')");
    assert_eq!(
        got,
        "ok:fontsLoad=reject:TypeError:Failed to execute 'load' on 'FontFaceSet': 1 argument required, but only 0 present.\
         |getElementById=throw:TypeError:Failed to execute 'getElementById' on 'Document': 1 argument required, but only 0 present.\
         |videoDecoder=reject:TypeError:Failed to execute 'isConfigSupported' on 'VideoDecoder': 1 argument required, but only 0 present.\
         |whenDefined=reject:TypeError:Failed to execute 'whenDefined' on 'CustomElementRegistry': 1 argument required, but only 0 present.",
        "a promise row must REJECT the arity TypeError and a plain row must THROW the same sentence"
    );
}
