//! The feasibility spike.
//!
//! A scratch test, never production code. It builds a small interface family out of V8 templates --
//! once per isolate -- instantiates it in two realms of one isolate (and in a second isolate, as a
//! worker would), and checks every property the TemplateBuilder must reproduce: key order, descriptor
//! flags, function names / lengths / native-ness, Signature receiver checks and promise rejection,
//! arity messages, constants, constructors (Illegal, `new.target`, a legacy factory function),
//! inheritance and a spliced named-properties object, well-known symbols, interceptors, instance
//! templates that deno_core's cppgc can wrap, per-realm singletons, persona omission, cross-realm
//! calls, realm-independent template data, lazy data properties on the global, and adoption of a
//! deno_core `#[op2]` class's per-realm constructor through its own template.
//!
//! Each check prints `item N  ok|FAIL  detail`; the test fails when any item fails. Two V8 methods that
//! rusty_v8 150 does not wrap are linked from the static V8 archive the workspace already builds
//! (precedent: `src/binding/v8_legacy.rs`).

use deno_core::{op2, v8, GarbageCollected, RuntimeOptions};
use std::cell::RefCell;
use std::ffi::CStr;

extern "C" {
    #[link_name = "_ZN2v816FunctionTemplate11HasInstanceENS_5LocalINS_5ValueEEE"]
    fn v8_function_template_has_instance(this: *const v8::FunctionTemplate, value: *const v8::Value) -> bool;
    #[link_name = "_ZN2v816FunctionTemplate28SetPrototypeProviderTemplateENS_5LocalIS0_EE"]
    fn v8_function_template_set_prototype_provider_template(
        this: *const v8::FunctionTemplate,
        provider: *const v8::FunctionTemplate,
    );
}

// ── a test-only deno_core cppgc class: the stand-in for DOMMatrix & co (items 11, 30) ─────────────

pub struct SpikeBox {
    v: f64,
}

unsafe impl GarbageCollected for SpikeBox {
    fn trace(&self, _v: &mut v8::cppgc::Visitor) {}
    fn get_name(&self) -> &'static CStr {
        c"SpikeBox"
    }
}

#[op2]
impl SpikeBox {
    #[constructor]
    #[cppgc]
    fn new(v: f64) -> SpikeBox {
        SpikeBox { v }
    }

    #[getter]
    fn v(&self) -> f64 {
        self.v
    }
}

deno_core::extension!(spike_ext, objects = [SpikeBox]);

// ── the templates, built once per isolate ───────────────────────────────────────────────────────

struct Templates {
    base: v8::Global<v8::FunctionTemplate>,
    node: v8::Global<v8::FunctionTemplate>,
    ctor: v8::Global<v8::FunctionTemplate>,
    list: v8::Global<v8::FunctionTemplate>,
    image: v8::Global<v8::FunctionTemplate>,
    image_fallback: v8::Global<v8::FunctionTemplate>,
    nav: v8::Global<v8::FunctionTemplate>,
    unscopables: v8::Global<v8::ObjectTemplate>,
}

thread_local! {
    static NODE_T: RefCell<Option<v8::Global<v8::FunctionTemplate>>> = const { RefCell::new(None) };
}

static DATA_VALUE: i32 = 42;

fn throw_type(scope: &mut v8::PinScope, msg: &str) {
    let m = v8::String::new(scope, msg).unwrap();
    let e = v8::Exception::type_error(scope, m);
    scope.throw_exception(e);
}

fn key<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::Name> {
    v8::String::new(scope, s).unwrap().into()
}

fn cb_illegal(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    throw_type(scope, "Failed to construct 'SpikeNode': Illegal constructor");
}

fn cb_node_type(_s: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_int32(1);
}

fn cb_text_get(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(v8::String::new(scope, "t").unwrap().into());
}

fn cb_noop(_s: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {}

fn cb_append(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if args.length() < 1 {
        throw_type(scope, "Failed to execute 'appendChild' on 'SpikeNode': 1 argument required, but only 0 present.");
        return;
    }
    rv.set(args.get(0));
}

fn has_node_instance(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let t = NODE_T.with(|t| t.borrow().clone()).expect("node template");
    let t = v8::Local::new(scope, t);
    unsafe { v8_function_template_has_instance(&*t, &*v) }
}

// promise-returning: no Signature; the callback's own check rejects instead of throwing (item 4)
fn cb_fetch_it(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this: v8::Local<v8::Value> = args.this().into();
    let ok = has_node_instance(scope, this);
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    if ok {
        let v = v8::undefined(scope);
        resolver.resolve(scope, v.into());
    } else {
        let m = v8::String::new(scope, "Failed to execute 'fetchIt' on 'SpikeNode': Illegal invocation").unwrap();
        let e = v8::Exception::type_error(scope, m);
        resolver.reject(scope, e);
    }
    rv.set(resolver.get_promise(scope).into());
}

fn cb_static(_s: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_int32(7);
}

fn cb_data(_s: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let ext = v8::Local::<v8::External>::try_from(args.data()).expect("External data");
    rv.set_int32(unsafe { *(ext.value() as *const i32) });
}

fn cb_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let nt = args.new_target();
    if nt.is_undefined() {
        throw_type(scope, "Failed to construct 'SpikeCtor': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
        return;
    }
    let this = args.this();
    let k = key(scope, "sawNewTarget");
    this.set(scope, k.into(), nt);
}

// [Replaceable]: the setter replaces the accessor with an own data property on the receiver
fn cb_replaceable_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let this = args.this();
    let k = key(scope, "rep");
    this.create_data_property(scope, k, args.get(0));
}

