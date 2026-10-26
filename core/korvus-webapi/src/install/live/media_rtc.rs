//! Real-time media: WebRTC, `MediaStream` and its tracks, `MediaRecorder`, the WebCodecs value
//! pair, Media Session, Media Source Extensions, and the CSS Font Loading pair that shared
//! `dom/09k-media-rtc.js` with them. The media ELEMENT family is [`super::media`].

use deno_core::v8;

use super::rows::{build_newable, parent_of, rechain_orphans};
use crate::install::installer::{self, Flavor};
use crate::install::InterfaceDef;

/// `RTCSessionDescription` and `RTCIceCandidate` from their `interface!` tables, in the WINDOW.
///
/// 20 rows that were two blocks of `dom/09k-media-rtc.js` (81 lines), two `op_nativize_ctor` native
/// masks and 23 `op_nativize` ones; `core/webrtc/rtc_values.rs` is their only source.
///
/// Both are NEWABLE (`Model::Value` in the capture, and both constructors are real: the recorded
/// answers include `new RTCSessionDescription()` succeeding and `new RTCIceCandidate()` throwing
/// TypeError), so each is built by [`super::rows::build_newable`] over a native constructor rather than by
/// `create_interface`, whose `Model::Value` interface object is a placeholder. Neither inherits, which
/// is what `parent: None` in the capture says.
///
/// WHY THIS RUNS BEFORE THE BOOTSTRAP SCRIPT, beside
/// [`super::install_window_small_elements`]. Every remaining `.js` file, `BOOTSTRAP` and `host.js`
/// were grepped for both interface names: after this lane's deletions
/// the only hits are gone, so nothing later defines or decorates a member of either and the table is
/// the last writer. The early position also means the interface objects exist before `host.js`'s
/// `enumerateProto` sweep reads the global, the same reason L11b and L11c run here.
///
/// Window-only: the bundle's `worker.surface.json` lists neither, and both left BOTH generated
/// registries with the tables, so nothing else on either surface defines them.
pub fn install_window_media_rtc<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    use crate::core::fonts::font_face::{cb_font_face_ctor, FONTFACE, FONTFACESET};
    use crate::core::media::media_recorder::{cb_recorder_ctor, MEDIARECORDER};
    use crate::core::media::media_session::{cb_metadata_ctor, MEDIAMETADATA};
    use crate::core::media::media_source::{cb_media_source_ctor, MEDIASOURCE, SOURCEBUFFERLIST};
    use crate::core::media::media_stream::{cb_stream_ctor, CANVASCAPTUREMEDIASTREAMTRACK, MEDIASTREAM, MEDIASTREAMTRACK};
    use crate::core::media::video_frame::{cb_color_space_ctor, cb_frame_ctor, VIDEOCOLORSPACE, VIDEOFRAME};
    use crate::core::webrtc::peer_connection::{cb_pc_ctor, RTCDATACHANNEL, RTCPEERCONNECTION, RTCSTATSREPORT};
    use crate::core::webrtc::rtc_values::{cb_ice_ctor, cb_sdp_ctor, RTCICECANDIDATE, RTCSESSIONDESCRIPTION};
    use crate::core::html::text_tracks::{TEXTTRACKCUELIST, VIDEOPLAYBACKQUALITY};
    use crate::core::webrtc::rtp::{RTCDTMFSENDER, RTCRTPRECEIVER, RTCRTPSENDER, RTCRTPTRANSCEIVER};

    let et = parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    let build = |scope: &mut v8::PinScope<'s, '_>,
                     ledger: &mut installer::InstallLedger,
                     def: &'static InterfaceDef,
                     parent: Option<(v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>)>|
     -> (v8::Local<'s, v8::Object>, v8::Local<'s, v8::Function>) {
        let built = installer::create_interface(scope, global, def, def.name, parent, None, Flavor::Table, ledger);
        for g in &built.gaps {
            eprintln!("[korvus] media/rtc install gap: {}.{}", g.interface, g.member);
        }
        (built.proto, built.ctor)
    };

    // The two value objects a page constructs by hand, and the two newables of the object graph.
    build_newable(scope, global, &RTCSESSIONDESCRIPTION, cb_sdp_ctor, None, None, &mut ledger);
    build_newable(scope, global, &RTCICECANDIDATE, cb_ice_ctor, None, None, &mut ledger);

    // PHASE 3 F7: what `enumerateDevices()` lists -- the base first, so `InputDeviceInfo` splices onto it.
    {
        use crate::core::media::media_device_info::{INPUTDEVICEINFO, MEDIADEVICEINFO};
        let device = build(scope, &mut ledger, &MEDIADEVICEINFO, None);
        build(scope, &mut ledger, &INPUTDEVICEINFO, Some(device));
    }

    // WebIDL inheritance order: the track base first, so the canvas capture track splices onto it.
    let track = build(scope, &mut ledger, &MEDIASTREAMTRACK, et);
    build(scope, &mut ledger, &CANVASCAPTUREMEDIASTREAMTRACK, Some(track));
    build_newable(scope, global, &MEDIASTREAM, cb_stream_ctor, Some("EventTarget"), None, &mut ledger);
    build_newable(scope, global, &MEDIARECORDER, cb_recorder_ctor, Some("EventTarget"), None, &mut ledger);

    build(scope, &mut ledger, &RTCSTATSREPORT, None);
    build(scope, &mut ledger, &RTCDATACHANNEL, et);
    build(scope, &mut ledger, &RTCRTPTRANSCEIVER, None);
    build(scope, &mut ledger, &RTCRTPSENDER, None);
    build(scope, &mut ledger, &RTCRTPRECEIVER, None);
    build(scope, &mut ledger, &RTCDTMFSENDER, et);
    build_newable(scope, global, &RTCPEERCONNECTION, cb_pc_ctor, Some("EventTarget"), None, &mut ledger);

    // The WebCodecs value pair and the Media Session value object: three newables that inherit
    // nothing, which is what `parent: None` in the capture says.
    build_newable(scope, global, &VIDEOCOLORSPACE, cb_color_space_ctor, None, None, &mut ledger);
    build_newable(scope, global, &VIDEOFRAME, cb_frame_ctor, None, None, &mut ledger);
    build_newable(scope, global, &MEDIAMETADATA, cb_metadata_ctor, None, None, &mut ledger);

    // W5.1 L4b: the Media Source Extensions pair. `SourceBufferList` first, because a `MediaSource`
    // mints two of them in its constructor and the mint reads that interface's prototype off the
    // global. `MediaSource` is this tree's FIRST UNION TABLE -- the table carries the worker
    // capture's thirteen prototype members and the window's selector leaves out the one the window
    // capture does not have (`handle`); see `core/media/media_source.rs`.
    build(scope, &mut ledger, &SOURCEBUFFERLIST, et);
    // `SourceBuffer`, which `addSourceBuffer` mints (`core/media/mse.rs`).
    build(scope, &mut ledger, &crate::core::media::mse::SOURCEBUFFER, et);
    // NO HAND SELECTOR ANY MORE. The realm's member list used to be written down
    // here, copied out of one capture; `install::installer::join` reads it off the ACTIVE
    // capture instead, so the list is derived and a bundle swap moves it.
    build_newable(scope, global, &MEDIASOURCE, cb_media_source_ctor, Some("EventTarget"), None, &mut ledger);

    // W5.1 L4b: the CSS Font Loading pair, the LAST two interfaces of `dom/09k-media-rtc.js`. Both
    // captures record both with byte-identical member lists, so neither needs a selector.
    build_newable(scope, global, &FONTFACE, cb_font_face_ctor, None, None, &mut ledger);
    build(scope, &mut ledger, &FONTFACESET, et);

    // The last two interfaces of the TEXT-TRACK family, which were generated-registry shells whose
    // bodies `dom/09k-media-rtc.js` supplied. Installed from THIS lane rather than from
    // `install_window_media` because this is the lane that gives them bodies; the other four of the
    // family (`TextTrack`, `TextTrackList`, `TextTrackCue`, `VTTCue`) were already L3 tables and only
    // their row BODIES changed, in `core/html/media_objects.rs`. Neither has a registry child and
    // neither is in the worker capture.
    build(scope, &mut ledger, &TEXTTRACKCUELIST, None);
    build(scope, &mut ledger, &VIDEOPLAYBACKQUALITY, None);

    installer::put_ledger(scope, ledger);
    // THIS LANE NO LONGER MOUNTS TWO MEMBERS OF INTERFACES IT DOES NOT OWN.
    // `install_canvas_capture_stream` and `install_document_fonts` stood here and defined
    // `HTMLCanvasElement.captureStream` and `Document.fonts` over those tables' `stub` rows, the
    // arrangement "the row is the SHAPE and a later pass supplies the BODY". Both are second writers
    // by invariant I2's definition -- an unbranded `build_fn` in a slot the install ledger claims --
    // and `installer::audit_surface` reported both `clobber:engine`. The BODIES are unchanged and
    // still live with the state they mint (`core/media/media_stream.rs`'s `cb_canvas_capture_stream`,
    // `core/fonts/font_face.rs`'s `cb_document_fonts`); the rows in `HTMLCanvasElement`'s and
    // `Document`'s own tables NAME them, so the one installer mounts them and both passes are gone.
    // TWO REGISTRY INTERFACES INHERIT FROM ONE THIS LANE TOOK OUT OF THE REGISTRY:
    // `BrowserCaptureMediaStreamTrack` and `MediaStreamTrackGenerator` are both
    // `: MediaStreamTrack`. The chain pass ran during the ESM layer, when `MediaStreamTrack` was no
    // longer a registry name and not yet a table one, so it left both on the bare `Object.prototype`
    // their shells were minted with -- measured as the only two CHANGED entries when the installer
    // golden was re-captured for this lane. [`super::rows::rechain_orphans`] repairs exactly that, derived from the
    // registry rather than from a written-down pair, and it touches only a child whose prototype is
    // still empty, so it cannot disturb a chain another pass set deliberately.
    rechain_orphans(scope, global, crate::install::generated_registry::REGISTRY);
}

