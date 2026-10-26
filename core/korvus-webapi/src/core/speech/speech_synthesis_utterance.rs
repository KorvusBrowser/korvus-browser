//! `core/speech/speech_synthesis_utterance.rs` — `SpeechSynthesisUtterance` (WHATWG Speech API): a
//! plain settable value object. Korvus has no TTS engine (no audio pipeline, no installed voice
//! list), so `SpeechSynthesis.speak()`/`getVoices()` have nowhere honest to go and stay untouched --
//! but the utterance itself is just spec-defined state, which IS honest to implement in full:
//! text/lang default to "" (text takes the constructor's optional argument), voice defaults to
//! null, volume/rate/pitch default to 1. The seven `on<type>` handlers (start/end/error/pause/
//! resume/mark/boundary) are wired in dom.js onto the existing generic EventTarget on<type>
//! accessor (op_et_on_getter/op_et_on_setter) -- genuine property storage, though nothing will ever
//! fire them without a synthesis engine to drive dispatch.

use deno_core::{v8, CppgcBase};
use macros::webapi;
use crate::binding::protos::{str_val, throw_type_error};

#[repr(C)]
#[derive(CppgcBase)]
pub struct SpeechSynthesisUtterance {
    text: std::cell::RefCell<String>,
    lang: std::cell::RefCell<String>,
    // `attribute SpeechSynthesisVoice? voice` -- stored as an opaque JS reference. Korvus has no
    // SpeechSynthesisVoice class to validate assignments against (no voice list exists), so unlike a
    // real binding this does NOT reject a non-voice object; a comment at the setter says so. null by
    // default.
    voice: std::cell::RefCell<Option<v8::Global<v8::Value>>>,
    // WebIDL `attribute float` (the RESTRICTED float type, not `unrestricted float`): stored at
    // IEEE-754 single precision like a real generated binding, so reading back a value that isn't
    // exactly representable in f32 shows the same rounding a browser shows; setting NaN/Infinity
    // throws (see ssu_set_float below), matching the spec's float conversion algorithm.
    volume: std::cell::Cell<f32>,
    rate: std::cell::Cell<f32>,
    pitch: std::cell::Cell<f32>,
}

pub(crate) trait SpeechSynthesisUtteranceBehavior {
    fn construct(_text: Option<String>) -> SpeechSynthesisUtterance where Self: Sized { stub!("SpeechSynthesisUtterance::constructor") }
}

#[webapi(base, own)]
impl SpeechSynthesisUtterance {
    #[constructor]
    #[cppgc]
    fn constructor(#[string] text: Option<String>) -> Result<SpeechSynthesisUtterance, ::deno_error::JsErrorBox> {
        dispatch!("SpeechSynthesisUtterance::constructor", <SpeechSynthesisUtterance as SpeechSynthesisUtteranceBehavior>::construct(text))
    }
}

impl SpeechSynthesisUtteranceBehavior for SpeechSynthesisUtterance {
    fn construct(text: Option<String>) -> SpeechSynthesisUtterance {
        SpeechSynthesisUtterance {
            text: std::cell::RefCell::new(text.unwrap_or_default()),
            lang: std::cell::RefCell::new(String::new()),
            voice: std::cell::RefCell::new(None),
            volume: std::cell::Cell::new(1.0),
            rate: std::cell::Cell::new(1.0),
            pitch: std::cell::Cell::new(1.0),
        }
    }
}

// ---- text / lang: plain DOMString (ToString coercion, no restriction on content) ----
pub(crate) fn cb_ssu_text_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) {
        let s = unsafe { u.as_ref() }.text.borrow().clone();
        rv.set(str_val(scope, &s));
    }
}
pub(crate) fn cb_ssu_text_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = args.get(0).to_rust_string_lossy(scope);
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { *unsafe { u.as_ref() }.text.borrow_mut() = s; }
}

pub(crate) fn cb_ssu_lang_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) {
        let s = unsafe { u.as_ref() }.lang.borrow().clone();
        rv.set(str_val(scope, &s));
    }
}
pub(crate) fn cb_ssu_lang_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let s = args.get(0).to_rust_string_lossy(scope);
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { *unsafe { u.as_ref() }.lang.borrow_mut() = s; }
}

// ---- voice: nullable object reference, stored verbatim (no SpeechSynthesisVoice class exists in
// Korvus to type-check assignments against -- see the field comment above). ----
pub(crate) fn cb_ssu_voice_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) {
        match unsafe { u.as_ref() }.voice.borrow().as_ref() {
            Some(g) => rv.set(v8::Local::new(scope, g)),
            None => rv.set_null(),
        }
    }
}
pub(crate) fn cb_ssu_voice_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let v = args.get(0);
    let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) else { return };
    let mut slot = unsafe { u.as_ref() }.voice.borrow_mut();
    *slot = if v.is_null_or_undefined() { None } else { Some(v8::Global::new(scope, v)) };
}

// ---- volume / rate / pitch: WebIDL `float` -- ToNumber(V), then the value MUST be finite (the
// restricted float conversion), else a TypeError (Blink's exact generated-binding wording). Raw
// callbacks can throw; #[op2] setters cannot, which is why these stay hand-written. Returns `None`
// (having already thrown) on a non-finite value, so the caller just bails without writing.
fn ssu_float_value(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, prop: &str) -> Option<f32> {
    let n = args.get(0).number_value(scope).unwrap_or(f64::NAN);
    let f = n as f32;
    if !f.is_finite() {
        throw_type_error(scope, &format!("Failed to set the '{prop}' property on 'SpeechSynthesisUtterance': The provided float value is non-finite."));
        return None;
    }
    Some(f)
}