fn cb_idx_get<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) -> v8::Intercepted {
    if index < 3 {
        rv.set_int32((index * 10) as i32);
        return v8::Intercepted::kYes;
    }
    v8::Intercepted::kNo
}

fn cb_named_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) -> v8::Intercepted {
    let s = name.to_rust_string_lossy(scope);
    if s.starts_with('n') {
        let v = v8::String::new(scope, &format!("named:{s}")).unwrap();
        rv.set(v.into());
        return v8::Intercepted::kYes;
    }
    v8::Intercepted::kNo
}

// item 17: the global's lazy data property instantiates the interface in the realm being read
fn cb_lazy_global<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _name: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) {
    let t = NODE_T.with(|t| t.borrow().clone()).expect("node template");
    let t = v8::Local::new(scope, t);
    let f = t.get_function(scope).unwrap();
    rv.set(f.into());
}

fn method<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    length: i32,
    sig: Option<v8::Local<'s, v8::Signature>>,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let mut b = v8::FunctionTemplate::builder(cb).length(length).constructor_behavior(v8::ConstructorBehavior::Throw);
    if let Some(s) = sig {
        b = b.signature(s);
    }
    b.build(scope)
}

fn build_templates(scope: &mut v8::PinScope, omit: &[&str]) -> Templates {
    let ro_dd = || v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    let base = v8::FunctionTemplate::new(scope, cb_illegal);
    let n = v8::String::new(scope, "SpikeBase").unwrap();
    base.set_class_name(n);

    let node = v8::FunctionTemplate::builder(cb_illegal).length(0).build(scope);
    let n = v8::String::new(scope, "SpikeNode").unwrap();
    node.set_class_name(n);
    node.inherit(base);
    node.read_only_prototype();
    let sig = v8::Signature::new(scope, node);
    let proto = node.prototype_template(scope);
    // members in declaration order (item 1); getters / setters are templates too (items 2, 3, 19)
    let g = method(scope, cb_node_type, 0, Some(sig));
    let n = v8::String::new(scope, "get nodeType").unwrap();
    g.set_class_name(n); // an accessor function's `name` (item 3)
    let k = key(scope, "nodeType");
    proto.set_accessor_property(k, Some(g), None, v8::PropertyAttribute::NONE);
    let g = method(scope, cb_text_get, 0, Some(sig));
    let n = v8::String::new(scope, "get textContent").unwrap();
    g.set_class_name(n);
    let s = method(scope, cb_noop, 1, Some(sig));
    let n = v8::String::new(scope, "set textContent").unwrap();
    s.set_class_name(n);
    let k = key(scope, "textContent");
    proto.set_accessor_property(k, Some(g), Some(s), v8::PropertyAttribute::NONE);
    if !omit.contains(&"appendChild") {
        let f = method(scope, cb_append, 1, Some(sig));
        let k = key(scope, "appendChild");
        proto.set(k, f.into());
    }
    let f = method(scope, cb_fetch_it, 0, None);
    let k = key(scope, "fetchIt");
    proto.set(k, f.into());
    let f = method(scope, cb_node_type, 0, None); // policy "none": any receiver
    let k = key(scope, "lenient");
    proto.set(k, f.into());
    let ext = v8::External::new(scope, &DATA_VALUE as *const i32 as *mut std::ffi::c_void);
    let f = v8::FunctionTemplate::builder(cb_data)
        .data(ext.into())
        .constructor_behavior(v8::ConstructorBehavior::Throw)
        .signature(sig)
        .build(scope);
    let k = key(scope, "fromData");
    proto.set(k, f.into());
    // constants on both the prototype and the interface object (item 6)
    let one = v8::Integer::new(scope, 1);
    let k = key(scope, "ELEMENT_NODE");
    proto.set_with_attr(k, one.into(), ro_dd());
    // symbols (item 9, 20)
    let tag = v8::Symbol::get_to_string_tag(scope);
    let tv = v8::String::new(scope, "SpikeNode").unwrap();
    proto.set_with_attr(tag.into(), tv.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM);
    let it = v8::Symbol::get_iterator(scope);
    proto.set_intrinsic_data_property(it.into(), v8::Intrinsic::ArrayProtoValues, v8::PropertyAttribute::DONT_ENUM);
    let k = key(scope, "values");
    proto.set_intrinsic_data_property(k, v8::Intrinsic::ArrayProtoValues, v8::PropertyAttribute::NONE);
    let unscopables = v8::ObjectTemplate::new(scope);
    let t = v8::Boolean::new(scope, true);
    let k = key(scope, "appendChild");
    unscopables.set(k, t.into());
    let us = v8::Symbol::get_unscopables(scope);
    proto.set_with_attr(us.into(), unscopables.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM);
    // statics and the interface-object constant (items 1, 4, 6)
    let f = method(scope, cb_static, 1, None);
    let k = key(scope, "create");
    node.set(k, f.into());
    let k = key(scope, "ELEMENT_NODE");
    node.set_with_attr(k, one.into(), ro_dd());
    // the instance: an internal field, a [LegacyUnforgeable] own accessor, a [Replaceable] one (2, 11, 22)
    let inst = node.instance_template(scope);
    inst.set_internal_field_count(1);
    let g = method(scope, cb_node_type, 0, None);
    let k = key(scope, "unforgeable");
    inst.set_accessor_property(k, Some(g), None, v8::PropertyAttribute::DONT_DELETE);
    let g = method(scope, cb_node_type, 0, None);
    let s = method(scope, cb_replaceable_set, 1, None);
    let k = key(scope, "rep");
    inst.set_accessor_property(k, Some(g), Some(s), v8::PropertyAttribute::NONE);

    // a constructible interface that sees new.target (item 7)
    let ctor = v8::FunctionTemplate::builder(cb_ctor).length(1).build(scope);
    let n = v8::String::new(scope, "SpikeCtor").unwrap();
    ctor.set_class_name(n);

    // an exotic list: indexed + NON_MASKING named interceptors on the instance template (items 10, 24)
    let list = v8::FunctionTemplate::new(scope, cb_illegal);
    let n = v8::String::new(scope, "SpikeList").unwrap();
    list.set_class_name(n);
    let lp = list.prototype_template(scope);
    let f = method(scope, cb_node_type, 1, None);
    let k = key(scope, "item");
    lp.set(k, f.into());
    let li = list.instance_template(scope);
    li.set_indexed_property_handler(v8::IndexedPropertyHandlerConfiguration::new().getter(cb_idx_get));
    li.set_named_property_handler(
        v8::NamedPropertyHandlerConfiguration::new()
            .getter(cb_named_get)
            .flags(v8::PropertyHandlerFlags::NON_MASKING | v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS),
    );

    // a legacy factory function whose .prototype is SpikeNode.prototype (item 7): the linked
    // SetPrototypeProviderTemplate, and the per-realm fallback on a template without a prototype
    let image = v8::FunctionTemplate::builder(cb_ctor).length(0).build(scope);
    let n = v8::String::new(scope, "SpikeImage").unwrap();
    image.set_class_name(n);
    unsafe { v8_function_template_set_prototype_provider_template(&*image, &*node) };
    image.read_only_prototype();
    let image_fallback = v8::FunctionTemplate::builder(cb_ctor).length(0).build(scope);
    let n = v8::String::new(scope, "SpikeImage2").unwrap();
    image_fallback.set_class_name(n);
    image_fallback.remove_prototype();

    // a per-realm singleton's interface (item 12)
    let nav = v8::FunctionTemplate::new(scope, cb_illegal);
    let n = v8::String::new(scope, "SpikeNavigator").unwrap();
    nav.set_class_name(n);
    let tag = v8::Symbol::get_to_string_tag(scope);
    nav.prototype_template(scope).set_with_attr(tag.into(), n.into(), v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM);

    let t = Templates {
        base: v8::Global::new(scope, base),
        node: v8::Global::new(scope, node),
        ctor: v8::Global::new(scope, ctor),
        list: v8::Global::new(scope, list),
        image: v8::Global::new(scope, image),
        image_fallback: v8::Global::new(scope, image_fallback),
        nav: v8::Global::new(scope, nav),
        unscopables: v8::Global::new(scope, unscopables),
    };
    NODE_T.with(|n| *n.borrow_mut() = Some(t.node.clone()));
    t
}

