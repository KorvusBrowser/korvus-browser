//! `core/messaging/transfer.rs` — ONE implementation of a `postMessage` transfer list, for every
//! `postMessage` in this engine.
//!
//! # What was wrong
//!
//! `MessagePort.postMessage` read `args.get(0)` and nothing else, so a transfer list was silently
//! dropped and `event.ports` was always empty. The two `Window.postMessage` bodies did read one, but
//! passed the caller's own array through as `ports` — so the receiver got the SENDER's port objects,
//! still entangled, still live at the sender, and no port was ever neutered. Neither validated
//! anything: a duplicate, a neutered port, a detached buffer and a plain object all went through.
//!
//! The observable cost of the first is one of the largest live clusters this goal has: a page does
//! `e = b.ports[0]; e.postMessage(…)` and reads `Cannot read properties of undefined (reading
//! 'postMessage')`.
//!
//! # The rule, and why it is here and not in three places
//!
//! HTML's StructuredSerializeWithTransfer is one algorithm and its error sentences are one set. The
//! interface name in them is the only thing that varies, so it is a parameter. This file owns:
//!
//!   1. **Reading** the list — WebIDL overload resolution, which is not the same for `MessagePort`
//!      (`(any, sequence<object>)` vs `(any, optional StructuredSerializeOptions)`) as for `Window`
//!      (the second positional argument is `targetOrigin`, a string).
//!   2. **Validating** it, whole, BEFORE anything is serialized or neutered. Chrome does the same,
//!      and it is observable twice over: an unclonable message with a bad transfer list reports the
//!      LIST's error, and a call that throws leaves every port named in it still usable.
//!   3. **Performing** it: a transferred port is replaced by a FRESH port entangled with the old
//!      one's peer, carrying the old one's queued messages, and the old one is neutered.
//!
//! # What a transferred port is, in this engine's model
//!
//! A `MessagePort` holds `other` (the same-isolate peer), `remote` (a cross-isolate channel id), a
//! message `queue` and `started`. Transferring it same-agent means: mint a new port, move `other`
//! (re-pointing the peer back at the new one), move `remote`'s registration, move the queue, and set
//! `detached` on the old one. `detached` is a FIELD and not `other.is_none()`, because a port
//! reconstructed for a cross-isolate channel has no `other` and is perfectly alive, and because
//! `close()` leaves a port transferable (measured: Chrome takes a closed port in a transfer list).
//!
//! # Which types are transferable here, and the gap that leaves
//!
//! `MessagePort` and `ArrayBuffer`. Chrome's `[Transferable]` set is larger — `ImageBitmap`,
//! `OffscreenCanvas`, `ReadableStream`/`WritableStream`/`TransformStream`, `MediaSourceHandle`,
//! `RTCDataChannel` — and each of those gets `Value at index i does not have a transferable type.`
//! here, where Chrome takes it. That is a LOUDER wrong answer than the silence it replaces, and it
//! is deliberate for the same reason the cross-isolate refusal is: the caller learns at the call
//! site instead of two turns later. Each of those types becomes one more arm of [`validate`] on the
//! day its transfer semantics exist in this engine; none of them is faked in the meantime.
//! (`Worker.postMessage`'s own list is `korvus-runtime`'s `apply_transfer` and is untouched by
//! this file — it detaches buffers and re-homes ports through the worker port plane.)
//!
//! # The cross-isolate path is NOT here
//!
//! A port whose peer lives in another isolate routes its posts out through `PortForward`, and that
//! frame format (`[u32 channel_id][bytes]`, `korvus-runtime`'s `port_encode`) has no slot for
//! transferred ports, nor has a worker isolate any allocator for a channel id — main owns that
//! counter. Transferring a port THROUGH such a port is therefore refused loudly rather than dropped
//! silently: [`throw_remote_unsupported`] is the one place that says so, its two call-site
//! behaviours are pinned by `korvus-runtime`'s `worker.rs` tests
//! (`a_port_is_refused_through_a_cross_isolate_port`,
//! `the_page_side_of_a_worker_channel_refuses_a_port_too`), and MP1's report says it in words.
//! A BUFFER in that list is not refused — it rides the frame perfectly well, and the same two tests
//! prove that the refusal is about ports and not about transfer lists.
//!
//! The refusal is loud on purpose, and the unit contract's own rule decides it: a silent drop is
//! "a shape a caller can reach through" — the page posts a port, is told nothing, and reads
//! `undefined.postMessage` a turn later, which is the exact cluster this file exists for.

