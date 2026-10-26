//! `binding/host_dom.rs` — the DOM engine infra structs, moved VERBATIM from `bindings.rs` (file-move
//! STEP: the binding tier). Pure code move, no logic change. These are the `OpState`-held host state
//! behind the native DOM surface: the arena itself ([`HostDom`]), wrapper interning ([`NodeCache`]),
//! prototype retyping ([`NodeProtos`]/[`TagProtos`]/[`ShadowRootProto`]), parsed qualified names
//! ([`QNameMap`]). The cached dataset ObjectTemplate that was here went when the builder gave
//! `DOMStringMap` its interceptor.
//!
//! An element's NAMESPACE used to live here too, in an `NsMap` keyed by node id. It does not any
//! more: it is a field on the arena node (`korvus_dom::ElementNs`), which is the only place that can
//! answer for a node the HTML parser built and the only place a `cloneNode` carries it from.
//!
//! Only the visibility changed on move: structs that were private to `bindings` are `pub(crate)`
//! (fields too), so `bindings` and the moved domain files keep reaching them through `crate::binding`.

use deno_core::v8;
use korvus_dom::Dom;

/// The active document arena, held in `OpState`. Default is an empty document.
pub struct HostDom(pub Dom);

impl Default for HostDom {
    fn default() -> Self {
        HostDom(Dom::empty())
    }
}

impl HostDom {
    /// Parse `xml` into a detached Document subtree, then drop the prolog whitespace the raw parser
    /// keeps -- the DOM-level shape of an XML document, applied where the arena hands its tree over.
    ///
    /// A Document node's children are, per DOM, at most one Element plus DocumentTypes, Comments and
    /// ProcessingInstructions: a Text child is not a thing a Document can have. `Dom::parse_xml_into`
    /// is a parser and keeps the newline-and-indent between `<?xml version="1.0"?>` and the root
    /// element as a text node, so `doc.firstChild` answered that text node instead of the root
    /// element and `doc.childNodes.length` was one too many.
    ///
    /// Only whitespace-only text children are removed. Character data at the document level is a
    /// well-formedness error rather than something to silently discard, and leaving it visible keeps
    /// the parser's own error reporting honest.
    pub(crate) fn parse_xml_document(&mut self, xml: &str) -> korvus_dom::NodeId {
        let root = self.0.parse_xml_into(xml);
        for child in self.0.children(root) {
            if self.0.node_type(child) == 3
                && self.0.node_value(child).is_none_or(|v| v.trim().is_empty())
            {
                self.0.remove_child(root, child);
            }
        }
        root
    }
}

/// Native node-wrapper intern cache: one canonical v8 wrapper per NodeId, so `===` identity holds
/// across queries -- the native-DOM replacement for the bootstrap's JS wrap Map.
/// `binding::protos::wrap_cached` is the keystone the native-first node-returning members build on
/// (they return interned wrappers instead of ids). Interior-mutable so a reader can take `&OpState`
/// alongside `scope`. There used to be an `op_wrap_node` door onto the same cache for JS callers;
/// It was deleted, unreferenced.
///
/// Indexed by the NodeId itself (arena ids are dense): a node-returning getter is one bounds-checked
/// load, where the `HashMap` it replaced hashed every id with SipHash (~23 % of `firstChild`
/// was this lookup, measured 2026-09-29). The wrapper is read in place, never cloned.
#[derive(Default)]
pub(crate) struct NodeCache(std::cell::RefCell<Vec<Option<v8::Global<v8::Object>>>>);