/// What a realm's installer does with the cached templates: instantiate, publish on the global, and run
/// the few per-realm steps templates cannot express.
fn install(scope: &mut v8::PinScope, t: &Templates) {
    let global = scope.get_current_context().global(scope);
    let put = |scope: &mut v8::PinScope, name: &str, v: v8::Local<v8::Value>| {
        let k = key(scope, name);
        global.define_own_property(scope, k, v, v8::PropertyAttribute::DONT_ENUM);
    };
    let base = v8::Local::new(scope, &t.base).get_function(scope).unwrap();
    put(scope, "SpikeBase", base.into());
    let node_t = v8::Local::new(scope, &t.node);
    let node = node_t.get_function(scope).unwrap();
    node.set_prototype(scope, base.into()); // inherit() chains the prototypes, not the interface objects
    put(scope, "SpikeNode", node.into());
    // item 17: a lazy data property, instantiated on first read
    let k = key(scope, "SpikeLazy");
    global.set_lazy_data_property_with_data(
        scope,
        k,
        cb_lazy_global,
        v8::undefined(scope).into(),
        v8::PropertyAttribute::DONT_ENUM,
        v8::SideEffectType::HasNoSideEffect,
        v8::SideEffectType::HasSideEffect,
    );
    let ctor = v8::Local::new(scope, &t.ctor).get_function(scope).unwrap();
    put(scope, "SpikeCtor", ctor.into());
    let list_t = v8::Local::new(scope, &t.list);
    let list = list_t.get_function(scope).unwrap();
    put(scope, "SpikeList", list.into());
    let image = v8::Local::new(scope, &t.image).get_function(scope).unwrap();
    put(scope, "SpikeImage", image.into());
    let image2 = v8::Local::new(scope, &t.image_fallback).get_function(scope).unwrap();
    let k = key(scope, "prototype");
    let node_proto = node.get(scope, k.into()).unwrap();
    image2.define_own_property(scope, k, node_proto, v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM | v8::PropertyAttribute::DONT_DELETE);
    put(scope, "SpikeImage2", image2.into());
    // a conditionally installed member lands after `constructor` (item 1; Blink's InstallConditionalFeatures)
    let node_proto = v8::Local::<v8::Object>::try_from(node_proto).unwrap();
    let late = v8::FunctionTemplate::builder(cb_node_type).length(0).constructor_behavior(v8::ConstructorBehavior::Throw).build(scope).get_function(scope).unwrap();
    let k = key(scope, "lateMember");
    node_proto.define_own_property(scope, k, late.into(), v8::PropertyAttribute::NONE);
    // @@unscopables comes from an ObjectTemplate (a fresh object per realm) and gets a null prototype
    let us = v8::Symbol::get_unscopables(scope);
    let u = node_proto.get(scope, us.into()).unwrap();
    let u = v8::Local::<v8::Object>::try_from(u).unwrap();
    let null = v8::null(scope);
    u.set_prototype(scope, null.into());
    // instances: two nodes from the instance template, and a list
    let inst_t = node_t.instance_template(scope);
    let a = inst_t.new_instance(scope).unwrap();
    let b = inst_t.new_instance(scope).unwrap();
    put(scope, "nodeA", a.into());
    put(scope, "nodeB", b.into());
    let l = list_t.instance_template(scope).new_instance(scope).unwrap();
    put(scope, "listA", l.into());
    // a per-realm singleton (item 12)
    let nav = v8::Local::new(scope, &t.nav).instance_template(scope).new_instance(scope).unwrap();
    put(scope, "spikeNavigator", nav.into());
    // the named-properties splice: node.prototype -> named-props object -> base.prototype (item 8)
    let np_t = v8::ObjectTemplate::new(scope);
    np_t.set_named_property_handler(v8::NamedPropertyHandlerConfiguration::new().getter(cb_named_get).flags(v8::PropertyHandlerFlags::NON_MASKING));
    let np = np_t.new_instance(scope).unwrap();
    let pk = key(scope, "prototype");
    let base_proto = base.get(scope, pk.into()).unwrap();
    np.set_prototype(scope, base_proto);
    let spliced = inst_t.new_instance(scope).unwrap();
    let sp_proto = v8::Object::new(scope);
    sp_proto.set_prototype(scope, np.into());
    let _ = spliced;
    put(scope, "namedProps", np.into());
    // an immutable [[Prototype]], as the global's (item 27)
    let im_t = v8::ObjectTemplate::new(scope);
    im_t.set_immutable_proto();
    let im = im_t.new_instance(scope).unwrap();
    put(scope, "immutableProto", im.into());
    put(scope, "splicedProto", sp_proto.into());
    let _ = &t.unscopables;
}

