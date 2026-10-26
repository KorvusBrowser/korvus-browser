//! `core/fonts/font_face.rs` -- `FontFace` and `FontFaceSet` as two `interface!` tables, and
//! `document.fonts`.
//!
//! Rows are VERBATIM from the two blocks of `install/generated_registry.rs` they replace (the worker
//! registry's rows are identical key for key, so one table serves both realms with no selector).
//!
//! Every descriptor a page can read off a `FontFace` is a CSS value the page itself assigned, and the
//! recorded answers are the CSS defaults with the CSS grammar's own rejection -- `o.style = "x"` is a
//! `SyntaxError` because `"x"` is not a `<font-style>`, not because a list of allowed strings was
//! copied out of a recorded case. The grammar is [`super::css_descriptors`], a port of the ~280 lines
//! of `dom/09k-media-rtc.js` this file replaces; it is the recorded behaviour and is deliberately kept
//! lenient where the JS was lenient.
//!
//! A `FontFaceSet` is a setlike over the faces the page has added -- and, for `document.fonts`, over the
//! document's CSS-CONNECTED faces first (Blink's `FontFaceSetDocument`): one `FontFace` per
//! `@font-face` rule of the active sheets that names a family and a `src` (`FontFace::Create(document, rule)`),
//! in sheet order, made when the style is next read (`CSSConnectedFontFaceList` -> `UpdateActiveStyle`) and kept
//! while the rules are, made anew when a sheet holding them changes (`ClearFontFaceCacheAndAddUserFonts`). They
//! are in the set but cannot be deleted from it; they are `unloaded` until layout needs one for a character in
//! its `unicode-range` (`core/css/sheet_loads.rs` tells [`css_faces_used`]), when it loads as `load()` loads it.
//! a device-check script's tag reads `document.fonts`' families: korvus answered an empty set where Chrome 150 lists the
//! page's two faces, `unloaded`.
//!
//! THE SET'S LOADING STATE is Blink's `FontFaceSet`: a face of the set starting to load makes it `loading`
//! (`AddToLoadingFonts`), resets a settled `ready`, and on a later task fires `loading`; once no face of it is
//! loading, a task fires `loadingdone` with the faces that loaded and `loadingerror` with those that failed
//! (`FontFaceSetLoadEvent.fontfaces`), makes it `loaded` and resolves `ready`. `ready` is pending from the start
//! and first resolves on the task after the document's load event (`FontFaceSetDocument::DidLayout`).
//! `check()` and `load()` ask the family's best-matching faces (`FontFaceCache::Get`: Blink's font matching over
//! stretch, style and weight) whose `unicode-range` covers the text, and `check()` skips a family the persona
//! has a font for (`IsPlatformFamilyMatchAvailable`).
//!
//! A FACE LOADS. `load()` moves it to `loading` and fetches its source list in order --
//! `url()` entries whose `format()` names a font format this engine reads, each a `font` request in
//! `cors` mode against the realm's base -- and the first response whose bytes are a font (see
//! [`super::font_probe`]: the kernel's loader asks, the body never reaches this realm) makes it `loaded`
//! and resolves `loaded` with the face. When none is, it is `error` and `loaded` rejects with
//! `NetworkError`. An `ArrayBuffer` / view source is the font itself: the constructor probes it, so the
//! face is `loaded` (or `error`, rejecting with `SyntaxError`) before `new` returns. Measured on stock
//! Chrome 150 (tools/realweb/seed/fontface): a font URL loads; a src list falls through a 404 to its next
//! entry; an empty `data:` URL, a 404 and a truncated WOFF2 with the right magic reject. This used to
//! reject EVERY load with `NetworkError` -- a decision drawn from the one oracle probe there was,
//! `new FontFace("probe", "url(data:,)").load()`, whose source is not a font in any browser -- and
//! tldraw.com logged 32 of them where Chrome logs none.
//!
//! `FontFaceSet.load(font)` loads the faces in the set its families name and resolves with them (an
//! empty array when none matches -- it used to never settle); `check(font)` is false while a face it
//! names has not loaded; `status` is `loading` while a face in the set is.
//!
//! A FACE THAT HAS NOT BEEN ASKED TO LOAD has a `loaded` promise that has not settled -- the promise
//! `load()` hands back and settles. A source list is walked in order, `local()` and `url()` alike
//! (`CSSFontFace::Load`): `src: url(missing), local("DejaVu Sans")` falls through the 404 to the installed
//! font and loads (Chrome 150, measured). When a face settles its status moves at once; its `loaded`
//! promise settles at once only if script was never handed it (Blink makes `loaded_property_` lazily) and on
//! a task otherwise, and the sets and `FontFaceSet.load()`s waiting on it hear on a task after that
//! (`FontFace::SetLoadStatus` -> `RunCallbacks`).
//!
//! `features`, `variations` and `palettes` are members webref declares that the capture does NOT ship,
//! so they are not rows and reading one is `undefined` -- which is exactly what the oracle records.
//!
//! INSTANCE STATE is a v8 private slot, not a `WeakMap` and not an own property, so
//! `Object.getOwnPropertyNames(new FontFace(...))` is `[]` and the slot is also the BRAND: a member
//! reached on `Object.create(FontFace.prototype)` is Chrome's `Illegal invocation`, where the JS's
//! `own()`/`WeakMap` answered for anything the file had minted and nothing else.

use deno_core::v8;

use super::css_descriptors as css;
use crate::binding::instance_state as st;
use crate::binding::protos::{
    build_fn, build_fn_data, make_dom_exc, rejected_promise, resolved_promise, shell_proto, str_val, throw_type_error,
};
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

pub(crate) const FACE: &str = "FontFace";
pub(crate) const FACESET: &str = "FontFaceSet";
/// Private-slot names for the iterator object and the realm's one iterator prototype. Neither is an
/// interface in the capture; they are internal, and a page sees only their effects.
const ITERATOR: &str = "FontFaceSetIterator";
const ITERATOR_PROTO: &str = "FontFaceSetIteratorProto";
/// Private-slot name of a `FontFaceSet.load()` call's state (Blink's `LoadFontPromiseResolver`): one of the
/// callbacks a loading face notifies, beside the sets it is in.
const LOAD_RESOLVER: &str = "FontFaceSetLoadResolver";
/// The document's private slot holding its `FontFaceSet`.
const DOC_SLOT: &str = "DocumentFonts";

fn throw_dom(scope: &mut v8::PinScope, msg: &str, name: &str) {
    if let Some(e) = make_dom_exc(scope, msg, name) {
        scope.throw_exception(e);
    }
}

/// Blink's message for a value the grammar rejected, in both the setter and the constructor. `prefix`
/// is the whole "Failed to ..." clause, which differs between the two (`set the 'style' property on
/// 'FontFace'` vs `construct 'FontFace'`); the tail is the same.
fn syntax_error(scope: &mut v8::PinScope, prefix: &str, value: &str) {
    throw_dom(scope, &format!("{prefix}: Failed to set '{value}' as a property value."), "SyntaxError");
}

/// A pending promise. In V8 a `Promise::Resolver` IS its promise, so the promise a face stores is also
/// what settles it ([`resolver_of`]).
fn pending_promise<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Value> {
    let resolver = v8::PromiseResolver::new(scope).unwrap();
    resolver.get_promise(scope).into()
}

/// The resolver of a face's `loaded` promise. Sound: the slot is private to this file and only ever
/// holds a promise this file made (a resolver's, or an already-rejected one, which settling ignores).
fn resolver_of<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::PromiseResolver>> {
    let v = st::get(scope, s, "loaded");
    if !v.is_promise() {
        return None;
    }
    Some(unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(v) })
}

/// Settle a face (`FontFace::SetLoadStatus(kLoaded)` / `SetError`): `loaded` resolves with the face, or
/// rejects with `err` (`NetworkError` for a failed load, `SyntaxError` for bytes handed to the constructor
/// that are not a font). The rejection is the face's state, not an unhandled error of the page's.
///
/// The status moves now. The promise settles NOW when script was never handed it -- Blink makes
/// `loaded_property_` only when `loaded` is read or `load()` returns it, and makes it settled -- and on a
/// TASK when it was (`LoadedProperty::Resolve` posted to the DOM-manipulation queue). The callbacks -- the
/// sets loading the face, the `FontFaceSet.load()`s waiting on it -- run on a task after that
/// (`RunCallbacks`), always.
fn settle_face(scope: &mut v8::PinScope, face: v8::Local<v8::Object>, s: v8::Local<v8::Object>, err: Option<(&str, &str)>) {
    st::set_str(scope, s, "status", if err.is_none() { "loaded" } else { "error" });
    if let Some((msg, name)) = err {
        // `SetError` keeps the face's FIRST error
        if !st::get(scope, s, "errName").is_string() || st::get_str(scope, s, "errName").is_empty() {
            st::set_str(scope, s, "errName", name);
            st::set_str(scope, s, "errMsg", msg);
        }
    }
    let data: v8::Local<v8::Value> = v8::Local::new(scope, face).into();
    if st::get_bool(scope, s, "used") {
        let f = build_fn_data(scope, cb_settle_promise, "", 0, data);
        crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
    } else {
        settle_promise_now(scope, face, s);
    }
    let f = build_fn_data(scope, cb_run_callbacks, "", 0, data);
    crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
}

/// The face's error as the DOMException its `loaded` rejects with (`FontFace::GetError`).
fn face_error<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Value>> {
    // `SetError(nullptr)` is a plain `NetworkError`
    if !st::get(scope, s, "errName").is_string() || st::get_str(scope, s, "errName").is_empty() {
        return make_dom_exc(scope, "A network error occurred.", "NetworkError");
    }
    let name = st::get_str(scope, s, "errName");
    let msg = st::get_str(scope, s, "errMsg");
    make_dom_exc(scope, &msg, &name)
}

/// Settle the face's `loaded` promise by its status, now.
fn settle_promise_now(scope: &mut v8::PinScope, face: v8::Local<v8::Object>, s: v8::Local<v8::Object>) {
    let Some(r) = resolver_of(scope, s) else { return };
    match st::get_str(scope, s, "status").as_str() {
        "loaded" => {
            r.resolve(scope, face.into());
        }
        "error" => {
            r.get_promise(scope).mark_as_handled();
            if let Some(e) = face_error(scope, s) {
                r.reject(scope, e);
            }
        }
        _ => {}
    }
}

fn cb_settle_promise(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Ok(face) = v8::Local::<v8::Object>::try_from(data) else { return };
    let Some(s) = st::state(scope, face.into(), FACE) else { return };
    settle_promise_now(scope, face, s);
}

/// `FontFace::RunCallbacks`: every callback the face was holding hears whether it loaded.
fn cb_run_callbacks(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Ok(face) = v8::Local::<v8::Object>::try_from(data) else { return };
    let Some(s) = st::state(scope, face.into(), FACE) else { return };
    let callbacks = st::get_array(scope, s, "callbacks").map(|a| st::array_items(scope, a)).unwrap_or_default();
    let none = st::array(scope, &[]);
    st::set(scope, s, "callbacks", none.into());
    let loaded = st::get_str(scope, s, "status") == "loaded";
    for cb in callbacks {
        notify(scope, cb, face, loaded);
    }
}

