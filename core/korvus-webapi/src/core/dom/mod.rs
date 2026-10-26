//! `dom` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

pub(crate) mod mutation_observer;
pub(crate) mod node_iterator;
pub(crate) use node_iterator::NodeIterator;
pub(crate) mod tree_walker;
pub(crate) use tree_walker::TreeWalker;
pub(crate) mod character_data;
pub(crate) mod node;
pub(crate) mod document_fragment;
pub(crate) mod document_type;
pub(crate) mod spine;
/// The one predicate that says which interfaces the DOM spine creates with an EMPTY selector -- public
/// because `tests/generated_registry.rs` gates it against the registry's `backfill` column, and the two
/// are a pair whose failure mode is a silently bare prototype.
#[allow(unused_imports)] // read by tests/generated_registry.rs, which the lib itself does not call.
pub use spine::is_html_element_shell;
pub(crate) use node::notify_childlist_batch;
pub(crate) mod element;
/// W5.1 L16b: the SVG animated-attribute value types (`SVGAnimatedLength`, `SVGLength`, `SVGRect`,
/// the list types...), as the bodies the registry rows name through `install::kits::svg_animated` /
/// `svg_value` / `svg_create_value`. Replaces dom/09k-svg.js's `KB.__svgAnimated` builders.
pub(crate) mod svg_values;
pub(crate) mod svg_geometry;
pub(crate) mod svg_text;
/// The codec-support GRAMMAR over that data -- `canPlayType`,
/// `MediaSource.isTypeSupported` and `MediaRecorder.isTypeSupported` answer from one place. Replaces
/// `dom/00c-media-codecs.js` and its three `KB.__codec*` bridge slots.
pub(crate) mod media_codecs;
pub(crate) use element::{JsElement, ScrollState, element_display_name};
pub use document::{document_base_url, set_document_url, CookieUrl, DocEncoding, DocumentUrl, PageContentType};
pub(crate) use document::{base_url, base_url_of, document_url_of, BaseUrlCache};
pub(crate) mod window_named;
/// The layout-metric bodies (`getBoundingClientRect`, `getClientRects`, `client*`, `offset*`) over the
/// page pipeline, the layout cache, and the hit-testing ops (W5.0d: moved out of the runtime).
pub(crate) mod geometry_queries;
pub use geometry_queries::{document_layouts, layout_counts, media_context, refresh_layout_cache, LayoutCache, NaturalImages};
pub(crate) mod document;
/// `DOMException` -- the one table for both realms, replacing the `host.js` class and the
/// different `worker-host.js` one. Its `error_builder_names` is the list the runtime registers with
/// `deno_core`'s error builder, so an op's `JsErrorBox` becomes a real DOMException.
pub mod dom_exception;
pub(crate) mod range;
pub(crate) mod dom_token_list;
/// The ten platform LIST interfaces (HTMLCollection, NodeList, DOMTokenList, NamedNodeMap, ...) as
/// native exotic objects with indexed/named interceptors, and their `interface!` tables.
pub(crate) mod collections;
pub(crate) mod text;
pub(crate) use text::notify_childlist;
pub(crate) mod shared;
/// The one WebIDL maplike/setlike iterator kind (`"<Iface> Iterator"`), shared by every table that
/// declares one -- the Highlight, HighlightRegistry, ViewTransitionTypeSet,
/// StylePropertyMapReadOnly and SVGStringList.
pub(crate) mod webidl_iter;
/// The seven ARIA ELEMENT-reflection rows of the `Element` table.
pub(crate) mod aria_reflection;
/// `SVGStringList` and the two `[SameObject]` content-attribute views that hand one out
/// (`requiredExtensions`, `systemLanguage`).
pub(crate) mod svg_string_list;
pub(crate) mod attr;
// The Attr/NamedNodeMap accessor CALLBACKS live in these two files and are named by their own
// `interface!` rows. The `op_dtl_like!` macro in shared.rs that wrapped six of them in JS-facing ops
// is deleted -- nothing called the ops.
pub(crate) mod named_node_map;
pub(crate) mod abort_controller;
pub(crate) mod abstract_range;
/// `host.js`'s `class StaticRange extends globalThis.AbstractRange` -- W4.e's last remains.
pub(crate) mod static_range;
