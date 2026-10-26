//! `modules/structured_clone.rs` — the `structuredClone` global.
//!
//! Was `__structuredCloneImpl` in `core/korvus-runtime/src/js/shared-globals.js`: 70 lines of JS behind
//! an `op_nativize` mask, whose platform-object branch wrote the clone's members through the
//! `KB.__evtSlotSet` bridge.
//!
//! WHY IT HAD TO MOVE IN THIS COMMIT rather than a later one. `ImageData`'s rows became BRANDED reads
//! in the same change, and a brand is a `v8::Private` that page JS cannot forge — which is the point
//! of it. The JS clone built its copy with `Object.create(Object.getPrototypeOf(v))`, so the copy
//! carried the prototype and NOT the brand, and every accessor on it threw `Illegal invocation`.
//! `tests/conformance.rs::image_data`'s `structured-clone` case caught it on the first gate run. A
//! Rust clone can stamp the brand; nothing in JS can, and nothing in JS should be able to.
//!
//! THE ALGORITHM IS THE JS ONE, ported: cycles through a seen-map, `Date`, `RegExp`, `ArrayBuffer`,
//! every `ArrayBufferView`, `Array`, `Map`, `Set`, and a generic object whose PROTOTYPE is preserved so
//! a platform object clones as its own type and survives `instanceof`. A function is a
//! `DataCloneError`. It is not a JSON round-trip.
//!
//! NOT V8's `ValueSerializer` (which `op_msg_serialize` / `op_msg_deserialize` use for postMessage),
//! and the reason is the one thing this algorithm exists for: the serializer has no host-object hook
//! registered in this engine, so a `DOMPoint`, a `DOMRect` or an `ImageData` would come back a plain
//! object. Making the two ONE implementation means teaching the serializer about platform objects,
//! which is the worker-transfer side's work and not this unit's. Recorded rather than pretended.

use deno_core::v8;

use crate::binding::protos::{call_method, get_prop, global_get, new_global, throw_dom};

/// `structuredClone(any value, optional StructuredSerializeOptions options)` — `value` is REQUIRED, so
/// a bare `structuredClone()` is a WebIDL arity TypeError rather than a clone of `undefined`.
pub(crate) fn cb_structured_clone<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue) {
    // The RECEIVER first, before the argument count (Chrome 150.0.7871.46 over loopback):
    // `structuredClone.call({}, 1)` and `structuredClone.call({})` are both `TypeError: Illegal
    // invocation` in a window, a frame and a worker, while `structuredClone.call(null, 1)` answers --
    // V8 hands a native function called with null the global. This body had no check, so the window
    // cloned for any receiver.
    if !crate::core::html::window_globals::window_receiver(scope, args.this().into()) {
        return;
    }
    if args.length() < 1 {
        let iface = if global_get(scope, "window").map(|v| v.is_object()).unwrap_or(false) { "Window" } else { "WorkerGlobalScope" };
        crate::binding::protos::throw_type_error(
            scope,
            &format!("Failed to execute 'structuredClone' on '{iface}': 1 argument required, but only 0 present."),
        );
        return;
    }
    let iface = if global_get(scope, "window").map(|v| v.is_object()).unwrap_or(false) { "Window" } else { "WorkerGlobalScope" };
    // `{transfer}`: a transferred port is the clone's new port, a transferred buffer is
    // detached at the source once its bytes are in the clone.
    let transfer = v8::Local::<v8::Object>::try_from(args.get(1)).ok().and_then(|o| get_prop(scope, o, "transfer")).filter(|t| !t.is_undefined());
    let Some(list) = crate::core::messaging::transfer::read_window_transfer(scope, transfer, true) else { return };
    let originals = crate::core::messaging::transfer::port_originals(&list);
    let Some(v) = clone_for_transfer(scope, args.get(0), iface, "structuredClone", &originals) else { return };
    let (_, pairs) = crate::core::messaging::transfer::take_ports_mapped(scope, &list, None);
    let v = substitute(scope, v, &pairs);
    crate::core::messaging::transfer::detach_buffers(scope, &list);
    rv.set(v);
}

/// WHO is cloning, for the `DataCloneError` sentence. Chrome's is
/// `Failed to execute '<method>' on '<interface>': <the value> could not be cloned.`, and the
/// interface is the CALLER's -- `structuredClone` on a `Window`, `postMessage` on a `MessagePort`.
/// One struct so the sentence has one writer and no call site can invent a variant of it.
pub(crate) struct CloneContext {
    iface: String,
    method: &'static str,
    /// What the call TRANSFERS (its ports): passed through as they are, not cloned.
    transfer: Vec<v8::Global<v8::Value>>,
}
type CloneCtx = CloneContext;