/// `FontFace::AddCallback`: a callback hears at once from a face that has settled, and is kept until it does
/// otherwise.
fn add_callback(scope: &mut v8::PinScope, face: v8::Local<v8::Object>, s: v8::Local<v8::Object>, cb: v8::Local<v8::Value>) {
    match st::get_str(scope, s, "status").as_str() {
        "loaded" => notify(scope, cb, face, true),
        "error" => notify(scope, cb, face, false),
        _ => {
            let mut items = st::get_array(scope, s, "callbacks").map(|a| st::array_items(scope, a)).unwrap_or_default();
            items.push(cb);
            let arr = st::array(scope, &items);
            st::set(scope, s, "callbacks", arr.into());
        }
    }
}

/// One callback hearing from a face: a set (`FontFaceSetDocument::NotifyLoaded` / `NotifyError`) or a
/// `FontFaceSet.load()` call (`LoadFontPromiseResolver::NotifyLoaded` / `NotifyError`).
fn notify(scope: &mut v8::PinScope, cb: v8::Local<v8::Value>, face: v8::Local<v8::Object>, loaded: bool) {
    if let (Ok(set), Some(ss)) = (v8::Local::<v8::Object>::try_from(cb), st::state(scope, cb, FACESET)) {
        push_to(scope, ss, if loaded { "loadedFonts" } else { "failedFonts" }, face.into());
        remove_from_loading_fonts(scope, set, ss, face.into());
        return;
    }
    let Some(rs) = st::state(scope, cb, LOAD_RESOLVER) else { return };
    let left = st::get_num(scope, rs, "numLoading") - 1.0;
    st::set_num(scope, rs, "numLoading", left);
    let errored = st::get_bool(scope, rs, "errorOccured");
    let promise = st::get(scope, rs, "promise");
    if !promise.is_promise() {
        return;
    }
    // Sound: the slot only ever holds the promise of a resolver this file made (see `resolver_of`).
    let resolver = unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(promise) };
    if loaded {
        if left > 0.0 || errored {
            return;
        }
        let faces = st::get(scope, rs, "faces");
        resolver.resolve(scope, faces);
    } else if !errored {
        st::set_bool(scope, rs, "errorOccured", true);
        let Some(fs) = st::state(scope, face.into(), FACE) else { return };
        if let Some(e) = face_error(scope, fs) {
            resolver.reject(scope, e);
        }
    }
}

/// Append `v` to the array in `s.name`.
fn push_to(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, name: &str, v: v8::Local<v8::Value>) {
    let mut items = st::get_array(scope, s, name).map(|a| st::array_items(scope, a)).unwrap_or_default();
    items.push(v);
    let arr = st::array(scope, &items);
    st::set(scope, s, name, arr.into());
}

/// One fresh object on `iface`'s prototype with no own property.
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

/// A face's loading bookkeeping: no error yet, a `loaded` promise script has not been handed (`used`), no
/// callback waiting, in no set.
fn init_load_state(scope: &mut v8::PinScope, s: v8::Local<v8::Object>) {
    st::set_str(scope, s, "errName", "");
    st::set_str(scope, s, "errMsg", "");
    st::set_bool(scope, s, "used", false);
    let none = st::array(scope, &[]);
    st::set(scope, s, "callbacks", none.into());
    let none = st::array(scope, &[]);
    st::set(scope, s, "sets", none.into());
}

// ---- FontFace ------------------------------------------------------------------------------------

pub(crate) fn cb_font_face_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("FontFace");
        throw_type_error(scope, &msg);
        return;
    }
    if args.length() < 2 {
        throw_type_error(scope, &format!("Failed to construct 'FontFace': 2 arguments required, but only {} present.", args.length()));
        return;
    }
    // NOTHING BELOW THROWS FOR A VALUE THE GRAMMAR REJECTS. CSS Font Loading's
    // constructor parses the family, the source and every descriptor, and a failure REJECTS the face's
    // `loaded` promise and leaves `status` at "error" -- the face is still handed back. Chrome 150,
    // measured on a loopback page:
    //
    // ```text
    // new FontFace("x","url(x.woff)",{unicodeRange:""})              [object FontFace], status error,
    //                                                                 unicodeRange U+0-10FFFF,
    //                                                                 loaded rejects SyntaxError
    //                                                                 "Failed to set '' as a property value."
    // {style:"italic", weight:"bogus", stretch:"condensed"}          italic | normal | condensed
    //                                                                 (the bad one keeps its initial value,
    //                                                                 the good ones after it still apply)
    // {weight:"bogus", style:"bad"}                                  message names 'bad': the FIRST failure
    //                                                                 in descriptor order (style before weight)
    // new FontFace("x","bogus(")                                     status error, "The source provided
    //                                                                 ('bogus(') could not be parsed as a
    //                                                                 value list."
    // new FontFace("x","bogus(",{style:"st"})                        message names 'st': descriptors first
    // ```
    //
    // This engine threw a SyntaxError out of `new` for every one of those, and a page that builds a
    // FontFace from a descriptor dictionary with an empty member died on the constructor.
    let family_raw = args.get(0);
    let Some(family_text) = st::to_str(scope, family_raw) else { return };
    let fam = css::serialize_family(&family_text);
    // `(CSSOMString or BufferSource) source`: an ArrayBuffer or a view is the BINARY form -- there is no `src` string
    // to parse, and the grammar is not asked -- told by what the value IS, as WebIDL's union conversion tells it
    // (korvus asked the object for a `byteLength`, which ran a page's getter and took any object with one for a
    // buffer). Anything else is a string.
    let source = args.get(1);
    let binary = source.is_array_buffer() || source.is_array_buffer_view() || source.is_shared_array_buffer();
    let source_text = if binary {
        None
    } else {
        let Some(t) = st::to_str(scope, source) else { return };
        Some(t)
    };
    // `FontFaceDescriptors`, converted as WebIDL converts it, after the two arguments before it (Chrome 150):
    // its twelve members in lexicographic order, each read once and made a string as it is read --
    // korvus read them in descriptor order.
    let Ok(init_dict) = crate::install::ctor_dicts::convert_dict(scope, args.get(2), "FontFaceDescriptors", "Failed to construct 'FontFace': ") else { return };
    let init: v8::Local<v8::Value> = init_dict.into();
    // The constructor's steps: the source parsed, then the descriptors APPLIED in `DESCRIPTORS` order, so the FIRST
    // bad descriptor is the one named in the error whichever order the page wrote them in. A rejected one keeps its
    // initial value.
    let mut src: Option<String> = None;
    let mut error: Option<String> = None;
    let mut source_error: Option<String> = None;
    if let Some(source_text) = source_text {
        match css::parse_source(&source_text) {
            Some(s) => src = Some(s),
            None => source_error = Some(format!("The source provided ('{source_text}') could not be parsed as a value list.")),
        }
    }
    let mut applied: Vec<(&'static str, String)> = Vec::new();
    for d in css::DESCRIPTORS {
        let Some(v) = st::dict_get(scope, init, d.name) else { continue };
        let Some(text) = st::to_str(scope, v) else { return };
        match (d.parse)(&text) {
            Some(parsed) => applied.push((d.name, parsed)),
            None => {
                if error.is_none() {
                    error = Some(format!("Failed to set '{text}' as a property value."));
                }
            }
        }
    }
    if error.is_none() {
        error = source_error;
    }
    let loaded = match &error {
        None => pending_promise(scope),
        Some(msg) => {
            let Some(e) = make_dom_exc(scope, msg, "SyntaxError") else { return };
            let p = rejected_promise(scope, e);
            // The rejection is the face's state, not an unhandled error of the page's.
            if let Ok(pr) = v8::Local::<v8::Promise>::try_from(p) {
                pr.mark_as_handled();
            }
            p
        }
    };
    let s = st::attach(scope, args.this(), FACE);
    st::set_str(scope, s, "family", &fam);
    match &src {
        Some(t) => st::set_str(scope, s, "src", t),
        None => st::set_null(scope, s, "src"),
    }
    st::set_bool(scope, s, "binary", binary);
    st::set_str(scope, s, "status", if error.is_some() { "error" } else { "unloaded" });
    for d in css::DESCRIPTORS {
        st::set_str(scope, s, d.name, d.initial);
    }
    for (name, value) in applied {
        st::set_str(scope, s, name, &value);
    }
    st::set(scope, s, "loaded", loaded);
    init_load_state(scope, s);
    // `font-src` is asked HERE, as Blink makes each `url()` source's request when the face is constructed
    // (`FontFace::InitCSSFontFace`): a violation carries the constructor's position, and `load()` only finds the
    // refused source failed (Chrome 150, measured: `new FontFace(...)`'s column, the rejection before the event).
    if let (Some(t), false, None) = (&src, binary, &error) {
        let base = realm_base(scope);
        let mut refused: Vec<v8::Local<v8::Value>> = Vec::new();
        for source in face_sources(t, &base) {
            if let Source::Url(u) = source {
                if crate::core::html::csp::realm_request_blocked(scope, &u, "font") {
                    // and its resource-timing entry, made at the refusal as Blink's request makes it
                    crate::core::timing::resource_timing::refused_request(scope, "css", &u);
                    refused.push(str_val(scope, &u));
                }
            }
        }
        let list = st::array(scope, &refused);
        st::set(scope, s, "cspRefused", list.into());
    }
    if let Some(msg) = &error {
        st::set_str(scope, s, "errName", "SyntaxError");
        st::set_str(scope, s, "errMsg", msg);
    }
    // The BINARY form is the font itself: parsed now, so the face is settled before `new` returns
    // (Chrome 150: `status` reads `loaded` -- or `error` -- on the next line).
    if binary && error.is_none() {
        let bytes = crate::core::streams::text_decoder_stream::buffer_source_bytes(source).unwrap_or_default();
        let face = args.this();
        if super::font_probe::probe(&bytes) {
            settle_face(scope, face, s, None);
        } else {
            settle_face(scope, face, s, Some(("Invalid font data in ArrayBuffer.", "SyntaxError")));
        }
    }
}

/// The twelve descriptor getters and `family`: the stored string, by name.
fn cb_face_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACE) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if name == "loaded" {
        // script holds the promise now (Blink's `FontStatusPromise` makes `loaded_property_`)
        st::set_bool(scope, s, "used", true);
    }
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// The twelve descriptor setters: the grammar decides, and a rejection is the CSS `SyntaxError`.
fn cb_face_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACE) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(text) = st::to_str(scope, args.get(0)) else { return };
    // `family` takes the string VERBATIM -- Chrome 150 measured: `f.family = ""`, `"1bad"`, `"inherit"`
    // and `"a,b"` each read back exactly as assigned, where the constructor serializes (see
    // `css::serialize_family`). It never throws.
    if name == "family" {
        st::set_str(scope, s, "family", &text);
        return;
    }
    let parse: fn(&str) -> Option<String> = {
        match css::descriptor(&name) {
            Some(d) => d.parse,
            None => {
                debug_assert!(false, "cb_face_set: unexpected descriptor {name}");
                return;
            }
        }
    };
    match parse(&text) {
        Some(parsed) => st::set_str(scope, s, &name, &parsed),
        None => syntax_error(scope, &format!("Failed to set the '{name}' property on 'FontFace'"), &text),
    }
}

