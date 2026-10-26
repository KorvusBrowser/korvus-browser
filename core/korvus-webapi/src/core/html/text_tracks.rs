//! `core/html/text_tracks.rs` -- the LIVE bodies of the text-track family, and the two interfaces of
//! it that were still generated-registry shells.
//!
//! WHAT THIS FILE FINISHES. W5.1 lane L3 moved `TextTrack`, `TextTrackList`, `TextTrackCue` and
//! `VTTCue` into `interface!` tables in `core/html/media_objects.rs`, and `HTMLMediaElement` /
//! `HTMLVideoElement` into `core/html/html_media_element.rs` -- but FIVE of those rows had no Rust
//! body, because `dom/09k-media-rtc.js` still owned them: `HTMLMediaElement`'s `textTracks` and
//! `addTextTrack`, `HTMLVideoElement.getVideoPlaybackQuality`, and every member of `TextTrack` and
//! `TextTrackList`. That is why `install_window_media` runs from the ESM layer (so 09k could replace
//! those bodies afterwards) and why `finalize_window_l3` existed (09k's `meth()` helper re-defined a
//! method as a bare native function, dropping the WebIDL arity guard the row had put on). This file is
//! those bodies, in Rust, and it takes both of those workarounds with it.
//!
//! IT ALSO BRINGS THE LAST TWO INTERFACES OF THE FAMILY OUT OF THE REGISTRY: `TextTrackCueList` and
//! `VideoPlaybackQuality` were shells whose bodies 09k supplied, so they are tables here, with rows
//! VERBATIM from their `install/generated_registry.rs` blocks.
//!
//! WHAT IS REAL, and why "empty" is an answer and not a gap. This engine fetches no track resource,
//! so a track a page ADDS is the only track that exists -- and `addTextTrack` is a purely local
//! operation whose whole effect is observable: the track is created `hidden` (in the element's list,
//! its cues active, nothing rendered), it joins the element's `textTracks`, and its cue list is empty
//! because nothing has parsed a WebVTT file. A video that has never presented a frame has presented,
//! dropped and corrupted NONE: `getVideoPlaybackQuality()`'s counters are real and they are zero
//! because the count is zero, while `creationTime` is the document's own clock.
//!
//! ---
//!
//! ## A `<track>` element OWNS a TextTrack, and the list is derived, not stored
//!
//! Until this unit `HTMLTrackElement.track` was a D2 `stub` and `HTMLMediaElement.textTracks` held a
//! STORED array that only `addTextTrack` ever appended to, so a page that wrote `<video><track></video>`
//! saw `track.track === null` and `video.textTracks.length === 0`. That is what made `svelte.dev`
//! render SvelteKit's own 500 page: `TypeError: Cannot read properties of null (reading 'cues')`.
//!
//! Both facts now have ONE home each:
//!
//!   * The `<track>` element's TextTrack is minted once, on first read, into a private slot on the
//!     element wrapper -- so `t.track === t.track` and every path that reaches the element reaches the
//!     SAME track. Its `kind`, `label`, `language` and `id` are not copied into the track: they are
//!     read THROUGH the element's own reflected members (`kind`, `label`, `srclang`, `id`), which are
//!     the single source for those four content attributes. Chrome agrees they are live
//!     (`k3.label = "after"` moves `k3.track.label`), and a copy would have been a second truth.
//!   * `HTMLMediaElement.textTracks` no longer stores membership. The list is DERIVED on every read:
//!     the element's `<track>` CHILDREN in tree order, then the tracks `addTextTrack` minted, in
//!     creation order. That is Blink's own concatenation (`TextTrackList` keeps `element_tracks_`
//!     ahead of `add_track_`) and it is what Chrome reports: `addTextTrack` first and `appendChild`
//!     second still puts the ELEMENT track at index 0. Deriving is also what makes the list live --
//!     `removeChild(track)` drops it, which a stored array cannot do.
//!
//! ### mode, and the default-track selection
//!
//! Every track starts `disabled`, and HTML's "honor user preferences for automatic text track
//! selection" is what turns the first `<track default>` of a kind group on. Measured in Chrome 151
//! headless over loopback (`track3.html`): within one group the FIRST `default` wins and later ones
//! stay `disabled`; `metadata` lands on `hidden` and every other group on `showing`; two defaults in
//! DIFFERENT groups are both selected.
//!
//! The trigger is the media element's resource selection, which runs ONCE, at the element's
//! insertion. Chrome's numbers show exactly that: a parsed `<video><track default>` is `showing`,
//! while `appendChild(video)` followed by `appendChild(track default)` leaves the track `disabled` --
//! the video's selection had already run, over no children. So [`select_defaults`] runs from the
//! insertion seam (`core/html/subresource.rs::resource_inserted`) and LATCHES the element; a media
//! element the PARSER built is delivered through no insertion seam at all, so it is latched lazily
//! instead, at the first read that could tell the difference. Both reproduce the measurement.
//!
//! ### cues
//!
//! `cues` and `activeCues` are `null` while the track is `disabled` and a (here always empty)
//! `TextTrackCueList` otherwise -- also measured, and also live: flipping `mode` to `hidden` and back
//! flips the answer. `addTextTrack` mints its track `hidden`, so its cue list was never null and this
//! rule does not move it.

