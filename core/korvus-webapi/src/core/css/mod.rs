//! `css` Web-API interfaces (blueprint domain). One file per interface,
//! extracted verbatim from bindings.rs; the struct + ops are re-exported so the `extension!`
//! lists in bindings.rs keep referring to them unqualified (as `core::geometry` does).

/// The bridge from the CSSOM interfaces to `korvus-page`'s parsed rules and its cascade -- the ops
/// `core/korvus-runtime/src/cssom.rs` used to own.
pub(crate) mod cssom;
pub(crate) mod background; // `background-image: url(...)`, fetched and tiled over its box.
/// `getComputedStyle`'s shorthands, serialized from their longhands as Blink writes them.
pub(crate) mod computed_shorthand;
/// `MediaQueryList` and `window.matchMedia`, replacing `host.js`'s `__wireMQL` block
/// -- five of the install ledger's seventeen second writers.
pub(crate) mod media_query_list;
/// `CSSRule` and the three rule interfaces this engine's parser produces.
pub(crate) mod css_rule;
pub(crate) mod css_style_declaration;
pub(crate) mod css_supports;
/// The loads a stylesheet induces: `@import`, `url()` images on rendered elements, `@font-face`
/// sources in use.
pub(crate) mod sheet_loads;
/// `StyleSheet`, `CSSStyleSheet` and `MediaList`.
pub(crate) mod style_sheet;
/// The CSS Custom Highlight API -- `Highlight`, `HighlightRegistry` and the `CSS.highlights`
/// singleton over them.
pub(crate) mod highlight;
/// `StylePropertyMapReadOnly`, `CSSStyleValue` and `Element.computedStyleMap()`.
pub(crate) mod style_property_map;
pub(crate) mod typed_om;
/// `ViewTransition`, `ViewTransitionTypeSet` and `Element.startViewTransition()`.
pub(crate) mod view_transition;