/// `load()`. See this file's header. The face starts loading BEFORE its promise is handed out
/// (`FontFace::load`: `Load()`, then `FontStatusPromise`), so a `local()` font that loads at once hands back a
/// settled promise.
fn cb_face_load(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACE) else { return };
    start_load(scope, args.this(), s);
    st::set_bool(scope, s, "used", true);
    let v = st::get(scope, s, "loaded");
    rv.set(v);
}

/// Begin loading an `unloaded` face (any other status: nothing to do) -- Blink's `CSSFontFace::Load`, which a
/// script's `load()`, a `FontFaceSet.load()` and layout needing a CSS-connected face all call.
///
/// The face is `loading` first, and every set it is in hears so (`CSSFontFace::SetLoadStatus` ->
/// `BeginFontLoading`). Then the sources go IN ORDER, `local()` and `url()` alike. A `local()` one is the installed
/// font of that name (`spec::RenderBackend::local_font`, Chromium's fontconfig match) and settles at once, as
/// Blink's `LocalFontFaceSource` does: measured on stock Chrome 150, `new FontFace(f,
/// 'local("DejaVu Sans")').load()` leaves the face `loaded` and its promise settled before `load()` returns, one no
/// font matches `error` and rejected with `NetworkError` -- the challenge frame probes 55 names so, and every one
/// failed here (a `local()` named no installed font). A `url()` is fetched, and a failed one moves on to the next
/// source -- a `local()` after it included (F9: `src: url(404), local("DejaVu Sans")` loads in Chrome 150).
fn start_load(scope: &mut v8::PinScope, face: v8::Local<v8::Object>, s: v8::Local<v8::Object>) {
    if st::get_str(scope, s, "status") != "unloaded" {
        return;
    }
    st::set_str(scope, s, "status", "loading");
    begin_font_loading(scope, face, s);
    let src = st::get_str(scope, s, "src");
    // a CSS-connected face's `url()`s resolve against its sheet's URL
    let base = match st::get(scope, s, "base") {
        b if b.is_string() => b.to_rust_string_lossy(scope),
        _ => realm_base(scope),
    };
    let sources: Vec<v8::Local<v8::Value>> = face_sources(&src, &base)
        .into_iter()
        .map(|source| match source {
            Source::Local(name) => str_val(scope, &format!("L{name}")),
            Source::Url(u) => str_val(scope, &format!("U{u}")),
        })
        .collect();
    let list = st::array(scope, &sources);
    try_source(scope, face, list, 0);
}

/// `CSSFontFace::SetLoadStatus(kLoading)`: each set holding the face starts loading it.
fn begin_font_loading(scope: &mut v8::PinScope, face: v8::Local<v8::Object>, s: v8::Local<v8::Object>) {
    let sets = st::get_array(scope, s, "sets").map(|a| st::array_items(scope, a)).unwrap_or_default();
    for set in sets {
        if let (Ok(set), Some(ss)) = (v8::Local::<v8::Object>::try_from(set), st::state(scope, set, FACESET)) {
            add_to_loading_fonts(scope, set, ss, face, s);
        }
    }
}

/// One entry of a face's `src` list.
enum Source {
    /// `local(<family-name>)`: a quoted string, or identifiers joined by single spaces.
    Local(String),
    /// A `url()` resolved against the base, its `format()` one this engine reads.
    Url(String),
}

/// The base URL a face's `url()` resolves against: the frame document's in a frame, else the realm's.
fn realm_base(scope: &mut v8::PinScope) -> String {
    match crate::core::html::browsing_context::running_frame(scope) {
        Some(id) => crate::core::html::browsing_context::frame_request_client(scope, id).1,
        None => crate::core::dom::base_url(scope),
    }
}

/// The entries of a `src` descriptor, in order: `local()` names, and `url()`s resolved against `base` -- an entry
/// whose `format()` names a format this engine does not read is skipped (CSS Fonts 4 `src`).
fn face_sources(src: &str, base: &str) -> Vec<Source> {
    let base = url::Url::parse(base).ok();
    let mut depth = 0;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in src.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&src[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&src[start..]);
    let mut out = Vec::new();
    for part in parts {
        let lower = part.to_ascii_lowercase();
        if let Some(name) = local_name(part) {
            out.push(Source::Local(name));
            continue;
        }
        if !lower.contains("url(") {
            continue;
        }
        if let Some(f) = lower.find("format(") {
            let fmt = lower[f + 7..].split(')').next().unwrap_or("").trim().trim_matches(|c| c == '"' || c == '\'');
            if !matches!(fmt, "woff2" | "woff" | "truetype" | "opentype" | "collection" | "woff2-variations" | "woff-variations" | "truetype-variations" | "opentype-variations") {
                continue;
            }
        }
        let Some(raw) = page::style::urls::urls_in(part).into_iter().next() else { continue };
        let joined = match &base {
            Some(b) => b.join(&raw).map(|u| u.to_string()).unwrap_or(raw),
            None => raw,
        };
        out.push(Source::Url(joined));
    }
    out
}

/// The `<family-name>` of a `local(...)` entry: its quoted string, or its identifiers joined by single spaces.
fn local_name(part: &str) -> Option<String> {
    let t = part.trim();
    let open = t.get(..6).filter(|p| p.eq_ignore_ascii_case("local("))?;
    let inner = t[open.len()..].strip_suffix(')')?.trim();
    if let Some(q) = inner.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let body = inner[1..].strip_suffix(q)?;
        return Some(body.to_string());
    }
    let words: Vec<&str> = inner.split_ascii_whitespace().collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// Try source `i` of `list` for `face` -- `L<name>` a `local()`, `U<url>` a `url()` -- moving on until one
/// loads, or settle the face as failed when there is none left (`CSSFontFace::Load`'s source walk).
fn try_source(scope: &mut v8::PinScope, face: v8::Local<v8::Object>, list: v8::Local<v8::Array>, i: u32) {
    let Some(s) = st::state(scope, face.into(), FACE) else { return };
    let mut i = i;
    loop {
        if i >= list.length() {
            settle_face(scope, face, s, Some(("A network error occurred.", "NetworkError")));
            return;
        }
        let entry = list.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        if let Some(name) = entry.strip_prefix('L') {
            let render = {
                let op_state = deno_core::JsRuntime::op_state_from(scope);
                let st = op_state.borrow();
                st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<spec::RenderHandle>()).map(|h| h.0.clone())
            };
            if render.as_ref().and_then(|r| r.local_font(name)).is_some() {
                settle_face(scope, face, s, None);
                return;
            }
            i += 1;
            continue;
        }
        let url = entry.strip_prefix('U').unwrap_or(&entry).to_string();
        // `font-src`: a source the document's CSP refuses is never fetched; it fails as an
        // unreachable one does and the next source is tried. A constructed face was asked when it was made (each
        // refusing policy reported then); a face from a style sheet is asked now.
        let refused = match st::get_array(scope, s, "cspRefused") {
            Some(a) => st::array_items(scope, a).into_iter().any(|v| v.to_rust_string_lossy(scope) == url),
            None => {
                let refused = crate::core::html::csp::realm_request_blocked(scope, &url, "font");
                if refused && !st::get_bool(scope, s, "originDirty") {
                    crate::core::timing::resource_timing::refused_request(scope, "css", &url);
                }
                refused
            }
        };
        if refused {
            i += 1;
            continue;
        }
        // A font is a subresource of the document: its fetch delays the load event as a stylesheet's does
        // (`sheet_loads::start_load`), and joins the request already in flight for the same URL. A worker has no
        // load event to delay (token 0).
        let has_document = {
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.has::<crate::core::html::browsing_context::BrowsingContexts>()
        };
        let token = if has_document { crate::core::html::browsing_context::new_load_delay_token(scope) } else { 0 };
        if token != 0 {
            crate::core::html::browsing_context::delay_load(scope, token);
        }
        let dirty = st::get_bool(scope, s, "originDirty");
        crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, dirty);
        let fetched = crate::core::html::subresource::start_shared_fetch(scope, &url, "font", "cors");
        crate::core::timing::resource_timing::from_origin_dirty_sheet(scope, false);
        let Some(p) = fetched else {
            if token != 0 {
                crate::core::html::browsing_context::undelay_load(scope, token);
            }
            i += 1;
            continue;
        };
        let idx: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, i).into();
        let tok: v8::Local<v8::Value> = v8::Integer::new_from_unsigned(scope, token).into();
        let data = st::array(scope, &[face.into(), list.into(), idx, tok]);
        let ok = build_fn_data(scope, cb_source_fetched, "", 1, data.into());
        let bad = build_fn_data(scope, cb_source_failed, "", 1, data.into());
        // the fetch's end is a task, not a promise job (`then_task`)
        crate::then_task(scope, p, ok, Some(bad));
        return;
    }
}

/// `(face, list, index)` out of a source callback's data, its load-delay token released.
fn source_data<'s>(scope: &mut v8::PinScope<'s, '_>, data: v8::Local<'s, v8::Value>) -> Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Array>, u32)> {
    let d = v8::Local::<v8::Array>::try_from(data).ok()?;
    if let Some(token) = d.get_index(scope, 3).and_then(|v| v.uint32_value(scope)).filter(|t| *t != 0) {
        crate::core::html::browsing_context::undelay_load(scope, token);
    }
    let face = d.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let list = d.get_index(scope, 1).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
    let i = d.get_index(scope, 2)?.uint32_value(scope)?;
    Some((face, list, i))
}

/// A source answered: the face loads when the response is a 2xx the CORS check lets through and its
/// bytes are a font; otherwise the next source is tried.
fn cb_source_fetched(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    #[cfg(not(feature = "analysis"))]
    let data = v8::Local::new(scope, args.data());
    // an analysis build makes this function a fused record: its data is the record's
    #[cfg(feature = "analysis")]
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((face, list, i)) = source_data(scope, data) else { return };
    let entry = list.get_index(scope, i).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let url = entry.strip_prefix('U').unwrap_or(&entry).to_string();
    let resp = v8::Local::new(scope, args.get(0));
    let ok = v8::Local::<v8::Object>::try_from(resp).ok().is_some_and(|r| {
        let status = crate::binding::protos::get_prop(scope, r, "status").and_then(|v| v.uint32_value(scope)).unwrap_or(0);
        let final_url = crate::binding::protos::get_prop(scope, r, "url").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_else(|| url.clone());
        let headers = crate::binding::protos::get_prop(scope, r, "headers")
            .map(|h| crate::core::html::isolation_policy::headers_from_js(scope, h))
            .unwrap_or_default();
        let base = realm_base(scope);
        (200..300).contains(&status) && cors_allows(&base, &final_url, &headers) && font_parsed(scope, &url)
    });
    match ok {
        true => {
            if let Some(s) = st::state(scope, face.into(), FACE) {
                settle_face(scope, face, s, None);
            }
        }
        false => try_source(scope, face, list, i + 1),
    }
}

