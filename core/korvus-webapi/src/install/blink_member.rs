//! `install/blink_member.rs` -- the `blink_member` kit: a registry row whose whole behaviour is ONE
//! fact of Blink's -- an attribute's initial value and its setter's IDL conversion, or a method's
//! precondition refusal -- written in `tools/sync/data/blink_members.json` with the Blink source it
//! comes from, and claimed onto the row by `gen_shells` (the Chrome oracle
//! ratchet).
//!
//! These rows were D2 stubs answering their shape default: `new SpeechRecognition().maxAlternatives`
//! was `undefined` where Chrome's constructor sets 1, `new AudioDecoder(...).flush()` resolved where
//! Chrome rejects "Cannot call 'flush' on an unconfigured codec.", `PressureObserver.knownSources` was
//! `undefined` where Chrome lists `["cpu"]`. A row whose answer needs real state (a list, a URL, a
//! focus check) is NOT this kit's: it has its own body.
//!
//! The row's `cfg` is the JSON entry, verbatim:
//!
//! ```text
//! {"value": <json>, "set": "<idl type>"}   attribute: the initial value; a writable one converts by
//!                                          `set` ("DOMString", "boolean", "unsigned long",
//!                                          "enum:a,b,c", "iface:Name") and stores per instance
//! {"array": [...], "frozen": bool}        attribute: an array, [SameObject] per instance (per
//!                                          interface object for a static)
//! {"new": "Iface"}                         attribute: an instance of Iface, [SameObject]
//! {"throw": ["Name", "message"]}           method: throws (a DOMException, or a TypeError)
//! {"reject": ["Name", "message"]}          method: returns a rejected promise
//! {"resolve": <json>}                      method: returns a promise resolved with the value
//! {"return": <json>}                       method: returns the value (`null` means `undefined`)
//! ```
//!
//! A method entry may add `"required": n`, its WebIDL required-argument count.

use deno_core::v8;

use crate::binding::protos::{body_data, build_fn_data, make_dom_exc, str_val, throw_dom, throw_type_error};
use crate::install::kits::{define_accessor, define_value};
use crate::install::{MemberDef, MemberKind};

fn entry(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments) -> (String, String, serde_json::Value) {
    let data = body_data(scope, args).to_rust_string_lossy(scope);
    // data = "<Iface>\0<member>\0<json>"
    let mut it = data.splitn(3, '\u{0}');
    let iface = it.next().unwrap_or("").to_string();
    let member = it.next().unwrap_or("").to_string();
    let json = serde_json::from_str(it.next().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
    (iface, member, json)
}

fn to_v8<'s>(scope: &mut v8::PinScope<'s, '_>, v: &serde_json::Value) -> v8::Local<'s, v8::Value> {
    match v {
        serde_json::Value::Null => v8::null(scope).into(),
        serde_json::Value::Bool(b) => v8::Boolean::new(scope, *b).into(),
        serde_json::Value::Number(n) => v8::Number::new(scope, n.as_f64().unwrap_or(0.0)).into(),
        serde_json::Value::String(s) => str_val(scope, s),
        serde_json::Value::Array(a) => {
            let items: Vec<v8::Local<v8::Value>> = a.iter().map(|x| to_v8(scope, x)).collect();
            v8::Array::new_with_elements(scope, &items).into()
        }
        serde_json::Value::Object(_) => v8::Object::new(scope).into(),
    }
}

fn slot_key<'s>(scope: &mut v8::PinScope<'s, '_>, member: &str) -> v8::Local<'s, v8::Private> {
    let n = v8::String::new(scope, &format!("korvus_bm_{member}")).unwrap();
    v8::Private::for_api(scope, Some(n))
}

/// The receiver check: an instance of the row's interface (any receiver for a static).
fn receiver_ok(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str, is_static: bool) -> bool {
    is_static || crate::install::webidl::has_brand(scope, this.into(), iface) || crate::binding::protos::implements(scope, this.into(), iface)
}

fn cb_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (iface, member, e) = entry(scope, &args);
    let is_static = e.get("static").and_then(|s| s.as_bool()).unwrap_or(false);
    let this = args.this();
    if !receiver_ok(scope, this, &iface, is_static) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let key = slot_key(scope, &member);
    if let Some(v) = this.get_private(scope, key).filter(|v| !v.is_undefined()) {
        rv.set(v);
        return;
    }
    let v: v8::Local<v8::Value> = if let Some(a) = e.get("array") {
        let arr = to_v8(scope, a);
        if e.get("frozen").and_then(|f| f.as_bool()).unwrap_or(false) {
            if let Ok(o) = v8::Local::<v8::Object>::try_from(arr) {
                let _ = o.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            }
        }
        this.set_private(scope, key, arr);
        arr
    } else if let Some(n) = e.get("new").and_then(|n| n.as_str()) {
        let Some(o) = mint(scope, n) else { return };
        this.set_private(scope, key, o.into());
        o.into()
    } else {
        to_v8(scope, e.get("value").unwrap_or(&serde_json::Value::Null))
    };
    rv.set(v);
}

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let ctor = crate::binding::protos::iface_object(scope, iface).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let proto = crate::binding::protos::get_prop(scope, ctor, "prototype").and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    crate::binding::protos::brand_shell_instance(scope, o);
    crate::install::webidl::brand(scope, o, iface);
    Some(o)
}

