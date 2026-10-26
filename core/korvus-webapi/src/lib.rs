//! `webapi` — the DOM / Web-API surface (the host layer).
//!
//! DOM tree, events, observers, `HTMLElement`, plus the
//! DOM-facing bindings. Non-DOM Web APIs (fetch/url/crypto/streams/encoding) come from Deno `ext/`
//! crates and are wired in the runtime rather than reimplemented here.
//!
//! Persona rule: every persona-visible value is read through the persona the runtime provides; there
//! are no hardcoded device values here (the bundle is the single source of persona values).
//!
//! G1.5: the arena DOM ([`dom`]) — HTML parsed via html5ever into a flat `NodeId` arena. G1.6
//! exposes it to JS through native `#[op2]` Document/Element/Node classes.

// So the `interface!` proc-macro's output (`::webapi::install::...`) resolves inside this crate too,
// not only in downstream crates that depend on `webapi`.
extern crate self as webapi;

/// Re-export the one-table macro so a table reads `webapi::interface! { ... }` (the spelling the
/// design uses), rather than reaching into the `macros` crate directly.
pub use macros::interface;

#[macro_use]
mod icpt; // `icpt!`: interceptor callbacks, recorded in an analysis build
#[macro_use]
pub mod behavior_rt;
pub mod binding;
pub mod install;
#[cfg(feature = "analysis")]
mod analysis_hook; // korvus-analysis Layer 1: the call capture (plugins/korvus-analysis drains it)
#[cfg(feature = "analysis")]
pub use analysis_hook::call_traced;
#[cfg(feature = "analysis-v8")]
pub use analysis_hook::install_builtin_trace;
#[cfg(feature = "analysis-v8")]
pub use analysis_hook::calibrate_hook_exits;
// The `core::geometry` files use the `stub!`/`dispatch!` macros from `behavior_rt`, so `core` must
// be declared after the `#[macro_use]` above — the textual macro scope then reaches the nested
// geometry files (`core/geometry/{dom_rect,dom_point,dom_matrix}.rs`).
pub(crate) mod core;
/// The page pipeline's Rust seam for the kernel: document install, parser-induced loads,
/// document readiness, the load-event deadline, `<body>` handler attributes.
pub mod page;
pub mod devtools_log; // What a CDP client with `Runtime` enabled is told the page logged, and the bindings it added.
pub(crate) mod modules;
mod idb;
mod crypto;
/// RSA and elliptic-curve keys on the BoringSSL this binary already links.
pub mod asym;
pub use korvus_dom as dom;

pub use binding::host_dom::HostDom;
pub use core::html::html_canvas_element::RenderState;
/// The tables this tree defines and this bundle does not expose; read by
/// `tests/exclusion.rs`, which asserts nothing installs them and neither capture records them.
pub use core::html::UNEXPOSED_BY_THIS_BUNDLE;

/// The browsing context whose realm is running (a frame's id), `None` for the page's own realm and in
/// a worker: korvus's frame realms share the page's V8 context for native callbacks, so this -- not the
/// current context -- is what tells a frame's call from the page's (the per-realm timer handles).
pub fn running_realm(scope: &mut deno_core::v8::PinScope) -> Option<u32> {
    core::html::browsing_context::running(scope)
}
/// The marker a request client in an opaque origin carries to the runtime's page-request preparation (`Origin: null`).
pub const OPAQUE_CLIENT: &str = "korvus-opaque-origin:";

extern "C" {
    fn korvus_incumbent_context(isolate: *mut std::ffi::c_void) -> *const deno_core::v8::Context;
}

