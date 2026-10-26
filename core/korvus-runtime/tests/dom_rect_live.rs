//! LIVE one-table gate for the DOMRect family.
//!
//! Boots the real engine -- whose context creation now reconciles the natively-realised DOMRect
//! family to its `interface!` table (parallel to the shell) -- and asserts the running interface is
//! byte-identical to the captured Brave surface: prototype members + descriptors, statics, the tag,
//! `fromRect.length`, AND both inheritance chains. The constructor chain (`DOMRect.__proto__ ===
//! DOMRectReadOnly`) is the one the reconcile FIXES: op2 leaves it at Function.prototype, a
//! divergence from every browser, and this proves the live engine now matches. Setter coercion
//! (`r.x = "5"` -> number) must still work -- the reconcile touches shape, never bodies.

use engine::Engine;

async fn page() -> Engine {
    let mut e = Engine::new(Box::new(persona::CapturedPersona::default()));
    e.run().await.unwrap();
    e.load_html("<!doctype html><html><body></body></html>").unwrap();
    e
}

/// Ordered `name:kind` describe lines the installer/reconcile must produce for an interface's own
/// prototype, from the surface fixture, plus the trailing non-enumerable `constructor`.
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
async fn live_dom_rect_family_is_byte_identical_to_the_capture() {
    let mut e = page().await;

    // describe_own of a prototype, JS side, in the same format as webapi::install::verify::describe_own.
    let describe = r#"const desc = (o) => Object.getOwnPropertyNames(o).map(n => {
        const d = Object.getOwnPropertyDescriptor(o, n);
        return (d.get || d.set)
            ? `${n}:accessor:e=${+d.enumerable}:c=${+d.configurable}:s=${+!!d.set}`
            : `${n}:data:e=${+d.enumerable}:c=${+d.configurable}:w=${+d.writable}`;
    }).join("\n");"#;

    for iface in ["DOMRectReadOnly", "DOMRect"] {
        let src = format!(r#"(() => {{ {describe} return desc({iface}.prototype); }})()"#);
        let got = e.eval_value("dr-desc", src).unwrap().as_str().unwrap().to_string();
        assert_eq!(got.lines().collect::<Vec<_>>(), expected_proto(iface), "{iface}.prototype != capture");
    }

    // The rest of the surface: chains (BOTH), tag, statics length, and coercion (a body behavior that
    // must survive a shape-only reconcile).
    let src = r#"(() => {
        const rro = DOMRectReadOnly, r = DOMRect;
        const q = new r(0,0,0,0); q.x = "5";
        // WebIDL `unrestricted double`: ToNumber, never a throw. `"abc"` is NaN, not a TypeError.
        // This comes from the ROW now (`attr "x" : Double` -- writable), not from an ESM pass.
        const b = new r(0,0,0,0); let threw = false;
        try { b.width = "abc"; } catch (e) { threw = true; }
        return [
            Object.prototype.toString.call(new r(1,2,3,4)),                 // [object DOMRect]
            Object.prototype.toString.call(r.fromRect({x:1,y:2,width:3,height:4})), // [object DOMRect]
            Object.getPrototypeOf(r.prototype) === rro.prototype,           // proto chain: true
            Object.getPrototypeOf(r) === rro,                               // ctor chain: true (the fix)
            Object.getPrototypeOf(rro.prototype) === Object.prototype,      // rro parent: true
            r.fromRect.length, rro.fromRect.length,                         // 0, 0
            typeof q.x + ":" + q.x,                                         // number:5 (coercion kept)
            threw,                                                          // false -- WebIDL never throws here
            Number.isNaN(b.width)                                           // true -- ToNumber("abc") is NaN
        ].join("|");
    })()"#;
    let got = e.eval_value("dr-rest", src.to_string()).unwrap().as_str().unwrap().to_string();
    assert_eq!(
        got,
        "[object DOMRect]|[object DOMRect]|true|true|true|0|0|number:5|false|true",
        "DOMRect chains/tag/statics/coercion != expected (ctor chain must be the reconcile's fix)"
    );
}
