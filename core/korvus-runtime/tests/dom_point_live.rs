//! LIVE one-table gate for the DOMPoint family (Phase 5 F1a; mirrors dom_rect_live.rs).
//!
//! Boots the real engine -- whose context creation now reconciles the natively-realised DOMPoint
//! family to its `interface!` table via `install::live::reconcile_dom_point_family`, the shell having
//! been removed from the registry (table-owned) -- and asserts the running interface is byte-identical to
//! the captured Brave surface: prototype members + descriptors, statics, the tag, `fromPoint.length`,
//! AND both inheritance chains. The constructor chain (`DOMPoint.__proto__ === DOMPointReadOnly`) is
//! the one the reconcile FIXES (op2 leaves it at Function.prototype). Setter coercion (`p.x = "5"` ->
//! number, from the 08c num() glue) must still work -- the reconcile touches shape, never bodies.

use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// Ordered `name:kind` describe lines the reconcile must produce for an interface's own prototype,
/// read from the surface fixture, plus the trailing non-enumerable `constructor`.
fn expected_proto(iface: &str) -> Vec<String> {
    let fx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(persona::shipped_artifact("surface")).unwrap()).unwrap();
    let mut out: Vec<String> = fx["interfaces"][iface]["members"].as_array().unwrap().iter().map(|m| {
        let n = m["name"].as_str().unwrap();
        match m["kind"].as_str().unwrap() {
            "accessor" => format!("{n}:accessor:e=1:c=1:s={}", m["set"].as_bool().unwrap() as u8),
            "method" => format!("{n}:data:e=1:c=1:w=1"),
            k => panic!("unexpected kind {k}"),
        }
    }).collect();
    // `constructor` AT THE CAPTURE'S SEAT, not appended. It is the last member
    // installed at interface-definition time, so a member a PARTIAL interface adds lands AFTER it --
    // `Navigator` 32 of 66, `Document` 240 of 247, `Screen` 9 of 11 with `onchange` and `isExtended`
    // behind it. Appending it was right on 83 % of the surface and wrong on exactly the interfaces a
    // fingerprinting script reads, and the index has been in the bundle since `3a5f0b60`.
    let at = fx["interfaces"][iface]["ctorIndex"].as_u64().map(|n| n as usize).unwrap_or(out.len()).min(out.len());
    out.insert(at, "constructor:data:e=0:c=1:w=1".to_string());
    out
}

#[tokio::test]
async fn live_dom_point_family_is_byte_identical_to_the_capture() {
    let mut e = page().await;

    let describe = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
        const d = Object.getOwnPropertyDescriptor(o, n);
        return (d.get || d.set)
            ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
            : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
    }).join("\n");"#;

    for iface in ["DOMPointReadOnly", "DOMPoint"] {
        let src = format!(r#"(() => {{ {describe} return desc({iface}.prototype); }})()"#);
        let got = e.eval_value("dp-desc", src).unwrap().as_str().unwrap().to_string();
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != capture");
    }

    // The rest of the surface: chains (BOTH), tag, statics length, and coercion (a body behavior that
    // must survive a shape-only reconcile).
    let src = r#"(() => {
        const rro = DOMPointReadOnly, r = DOMPoint;
        const q = new r(0,0,0,0); q.x = "5";
        // WebIDL `unrestricted double` from the ROW (`attr "z" : Double`, writable): ToNumber, never
        // a throw, so a non-numeric string is NaN.
        const b = new r(0,0,0,0); let threw = false;
        try { b.z = "abc"; } catch (e) { threw = true; }
        return [
            Object.prototype.toString.call(new r(1,2,3,4)),                    // [object DOMPoint]
            Object.prototype.toString.call(r.fromPoint({x:1,y:2,z:3,w:4})),    // [object DOMPoint]
            Object.getPrototypeOf(r.prototype) === rro.prototype,              // proto chain: true
            Object.getPrototypeOf(r) === rro,                                  // ctor chain: true (the fix)
            Object.getPrototypeOf(rro.prototype) === Object.prototype,         // rro parent: true
            r.fromPoint.length, rro.fromPoint.length,                          // 0, 0
            typeof q.x + ":" + q.x,                                            // number:5 (coercion kept)
            threw,                                                             // false
            Number.isNaN(b.z)                                                  // true
        ].join("|");
    })()"#;
    let got = e.eval_value("dp-rest", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "[object DOMPoint]|[object DOMPoint]|true|true|true|0|0|number:5|false|true",
        "DOMPoint chains/tag/statics/coercion != expected (ctor chain must be the reconcile's fix)"
    );
}