/// The browsing context of the INCUMBENT realm -- the realm of the JavaScript function that made the call now running
/// in a native callback (V8's `GetIncumbentContext`, through `shim/realm.cc`); `None` for the page and a worker, and
/// when no JavaScript is on the stack. The window whose `setTimeout` a script calls is, but for a script calling another
/// window's method by name (`parent.setTimeout`), the script's own: korvus builds every window's timer functions in the
/// page's context, so neither their context nor their `this` names the frame (`scope_timers`).
pub fn incumbent_realm(scope: &mut deno_core::v8::PinScope) -> Option<u32> {
    let isolate: &mut deno_core::v8::Isolate = scope;
    // SAFETY: `UnsafeRawIsolatePtr` is `repr(transparent)` over the C++ `v8::Isolate*` the shim's call is a member of
    // (as `install_codegen_hook`); the returned pointer is a `Local` handle in the current handle scope, or null.
    let ctx: Option<deno_core::v8::Local<deno_core::v8::Context>> = unsafe {
        let raw = isolate.as_raw_isolate_ptr();
        let ptr: *mut std::ffi::c_void = std::mem::transmute(raw);
        let c = korvus_incumbent_context(ptr);
        if c.is_null() { None } else { Some(std::mem::transmute::<*const deno_core::v8::Context, deno_core::v8::Local<deno_core::v8::Context>>(c)) }
    };
    let g = ctx?.global(scope);
    core::html::browsing_context::ctx_of_window(scope, g)
}
/// How many times this thread has built the `CSSStyleDeclaration` templates -- once per
/// runtime, not once per declaration. Read by `korvus-runtime`'s `computed_style` test as a count.
pub use core::css::css_style_declaration::declaration_template_builds;
/// A fetched classic script compiled off the page's thread, for the classic-script compile to take (`core::html::script_stream`).
pub use core::html::script_stream::compile as compile_streamed_script;
pub use core::html::task_queue::ScriptBelow;
pub use core::css::cssom::{deliver_frame, note_paint_without_compositor, offer_frame_again, render_document_frame, render_document_png, render_document_pdf, render_document_rgba, render_document_shot, render_popup_shot, PrintOptions};
pub use core::dom::geometry_queries::layout_metrics;
pub use core::dom::document::take_reopened_lifecycle;
pub use core::timing::loaf::{begin_task as loaf_begin_task, install_loaf_monitor, loaf_rendering_opportunity, task_end as loaf_task_end, task_intervals as loaf_task_intervals, task_start as loaf_task_start};
pub use core::timing::performance::mark_heap_baseline;
/// The realm's clock read in Rust (`performance.now()` without a page-replaceable lookup), and an `Instant` stamped on
/// it as Blink stamps an event's time: the runtime's trusted input takes its `timeStamp`s here.
pub use core::timing::performance::{clock_now_ms as clock_now, instant_to_ms as clock_at};
pub use core::timing::resource_timing::{drain_module_fetches, ModuleFetches};
/// A Trusted Types sink's argument as Blink's bindings convert it (the runtime's `importScripts`).
pub use core::trusted_types::tables::{sink_string, sink_text};
pub use core::html::animation_frame::has_pending_frames as has_pending_animation_frame;
/// What an isolate's template cache did, for the runtime's `Engine::template_counts`.
pub use install::templates::{template_counts, TemplateCounts};
/// Blueprint files -- the `korvus` binary switches them on; the runtime writes what a
/// process recorded once its top-level window realm has installed.
pub use install::templates::{enable_disk_cache as enable_template_disk_cache, persist as persist_templates, report_loaded_surfaces};
// The context snapshot (slice 2a): the template cache in and out of a snapshot of the page realm
#[cfg(not(feature = "analysis"))]
pub use install::templates::{restore_templates, snapshot_file, snapshot_templates};
// The context snapshot (slice 3): the rest of what a snapshot of the page realm carries
#[cfg(not(feature = "analysis"))]
pub mod snapshot;

