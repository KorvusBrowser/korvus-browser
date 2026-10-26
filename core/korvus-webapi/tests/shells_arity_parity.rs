//! Phase 4 arity liveness gate: the Rust arity pass (`op_finalize_surface`) is applied on the live
//! surface, so a WebIDL method with required arguments throws when called with too few. Before 3d this
//! compared the JS and Rust passes byte-for-byte; the JS pass was deleted with generated_shells.js, so
//! this now checks the single (Rust) path directly on a handful of well-known configurable methods --
//! enough to catch the pass silently not running (which no surface gate can see, since a guard keeps
//! the member's name and length).

use engine::Engine;

const PAGE: &str = include_str!("fixtures/snapshot-page.html");

// Known configurable methods with WebIDL-required arguments; calling with zero args must throw the
// arity error (the guard checks arity before forwarding, so the receiver never matters here).
const PROBE: &str = r#"(() => {
    const el = document.createElement("div");
    const cases = [
        ["document.getElementById", () => document.getElementById()],
        ["document.createElement", () => document.createElement()],
        ["document.querySelector", () => document.querySelector()],
        ["Element.setAttribute", () => el.setAttribute()],
        ["Element.getAttribute", () => el.getAttribute()],
        ["Node.appendChild", () => el.appendChild()],
    ];
    const out = {};
    for (const [name, fn] of cases) {
        try { fn(); out[name] = "NO-THROW"; }
        catch (e) { out[name] = String((e && e.message) || e).slice(0, 60); }
    }
    return JSON.stringify(out);
})()"#;

#[tokio::test]
async fn arity_guards_are_live() {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html(PAGE).unwrap();
    e.run().await.unwrap();
    let raw = e.eval_value("arity-probe", PROBE.to_string()).unwrap();
    let out: std::collections::HashMap<String, String> = serde_json::from_str(raw.as_str().unwrap()).unwrap();

    let no_throw: Vec<&String> = out.iter().filter(|(_, v)| *v == "NO-THROW").map(|(k, _)| k).collect();
    assert!(
        no_throw.is_empty(),
        "arity guards are NOT live -- these required-arg methods did not throw when called with too \
         few arguments (op_finalize_surface did not run): {no_throw:?}\nfull: {out:#?}"
    );
}