pub(crate) fn cb_ssu_volume_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { rv.set(v8::Number::new(scope, unsafe { u.as_ref() }.volume.get() as f64).into()); }
}
pub(crate) fn cb_ssu_volume_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(f) = ssu_float_value(scope, &args, "volume") else { return };
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { unsafe { u.as_ref() }.volume.set(f); }
}

pub(crate) fn cb_ssu_rate_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { rv.set(v8::Number::new(scope, unsafe { u.as_ref() }.rate.get() as f64).into()); }
}
pub(crate) fn cb_ssu_rate_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(f) = ssu_float_value(scope, &args, "rate") else { return };
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { unsafe { u.as_ref() }.rate.set(f); }
}

pub(crate) fn cb_ssu_pitch_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { rv.set(v8::Number::new(scope, unsafe { u.as_ref() }.pitch.get() as f64).into()); }
}
pub(crate) fn cb_ssu_pitch_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(f) = ssu_float_value(scope, &args, "pitch") else { return };
    if let Some(u) = deno_core::cppgc::try_unwrap_cppgc_object::<SpeechSynthesisUtterance>(scope, args.this().into()) { unsafe { u.as_ref() }.pitch.set(f); }
}

// This file registers no `op_*`: the six attribute callbacks are named directly by the
// `SpeechSynthesisUtterance` table, so nothing needs a JS-facing builder.


// ── the table ───────────────────────────────────────────────────────────────
//
// `dom/08n-speech.js` (40 lines: an `Object.setPrototypeOf` onto EventTarget, one
// `Object.defineProperty` shape over the six attributes, a second over the seven handlers, and
// `globalThis.SpeechSynthesisUtterance = SSU`) is gone. The thirteen members are the rows below,
// mounted by THE installer (`install::live::install_window_small_elements`) with the chain to `EventTarget` as
// the installer's WebIDL splice.
//
// Names, order, kinds, nullability and descriptor flags are VERBATIM from the
// `SpeechSynthesisUtterance` block of `install/generated_registry.rs`; the seven
// `SpeechSynthesisEventHandler` rows take the shared `event_handler` kit the generated rows already
// named -- the same private-slot storage every other `on<type>` in this engine uses, genuine
// storage, `null` until assigned, though nothing will fire them without a synthesis engine.
//
// The interface object is a native constructor over [`cb_ssu_ctor`], not the raw cppgc class
// template `dom/00-bootstrap.js` used to publish: an instance keeps its cppgc identity (the unwrap
// reads the wrapper tag, never the prototype chain) and gains the prototype the page can see, which
// is the shape W5.1 L8 gave `DataTransfer`.

/// `new SpeechSynthesisUtterance(text?)` -- the WebIDL constructor, minting the cppgc value on the
/// table's prototype (or on a subclass's, when `new.target` is one).
pub fn cb_ssu_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    use crate::binding::protos::get_prop;
    if !args.new_target().is_object() {
        throw_type_error(scope, &crate::binding::protos::use_new_msg("SpeechSynthesisUtterance"));
        return;
    }
    let text = if args.length() > 0 && !args.get(0).is_undefined() {
        Some(args.get(0).to_rust_string_lossy(scope))
    } else {
        None
    };
    let obj = crate::binding::class::mint(
        scope,
        <SpeechSynthesisUtterance as SpeechSynthesisUtteranceBehavior>::construct(text),
    );
    let proto = v8::Local::<v8::Object>::try_from(args.new_target())
        .ok()
        .and_then(|nt| get_prop(scope, nt, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok());
    match proto {
        Some(p) => {
            obj.set_prototype(scope, p.into());
        }
        None => crate::install::live::set_table_proto(scope, obj, "SpeechSynthesisUtterance"),
    }
    rv.set(obj.into());
}

pub fn install_speech_synthesis_utterance(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static crate::install::MemberDef) {
    use crate::core::html::html_input_element::acc2;
    match member.name {
        "text" => acc2(scope, proto, member, cb_ssu_text_get, None, cb_ssu_text_set, None),
        "lang" => acc2(scope, proto, member, cb_ssu_lang_get, None, cb_ssu_lang_set, None),
        "voice" => acc2(scope, proto, member, cb_ssu_voice_get, None, cb_ssu_voice_set, None),
        "volume" => acc2(scope, proto, member, cb_ssu_volume_get, None, cb_ssu_volume_set, None),
        "rate" => acc2(scope, proto, member, cb_ssu_rate_get, None, cb_ssu_rate_set, None),
        "pitch" => acc2(scope, proto, member, cb_ssu_pitch_get, None, cb_ssu_pitch_set, None),
        other => debug_assert!(false, "install_speech_synthesis_utterance: unexpected member {other}"),
    }
}

webapi::interface! {
    "SpeechSynthesisUtterance" : "EventTarget" [value, window] {
        ctor    "SpeechSynthesisUtterance"      len 0            = stub,
        attr    "text"   : Object                                = install_speech_synthesis_utterance,
        attr    "lang"   : Object                                = install_speech_synthesis_utterance,
        attr    "voice"  : Object nullable                       = install_speech_synthesis_utterance,
        attr    "volume" : Object                                = install_speech_synthesis_utterance,
        attr    "rate"   : Object                                = install_speech_synthesis_utterance,
        attr    "pitch"  : Object                                = install_speech_synthesis_utterance,
        handler "onstart"    nullable = event_handler,
        handler "onend"      nullable = event_handler,
        handler "onerror"    nullable = event_handler,
        handler "onpause"    nullable = event_handler,
        handler "onresume"   nullable = event_handler,
        handler "onmark"     nullable = event_handler,
        handler "onboundary" nullable = event_handler
    }
}
