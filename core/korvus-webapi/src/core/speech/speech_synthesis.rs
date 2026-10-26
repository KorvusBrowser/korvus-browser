//! `core/speech/speech_synthesis.rs` -- the `SpeechSynthesis` interface and the `speechSynthesis`
//! singleton.
//!
//! WHAT THIS REPLACES. The `window.speechSynthesis` block of `dom/09i-singletons.js`: six
//! `Object.defineProperty` sites over the generated shell's prototype, plus the own accessor that
//! published the singleton.
//!
//! `[SameObject] readonly attribute SpeechSynthesis speechSynthesis` on Window was entirely ABSENT
//! here before that block -- `speechSynthesis` threw a bare ReferenceError, which is louder than any
//! shape mismatch, and the capture lists `speechSynthesis` among the window's globals.
//!
//! THE ANSWERS are what real Brave on this machine gives with no installed voice pack, headless:
//! `pending` / `speaking` / `paused` false and `getVoices()` `[]`. `onvoiceschanged` gets the same
//! event-handler store `SpeechSynthesisUtterance`'s table gives its seven handlers -- a genuine
//! get/set -- and hears the ONE `voiceschanged` the voice list's arrival fires (below).
//!
//! `speak` / `cancel` / `pause` / `resume` are honest D2 `stub` rows, not no-op bodies dressed up as
//! implementations: there is no synthesis engine here, so there is nothing for them to do, and a stub
//! row is what the ledger counts and `tools/status/data/layers.json` reports. Their observable behaviour is
//! unchanged -- all four are `undefined`-returning and the shell answered `undefined` too.

use deno_core::v8;

use crate::install::live::{getter_row, method_row};
use crate::install::MemberDef;
use crate::install::kits::branded;

fn cb_false(_scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set_bool(false);
}

/// `getVoices()` -- the persona's voice list once it has ARRIVED, `[]` before.
///
/// Measured headed on this machine (`speech voices` probe): stock Chrome 150 answers `[]` synchronously
/// and in the first task, fires ONE trusted `voiceschanged` ~8 ms later, and from then on lists its 19
/// Google voices; Brave 151 the same with an empty list; Edge 151 its 23 Microsoft voices. Each call is a
/// FRESH array (`getVoices() !== getVoices()`) of the SAME voice objects (`[0] === [0]`), which inherit
/// from `SpeechSynthesisVoice.prototype` and own nothing. korvus answered `[]` for every persona.
fn cb_get_voices(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let arrived = crate::core::html::window_globals::window_singleton(scope, "speechSynthesis")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .is_some_and(|o| crate::core::css::css_style_declaration::priv_get(scope, o, P_VOICES_ARRIVED).is_some());
    let voices = if arrived { voice_objects(scope) } else { Vec::new() };
    let a = v8::Array::new_with_elements(scope, &voices);
    rv.set(a.into());
}

/// Set on the singleton when the voice list has arrived (the task that fires `voiceschanged`).
const P_VOICES_ARRIVED: &str = "korvus_speech_voices_arrived";
/// The realm slot holding this realm's voice objects, minted once so their identity is stable.
const VOICES_SLOT: &str = "korvus_speech_voices";
/// A voice object's private slot holding its five values, in table order.
const P_VOICE: &str = "korvus_speech_voice";

/// This realm's `SpeechSynthesisVoice` objects, minted on first use from the persona's list.
fn voice_objects<'s>(scope: &mut v8::PinScope<'s, '_>) -> Vec<v8::Local<'s, v8::Value>> {
    use crate::binding::protos::{realm_slot, set_realm_slot};
    if let Some(arr) = realm_slot(scope, VOICES_SLOT).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        return (0..arr.length()).filter_map(|i| arr.get_index(scope, i)).collect();
    }
    let list = crate::core::navigator::navigator::persona(scope).map(|p| p.speech_voices()).unwrap_or_default();
    let Some(proto) = crate::binding::protos::shell_proto(scope, "SpeechSynthesisVoice") else { return Vec::new() };
    let mut out = Vec::new();
    for v in &list {
        let o = crate::binding::protos::mint_on(scope, proto.into());
        let vals: [v8::Local<v8::Value>; 5] = [
            crate::binding::protos::str_val(scope, &v.voice_uri),
            crate::binding::protos::str_val(scope, &v.name),
            crate::binding::protos::str_val(scope, &v.lang),
            v8::Boolean::new(scope, v.local_service).into(),
            v8::Boolean::new(scope, v.default).into(),
        ];
        let packed = v8::Array::new_with_elements(scope, &vals);
        crate::core::dom::element::priv_set(scope, o, P_VOICE, packed.into());
        out.push(o.into());
    }
    let arr = v8::Array::new_with_elements(scope, &out);
    set_realm_slot(scope, VOICES_SLOT, arr.into());
    out
}