use deno_core::v8;

use crate::binding::protos::{get_prop, throw_dom, throw_type_error};

use super::message_port::{is_message_port, mp_adopt_transferred, mp_is_detached};

/// One validated entry of a transfer list, in the caller's order.
pub(crate) enum Transferable<'s> {
    /// A `MessagePort`. Neutered and replaced by a fresh entangled port when the transfer is taken.
    Port(v8::Local<'s, v8::Value>),
    /// An `ArrayBuffer`. Detached AFTER the message has been serialized — the bytes are copied into
    /// the message first, which is what gives the observable `byteLength -> 0` at the source.
    Buffer(v8::Local<'s, v8::ArrayBuffer>),
}

/// A read-and-validated transfer list. Empty is the overwhelmingly common case and costs nothing.
pub(crate) struct TransferList<'s>(Vec<Transferable<'s>>);

impl<'s> TransferList<'s> {
    /// Does the list carry at least one `MessagePort`? The cross-isolate send path asks, because a
    /// buffer transfer rides it fine and only a port cannot.
    pub(crate) fn has_ports(&self) -> bool {
        self.0.iter().any(|e| matches!(e, Transferable::Port(_)))
    }
}

/// `sequence<object>`: WebIDL iterates the value, so `Array.from` is the conversion — it runs the
/// iterator protocol, and a value with no `Symbol.iterator` is a `TypeError` from the caller's
/// perspective, which is what the options dictionary's conversion (`ctor_dicts::convert_dict`) turns into Chrome's sentence.
fn to_sequence<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Array>> {
    // A REAL array is read by index and never through `Array.from`, because `Array.from` is a page
    // property: a site that replaces it would otherwise change what `postMessage` transfers. Blink
    // has the same fast path for a JSArray, so this is its behaviour and not a shortcut past it.
    if let Ok(a) = v8::Local::<v8::Array>::try_from(v) {
        return Some(a);
    }
    // The realm's own `Array.from`.
    let from = crate::binding::protos::intrinsic_fn(scope, "Array.from")?;
    let recv = crate::binding::protos::intrinsic(scope, "Array")?;
    let out = {
        v8::tc_scope!(let tc, scope);
        let r = from.call(tc, recv, &[v]);
        tc.reset(); // the caller re-throws with Chrome's own sentence
        r
    };
    out.and_then(|o| v8::Local::<v8::Array>::try_from(o).ok())
}

/// Is `v` iterable — i.e. does WebIDL's overload resolution route it to `sequence<object>` rather
/// than to the options dictionary? ("an object with a callable `Symbol.iterator`".)
fn is_iterable(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> bool {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return false };
    let key = v8::Symbol::get_iterator(scope);
    o.get(scope, key.into()).map(|f| f.is_function()).unwrap_or(false)
}

/// `MessagePort.postMessage(message, transfer)` / `(message, options)` — WebIDL overload resolution
/// over the SECOND argument, then the validated list.
///
/// `undefined`/`null` and an absent argument are the options overload with no transfer; an iterable
/// is the sequence; a plain object is the dictionary; anything else fails overload resolution.
pub(crate) fn read_port_transfer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &v8::FunctionCallbackArguments<'s>,
    source_port: v8::Local<'s, v8::Value>,
) -> Option<TransferList<'s>> {
    let seq = if args.length() < 2 {
        None
    } else {
        let v = args.get(1);
        if v.is_undefined() || v.is_null() {
            None
        } else if is_iterable(scope, v) {
            match to_sequence(scope, v) {
                Some(a) => Some(a),
                None => {
                    throw_type_error(scope, "Failed to execute 'postMessage' on 'MessagePort': Overload resolution failed.");
                    return None;
                }
            }
        } else if v.is_object() {
            // `PostMessageOptions`, converted as WebIDL converts it: `transfer` (a sequence, checked as one), then
            // `includeUserActivation` (korvus read `transfer` alone)
            let Ok(d) = crate::install::ctor_dicts::convert_dict(scope, v, "PostMessageOptions", "Failed to execute 'postMessage' on 'MessagePort': ") else { return None };
            get_prop(scope, d, "transfer").and_then(|t| v8::Local::<v8::Array>::try_from(t).ok()).filter(|t| t.length() > 0)
        } else {
            throw_type_error(scope, "Failed to execute 'postMessage' on 'MessagePort': Overload resolution failed.");
            return None;
        }
    };
    validate(scope, seq, "MessagePort", Some(source_port))
}

