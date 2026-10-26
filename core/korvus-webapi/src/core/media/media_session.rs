//! `core/media/media_session.rs` -- `MediaMetadata` as one `interface!` table.
//!
//! Rows are VERBATIM from the `MediaMetadata` block of `install/generated_registry.rs`.
//!
//! The Media Session VALUE object: a page constructs one, fills it in and hands it to
//! `navigator.mediaSession.metadata` so the platform can draw a media notification. There is no
//! platform notification here, and that changes nothing a page can read back -- every member of this
//! interface is state the page itself put there, and all of it round-trips.
//!
//! `artwork` IS A SEQUENCE OF `MediaImage` DICTIONARIES, and the two rules that make it observable are
//! both here: assigning REPLACES the whole list after converting every entry (a non-iterable, a
//! non-object entry and an entry with no `src` are each the binding layer's TypeError), and reading
//! gives back a FRESH FROZEN array of fresh frozen images (`FrozenArray<MediaImage>`, C2), so a page can
//! neither change the list it got nor reach the object's own.
//!
//! `chapterInfo` is readonly: the init's `chapterInfo` entries as `ChapterInformation` objects
//! (`core/misc_ifaces.rs`), the same objects -- frozen, as Blink's getter freezes them -- on every read of a
//! fresh frozen array.
//!
//! NOT YET BLINK'S: an image's `src` is kept as given, where Blink's `ProcessArtworkVector` resolves it against the
//! document (and refuses one that does not resolve), and assigning `[]` clears the list where Blink keeps the old
//! one (`SetArtworkInternal` returns early on an empty list).

use deno_core::v8;

use crate::binding::instance_state as st;
use crate::binding::protos::{build_fn_data, str_val, throw_type_error};
use crate::install::kits::define_accessor;
use crate::install::MemberDef;

const META: &str = "MediaMetadata";

/// WebIDL `sequence<MediaImage>`: `None` means the conversion threw and the error is already pending.
fn normalize_artwork<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<v8::Value>,
    op: &str,
) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    if v.is_null_or_undefined() {
        return Some(Vec::new());
    }
    let Some(items) = crate::core::media::media_stream::iterable_items(scope, v) else {
        throw_type_error(scope, &format!("Failed to {op} the 'artwork' property on 'MediaMetadata': The provided value cannot be converted to a sequence."));
        return None;
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Ok(o) = v8::Local::<v8::Object>::try_from(item) else {
            throw_type_error(scope, &format!("Failed to {op} the 'artwork' property on 'MediaMetadata': The provided value is not of type 'MediaImage'."));
            return None;
        };
        let Some(src) = st::dict_get(scope, o.into(), "src") else {
            throw_type_error(scope, &format!("Failed to {op} the 'artwork' property on 'MediaMetadata': Failed to read the 'src' property from 'MediaImage': Required member is undefined."));
            return None;
        };
        let entry = v8::Object::new(scope);
        let text = st::to_str(scope, src)?;
        st::set_str(scope, entry, "src", &text);
        for k in ["sizes", "type"] {
            let text = match st::dict_get(scope, o.into(), k) {
                Some(v) => st::to_str(scope, v)?,
                None => String::new(),
            };
            st::set_str(scope, entry, k, &text);
        }
        out.push(entry.into());
    }
    Some(out)
}

pub(crate) fn cb_metadata_ctor(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    if args.new_target().is_undefined() {
        let msg = crate::binding::protos::use_new_msg("MediaMetadata");
        throw_type_error(scope, &msg);
        return;
    }
    // `MediaMetadataInit`, converted as WebIDL converts it (Chrome 150): `album`, `artist`,
    // `artwork` (each `MediaImage` with it), `chapterInfo`, `title`, each read once in that order; korvus read title,
    // artist, album first. What follows reads the CONVERTED dictionary.
    let Ok(init) = crate::install::ctor_dicts::convert_dict(scope, args.get(0), "MediaMetadataInit", "Failed to construct 'MediaMetadata': ") else { return };
    let init: v8::Local<v8::Value> = init.into();
    let mut fields = Vec::new();
    for k in ["title", "artist", "album"] {
        let text = match st::dict_get(scope, init, k) {
            Some(v) => match st::to_str(scope, v) {
                Some(t) => t,
                None => return,
            },
            None => String::new(),
        };
        fields.push((k, text));
    }
    let art = match st::dict_get(scope, init, "artwork") {
        Some(v) => match normalize_artwork(scope, v, "construct") {
            Some(a) => a,
            None => return,
        },
        None => Vec::new(),
    };
    // `sequence<ChapterInformationInit> chapterInfo`: each entry a ChapterInformation (title, startTime,
    // its own artwork), converted here so a bad entry is the constructor's TypeError.
    let mut chapters = Vec::new();
    if let Some(v) = st::dict_get(scope, init, "chapterInfo") {
        let Ok(list) = v8::Local::<v8::Array>::try_from(v) else {
            throw_type_error(scope, "Failed to construct 'MediaMetadata': Failed to read the 'chapterInfo' property from 'MediaMetadataInit': The provided value cannot be converted to a sequence.");
            return;
        };
        for i in 0..list.length() {
            let Some(e) = list.get_index(scope, i) else { return };
            let title = match st::dict_get(scope, e, "title") {
                Some(v) => match st::to_str(scope, v) {
                    Some(t) => t,
                    None => return,
                },
                None => String::new(),
            };
            let start = match st::dict_get(scope, e, "startTime") {
                Some(v) => match v.number_value(scope) {
                    Some(n) if n.is_finite() => n,
                    Some(_) => {
                        throw_type_error(scope, "Failed to construct 'MediaMetadata': Failed to read the 'startTime' property from 'ChapterInformationInit': The provided double value is non-finite.");
                        return;
                    }
                    None => return,
                },
                None => 0.0,
            };
            let art = match st::dict_get(scope, e, "artwork") {
                Some(v) => match normalize_artwork(scope, v, "construct") {
                    Some(a) => a,
                    None => return,
                },
                None => Vec::new(),
            };
            let Some(c) = crate::core::misc_ifaces::new_chapter(scope, &title, start, &art) else { return };
            chapters.push(c.into());
        }
    }
    let s = st::attach(scope, args.this(), META);
    for (k, text) in fields {
        st::set_str(scope, s, k, &text);
    }
    let arr = st::array(scope, &art);
    st::set(scope, s, "artwork", arr.into());
    let chapters = st::array(scope, &chapters);
    st::set(scope, s, "chapterInfo", chapters.into());
}