use deno_core::v8;
use deno_core::v8::MapFnTo;

use crate::binding::instance_state as st;
use crate::binding::protos::{
    arg_node_id, build_fn_data, get_prop, js_now, make_dom_exc, shell_proto, str_val, throw_type_error, wrap_cached,
};
use crate::binding::HostDom;
use crate::install::kits::define_accessor;
use crate::install::live::method_row;
use crate::install::MemberDef;

pub(crate) const TRACK: &str = "TextTrack";
pub(crate) const LIST: &str = "TextTrackList";
pub(crate) const CUES: &str = "TextTrackCueList";
pub(crate) const QUALITY: &str = "VideoPlaybackQuality";

/// The `<track>` element's own `TextTrack`, in a private slot on the element wrapper.
const EL_TRACK: &str = "HTMLTrackElement_track";
/// The media element's one-shot "automatic text track selection has run" latch.
const SELECTED: &str = "HTMLMediaElement_trackDefaults";

/// The five `TextTrackKind` values, in the IDL's order.
const KINDS: &[&str] = &["subtitles", "captions", "descriptions", "chapters", "metadata"];
/// The three `TextTrackMode` values.
const MODES: &[&str] = &["disabled", "hidden", "showing"];

fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, iface)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    Some(o)
}

fn mint_cue_list<'s>(scope: &mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, CUES)?;
    let s = st::attach(scope, o, CUES);
    let empty = st::array(scope, &[]);
    st::set(scope, s, "cues", empty.into());
    Some(o)
}

/// The media element's `TextTrackList`. It is an EXOTIC object: WebIDL gives `TextTrackList` an
/// indexed property getter, and `v.textTracks[0]` is how every page reads the list -- so it is minted
/// from the interface's template, which carries the indexed interceptor ([`configure`]), rather than as
/// a plain object. `media` is the element the membership is derived from; `tracks` holds only what
/// `addTextTrack` minted.
fn mint_track_list<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    media: v8::Local<v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let proto = shell_proto(scope, LIST)?;
    let o = crate::binding::protos::mint_on(scope, proto.into());
    let s = st::attach(scope, o, LIST);
    let empty = st::array(scope, &[]);
    st::set(scope, s, "tracks", empty.into());
    st::set(scope, s, "media", media.into());
    Some(o)
}

/// One track, in the state `addTextTrack` leaves it: `hidden`, with an id the page never set and two
/// empty cue lists.
fn mint_text_track<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    kind: &str,
    label: &str,
    language: &str,
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, TRACK)?;
    let s = st::attach(scope, o, TRACK);
    st::set_str(scope, s, "kind", if KINDS.contains(&kind) { kind } else { "subtitles" });
    st::set_str(scope, s, "label", label);
    st::set_str(scope, s, "language", language);
    st::set_str(scope, s, "id", "");
    // `addTextTrack` creates the track already HIDDEN -- it is in the element's list and its cues are
    // active, but nothing is rendered.
    st::set_str(scope, s, "mode", "hidden");
    let cues = mint_cue_list(scope)?;
    st::set(scope, s, "cues", cues.into());
    let active = mint_cue_list(scope)?;
    st::set(scope, s, "activeCues", active.into());
    Some(o)
}