/// `Window.postMessage`'s transfer list: the third positional argument, or the options dictionary's
/// `transfer` member — the caller has already done that overload resolution, because its second
/// argument is `targetOrigin` and only it can tell the two apart.
pub(crate) fn read_window_transfer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    transfer: Option<v8::Local<'s, v8::Value>>,
    from_options: bool,
) -> Option<TransferList<'s>> {
    let seq = match transfer.filter(|t| !t.is_undefined() && !t.is_null()) {
        None => None,
        Some(t) => match is_iterable(scope, t).then(|| to_sequence(scope, t)).flatten() {
            Some(a) => Some(a),
            None => {
                if from_options {
                    // `StructuredSerializeOptions`, not `WindowPostMessageOptions`: `transfer` is
                    // INHERITED, and Chrome names the dictionary that declares the member.
                    throw_type_error(
                        scope,
                        "Failed to execute 'postMessage' on 'Window': Failed to read the 'transfer' property from 'StructuredSerializeOptions': The provided value cannot be converted to a sequence.",
                    );
                } else {
                    throw_type_error(
                        scope,
                        "Failed to execute 'postMessage' on 'Window': The provided value cannot be converted to a sequence.",
                    );
                }
                return None;
            }
        },
    };
    validate(scope, seq, "Window", None)
}

/// The whole list, checked before anything is serialized or neutered, in Chrome's order: per index,
/// the type and then the state; the "source port" check runs afterwards over the whole list, which
/// is why a list holding the receiver TWICE reports the duplicate and not the source.
fn validate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    seq: Option<v8::Local<'s, v8::Array>>,
    iface: &str,
    source_port: Option<v8::Local<'s, v8::Value>>,
) -> Option<TransferList<'s>> {
    let Some(arr) = seq else { return Some(TransferList(Vec::new())) };
    let mut out: Vec<Transferable<'s>> = Vec::with_capacity(arr.length() as usize);
    let mut seen: Vec<v8::Local<'s, v8::Value>> = Vec::with_capacity(arr.length() as usize);
    for i in 0..arr.length() {
        let Some(item) = arr.get_index(scope, i) else { continue };
        if !item.is_object() {
            throw_type_error(scope, &format!("Failed to execute 'postMessage' on '{iface}': Failed to convert value to 'object'."));
            return None;
        }
        let dup = seen.iter().any(|p| p.strict_equals(item));
        if is_message_port(scope, item) {
            if mp_is_detached(scope, item) {
                throw_dom(scope, &format!("Failed to execute 'postMessage' on '{iface}': Port at index {i} is already neutered."), "DataCloneError");
                return None;
            }
            if dup {
                throw_dom(scope, &format!("Failed to execute 'postMessage' on '{iface}': Message port at index {i} is a duplicate of an earlier port."), "DataCloneError");
                return None;
            }
            seen.push(item);
            out.push(Transferable::Port(item));
        } else if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(item) {
            if ab.was_detached() {
                throw_dom(scope, &format!("Failed to execute 'postMessage' on '{iface}': ArrayBuffer at index {i} is already detached."), "DataCloneError");
                return None;
            }
            if dup {
                throw_dom(scope, &format!("Failed to execute 'postMessage' on '{iface}': ArrayBuffer at index {i} is a duplicate of an earlier ArrayBuffer."), "DataCloneError");
                return None;
            }
            seen.push(item);
            out.push(Transferable::Buffer(ab));
        } else {
            throw_dom(scope, &format!("Failed to execute 'postMessage' on '{iface}': Value at index {i} does not have a transferable type."), "DataCloneError");
            return None;
        }
    }
    if let Some(src) = source_port {
        for (i, e) in out.iter().enumerate() {
            if let Transferable::Port(p) = e {
                if p.strict_equals(src) {
                    throw_dom(scope, &format!("Failed to execute 'postMessage' on '{iface}': Port at index {i} contains the source port."), "DataCloneError");
                    return None;
                }
            }
        }
    }
    Some(TransferList(out))
}

