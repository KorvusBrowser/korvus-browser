//! The one-table installer, proven end to end on a synthetic interface.
//!
//! No real interface and no engine surface is touched: a bare `JsRuntime` gets a scope, a synthetic
//! `InterfaceDef` of three stub members is installed onto a prototype, and the prototype is read back
//! from JS. This validates the descriptor -> installer -> D2 stub path (the whole skeleton) before
//! the `interface!` macro and the DOMRect migration wire it to a real prototype.

use deno_core::{v8, RuntimeOptions};
use webapi::install::installer::install_members;
use webapi::install::{Exposed, InterfaceDef, MemberDef, MemberKind, Model, TypeTag};

/// A stub member's `InstallFn` is never called (the installer's stub path handles it), so a synthetic
/// table can point every row here.
fn unused_install(_: &mut v8::PinScope, _: v8::Local<v8::Object>, _: &'static MemberDef) {
    unreachable!("stub members are mounted by the installer's stub path, not their InstallFn");
}

static MEMBERS: [MemberDef; 3] = [
    // readonly numeric attribute -> getter returns 0
    MemberDef { name: "n", label: "T::n", kind: MemberKind::Attr { readonly: true }, ty: TypeTag::Integer, length: 0, body: webapi::install::BodyKind::Stub, install: unused_install, cval: webapi::install::ConstVal::None, static_member: false, exposed: webapi::install::Exposed::Both, enumerable: true, configurable: true, nullable: false, required: 0, lenient_this: false, cfg: "", exposure: webapi::install::Exposure::Unconditional },
    // void method -> returns undefined
    MemberDef { name: "m", label: "T::m", kind: MemberKind::Method, ty: TypeTag::Undefined, length: 0, body: webapi::install::BodyKind::Stub, install: unused_install, cval: webapi::install::ConstVal::None, static_member: false, exposed: webapi::install::Exposed::Both, enumerable: true, configurable: true, nullable: false, required: 0, lenient_this: false, cfg: "", exposure: webapi::install::Exposure::Unconditional },
    // writable string attribute -> getter "" ; setter swallows the write
    MemberDef { name: "s", label: "T::s", kind: MemberKind::Attr { readonly: false }, ty: TypeTag::DomString, length: 0, body: webapi::install::BodyKind::Stub, install: unused_install, cval: webapi::install::ConstVal::None, static_member: false, exposed: webapi::install::Exposed::Both, enumerable: true, configurable: true, nullable: false, required: 0, lenient_this: false, cfg: "", exposure: webapi::install::Exposure::Unconditional },
];
static DEF: InterfaceDef = InterfaceDef {
    name: "T",
    parent: None,
    model: Model::Singleton,
    exposed: Exposed::Window,
    ctor: None,
    members: &MEMBERS,
    tag: Some("T"),
    backfill: false, slot_backed: false, aliases: &[], exposure: webapi::install::Exposure::Unconditional,};

#[test]
fn installer_mounts_stub_members_with_the_right_shape() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);

    let proto = v8::Object::new(scope);
    // Put the prototype on the global as `T` so a script can read it back.
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, "T").unwrap();
    global.set(scope, key.into(), proto.into());

    // Select all three, in order -- the join the installer does against a bundle selector.
    let gaps = install_members(scope, proto, &DEF, &["n", "m", "s"]);
    assert!(gaps.is_empty(), "no gaps for a table that carries every selected member");

    // Read the shape back from JS: numeric readonly -> 0, void method -> undefined, writable string
    // -> "" and a write is swallowed (the stub setter has nowhere to store it yet).
    let src = v8::String::new(
        scope,
        r#"(() => {
            // READ THROUGH AN INSTANCE, not off the prototype. A table's `= stub` row carries the
            // receiver check the registry's always had (`installer::put_d2_stub`): the
            // prototype is NOT an instance of its own interface, and reading a member off it is what
            // every browser answers `TypeError: Illegal invocation` for. `Object.create` gives the
            // shape gate the receiver a real call would have.
            const t = Object.create(T);
            const before = String(t.n) + "|" + (typeof t.m) + "|" + String(t.m()) + "|" + JSON.stringify(t.s);
            t.s = "written";                 // stub setter swallows
            const desc = Object.getOwnPropertyDescriptor(T, "n");
            return before + "|after-set:" + JSON.stringify(t.s)
                 + "|enum:" + desc.enumerable + "|conf:" + desc.configurable
                 + "|mnative:" + (Function.prototype.toString.call(t.m).includes("[native code]"));
        })()"#,
    ).unwrap();
    let script = v8::Script::compile(scope, src, None).expect("compile");
    let out = script.run(scope).expect("run").to_rust_string_lossy(scope);

    assert_eq!(
        out,
        "0|function|undefined|\"\"|after-set:\"\"|enum:true|conf:true|mnative:true",
        "the installed stub surface must match the D2 shape contract",
    );
}

#[test]
fn describe_own_reports_the_installed_descriptor_shapes() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);

    let proto = v8::Object::new(scope);
    let gaps = install_members(scope, proto, &DEF, &["n", "m", "s"]);
    assert!(gaps.is_empty());

    // Order is insertion order (n, m, s). A readonly attr is a getter-only accessor; a method is a
    // writable data property; a writable attr is a get/set accessor. This is the shape a cutover's
    // byte-identical gate compares.
    let desc = webapi::install::verify::describe_own(scope, proto);
    assert_eq!(
        desc,
        vec![
            "n:accessor:e=1:c=1:s=0".to_string(),
            "m:data:e=1:c=1:w=1".to_string(),
            "s:accessor:e=1:c=1:s=1".to_string(),
        ],
    );
}

#[test]
fn install_singleton_builds_a_constructorless_interface_object() {
    use webapi::install::installer::install_singleton;
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);

    let (_proto, gaps) = install_singleton(scope, global, &DEF, "T", &["n", "m", "s"]);
    assert!(gaps.is_empty());

    let src = v8::String::new(
        scope,
        r#"(() => {
            let threw = false;
            try { new T(); } catch (e) { threw = e instanceof TypeError; }
            const names = Object.getOwnPropertyNames(T.prototype);
            const inst = Object.create(T.prototype);
            const cdesc = Object.getOwnPropertyDescriptor(T.prototype, "constructor");
            return [typeof T, threw, Object.prototype.toString.call(inst),
                    names.join(","), "ctorEnum:" + cdesc.enumerable,
                    "protoW:" + Object.getOwnPropertyDescriptor(T, "prototype").writable].join("|");
        })()"#,
    ).unwrap();
    let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    // constructor-less function, `new` throws TypeError, toStringTag "T", members in install order
    // then a non-enumerable constructor, prototype non-writable.
    assert_eq!(out, "function|true|[object T]|n,m,s,constructor|ctorEnum:false|protoW:false");
}