/// The `TextTrack` a `<track>` element exposes: `disabled`, with no values of its own, because
/// `kind`/`label`/`language`/`id` are read THROUGH the element (the `element` slot) rather than
/// copied out of it.
fn mint_element_track<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    el: v8::Local<v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    let o = mint(scope, TRACK)?;
    let s = st::attach(scope, o, TRACK);
    st::set(scope, s, "element", el.into());
    st::set_str(scope, s, "mode", "disabled");
    let cues = mint_cue_list(scope)?;
    st::set(scope, s, "cues", cues.into());
    let active = mint_cue_list(scope)?;
    st::set(scope, s, "activeCues", active.into());
    Some(o)
}

/// `HTMLTrackElement.track` -- the element's own `TextTrack`, minted once and kept in a private slot
/// on the element wrapper (the same mechanism, and the same canonical wrapper, the media element's
/// list uses), so `t.track === t.track` and the list and the element see one object.
pub(crate) fn track_for_element<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    el: v8::Local<v8::Object>,
) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(s) = st::state(scope, el.into(), EL_TRACK) {
        if let Ok(t) = v8::Local::<v8::Object>::try_from(st::get(scope, s, "track")) {
            return Some(t);
        }
    }
    let t = mint_element_track(scope, el)?;
    let s = st::attach(scope, el, EL_TRACK);
    st::set(scope, s, "track", t.into());
    // A media element the PARSER built reaches no insertion seam, so its automatic selection has not
    // run; this is the first moment a page could see the difference.
    if let Some(media) = media_parent(scope, el) {
        select_defaults(scope, media);
    }
    Some(t)
}

fn items<'s>(scope: &mut v8::PinScope<'s, '_>, s: v8::Local<v8::Object>, name: &str) -> Vec<v8::Local<'s, v8::Value>> {
    match st::get_array(scope, s, name) {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    }
}

// ---- the derived membership ----------------------------------------------------------------------

/// Is this tag a media element -- the two that can hold `<track>` children?
fn is_media_tag(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("video") || tag.eq_ignore_ascii_case("audio")
}

/// The `<track>` CHILDREN of a media element, in tree order, as canonical wrappers. CHILDREN, not
/// descendants: Chrome reports `0` for a `<track>` nested one `<div>` deep inside a `<video>`.
fn track_children<'s>(scope: &mut v8::PinScope<'s, '_>, media: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Object>> {
    let Some(id) = arg_node_id(scope, media.into()) else { return Vec::new() };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let ids: Vec<u32> = {
        let stt = op_state.borrow();
        let Some(host) = stt.try_borrow::<HostDom>() else { return Vec::new() };
        let dom = &host.0;
        dom.children(id)
            .into_iter()
            .filter(|c| dom.tag_name(*c).is_some_and(|t| t.eq_ignore_ascii_case("track")))
            .collect()
    };
    ids.into_iter().map(|c| wrap_cached(scope, &op_state, c)).collect()
}

/// The media element a `<track>` is a child of, or `None` when it is detached or parented by
/// anything else.
fn media_parent<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    let id = arg_node_id(scope, el.into())?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let pid = {
        let stt = op_state.borrow();
        let dom = &stt.try_borrow::<HostDom>()?.0;
        let p = dom.parent(id)?;
        if !dom.tag_name(p).is_some_and(is_media_tag) {
            return None;
        }
        p
    };
    Some(wrap_cached(scope, &op_state, pid))
}

/// EVERY track of a `TextTrackList`, derived: the media element's `<track>` children in tree order,
/// then the tracks `addTextTrack` minted. Blink's own order, and Chrome's (`addTextTrack` called
/// FIRST still lands behind a `<track>` appended after it).
fn list_tracks<'s>(scope: &mut v8::PinScope<'s, '_>, ls: v8::Local<v8::Object>) -> Vec<v8::Local<'s, v8::Value>> {
    let mut out: Vec<v8::Local<v8::Value>> = Vec::new();
    if let Ok(media) = v8::Local::<v8::Object>::try_from(st::get(scope, ls, "media")) {
        for el in track_children(scope, media) {
            if let Some(t) = track_for_element(scope, el) {
                out.push(t.into());
            }
        }
    }
    out.extend(items(scope, ls, "tracks"));
    out
}

