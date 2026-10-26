//! Live, at-context-creation wiring of the hand-written tables
//! -- ONE FILE PER DOMAIN, and this module is the index.
//!
//! Every table in the tree is mounted from here, by [`super::installer`]: a realm's context creation calls
//! the `install_window_*` / `install_worker_*` function of each domain below, each of which walks its
//! interfaces' `interface!` rows through the ONE member mount ([`rows`]) and into the context's
//! install ledger. There is no JS glue and so no `[native code]` masking or bridge slot.
//!
//! WHERE A DOMAIN'S INSTALL RUNS IS PART OF THE DOMAIN, and each function's own doc comment states
//! it. Five of them have a position that was recorded after a defect was MEASURED, and the doc
//! comment is the record: [`media`] runs from [`bootstrap`] right after the registry install; [`small_elements`] and [`abort_style`] run BEFORE the bootstrap script,
//! so the kernel's CSSOM decorator could wrap the table's own row -- W4.f RETIRED that decorator, so
//! that half of the reason is gone and only the position remains; [`foreign`] and [`postshells`] are
//! the first statements of the runtime's pre-bootstrap block, which is the slot the webapi
//! extension's last two ESM bodies held; [`window`] ran pre-bootstrap because `BOOTSTRAP` assigned
//! `globalThis.print` over the table's `print` no-op, which silenced the engine -- W4.f made the sink
//! the table's own `print` row, so that reason is gone too and the position is merely unchanged.
//! Moving one is a measurement, not a tidy-up.
//!
//! The files were one 2,003-line `live.rs` whose functions were named for the LANE that wrote them
//! (`install_window_l11b`, `l3`, `l12`, `l9`); each was renamed for its domain and gave it a
//! file, so a lane touches one file and the per-lane merge conflict disappears.

/// Abort, style, blob, fetch and notification.
pub(crate) mod abort_style;
/// THE WEBAPI EXTENSION'S ESM LAYER, IN RUST -- the bootstrap sequence `src/js/` used to be.
pub(crate) mod bootstrap;
/// The Web Animations family.
pub(crate) mod animations;
/// The Web Audio family.
pub(crate) mod audio;
/// `FeaturePolicy`, the interface `document.featurePolicy` hands back.
pub(crate) mod feature_policy;
/// The 2D canvas rendering context and the objects it hands back.
pub(crate) mod canvas;
/// The platform LIST interfaces (`HTMLCollection`, `NodeList`, `DOMTokenList`, …).
pub(crate) mod collections;
/// Crypto, messaging, cookies, `FileReader` and `XMLHttpRequest`.
pub(crate) mod crypto_xhr;
/// The `DataTransfer` family and `FileList`.
pub(crate) mod datatransfer;
/// The five elements whose missing members look at OTHER elements (`<area>`, `<canvas>`, `<label>`,
/// `<map>`, `<legend>`).
pub(crate) mod element_refs;
/// `Event` and the fifteen interfaces that inherit from it, plus `Touch` and `MessageChannel`.
/// `HTMLBodyElement`.
pub(crate) mod body_element;
/// The `CSS` namespace object.
pub(crate) mod css;
/// The CSSOM: style sheets, media lists and rules.
pub(crate) mod cssom;
/// The five element interfaces whose members the registry's kit rows did not carry.
pub(crate) mod per_tag_elements;
/// `HTMLScriptElement`.
pub(crate) mod script_element;
/// Web Storage: `Storage` and the two area objects.
pub(crate) mod storage;
pub(crate) mod event_family;
/// The forms family (`<form>`, `<output>`, `<fieldset>`, `<meter>`).
pub(crate) mod forms;
/// The cppgc value families `deno_core` realises natively: `DOMRect`, `DOMPoint` and `DOMQuad`.
pub(crate) mod geometry;
/// The two Fetch cppgc value classes: `Headers` and `FormData`, in BOTH realms.
pub(crate) mod fetch_values;
/// `HTMLImageElement` and the legacy `Image` factory.
pub(crate) mod html_image;
/// The twelve IndexedDB interfaces and the `indexedDB` global.
pub(crate) mod indexeddb;
/// The media ELEMENT family (`<video>`, `<audio>`, `<track>`, `<source>`, `<option>`, text tracks).
pub(crate) mod media;
/// Real-time media: WebRTC, `MediaStream`, WebCodecs, MSE and the CSS Font Loading pair.
pub(crate) mod media_rtc;
/// Mutation observation, the custom-element registry and `<dialog>`.
pub(crate) mod mutation_custom_elements;
/// `Navigator` / `WorkerNavigator` -- where the one table mounts in each realm.
pub(crate) mod navigator;
/// The Performance / Intersection / Resize observers.
pub(crate) mod observers;
/// The performance TIMELINE: `Performance`, the entry family, Navigation Timing L1, `EventCounts`.
pub(crate) mod performance;
/// Everything `js/dom/09-postshells.js` did after the shell pass, and the two finalize runs.
pub(crate) mod postshells;
/// `Range`, `Selection`, and the receiver check their JS-class-backed rows carry.
pub(crate) mod ranges;
/// The row-mount helpers every domain shares.
pub(crate) mod rows;
/// The eleven small element interfaces.
pub(crate) mod small_elements;
/// The six WHATWG Streams interfaces.
pub(crate) mod streams;
/// The six `<table>` element interfaces.
pub(crate) mod tables;
/// `URL` and `URLSearchParams`.
pub(crate) mod url;
/// The `Window` / `Document` surface and its singletons.
pub(crate) mod window;
/// The worker family: `Worker` and its four receive pumps.
pub(crate) mod workers;