impl CloneContext {
    /// `<value> could not be cloned.` — the value rendered the way Chrome renders it, which is V8's
    /// DETAIL string: a function's source text, `Symbol(desc)` for a symbol. `ToString` cannot be
    /// used, because it throws on exactly the symbols this sentence has to name.
    fn refuse(&self, scope: &mut v8::PinScope, v: v8::Local<v8::Value>) {
        let shown = v.to_detail_string(scope).map(|s| s.to_rust_string_lossy(scope)).unwrap_or_else(|| "object".to_string());
        let (iface, method) = (&self.iface, self.method);
        throw_dom(scope, &format!("Failed to execute '{method}' on '{iface}': {shown} could not be cloned."), "DataCloneError");
    }
}

/// StructuredSerialize, with the caller's name in whatever it refuses. THE clone entry point: every
/// `postMessage` body and `structuredClone` itself go through here, so the refusal sentence is the
/// same rule everywhere and the only thing that varies is the two names in it.
pub(crate) fn clone_for<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    iface: &str,
    method: &'static str,
) -> Option<v8::Local<'s, v8::Value>> {
    clone_for_transfer(scope, v, iface, method, &[])
}

/// [`clone_for`] for a call that transfers `transferred` (its ports): each one stays itself in the clone,
/// for [`substitute`] to replace with what it became once the transfer is taken.
pub(crate) fn clone_for_transfer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    iface: &str,
    method: &'static str,
    transferred: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    let transfer = transferred.iter().map(|t| v8::Global::new(scope, *t)).collect();
    let ctx = CloneContext { iface: iface.to_string(), method, transfer };
    clone_with_v8(scope, v, &ctx)
}

/// [`clone_for_transfer`] read back in `realm` -- the RECEIVER's, where the message is delivered (as
/// `browsing_context`'s `clone_into_realm`): a port message to a frame is the frame's own objects, `e.data instanceof
/// Object` there (Chrome 150 measured). `None`: the current realm.
pub(crate) fn clone_for_transfer_in<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    realm: Option<v8::Local<'s, v8::Context>>,
    v: v8::Local<'s, v8::Value>,
    iface: &str,
    method: &'static str,
    transferred: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    let Some(realm) = realm.filter(|r| *r != scope.get_current_context()) else {
        return clone_for_transfer(scope, v, iface, method, transferred);
    };
    let kept: Vec<v8::Global<v8::Value>> = transferred.iter().map(|t| v8::Global::new(scope, *t)).collect();
    let g = {
        let scope: &mut v8::PinScope = &mut v8::ContextScope::new(scope, realm);
        let v = v8::Local::new(scope, v);
        let transferred: Vec<v8::Local<v8::Value>> = kept.iter().map(|t| v8::Local::new(scope, t)).collect();
        clone_for_transfer(scope, v, iface, method, &transferred).map(|d| v8::Global::new(scope, d))
    }?;
    Some(v8::Local::new(scope, &g))
}

// ── THE CLONE IS V8'S SERIALIZER, as Blink's is ───────────────────────────────
//
// A script's own values went through the hand-written walk below, which kept what V8's serializer drops and
// dropped what it keeps -- measured against stock Chrome 150 over 45 values (`tests/fixtures/structured-clone`):
// a class instance kept its prototype (Chrome: a plain object), an array's holes became `undefined` and its
// named properties were lost, a sparse array was filled, and a WeakMap, WeakSet, WeakRef, Promise, Proxy,
// generator, `arguments` or iterator cloned as an empty object where Chrome throws V8's own
// "#<WeakMap> could not be cloned.". So the value now goes through V8's `ValueSerializer`, and the platform
// objects -- which V8 cannot see into -- through its host-object hook: a `[Serializable]` one is cloned by
// the walk below (its state, its brand), a transferred port passes as itself (for `substitute`), any other is
// Chrome's "<Interface> object could not be cloned.". The clone is read back in the CURRENT realm, and the
// read runs no script.

/// The serializer's side table: the objects a host-object record stands for, by index.
type HostStash = std::rc::Rc<std::cell::RefCell<Vec<v8::Global<v8::Object>>>>;

struct LocalSer<'a> {
    ctx: &'a CloneContext,
    stash: HostStash,
}

impl LocalSer<'_> {
    fn transferred(&self, scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
        self.ctx.transfer.iter().any(|t| v8::Local::new(scope, t).strict_equals(v))
    }
}