fn cb_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let (iface, member, e) = entry(scope, &args);
    let this = args.this();
    if !receiver_ok(scope, this, &iface, false) {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let ty = e.get("set").and_then(|s| s.as_str()).unwrap_or("any").to_string();
    let v = args.get(0);
    let converted: v8::Local<v8::Value> = if ty == "DOMString" {
        match v.to_string(scope) {
            Some(s) => s.into(),
            None => return,
        }
    } else if ty == "boolean" {
        v8::Boolean::new(scope, v.boolean_value(scope)).into()
    } else if ty == "unsigned long" {
        let Some(n) = v.number_value(scope) else { return };
        let n = if n.is_finite() { (n.trunc() as i64).rem_euclid(1 << 32) as f64 } else { 0.0 };
        v8::Number::new(scope, n).into()
    } else if let Some(list) = ty.strip_prefix("enum:") {
        // An enumeration attribute IGNORES a value outside the enum (WebIDL).
        let Some(s) = v.to_string(scope) else { return };
        let text = s.to_rust_string_lossy(scope);
        if !list.split(',').any(|k| k == text) {
            return;
        }
        s.into()
    } else if let Some(want) = ty.strip_prefix("iface:") {
        if !crate::binding::protos::implements(scope, v, want) {
            throw_type_error(scope, &format!("Failed to set the '{member}' property on '{iface}': The provided value is not of type '{want}'."));
            return;
        }
        v
    } else {
        v
    };
    let key = slot_key(scope, &member);
    this.set_private(scope, key, converted);
}

fn cb_method(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let (iface, _member, e) = entry(scope, &args);
    let is_static = e.get("static").and_then(|s| s.as_bool()).unwrap_or(false);
    let this = args.this();
    let promise = e.get("reject").is_some() || e.get("resolve").is_some();
    if !receiver_ok(scope, this, &iface, is_static) {
        if promise {
            if let Some(p) = v8::PromiseResolver::new(scope) {
                let m = v8::String::new(scope, "Illegal invocation").unwrap();
                let x = v8::Exception::type_error(scope, m);
                p.reject(scope, x);
                rv.set(p.get_promise(scope).into());
            }
        } else {
            throw_type_error(scope, "Illegal invocation");
        }
        return;
    }
    let error_of = |scope: &mut v8::PinScope<'_, '_>, pair: &serde_json::Value| -> (String, String) {
        let name = pair.get(0).and_then(|x| x.as_str()).unwrap_or("Error").to_string();
        let msg = pair.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let _ = scope;
        (name, msg)
    };
    if let Some(pair) = e.get("throw") {
        let (name, msg) = error_of(scope, pair);
        if name == "TypeError" {
            throw_type_error(scope, &msg);
        } else {
            throw_dom(scope, &msg, &name);
        }
        return;
    }
    if let Some(pair) = e.get("reject") {
        let (name, msg) = error_of(scope, pair);
        let Some(p) = v8::PromiseResolver::new(scope) else { return };
        let x = if name == "TypeError" {
            // Blink's context applied after the error is made
            Some(deno_core::error::native_error(scope, "TypeError", &msg))
        } else {
            make_dom_exc(scope, &msg, &name)
        };
        if let Some(x) = x {
            p.reject(scope, x);
        }
        rv.set(p.get_promise(scope).into());
        return;
    }
    if let Some(v) = e.get("resolve") {
        let Some(p) = v8::PromiseResolver::new(scope) else { return };
        let x = to_v8(scope, v);
        p.resolve(scope, x);
        rv.set(p.get_promise(scope).into());
        return;
    }
    // `"return": null` is `undefined` (an `undefined`-returning operation); JSON has no undefined.
    if let Some(v) = e.get("return") {
        if v.is_null() {
            rv.set_undefined();
        } else {
            let x = to_v8(scope, v);
            rv.set(x);
        }
    }
}

/// The kit body: an accessor (getter, and the setter for a writable row) or a method.
pub fn install(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let iface = member.label.split("::").next().unwrap_or("");
    let is_static = matches!(member.kind, MemberKind::StaticAttr { .. } | MemberKind::StaticMethod);
    // The entry, with the static flag folded in for the bodies.
    let mut cfg: serde_json::Value = serde_json::from_str(member.cfg).unwrap_or(serde_json::Value::Null);
    if is_static {
        if let Some(o) = cfg.as_object_mut() {
            o.insert("static".to_string(), serde_json::Value::Bool(true));
        }
    }
    let data = format!("{iface}\u{0}{}\u{0}{cfg}", member.name);
    match member.kind {
        MemberKind::Attr { readonly } | MemberKind::StaticAttr { readonly } => {
            let d = str_val(scope, &data);
            let g = build_fn_data(scope, cb_get, &format!("get {}", member.name), 0, d);
            let s = if readonly {
                None
            } else {
                let d = str_val(scope, &data);
                Some(build_fn_data(scope, cb_set, &format!("set {}", member.name), 1, d))
            };
            define_accessor(scope, proto, member, g, s);
        }
        _ => {
            let d = str_val(scope, &data);
            let f = build_fn_data(scope, cb_method, member.name, member.length as i32, d);
            // `"required": n` in the entry: the operation's own required-argument count where the
            // registry row carries none (the capture records `length`, not `required`).
            let required = cfg.get("required").and_then(|r| r.as_u64()).map(|r| r as u32).unwrap_or(0).max(member.required as u32);
            let v: v8::Local<v8::Value> = if required > 0 {
                // The guard rejects for a promise-returning operation and throws otherwise. The ENTRY says
                // which this one is: a `reject` / `resolve` entry returns a promise; a `return` / `throw`
                // entry does not, whatever webref types it (`MediaSession.setMicrophoneActive` is
                // `Promise<undefined>` there and returns `undefined` in Chrome 150, measured).
                let promise = cfg.get("reject").is_some() || cfg.get("resolve").is_some();
                let ty = if promise { member.ty } else { crate::install::TypeTag::Any };
                crate::binding::protos::make_arity_guard(scope, iface, member.name, required as i32, member.length as i32, f.into(), ty).into()
            } else {
                f.into()
            };
            define_value(scope, proto, member, v, true);
        }
    }
}