fn eval(scope: &mut v8::PinScope, src: &str) -> String {
    v8::tc_scope!(let tc, scope);
    let code = v8::String::new(tc, src).unwrap();
    let Some(script) = v8::Script::compile(tc, code, None) else {
        let e = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_default();
        return format!("COMPILE {e}");
    };
    match script.run(tc) {
        Some(v) => v.to_rust_string_lossy(tc),
        None => {
            let e = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_default();
            format!("THROW {e}")
        }
    }
}

/// The JS side of the checks: one line per observation, `name=value`.
const PROBE: &str = r#"(() => {
  const o = [];
  const put = (k, f) => { let v; try { v = f(); } catch (e) { v = 'probe-throw:' + e.message; } o.push(k + '=' + v); };
  const tryv = (f) => { try { const r = f(); return 'ok:' + (typeof r === 'string' ? r : typeof r === 'number' ? r : typeof r); } catch (e) { return 'throw:' + e.name + ':' + e.message; } };
  const d = (o2, k) => { const x = Object.getOwnPropertyDescriptor(o2, k); if (!x) return 'absent'; return ('value' in x ? 'd:' + (x.writable ? 'w' : '-') : 'a:') + (x.enumerable ? 'e' : '-') + (x.configurable ? 'c' : '-'); };
  const P = SpikeNode.prototype;
  put('1.protoKeys', () => Object.getOwnPropertyNames(P).join(','));
  put('1.protoSymbols', () => Object.getOwnPropertySymbols(P).map(String).join(','));
  put('1.ifaceKeys', () => Object.getOwnPropertyNames(SpikeNode).join(','));
  put('1.globalKeys', () => Object.getOwnPropertyNames(globalThis).filter((k) => /^Spike/.test(k)).join(','));
  put('2.nodeType', () => d(P, 'nodeType'));
  put('2.appendChild', () => d(P, 'appendChild'));
  put('2.ELEMENT_NODE', () => d(P, 'ELEMENT_NODE'));
  put('2.unforgeable', () => d(nodeA, 'unforgeable'));
  put('2.unforgeableShared', () => Object.getOwnPropertyDescriptor(nodeA, 'unforgeable').get === Object.getOwnPropertyDescriptor(nodeB, 'unforgeable').get);
  nodeA.rep = 5;
  put('2.replaceable', () => d(nodeA, 'rep') + '/' + nodeA.rep);
  put('2.lenientAnyReceiver', () => tryv(() => P.lenient.call({})));
  const g = Object.getOwnPropertyDescriptor(P, 'nodeType').get;
  const s = Object.getOwnPropertyDescriptor(P, 'textContent').set;
  put('3.getName', () => JSON.stringify(g.name) + '/' + g.length);
  put('3.setName', () => JSON.stringify(s.name) + '/' + s.length);
  put('3.methodName', () => JSON.stringify(P.appendChild.name) + '/' + P.appendChild.length);
  put('3.toString', () => Function.prototype.toString.call(P.appendChild));
  put('3.tells', () => ['prototype', 'arguments', 'caller'].filter((k) => Object.prototype.hasOwnProperty.call(P.appendChild, k)).join('+') || '-');
  put('3.ctorName', () => JSON.stringify(SpikeNode.name) + '/' + SpikeNode.length);
  put('4.plainReceiver', () => tryv(() => P.appendChild.call({}, 1)));
  put('4.createReceiver', () => tryv(() => P.appendChild.call(Object.create(P), 1)));
  put('4.getterPlain', () => tryv(() => g.call({})));
  put('4.realInstance', () => tryv(() => P.appendChild.call(nodeA, 'x')));
  put('4.staticAnyReceiver', () => tryv(() => SpikeNode.create.call({})));
  globalThis.__rej = 'pending';
  P.fetchIt.call({}).then(() => { globalThis.__rej = 'resolved'; }, (e) => { globalThis.__rej = 'rejected:' + e.name + ':' + e.message; });
  globalThis.__res = 'pending';
  P.fetchIt.call(nodeA).then(() => { globalThis.__res = 'resolved'; }, (e) => { globalThis.__res = 'rejected:' + e.message; });
  put('5.arity', () => tryv(() => nodeA.appendChild()));
  put('6.ifaceConst', () => d(SpikeNode, 'ELEMENT_NODE') + '/' + SpikeNode.ELEMENT_NODE);
  put('6.protoConst', () => d(P, 'ELEMENT_NODE') + '/' + P.ELEMENT_NODE);
  put('7.illegal', () => tryv(() => new SpikeNode()));
  put('7.callNotNew', () => tryv(() => SpikeCtor(1)));
  class Sub extends SpikeCtor {}
  const sub = new Sub(1);
  put('7.newTarget', () => (sub.sawNewTarget === Sub) + '/' + (Object.getPrototypeOf(sub) === Sub.prototype));
  put('7.factoryProto', () => (SpikeImage.prototype === SpikeNode.prototype) + '/' + d(SpikeImage, 'prototype'));
  put('7.factoryProtoFallback', () => (SpikeImage2.prototype === SpikeNode.prototype) + '/' + d(SpikeImage2, 'prototype'));
  put('8.inherit', () => (Object.getPrototypeOf(SpikeNode) === SpikeBase) + '/' + (Object.getPrototypeOf(P) === SpikeBase.prototype));
  put('8.splice', () => (Object.getPrototypeOf(splicedProto) === namedProps) + '/' + (Object.getPrototypeOf(namedProps) === SpikeBase.prototype) + '/' + splicedProto.nfoo);
  put('9.toStringTag', () => Object.prototype.toString.call(nodeA));
  put('9.iterator', () => (P[Symbol.iterator] === Array.prototype.values) + '/' + (P.values === Array.prototype.values));
  const u = P[Symbol.unscopables];
  put('9.unscopables', () => Object.keys(u).join(',') + '/' + (Object.getPrototypeOf(u) === null));
  put('10.indexed', () => listA[1] + '/' + listA[5]);
  put('10.namedNonMasking', () => listA.nfoo + '/' + typeof listA.item + '/' + listA.other);
  put('12.navigatorTag', () => Object.prototype.toString.call(spikeNavigator));
  put('13.appendChildPresent', () => 'appendChild' in P);
  put('16.data', () => P.fromData.call(nodeA));
  put('17.lazyBefore', () => JSON.stringify(Object.getOwnPropertyDescriptor(globalThis, 'SpikeLazy') ? 'present' : 'absent'));
  put('27.immutable', () => tryv(() => Object.setPrototypeOf(immutableProto, {})).split(':').slice(0, 2).join(':'));
  put('17.lazyValue', () => SpikeLazy === SpikeNode);
  put('17.lazyDescriptor', () => d(globalThis, 'SpikeLazy'));
  put('17.order', () => Object.getOwnPropertyNames(globalThis).filter((k) => /^Spike/.test(k)).join(','));
  return o.join('\n');
})()"#;