impl v8::ValueSerializerImpl for LocalSer<'_> {
    fn has_custom_host_object(&self, _isolate: &v8::Isolate) -> bool {
        true
    }
    fn is_host_object<'s>(&self, scope: &mut v8::PinScope<'s, '_>, object: v8::Local<'s, v8::Object>) -> Option<bool> {
        let v: v8::Local<v8::Value> = object.into();
        Some(self.transferred(scope, v) || platform_interface(scope, v).is_some())
    }
    fn write_host_object<'s>(&self, scope: &mut v8::PinScope<'s, '_>, object: v8::Local<'s, v8::Object>, h: &dyn v8::ValueSerializerHelper) -> Option<bool> {
        let v: v8::Local<v8::Value> = object.into();
        let out = if self.transferred(scope, v) {
            object
        } else {
            let iface = platform_interface(scope, v)?;
            if !SERIALIZABLE.contains(&iface.as_str()) {
                refuse_platform(scope, &iface, self.ctx);
                return None;
            }
            let seen = v8::Map::new(scope);
            let c = clone_object(scope, v, seen, 0, self.ctx)?;
            v8::Local::<v8::Object>::try_from(c).ok()?
        };
        let mut stash = self.stash.borrow_mut();
        stash.push(v8::Global::new(scope, out));
        h.write_uint32((stash.len() - 1) as u32);
        Some(true)
    }
    /// V8's own refusal ("#<WeakMap> could not be cloned.", "Symbol(q) could not be cloned.", "An ArrayBuffer is
    /// detached and could not be cloned.") as Blink throws it: a DataCloneError with the caller's sentence.
    fn throw_data_clone_error<'s>(&self, scope: &mut v8::PinScope<'s, '_>, message: v8::Local<'s, v8::String>) {
        let m = message.to_rust_string_lossy(scope);
        throw_dom(scope, &format!("Failed to execute '{}' on '{}': {m}", self.ctx.method, self.ctx.iface), "DataCloneError");
    }
}

struct LocalDeser {
    stash: HostStash,
}

impl v8::ValueDeserializerImpl for LocalDeser {
    fn read_host_object<'s>(&self, scope: &mut v8::PinScope<'s, '_>, h: &dyn v8::ValueDeserializerHelper) -> Option<v8::Local<'s, v8::Object>> {
        let mut i = 0u32;
        if !h.read_uint32(&mut i) {
            return None;
        }
        self.stash.borrow().get(i as usize).map(|g| v8::Local::new(scope, g))
    }
}

/// StructuredSerialize then StructuredDeserialize in one isolate: V8's serializer writes `v` (calling its
/// getters, in order, once), and its deserializer reads the clone back in the current realm. `None`: an
/// exception is pending -- a DataCloneError, or what a getter threw.
fn clone_with_v8<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, ctx: &CloneContext) -> Option<v8::Local<'s, v8::Value>> {
    use v8::{ValueDeserializerHelper, ValueSerializerHelper};
    let stash: HostStash = Default::default();
    let context = scope.get_current_context();
    let bytes = {
        let ser = v8::ValueSerializer::new(scope, Box::new(LocalSer { ctx, stash: stash.clone() }));
        ser.write_header();
        ser.write_value(context, v)?;
        ser.release()
    };
    let de = v8::ValueDeserializer::new(scope, Box::new(LocalDeser { stash }), &bytes);
    de.read_header(context)?;
    de.read_value(context)
}

/// Replace, inside a finished clone, every transferred object with what it became (`pairs`: original,
/// replacement) -- the clone's own objects, arrays, maps and sets, walked once each.
pub(crate) fn substitute<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    root: v8::Local<'s, v8::Value>,
    pairs: &[(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)],
) -> v8::Local<'s, v8::Value> {
    if pairs.is_empty() {
        return root;
    }
    let swap = |v: v8::Local<'s, v8::Value>| pairs.iter().find(|(o, _)| o.strict_equals(v)).map(|(_, f)| *f);
    if let Some(f) = swap(root) {
        return f;
    }
    let seen = v8::Set::new(scope);
    let mut stack = vec![root];
    while let Some(v) = stack.pop() {
        if !v.is_object() || seen.has(scope, v).unwrap_or(false) || platform_interface(scope, v).is_some() {
            continue;
        }
        seen.add(scope, v);
        if let Ok(m) = v8::Local::<v8::Map>::try_from(v) {
            let a = m.as_array(scope);
            let mut i = 0;
            while i + 1 < a.length() {
                if let (Some(k), Some(val)) = (a.get_index(scope, i), a.get_index(scope, i + 1)) {
                    if let Some(f) = swap(val) {
                        m.set(scope, k, f);
                    } else {
                        stack.push(val);
                    }
                    stack.push(k);
                }
                i += 2;
            }
            continue;
        }
        let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { continue };
        let args = v8::GetPropertyNamesArgsBuilder::new()
            .mode(v8::KeyCollectionMode::OwnOnly)
            .property_filter(v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS)
            .index_filter(v8::IndexFilter::IncludeIndices)
            .key_conversion(v8::KeyConversionMode::ConvertToString)
            .build();
        let Some(keys) = o.get_own_property_names(scope, args) else { continue };
        for i in 0..keys.length() {
            let Some(k) = keys.get_index(scope, i) else { continue };
            let Some(val) = o.get(scope, k) else { continue };
            match swap(val) {
                Some(f) => {
                    o.set(scope, k, f);
                }
                None => stack.push(val),
            }
        }
    }
    root
}