fn cb_meta_str_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), META) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let v = st::get(scope, s, &name);
    rv.set(v);
}

/// `title` / `artist` / `album` are `DOMString`, so an assignment is ToString and never an error --
/// `m.title = undefined` really does read back `"undefined"`.
fn cb_meta_str_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), META) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let Some(text) = st::to_str(scope, args.get(0)) else { return };
    st::set_str(scope, s, &name, &text);
}

/// A stored artwork entry as a `MediaImage` dictionary converts to JS: a NEW object, its members own data properties
/// in the dictionary's sorted order (`sizes`, `src`, `type` -- Blink's `ToV8Traits<MediaImage>`), on every conversion.
pub(crate) fn media_image<'s>(scope: &mut v8::PinScope<'s, '_>, stored: v8::Local<v8::Value>) -> v8::Local<'s, v8::Value> {
    let out = v8::Object::new(scope);
    if let Ok(e) = v8::Local::<v8::Object>::try_from(stored) {
        for k in ["sizes", "src", "type"] {
            let v = st::get(scope, e, k);
            if let Some(key) = v8::String::new(scope, k) {
                let _ = out.create_data_property(scope, key.into(), v);
            }
        }
    }
    out.into()
}

/// `artwork` / `chapterInfo`: `[CallWith=ScriptState]` getters that build a `LocalVector` per read, so a NEW
/// frozen array every time (`install::frozen_array`) -- and Blink's C++ freezes each ENTRY too
/// (`FreezeV8Object`): an image is a new frozen `MediaImage` object per read, a chapter the same
/// `ChapterInformation` object, frozen (Chrome 150 measured, C2). The list a page is handed is not the list the
/// object holds, and nothing in it can be changed.
fn cb_meta_list_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), META) else { return };
    let name = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let stored = match st::get_array(scope, s, &name) {
        Some(a) => st::array_items(scope, a),
        None => Vec::new(),
    };
    let answer = crate::install::frozen_array::answer(scope, args.this(), META, &name, |_| 0.0, |scope| {
        Some(
            stored
                .iter()
                .map(|e| {
                    let v = if name == "artwork" { media_image(scope, *e) } else { *e };
                    if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                        let _ = o.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
                    }
                    v
                })
                .collect(),
        )
    });
    if let Some(a) = answer {
        rv.set(a);
    }
}

fn cb_meta_artwork_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(s) = st::state_or_throw(scope, args.this().into(), META) else { return };
    let Some(items) = normalize_artwork(scope, args.get(0), "set") else { return };
    let arr = st::array(scope, &items);
    st::set(scope, s, "artwork", arr.into());
}

pub fn install_media_metadata(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "title" | "artist" | "album" => {
            let d = str_val(scope, member.name);
            let g = build_fn_data(scope, cb_meta_str_get, &format!("get {}", member.name), 0, d);
            let d = str_val(scope, member.name);
            let s = build_fn_data(scope, cb_meta_str_set, &format!("set {}", member.name), 1, d);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "artwork" => {
            let d = str_val(scope, "artwork");
            let g = build_fn_data(scope, cb_meta_list_get, "get artwork", 0, d);
            let d = str_val(scope, "artwork");
            let s = build_fn_data(scope, cb_meta_artwork_set, "set artwork", 1, d);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "chapterInfo" => {
            let d = str_val(scope, "chapterInfo");
            let g = build_fn_data(scope, cb_meta_list_get, "get chapterInfo", 0, d);
            define_accessor(scope, proto, member, g, None);
        }
        other => debug_assert!(false, "install_media_metadata: unexpected member {other}"),
    }
}

webapi::interface! {
    "MediaMetadata" [value, window] {
    ctor "MediaMetadata" len 0                        = stub,
    attr "title" : Object                             = install_media_metadata,
    attr "artist" : Object                            = install_media_metadata,
    attr "album" : Object                             = install_media_metadata,
    attr "artwork" : Object                           = install_media_metadata,
    attr "chapterInfo" : Object readonly              = install_media_metadata
    }
}
