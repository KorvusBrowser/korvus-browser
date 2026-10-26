//! The exotic interceptor kind, proven end to end.
//!
//! The one-table skeleton must reach past plain prototype accessors to the interceptor-backed kinds
//! (named/indexed getters -- DOMStringMap `dataset`, HTMLCollection named access), which only a native
//! `v8::ObjectTemplate` can carry. A synthetic `ExoticMap` table with a `NamedGetter` row + a normal
//! `item` method is installed via `install_named_getter_object`; the test proves the declared member
//! still mounts, an arbitrary property read routes through the interceptor (and, being a D2 stub with
//! no store, reads `undefined` rather than a fabricated value), and the interceptor's firing is
//! recorded in the same coverage registry the corpus reads.

use deno_core::{v8, RuntimeOptions};
use webapi::install::installer::install_named_getter_object;
use webapi::install::{Exposed, InterfaceDef, MemberDef, MemberKind, Model, TypeTag};

fn unused(_: &mut v8::PinScope, _: v8::Local<v8::Object>, _: &'static MemberDef) {
    unreachable!("stub members are mounted by the installer's stub path");
}

static MEMBERS: [MemberDef; 2] = [
    // The interceptor row itself -- not a prototype member; install_named_getter_object skips it.
    MemberDef { name: "__named__", label: "ExoticMap::named", kind: MemberKind::NamedGetter, ty: TypeTag::Any, length: 0, body: webapi::install::BodyKind::Stub, install: unused, cval: webapi::install::ConstVal::None, static_member: false, exposed: webapi::install::Exposed::Both, enumerable: true, configurable: true, nullable: false, required: 0, lenient_this: false, cfg: "", exposure: webapi::install::Exposure::Unconditional },
    // A normal declared method, mounted on the instance as a D2 stub (Object -> null).
    MemberDef { name: "item", label: "ExoticMap::item", kind: MemberKind::Method, ty: TypeTag::Object, length: 1, body: webapi::install::BodyKind::Stub, install: unused, cval: webapi::install::ConstVal::None, static_member: false, exposed: webapi::install::Exposed::Both, enumerable: true, configurable: true, nullable: false, required: 0, lenient_this: false, cfg: "", exposure: webapi::install::Exposure::Unconditional },
];
static DEF: InterfaceDef = InterfaceDef {
    name: "ExoticMap",
    parent: None,
    model: Model::Value,
    exposed: Exposed::Window,
    ctor: None,
    members: &MEMBERS,
    tag: Some("ExoticMap"),
    backfill: false, slot_backed: false, aliases: &[], exposure: webapi::install::Exposure::Unconditional,};

#[test]
fn a_named_getter_interface_installs_a_live_interceptor() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);

    let (obj, gaps) = install_named_getter_object(scope, &DEF, &["__named__", "item"]);
    assert!(gaps.is_empty(), "the declared `item` member is in the table");
    let g = scope.get_current_context().global(scope);
    let k = v8::String::new(scope, "m").unwrap();
    g.set(scope, k.into(), obj.into());

    let src = v8::String::new(
        scope,
        r#"[
            typeof m.item,                       // function -- the declared member mounted
            String(m.item()),                    // null -- D2 stub for an Object-typed method
            String(m.anArbitraryNameNotPresent), // undefined -- interceptor is a store-less D2 stub
            ("anArbitraryNameNotPresent" in m)   // false -- no query interceptor, so nothing is claimed
        ].join("|")"#,
    ).unwrap();
    let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(out, "function|null|undefined|false", "declared member + transparent D2 named interceptor");

    // The interceptor actually fired -- recorded into the coverage registry the corpus reads.
    let cov = webapi::behavior_rt::coverage_json();
    assert!(cov.contains("\"NamedGetter::stub\""), "the named-getter interceptor's firing must be recorded:\n{cov}");
}