/// Trusted input — the CDP `Input` domain's entry points.
///
/// korvus-runtime synthesises user gestures through these; they dispatch genuinely-trusted events
/// (`isTrusted === true`) rather than the `dispatchEvent` a page script can forge. They used to be
/// reached as `webapi::bindings::*`; `bindings` is gone, and this is the named home they should
/// always have had.
pub mod input {
    /// Stamp an event the ENGINE built as trusted (`isTrusted` true) -- the runtime's worker-side
    /// message delivery uses it.
    pub fn mark_event_trusted(scope: &mut deno_core::v8::PinScope, ev: deno_core::v8::Local<deno_core::v8::Object>) {
        crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    }
    pub use crate::core::dom::shared::selector_exists;
    pub use crate::core::events::shared::{
        dispatch_trusted_mouse, dispatch_trusted_mouse_on_selector, dispatch_trusted_pointer,
        dispatch_trusted_pointer_on_selector, dispatch_trusted_touch, dispatch_trusted_wheel,
        focus_selector,
    };
    /// The pointer's hover chain and its four boundary events -- what the engine calls
    /// before each `mousemove` sample. A pointer that leaves the document needs no separate entry
    /// point: a move to a point on no box empties the chain through this one.
    pub use crate::core::events::hover::move_pointer_to;
    /// FIXER C6: a real mouse -- one platform mouse or wheel event in, every DOM event Blink 150 makes of it out
    /// (`mouse_input`, `wheel_input`); the element a person would click (`element_center`); the frame interval
    /// continuous input is aligned to.
    pub use crate::core::events::pointer_input::{
        button_bit, dispatch_drag, element_center, frame_interval_ms, held_button, mouse_input, mouse_input_at, platform_button, platform_move,
        set_intercept_drags, single_event, take_drag_intercepted, wheel_input, Modifiers, MouseInput, MouseKind, WheelInput, NO_BUTTON,
    };
    /// The focus ring. `focus_under_pointer` is `mousedown`'s default action -- the
    /// press that puts the caret in the field a person just clicked; `move_focus_to` is the
    /// primitive under it and under `focus_selector`.
    pub use crate::core::events::focus::{focus_under_pointer, move_focus_to};
    /// Typing AT THE FOCUS, which is the only place a browser types. `insert_text_at_focus`
    /// is CDP `Input.insertText` (one edit, no key events); `key_at_focus` is one keystroke and the
    /// `keypress` + editing command that follow it. Both replace the document-targeted dispatchers
    /// and the selector-targeted ones, which is why neither `dispatch_trusted_key*` nor
    /// `edit_value_on_selector` is exported any more -- they are deleted, not hidden.
    pub use crate::core::events::typing::{ime_set_composition_at_focus, insert_text_at_focus, key_at_focus, Stroke};
    /// The whole key event (`KeyInput`, CDP's parameters verbatim or the platform keyboard's
    /// via `platform_key_input`), its modifier state (`KeyMods`, `held_modifiers` -- what a pointer event
    /// made while a key is held carries), and user activation, which trusted key AND pointer input set
    /// (`notify_user_activation` is the one hook both call).
    pub use crate::core::events::typing::{held_modifiers, platform_key_input, KeyInput, KeyMods};
    pub use crate::core::events::user_activation::{
        consume_transient_user_activation, has_sticky_user_activation, has_transient_user_activation, notify_user_activation,
    };
}
// MessagePort's cross-realm plumbing moved to its interface file (STEP 3); korvus-runtime's worker
// bridge still reaches it through the crate root.
pub use core::messaging::message_port::{is_message_port, mp_remote_port, mp_take_peer};
/// V8-serialized messages (a worker's postMessage, a cross-isolate BroadcastChannel): serializable platform
/// objects and transferred ports as host objects.
pub use crate::modules::clone_state::{wire_deserialize, wire_serialize};
// The `webapi` plugin slot: a claimed member's call, as the plugin host reads and answers it.
mod plugin_call;
// Moved to their domains (file-move STEP 1); still re-exported here for korvus-runtime (worker.rs).
pub use core::dom::{document_base_url, set_document_url, CookieUrl, DocEncoding, DocumentUrl, PageContentType};
/// Which interfaces the DOM spine creates with an EMPTY selector, so `tests/generated_registry.rs` can
/// gate that set against the registry's `backfill` column: the two passes are a pair, and an interface
/// in one and not the other gets a silently bare prototype.
pub use core::dom::is_html_element_shell;
/// The layout cache and the media context the runtime's CSSOM ops share with the geometry rows (W5.0d).
pub use core::dom::{document_layouts, layout_counts, media_context, refresh_layout_cache, LayoutCache, NaturalImages};
/// The window geometry FALLBACK CHAIN, which the `Window` table's `innerWidth` row, the
/// cascade's `media_context` and the runtime's synthetic-input driver all read. One definition.
pub use core::html::dialog::{set_bridge as set_dialog_bridge, DialogBridge};
pub use core::html::popup::{opened_by_script, opener_gone, opener_message, popup_closed, popup_message, set_bridge as set_popup_bridge, set_opened_by_script, PopupBridge};
pub use core::html::window_geometry::{set_window_bounds, window_geometry_of, window_resized, WindowBounds, WindowState};
/// The media header probe and its store: the kernel's loader reads a `video`/`audio` body's header and
/// hands the page's realm the answer, not the bytes.
pub use core::html::media_probe::{probe as media_probe, MediaProbe, MediaProbes};
pub use core::fonts::font_probe::{probe as font_probe, FontProbes};
/// `screen.orientation`. The accessor itself is a NATIVE row of `Screen.prototype`
/// (`JsScreen` in korvus-runtime, where the other ten live and where the member's position before
/// `constructor` is decided); the object it answers is built here, from the persona.
pub use core::html::screen_orientation::screen_orientation;
/// The uncaught-error tally (HTML "report an exception") and the worker realm's event-model install.
pub use binding::hidden::hide_deno;
/// Seal this realm's interface prototypes into a V8 Private, so a mint stops resolving
/// through a `globalThis[Iface]` the page can write. Called once per realm, after the LAST install
/// and before any page or worker script.
pub use binding::protos::seal_interface_protos;
pub use core::workers::service_worker::SwClientPort;
/// The page realm's install-time global surface, which a nested context's window is
/// built from. Called once, right after [`seal_interface_protos`], under the same contract.
pub use core::html::seal_frame_surface;
/// A DevTools client's isolated worlds (`Page.createIsolatedWorld`, a new-document script's `worldName`).
pub use core::html::isolated_world::{
    create_isolated_world, create_isolated_world_in, devtools_realm_context, devtools_realm_of, isolated_world_context, isolated_world_of, FRAME_REALM_BASE,
};
pub use core::html::browsing_context::{
    close_popup, content_frame_of, create_frame_world, devtools_frames, devtools_popups, frame_of_node, frame_realm, navigate_popup, popup_document_root, DevtoolsFrame,
    DevtoolsPopup,
};
pub use core::events::report::{
    begin_module_evaluation, end_module_evaluation, install_rejection_handler, report_exception, report_exception_at, caught_site,
    reset_uncaught_errors, reset_unhandled_rejections, ErrorSite,
    uncaught_error_count, uncaught_errors, unhandled_rejections,
};
pub use core::events::install_worker_event_model;
/// The ServiceWorker realm's own two entry points, called from `korvus-runtime`'s worker
/// block -- the fetch transport at realm setup, and the lifecycle when the SW's script has loaded.
pub use core::workers::service_worker_realm::{install_sw_fetch_transport, script_loaded as sw_script_loaded};
/// The HOST side of HTML's "delaying the load event" set. `Engine::pump`
/// stamps the event loop's turns, which the starvation watchdog inside
/// `core/html/browsing_context.rs` reads, and releases a load step that a DRAINED loop can never
/// release. Neither is a deadline: Chrome's `load` waits for its subresources however long they take.
pub use core::html::browsing_context::{event_loop_turns, release_unreleasable_load_delays, EventLoopTurns};
/// `p.then(ok, err)` with each step run as a task, not a promise job (`core/html/task_queue.rs`).
pub use core::html::task_queue::{queue_next_task, run_as_task, then_new_task, then_task, AfterTaskHook};
pub use core::events::report::notify_rejected_promises;
/// The TOP-LEVEL navigation a document asks for, and the request that performs it. The
/// slot is here because the three fillers are -- `location.*` (through `korvus-runtime`'s
/// `location.rs`), `window.open(url, "_self")` and a form submission targeting the page -- and the
/// drivers (`korvus-cdp`, `korvus fetch`) take it through `Engine::take_due_navigation`.
pub use core::html::browsing_context::{
    request_top_navigation, NavigationReason, NavigationTarget, PageNavigation, PendingNavigation,
};
/// The Navigation API's events: the runtime fires `navigate` before each navigation of the
/// page it commits and reports each commit, and performs the navigations this crate starts.
pub use core::html::navigate_event::{
    before_update as navigation_before_update, can_change_to_url_for_history_api, dispatch_navigate_event, document_committed as navigation_document_committed,
    invalid_url_text, load_event_finished as document_load_event_finished, set_page_hooks, traverse_cancelled, update_for_navigation, ClientReason,
    DispatchResult, HistoryHandling, NavigateCommit, NavigateEventType, NavigateParams, NavigationType, PageHooks, PageNavigate,
};
/// A frame's own History: `pushState` / `replaceState` / `state` in a frame's realm.
pub use core::html::browsing_context::{frame_history_change, frame_history_state};
/// B4 (ABI 3 step 10): install a loaded plugin's bound globals on the current (top-page) realm -- the runtime
/// calls this at the top page's injected-script point, before the page's own scripts (frames get them in
/// `run_doc_scripts`).
pub use core::html::browsing_context::{install_named_bindings, install_plugin_bindings_current};
/// The HOST's rendering opportunity for the geometry observers: `Engine::pump` offers it
/// between slices of the event loop, and it arms a frame only when an observed answer could have moved.
pub use core::intersection_observer::rendering::{next_frame_in_ms, rendering_opportunity};
pub use core::animation::css::{loop_resumed as css_loop_resumed, loop_stopped as css_loop_stopped, next_event_in as css_next_event_in};
/// The animation timeline moves to a fresh moment. The HOST's rendering opportunity
/// calls it beside [`rendering_opportunity`]; between two of them the timeline is frozen, which is
/// what Chrome's is, and what lets a style memo survive a page that has an animation running.
pub use core::animation::advance_frame_time;
pub use core::messaging::{PortForward, mp_register_remote};
pub use modules::broadcastchannel::BcForward;