fn cb_source_failed(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    #[cfg(not(feature = "analysis"))]
    let data = v8::Local::new(scope, args.data());
    // an analysis build makes this function a fused record: its data is the record's
    #[cfg(feature = "analysis")]
    let data = crate::binding::protos::body_data(scope, &args);
    let Some((face, list, i)) = source_data(scope, data) else { return };
    try_source(scope, face, list, i + 1);
}

/// A font is a CORS request: a cross-origin response loads only with `Access-Control-Allow-Origin`
/// naming the requesting origin or `*`.
fn cors_allows(base: &str, final_url: &str, headers: &[(String, String)]) -> bool {
    let (Ok(b), Ok(f)) = (url::Url::parse(base), url::Url::parse(final_url)) else { return true };
    if b.origin() == f.origin() || !matches!(f.scheme(), "http" | "https") {
        return true;
    }
    match crate::core::html::isolation_policy::header_value(headers, "access-control-allow-origin") {
        Some(v) => v.trim() == "*" || v.trim() == b.origin().ascii_serialization(),
        None => false,
    }
}

/// Did the loader find a font in the body it fetched for `url`?
fn font_parsed(scope: &mut v8::PinScope, url: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    st.try_borrow::<spec::kernel::Context>()
        .and_then(|c| c.get::<super::font_probe::FontProbes>())
        .and_then(|p| p.get(url))
        .unwrap_or(false)
}

pub fn install_font_face(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "load" => method_row(scope, proto, member, cb_face_load),
        "status" | "loaded" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_face_get, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        _ => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_face_get, &format!("get {}", member.name), 0, d);
            let d = str_val(scope, member.name);
            let s = build_fn_data(scope, cb_face_set, &format!("set {}", member.name), 1, d);
            define_accessor(scope, proto, member, g, Some(s));
        }
    }
}

// ---- FontFaceSet ---------------------------------------------------------------------------------

/// One `FontFaceSet` for `doc`, with Blink's `FontFaceSet` state: the faces script added (`faces`), the
/// document's CSS-connected faces and the keys of the rules they were made from (`cssFaces`, `cssKeys`),
/// whether it is loading and the faces it is loading, has loaded and has seen fail since it last fired
/// `loadingdone`, and `ready`.
///
/// `ready` is PENDING from the start (Blink makes its `ReadyProperty` pending) and resolves on the task after
/// the document's load event ([`load_event_finished`]) -- or is settled already when the set is first read
/// after that event, the state Blink's set (made with the document) is in by then.
fn mint_face_set<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, FACESET)?;
    let s = st::attach(scope, o, FACESET);
    for name in ["faces", "cssFaces", "cssKeys", "loadingFonts", "loadedFonts", "failedFonts"] {
        let empty = st::array(scope, &[]);
        st::set(scope, s, name, empty.into());
    }
    st::set_bool(scope, s, "isLoading", false);
    st::set_bool(scope, s, "fireLoading", false);
    st::set_bool(scope, s, "pendingTask", false);
    let finished = load_finished(scope, doc);
    let ready = if finished { resolved_promise(scope, o.into()) } else { pending_promise(scope) };
    st::set(scope, s, "ready", ready);
    st::set_bool(scope, s, "readyPending", !finished);
    Some(o)
}

/// Has `doc`'s load event run -- what Blink's set waits for before it first resolves `ready`
/// (`FontFaceSetDocument::DidLayout` returns until `LoadEventFinished`)? The page's document by its readiness, a
/// frame's by its own; a document with no browsing context (`createHTMLDocument`, `createDocument`) has no load
/// event, and its set's `ready` waits for a face of its own to load.
fn load_finished(scope: &mut v8::PinScope, doc: v8::Local<v8::Object>) -> bool {
    use crate::core::html::browsing_context::{doc_readiness, frame_doc_readiness, frame_document_window, BrowsingContexts, Readiness};
    let docv: v8::Local<v8::Value> = doc.into();
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is_page = matches!(crate::binding::protos::document_val(scope, &op_state), Some(d) if d == docv);
    // a realm with no browsing contexts (a harness's bare document) has no lifecycle: its document is finished
    if !op_state.borrow().has::<BrowsingContexts>() {
        return is_page;
    }
    if is_page {
        return doc_readiness(scope) == Readiness::Complete;
    }
    if frame_document_window(scope, docv).is_some() {
        let root = deno_core::cppgc::try_unwrap_cppgc_object::<crate::core::dom::document::JsDocument>(scope, docv).map(|p| unsafe { p.as_ref() }.root);
        return root.and_then(|r| frame_doc_readiness(scope, r)).is_none_or(|r| r == Readiness::Complete);
    }
    false
}

/// The faces script added, in insertion order (Blink's `non_css_connected_faces_`).
fn faces_of<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    match st::get_array(scope, s, "faces") {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    }
}

fn set_faces(scope: &mut v8::PinScope, s: v8::Local<v8::Object>, items: &[v8::Local<v8::Value>]) {
    let arr = st::array(scope, items);
    st::set(scope, s, "faces", arr.into());
}

/// The index of `face` in the set, by IDENTITY (`Array#indexOf`'s rule).
fn index_of(scope: &mut v8::PinScope, items: &[v8::Local<v8::Value>], face: v8::Local<v8::Value>) -> Option<usize> {
    let _ = scope;
    items.iter().position(|f| f == &face)
}

/// Every face of the set as Blink iterates it (`FontFaceSet::CreateIterationSource`): the CSS-connected ones, then
/// the ones script added.
fn all_faces<'s>(scope: &mut v8::PinScope<'s, '_>, set: v8::Local<v8::Object>, s: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    let mut out = css_faces(scope, set, s);
    out.extend(faces_of(scope, s));
    out
}

// ---- the CSS-connected faces ---------------------------------------------------------------------

/// One `@font-face` rule of the document's active sheets that makes a face (`FontFace::Create(document, rule)`):
/// a `<family-name>` and a `src` list; every other descriptor its value, or its initial one where the rule has
/// none (the CSS parser dropped a value it could not parse).
struct CssRule {
    /// The rule's identity: its sheet, that sheet's rules this time, its place among them, its text. A face lives
    /// as long as its key does.
    key: String,
    sheet: u32,
    family: String,
    src: String,
    /// What its `url()`s resolve against: its sheet's URL.
    base: String,
    /// The rule is origin-dirty (`sheet_loads::dirty_bases`): its font's request makes no resource-timing entry.
    origin_dirty: bool,
    descriptors: Vec<(&'static str, String)>,
}

/// The `@font-face` descriptor each `FontFace` attribute reads.
const CSS_DESCRIPTOR_NAMES: &[(&str, &str)] = &[
    ("style", "font-style"),
    ("weight", "font-weight"),
    ("stretch", "font-stretch"),
    ("unicodeRange", "unicode-range"),
    ("variant", "font-variant"),
    ("featureSettings", "font-feature-settings"),
    ("variationSettings", "font-variation-settings"),
    ("display", "font-display"),
    ("ascentOverride", "ascent-override"),
    ("descentOverride", "descent-override"),
    ("lineGapOverride", "line-gap-override"),
    ("sizeAdjust", "size-adjust"),
];

/// The generic family keywords: never a `<family-name>` unquoted, and no face of a set is asked for one
/// (`FontFamily::FamilyIsGeneric`).
const GENERIC_FAMILIES: &[&str] = &[
    "serif", "sans-serif", "cursive", "fantasy", "monospace", "system-ui", "math", "emoji", "fangsong", "ui-serif",
    "ui-sans-serif", "ui-monospace", "ui-rounded", "-webkit-body", "-webkit-pictograph",
];

/// The `@font-face` rules of the document's sheets that apply -- the sheet on and its `@media` / `@supports` chain
/// matching -- in sheet order, each that makes a face.
fn css_rules(scope: &mut v8::PinScope) -> Vec<CssRule> {
    use std::hash::{Hash, Hasher};
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let Some(p) = st.try_borrow::<spec::kernel::Context>().and_then(|c| c.get::<page::PageHandle>()).map(|h| h.0.clone()) else {
        return Vec::new();
    };
    let Some(host) = st.try_borrow::<crate::binding::HostDom>() else { return Vec::new() };
    p.sync_document_sheets(&host.0);
    let media = crate::core::css::cssom::media_context(&st);
    let doc_base = crate::core::dom::base_url_of(&st);
    let doc_url = crate::core::dom::document_url_of(&st);
    let mut out = Vec::new();
    for handle in 0..p.sheet_count() as u32 {
        p.with_sheet(handle, |sheet| {
            if !sheet.sheet_applies(&media) || sheet.font_faces().is_empty() {
                return;
            }
            let dirty = crate::core::css::sheet_loads::dirty_bases(sheet, &doc_url);
            // A sheet whose text or rule list moved holds new rules: Blink then rebuilds the font face cache and
            // makes every CSS-connected face anew (`ClearFontFaceCacheAndAddUserFonts`).
            let generation = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                sheet.text().hash(&mut h);
                sheet.rules().len().hash(&mut h);
                h.finish()
            };
            for (i, entry) in sheet.font_faces().iter().enumerate() {
                if !sheet.chain_matches(entry.media, &media) {
                    continue;
                }
                let Some(family) = css_family_name(entry.block.get("font-family")) else { continue };
                let Some(src) = css::parse_source(entry.block.get("src")) else { continue };
                let descriptors = css::DESCRIPTORS
                    .iter()
                    .map(|d| {
                        let raw = CSS_DESCRIPTOR_NAMES.iter().find(|(n, _)| *n == d.name).map(|(_, c)| entry.block.get(c)).unwrap_or("");
                        let value = if raw.trim().is_empty() { None } else { (d.parse)(raw) };
                        (d.name, value.unwrap_or_else(|| d.initial.to_string()))
                    })
                    .collect();
                out.push(CssRule {
                    key: format!("{handle}:{generation:x}:{i}:{}", entry.block.css_text()),
                    sheet: handle,
                    family,
                    src,
                    base: sheet.base_of(entry.base).unwrap_or(&doc_base).to_string(),
                    origin_dirty: dirty.contains(&entry.base),
                    descriptors,
                });
            }
        });
    }
    out
}

/// An `@font-face` rule's `font-family` as its face's `family` reads it (`CSSFontFamilyValue::Value`): a string's
/// contents, or the identifiers joined by one space; `None` for an unquoted generic family or CSS-wide keyword,
/// which `<family-name>` excludes (`AtRuleDescriptorParser`: `ConsumeGenericFamily`, then `ConsumeFamilyName`).
fn css_family_name(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.starts_with('"') || t.starts_with('\'') {
        return Some(css_unescape(t));
    }
    let lower = t.to_ascii_lowercase();
    if GENERIC_FAMILIES.contains(&lower.as_str()) || ["initial", "inherit", "unset", "revert", "revert-layer", "default"].contains(&lower.as_str()) {
        return None;
    }
    Some(t.split_ascii_whitespace().collect::<Vec<_>>().join(" "))
}