/// A guard against a prototype chain or a nesting depth this engine will not survive. The JS had no
/// limit and relied on the seen-map alone; a cycle is still handled by the map, and this only stops a
/// pathologically deep literal from blowing the Rust stack.
const MAX_DEPTH: u32 = 512;

fn clone_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    seen: v8::Local<'s, v8::Map>,
    depth: u32,
    ctx: &CloneCtx,
) -> Option<v8::Local<'s, v8::Value>> {
    if depth > MAX_DEPTH {
        throw_dom(scope, "The object is too deeply nested to be cloned.", "DataCloneError");
        return None;
    }
    // A function and a SYMBOL are both unclonable, and both name themselves in the refusal. The
    // symbol arm is newer: `!v.is_object()` used to return a symbol unchanged, so
    // `structuredClone(Symbol())` answered the symbol where Chrome 150 throws.
    if v.is_function() || v.is_symbol() {
        ctx.refuse(scope, v);
        return None;
    }
    if !v.is_object() {
        return Some(v);
    }
    if ctx.transfer.iter().any(|t| v8::Local::new(scope, t).strict_equals(v)) {
        return Some(v); // transferred: itself, until `substitute` puts what it became in its place
    }
    if let Some(prev) = seen.get(scope, v).filter(|p| !p.is_undefined()) {
        return Some(prev);
    }
    // A Boolean, Number, BigInt or String OBJECT is serialized as its [[...Data]] and comes back a new wrapper
    // of that value (HTML StructuredSerializeInternal; Chrome 150: V8's ValueSerializer, so
    // `'x' + structuredClone(Object(611n))` is 'x611'). The generic arm below kept the prototype and not the
    // internal slot, so every read of the clone's value threw `BigInt.prototype.valueOf requires that 'this' be a
    // BigInt` -- and a bot challenge posts exactly that sum. V8's own serializer
    // reads the slot, so no page-replaceable `valueOf` is called. A Symbol object is a DataCloneError
    // (`[object Symbol] could not be cloned.`), as in Chrome.
    if v.is_big_int_object() || v.is_number_object() || v.is_string_object() || v.is_boolean_object() {
        let bytes = crate::modules::clone_state::wire_serialize(scope, v, &[]);
        let out = crate::modules::clone_state::wire_deserialize(scope, &bytes, &[]);
        seen.set(scope, v, out);
        return Some(out);
    }
    if v.is_symbol_object() {
        ctx.refuse(scope, v);
        return None;
    }
    if v.is_date() {
        let t = call_method(scope, v, "getTime", &[])?;
        return new_global(scope, "Date", &[t]).map(|d| d.into());
    }
    if v.is_reg_exp() {
        let o = v8::Local::<v8::Object>::try_from(v).ok()?;
        let source = get_prop(scope, o, "source")?;
        let flags = get_prop(scope, o, "flags")?;
        return new_global(scope, "RegExp", &[source, flags]).map(|r| r.into());
    }
    if v.is_array_buffer() {
        let zero: v8::Local<v8::Value> = v8::Integer::new(scope, 0).into();
        return call_method(scope, v, "slice", &[zero]);
    }
    if v.is_array_buffer_view() {
        let o = v8::Local::<v8::Object>::try_from(v).ok()?;
        let buffer = get_prop(scope, o, "buffer")?;
        let buf = clone_value(scope, buffer, seen, depth + 1, ctx)?;
        let byte_offset = get_prop(scope, o, "byteOffset")?;
        let ctor = get_prop(scope, o, "constructor").and_then(|c| v8::Local::<v8::Function>::try_from(c).ok())?;
        // A DataView takes a byteLength where a TypedArray takes an element count, and each object's
        // own `length`/`byteLength` is the right one for its own constructor.
        let count = match get_prop(scope, o, "length").filter(|l| !l.is_undefined()) {
            Some(l) => l,
            None => get_prop(scope, o, "byteLength")?,
        };
        return ctor.new_instance(scope, &[buf, byte_offset, count]).map(|x| x.into());
    }
    if v.is_array() {
        let src = v8::Local::<v8::Array>::try_from(v).ok()?;
        let out = v8::Array::new(scope, src.length() as i32);
        seen.set(scope, v, out.into());
        for i in 0..src.length() {
            let Some(item) = src.get_index(scope, i) else { continue };
            let c = clone_value(scope, item, seen, depth + 1, ctx)?;
            out.set_index(scope, i, c);
        }
        return Some(out.into());
    }
    if v.is_map() {
        let src = v8::Local::<v8::Map>::try_from(v).ok()?;
        let out = v8::Map::new(scope);
        seen.set(scope, v, out.into());
        // `Map::as_array` is `[k0, v0, k1, v1, …]`.
        let pairs = src.as_array(scope);
        let mut i = 0;
        while i + 1 < pairs.length() {
            let (Some(k), Some(val)) = (pairs.get_index(scope, i), pairs.get_index(scope, i + 1)) else { break };
            let ck = clone_value(scope, k, seen, depth + 1, ctx)?;
            let cv = clone_value(scope, val, seen, depth + 1, ctx)?;
            out.set(scope, ck, cv);
            i += 2;
        }
        return Some(out.into());
    }
    if v.is_set() {
        let src = v8::Local::<v8::Set>::try_from(v).ok()?;
        let out = v8::Set::new(scope);
        seen.set(scope, v, out.into());
        let items = src.as_array(scope);
        for i in 0..items.length() {
            let Some(item) = items.get_index(scope, i) else { continue };
            let c = clone_value(scope, item, seen, depth + 1, ctx)?;
            out.add(scope, c);
        }
        return Some(out.into());
    }
    if v.is_native_error() {
        return clone_error(scope, v, seen, depth, ctx);
    }
    if let Some(iface) = platform_interface(scope, v) {
        if !SERIALIZABLE.contains(&iface.as_str()) {
            refuse_platform(scope, &iface, ctx);
            return None;
        }
    }
    clone_object(scope, v, seen, depth, ctx)
}

