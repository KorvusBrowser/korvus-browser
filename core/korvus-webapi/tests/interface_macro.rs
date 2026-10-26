//! The `interface!` macro: a table expands to an `InterfaceDef`, and that descriptor drives the
//! installer end to end (docs/PHASE-3-DESIGN.md §2-4). Synthetic interface, no engine surface.

use deno_core::{v8, RuntimeOptions};
use webapi::binding::protos::build_fn;
use webapi::install::installer::{install_constructable, install_members};
use webapi::install::MemberDef;

// A real (non-stub) member body: an `InstallFn` that mounts a method returning `true`. Proves the
// installer's non-stub path (it calls `member.install`), distinct from the D2 stub path.
fn widget_real(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, m: &'static MemberDef) {
    fn cb(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
        let _ = scope;
        rv.set_bool(true);
    }
    let f = build_fn(scope, cb, m.name, m.length as i32);
    if let Some(k) = v8::String::new(scope, m.name) { proto.set(scope, k.into(), f.into()); }
}

// One table -> one `InterfaceDef`. `n` readonly int stub, `m` void method stub, `s` writable string
// stub, `real` a genuine method. The macro emits `pub static WIDGET`.
webapi::interface! {
    "Widget" : "EventTarget" [singleton, window] {
        attr    "n"    : Integer   readonly = stub,
        method  "m"    : Undefined          = stub,
        attr    "s"    : DomString          = stub,
        method  "real" : Boolean            = widget_real,
    }
}

// PER-MEMBER exposure. The table is `both`; `winOnly` says `window` and `workOnly` says
// `worker`, so one table describes what the two captures record for an interface whose member set
// differs by realm. `ServiceWorkerRegistration` is the real case -- `[Exposed=(Window,Worker)]` with
// a window-only `cookies` -- and before this word a hand table could not express it at all.
webapi::interface! {
    "Gadget" [interface, both] {
        attr   "shared"  : Integer readonly        = stub,
        attr   "winOnly" : Integer readonly window = stub,
        attr   "workOnly": Integer readonly worker = stub,
        method "bothM"   : Undefined         both  = stub,
    }
}

#[test]
fn a_row_may_state_its_own_exposure() {
    use webapi::install::Exposed;
    assert_eq!(GADGET.exposed, Exposed::Both);
    // Omitted -> the table's.
    assert_eq!(GADGET.member("shared").unwrap().exposed, Exposed::Both);
    assert_eq!(GADGET.member("winOnly").unwrap().exposed, Exposed::Window);
    assert_eq!(GADGET.member("workOnly").unwrap().exposed, Exposed::Worker);
    // Stated explicitly, and the same as the table's -- still legal, and still `Both`.
    assert_eq!(GADGET.member("bothM").unwrap().exposed, Exposed::Both);
    // The word does not disturb the rest of the row.
    assert!(matches!(GADGET.member("winOnly").unwrap().kind, webapi::install::MemberKind::Attr { readonly: true }));
    assert_eq!(GADGET.member("winOnly").unwrap().label, "Gadget::winOnly");
}

#[test]
fn the_table_expands_to_the_right_descriptor() {
    assert_eq!(WIDGET.name, "Widget");
    assert_eq!(WIDGET.parent, Some("EventTarget"));
    assert_eq!(WIDGET.members.len(), 4);
    // shape halves came from the table
    assert_eq!(WIDGET.member("n").unwrap().body, webapi::install::BodyKind::Stub);
    assert_eq!(WIDGET.member("n").unwrap().label, "Widget::n");
    assert_eq!(WIDGET.member("real").unwrap().body, webapi::install::BodyKind::Install);
    assert!(matches!(WIDGET.member("n").unwrap().kind, webapi::install::MemberKind::Attr { readonly: true }));
    assert!(matches!(WIDGET.member("s").unwrap().kind, webapi::install::MemberKind::Attr { readonly: false }));
}