/// Perform the transfer and answer the `ports` array the receiving `MessageEvent` carries: one FRESH
/// port per transferred port, entangled with the old one's peer and holding the old one's queued
/// messages, with the old one neutered.
///
/// Called only after the message has been serialized, so a `DataCloneError` leaves every port in the
/// list alive — which is observable, and which Chrome does too.
///
/// Each fresh port is minted in `realm`, the RECEIVER's (`None`: the current realm): HTML deserializes a
/// transferred port in the realm the message is delivered to, so a frame's `e.ports[0] instanceof MessagePort`
/// holds there. Minted in the caller's realm, a port posted into a sandboxed frame was the page's object
/// (a bot-detection script's ri frame; Chrome 150 measured).
pub(crate) fn take_ports<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    list: &TransferList<'s>,
    realm: Option<v8::Local<'s, v8::Context>>,
) -> v8::Local<'s, v8::Array> {
    let n = list.0.iter().filter(|e| matches!(e, Transferable::Port(_))).count();
    let ports = v8::Array::new(scope, n as i32);
    let mut at = 0u32;
    for e in &list.0 {
        if let Transferable::Port(p) = e {
            let fresh = mp_adopt_transferred(scope, *p, realm);
            ports.set_index(scope, at, fresh);
            at += 1;
        }
    }
    ports
}

/// The ports the list transfers, as the page passed them: a structured clone hands them through as they are,
/// and [`take_ports_mapped`] then says what each became.
pub(crate) fn port_originals<'s>(list: &TransferList<'s>) -> Vec<v8::Local<'s, v8::Value>> {
    list.0.iter().filter_map(|e| if let Transferable::Port(p) = e { Some(*p) } else { None }).collect()
}

/// [`take_ports`], and each transferred port paired with the fresh port it became -- for the clone's
/// references to it (`postMessage({port}, '*', [port])`: `e.data.port === e.ports[0]`).
pub(crate) fn take_ports_mapped<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    list: &TransferList<'s>,
    realm: Option<v8::Local<'s, v8::Context>>,
) -> (v8::Local<'s, v8::Array>, Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)>) {
    let originals = port_originals(list);
    let ports = take_ports(scope, list, realm);
    let pairs = originals.into_iter().enumerate().filter_map(|(i, o)| ports.get_index(scope, i as u32).map(|f| (o, f))).collect();
    (ports, pairs)
}

/// Detach every `ArrayBuffer` in the list. AFTER serialization: the bytes are already in the
/// message, so detaching here is what makes the source's `byteLength` 0 and the receiver's whole.
pub(crate) fn detach_buffers(scope: &mut v8::PinScope, list: &TransferList) {
    let _ = scope;
    for e in &list.0 {
        if let Transferable::Buffer(ab) = e {
            if ab.is_detachable() {
                let _ = ab.detach(None);
            }
        }
    }
}

/// The `DataCloneError` a CROSS-ISOLATE port transfer is refused with. Named, and with a sentence
/// that says what is not supported, so the one call site reads as a refusal rather than as an
/// omission — this file's header says why the frame format cannot carry it.
pub(crate) fn throw_remote_unsupported(scope: &mut v8::PinScope) {
    throw_dom(
        scope,
        "Failed to execute 'postMessage' on 'MessagePort': a MessagePort cannot be transferred through a port whose other end is in another agent.",
        "DataCloneError",
    );
}