/// HTML's "honor user preferences for automatic text track selection", as Chrome runs it, ONCE per
/// media element. Groups are the spec's: subtitles and captions share one, descriptions, chapters and
/// metadata have their own. The first `<track default>` of a group is selected; `metadata` is selected
/// `hidden` (it is not rendered) and every other group `showing`. Later defaults in the same group,
/// and every track added after this has run, stay `disabled`.
pub(crate) fn select_defaults(scope: &mut v8::PinScope, media: v8::Local<v8::Object>) {
    if st::state(scope, media.into(), SELECTED).is_some() {
        return;
    }
    st::attach(scope, media, SELECTED);
    let mut taken: [bool; 4] = [false; 4];
    for el in track_children(scope, media) {
        let is_default = get_prop(scope, el, "default").is_some_and(|v| v.boolean_value(scope));
        if !is_default {
            continue;
        }
        let kind = get_prop(scope, el, "kind").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let (group, mode) = match kind.as_str() {
            "subtitles" | "captions" => (0usize, "showing"),
            "descriptions" => (1, "showing"),
            "chapters" => (2, "showing"),
            "metadata" => (3, "hidden"),
            _ => continue,
        };
        if taken[group] {
            continue;
        }
        taken[group] = true;
        let Some(t) = track_for_element(scope, el) else { continue };
        if let Some(s) = st::state(scope, t.into(), TRACK) {
            st::set_str(scope, s, "mode", mode);
        }
    }
}

/// The insertion seam's entry point (`core/html/subresource.rs::resource_inserted`): a media element
/// has just been inserted, which is when its resource selection -- and so its automatic text track
/// selection -- runs, over the `<track>` children it has AT THAT MOMENT. Latching here is what makes
/// a track appended afterwards stay `disabled`, as Chrome leaves it.
pub(crate) fn media_element_inserted(scope: &mut v8::PinScope, media: v8::Local<v8::Object>) {
    select_defaults(scope, media);
}

// ---- TextTrackList's indexed property getter -----------------------------------------------------

/// `TextTrackList`'s instance template: the indexed interceptor. `TextTrackList` has an indexed property
/// getter in WebIDL and no named one, no setter and no deleter. Applied once per isolate by the builder
/// (`install::instance_shapes`); every list had a cached `ObjectTemplate` of its own
/// until then.
pub(crate) fn configure(it: v8::Local<v8::ObjectTemplate>) {
    it.set_indexed_property_handler(
        v8::IndexedPropertyHandlerConfiguration::new()
            .getter_raw(crate::binding::refs::c(icpt!(idx_get, holder, cb_list_idx_get).map_fn_to()))
            .query_raw(crate::binding::refs::c(icpt!(idx_query, holder, cb_list_idx_query).map_fn_to()))
            .enumerator_raw(crate::binding::refs::c(icpt!(idx_keys, holder, cb_list_idx_enum).map_fn_to())),
    );
}

fn cb_list_idx_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue,
) -> v8::Intercepted {
    let Some(ls) = st::state(scope, args.holder().into(), LIST) else { return v8::Intercepted::kNo };
    match list_tracks(scope, ls).get(index as usize) {
        Some(t) => {
            rv.set(*t);
            v8::Intercepted::kYes
        }
        None => v8::Intercepted::kNo,
    }
}

fn cb_list_idx_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let Some(ls) = st::state(scope, args.holder().into(), LIST) else { return v8::Intercepted::kNo };
    if (index as usize) >= list_tracks(scope, ls).len() {
        return v8::Intercepted::kNo;
    }
    // {writable:false, enumerable:true, configurable:true} -- WebIDL's indexed property.
    let attrs = v8::Integer::new(scope, v8::PropertyAttribute::READ_ONLY.as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

fn cb_list_idx_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let n = match st::state(scope, args.holder().into(), LIST) {
        Some(ls) => list_tracks(scope, ls).len(),
        None => 0,
    };
    let arr = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let k = v8::Integer::new_from_unsigned(scope, i as u32);
        arr.set_index(scope, i as u32, k.into());
    }
    rv.set(arr);
}

// ---- TextTrackCueList ----------------------------------------------------------------------------