#[test]
fn the_descriptor_installs_stub_and_real_members() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);

    let proto = v8::Object::new(scope);
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, "W").unwrap();
    global.set(scope, key.into(), proto.into());

    let gaps = install_members(scope, proto, &WIDGET, &["n", "m", "s", "real"]);
    assert!(gaps.is_empty());

    let src = v8::String::new(
        scope,
        // Through an INSTANCE: a table's stub rows check the receiver now, and a prototype is not an
        // instance of its own interface (`Illegal invocation` is what a browser answers).
        r#"(() => { const w = Object.create(W);
             return String(w.n) + "|" + String(w.m()) + "|" + JSON.stringify(w.s) + "|" + String(w.real()); })()"#,
    ).unwrap();
    let script = v8::Script::compile(scope, src, None).unwrap();
    let out = script.run(scope).unwrap().to_rust_string_lossy(scope);
    // stub int -> 0, stub void -> undefined, stub string -> "", real method -> true.
    assert_eq!(out, "0|undefined|\"\"|true");
}

// A ctor row -> InterfaceDef.ctor (Some), not a prototype member.
webapi::interface! {
    "Rectish" : "EventTarget" [value, window] {
        ctor   "Rectish" len 0 = stub,
        attr   "w" : Double = stub,
        static_method "of" : Object len 1 = stub,
    }
}

#[test]
fn a_ctor_row_becomes_the_interface_ctor_not_a_member() {
    // ctor pulled out of members
    assert!(RECTISH.ctor.is_some());
    let c = RECTISH.ctor.as_ref().unwrap();
    assert_eq!(c.name, "Rectish");
    assert_eq!(c.label, "Rectish::Rectish");
    assert_eq!(c.length, 0);
    assert!(c.is_stub);
    // members = the non-ctor rows only (w, of)
    assert_eq!(RECTISH.members.len(), 2);
    assert!(RECTISH.member("w").is_some());
    assert!(RECTISH.member("of").is_some());
    assert!(RECTISH.member("Rectish").is_none());
    assert!(matches!(RECTISH.member("of").unwrap().kind, webapi::install::MemberKind::StaticMethod));
}

// F1b: const VALUES (a/m/v coverage) + static_attr grammar. A const row's RHS is its value; the
// installer mounts it with the WebIDL constant descriptor. A static_attr is a StaticAttr accessor.
webapi::interface! {
    "Consty" [value, window] {
        ctor        "Consty"          len 0 = stub,
        constant    "ANSWER" : Integer      = 42,
        constant    "READY"  : Boolean      = true,
        static_attr "flag"   : Boolean readonly = stub,
    }
}

#[test]
fn const_values_and_static_attr_expand_and_install() {
    // shape: const kind + carried value; static_attr kind + readonly.
    assert_eq!(CONSTY.member("ANSWER").unwrap().body, webapi::install::BodyKind::Stub);
    assert!(matches!(CONSTY.member("ANSWER").unwrap().kind, webapi::install::MemberKind::Const));
    assert!(matches!(CONSTY.member("ANSWER").unwrap().cval, webapi::install::ConstVal::Num(n) if n == 42.0));
    assert!(matches!(CONSTY.member("READY").unwrap().cval, webapi::install::ConstVal::Bool(true)));
    assert!(matches!(CONSTY.member("flag").unwrap().kind, webapi::install::MemberKind::StaticAttr { readonly: true }));

    // install: const on the prototype with { value, e=1, c=0, w=0 }; static_attr as a getter on the ctor.
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);
    let (_proto, _ctor, gaps) =
        install_constructable(scope, global, &CONSTY, "Consty", &["ANSWER", "READY"], &["flag"], None);
    assert!(gaps.is_empty());

    let src = v8::String::new(scope, r#"(() => {
        const d = Object.getOwnPropertyDescriptor(Consty.prototype, "ANSWER");
        const s = Object.getOwnPropertyDescriptor(Consty, "flag");
        return [Consty.prototype.ANSWER, Consty.prototype.READY,
                +d.enumerable, +d.configurable, +d.writable, typeof s.get].join("|");
    })()"#).unwrap();
    let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(out, "42|true|1|0|0|function", "const value/descriptor or static_attr getter != expected");
}