fn observations(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

struct Report {
    rows: Vec<(String, bool, String)>,
}

impl Report {
    fn check(&mut self, item: &str, ok: bool, detail: String) {
        self.rows.push((item.to_string(), ok, detail));
    }
    fn expect(&mut self, item: &str, obs: &std::collections::BTreeMap<String, String>, key: &str, want: &str) {
        let got = obs.get(key).cloned().unwrap_or_else(|| "<missing>".into());
        self.check(item, got == want, format!("{key}: {got}{}", if got == want { String::new() } else { format!("  (want {want})") }));
    }
}

fn new_realm<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Context> {
    let main = scope.get_current_context();
    let realm = v8::Context::new(scope, Default::default());
    let token = main.get_security_token(scope);
    realm.set_security_token(token);
    for slot in [deno_core::CONTEXT_STATE_SLOT_INDEX, deno_core::MODULE_MAP_SLOT_INDEX] {
        let p = main.get_aligned_pointer_from_embedder_data(slot);
        // SAFETY: the main realm's pointers, which outlive this scratch realm (as in browsing_context).
        unsafe { realm.set_aligned_pointer_in_embedder_data(slot, p) };
    }
    realm
}

fn realm_checks(r: &mut Report, realm: &str, obs: &std::collections::BTreeMap<String, String>) {
    let it = |n: u32| format!("{n:>2} {realm}");
    r.expect(&it(1), obs, "1.protoKeys", "nodeType,textContent,appendChild,fetchIt,lenient,fromData,ELEMENT_NODE,values,constructor,lateMember");
    r.expect(&it(1), obs, "1.protoSymbols", "Symbol(Symbol.toStringTag),Symbol(Symbol.iterator),Symbol(Symbol.unscopables)");
    r.expect(&it(1), obs, "1.ifaceKeys", "length,name,prototype,create,ELEMENT_NODE");
    r.expect(&it(1), obs, "1.globalKeys", "SpikeBase,SpikeNode,SpikeLazy,SpikeCtor,SpikeList,SpikeImage,SpikeImage2");
    r.expect(&it(2), obs, "2.nodeType", "a:ec");
    r.expect(&it(2), obs, "2.appendChild", "d:wec");
    r.expect(&it(2), obs, "2.ELEMENT_NODE", "d:-e-");
    r.expect(&it(2), obs, "2.unforgeable", "a:e-");
    r.expect(&it(2), obs, "2.unforgeableShared", "true");
    r.expect(&it(2), obs, "2.replaceable", "d:wec/5");
    r.expect(&it(2), obs, "2.lenientAnyReceiver", "ok:1");
    r.expect(&it(3), obs, "3.getName", "\"get nodeType\"/0");
    r.expect(&it(3), obs, "3.setName", "\"set textContent\"/1");
    r.expect(&it(3), obs, "3.methodName", "\"appendChild\"/1");
    r.expect(&it(3), obs, "3.toString", "function appendChild() { [native code] }");
    r.expect(&it(3), obs, "3.tells", "-");
    r.expect(&it(3), obs, "3.ctorName", "\"SpikeNode\"/0");
    r.expect(&it(4), obs, "4.plainReceiver", "throw:TypeError:Illegal invocation");
    r.expect(&it(4), obs, "4.createReceiver", "throw:TypeError:Illegal invocation");
    r.expect(&it(4), obs, "4.getterPlain", "throw:TypeError:Illegal invocation");
    r.expect(&it(4), obs, "4.realInstance", "ok:x");
    r.expect(&it(4), obs, "4.staticAnyReceiver", "ok:7");
    r.expect(&it(5), obs, "5.arity", "throw:TypeError:Failed to execute 'appendChild' on 'SpikeNode': 1 argument required, but only 0 present.");
    r.expect(&it(6), obs, "6.ifaceConst", "d:-e-/1");
    r.expect(&it(6), obs, "6.protoConst", "d:-e-/1");
    r.expect(&it(7), obs, "7.illegal", "throw:TypeError:Failed to construct 'SpikeNode': Illegal constructor");
    r.expect(&it(7), obs, "7.callNotNew", "throw:TypeError:Failed to construct 'SpikeCtor': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
    r.expect(&it(7), obs, "7.newTarget", "true/true");
    r.expect(&it(7), obs, "7.factoryProto", "true/d:---");
    r.expect(&it(7), obs, "7.factoryProtoFallback", "true/d:---");
    r.expect(&it(8), obs, "8.inherit", "true/true");
    r.expect(&it(8), obs, "8.splice", "true/true/named:nfoo");
    r.expect(&it(9), obs, "9.toStringTag", "[object SpikeNode]");
    r.expect(&it(9), obs, "9.iterator", "true/true");
    r.expect(&it(9), obs, "9.unscopables", "appendChild/true");
    r.expect(&it(10), obs, "10.indexed", "10/undefined");
    r.expect(&it(10), obs, "10.namedNonMasking", "named:nfoo/function/undefined");
    r.expect(&it(12), obs, "12.navigatorTag", "[object SpikeNavigator]");
    r.expect(&it(13), obs, "13.appendChildPresent", "true");
    r.expect(&it(16), obs, "16.data", "42");
    r.expect(&it(27), obs, "27.immutable", "throw:TypeError");
    r.expect(&it(17), obs, "17.lazyValue", "true");
    r.expect(&it(17), obs, "17.lazyDescriptor", "d:w-c");
    r.expect(&it(17), obs, "17.order", "SpikeBase,SpikeNode,SpikeLazy,SpikeCtor,SpikeList,SpikeImage,SpikeImage2");
}

/// The tokio runtime each spike's isolate is made inside. deno_core's V8 platform hands a DELAYED task
/// (V8's memory reducer posts one when a GC sees possible garbage) to the tokio runtime current when the
/// isolate was made, and ABORTS the process when there was none: these two tests did that about once in
/// twelve parallel runs of this binary (gdb: MemoryReducer::NotifyPossibleGarbage -> PostDelayedTask ->
/// deno_core::runtime::setup::spawn_delayed_task -> abort), taking every other test down with them.
fn spike_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime for the spike's isolate")
}

#[test]
fn template_feasibility_spike() {
    let tokio = spike_runtime();
    let _in_tokio = tokio.enter();
    let mut rt = engine::new_runtime(RuntimeOptions { extensions: vec![spike_ext::init()], ..Default::default() });
    let main = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), main);
    let mut r = Report { rows: Vec::new() };
    let t = build_templates(scope, &[]);

    // realm 1 (the main context) and realm 2 (a fresh context of the same isolate)
    install(scope, &t);
    let obs1 = observations(&eval(scope, PROBE));
    scope.perform_microtask_checkpoint();
    let rej1 = eval(scope, "globalThis.__rej + ' | ' + globalThis.__res");
    let realm2 = new_realm(scope);
    let (obs2, rej2, cross) = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm2);
        install(scope, &t);
        let obs2 = observations(&eval(scope, PROBE));
        scope.perform_microtask_checkpoint();
        let rej2 = eval(scope, "globalThis.__rej + ' | ' + globalThis.__res");
        (obs2, rej2, ())
    };
    let _ = cross;
    realm_checks(&mut r, "realm1", &obs1);
    realm_checks(&mut r, "realm2", &obs2);
    let want_rej = "rejected:TypeError:Failed to execute 'fetchIt' on 'SpikeNode': Illegal invocation | resolved";
    r.check(" 4 realm1", rej1 == want_rej, format!("promise member: {rej1}"));
    r.check(" 4 realm2", rej2 == want_rej, format!("promise member: {rej2}"));

    // items 12, 14, 31: per-realm identity and cross-realm calls (realm 2 reads realm 1's objects)
    let g1 = scope.get_current_context().global(scope);
    let names = ["SpikeNode", "nodeA", "spikeNavigator"];
    {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm2);
        let g2 = scope.get_current_context().global(scope);
        for n in names {
            let k = key(scope, n);
            let v = g1.get(scope, k.into()).unwrap();
            let k2 = key(scope, &format!("r1_{n}"));
            g2.set(scope, k2.into(), v);
        }
        let per_realm = eval(scope, "[r1_SpikeNode !== SpikeNode, r1_SpikeNode.prototype !== SpikeNode.prototype, r1_spikeNavigator !== spikeNavigator].join('/')");
        r.check("12 per-realm", per_realm == "true/true/true", format!("interface, prototype, singleton differ per realm: {per_realm}"));
        r.check("31 per-realm", per_realm.starts_with("true/true"), format!("get_function in a second context is that context's own: {per_realm}"));
        let cross = eval(scope, "[SpikeNode.prototype.appendChild.call(r1_nodeA, 'c'), r1_SpikeNode.prototype.appendChild.call(nodeA, 'd'), (r1_nodeA instanceof r1_SpikeNode), (r1_nodeA instanceof SpikeNode)].join('/')");
        r.check("14 cross-realm", cross == "c/d/true/false", format!("a realm-2 member on a realm-1 object (and back); instanceof per realm: {cross}"));
        let again = eval(scope, "SpikeNode === SpikeNode && Object.getPrototypeOf(nodeA) === SpikeNode.prototype");
        r.check("31 per-realm", again == "true", format!("instances take the realm's own prototype: {again}"));
    }

    // item 11: an instance of OUR template is wrappable by deno_core's cppgc, and unwraps by type
    {
        let node_t = v8::Local::new(scope, &t.node);
        let inst = node_t.instance_template(scope).new_instance(scope).unwrap();
        let wrapped = deno_core::cppgc::wrap_object(scope, inst, SpikeBox { v: 5.0 });
        let back = deno_core::cppgc::try_unwrap_cppgc_object::<SpikeBox>(scope, wrapped.into());
        let v = back.map(|b| b.v);
        r.check("11 cppgc", v == Some(5.0), format!("wrap_object on a template instance, then try_unwrap: {v:?}"));
        let fields = inst.internal_field_count();
        r.check("22 fields", fields == 1, format!("set_internal_field_count(1) -> {fields}"));
    }

    // item 29: HasInstance, linked from the V8 archive
    {
        let a = eval(scope, "typeof nodeA");
        let k = key(scope, "nodeA");
        let na = g1.get(scope, k.into()).unwrap();
        let plain: v8::Local<v8::Value> = v8::Object::new(scope).into();
        let yes = has_node_instance(scope, na);
        let no = has_node_instance(scope, plain);
        r.check("29 HasInstance", yes && !no && a == "object", format!("instance {yes}, plain object {no}"));
    }

    // item 30: a deno_core #[op2] class adopted per realm through its own template
    {
        let main_ctor = eval(scope, "typeof Deno.core.ops.SpikeBox");
        let main_box = deno_core::cppgc::make_cppgc_object(scope, SpikeBox { v: 1.0 });
        let k = key(scope, "__box1");
        g1.set(scope, k.into(), main_box.into());
        let same_main = eval(scope, "Object.getPrototypeOf(__box1).constructor === Deno.core.ops.SpikeBox");
        let res = {
            let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm2);
            let b2 = deno_core::cppgc::make_cppgc_object(scope, SpikeBox { v: 2.0 });
            let g2 = scope.get_current_context().global(scope);
            let k = key(scope, "__box2");
            g2.set(scope, k.into(), b2.into());
            let k1 = key(scope, "__box1");
            let v1 = g1.get(scope, k1.into()).unwrap();
            g2.set(scope, k1.into(), v1);
            eval(scope, "(() => { const C2 = Object.getPrototypeOf(__box2).constructor; const C1 = Object.getPrototypeOf(__box1).constructor; const made = new C2(7); return [typeof C2, C2 !== C1, C2.name, made.v, Object.getPrototypeOf(made) === C2.prototype, __box2.v, Object.getOwnPropertyDescriptor(C1.prototype, 'v').get.call(__box2)].join('/'); })()")
        };
        r.check("30 adopt", main_ctor == "function" && same_main == "true" && res == "function/true/SpikeBox/7/true/2/2",
            format!("main class {main_ctor}, instance.constructor is it: {same_main}; realm 2: {res}"));
    }

    // API presence items exercised above
    for (n, what) in [("18", "Signature::new + builder.signature"), ("19", "set_accessor_property"),
                      ("20", "set_intrinsic_data_property"), ("21", "External template data"),
                      ("23", "set_lazy_data_property_with_data"), ("24", "NON_MASKING"),
                      ("25", "inherit / read_only_prototype / remove_prototype"),
                      ("26", "length / constructor_behavior / set_class_name")] {
        let depends = match n { "18" => vec![" 4 realm1"], "19" => vec![" 2 realm1"], "20" => vec![" 9 realm1"],
                                "21" => vec!["16 realm1"], "23" => vec!["17 realm1"], "24" => vec!["10 realm1"],
                                "25" => vec![" 7 realm1", " 8 realm1"], _ => vec![" 3 realm1", " 7 realm1"] };
        let ok = r.rows.iter().filter(|(i, _, _)| depends.contains(&i.as_str())).all(|(_, ok, _)| *ok);
        r.check(&format!("{n} api"), ok, format!("{what} (via items {depends:?})"));
    }

    let mut failed = 0;
    for (item, ok, detail) in &r.rows {
        println!("item {item:<10} {}  {detail}", if *ok { "ok  " } else { "FAIL" });
        failed += (!ok) as usize;
    }
    println!("spike: {} checks, {failed} failed", r.rows.len());
    assert_eq!(failed, 0, "feasibility items failed -- see the table above");
}