/// The hand tables (and hand-minted globals) the registry walk creates at their capture position --
/// see [`crate::install::installer::OrderedHand`] for why the position is the walk's.
pub(crate) static ORDERED_HAND: &[crate::install::installer::OrderedHand] = &[
    // Four Navigation API interfaces left the registry for their tables
    // (`core/html/navigate_event.rs`), created where their rows were.
    crate::install::installer::OrderedHand {
        name: "NavigationTransition",
        realm: crate::install::Exposed::Window,
        create: crate::core::html::navigate_event::create_navigation_transition,
    },
    crate::install::installer::OrderedHand {
        name: "NavigationPrecommitController",
        realm: crate::install::Exposed::Window,
        create: crate::core::html::navigate_event::create_navigation_precommit_controller,
    },
    crate::install::installer::OrderedHand {
        name: "NavigationDestination",
        realm: crate::install::Exposed::Window,
        create: crate::core::html::navigate_event::create_navigation_destination,
    },
    crate::install::installer::OrderedHand {
        name: "NavigateEvent",
        realm: crate::install::Exposed::Window,
        create: crate::core::html::navigate_event::create_navigate_event,
    },
    // Two stream tables that took over their generated rows keep those rows' positions
    // (created with the streams family, each realm's order ratchet rose by exactly 2).
    crate::install::installer::OrderedHand {
        name: "TextEncoderStream",
        realm: crate::install::Exposed::Both,
        create: crate::install::live::streams::build_text_encoder_stream,
    },
    crate::install::installer::OrderedHand {
        name: "TextDecoderStream",
        realm: crate::install::Exposed::Both,
        create: crate::install::live::streams::build_text_decoder_stream,
    },
    // The two compression tables took over their generated rows, so the walk creates
    // them here instead -- at the SAME capture position the rows had, in both realms, which is what
    // keeps each realm's own-global order unmoved.
    crate::install::installer::OrderedHand {
        name: "CompressionStream",
        realm: crate::install::Exposed::Both,
        create: crate::install::live::streams::build_compression_stream,
    },
    crate::install::installer::OrderedHand {
        name: "DecompressionStream",
        realm: crate::install::Exposed::Both,
        create: crate::install::live::streams::build_decompression_stream,
    },
    crate::install::installer::OrderedHand {
        name: "createImageBitmap",
        realm: crate::install::Exposed::Window,
        create: crate::core::html::canvas_image::create_window_create_image_bitmap,
    },
    crate::install::installer::OrderedHand {
        name: "NetworkInformation",
        realm: crate::install::Exposed::Both,
        create: crate::core::navigator::network_information::create_network_information,
    },
    // `BatteryManager` left the registry for its table (`core/navigator/battery_manager.rs`).
    crate::install::installer::OrderedHand {
        name: "BatteryManager",
        realm: crate::install::Exposed::Window,
        create: crate::core::navigator::battery_manager::create_battery_manager,
    },
    crate::install::installer::OrderedHand {
        name: "XPathResult",
        realm: crate::install::Exposed::Window,
        create: crate::core::xml::xpath::create_xpath_result,
    },
    crate::install::installer::OrderedHand {
        name: "XPathExpression",
        realm: crate::install::Exposed::Window,
        create: crate::core::xml::xpath::create_xpath_expression,
    },
    crate::install::installer::OrderedHand {
        name: "XPathEvaluator",
        realm: crate::install::Exposed::Window,
        create: crate::core::xml::xpath::create_xpath_evaluator,
    },
    // `PerformanceResourceTiming` left the registry for its table; created where its row
    // was, so neither realm's own-global order moves (created later, each ratchet rose by one).
    crate::install::installer::OrderedHand {
        name: "PerformanceResourceTiming",
        realm: crate::install::Exposed::Both,
        create: crate::core::timing::resource_timing::create_performance_resource_timing,
    },
    // `PerformanceServerTiming` left the registry for its table (`core/timing/server_timing.rs`).
    crate::install::installer::OrderedHand {
        name: "PerformanceServerTiming",
        realm: crate::install::Exposed::Both,
        create: crate::core::timing::server_timing::create_performance_server_timing,
    },
    // The two same-document navigation events left the registry for hand tables with
    // real constructors; created where their generated rows were.
    crate::install::installer::OrderedHand {
        name: "PopStateEvent",
        realm: crate::install::Exposed::Window,
        create: crate::core::events::navigation_events::create_pop_state_event,
    },
    crate::install::installer::OrderedHand {
        name: "HashChangeEvent",
        realm: crate::install::Exposed::Window,
        create: crate::core::events::navigation_events::create_hash_change_event,
    },
    // `RTCPeerConnectionIceEvent` left the registry for its table (`core/webrtc/ice_event.rs`).
    crate::install::installer::OrderedHand {
        name: "RTCPeerConnectionIceEvent",
        realm: crate::install::Exposed::Window,
        create: crate::core::webrtc::ice_event::create_rtc_peer_connection_ice_event,
    },
    crate::install::installer::OrderedHand {
        name: "caches",
        realm: crate::install::Exposed::Window,
        create: crate::modules::cache_storage::cache::create_window_caches,
    },
];

