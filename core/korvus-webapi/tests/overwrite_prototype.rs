//! `installer::overwrite_prototype` (W5.0b contract item 1): a hand table installed onto the prototype
//! of an EXISTING constructor -- a JS class the runtime still provides (`Event`, `EventTarget` until
//! W4.b) -- makes that prototype the table's: the recorded member order and descriptors, the class
//! body's extras gone, `constructor` last (Chrome's position on every interface prototype), the
//! statics on the constructor, `Symbol.toStringTag`, the constructor's `length` from the `ctor` row,
//! and one `Pass::Create` ledger entry per row (a WebIDL constant is two rows: `constant` on the
//! prototype, `static_constant` on the interface object -- the registry carries it the same way). This is the Rust form of what `dom/09g-event.js`
//! did by hand for `Event.prototype`.

use deno_core::{v8, RuntimeOptions};
use webapi::install::installer::{overwrite_prototype, Holder, InstallLedger, Pass};
use webapi::install::verify::describe_own;

webapi::interface! {
    "Zed" [interface, window] {
        ctor          "Zed"                         len 1 = stub,
        attr          "alpha" : DomString readonly        = stub,
        attr          "beta"  : Integer                   = stub,
        constant        "K_ONE" : Integer                 = 1,
        method          "go"    : Undefined       len 2   = stub,
        static_constant "K_ONE" : Integer                 = 1,
        static_method   "make"  : Object          len 0   = stub,
    }
}

fn eval<'s>(scope: &mut v8::PinScope<'s, '_>, src: &str) -> v8::Local<'s, v8::Value> {
    let code = v8::String::new(scope, src).unwrap();
    v8::Script::compile(scope, code, None).unwrap().run(scope).unwrap()
}

#[tokio::test]
async fn a_table_overwrites_an_existing_class_prototype_in_table_order() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);

    // The class the runtime "still provides": members in the wrong order, one the table does not
    // have (`zzz`), a data `beta` where the table wants an accessor, `length` 2 where WebIDL says 1.
    let ctor_v = eval(
        scope,
        r#"(() => {
            class Zed { constructor(a, b) {} go() {} static make() {} }
            Object.defineProperty(Zed.prototype, "zzz", { value: 1, configurable: true });
            Zed.prototype.beta = 5;
            globalThis.Zed = Zed;
            return Zed;
        })()"#,
    );
    let ctor = v8::Local::<v8::Function>::try_from(ctor_v).unwrap();
    let before = eval(scope, r#"Object.getOwnPropertyNames(Zed.prototype).join(",") + "|" + Zed.length"#).to_rust_string_lossy(scope);
    assert_eq!(before, "constructor,go,zzz,beta|2");

    let mut ledger = InstallLedger::default();
    let proto = overwrite_prototype(scope, ctor, &ZED, &mut ledger).expect("a plain class prototype takes the table");

    // Table order, table descriptors, constructor LAST, the class's extra member gone.
    assert_eq!(
        describe_own(scope, proto),
        vec![
            "alpha:accessor:e=1:c=1:s=0".to_string(),
            "beta:accessor:e=1:c=1:s=1".to_string(),
            "K_ONE:data:e=1:c=0:w=0".to_string(),
            "go:data:e=1:c=1:w=1".to_string(),
            "constructor:data:e=0:c=1:w=1".to_string(),
        ]
    );
    let after = eval(
        scope,
        r#"[
            Zed.prototype.constructor === Zed,
            Zed.length,
            Zed.prototype[Symbol.toStringTag],
            typeof Zed.make, Zed.make.length,
            Zed.prototype.K_ONE, Zed.K_ONE,
            "zzz" in Zed.prototype,
            Object.getOwnPropertyDescriptor(Zed.prototype, "go").value.length,
            Object.getOwnPropertySymbols(Zed.prototype).map(String).join(","),
        ].join("|")"#,
    )
    .to_rust_string_lossy(scope);
    assert_eq!(after, "true|1|Zed|function|0|1|1|false|2|Symbol(Symbol.toStringTag)");

    // One Create entry per row, on the holder the row names, plus the constructor.
    let got: Vec<(&str, &str, Holder, Pass)> = ledger.entries().iter().map(|e| (e.iface, e.member, e.holder, e.pass)).collect();
    assert_eq!(
        got,
        vec![
            ("Zed", "alpha", Holder::Prototype, Pass::Create),
            ("Zed", "beta", Holder::Prototype, Pass::Create),
            ("Zed", "K_ONE", Holder::Prototype, Pass::Create),
            ("Zed", "go", Holder::Prototype, Pass::Create),
            ("Zed", "K_ONE", Holder::InterfaceObject, Pass::Create),
            ("Zed", "make", Holder::InterfaceObject, Pass::Create),
            ("Zed", "constructor", Holder::Prototype, Pass::Create),
        ]
    );
}

#[tokio::test]
async fn a_sealed_constant_makes_a_second_overwrite_refuse_rather_than_half_reorder() {
    // WebIDL constants are non-configurable, so once a table has put `K_ONE` on the prototype a second
    // overwrite cannot delete it and cannot move it: the honest answer is an error naming the survivor,
    // not a prototype whose order is half the table's.
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let ctor_v = eval(scope, r#"(() => { class Zed { constructor(a, b) {} } globalThis.Zed = Zed; return Zed; })()"#);
    let ctor = v8::Local::<v8::Function>::try_from(ctor_v).unwrap();
    let mut ledger = InstallLedger::default();
    let p1 = overwrite_prototype(scope, ctor, &ZED, &mut ledger).unwrap();
    let first = describe_own(scope, p1);
    assert_eq!(first.last().map(String::as_str), Some("constructor:data:e=0:c=1:w=1"));
    let err = overwrite_prototype(scope, ctor, &ZED, &mut ledger).unwrap_err();
    assert!(err.contains("K_ONE"), "{err}");
    // The constant is the only survivor; a table WITHOUT constants overwrites again and again.
    let e2 = eval(scope, r#"Object.getOwnPropertyNames(Zed.prototype).join(",")"#).to_rust_string_lossy(scope);
    assert_eq!(e2, "K_ONE,constructor");
}