/// A CSS string token's value: its quotes off, its escapes resolved.
fn css_unescape(q: &str) -> String {
    let mut chars = q.chars();
    let Some(quote) = chars.next() else { return String::new() };
    let inner = chars.as_str();
    let inner = inner.strip_suffix(quote).unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.peek().copied() {
            None => {}
            Some('\n') => {
                it.next();
            }
            Some(h) if h.is_ascii_hexdigit() => {
                let mut hex = String::new();
                while hex.len() < 6 {
                    match it.peek() {
                        Some(d) if d.is_ascii_hexdigit() => {
                            hex.push(*d);
                            it.next();
                        }
                        _ => break,
                    }
                }
                if matches!(it.peek(), Some(' ' | '\t' | '\n')) {
                    it.next();
                }
                let v = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                out.push(char::from_u32(v).filter(|_| v != 0).unwrap_or('\u{FFFD}'));
            }
            Some(o) => {
                out.push(o);
                it.next();
            }
        }
    }
    out
}

/// The sheet a CSS-connected face's key names.
fn sheet_of_key(key: &str) -> Option<u32> {
    key.split(':').next()?.parse().ok()
}

/// The set's CSS-connected faces, current (`FontFaceSetDocument::CSSConnectedFontFaceList`, which brings the active
/// style up to date first). The faces stay while the rules do. A sheet APPENDED after them adds faces and keeps
/// theirs (`kActiveSheetsAppended`); any other change to the rules -- a sheet removed, switched off or edited --
/// makes every one anew (Chrome 150, measured: after `style.textContent +=` a rule, `[...document.fonts][0]` is
/// another object).
fn css_faces<'s>(scope: &mut v8::PinScope<'s, '_>, set: v8::Local<v8::Object>, s: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    let rules = css_rules(scope);
    let old = st::get_array(scope, s, "cssFaces").map(|a| st::array_items(scope, a)).unwrap_or_default();
    let old_keys: Vec<String> = st::get_array(scope, s, "cssKeys")
        .map(|a| st::array_items(scope, a))
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.to_rust_string_lossy(scope))
        .collect();
    let same_prefix = rules.len() >= old_keys.len() && rules.iter().zip(&old_keys).all(|(r, k)| r.key == *k);
    if same_prefix && rules.len() == old_keys.len() && old.len() == old_keys.len() {
        return old;
    }
    let last_sheet = old_keys.iter().filter_map(|k| sheet_of_key(k)).max();
    let appended = same_prefix && old.len() == old_keys.len() && rules[old_keys.len()..].iter().all(|r| last_sheet.is_none_or(|l| r.sheet > l));
    let keep = if appended { old_keys.len() } else { 0 };
    let mut faces: Vec<v8::Local<v8::Value>> = old.into_iter().take(keep).collect();
    let mut keys: Vec<v8::Local<v8::Value>> = Vec::with_capacity(rules.len());
    for r in &rules[..keep] {
        keys.push(str_val(scope, &r.key));
    }
    for r in &rules[keep..] {
        let Some(face) = make_css_face(scope, set, r) else { continue };
        faces.push(face.into());
        keys.push(str_val(scope, &r.key));
    }
    let arr = st::array(scope, &faces);
    st::set(scope, s, "cssFaces", arr.into());
    let arr = st::array(scope, &keys);
    st::set(scope, s, "cssKeys", arr.into());
    faces
}

/// A CSS-connected face for `rule`, in `set` (Blink: in the document's font face cache). Its family is the rule's
/// name as written, unquoted -- `"F9 Used"` reads `F9 Used`, where the constructor would quote it -- and it waits
/// `unloaded` until something loads it.
fn make_css_face<'s>(scope: &mut v8::PinScope<'s, '_>, set: v8::Local<v8::Object>, rule: &CssRule) -> Option<v8::Local<'s, v8::Object>> {
    let face = mint(scope, FACE)?;
    let s = st::attach(scope, face, FACE);
    st::set_str(scope, s, "family", &rule.family);
    st::set_str(scope, s, "src", &rule.src);
    st::set_bool(scope, s, "binary", false);
    st::set_str(scope, s, "status", "unloaded");
    for (name, value) in &rule.descriptors {
        st::set_str(scope, s, name, value);
    }
    let loaded = pending_promise(scope);
    st::set(scope, s, "loaded", loaded);
    init_load_state(scope, s);
    let sets = st::array(scope, &[set.into()]);
    st::set(scope, s, "sets", sets.into());
    st::set_str(scope, s, "base", &rule.base);
    st::set_bool(scope, s, "css", true);
    st::set_bool(scope, s, "originDirty", rule.origin_dirty);
    Some(face)
}

// ---- the set's loading state (Blink's `FontFaceSet`) ---------------------------------------------

/// `AddToLoadingFonts`: the first face to start loading makes the set `loading`, resets a settled `ready` and asks
/// for the `loading` event on a task; the set then waits on the face.
fn add_to_loading_fonts(scope: &mut v8::PinScope, set: v8::Local<v8::Object>, ss: v8::Local<v8::Object>, face: v8::Local<v8::Object>, fs: v8::Local<v8::Object>) {
    if !st::get_bool(scope, ss, "isLoading") {
        st::set_bool(scope, ss, "isLoading", true);
        st::set_bool(scope, ss, "fireLoading", true);
        if !st::get_bool(scope, ss, "readyPending") {
            let ready = pending_promise(scope);
            st::set(scope, ss, "ready", ready);
            st::set_bool(scope, ss, "readyPending", true);
        }
        handle_soon(scope, set, ss);
    }
    let mut loading = st::get_array(scope, ss, "loadingFonts").map(|a| st::array_items(scope, a)).unwrap_or_default();
    let fv: v8::Local<v8::Value> = face.into();
    if loading.contains(&fv) {
        return;
    }
    loading.push(fv);
    let arr = st::array(scope, &loading);
    st::set(scope, ss, "loadingFonts", arr.into());
    add_callback(scope, face, fs, set.into());
}

/// `RemoveFromLoadingFonts`: the set stops waiting on the face; with nothing left, it settles on a task.
fn remove_from_loading_fonts(scope: &mut v8::PinScope, set: v8::Local<v8::Object>, ss: v8::Local<v8::Object>, face: v8::Local<v8::Value>) {
    let mut loading = st::get_array(scope, ss, "loadingFonts").map(|a| st::array_items(scope, a)).unwrap_or_default();
    if let Some(i) = loading.iter().position(|f| *f == face) {
        loading.remove(i);
    }
    let empty = loading.is_empty();
    let arr = st::array(scope, &loading);
    st::set(scope, ss, "loadingFonts", arr.into());
    if empty {
        handle_soon(scope, set, ss);
    }
}

/// `ShouldSignalReady`: nothing loading, and a `loadingdone` or a `ready` still owed.
fn should_signal_ready(scope: &mut v8::PinScope, ss: v8::Local<v8::Object>) -> bool {
    let idle = st::get_array(scope, ss, "loadingFonts").is_none_or(|a| a.length() == 0);
    idle && (st::get_bool(scope, ss, "isLoading") || st::get_bool(scope, ss, "readyPending"))
}

/// `HandlePendingEventsAndPromisesSoon`: one task (the font-loading task source) at a time.
fn handle_soon(scope: &mut v8::PinScope, set: v8::Local<v8::Object>, ss: v8::Local<v8::Object>) {
    if st::get_bool(scope, ss, "pendingTask") {
        return;
    }
    st::set_bool(scope, ss, "pendingTask", true);
    let data: v8::Local<v8::Value> = v8::Local::new(scope, set).into();
    let f = build_fn_data(scope, cb_handle_pending, "", 0, data);
    crate::core::intersection_observer::rendering::queue_task(scope, f, 0.0);
}

/// `HandlePendingEventsAndPromises`: the `loading` event if one is owed, then `FireDoneEventIfPossible` -- which
/// brings style and layout up to date first (they may start more loads) and fires nothing while a face loads or a
/// `loading` event is still owed.
fn cb_handle_pending(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let data = crate::binding::protos::body_data(scope, &args);
    let Ok(set) = v8::Local::<v8::Object>::try_from(data) else { return };
    let Some(ss) = st::state(scope, set.into(), FACESET) else { return };
    st::set_bool(scope, ss, "pendingTask", false);
    if st::get_bool(scope, ss, "fireLoading") {
        st::set_bool(scope, ss, "fireLoading", false);
        dispatch_load_event(scope, set, "loading", &[]);
    }
    if !should_signal_ready(scope, ss) {
        return;
    }
    update_style_and_layout(scope);
    if st::get_bool(scope, ss, "fireLoading") || !should_signal_ready(scope, ss) {
        return;
    }
    fire_done_event(scope, set, ss);
}

/// `FireDoneEvent`: `loadingdone` with the faces that loaded, `loadingerror` with those that failed (when any),
/// the set `loaded`; then `ready` resolves with the set.
fn fire_done_event(scope: &mut v8::PinScope, set: v8::Local<v8::Object>, ss: v8::Local<v8::Object>) {
    if st::get_bool(scope, ss, "isLoading") {
        let loaded = st::get_array(scope, ss, "loadedFonts").map(|a| st::array_items(scope, a)).unwrap_or_default();
        let failed = st::get_array(scope, ss, "failedFonts").map(|a| st::array_items(scope, a)).unwrap_or_default();
        for name in ["loadedFonts", "failedFonts"] {
            let empty = st::array(scope, &[]);
            st::set(scope, ss, name, empty.into());
        }
        st::set_bool(scope, ss, "isLoading", false);
        dispatch_load_event(scope, set, "loadingdone", &loaded);
        if !failed.is_empty() {
            dispatch_load_event(scope, set, "loadingerror", &failed);
        }
    }
    if st::get_bool(scope, ss, "readyPending") {
        st::set_bool(scope, ss, "readyPending", false);
        if let Some(r) = ready_resolver(scope, ss) {
            r.resolve(scope, set.into());
        }
    }
}

/// The resolver of the set's pending `ready` (made by [`pending_promise`], see [`resolver_of`]).
fn ready_resolver<'s>(scope: &mut v8::PinScope<'s, '_>, ss: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::PromiseResolver>> {
    let v = st::get(scope, ss, "ready");
    if !v.is_promise() {
        return None;
    }
    Some(unsafe { v8::Local::<v8::PromiseResolver>::cast_unchecked(v) })
}

/// A trusted `FontFaceSetLoadEvent` of type `ty` at the set, its `fontfaces` the faces given.
fn dispatch_load_event(scope: &mut v8::PinScope, set: v8::Local<v8::Object>, ty: &str, faces: &[v8::Local<v8::Value>]) {
    let Some(ctor) = crate::binding::protos::iface_object(scope, "FontFaceSetLoadEvent").and_then(|v| v8::Local::<v8::Function>::try_from(v).ok()) else {
        return;
    };
    let init = crate::binding::protos::init_dict(scope);
    let list = st::array(scope, faces);
    crate::binding::protos::set_prop(scope, init, "fontfaces", list.into());
    let t = str_val(scope, ty);
    let Some(ev) = ctor.new_instance(scope, &[t, init.into()]) else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let target: v8::Local<v8::Value> = v8::Local::new(scope, set).into();
    crate::core::events::event_target::et_dispatch(scope, target, ev);
}

/// `Document::UpdateStyleAndLayout` as far as fonts go: the style-induced loads a rendering opportunity would
/// start (`core/css/sheet_loads.rs`), the CSS-connected faces layout needs among them.
fn update_style_and_layout(scope: &mut v8::PinScope) {
    crate::core::css::sheet_loads::sheet_reparsed(scope);
}

