//! G1.1 acceptance gate: the engine runs real JS with correct semantics + a working event loop.
//! Single test fn (isolates created sequentially) to avoid concurrent V8 isolate creation.
//! Results are observed via the native `print(...)` op.

use engine::Engine;

/// Minimal persona for engine tests.

fn engine() -> Engine {
    Engine::new(Box::new(persona::CapturedPersona::default()))
}

/// Run a script, drive the event loop to completion, return captured `print(...)` output.
async fn output_of(src: &str) -> Vec<String> {
    let mut e = engine();
    e.eval("test", src.to_string()).expect("eval");
    e.run().await.expect("event loop");
    e.take_output()
}

/// G1.8 prerequisite: eval returns the completion value to Rust as JSON (for CDP Runtime.evaluate).
#[tokio::test]
async fn eval_returns_values() {
    let mut e = engine();
    assert_eq!(e.eval_value("v", "1 + 2 * 3".into()).unwrap(), serde_json::json!(7));
    assert_eq!(
        e.eval_value("v", "JSON.stringify({a:1})".into()).unwrap(),
        serde_json::json!("{\"a\":1}")
    );
    assert_eq!(
        e.eval_value("v", "({ x: [1, 2], y: true, z: \"hi\" })".into()).unwrap(),
        serde_json::json!({"x": [1, 2], "y": true, "z": "hi"})
    );
}

#[tokio::test]
async fn g1_1_gate() {
    // 1. arithmetic (correct operator precedence via real V8)
    assert_eq!(output_of("print(String(1 + 2 * 3))").await, vec!["7"], "arithmetic");

    // 2. JSON built-in
    assert_eq!(
        output_of("print(JSON.stringify({a:1,b:[2,3]}))").await,
        vec!["{\"a\":1,\"b\":[2,3]}"],
        "JSON"
    );

    // 3. Promise / microtask ordering — `.then` runs after sync code
    assert_eq!(
        output_of(r#"print("a"); Promise.resolve().then(() => print("c")); print("b");"#).await,
        vec!["a", "b", "c"],
        "microtask ordering"
    );

    // 4. setTimeout — callback fires after sync code once the loop is driven
    assert_eq!(
        output_of(r#"setTimeout(() => print("late"), 5); print("early");"#).await,
        vec!["early", "late"],
        "setTimeout"
    );

    // 5. async / await
    assert_eq!(
        output_of(r#"(async () => { const x = await Promise.resolve(41); print(String(x + 1)); })();"#)
            .await,
        vec!["42"],
        "async/await"
    );
}
