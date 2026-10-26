//! `core/html/html_media_parts.rs` -- `HTMLTrackElement` and `HTMLSourceElement` as two `interface!`
//! tables.
//!
//! The `<track>` and `<source>` halves of `dom/09a-media-parts.js` (188 lines, 3 native masks, 5
//! `Object.defineProperty` sites). Both elements describe a resource that has been declared and never
//! fetched, which is the permanent state of every one of them in this engine, and between them they
//! are almost pure content-attribute reflection -- so almost every row below is a shared kit rather
//! than a body.
//!
//! Rows are VERBATIM from the two blocks of `install/generated_registry.rs` they replace; mounted by
//! THE installer (`install::live::install_window_media`).
//!
//! TWO THINGS HERE ARE EASY TO GET WRONG and are taken from the recordings rather than from intuition:
//!
//!   * `<track kind>` has an INVALID value default of "metadata" and a MISSING value default of
//!     "subtitles" -- they differ, which is unusual, and the recording confirms it:
//!     `track.kind = "x"` reads back "metadata" while a bare `<track>` reads "subtitles".
//!   * `HTMLSourceElement`'s `width`/`height` reflect as unsigned long with default 0, the same
//!     shape `<video>`'s do -- not as strings, which is what a `[Reflect]` guess would give.
//!
//! `HTMLTrackElement.track` WAS a D2 stub, on the reasoning that nothing here creates a `TextTrack`
//! for a `<track>` whose resource is never fetched. That reasoning was wrong, and this corrects
//! it: Chrome mints the track on the ELEMENT, not on the load, so a detached
//! `document.createElement("track").track` is already `[object TextTrack]` there while it was `null`
//! here -- which is why `svelte.dev` rendered SvelteKit's 500 page (`Cannot read properties of null
//! (reading 'cues')`). What the missing FETCH really costs is the track's CUES, and Chrome says that
//! separately: `cues` is `null` for a `disabled` track and an EMPTY `TextTrackCueList` otherwise. The
//! body lives in `core/html/text_tracks.rs` with the rest of the family.

use deno_core::v8;

use crate::core::html::html_input_element::acc1;
use crate::core::html::html_media_element::cb_media_zero;
use crate::core::html::text_tracks::cb_track_element_track;
use crate::install::MemberDef;

pub fn install_html_track_element(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        // NONE. The track's resource is never fetched, so it never leaves that state.
        "readyState" => acc1(scope, proto, member, cb_media_zero, None),
        "track" => acc1(scope, proto, member, cb_track_element_track, None),
        other => debug_assert!(false, "install_html_track_element: unexpected member {other}"),
    }
}

webapi::interface! {
    "HTMLTrackElement" : "HTMLElement" [element, window] {
    ctor            "HTMLTrackElement" len 0                          = stub,
    attr            "kind" : Object cfg "kind|subtitles,captions,descriptions,chapters,metadata|subtitles|metadata" = reflect_enum,
    attr            "src" : Object cfg "src"                          = reflect_url,
    attr            "srclang" : Object cfg "srclang"                  = reflect_str,
    attr            "label" : Object cfg "label"                      = reflect_str,
    attr            "default" : Object cfg "default"                  = reflect_bool,
    attr            "readyState" : Object readonly                    = install_html_track_element,
    attr            "track" : Object readonly                         = install_html_track_element,
    constant        "NONE" : Object                                   = 0,
    constant        "LOADING" : Object                                = 1,
    constant        "LOADED" : Object                                 = 2,
    constant        "ERROR" : Object                                  = 3,
    static_constant "NONE" : Object                                   = 0,
    static_constant "LOADING" : Object                                = 1,
    static_constant "LOADED" : Object                                 = 2,
    static_constant "ERROR" : Object                                  = 3
    }
}

webapi::interface! {
    "HTMLSourceElement" : "HTMLElement" [element, window] {
    ctor            "HTMLSourceElement" len 0                         = stub,
    attr            "src" : Object cfg "src"                          = reflect_url,
    attr            "type" : Object cfg "type"                        = reflect_str,
    attr            "srcset" : Object cfg "srcset"                    = reflect_str,
    attr            "sizes" : Object cfg "sizes"                      = reflect_str,
    attr            "media" : Object cfg "media"                      = reflect_str,
    attr            "width" : Object cfg "width|0|"                   = reflect_ulong,
    attr            "height" : Object cfg "height|0|"                 = reflect_ulong
    }
}