// ---- the set's members ---------------------------------------------------------------------------

/// `size`: the CSS-connected faces and the ones script added.
fn cb_set_size(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let n = css_faces(scope, args.this(), s).len() + faces_of(scope, s).len();
    rv.set(v8::Integer::new_from_unsigned(scope, n as u32).into());
}

/// `loading` from the moment a face of the set starts loading to the task that fires `loadingdone`, else `loaded`
/// (Blink's `is_loading_`). The brand check still runs: `status` on a foreign receiver is `Illegal invocation`.
fn cb_set_status(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let loading = st::get_bool(scope, s, "isLoading");
    rv.set(str_val(scope, if loading { "loading" } else { "loaded" }));
}

/// `ready` (`FontFaceSetDocument::ready`): a settled one first brings style and layout up to date, which may start
/// loads and replace it with a pending one.
fn cb_set_ready(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    if !st::get_bool(scope, s, "readyPending") {
        update_style_and_layout(scope);
    }
    rv.set(st::get(scope, s, "ready"));
}

/// The parts of a face that `check()` and `load()` match on (Blink's `FontFaceCache` entry): its family
/// (unquoted; families compare case-insensitively), its capabilities -- width, slope and weight ranges --, its
/// `unicode-range`, and whether it has loaded.
struct FaceInfo {
    family: String,
    caps: Caps,
    ranges: Vec<(u32, u32)>,
    loaded: bool,
}

/// `FontSelectionCapabilities`: `(min, max)` of width (percent), slope (degrees) and weight.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Caps {
    width: (f32, f32),
    slope: (f32, f32),
    weight: (f32, f32),
}

/// `FontSelectionRequest`.
#[derive(Clone, Copy, Debug)]
struct Request {
    width: f32,
    slope: f32,
    weight: f32,
}

/// Blink's `kItalicThreshold`/`kItalicSlopeValue` (14 degrees) and the weight search thresholds.
const ITALIC_SLOPE: f32 = 14.0;
const LOWER_WEIGHT_SEARCH: f32 = 400.0;
const UPPER_WEIGHT_SEARCH: f32 = 500.0;

/// A face's family as the font face cache keys it (`familyNameUnquoted`).
fn family_key(family: &str) -> String {
    let t = family.trim();
    let name = if t.starts_with('"') || t.starts_with('\'') { css_unescape(t) } else { t.to_string() };
    name.to_lowercase()
}

fn face_info(scope: &mut v8::PinScope, face: v8::Local<v8::Value>) -> Option<FaceInfo> {
    let s = st::state(scope, face, FACE)?;
    let family = family_key(&st::get_str(scope, s, "family"));
    let caps = Caps {
        width: stretch_caps(&st::get_str(scope, s, "stretch")),
        slope: style_caps(&st::get_str(scope, s, "style")),
        weight: weight_caps(&st::get_str(scope, s, "weight")),
    };
    let ranges = unicode_ranges(&st::get_str(scope, s, "unicodeRange"));
    let loaded = st::get_str(scope, s, "status") == "loaded";
    Some(FaceInfo { family, caps, ranges, loaded })
}

/// A stretch keyword's percentage (`font-stretch`).
fn stretch_keyword(k: &str) -> Option<f32> {
    Some(match k {
        "ultra-condensed" => 50.0,
        "extra-condensed" => 62.5,
        "condensed" => 75.0,
        "semi-condensed" => 87.5,
        "normal" => 100.0,
        "semi-expanded" => 112.5,
        "expanded" => 125.0,
        "extra-expanded" => 150.0,
        "ultra-expanded" => 200.0,
        _ => return None,
    })
}

/// A `(min, max)` from one or two values, swapped when decreasing (CSS Fonts 4 §4: user agents swap a decreasing
/// range); `None` when a value does not parse.
fn range_of(v: &str, one: impl Fn(&str) -> Option<f32>) -> Option<(f32, f32)> {
    let parts: Vec<&str> = v.split_ascii_whitespace().collect();
    match parts.as_slice() {
        [a] => one(*a).map(|x| (x, x)),
        [a, b] => {
            let (x, y) = (one(*a)?, one(*b)?);
            Some((x.min(y), x.max(y)))
        }
        _ => None,
    }
}

/// `GetFontSelectionCapabilities`' width: a keyword or a percentage, or a range of them; normal otherwise.
fn stretch_caps(v: &str) -> (f32, f32) {
    let one = |p: &str| stretch_keyword(p).or_else(|| p.strip_suffix('%').and_then(|n| n.parse::<f32>().ok()));
    range_of(&v.to_ascii_lowercase(), one).unwrap_or((100.0, 100.0))
}

/// `GetFontSelectionCapabilities`' slope: `italic` and a bare `oblique` 14 degrees, `oblique <angle>{1,2}` the
/// angles; `normal` and `auto` 0.
fn style_caps(v: &str) -> (f32, f32) {
    let lower = v.trim().to_ascii_lowercase();
    if lower == "italic" || lower == "oblique" {
        return (ITALIC_SLOPE, ITALIC_SLOPE);
    }
    if let Some(angles) = lower.strip_prefix("oblique ") {
        let angle = |p: &str| {
            let num = |s: &str, u: &str| s.strip_suffix(u).and_then(|n| n.parse::<f32>().ok());
            num(p, "deg")
                .or_else(|| num(p, "grad").map(|g| g * 0.9))
                .or_else(|| num(p, "rad").map(|r| r.to_degrees()))
                .or_else(|| num(p, "turn").map(|t| t * 360.0))
        };
        if let Some(r) = range_of(angles, angle) {
            return r;
        }
    }
    (0.0, 0.0)
}

/// `GetFontSelectionCapabilities`' weight: `bold` 700, a number or a range of numbers within 1..=1000; normal (400)
/// otherwise.
fn weight_caps(v: &str) -> (f32, f32) {
    let lower = v.trim().to_ascii_lowercase();
    if lower == "bold" {
        return (700.0, 700.0);
    }
    let one = |p: &str| p.parse::<f32>().ok().filter(|n| (1.0..=1000.0).contains(n));
    range_of(&lower, one).unwrap_or((400.0, 400.0))
}

/// `unicode-range` as inclusive code point ranges: `U+0-7F`, `U+4E00`, `U+4??`.
fn unicode_ranges(text: &str) -> Vec<(u32, u32)> {
    text.split(',')
        .filter_map(|part| {
            let t = part.trim();
            let t = t.strip_prefix("U+").or_else(|| t.strip_prefix("u+"))?;
            if let Some((a, b)) = t.split_once('-') {
                return Some((u32::from_str_radix(a, 16).ok()?, u32::from_str_radix(b, 16).ok()?));
            }
            if t.contains('?') {
                let lo = u32::from_str_radix(&t.replace('?', "0"), 16).ok()?;
                let hi = u32::from_str_radix(&t.replace('?', "F"), 16).ok()?;
                return Some((lo, hi));
            }
            let v = u32::from_str_radix(t, 16).ok()?;
            Some((v, v))
        })
        .collect()
}

fn covers(ranges: &[(u32, u32)], c: char) -> bool {
    ranges.is_empty() || ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&(c as u32)))
}

/// The `FontSelectionRequest` a `font` shorthand makes (`ResolveFontStyle` over the default 10px sans-serif): its
/// weight -- `bold` 700, `bolder` and `lighter` from the default 400 (700, 100), a number --, its width -- a
/// `font-stretch` keyword -- and its slope -- `italic` and `oblique` 14 degrees.
fn font_request(font: &str) -> Request {
    let t = css::ws(font);
    let mut r = Request { width: 100.0, slope: 0.0, weight: 400.0 };
    for p in t.split(' ').take(4) {
        let lower = p.to_ascii_lowercase();
        match lower.as_str() {
            "normal" | "small-caps" => {}
            "italic" | "oblique" => r.slope = ITALIC_SLOPE,
            "bold" | "bolder" => r.weight = 700.0,
            "lighter" => r.weight = 100.0,
            k => {
                if let Some(w) = stretch_keyword(k) {
                    r.width = w;
                } else if let Some(n) = k.parse::<f32>().ok().filter(|n| (1.0..=1000.0).contains(n)) {
                    r.weight = n;
                } else {
                    break;
                }
            }
        }
    }
    r
}

/// `FontSelectionAlgorithm::StretchDistance`.
fn stretch_distance(c: &Caps, req: &Request, bounds: &Caps) -> f32 {
    let (lo, hi) = c.width;
    if (lo..=hi).contains(&req.width) {
        return 0.0;
    }
    if req.width > 100.0 {
        if lo > req.width {
            return lo - req.width;
        }
        let threshold = req.width.max(bounds.width.1);
        return threshold - hi;
    }
    if hi < req.width {
        return req.width - hi;
    }
    let threshold = req.width.min(bounds.width.0);
    lo - threshold
}

/// `FontSelectionAlgorithm::StyleDistance`.
fn style_distance(c: &Caps, req: &Request, bounds: &Caps) -> f32 {
    let (lo, hi) = c.slope;
    let s = req.slope;
    if (lo..=hi).contains(&s) {
        return 0.0;
    }
    if s >= ITALIC_SLOPE {
        if lo > s {
            return lo - s;
        }
        return s.max(bounds.slope.1) - hi;
    }
    if s >= 0.0 {
        if hi >= 0.0 && hi < s {
            return s - hi;
        }
        if lo > s {
            return lo;
        }
        return s.max(bounds.slope.1) - hi;
    }
    if s > -ITALIC_SLOPE {
        if lo > s && lo <= 0.0 {
            return lo - s;
        }
        if hi < s {
            return -hi;
        }
        return lo - s.min(bounds.slope.0);
    }
    if hi < s {
        return s - hi;
    }
    lo - s.min(bounds.slope.0)
}

/// `FontSelectionAlgorithm::WeightDistance`.
fn weight_distance(c: &Caps, req: &Request, bounds: &Caps) -> f32 {
    let (lo, hi) = c.weight;
    let w = req.weight;
    if (lo..=hi).contains(&w) {
        return 0.0;
    }
    if (LOWER_WEIGHT_SEARCH..=UPPER_WEIGHT_SEARCH).contains(&w) {
        if lo > w && lo <= UPPER_WEIGHT_SEARCH {
            return lo - w;
        }
        if hi < w {
            return UPPER_WEIGHT_SEARCH - hi;
        }
        return lo - w.min(bounds.weight.0);
    }
    if w < LOWER_WEIGHT_SEARCH {
        if hi < w {
            return w - hi;
        }
        return lo - w.min(bounds.weight.0);
    }
    if lo > w {
        return lo - w;
    }
    w.max(bounds.weight.1) - hi
}

/// `FontSelectionAlgorithm::IsBetterMatchForRequest`: stretch first, then style, then weight.
fn better_match(first: &Caps, second: &Caps, req: &Request, bounds: &Caps) -> bool {
    for distance in [stretch_distance, style_distance, weight_distance] {
        let (a, b) = (distance(first, req, bounds), distance(second, req, bounds));
        if a < b {
            return true;
        }
        if a > b {
            return false;
        }
    }
    false
}