// item 13: persona selection -- a member the bundle does not record is never added; the rest keep order
// item 15: the same builder in a second isolate (a worker's)
#[test]
fn template_spike_second_isolate_and_omission() {
    let tokio = spike_runtime();
    let _in_tokio = tokio.enter();
    let mut rt = engine::new_runtime(RuntimeOptions::default());
    let main = rt.main_context();
    v8::scope_with_context!(scope, rt.v8_isolate(), main);
    let t = build_templates(scope, &["appendChild"]);
    install(scope, &t);
    let obs = observations(&eval(scope, PROBE));
    let keys = obs.get("1.protoKeys").cloned().unwrap_or_default();
    println!("item 13 omit: {keys}");
    assert_eq!(keys, "nodeType,textContent,fetchIt,lenient,fromData,ELEMENT_NODE,values,constructor,lateMember");
    assert_eq!(obs.get("13.appendChildPresent").map(String::as_str), Some("false"));
    // (appendChild is the omitted member here, so the Signature is checked on a getter)
    println!("item 15 second isolate: 4.getterPlain={:?} 3.getName={:?}", obs.get("4.getterPlain"), obs.get("3.getName"));
    assert_eq!(obs.get("4.getterPlain").map(String::as_str), Some("throw:TypeError:Illegal invocation"));
    assert_eq!(obs.get("3.getName").map(String::as_str), Some("\"get nodeType\"/0"));
}