/// The interfaces HTML and Chrome mark `[Serializable]`: the only PLATFORM objects a structured clone
/// copies. Any other platform object -- a `PluginArray`, a `Navigator`, an element, an
/// `Event`, `Headers` -- is a DataCloneError in Chrome 150 (measured: `S-structured-clone-platform`),
/// and a device-check script's interstitial posts `navigator.plugins` to a frame to see that it is.
const SERIALIZABLE: &[&str] = &[
    "Blob", "File", "FileList", "ImageData", "ImageBitmap", "DOMPoint", "DOMPointReadOnly", "DOMRect",
    "DOMRectReadOnly", "DOMQuad", "DOMMatrix", "DOMMatrixReadOnly", "DOMException", "CryptoKey",
    "RTCCertificate", "RTCEncodedAudioFrame", "RTCEncodedVideoFrame", "FileSystemHandle",
    "FileSystemFileHandle", "FileSystemDirectoryHandle", "VideoFrame", "AudioData", "EncodedAudioChunk",
    "EncodedVideoChunk", "QuotaExceededError",
];

/// The most-derived interface of a PLATFORM object (one the engine made: branded, or a node wrapper) --
/// the first prototype that names an interface, else the brand's -- and `None` for a script's own object,
/// whatever its prototype.
pub(crate) fn platform_interface(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<String> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    let brand = crate::binding::brand::brand_of(scope, v).and_then(crate::binding::brand::name_of);
    if brand.is_none() && crate::binding::protos::unwrap_id(scope, v).is_none() {
        return None;
    }
    let mut cur = o.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
    for _ in 0..32 {
        let Some(p) = cur else { break };
        if let Some(n) = crate::binding::protos::iface_name_of(scope, p).filter(|n| n.is_string()) {
            return Some(n.to_rust_string_lossy(scope));
        }
        cur = p.get_prototype(scope).and_then(|x| v8::Local::<v8::Object>::try_from(x).ok());
    }
    brand
}

/// Chrome's words: `<Interface> object could not be cloned.`, and `#<Window>` for a window.
fn refuse_platform(scope: &mut v8::PinScope, iface: &str, ctx: &CloneCtx) {
    let what = match iface {
        "Window" => "#<Window> could not be cloned.".to_string(),
        "MessagePort" => "A MessagePort could not be cloned because it was not transferred.".to_string(),
        _ => format!("{iface} object could not be cloned."),
    };
    throw_dom(scope, &format!("Failed to execute '{}' on '{}': {what}", ctx.method, ctx.iface), "DataCloneError");
}