/// StructuredSerialize and StructuredDeserialize of `v` in the current realm, a refusal thrown as `iface.method`'s
/// `DataCloneError` -- `history.pushState`'s state.
pub fn structured_clone_for<'s>(scope: &mut deno_core::v8::PinScope<'s, '_>, v: deno_core::v8::Local<'s, deno_core::v8::Value>, iface: &str, method: &'static str) -> Option<deno_core::v8::Local<'s, deno_core::v8::Value>> {
    modules::structured_clone::clone_for(scope, v, iface, method)
}

/// The document's base URL (`<base href>`, else its URL) -- what `history.pushState`'s URL resolves against.
pub fn page_base_url(scope: &mut deno_core::v8::PinScope) -> String {
    core::dom::base_url(scope)
}

/// Banner string used by the binary.
pub fn surface_name() -> &'static str {
    "korvus-webapi (native DOM, G1.6)"
}

/// The v8 Private name a `Worker` instance's isolate id lives on.
///
/// The `Worker` TABLE writes it (`core/workers/worker.rs`) and `korvus-runtime`'s `postMessage` /
/// `terminate` bodies read it, so the name is exported rather than spelled twice — this is the ONE
/// fact the two crates share about a Worker instance.
pub fn worker_id_slot() -> &'static str {
    crate::core::workers::worker::ID_SLOT
}