// F1b: the remaining F1 kinds. named/indexed getters (instance interceptors — declared in the table,
// no-op on the prototype), and iterable/maplike/setlike expressed as their concrete method rows
// (entries/keys/values/forEach — how the capture records them, so F2 emits them as `method`).
webapi::interface! {
    "Collectiony" [value, window] {
        ctor           "Collectiony"          len 0 = stub,
        attr           "length" : Integer readonly  = stub,
        method         "item"   : Object     len 1   = stub,
        indexed_getter "__indexed__" : Object        = stub,
        named_getter   "__named__"   : Object        = stub,
        method         "entries" : Object    len 0   = stub,
        method         "keys"    : Object    len 0   = stub,
        method         "values"  : Object    len 0   = stub,
        method         "forEach" : Undefined len 1   = stub,
    }
}

#[test]
fn expresses_interceptor_kinds_and_iterable_methods() {
    use webapi::install::MemberKind;
    assert!(matches!(COLLECTIONY.member("__indexed__").unwrap().kind, MemberKind::IndexedGetter));
    assert!(matches!(COLLECTIONY.member("__named__").unwrap().kind, MemberKind::NamedGetter));
    for m in ["entries", "keys", "values", "forEach"] {
        assert!(matches!(COLLECTIONY.member(m).unwrap().kind, MemberKind::Method), "{m} should be a method");
    }

    // install: methods + length mount; the interceptor rows no-op on the prototype (mounted at
    // instance-template time in the live phase), so no Gap and they do NOT appear on the prototype.
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);
    let selected = ["length", "item", "__indexed__", "__named__", "entries", "keys", "values", "forEach"];
    let (_proto, _ctor, gaps) =
        install_constructable(scope, global, &COLLECTIONY, "Collectiony", &selected, &[], None);
    assert!(gaps.is_empty(), "no gaps: every selected member is in the table");

    let src = v8::String::new(scope,
        r#"Object.getOwnPropertyNames(Collectiony.prototype).join(",")"#).unwrap();
    let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(out, "length,item,entries,keys,values,forEach,constructor",
        "interceptor rows must NOT appear as prototype members");
}

// F3-prep: a non-constructible interface (`[interface]` model) exposes a prototype but `new X()`
// throws "Illegal constructor" — the shape of most WebIDL interfaces (the generated registry marks
// every non-CONSTRUCTIBLE interface this way).
webapi::interface! {
    "Ghosty" [interface, window] {
        ctor   "Ghosty"          len 0 = stub,
        attr   "kind" : DomString readonly = stub,
    }
}

#[test]
fn interface_model_throws_illegal_constructor() {
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let ctx = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), ctx);
    let global = scope.get_current_context().global(scope);
    let (_p, _c, gaps) = install_constructable(scope, global, &GHOSTY, "Ghosty", &["kind"], &[], None);
    assert!(gaps.is_empty());
    // EXACT, and BOTH SHAPES. This used to read `e.message.includes("Illegal")`, which
    // is true of Chrome's message and of the one this engine threw instead, so it could not tell them
    // apart. Chrome 150 over loopback, on every interface WebIDL gives no constructor:
    //   new X()  ->  TypeError: Failed to construct 'X': Illegal constructor
    //   X()      ->  TypeError: Illegal constructor
    let src = v8::String::new(scope, r#"(() => {
        const t = (f) => { try { f(); return "no-throw"; } catch (e) { return e.constructor.name + ": " + e.message; } };
        return t(() => new Ghosty()) + " | " + t(() => Ghosty());
    })()"#).unwrap();
    let out = v8::Script::compile(scope, src, None).unwrap().run(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(
        out,
        "TypeError: Failed to construct 'Ghosty': Illegal constructor | TypeError: Illegal constructor",
        "a [interface]-model interface object must answer Chrome's two messages"
    );
}