/// A native error clones as an ERROR of its type (HTML's serialization of an Error, as V8 writes it):
/// the name when it is one of the seven built-in ones (else `Error`), an own `message`, the `stack`,
/// and an own `cause`. It was a plain object -- `[object Object]`, no message.
fn clone_error<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    seen: v8::Local<'s, v8::Map>,
    depth: u32,
    ctx: &CloneCtx,
) -> Option<v8::Local<'s, v8::Value>> {
    let src = v8::Local::<v8::Object>::try_from(v).ok()?;
    let name = get_prop(scope, src, "name").map(|n| n.to_rust_string_lossy(scope)).unwrap_or_default();
    let ctor = match name.as_str() {
        "EvalError" | "RangeError" | "ReferenceError" | "SyntaxError" | "TypeError" | "URIError" => name.as_str(),
        _ => "Error",
    };
    let own = |scope: &mut v8::PinScope<'s, '_>, key: &str| -> Option<v8::Local<'s, v8::Value>> {
        let k = v8::String::new(scope, key)?;
        let d = src.get_own_property_descriptor(scope, k.into())?;
        let d = v8::Local::<v8::Object>::try_from(d).ok()?;
        get_prop(scope, d, "value").filter(|x| !x.is_undefined())
    };
    let message = own(scope, "message").and_then(|m| m.to_string(scope)).map(|m| -> v8::Local<v8::Value> { m.into() });
    let args: Vec<v8::Local<v8::Value>> = message.into_iter().collect();
    let out = new_global(scope, ctor, &args)?;
    seen.set(scope, v, out.into());
    let hidden = |scope: &mut v8::PinScope<'s, '_>, key: &str, val: v8::Local<'s, v8::Value>| {
        let Some(k) = v8::String::new(scope, key) else { return };
        let mut pd = v8::PropertyDescriptor::new_from_value_writable(val, true);
        pd.set_enumerable(false);
        pd.set_configurable(true);
        out.define_property(scope, k.into(), &pd);
    };
    if let Some(stack) = get_prop(scope, src, "stack").filter(|s| s.is_string()) {
        hidden(scope, "stack", stack);
    }
    if let Some(cause) = own(scope, "cause") {
        let c = clone_value(scope, cause, seen, depth + 1, ctx)?;
        hidden(scope, "cause", c);
    }
    Some(out.into())
}

/// Throw the clone's DataCloneError when `v` holds a platform object no structured clone may copy, and
/// answer whether it may go on. For the paths that serialize with V8's `ValueSerializer` (a worker's or a
/// channel's `postMessage`), which has no host-object hook here and would write the object's own
/// properties as if it were a script's.
pub fn refuse_platform_objects(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, iface: &str, method: &'static str) -> bool {
    let ctx = CloneContext { iface: iface.to_string(), method, transfer: Vec::new() };
    let seen = v8::Set::new(scope);
    walk_refusing(scope, v, seen, 0, &ctx)
}