fn cb_cues_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), CUES) else { return };
    rv.set_int32(items(scope, s, "cues").len() as i32);
}

/// `getCueById(id)`: a cue's `id` is a `TextTrackCue` member, and nothing here mints a cue, so the
/// list is empty and the answer is null -- read off the real list rather than hard-coded, so it stays
/// right the moment `addCue` is given one.
fn cb_cues_by_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), CUES) else { return };
    let Some(want) = st::to_str(scope, args.get(0)) else { return };
    for c in items(scope, s, "cues") {
        let Ok(o) = v8::Local::<v8::Object>::try_from(c) else { continue };
        let id = crate::binding::protos::get_prop(scope, o, "id").map(|v| v.to_rust_string_lossy(scope));
        if id.as_deref() == Some(want.as_str()) {
            rv.set(c);
            return;
        }
    }
    let n = v8::null(scope);
    rv.set(n.into());
}

pub fn install_cue_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => {
            let d = str_val(scope, "length");
            let g = build_fn_data(scope, cb_cues_length, "get length", 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "getCueById" => method_row(scope, proto, member, cb_cues_by_id),
        other => debug_assert!(false, "install_cue_list: unexpected member {other}"),
    }
}

// ---- TextTrackList -------------------------------------------------------------------------------

fn cb_list_length(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), LIST) else { return };
    rv.set_int32(list_tracks(scope, s).len() as i32);
}

/// `getTrackById(id)` over the DERIVED list. The id is read through the track's own `id` member
/// rather than out of its state, because an element track has no stored id -- its id IS the `<track>`
/// element's, live.
fn cb_list_by_id(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), LIST) else { return };
    let Some(want) = st::to_str(scope, args.get(0)) else { return };
    for t in list_tracks(scope, s) {
        let Ok(o) = v8::Local::<v8::Object>::try_from(t) else { continue };
        let id = get_prop(scope, o, "id").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        if id == want {
            rv.set(t);
            return;
        }
    }
    let n = v8::null(scope);
    rv.set(n.into());
}

pub fn install_track_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => {
            let d = str_val(scope, "length");
            let g = build_fn_data(scope, cb_list_length, "get length", 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "getTrackById" => method_row(scope, proto, member, cb_list_by_id),
        other => debug_assert!(false, "install_track_list: unexpected member {other}"),
    }
}

// ---- TextTrack -----------------------------------------------------------------------------------

/// `kind`, `label`, `language`, `id`, `mode`, `cues` and `activeCues`, from the one state object.
///
/// TWO of the seven are not a plain read, and both were measured in Chrome rather than reasoned:
///
///   * a track that a `<track>` ELEMENT owns has no `kind`/`label`/`language`/`id` of its own -- they
///     are the element's `kind`, `label`, `srclang` and `id`, read live, so `k.label = "after"` moves
///     `k.track.label`. Reading them THROUGH the element's own members is also what keeps this file
///     from owning a second copy of four content attributes the element already reflects.
///   * `cues` and `activeCues` are `null` while the track is `disabled` -- Chrome's answer, and a live
///     one: setting `mode = "hidden"` makes the (empty) `TextTrackCueList` appear and setting it back
///     to `disabled` makes it null again. (Chrome 150's `activeCues` was also null until the cue list
///     existed; Chrome 152's `TextTrack::activeCues` answers its list whenever the track is not disabled.)
fn cb_track_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    if matches!(name.as_str(), "cues" | "activeCues") && st::get_str(scope, s, "mode") == "disabled" {
        let n = v8::null(scope);
        rv.set(n.into());
        return;
    }
    // Blink keeps the cue list unmade until `cues` is read or a cue is added
    // (`TextTrack::EnsureTextTrackCueList`); Chrome 152's `activeCues` no longer waits for it (a fresh
    // `addTextTrack("subtitles").activeCues` is a TextTrackCueList, measured; Chrome 150 answered null).
    if name == "cues" {
        st::set_bool(scope, s, "cueListMade", true);
    }
    if matches!(name.as_str(), "kind" | "label" | "language" | "id") {
        if let Ok(el) = v8::Local::<v8::Object>::try_from(st::get(scope, s, "element")) {
            // `language` is the element's `srclang`; the other three share their name with it.
            let prop = if name == "language" { "srclang" } else { name.as_str() };
            let v = get_prop(scope, el, prop).unwrap_or_else(|| str_val(scope, ""));
            rv.set(v);
            return;
        }
    }
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// `mode = v`: the `TextTrackMode` enum check is the binding layer's.
fn cb_track_mode_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let Some(m) = st::to_str(scope, args.get(0)) else { return };
    if !MODES.contains(&m.as_str()) {
        throw_type_error(
            scope,
            &format!("Failed to set the 'mode' property on 'TextTrack': The provided value '{m}' is not a valid enum value of type TextTrackMode."),
        );
        return;
    }
    st::set_str(scope, s, "mode", &m);
}