/// One op off the STASHED ops table, by name.
///
/// `op_stash_ops` puts `Deno.core.ops` in a v8 `Private` on the global before the `Deno` global is
/// deleted for stealth, and this is the one reader. `korvus-runtime` needs it for the WORKER's timer
/// pump, which has to reach the async `op_timer_next_due` as a JS function to get the promise whose
/// resolution IS the next turn of the event loop — the property the whole timer task source exists
/// for. `None` where the op is not registered in this realm.
pub fn stashed_op<'s>(
    scope: &mut deno_core::v8::PinScope<'s, '_>,
    name: &str,
) -> Option<deno_core::v8::Local<'s, deno_core::v8::Function>> {
    crate::core::xmlhttprequest::xml_http_request::deno_op(scope, name)
}

/// Build the worker realm's global scope from its tables and chain `globalThis` onto it
/// (`core/workers/worker_global_scope.rs`). Called by `korvus-runtime`'s worker bootstrap.
pub use core::workers::worker_global_scope::{install_worker_global_scope, publish_scope_singleton};
pub use core::workers::worker::WorkerErrorFrame;
/// Arm an isolate's main-side worker pumps (the SW BroadcastChannel/MessagePort reply drains). The
/// `navigator.serviceWorker.register` path arms these itself; `Engine::spawn_background` (a plugin-spawned MV3
/// background) must call this too, or the SW's reply has no main-side drain. Idempotent (`PumpedIsolates`).
pub use core::workers::shared_worker::start_isolate_pumps;
/// A worker realm's cross-origin isolation, from its creator.
pub use core::html::isolation_policy::apply_worker_isolation;