impl NodeCache {
    /// Whether no node has a wrapper (none may exist where a snapshot of the realm is made).
    #[cfg(not(feature = "analysis"))]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.borrow().iter().all(Option::is_none)
    }

    /// Node `id`'s wrapper, if it has one.
    pub(crate) fn local<'s>(&self, scope: &mut v8::PinScope<'s, '_>, id: u32) -> Option<v8::Local<'s, v8::Object>> {
        self.0.borrow().get(id as usize).and_then(Option::as_ref).map(|g| v8::Local::new(scope, g))
    }

    /// Record `wrapper` as node `id`'s canonical wrapper (replacing any earlier one).
    pub(crate) fn insert(&self, id: u32, wrapper: v8::Global<v8::Object>) {
        let mut v = self.0.borrow_mut();
        if v.len() <= id as usize {
            v.resize_with(id as usize + 1, || None);
        }
        v[id as usize] = Some(wrapper);
    }

    /// Forget every wrapper: a new document replaces the arena's ids.
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// The page `document` object, held as a `Global` after the first lookup.
///
/// `global_get(scope, "document")` is not a cheap property read on this window: the global carries
/// the named-property machinery (js/dom/09h-window-named.js), so each lookup allocates a v8 string
/// and walks that path. `Node.parentNode` has to answer `document` for the document's own children,
/// and tree walks call it constantly -- reading it through `global_get` there measured a 10x
/// slowdown on `dom/ranges/Range-set.html` (453ms -> 5.3s, past the runner's per-file deadline).
#[derive(Default)]
pub(crate) struct DocumentGlobal(pub(crate) std::cell::RefCell<Option<v8::Global<v8::Value>>>);

/// Return the canonical `Element` wrapper for `id`, creating+caching it on first use. Native
/// interning: `make_cppgc_object` builds the cppgc wrapper (Element/HTMLElement prototype); the JS
/// side retypes text/comment/etc. once (the wrapper is cached, so the retype sticks).
/// Create-or-return the canonical wrapper for `id` (native intern primitive), inside a raw callback.
/// Per-nodeType prototype registry (nodeType -> JS prototype), populated once from JS, so native
/// wrappers get the right chain (Text.prototype for text nodes, etc.). Elements (type 1) keep the
/// default JsElement.prototype (= HTMLElement.prototype); only non-element types need an entry.
#[derive(Default)]
pub(crate) struct NodeProtos(pub(crate) std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::Object>>>);

/// The `ShadowRoot.prototype` object, registered once from generated_shells.js after the shell is
/// created (Korvus GS ②-A3). A shadow-root arena node reuses `NodeKind::Document` (nodeType 9), which
/// it shares with detached document roots, so it cannot be keyed by nodeType in [`NodeProtos`]; a
/// single slot plus a `Dom::is_shadow_root` check in [`node_proto`] distinguishes it. With this
/// registered, every path that wraps a shadow-root NodeId through `wrap_cached` (attachShadow, the
/// `shadowRoot` getter, `getRootNode()` on shadow content) yields a ShadowRoot-typed wrapper with
/// canonical NodeCache identity.
#[derive(Default)]
pub(crate) struct ShadowRootProto(pub(crate) std::cell::RefCell<Option<v8::Global<v8::Object>>>);

/// `HTMLUnknownElement.prototype`, for the tag-proto MISS path.
///
/// HTML's rule, and Brave 151 follows it exactly (recorded on this machine): a tag with no interface
/// of its own is `HTMLUnknownElement` -- UNLESS its name is a valid custom element name (it contains a
/// hyphen), which is `HTMLElement`. Korvus answered `HTMLElement` for every unknown tag, so
/// `document.createElement("zzz").constructor.name` differed from the reference browser on a one-line
/// probe, and so did every `<blink>`, `<isindex>` and unrecognised tag a real page contains.
pub(crate) struct UnknownElementProto(pub(crate) std::cell::RefCell<Option<v8::Global<v8::Object>>>);
impl Default for UnknownElementProto {
    fn default() -> Self {
        Self(std::cell::RefCell::new(None))
    }
}

/// Per-tag element prototypes: lowercase tag name -> HTML*Element.prototype (HTMLFormElement, ...).
/// Elements (nodeType 1) select their prototype by tag; unregistered tags fall back to HTMLElement.
#[derive(Default)]
pub(crate) struct TagProtos(pub(crate) std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Object>>>);