/// One `SpeechSynthesisVoice` attribute: the value at `index` of the receiver's slot. A receiver that is
/// not a voice is `TypeError: Illegal invocation`, as Chrome's getters answer one.
fn voice_attr(scope: &mut v8::PinScope, args: &v8::FunctionCallbackArguments, rv: &mut v8::ReturnValue, index: u32) {
    if !branded(scope, args) {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    }
    let this = args.this();
    let Some(vals) = crate::core::css::css_style_declaration::priv_get(scope, this, P_VOICE).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else {
        crate::binding::protos::throw_type_error(scope, "Illegal invocation");
        return;
    };
    if let Some(v) = vals.get_index(scope, index) {
        rv.set(v);
    }
}

fn cb_voice_uri(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    voice_attr(scope, &args, &mut rv, 0);
}
fn cb_voice_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    voice_attr(scope, &args, &mut rv, 1);
}
fn cb_voice_lang(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    voice_attr(scope, &args, &mut rv, 2);
}
fn cb_voice_local(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    voice_attr(scope, &args, &mut rv, 3);
}
fn cb_voice_default(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    voice_attr(scope, &args, &mut rv, 4);
}

/// The five `SpeechSynthesisVoice` rows.
pub fn install_speech_voice(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::install::kits::brand_getter;
    match member.name {
        "voiceURI" => brand_getter(scope, proto, member, cb_voice_uri),
        "name" => brand_getter(scope, proto, member, cb_voice_name),
        "lang" => brand_getter(scope, proto, member, cb_voice_lang),
        "localService" => brand_getter(scope, proto, member, cb_voice_local),
        "default" => brand_getter(scope, proto, member, cb_voice_default),
        other => debug_assert!(false, "install_speech_voice: unexpected member {other}"),
    }
}

/// The body the four implemented rows of [`SPEECHSYNTHESIS`] name.
pub fn install_speech_synthesis(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "pending" | "speaking" | "paused" => getter_row(scope, proto, member, cb_false),
        "getVoices" => method_row(scope, proto, member, cb_get_voices),
        other => debug_assert!(false, "install_speech_synthesis: unexpected member {other}"),
    }
}

/// Build `SpeechSynthesis` from its table onto the real `EventTarget` chain and publish the
/// `speechSynthesis` singleton as an own accessor of the window.
///
/// OWN, on the window: `SpeechSynthesis` is `[Exposed=Window]` and `speechSynthesis` is a member of
/// the `[Global]` `Window` interface, which WebIDL makes an own property of the global object.
/// Headless Chrome 150.0.7871.46 on this machine answers
/// `Object.prototype.hasOwnProperty.call(window, "speechSynthesis")` TRUE (probed 2026-09-11, over a
/// loopback server), and `Object.prototype.toString.call(speechSynthesis)` `[object SpeechSynthesis]`.
pub fn install_window_speech_synthesis<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::install::installer::{self, Flavor};
    use crate::install::live::parent_of;
    let mut ledger = installer::take_ledger(scope);
    // `SpeechSynthesisVoice` first: `getVoices()` mints its instances on this realm's prototype.
    let voice = installer::create_interface(scope, global, &SPEECHSYNTHESISVOICE, "SpeechSynthesisVoice", None, None, Flavor::Table, &mut ledger);
    for g in &voice.gaps {
        eprintln!("[korvus] speech install gap: {}.{}", g.interface, g.member);
    }
    let parent = parent_of(scope, "EventTarget");
    let built = installer::create_interface(scope, global, &SPEECHSYNTHESIS, "SpeechSynthesis", parent, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus] speech install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);

    // ONLY IF THIS DEVICE HAS IT. The interface object above is published by
    // `create_interface`'s own capture rule; this own accessor of the global was outside every rule,
    // so an Opera window -- whose capture records no Speech API at all, not one of its eleven names --
    // carried a `speechSynthesis` a real Opera answers `undefined` for.
    // MEASURED `Unconditional`: `speechSynthesis` is an own property of `window` on
    // `http://web-platform.test:<p>/` -- `isSecureContext` false -- on Chrome 150, Brave 151, Edge 151
    // and Opera 135 alike, exactly as on a loopback page. It goes through the one gate so that the
    // condition is STATED rather than absent by default.
    if !crate::core::html::exposure::gate(
        scope,
        crate::core::html::exposure::Publishes::Global,
        "speechSynthesis",
        crate::core::html::exposure::Exposure::Unconditional,
    ) {
        return;
    }

    // The singleton: an instance of the table's prototype with no own properties, behind a
    // `[SameObject]` accessor so two reads are the same object. The realm's own, kept where every window
    // singleton is, so a realm built from templates makes its own on the first read.
    let s = crate::binding::protos::mint_on(scope, built.proto.into());
    crate::core::html::window_globals::window_slot_set(scope, "speechSynthesis", s.into());
    crate::core::html::window_globals::register_mint("speechSynthesis", crate::core::html::window_globals::Mint::Plain);
    let n = crate::binding::protos::str_val(scope, "speechSynthesis");
    let g = crate::binding::protos::build_fn_data(scope, cb_same_object, "get speechSynthesis", 0, n);
    let Some(k) = v8::String::new(scope, "speechSynthesis") else { return };
    let mut d = v8::PropertyDescriptor::new_from_get_set(g.into(), v8::undefined(scope).into());
    d.set_enumerable(true);
    d.set_configurable(true);
    global.define_property(scope, k.into(), &d);
}