/// The worker realm's REGISTRY install -- every capture-shaped interface that is not a hand table.
/// It was `generated_worker_shells.js`'s first statement; the file is deleted and this
/// is the FIRST of the worker's Rust installs instead.
pub use install::installer::install_worker_shells;

// ── The extension assembly. This is all that was left of bindings.rs once every interface,
// helper and backend had moved to its own home (BLUEPRINT STEP 3/6). Ops and cppgc objects are
// collected from each interface file's own declaration — see binding/exposure.rs.

use deno_core::extension;
pub(crate) use crate::core::html::canvas_2d::Canvas2DStore;
pub use crate::core::html::text_viewer; // A/B C5: the viewer documents, for the page's navigation (korvus-runtime) and a frame's.
pub use crate::core::html::webaudio::AudioStore;
pub use crate::core::html::webgl::WebGLStore;
use crate::modules::indexeddb::backend::IdbBackend;
use crate::binding::{DocumentGlobal, ElementProto, MathmlElementProto, MathmlTagProtos, NodeCache, NodeProtos, OwnerMap, QNameMap, ShadowRootProto, SvgElementProto, SvgTagProtos, TagProtos, UnknownElementProto};
use crate::core::dom::ScrollState;
use crate::core::events::EventListeners;
use crate::modules::cache_storage::CacheStore;
use crate::modules::broadcastchannel::BcRegistry;
use crate::core::messaging::PortRegistry;
use crate::core::events::report::UncaughtErrors;
use crate::core::events::shared::FocusState;
use crate::modules::storage::storage::HostStorage;
use crate::modules::cookie_store::jar::HarnessCookies;

fn webapi_registered_ops() -> Vec<deno_core::OpDecl> {
    crate::binding::exposure::ops()
}