/// Per-tag element prototypes for the SVG namespace: the EXACT (case-sensitive) local name ->
/// SVG*Element.prototype. Separate from [`TagProtos`] because the two namespaces disagree about both
/// case and meaning: SVG local names are case-sensitive (`clipPath`, `feBlend`) where HTML tags are
/// folded to lowercase, and five names -- `a`, `script`, `style`, `title`, `text` -- name a different
/// interface in each namespace. Keying one map by lowercase tag alone cannot answer both, which is why
/// `createElementNS(svgNS, "rect")` used to come back `HTMLUnknownElement`: `rect` is in neither the
/// HTML tag map nor a custom-element name, so it fell to the MISS path.
///
/// The map is the SVG half of Blink's element factory (`core/svg/svg_tag_names.json5`), and its MISS
/// path is that file's own `fallbackJSInterfaceName`: an SVG-namespaced tag with no interface of its
/// own is `SVGElement`, not `HTMLUnknownElement` (recorded: `createElementNS(svgNS,"animateColor")`
/// is an SVGElement in Brave 151, and that tag is one Blink maps to the fallback explicitly).
#[derive(Default)]
pub(crate) struct SvgTagProtos(pub(crate) std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Object>>>);

/// `SVGElement.prototype`, the SVG-namespace MISS path (see [`SvgTagProtos`]).
#[derive(Default)]
pub(crate) struct SvgElementProto(pub(crate) std::cell::RefCell<Option<v8::Global<v8::Object>>>);

/// `Element.prototype`: an element in NO namespace or in one that is not HTML, SVG or MathML
/// (`createElementNS("http://example.com/x", "foo")`, `createElementNS(null, "foo")`, an XML document's
/// elements) -- Chrome 150 answers `Element`, never `HTMLUnknownElement`.
#[derive(Default)]
pub(crate) struct ElementProto(pub(crate) std::cell::RefCell<Option<v8::Global<v8::Object>>>);

/// Per-tag element prototypes for the MathML namespace, the same shape as [`SvgTagProtos`] and for the
/// same reason: `a` is declared in MathML too, and a MathML `<mi>` is not an HTML tag at all, so the
/// HTML map answers `HTMLUnknownElement` for every one of them.
///
/// The table (`korvus_dom::tags::MATHML_TAGS`, from `core/mathml/mathml_tag_names.json5`) has been
/// generated since W9.1a and was read by NOTHING until W9.1 -- 34 rows nobody consumed, while every
/// `<math>` on a page came out `HTMLUnknownElement`. Blink declares MathMLRowElement,
/// MathMLFractionElement and 26 more in C++ and exposes ONE interface to script, so all 34 rows name
/// `MathMLElement`; the map is kept per-tag anyway, because the shape is the namespace's, not this
/// build's, and a capture that splits them would change the table and nothing else.
#[derive(Default)]
pub(crate) struct MathmlTagProtos(pub(crate) std::cell::RefCell<std::collections::HashMap<String, v8::Global<v8::Object>>>);

/// `MathMLElement.prototype`, the MathML-namespace MISS path -- `mathml_tag_names.json5`'s own
/// `fallbackJSInterfaceName`. Headless Chrome 150 on this machine, measured 2026-09-12:
/// `createElementNS(mathmlNS, "zzznotatag")` is a `MathMLElement`, exactly as `mi` and `math` are.
#[derive(Default)]
pub(crate) struct MathmlElementProto(pub(crate) std::cell::RefCell<Option<v8::Global<v8::Object>>>);

/// ownerDocument overrides: NodeId -> the creating Document (a page-JS facade document held strongly).
/// Absent = the page document (the historical default, still correct for every main-document node).
/// Set when a non-main document's factories create or adopt a node (`DOMImplementation.createDocument`
/// / `createHTMLDocument` / a facade's `createElement`), so `Node.ownerDocument` reports the creating
/// document. Adoption falls out naturally: appending a node into another document re-marks it.
#[derive(Default)]
pub(crate) struct OwnerMap(pub(crate) std::cell::RefCell<std::collections::HashMap<u32, v8::Global<v8::Value>>>);

/// Parsed qualified names ("validate and extract" output): NodeId -> (prefix, localName). Recorded by
/// the createElementNS / DOMImplementation.createDocument paths; `prefix` None = no prefix. Backs
/// `Element.prefix` (null when unrecorded) and the QName-aware `Element.localName` (the parsed local
/// name rather than the whole tag). Absent = legacy behavior: prefix null, localName = the tag.
#[derive(Default)]
pub(crate) struct QNameMap(pub(crate) std::cell::RefCell<std::collections::HashMap<u32, (Option<String>, String)>>);