/// The interfaces of this lane the WORKER capture lists, from the SAME tables (invariant I8), called
/// from `core/korvus-runtime/src/worker.rs` beside [`super::install_worker_small_elements`].
///
/// `RTCDataChannel`, `VideoFrame` and `VideoColorSpace` are all `[Exposed=(Window,Worker)]`, and
/// `worker.surface.json` records each with a member list BYTE-IDENTICAL to the window's (compared key
/// by key, 2026-09-11); it lists none of the lane's others. The table inventory is name-based, so
/// these tables left BOTH generated registries -- without this call the worker would lose all three
/// outright.
pub fn install_worker_media_rtc<'s>(scope: &mut v8::PinScope<'s, '_>) {
    use crate::core::fonts::font_face::{cb_font_face_ctor, FONTFACE, FONTFACESET};
    use crate::core::media::media_source::{cb_media_source_ctor, MEDIASOURCE, SOURCEBUFFERLIST};
    use crate::core::media::video_frame::{cb_color_space_ctor, cb_frame_ctor, VIDEOCOLORSPACE, VIDEOFRAME};
    use crate::core::webrtc::peer_connection::RTCDATACHANNEL;
    let global = scope.get_current_context().global(scope);
    let et = parent_of(scope, "EventTarget");
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, global, &RTCDATACHANNEL, "RTCDataChannel", et, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] media/rtc install gap: {}.{}", g.interface, g.member);
    }
    build_newable(scope, global, &VIDEOCOLORSPACE, cb_color_space_ctor, None, None, &mut ledger);
    build_newable(scope, global, &VIDEOFRAME, cb_frame_ctor, None, None, &mut ledger);
    // W5.1 L4b: the MSE pair, from the SAME tables the window uses (invariant I8). `SourceBufferList`
    // first, because the `MediaSource` constructor mints two of them. NO SELECTOR here: the worker
    // takes the union table whole, which is the realm the 13th member (`handle`) belongs to.
    let built = installer::create_interface(scope, global, &SOURCEBUFFERLIST, "SourceBufferList", et, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] media/rtc install gap: {}.{}", g.interface, g.member);
    }
    let built = installer::create_interface(scope, global, &crate::core::media::mse::SOURCEBUFFER, "SourceBuffer", et, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] media/rtc install gap: {}.{}", g.interface, g.member);
    }
    build_newable(scope, global, &MEDIASOURCE, cb_media_source_ctor, Some("EventTarget"), None, &mut ledger);
    // W5.1 L4b: the CSS Font Loading pair, from the SAME tables (invariant I8). `worker.surface.json`
    // records both with member lists byte-identical to the window's. `WorkerGlobalScope.fonts` is NOT
    // this lane's -- it stays that interface's registry row, answering `undefined` as it does today.
    build_newable(scope, global, &FONTFACE, cb_font_face_ctor, None, None, &mut ledger);
    let built = installer::create_interface(scope, global, &FONTFACESET, "FontFaceSet", et, None, Flavor::Table, &mut ledger);
    for g in &built.gaps {
        eprintln!("[korvus-worker] media/rtc install gap: {}.{}", g.interface, g.member);
    }
    installer::put_ledger(scope, ledger);
    // Symmetry with the window: repair any worker-registry child whose parent is a name this lane
    // took out of that registry. `RTCDataChannel` has no subclass in the worker capture today, so this
    // is a no-op there -- and it is derived, so it stays right if one appears.
    rechain_orphans(scope, global, crate::install::generated_registry::WORKER_ORDER);
}