/// A `[SameObject]` getter over the realm's singleton (its slot's name is the data). No receiver check:
/// the device answers `get.call({})` with the object.
fn cb_same_object(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if let Some(v) = crate::core::html::window_globals::window_singleton(scope, &name) {
        rv.set(v);
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            request_voices(scope, o);
        }
    }
}

/// Set on the singleton once its voice list has been asked for.
const P_VOICES_ASKED: &str = "korvus_speech_voices_asked";

/// The singleton's first read asks for the voice list, and the (empty) list's arrival fires ONE trusted
/// `voiceschanged` a task later. Brave 151 on this machine, measured
/// (`c8/repro/a bot sensor/voices*.html`): one plain `Event` (`bubbles`/`cancelable` false, `isTrusted`
/// true) shortly after the first `speechSynthesis` read with `getVoices()` still `[]`; a listener added
/// 600 ms after a late first read hears it; a later `getVoices()` fires nothing more. Without it a page
/// that awaits the event before reading voices waited forever, and a script that times the wait read
/// a browser with no speech service at all.
fn request_voices(scope: &mut v8::PinScope, synth: v8::Local<v8::Object>) {
    use crate::core::css::css_style_declaration::priv_get;
    if priv_get(scope, synth, P_VOICES_ASKED).is_some() {
        return;
    }
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::core::dom::element::priv_set(scope, synth, P_VOICES_ASKED, t);
    let f = crate::binding::protos::build_fn(scope, cb_voices_arrived, "", 0);
    // ~8 ms after the first read in Chrome 150 and Brave 151 alike (measured), so a page's
    // `setTimeout(f, 0)` still reads the empty list first.
    crate::core::intersection_observer::rendering::queue_task(scope, f, 8.0);
}

fn cb_voices_arrived(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if let Some(v) = crate::core::html::window_globals::window_singleton(scope, "speechSynthesis") {
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            crate::core::dom::element::priv_set(scope, o, P_VOICES_ARRIVED, t);
        }
        crate::core::html::browsing_context::fire_at(scope, v, "voiceschanged");
    }
}

// Rows VERBATIM from the `SpeechSynthesis` block of `install/generated_registry.rs` (window-only:
// the worker capture does not list it), in the capture's order, with its kinds, lengths, `required`
// counts and descriptor flags.
// `SpeechSynthesisVoice`, VERBATIM from its generated-registry block (`I603`), which this replaces: five
// readonly attributes in this order and a non-constructible interface object of length 0. Instances are
// minted by `getVoices()` and nowhere else.
webapi::interface! {
    "SpeechSynthesisVoice" [interface, window] {
        ctor    "SpeechSynthesisVoice"                 len 0 = stub,
        attr    "voiceURI"     : DomString readonly          = install_speech_voice,
        attr    "name"         : DomString readonly          = install_speech_voice,
        attr    "lang"         : DomString readonly          = install_speech_voice,
        attr    "localService" : Boolean readonly            = install_speech_voice,
        attr    "default"      : Boolean readonly            = install_speech_voice
    }
}

webapi::interface! {
    "SpeechSynthesis" : "EventTarget" [interface, window] {
        ctor    "SpeechSynthesis"                  len 0 = stub,
        attr    "pending"   : Any readonly               = install_speech_synthesis,
        attr    "speaking"  : Any readonly               = install_speech_synthesis,
        attr    "paused"    : Any readonly               = install_speech_synthesis,
        handler "onvoiceschanged"                        = event_handler,
        method  "cancel"    : Any len 0                  = stub,
        method  "getVoices" : Any len 0                  = install_speech_synthesis,
        method  "pause"     : Any len 0                  = stub,
        method  "resume"    : Any len 0                  = stub,
        method  "speak"     : Any len 1 required 1       = stub,
        // EDGE'S ONE, last, which is where Edge 151 puts it. MEASURED on Microsoft Edge 151.0.0.0,
        // 2026-09-13: `SpeechSynthesis.prototype.preload.length === 2`, and the arity guard is real --
        // `speechSynthesis.preload()` and `preload("a")` throw
        // `TypeError: Failed to execute 'preload' on 'SpeechSynthesis': 2 arguments required, but only
        // N present.`, so `required` is 2 and not a count inferred from the length. With two arguments
        // it got past the guard and threw on the FIRST argument's type ("The provided value cannot be
        // converted to a sequence"), so the return type was never observable -- `Any` is the honest tag
        // for a type nobody measured, and there is no synthesis engine here to preload a voice into.
        method  "preload"   : Any len 2 required 2       = stub
    }
}