/// `FontFaceCache::Get(description, family)`: the faces of the family whose capabilities match the request best
/// -- one `CSSSegmentedFontFace` per distinct capabilities, the first best of them -- as indices into `infos`, the
/// CSS-connected ones first; `None` when no face has the family.
fn segmented(infos: &[FaceInfo], family: &str, req: &Request) -> Option<Vec<usize>> {
    let mut groups: Vec<(Caps, Vec<usize>)> = Vec::new();
    for (k, info) in infos.iter().enumerate() {
        if info.family != family {
            continue;
        }
        match groups.iter_mut().find(|(c, _)| *c == info.caps) {
            Some((_, members)) => members.push(k),
            None => groups.push((info.caps, vec![k])),
        }
    }
    let first = groups.first()?.0;
    let mut bounds = first;
    for (c, _) in &groups {
        bounds.width = (bounds.width.0.min(c.width.0), bounds.width.1.max(c.width.1));
        bounds.slope = (bounds.slope.0.min(c.slope.0), bounds.slope.1.max(c.slope.1));
        bounds.weight = (bounds.weight.0.min(c.weight.0), bounds.weight.1.max(c.weight.1));
    }
    let mut best = 0;
    for i in 1..groups.len() {
        if better_match(&groups[i].0, &groups[best].0, req, &bounds) {
            best = i;
        }
    }
    Some(groups.swap_remove(best).1)
}

/// A family of the `font` list as the set looks it up: `None` for a generic keyword (`FamilyIsGeneric`), else its
/// cache key.
fn named_family(f: &str) -> Option<String> {
    let t = f.trim();
    let quoted = t.starts_with('"') || t.starts_with('\'');
    if !quoted && GENERIC_FAMILIES.contains(&t.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(family_key(t))
}

/// `IsPlatformFamilyMatchAvailable`: does the persona have an installed font of this family (its captured font
/// map, the one layout measures with)?
fn platform_family_available(scope: &mut v8::PinScope, family: &str) -> bool {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    let key = crate::core::dom::geometry_queries::media_context(&st).theme.font_map;
    page::font::resolve_face_in(page::font::font_map(key), &format!("\"{}\"", family.replace('"', ""))).is_some()
}

/// `check(font, text = " ")`: false when, for a character of the text, a family of the font that is neither
/// generic nor installed has faces in the set and one of the best-matching ones covers the character and has not
/// loaded (`FontFaceSet::check` -> `CSSSegmentedFontFace::CheckFont`). Chrome 150, measured (F9): an unloaded
/// `@font-face` family is false, a family no face names true, `bold italic 16px X` with text outside X's
/// `unicode-range` true.
fn cb_set_check(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let Some(font) = st::to_str(scope, args.get(0)) else { return };
    let text = if args.length() > 1 && !args.get(1).is_undefined() {
        let Some(t) = st::to_str(scope, args.get(1)) else { return };
        t
    } else {
        " ".to_string()
    };
    let Some(families) = css::parse_font_shorthand(&font) else {
        throw_dom(
            scope,
            &format!("Failed to execute 'check' on 'FontFaceSet': Could not resolve '{font}' as a font."),
            "SyntaxError",
        );
        return;
    };
    let req = font_request(&font);
    let faces = all_faces(scope, args.this(), s);
    let infos: Vec<FaceInfo> = faces.iter().filter_map(|f| face_info(scope, *f)).collect();
    let mut available: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    for c in text.chars() {
        for f in &families {
            let Some(family) = named_family(f) else { continue };
            let installed = match available.get(&family) {
                Some(v) => *v,
                None => {
                    let v = platform_family_available(scope, &family);
                    available.insert(family.clone(), v);
                    v
                }
            };
            if installed {
                continue;
            }
            if let Some(group) = segmented(&infos, &family, &req) {
                if group.iter().any(|&k| !infos[k].loaded && covers(&infos[k].ranges, c)) {
                    rv.set_bool(false);
                    return;
                }
            }
        }
    }
    rv.set_bool(true);
}

/// `load(font, text = " ")`: the best-matching faces of each named family whose `unicode-range` meets the text
/// (`CSSSegmentedFontFace::Match`), each loaded (`LoadWithCallback`); the promise resolves with them once all have
/// loaded -- at once when there are none, or none left loading -- and rejects with the first one's error
/// (`LoadFontPromiseResolver`). An unparseable font is the grammar's `SyntaxError`.
fn cb_set_load(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let Some(font) = st::to_str(scope, args.get(0)) else { return };
    let text = if args.length() > 1 && !args.get(1).is_undefined() {
        let Some(t) = st::to_str(scope, args.get(1)) else { return };
        t
    } else {
        " ".to_string()
    };
    let Some(families) = css::parse_font_shorthand(&font) else {
        let Some(err) = make_dom_exc(
            scope,
            &format!("Failed to execute 'load' on 'FontFaceSet': Could not resolve '{font}' as a font."),
            "SyntaxError",
        ) else {
            return;
        };
        rv.set(rejected_promise(scope, err));
        return;
    };
    let req = font_request(&font);
    let faces = all_faces(scope, args.this(), s);
    let infos: Vec<(usize, FaceInfo)> = faces.iter().enumerate().filter_map(|(i, f)| face_info(scope, *f).map(|info| (i, info))).collect();
    let (index, infos): (Vec<usize>, Vec<FaceInfo>) = infos.into_iter().unzip();
    let mut chosen: Vec<v8::Local<v8::Value>> = Vec::new();
    for f in &families {
        let Some(family) = named_family(f) else { continue };
        if let Some(group) = segmented(&infos, &family, &req) {
            for k in group {
                if !text.is_empty() && text.chars().any(|c| covers(&infos[k].ranges, c)) {
                    chosen.push(faces[index[k]]);
                }
            }
        }
    }
    let Some(resolver) = v8::PromiseResolver::new(scope) else { return };
    let promise = resolver.get_promise(scope);
    let list = st::array(scope, &chosen);
    if chosen.is_empty() {
        resolver.resolve(scope, list.into());
        rv.set(promise.into());
        return;
    }
    let holder = v8::Object::new(scope);
    let rs = st::attach(scope, holder, LOAD_RESOLVER);
    st::set(scope, rs, "promise", promise.into());
    st::set(scope, rs, "faces", list.into());
    st::set_num(scope, rs, "numLoading", chosen.len() as f64);
    st::set_bool(scope, rs, "errorOccured", false);
    for f in chosen {
        let (Ok(face), Some(fs)) = (v8::Local::<v8::Object>::try_from(f), st::state(scope, f, FACE)) else { continue };
        start_load(scope, face, fs);
        add_callback(scope, face, fs, holder.into());
    }
    rv.set(promise.into());
}

/// `add(face)`: a face already in the set -- added, or CSS-connected -- stays where it is; a new one joins the
/// end, and the set waits on it if it is loading (`FontFaceSet::addForBinding`).
fn cb_set_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let face = args.get(0);
    if !st::is_a(scope, face, FACE) {
        throw_type_error(scope, "Failed to execute 'add' on 'FontFaceSet': parameter 1 is not of type 'FontFace'.");
        return;
    }
    let this = args.this();
    let mut items = faces_of(scope, s);
    let css = css_faces(scope, this, s);
    if index_of(scope, &items, face).is_none() && index_of(scope, &css, face).is_none() {
        items.push(face);
        set_faces(scope, s, &items);
        if let (Ok(fobj), Some(fs)) = (v8::Local::<v8::Object>::try_from(face), st::state(scope, face, FACE)) {
            push_to(scope, fs, "sets", this.into());
            if st::get_str(scope, fs, "status") == "loading" {
                add_to_loading_fonts(scope, this, s, fobj, fs);
            }
        }
    }
    rv.set(this.into());
}

/// Take `set` out of the sets a face is in.
fn leave_set(scope: &mut v8::PinScope, face: v8::Local<v8::Value>, set: v8::Local<v8::Object>) {
    let Some(fs) = st::state(scope, face, FACE) else { return };
    let setv: v8::Local<v8::Value> = set.into();
    let sets: Vec<v8::Local<v8::Value>> = st::get_array(scope, fs, "sets").map(|a| st::array_items(scope, a)).unwrap_or_default().into_iter().filter(|v| *v != setv).collect();
    let arr = st::array(scope, &sets);
    st::set(scope, fs, "sets", arr.into());
}

/// `delete(face)`: only a face script added can leave; a CSS-connected one answers false and stays
/// (`FontFaceSet::deleteForBinding`; Chrome 150, measured).
fn cb_set_delete(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let face = args.get(0);
    let mut items = faces_of(scope, s);
    match index_of(scope, &items, face) {
        Some(i) => {
            items.remove(i);
            set_faces(scope, s, &items);
            let this = args.this();
            leave_set(scope, face, this);
            if st::state(scope, face, FACE).is_some_and(|fs| st::get_str(scope, fs, "status") == "loading") {
                remove_from_loading_fonts(scope, this, s, face);
            }
            rv.set_bool(true);
        }
        None => rv.set_bool(false),
    }
}

/// `clear()`: the faces script added leave; the CSS-connected ones stay.
fn cb_set_clear(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let this = args.this();
    let items = faces_of(scope, s);
    if items.is_empty() {
        return;
    }
    set_faces(scope, s, &[]);
    for face in items {
        leave_set(scope, face, this);
        if st::state(scope, face, FACE).is_some_and(|fs| st::get_str(scope, fs, "status") == "loading") {
            remove_from_loading_fonts(scope, this, s, face);
        }
    }
}

/// `has(face)`: added by script, or CSS-connected.
fn cb_set_has(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let face = args.get(0);
    let items = faces_of(scope, s);
    if index_of(scope, &items, face).is_some() {
        rv.set_bool(true);
        return;
    }
    let css = css_faces(scope, args.this(), s);
    rv.set_bool(index_of(scope, &css, face).is_some());
}

/// `forEach(cb, thisArg)` over a SNAPSHOT of the faces, CSS-connected first, so a callback that adds or removes
/// does not disturb the walk (`CreateIterationSource` takes one too).
fn cb_set_for_each<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    // A setlike's callback is `(value, key, set)` with value === key.
    let faces = all_faces(scope, args.this(), s);
    crate::core::dom::webidl_iter::for_each(scope, &args, "FontFaceSet", |_, i| faces.get(i).map(|f| (*f, *f)));
}

