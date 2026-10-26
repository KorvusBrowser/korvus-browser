//! `html` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod html_button_element;
pub(crate) mod html_input_element;
/// The forms family, replacing `dom/08w-form-element.js`,
/// `dom/08y-output-fieldset.js` and `dom/08m-meter-element.js`: `HTMLFormElement`,
/// `HTMLOutputElement`, `HTMLFieldSetElement` and `HTMLMeterElement`.
pub(crate) mod html_form_element;
pub(crate) mod html_meter_element;
pub(crate) mod html_output_element;
pub(crate) mod html_script_element;
/// Import maps: `<script type="importmap">`'s parse + HTML's "resolve a module
/// specifier". Pure logic over `serde_json` and `url` -- no V8, no DOM -- so the resolution rules
/// are testable without a realm and without a browser.
pub(crate) mod import_map;
pub(crate) mod custom_element_registry;
pub(crate) mod html_dialog_element;
pub(crate) mod html_marquee_element;
pub(crate) mod html_canvas_element;
pub(crate) mod canvas_2d;
/// `CanvasImageSource`: the pixels behind an image argument, `ImageBitmap`, `createImageBitmap`, tainting.
pub(crate) mod canvas_image;
/// `ImageData` -- the canvas pixel buffer, as a table with a native constructor.
pub(crate) mod image_data;
pub(crate) mod pixel_format;
pub(crate) mod webaudio;
pub(crate) mod webgl;
pub(crate) mod webgpu;
pub(crate) mod presentation;
pub(crate) mod web_locks;
pub(crate) mod edit_context;
pub(crate) mod html_body_element;
pub(crate) mod html_element;
pub(crate) mod element_internals;
pub(crate) mod html_model_element;
pub(crate) mod html_image_element;
/// The six `<table>` interfaces as six `interface!` tables -- the tree algorithms
/// `dom/08s-table-element.js` and `dom/08x-table-parts.js` carried, in Rust (W5.1 L2).
pub(crate) mod html_table_elements;
/// The seven small element interfaces that had no Rust file of their own:
/// `<object>`/`<embed>`, `<link>`, `<iframe>`/`<frame>`, `<textarea>` and `<a>`.
pub(crate) mod html_anchor_element;
/// The elements replacing `dom/09c-small-elements.js` and the `<area>` half
/// of `dom/08c-collections.js`'s hyperlink loop. What they have in common is that their missing
/// members LOOK AT OTHER ELEMENTS -- a map's areas, a legend's form, a label's control -- which is
/// why plain attribute reflection never reached them. (`HTMLCanvasElement`'s table is the fifth, and
/// lives in `html_canvas_element.rs` with the rest of the canvas.)
pub(crate) mod html_area_element;
pub(crate) mod html_label_element;
pub(crate) mod html_map_element;
pub(crate) mod html_iframe_element;
pub(crate) mod html_link_element;
pub(crate) mod html_object_element;
pub(crate) mod html_text_area_element;
/// `HTMLTemplateElement` -- the interface `dom/09-postshells.js` existed to install after the shell
/// pass, as one `interface!` table.
pub(crate) mod html_template_element;
/// The media family, replacing `dom/08z-media-element.js`,
/// `dom/09a-media-parts.js` and `dom/00c-media-codecs.js`: the three media ELEMENT interfaces, the
/// two media-adjacent ones, `<option>`, and the seven non-element objects a media element hands out.
pub(crate) mod html_media_element;
pub(crate) mod html_media_parts;
/// A media element's load: the load and resource selection algorithms, the fetch, the readiness and
/// playback states.
pub(crate) mod media_load;
/// What a media resource's header says: container, duration, frame size.
pub mod media_probe;
pub(crate) mod html_option_element;
pub(crate) mod media_capabilities;
pub(crate) mod media_objects;
pub(crate) mod text_tracks;
pub(crate) mod html_portal_element;
pub(crate) mod form_encoding;
pub(crate) mod hyperlink;
/// The `<select>` bodies that are Rust today (`selectedIndex`); the table is W5.1's.
pub(crate) mod html_select_element;
pub(crate) mod html_slot_element;
pub(crate) mod html_style_element;
pub(crate) mod html_title_element;
/// COOP/COEP: the RFC 8941 parser, the delivered-policy resolution, the `SharedArrayBuffer` gate.
pub(crate) mod isolation_policy;
/// Nested and auxiliary browsing contexts -- the single-context emulation `dom/03-element-glue.js`
/// carried, in Rust (W5.0d).
pub(crate) mod activation;
pub(crate) mod browsing_context;
/// A DevTools client's isolated worlds over the page's document.
pub(crate) mod isolated_world;
/// `devicemotion` / `deviceorientation` / `deviceorientationabsolute` on a device with no such sensor: the one
/// null event a listener's addition starts.
pub(crate) mod device_sensors;
/// A fetched classic script compiled off the page's thread (V8's script streaming, as Blink's `ScriptStreamer`).
pub(crate) mod script_stream;
/// Element-induced subresource loads -- which element/attribute induces a request, with what
/// destination and mode, and the `load`/`error` that follows (W5.0e; was dom/09j-subresource.js).
pub(crate) mod subresource;
/// Which image an `<img>`/`<picture>` requests (`srcset`, `sizes`, the persona's DPR).
pub(crate) mod srcset;
/// `loading=lazy` -- deferred until near the viewport.
pub(crate) mod lazy_load;
/// The load priority Blink gives a request, and the `priority` field it carries.
pub(crate) mod load_priority;
/// The document's Content-Security-Policy, as far as the icon downloads need it.
pub(crate) mod csp;
// What a navigation to a non-HTML, non-XML response becomes -- the page's AND a frame's (moved from korvus-runtime, A/B C5).
pub mod text_viewer;
// A document's referrer policy and the one each of its requests carries.
pub(crate) mod referrer_policy;
// A cross-site frame in its own engine on its own thread.
pub mod remote_frame;
pub mod popup; // A popup as a page target of its own, under the browser endpoint (D74 step 3).
pub mod dialog; // `alert`, `confirm` and `prompt` asked of a person through the endpoint (D88 step 2).
/// The favicon and the manifest icon, requested for the page after `load`.
pub(crate) mod link_icons;
pub(crate) mod plugins;
pub(crate) mod legacy_factories;
pub(crate) mod window;
/// The window SELF-ALIASES and the `navigator`/`screen`/`location` singletons, off the
/// kernel -- and the proof that the deno_core `objects =` classes are reachable from Rust.
/// `requestAnimationFrame` / `cancelAnimationFrame` / `requestIdleCallback` /
/// `cancelIdleCallback` -- the `nat(...)` block of `host.js`.
pub(crate) mod animation_frame;
pub(crate) mod window_globals;
/// The `Window`'s thirteen geometry attributes and three scroll methods -- the last
/// big `Object.defineProperty(globalThis, …)` block of `host.js`.
pub(crate) mod window_geometry;
/// The `ScreenOrientation` `screen.orientation` hands back, carrying the worn
/// bundle's captured `type` and `angle`.
pub(crate) mod screen_orientation;
/// `Screen` -- an own class, moved from the kernel.
pub mod screen;
/// The window's REMAINING own globals -- the 43 names the worn capture records and no
/// install created, from `external` and `trustedTypes` to the six `BarProp`s and the webkit aliases.
pub(crate) mod window_extras;
/// WebGPU's five constant NAMESPACES, the only non-interface members of that surface.
pub(crate) mod gpu_namespaces;
/// `BarProp` and `Viewport` -- the two one-member interfaces behind seven window globals.
pub(crate) mod bar_props;
/// `VisualViewport` and the `visualViewport` singleton -- an interface object that was
/// on the global with no instance behind it, which is a shape no browser has.
pub(crate) mod visual_viewport;
/// `CrashReportContext` and the `crashReport` singleton -- a real byte budget, so
/// `set()` answers the device's InvalidStateError, NotAllowedError or nothing at all.
pub(crate) mod crash_report;
/// `DocumentPictureInPicture` and its singleton, whose `.window` must be `null` rather
/// than a stub's `undefined`.
pub(crate) mod document_pip;
/// `Scheduler` and the `scheduler` singleton -- a real priority queue on this realm's
/// own macrotask hook, so `postTask` RUNS the task and resolves with its value.
pub(crate) mod scheduler;
/// A host load-pipeline step run as a task of the event loop under any driver.
pub(crate) mod task_queue;
pub(crate) mod navigation_api;
/// The Navigation API's events and the promises of its navigations.
pub(crate) mod navigate_event;
// The session history `History` and the Navigation API both read.
pub(crate) mod session_history;
/// A name's EXPOSURE CONDITION -- `[SecureContext]`, which is a platform fact and
/// decides whether a global exists at all on this document's origin.
pub(crate) mod exposure;
pub use browsing_context::{seal_frame_surface, BrowsingContexts};
pub use isolation_policy::IsolationGate;

/// The interfaces this tree DEFINES and this bundle does not EXPOSE — the selector model, stated
/// where a test can read it (docs/ONE-TABLE-ARCHITECTURE.md D3/D4).
///
/// Each is a complete `interface!` table whose implementation is correct for a browser that ships it,
/// and which `brave-151-linux-20260907T1006` does not record among its globals. Publishing one would
/// be an extra global no Brave has — a structural diff, which fingerprints worse than the absence it
/// would fill. Before this lane the decision was "the `.js` file is missing from `dom.js`'s import
/// list", which a reader could not tell from a forgotten import; it is "no `install/live/*.rs` names
/// this static" now, and `tests/exclusion.rs` asserts both halves of it — the name is on neither
/// captured surface, and nothing installs it.
pub static UNEXPOSED_BY_THIS_BUNDLE: &[&crate::install::InterfaceDef] =
    &[&html_model_element::HTMLMODELELEMENT, &html_portal_element::HTMLPORTALELEMENT];