fn walk_refusing(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, seen: v8::Local<v8::Set>, depth: u32, ctx: &CloneCtx) -> bool {
    if depth > MAX_DEPTH || !v.is_object() || v.is_array_buffer() || v.is_array_buffer_view() || v.is_native_error() {
        return true;
    }
    if seen.has(scope, v).unwrap_or(false) {
        return true;
    }
    seen.add(scope, v);
    if let Some(iface) = platform_interface(scope, v) {
        // A port rides the worker path's own transfer machinery, as it did before this check.
        if !SERIALIZABLE.contains(&iface.as_str()) && iface != "MessagePort" {
            refuse_platform(scope, &iface, ctx);
            return false;
        }
        return true;
    }
    let items: Vec<v8::Local<v8::Value>> = if let Ok(m) = v8::Local::<v8::Map>::try_from(v) {
        let a = m.as_array(scope);
        (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect()
    } else if let Ok(s) = v8::Local::<v8::Set>::try_from(v) {
        let a = s.as_array(scope);
        (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect()
    } else {
        let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return true };
        let args = v8::GetPropertyNamesArgsBuilder::new()
            .mode(v8::KeyCollectionMode::OwnOnly)
            .property_filter(v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS)
            .index_filter(v8::IndexFilter::IncludeIndices)
            .key_conversion(v8::KeyConversionMode::ConvertToString)
            .build();
        match o.get_own_property_names(scope, args) {
            Some(keys) => (0..keys.length()).filter_map(|i| keys.get_index(scope, i)).filter_map(|k| o.get(scope, k)).collect(),
            None => Vec::new(),
        }
    };
    items.into_iter().all(|x| walk_refusing(scope, x, seen, depth + 1, ctx))
}

/// A generic object: the PROTOTYPE is preserved, so a platform object clones as its own type and
/// survives `instanceof` — which is what makes this not a JSON round-trip.
fn clone_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    seen: v8::Local<'s, v8::Map>,
    depth: u32,
    ctx: &CloneCtx,
) -> Option<v8::Local<'s, v8::Value>> {
    let src = v8::Local::<v8::Object>::try_from(v).ok()?;
    // A `[Serializable]` platform object is a NEW object of its interface built from its values
    // (HTML's serialization steps): copying own properties onto a minted object left a DOMPoint whose `x`
    // threw, an empty DOMException, a 0-byte Blob.
    if let Some(iface) = platform_interface(scope, v) {
        if let Some(st) = crate::modules::clone_state::read(scope, src, &iface) {
            let out = crate::modules::clone_state::build(scope, &st)?;
            seen.set(scope, v, out.into());
            return Some(out.into());
        }
    }
    let mut out = None;
    // Deserialized in the CURRENT realm (a message to a frame is cloned inside the frame's
    // context): an interface prototype becomes that realm's own, and a plain object keeps the
    // `Object.prototype` it was born with here -- the source's is ANOTHER realm's, recognisable as the
    // one prototype whose own prototype is null.
    if let Some(p) = src.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok()) {
        let ctx = scope.get_current_context();
        let here = crate::binding::protos::proto_in_realm(scope, ctx, p);
        let is_object_proto = here == p
            && crate::binding::protos::iface_name_of(scope, p).is_none()
            && p.get_prototype(scope).is_some_and(|pp| pp.is_null());
        if !is_object_proto {
            // A clone of a platform object is one, made from its interface's template --
            // unless that template carries interceptors, which answer only for their own family's objects
            // (`install::instance_shapes`; no browser clones a list or a declaration): that clone stays the
            // ordinary object it was. Either way it carries the SOURCE's brand, and a clone of a fake is not
            // one.
            let branded = crate::binding::brand::brand_of(scope, src.into()).is_some();
            let shaped = shaped_interface(scope, here);
            let o = if branded && !shaped {
                crate::binding::protos::mint_on(scope, here.into())
            } else {
                let o = v8::Object::new(scope);
                o.set_prototype(scope, here.into());
                o
            };
            crate::binding::brand::copy_brand(scope, src.into(), o);
            out = Some(o);
        }
    }
    let out = out.unwrap_or_else(|| v8::Object::new(scope));
    seen.set(scope, v, out.into());
    // Own ENUMERABLE string keys, defined rather than assigned: a slot-backed source (ImageData's own
    // `data`) can have a same-named getter-only accessor on its own prototype, which the clone shares,
    // and a plain assignment would hit that accessor and throw before the clone owned anything.
    let args = v8::GetPropertyNamesArgsBuilder::new()
        .mode(v8::KeyCollectionMode::OwnOnly)
        .property_filter(v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS)
        .index_filter(v8::IndexFilter::IncludeIndices)
        .key_conversion(v8::KeyConversionMode::ConvertToString)
        .build();
    // A PLAIN object's properties are CreateDataProperty's -- writable, enumerable, configurable -- whatever
    // the source's were (the clone of a frozen object was read-only, and wix.com's strict footer
    // code threw assigning to it). Only a platform object's clone keeps a member's own writability: its
    // instance shape (an ImageData's own `data`) is part of what it is.
    let platform = crate::binding::brand::brand_of(scope, src.into()).is_some();
    if let Some(keys) = src.get_own_property_names(scope, args) {
        for i in 0..keys.length() {
            let Some(k) = keys.get_index(scope, i) else { continue };
            let Some(val) = src.get(scope, k) else { continue };
            let c = clone_value(scope, val, seen, depth + 1, ctx)?;
            let Ok(kname) = v8::Local::<v8::Name>::try_from(k) else { continue };
            let writable = !platform
                || src
                    .get_own_property_descriptor(scope, kname)
                    .and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())
                    .and_then(|d| get_prop(scope, d, "writable"))
                    .map(|w| w.boolean_value(scope))
                    .unwrap_or(true);
            let mut pd = v8::PropertyDescriptor::new_from_value_writable(c, writable);
            pd.set_enumerable(true);
            pd.set_configurable(true);
            out.define_property(scope, kname, &pd);
        }
    }
    copy_slot_backed_members(scope, src, out, seen, depth, ctx);
    Some(out.into())
}