// ── THE INDEX ───────────────────────────────────────────────────────────────────────────────────
//
// One path per name, so `install::live::X` means what it always meant and a caller never has to know
// which file X lives in. The modules above are `pub(crate)`; this is their public face.

pub use abort_style::{install_window_abort_style, install_worker_abort_style};
pub use bootstrap::{install_window_bootstrap, install_worker_bootstrap};
pub use animations::install_window_animations;
pub use audio::install_window_audio;
pub use feature_policy::install_window_feature_policy;
/// The five Trusted Types interfaces in the WORKER realm, from the SAME tables the
/// window uses (invariant I8). The tables take all five out of the generated registry in both
/// realms, so without this call a worker would lose them outright.
pub use crate::core::trusted_types::tables::install_worker_trusted_types;
/// The `Scheduler` interface in the WORKER realm, from the SAME table the window uses.
pub use crate::core::html::scheduler::install_worker_scheduler;
pub use canvas::{
    install_window_canvas, install_window_webgl, install_window_webgpu, install_worker_canvas,
    install_worker_webgl, install_worker_webgpu, wire_worker_canvas_oracle,
};
pub use collections::{install_window_collections, install_worker_collections};
pub use crypto_xhr::{install_window_crypto_xhr, install_worker_crypto_xhr};
pub use datatransfer::{install_window_datatransfer, install_worker_datatransfer};
pub use element_refs::install_window_element_refs;
pub use body_element::install_window_body_element;
pub use css::install_window_css;
pub use cssom::install_window_cssom;
pub use crate::modules::wasm_streaming::install_wasm_streaming_rows;
pub use per_tag_elements::install_window_per_tag_elements;
pub use script_element::install_window_script_element;
pub use storage::install_window_storage;
pub use event_family::{install_window_event_family_now, install_worker_event_family};
pub use forms::install_window_forms;
pub use fetch_values::install_fetch_values;
pub use html_image::install_window_html_image_element;
pub use indexeddb::{install_window_indexeddb, install_worker_indexeddb};
pub use media::{install_window_media, install_worker_media};
pub use media_rtc::{install_window_media_rtc, install_worker_media_rtc};
pub use mutation_custom_elements::install_window_mutation_custom_elements;
pub use navigator::{install_window_navigator, install_worker_navigator, NAVIGATOR};
pub use observers::{install_window_observers, install_worker_observers};
pub use performance::{install_window_performance, install_worker_performance};
pub use postshells::{finalize_surface, install_window_postshells};
pub use ranges::install_window_ranges;
pub use rows::{parent_of, strip_own};
pub use small_elements::{install_window_small_elements, install_worker_small_elements};
pub use streams::{install_window_streams, install_worker_streams};
pub use tables::install_window_tables;
pub use url::{install_window_url, install_worker_url};
pub use workers::{install_window_service_workers, install_window_workers, install_worker_service_workers, install_worker_workers};
pub use window::{
    apply_secure_context_exposure,
    install_window_chrome, install_window_extras, install_window_globals, install_window_legacy_factories,
    install_window_named_properties,
    install_window_plugins, install_window_speech_synthesis, install_window_window,
    set_window_document, set_window_location, window_location_minted, define_window_getter_singleton, fire_window_event, make_empty_dom_string_list,
};

// The row mounts an interface FILE names to install its own members, and the two brand-checked
// variants `core/dom/range.rs` and `core/editing/selection.rs` name.
pub(crate) use ranges::{branded_getter_row, branded_method_row, Brand};
pub(crate) use rows::{getter_row, method_row, mint_table, pair_row, set_table_proto};