/// `addCue(cue)` / `removeCue(cue)`. `data` says which. Adding a cue the track already has is a no-op
/// (the spec's own set semantics); removing one it does not have is `NotFoundError`.
fn cb_track_edit_cues(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), TRACK) else { return };
    let op = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let cue = args.get(0);
    if !cue.is_object() {
        throw_type_error(scope, &format!("Failed to execute '{op}' on 'TextTrack': parameter 1 is not of type 'TextTrackCue'."));
        return;
    }
    if op == "addCue" {
        st::set_bool(scope, s, "cueListMade", true);
    }
    let cues = st::get(scope, s, "cues");
    let Some(cs) = st::state(scope, cues, CUES) else { return };
    let cur = items(scope, cs, "cues");
    let found = cur.iter().any(|c| *c == cue);
    if op == "removeCue" && !found {
        if let Some(e) = make_dom_exc(
            scope,
            "Failed to execute 'removeCue' on 'TextTrack': The specified cue is not listed in the TextTrack's list of cues.",
            "NotFoundError",
        ) {
            scope.throw_exception(e);
        }
        return;
    }
    let mut next: Vec<v8::Local<v8::Value>> = Vec::with_capacity(cur.len() + 1);
    for c in cur {
        if op == "removeCue" && c == cue {
            continue;
        }
        next.push(c);
    }
    if op == "addCue" && !found {
        next.push(cue);
    }
    let arr = st::array(scope, &next);
    st::set(scope, cs, "cues", arr.into());
}

pub fn install_text_track(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "kind" | "label" | "language" | "id" | "cues" | "activeCues" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_track_field, &format!("get {}", member.name), 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        "mode" => {
            let d = str_val(scope, "mode");
            let g = build_fn_data(scope, cb_track_field, "get mode", 0, d);
            let d = str_val(scope, "mode");
            let s = build_fn_data(scope, cb_track_mode_set, "set mode", 1, d);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "addCue" | "removeCue" => {
            use crate::binding::protos::make_arity_guard;
            let d = str_val(scope, member.name);
            let f = build_fn_data(scope, cb_track_edit_cues, member.name, member.length as i32, d);
            let v: v8::Local<v8::Value> = if member.required > 0 {
                make_arity_guard(scope, TRACK, member.name, member.required as i32, member.length as i32, f.into(), member.ty).into()
            } else {
                f.into()
            };
            crate::install::kits::define_value(scope, proto, member, v, true);
        }
        other => debug_assert!(false, "install_text_track: unexpected member {other}"),
    }
}

// ---- VideoPlaybackQuality ------------------------------------------------------------------------

fn cb_quality_field(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), QUALITY) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

pub fn install_playback_quality(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let d = str_val(scope, member.name);
    let g = build_fn_data(scope, cb_quality_field, &format!("get {}", member.name), 0, d);
    define_accessor(scope, proto, member, g, None);
}

// ---- the media element's members -----------------------------------------------------------------

/// The element's own `TextTrackList`, one per element, created on first read and kept in a v8 private
/// slot of the element wrapper -- where the deleted JS kept it in a `WeakMap` keyed by the same object.
fn list_for<'s>(scope: &mut v8::PinScope<'s, '_>, el: v8::Local<v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    const SLOT: &str = "HTMLMediaElement_textTracks";
    if let Some(s) = st::state(scope, el.into(), SLOT) {
        if let Ok(l) = v8::Local::<v8::Object>::try_from(st::get(scope, s, "list")) {
            return Some(l);
        }
    }
    let list = mint_track_list(scope, el)?;
    let s = st::attach(scope, el, SLOT);
    st::set(scope, s, "list", list.into());
    Some(list)
}

