//! The media ELEMENT family: `<video>`, `<audio>`, `<track>`, `<source>`, `<option>` and the text
//! tracks behind them. The streams/RTC half of media is [`super::media_rtc`].

use deno_core::v8;

use super::rows::{build_newable_with_data, parent_of};
use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// The thirteen interfaces of lane L3 from their `interface!` tables, in the WINDOW.
///
/// `HTMLMediaElement`, `HTMLVideoElement`, `HTMLAudioElement` (`core/html/html_media_element.rs`),
/// `HTMLTrackElement`, `HTMLSourceElement` (`html_media_parts.rs`), `HTMLOptionElement`
/// (`html_option_element.rs`), and `TimeRanges`, `MediaError`, `MediaCapabilities`, `TextTrackList`,
/// `TextTrackCue`, `TextTrack`, `VTTCue` (`media_objects.rs`) -- 152 rows that were
/// `dom/08z-media-element.js` (262 lines), `dom/09a-media-parts.js` (188) and
/// `dom/00c-media-codecs.js` (366), 8 native masks, 12 `Object.defineProperty` sites and 45 `KB.__`
/// bridge slots. All of that is deleted; this is their only source.
///
/// WHY THIS RUNS FROM [`super::install_window_bootstrap`], right after the registry install, and NOT
/// later in the runtime's pre-bootstrap block like [`super::install_window_small_elements`] or [`super::install_window_tables`].
/// `dom/09k-media-rtc.js` was the LAST module of the ESM layer and it owned the live bodies of five
/// members of this family: `HTMLMediaElement`'s `textTracks` and `addTextTrack`,
/// `HTMLVideoElement.getVideoPlaybackQuality`, and every member of `TextTrack` and `TextTrackList`.
/// It replaced a body with `Object.defineProperty` over the descriptor that was ALREADY there, so it
/// needed the row in place first. W5.1 lane L4 moved all five into `core/html/text_tracks.rs` and the
/// rows now name them, so NOTHING re-defines a member of these thirteen after this install -- which is
/// also why `finalize_window_l3`, the guard-repair pass that existed only because 09k's `meth()`
/// helper dropped the WebIDL arity guard, is gone. The position is kept because it is also BEFORE
/// [`crate::install::dom_chain::fill_dom_protos`] (see below) and moving it is a separate measurement.
///
/// It is also BEFORE [`crate::install::dom_chain::fill_dom_protos`], which maps every tag `korvus_dom::tags`
/// names onto `globalThis[iface].prototype`, so `video`, `audio`, `track`, `source` and `option` land
/// on these prototypes through the ONE tag pass rather than five hand writes.
///
/// Nothing in `korvus-runtime`'s `BOOTSTRAP` or in `host.js` touches a member of these thirteen
/// (grepped for all thirteen names and for every member this lane gives a body).
///
/// Window-only: the bundle's `worker.surface.json` lists none of the thirteen, and all thirteen left
/// BOTH generated registries with the tables, so nothing else on either surface defines them.
pub fn install_window_media<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::binding::protos::cb_shell_ctor;
    use crate::core::html::html_media_element::{HTMLAUDIOELEMENT, HTMLMEDIAELEMENT, HTMLVIDEOELEMENT};
    use crate::core::html::html_media_parts::{HTMLSOURCEELEMENT, HTMLTRACKELEMENT};
    use crate::core::html::html_option_element::HTMLOPTIONELEMENT;
    use crate::core::html::media_objects::{
        MEDIACAPABILITIES, MEDIAERROR, TEXTTRACK, TEXTTRACKCUE, TEXTTRACKLIST, TIMERANGES, VTTCUE,
    };

    let he = parent_of(scope, "HTMLElement");
    let et = parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    let build = |scope: &mut v8::PinScope<'s, '_>,
                     ledger: &mut installer::InstallLedger,
                     def: &'static InterfaceDef,
                     parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>|
     -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>) {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[korvus] media install gap: {}.{}", g.interface, g.member);
        }
        (built.proto, built.ctor)
    };

    // WebIDL inheritance order: the media base first, so `<video>` and `<audio>` splice onto it.
    let hme = build(scope, &mut ledger, &HTMLMEDIAELEMENT, he);
    build(scope, &mut ledger, &HTMLVIDEOELEMENT, Some(hme));
    build(scope, &mut ledger, &HTMLAUDIOELEMENT, Some(hme));
    build(scope, &mut ledger, &HTMLTRACKELEMENT, he);
    build(scope, &mut ledger, &HTMLSOURCEELEMENT, he);
    build(scope, &mut ledger, &HTMLOPTIONELEMENT, he);
    build(scope, &mut ledger, &TIMERANGES, None);
    build(scope, &mut ledger, &MEDIAERROR, None);
    build(scope, &mut ledger, &MEDIACAPABILITIES, None);
    build(scope, &mut ledger, &TEXTTRACKLIST, et);
    build(scope, &mut ledger, &TEXTTRACKCUE, et);
    build(scope, &mut ledger, &TEXTTRACK, et);
    // `VTTCue` is the one NEWABLE interface of the thirteen (`Model::Value`): the registry built its
    // interface object with `cb_shell_ctor`, so `new VTTCue(0,1,"t")` mints an object and calling it
    // without `new` throws "Please use the 'new' operator". Keeping that exact constructor keeps both
    // answers; `create_interface`'s `Model::Value` placeholder would have lost the second.
    //
    // `_with_data` and the name: `cb_shell_ctor`'s no-`new` message carries Chrome's
    // `Failed to construct 'VTTCue': ` prefix, and it reads the name out of the constructor's `data` -- and the
    // required-argument count after it (`build_interface_ctor`'s shape): `new VTTCue(1)` is "3 arguments required, but
    // only 1 present." in Chrome 150, where the bare name let it reach the cue's own number checks.
    let vtt_name = crate::binding::protos::str_val(scope, "VTTCue|3");
    build_newable_with_data(scope, global, &VTTCUE, cb_shell_ctor, vtt_name, Some("TextTrackCue"), None, &mut ledger);
    installer::put_ledger(scope, ledger);
}

/// `MediaCapabilities` in the WORKER realm (invariant I8), called from
/// `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_small_elements`].
///
/// The one interface of this lane the worker capture lists -- `navigator.mediaCapabilities` is
/// `[Exposed=(Window,Worker)]`, and `worker.surface.json` records `MediaCapabilities` with a member
/// list byte-identical to the window's and lists none of the other twelve. The inventory is
/// name-based, so the table left BOTH generated registries; without this call the worker would lose
/// the interface entirely.
pub fn install_worker_media<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::html::media_objects::MEDIACAPABILITIES;
    let global = scope.get_current_context().global(scope);
    let mut ledger = installer::take_ledger(scope);
    let built =
        installer::create_interface(scope, global, &MEDIACAPABILITIES, "MediaCapabilities", None, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] media install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
}