/// Whether the interface `proto` belongs to -- its own, or the first up its chain, as a mint finds it --
/// carries an instance shape (`install::instance_shapes`).
fn shaped_interface(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>) -> bool {
    let mut cur = Some(proto);
    for _ in 0..16 {
        let Some(c) = cur else { break };
        if let Some(n) = crate::binding::protos::iface_name_of(scope, c) {
            return crate::install::instance_shapes::has_shape(&n.to_rust_string_lossy(scope));
        }
        cur = c.get_prototype(scope).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    }
    false
}

/// A slot-backed platform object (`ImageData`, `Response`, `MessageEvent`, `DOMPoint`, …) keeps its
/// state behind v8 Privates, invisible to own-key reflection. What IS visible is the ACCESSOR the
/// table put on its prototype, so read every inherited getter's value off the source and write it into
/// the clone's own slot store — and STAMP THE CLONE'S BRAND, which is the part only Rust can do.
///
/// Own properties copied above always win (a getter here never overwrites one), so a real own property
/// like `ImageData`'s `data` is untouched.
fn copy_slot_backed_members<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    src: v8::Local<'s, v8::Object>,
    out: v8::Local<'s, v8::Object>,
    seen: v8::Local<'s, v8::Map>,
    depth: u32,
    ctx: &CloneCtx,
) {
    let Some(object_proto) = crate::binding::protos::intrinsic(scope, "Object.prototype")
        .and_then(|p| v8::Local::<v8::Object>::try_from(p).ok())
    else {
        return;
    };
    let mut cur = src.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
    while let Some(proto) = cur {
        if proto == object_proto {
            break;
        }
        // The interface this prototype belongs to, from its own `constructor`: that is the brand the
        // clone needs so the table's branded rows accept it.
        if let Some(name) = get_prop(scope, proto, "constructor")
            .and_then(|c| v8::Local::<v8::Function>::try_from(c).ok())
            .map(|c| c.get_name(scope).to_rust_string_lossy(scope))
            .filter(|n| !n.is_empty())
        {
            crate::install::webidl::brand(scope, out, &name);
        }
        let args = v8::GetPropertyNamesArgsBuilder::new()
            .mode(v8::KeyCollectionMode::OwnOnly)
            .property_filter(v8::PropertyFilter::SKIP_SYMBOLS)
            .index_filter(v8::IndexFilter::IncludeIndices)
            .build();
        let names: Vec<v8::Local<v8::Value>> = match proto.get_own_property_names(scope, args) {
            Some(k) => (0..k.length()).filter_map(|i| k.get_index(scope, i)).collect(),
            None => Vec::new(),
        };
        for key in names {
            let name = key.to_rust_string_lossy(scope);
            if name == "constructor" {
                continue;
            }
            let Ok(kname) = v8::Local::<v8::Name>::try_from(key) else { continue };
            if out.has_own_property(scope, kname).unwrap_or(false) {
                continue;
            }
            let Some(getter) = proto
                .get_own_property_descriptor(scope, kname)
                .and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())
                .and_then(|d| get_prop(scope, d, "get"))
                .and_then(|g| v8::Local::<v8::Function>::try_from(g).ok())
            else {
                continue;
            };
            // A getter that throws for this receiver, or answers `undefined`, contributes nothing --
            // the JS did the same, with a `try`/`continue`.
            let srcv: v8::Local<v8::Value> = src.into();
            let value = {
                v8::tc_scope!(let tc, scope);
                let r = getter.call(tc, srcv, &[]).map(|x| v8::Global::new(tc, x));
                tc.reset();
                r
            };
            let Some(value) = value else { continue };
            let value = v8::Local::new(scope, &value);
            if value.is_undefined() {
                continue;
            }
            let Some(c) = clone_value(scope, value, seen, depth + 1, ctx) else { continue };
            crate::core::events::event::evt_slot_write(scope, out, &name, c);
        }
        cur = proto.get_prototype(scope).and_then(|p| v8::Local::<v8::Object>::try_from(p).ok());
    }
}

/// Install `globalThis.structuredClone` — a writable, enumerable, configurable data property, which is
/// the descriptor Chrome has.
pub fn install_structured_clone<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    let f = crate::binding::protos::build_fn(scope, cb_structured_clone, "structuredClone", 1);
    // The body does its own WebIDL argument check (`structuredClone` is NOT promise-returning, so it
    // throws), so the `generated_order::WINQ` arity pass would only have wrapped an identical check
    // around it -- naming 'Window' in a worker. That pass is deleted; the mark records the fact.
    crate::binding::protos::stamp_arity_guarded(scope, f);
    let Some(key) = v8::String::new(scope, "structuredClone") else { return };
    let mut pd = v8::PropertyDescriptor::new_from_value_writable(f.into(), true);
    pd.set_enumerable(true);
    pd.set_configurable(true);
    global.define_property(scope, key.into(), &pd);
}