/// `HTMLMediaElement.textTracks`: the element's list of text tracks -- its `<track>` children in tree
/// order, then whatever `addTextTrack` minted. The LIST object is one per element and stable; its
/// MEMBERSHIP is derived on every read, which is what makes `appendChild`/`removeChild` of a `<track>`
/// show up in it.
pub(crate) fn cb_media_text_tracks(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(l) = list_for(scope, args.this()) else { return };
    // A media element the PARSER built reaches no insertion seam; this is the other first read that
    // could observe its automatic selection.
    select_defaults(scope, args.this());
    rv.set(l.into());
}

/// `HTMLMediaElement.addTextTrack(kind, label, language)`: a purely local operation whose whole
/// effect is observable -- the new track is `hidden`, has an empty cue list and joins the element's
/// `textTracks`.
pub(crate) fn cb_media_add_text_track(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(kind) = st::to_str(scope, args.get(0)) else { return };
    if !KINDS.contains(&kind.as_str()) {
        throw_type_error(
            scope,
            &format!(
                "Failed to execute 'addTextTrack' on 'HTMLMediaElement': The provided value '{kind}' is not a valid enum value of type TextTrackKind."
            ),
        );
        return;
    }
    let label = if args.get(1).is_undefined() {
        String::new()
    } else {
        match st::to_str(scope, args.get(1)) {
            Some(s) => s,
            None => return,
        }
    };
    let language = if args.get(2).is_undefined() {
        String::new()
    } else {
        match st::to_str(scope, args.get(2)) {
            Some(s) => s,
            None => return,
        }
    };
    let Some(track) = mint_text_track(scope, &kind, &label, &language) else { return };
    let Some(list) = list_for(scope, args.this()) else { return };
    if let Some(ls) = st::state(scope, list.into(), LIST) {
        let mut tracks = items(scope, ls, "tracks");
        tracks.push(track.into());
        let arr = st::array(scope, &tracks);
        st::set(scope, ls, "tracks", arr.into());
    }
    rv.set(track.into());
}

/// `HTMLTrackElement.track` -- the `TextTrack` this `<track>` element exposes. A real object whether
/// or not the element is in a media element, whether or not it is in the document, and whether or not
/// its resource was ever fetched (it never is here): Chrome mints it on the element, not on the load.
pub(crate) fn cb_track_element_track(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    // The brand IS the arena: a `<track>` wrapper unwraps to a node id, and nothing else does.
    if arg_node_id(scope, this.into()).is_none() {
        throw_type_error(scope, "Illegal invocation");
        return;
    }
    let Some(t) = track_for_element(scope, this) else { return };
    rv.set(t.into());
}

/// `HTMLVideoElement.getVideoPlaybackQuality()`: a fresh snapshot each call. A video that has never
/// presented a frame has presented, dropped and corrupted none -- the counters are real and they are
/// zero because the count is zero -- and `creationTime` is the document's own clock.
pub(crate) fn cb_video_playback_quality(scope: &mut v8::PinScope, _a: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(o) = mint(scope, QUALITY) else { return };
    let s = st::attach(scope, o, QUALITY);
    let now = js_now(scope);
    st::set_num(scope, s, "creationTime", now);
    for k in ["totalVideoFrames", "droppedVideoFrames", "corruptedVideoFrames"] {
        st::set_num(scope, s, k, 0.0);
    }
    rv.set(o.into());
}

// ---- the two tables this lane brings out of the registry ------------------------------------------

webapi::interface! {
    "TextTrackCueList" [interface, window] {
    ctor   "TextTrackCueList" len 0                        = stub,
    attr   "length" : Object readonly                      = install_cue_list,
    method "getCueById" : Object len 1 required 1 nullable = install_cue_list,
    method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "VideoPlaybackQuality" [interface, window] {
    ctor "VideoPlaybackQuality" len 0                 = stub,
    attr "creationTime" : Object readonly             = install_playback_quality,
    attr "totalVideoFrames" : Object readonly         = install_playback_quality,
    attr "droppedVideoFrames" : Object readonly       = install_playback_quality,
    attr "corruptedVideoFrames" : Object readonly     = install_playback_quality
    }
}