extension!(
    webapi_core_ext,
    // Registration is collected from each interface file's own `webapi_ops!` declaration; deno_core
    // expands `ops_fn` to `ext.ops.to_mut().extend(collected())`. See binding/exposure.rs.
    ops_fn = webapi_registered_ops,
    state = |state| {
        state.put(HostDom::default());
        state.put(crate::core::animation::AnimationStore::default()); // the Web Animations model (Track 3.2).
        state.put(IdbBackend::default());
        state.put(BcRegistry::default());
        state.put(PortRegistry::default());
        state.put(CacheStore::default());
        state.put(Canvas2DStore::default()); // R0 render spine: per-context 2D display lists.
        state.put(WebGLStore::default()); // R3 render spine: per-context GL command lists.
        state.put(AudioStore::default()); // R3.5 render spine: per-context Web Audio graphs.
        state.put(ScrollState::default());
        state.put(FocusState::default()); // focus tracking: document.activeElement via el.focus().
        // The value a text control held just before the user's FIRST edit of the
        // current focus session -- what `change` compares against when the focus leaves.
        state.put(crate::core::events::typing::UserEditState::default());
        // the cookies of a document the engine was handed as a string (`transport::cookies`, "the harness")
        state.put(HarnessCookies::default());
        state.put(EventListeners::default());
        state.put(HostStorage::default());
        state.put(NodeCache::default());
        state.put(DocumentGlobal::default());
        state.put(NodeProtos::default());
        state.put(ShadowRootProto::default());
        state.put(UnknownElementProto::default());
        state.put(TagProtos::default());
        state.put(SvgTagProtos::default());
        state.put(SvgElementProto::default());
        state.put(ElementProto::default());
        state.put(MathmlTagProtos::default());
        state.put(MathmlElementProto::default());
        state.put(OwnerMap::default());
        state.put(QNameMap::default());
        state.put(UncaughtErrors::default());
        // The unhandled-rejection tally, the other half of the reporting store.
        state.put(crate::core::events::report::UnhandledRejections::default());
        // W5.0d: the nested/auxiliary browsing contexts, the SharedArrayBuffer gate, the layout cache.
        state.put(crate::core::html::BrowsingContexts::default());
        state.put(crate::core::html::IsolationGate::default());
        state.put(LayoutCache::default());
        // The document base URL (`<base href>`), memoised on the DOM's revision counter.
        // Every URL-valued reflection reads it now, so the `<base>` scan cannot be per-call.
        state.put(core::dom::BaseUrlCache::default());
        // <input> checkedness (the dirty value, keyed by node id). Shared rather than per-wrapper
        // because `new FormData(form)` reads it straight off the arena -- see html_input_element.rs.
        // The blob-URL registry `URL.createObjectURL` writes. It was two JS `Map`s in
        // `host.js` (only the serial had moved, into the runtime's `natives::BlobUrlSeq`); the two
        // synchronous readers -- `host.js`'s `resolveWorkerSource` and `core/html/browsing_context.rs` --
        // read it through Rust now, so the `KB.__blobUrlContents` bridge slot is gone.
        state.put(crate::modules::url::url::BlobUrlRegistry::default());
    },
    // The `customizer` that put each file's `webapi_objects!` classes into deno_core's `objects =` list is gone:
    // every class is an own class, built from its table.
);

// `webapi_ext` WAS HERE, AND `core/korvus-webapi/src/js/` WITH IT. The extension carried
// no ops, no state and no customizer -- only an `esm = [...]` list and the entry point that ordered
// it. All five of those files are deleted and every statement they ran is
// `install::live::install_window_bootstrap`, called as the first line of the runtime's pre-bootstrap
// block, which is the statement immediately after the `JsRuntime::new` that used to evaluate them.
//
// So this crate now defines the whole Web API surface WITHOUT SHIPPING A LINE OF JAVASCRIPT, which is
// what invariants I2 and I3 mean in practice: one installer walking tables, and no JS that defines a
// member, masks a body or holds an order.

/// The local-scheme answers the runtime's `fetch` needs: a `blob:` URL's type and bytes, and a decoded `data:` URL.
pub use crate::modules::url::url::{blob_bytes_handle, blob_url_bytes, decode_data_url, shared_blob_bytes, BlobBytes};

/// `eval(TrustedScript)` -- the code-generation callback, registered once per isolate.
pub use crate::core::trusted_types::tables::{compliant_script, install_codegen_hook, string_compilation_allowed};
pub use crate::modules::structured_clone::refuse_platform_objects;
