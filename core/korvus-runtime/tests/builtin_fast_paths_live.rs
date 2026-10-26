//! LIVE gate: the ECMAScript built-ins keep V8's fast paths -- the engine writes nothing on their
//! prototypes.
//!
//! The seal (`binding::protos::seal_interface_protos`) names every prototype the global carries,
//! `RegExp.prototype` and `Array.prototype` included, and it named them with a private property ON the
//! object. One more property is a new hidden class, and V8's fast paths ask a built-in prototype for its
//! initial one: `'ab'.replace(/a/, fn)` then ran through the generic `RegExp.prototype[Symbol.replace]`,
//! which a page reads as an extra frame in `Error.stack` (and times: 200 000 replaces were ~3x slower).
//! a bot challenge reads stacks.
//!
//! **Expectations read from Google Chrome 150.0.7871.46 on this machine on 2026-10-07** (the same
//! script, served from 127.0.0.1): the callback's caller is `String.replace` for a plain RegExp, and
//! `[Symbol.replace]` only for one whose `exec` is an own property -- the slow path Chrome takes too.

use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body><iframe></iframe></body></html>").unwrap();
    e.run().await.unwrap();
    e
}

fn eval(e: &mut Engine, tag: &'static str, src: &str) -> String {
    e.eval_value(tag, src.to_string()).unwrap().as_str().unwrap_or_default().to_string()
}

/// The callback's caller frame, for each way of calling `replace`, in `w`'s realm.
const PROBE: &str = r#"((w) => {
    const R = [];
    const caller = () => new w.Error().stack.split('\n')[3].trim();
    w.String.prototype.replace.call('ab', new w.RegExp('a'), () => { R.push(caller()); return 'c'; });
    w.String.prototype.replace.call('ab', new w.RegExp('a', 'g'), () => { R.push(caller()); return 'c'; });
    const own = new w.RegExp('a'); own.exec = w.RegExp.prototype.exec;
    w.String.prototype.replace.call('ab', own, () => { R.push(caller()); return 'c'; });
    return R.join('|');
})"#;

const CHROME: &str = "at String.replace (<anonymous>)|at String.replace (<anonymous>)|at [Symbol.replace] (<anonymous>)";

#[tokio::test]
async fn regexp_replace_takes_the_fast_path_as_chrome_does() {
    let mut e = page().await;
    assert_eq!(eval(&mut e, "rx-window", &format!("{PROBE}(window)")), CHROME);
    // a frame's realm is sealed the same way
    assert_eq!(eval(&mut e, "rx-frame", &format!("{PROBE}(document.querySelector('iframe').contentWindow)")), CHROME);
}

/// The names still answer where the engine asks them: a structured clone of a plain object and of an
/// array, and a built-in's prototype across realms.
#[tokio::test]
async fn builtin_prototypes_are_still_known_to_the_engine() {
    let mut e = page().await;
    let src = r#"(() => {
        const f = document.querySelector('iframe').contentWindow;
        const o = structuredClone(new f.Object());
        const a = structuredClone(f.Array.of(1, 2));
        return [Object.getPrototypeOf(o) === Object.prototype, Array.isArray(a), Object.getPrototypeOf(a) === Array.prototype, a.join()].join('|');
    })()"#;
    assert_eq!(eval(&mut e, "rx-clone", src), "true|true|true|1,2");
}