/// `keys` / `values` / `entries`. The iterator is a bare object on ONE SHARED iterator prototype that
/// carries `next`, `@@iterator` and `@@toStringTag` -- so the object a page is handed has NO own
/// property, which is what Chrome does and what the JS this replaces did not.
///
/// Measured, headless Chrome 150.0.7871.46 over a loopback server:
///     Object.prototype.toString.call(document.fonts.keys())   "[object FontFaceSet Iterator]"
///     Object.getOwnPropertyNames(document.fonts.keys())       []
/// The JS put `next` and `@@iterator` on the iterator ITSELF, so `getOwnPropertyNames` answered
/// `["next"]` -- an own property parked on an object handed to a page, which is the shape the quality
/// gate counts as a tell everywhere else in this tree. The capture records no entry for the iterator
/// prototype (it is not an interface), so Chrome is the only source for this and it is cited here.
///
/// `data` selects the projection: `"pair"` for `entries` (`[face, face]`), anything else for the face.
fn cb_set_iterator(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), FACESET) else { return };
    let pair = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope) == "pair";
    // a snapshot, the CSS-connected faces first (`FontFaceSet::CreateIterationSource`)
    let items: Vec<v8::Local<v8::Value>> = all_faces(scope, args.this(), s)
        .into_iter()
        .map(|f| if pair { st::array(scope, &[f, f]).into() } else { f })
        .collect();
    let list = st::array(scope, &items);
    let Some(proto) = iterator_proto(scope) else { return };
    let wrap = crate::binding::protos::mint_on(scope, proto.into());
    // The cursor and the list live in the iterator's own PRIVATE slot, never as own properties.
    let state = st::attach(scope, wrap, ITERATOR);
    st::set(scope, state, "items", list.into());
    st::set_num(scope, state, "i", 0.0);
    rv.set(wrap.into());
}

/// The one shared `%FontFaceSetIterator.prototype%` of this realm, built on first use and parked in a
/// private slot on `FontFaceSet.prototype` -- so every iterator this realm mints has the same
/// `[[Prototype]]`, as Chrome's do.
fn iterator_proto<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let host = shell_proto(scope, FACESET)?;
    if let Some(s) = st::state(scope, host.into(), ITERATOR_PROTO) {
        let v = st::get(scope, s, "proto");
        if v.is_object() {
            return v8::Local::<v8::Object>::try_from(v).ok();
        }
    }
    let p = v8::Object::new(scope);
    let next = build_fn(scope, cb_iterator_next, "next", 0);
    crate::binding::protos::set_prop(scope, p, "next", next.into());
    let self_fn = build_fn(scope, cb_iterator_self, "[Symbol.iterator]", 0);
    let key = v8::Symbol::get_iterator(scope);
    p.set(scope, key.into(), self_fn.into());
    let tag_key = v8::Symbol::get_to_string_tag(scope);
    let tag = str_val(scope, "FontFaceSet Iterator");
    let mut d = v8::PropertyDescriptor::new_from_value_writable(tag, false);
    d.set_enumerable(false);
    d.set_configurable(true);
    p.define_property(scope, tag_key.into(), &d);
    let s = st::attach(scope, host, ITERATOR_PROTO);
    st::set(scope, s, "proto", p.into());
    Some(p)
}

fn cb_iterator_next(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), ITERATOR) else { return };
    let i = st::get_num(scope, s, "i") as usize;
    let items = match st::get_array(scope, s, "items") {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    };
    let out = v8::Object::new(scope);
    if i < items.len() {
        st::set_num(scope, s, "i", (i + 1) as f64);
        st::set(scope, out, "value", items[i]);
        st::set_bool(scope, out, "done", false);
    } else {
        let u = v8::undefined(scope).into();
        st::set(scope, out, "value", u);
        st::set_bool(scope, out, "done", true);
    }
    rv.set(out.into());
}

fn cb_iterator_self(_scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    rv.set(args.this().into());
}

pub fn install_font_face_set(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "size" => {
            let g = build_fn(scope, cb_set_size, "get size", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "status" => {
            let g = build_fn(scope, cb_set_status, "get status", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "ready" => {
            let g = build_fn(scope, cb_set_ready, "get ready", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "check" => method_row(scope, proto, member, cb_set_check),
        "load" => method_row(scope, proto, member, cb_set_load),
        "add" => method_row(scope, proto, member, cb_set_add),
        "delete" => method_row(scope, proto, member, cb_set_delete),
        "clear" => method_row(scope, proto, member, cb_set_clear),
        "has" => method_row(scope, proto, member, cb_set_has),
        "forEach" => method_row(scope, proto, member, cb_set_for_each),
        "keys" | "values" | "entries" => {
            let d = str_val(scope, if member.name == "entries" { "pair" } else { "value" });
            let f = build_fn_data(scope, cb_set_iterator, member.name, member.length as i32, d);
            crate::install::kits::define_value(scope, proto, member, f.into(), true);
        }
        other => debug_assert!(false, "install_font_face_set: unexpected member {other}"),
    }
}

// ---- `document.fonts` ----------------------------------------------------------------------------

/// ONE `FontFaceSet` per document, created on first read and stable thereafter.
///
/// This is the one member of this lane on an interface it does not own: the BODY is this file's,
/// because minting a set means minting its state -- the same arrangement
/// `core/media/media_stream.rs` has for `HTMLCanvasElement.captureStream`. The document's set is kept
/// in the document's own private slot, so it is per-document without a `WeakMap`.
///
/// THE ROW IS `Document`'S OWN TABLE ROW, naming this callback. It used to be a `stub`
/// row that `install_document_fonts` defined over from `install_window_media_rtc` -- "the row is the
/// SHAPE and a later pass supplies the BODY", which is the arrangement invariant I2's ledger calls a
/// second writer: an unbranded `build_fn` in a slot the ledger claims, reported `clobber:engine` by
/// `installer::audit_surface`. Nothing about the body or its home moved; the mount did, and that pass
/// is deleted.
pub(crate) fn cb_document_fonts(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    if let Some(set) = document_set(scope, args.this()) {
        rv.set(set.into());
    }
}

/// `doc`'s set if it has one yet.
fn existing_document_set<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let s = st::state(scope, doc.into(), DOC_SLOT)?;
    let v = st::get(scope, s, "set");
    v8::Local::<v8::Object>::try_from(v).ok()
}

/// `doc`'s set, made on first need (Blink makes it with the document: `FontFaceSetDocument::From`).
fn document_set<'s>(scope: &mut v8::PinScope<'s, '_>, doc: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(set) = existing_document_set(scope, doc) {
        return Some(set);
    }
    let set = mint_face_set(scope, doc)?;
    let s = st::attach(scope, doc, DOC_SLOT);
    st::set(scope, s, "set", set.into());
    Some(set)
}

/// Layout needs these CSS-connected faces -- `(sheet, rule)` of each, the index of the rule among its sheet's
/// `@font-face` rules -- for characters in their `unicode-range` (`core/css/sheet_loads.rs`'s scan): each still
/// `unloaded` loads (`CSSFontFace::MaybeLoadFont` -> `Load`), and the page's set waits on it.
pub(crate) fn css_faces_used(scope: &mut v8::PinScope, used: &[(u32, u32)]) {
    if used.is_empty() {
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let Some(doc) = crate::binding::protos::document_val(scope, &op_state) else { return };
    let Ok(doc) = v8::Local::<v8::Object>::try_from(doc) else { return };
    let Some(set) = document_set(scope, doc) else { return };
    let Some(ss) = st::state(scope, set.into(), FACESET) else { return };
    let faces = css_faces(scope, set, ss);
    let keys = st::get_array(scope, ss, "cssKeys").map(|a| st::array_items(scope, a)).unwrap_or_default();
    for (face, key) in faces.into_iter().zip(keys) {
        let key = key.to_rust_string_lossy(scope);
        let mut parts = key.splitn(4, ':');
        let sheet = parts.next().and_then(|v| v.parse::<u32>().ok());
        let rule = parts.nth(1).and_then(|v| v.parse::<u32>().ok());
        let (Some(sheet), Some(rule)) = (sheet, rule) else { continue };
        if !used.contains(&(sheet, rule)) {
            continue;
        }
        let (Ok(f), Some(fs)) = (v8::Local::<v8::Object>::try_from(face), st::state(scope, face, FACE)) else { continue };
        start_load(scope, f, fs);
    }
}

/// `FontFaceSetDocument::DidLayout(document)` as Blink runs it once `doc`'s load event has run
/// (`Document::ImplicitClose`): its set, when nothing of it is loading, settles on the next task -- `loadingdone`
/// if a load is owed one, then `ready`. Chrome 150, measured (F9): `ready` read at parse time resolves after the
/// page's `load` listeners have run.
pub(crate) fn load_event_finished(scope: &mut v8::PinScope, doc: v8::Local<v8::Value>) {
    let Ok(doc) = v8::Local::<v8::Object>::try_from(doc) else { return };
    let Some(set) = existing_document_set(scope, doc) else { return };
    let Some(ss) = st::state(scope, set.into(), FACESET) else { return };
    if should_signal_ready(scope, ss) {
        handle_soon(scope, set, ss);
    }
}

/// [`load_event_finished`] for the page's own document (`webapi::page::load_event_finished`).
pub(crate) fn page_load_finished(scope: &mut v8::PinScope) {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if let Some(doc) = crate::binding::protos::document_val(scope, &op_state) {
        load_event_finished(scope, doc);
    }
}

// ---- the tables ----------------------------------------------------------------------------------

webapi::interface! {
    "FontFace" [value, both] {
    ctor "FontFace" len 2                              = stub,
    attr "family" : Object                             = install_font_face,
    attr "style" : Object                              = install_font_face,
    attr "weight" : Object                             = install_font_face,
    attr "stretch" : Object                            = install_font_face,
    attr "unicodeRange" : Object                       = install_font_face,
    attr "variant" : Object                            = install_font_face,
    attr "featureSettings" : Object                    = install_font_face,
    attr "display" : Object                            = install_font_face,
    attr "ascentOverride" : Object                     = install_font_face,
    attr "descentOverride" : Object                    = install_font_face,
    attr "lineGapOverride" : Object                    = install_font_face,
    attr "sizeAdjust" : Object                         = install_font_face,
    attr "status" : Object readonly                    = install_font_face,
    attr "loaded" : Promise readonly                   = install_font_face,
    method "load" : Promise                            = install_font_face,
    attr "variationSettings" : Object                  = install_font_face
    }
}

// `required` is copied VERBATIM from the registry rows, webref's numbers and not the spec's reading:
// `forEach` and `has` declare 0 (they come from the setlike declaration), `check`, `load`, `add` and
// `delete` declare 1. A table row's arity guard is the registry's, so copying keeps the surface
// identical -- a "fix" here would be this file inventing a behaviour the capture never recorded.
webapi::interface! {
    "FontFaceSet" : "EventTarget" [interface, both] {
    ctor    "FontFaceSet" len 0                        = stub,
    handler "onloading" nullable                       = event_handler,
    handler "onloadingdone" nullable                   = event_handler,
    handler "onloadingerror" nullable                  = event_handler,
    attr    "ready" : Promise readonly                 = install_font_face_set,
    attr    "status" : Object readonly                 = install_font_face_set,
    attr    "size" : Object readonly                   = install_font_face_set,
    method  "check" : Object len 1 required 1          = install_font_face_set,
    method  "load" : Promise len 1 required 1          = install_font_face_set,
    method  "add" : Object len 1 required 1            = install_font_face_set,
    method  "clear" : Object                           = install_font_face_set,
    method  "delete" : Object len 1 required 1         = install_font_face_set,
    method  "entries" : Object                         = install_font_face_set,
    method  "forEach" : Object len 1                   = install_font_face_set,
    method  "has" : Object len 1                       = install_font_face_set,
    method  "keys" : Object                            = install_font_face_set,
    method  "values" : Object                          = install_font_face_set,
    method "@@iterator" : Object len 0 hidden cfg "values" = own_alias
    }
}
