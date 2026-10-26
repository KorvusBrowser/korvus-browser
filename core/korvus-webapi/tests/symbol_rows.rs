//! A table row can name a well-known symbol (`@@iterator`), and the `array_proto_fn` kit mounts the
//! SAME function object as `Array.prototype`'s (W5.0 prep): WebIDL gives an interface with an indexed
//! property getter `Array.prototype`'s value iterator, and an iterable declaration its
//! `entries`/`keys`/`values`/`forEach` -- Chrome 151: `NodeList.prototype[Symbol.iterator] ===
//! Array.prototype.values`, non-enumerable, writable, configurable; `NodeList.prototype.forEach ===
//! Array.prototype.forEach`, enumerable.

use deno_core::{v8, RuntimeOptions};
use webapi::install::installer::install_constructable;

webapi::interface! {
    "Listy" [interface, window] {
        method "entries" : Object len 0 = array_proto_fn,
        method "forEach" : Undefined len 1 = array_proto_fn,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn,
        method "item" : Object len 1 = stub,
    }
}

#[tokio::test]
async fn a_symbol_keyed_row_mounts_the_well_known_symbol() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);
    let (_p, _c, gaps) = install_constructable(scope, global, &LISTY, "Listy", &["entries", "forEach", "@@iterator", "item"], &[], None);
    assert!(gaps.is_empty());
    let src = v8::String::new(
        scope,
        r#"(() => {
            const P = Listy.prototype;
            const d = Object.getOwnPropertyDescriptor(P, Symbol.iterator);
            return [
                P[Symbol.iterator] === Array.prototype.values,
                P.entries === Array.prototype.entries,
                P.forEach === Array.prototype.forEach,
                d.writable, d.enumerable, d.configurable,
                Object.getOwnPropertyNames(P).join(","),
                Object.getOwnPropertySymbols(P).map(String).join(","),
                typeof P.item,
            ].join("|");
        })()"#,
    )
    .unwrap();
    let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(out, "true|true|true|true|false|true|entries,forEach,item,constructor|Symbol(Symbol.iterator),Symbol(Symbol.toStringTag)|function");
}
