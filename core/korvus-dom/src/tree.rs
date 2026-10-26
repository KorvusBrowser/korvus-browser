//! Arena DOM — a flat `Vec<Node>` with `NodeId` (Copy u32) handles.
//!
//! G1.5: parse HTML into this arena; G1.6 exposes it to JS via native `#[op2]` Document/Element/Node
//! classes (in `engine`, keyed by `NodeId`). Arena (not Rc/RefCell) so handles are cheap and Copy.

use crate::atom::Atom;

/// Index into [`Dom::nodes`].
pub type NodeId = u32;

/// The namespace an element node is in — the fact the HTML parser's FOREIGN CONTENT rules produce and
/// which nothing downstream can recover once it is gone.
///
/// html5ever applies the whole of the tree-construction foreign-content algorithm: inside `<svg>` it
/// puts the element in the SVG namespace, applies the SVG tag-name adjustment table (`clippath` ->
/// `clipPath`, `feblend` -> `feBlend`) and adjusts the foreign attribute names. The rcdom walk used to
/// copy only `QualName::local` and drop the namespace, so an inline `<svg><rect>` -- how essentially
/// every real page ships an icon, a logo or a chart -- landed in the arena indistinguishable from an
/// unknown HTML tag: `HTMLUnknownElement` in the XHTML namespace with every SVG member `undefined`,
/// while `createElementNS(svgNS,"rect")` (which recorded the namespace in a SIDE map) worked.
///
/// The namespace lives HERE, on the node, and is the single source for it: `Element.namespaceURI`,
/// the prototype pick, `tagName` casing, `getElementsByTagName(NS)`, `isEqualNode` and serialization
/// all read this one field. It is `Clone`, so `cloneNode` carries the namespace for free -- which the
/// side map could not do, since it was keyed by node id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ElementNs {
    /// `http://www.w3.org/1999/xhtml` — `createElement`, every HTML-namespace element the parser
    /// builds, and the XML parse path (which does no xmlns resolution of its own).
    #[default]
    Html,
    /// `http://www.w3.org/2000/svg`.
    Svg,
    /// `http://www.w3.org/1998/Math/MathML`.
    MathMl,
    /// Explicitly no namespace: `createElementNS(null, …)` / `createDocument(null, …)`.
    Null,
    /// Any other namespace URI, verbatim -- interned, as an element's name is: a `String` here made every
    /// node's kind 24 bytes wider for the rare element in a namespace of its own.
    Other(Atom),
}

/// The HTML namespace URI.
pub const NS_HTML: &str = "http://www.w3.org/1999/xhtml";
/// The SVG namespace URI.
pub const NS_SVG: &str = "http://www.w3.org/2000/svg";
/// The MathML namespace URI.
pub const NS_MATHML: &str = "http://www.w3.org/1998/Math/MathML";
/// The XML namespace URI — the one the reserved `xml` prefix is always bound to.
pub const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

impl ElementNs {
    /// The namespace URI, or `None` for the null namespace — exactly what `Element.namespaceURI`
    /// answers.
    pub fn uri(&self) -> Option<&str> {
        match self {
            ElementNs::Html => Some(NS_HTML),
            ElementNs::Svg => Some(NS_SVG),
            ElementNs::MathMl => Some(NS_MATHML),
            ElementNs::Null => None,
            ElementNs::Other(u) => Some(u.as_str()),
        }
    }

    /// The namespace for a URI, `None`/`""` meaning the null namespace. The three well-known URIs get
    /// their own variants so the parser never allocates for them.
    pub fn from_uri(uri: Option<&str>) -> ElementNs {
        match uri {
            None | Some("") => ElementNs::Null,
            Some(NS_HTML) => ElementNs::Html,
            Some(NS_SVG) => ElementNs::Svg,
            Some(NS_MATHML) => ElementNs::MathMl,
            Some(u) => ElementNs::Other(Atom::new(u)),
        }
    }

    /// Whether this is the HTML namespace — the test `tagName` casing, attribute-name case folding
    /// and `getElementsByTagName` all key on.
    pub fn is_html(&self) -> bool {
        matches!(self, ElementNs::Html)
    }
}

/// What a DOM node is.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    Document,
    /// A DocumentFragment: `document.createDocumentFragment()`, `new DocumentFragment()`, a
    /// `<template>`'s `content`, and a Range's `createContextualFragment`. nodeType 11, nodeName
    /// "#document-fragment". It is its own kind rather than a reused `Document` so that the pieces a
    /// page can observe are right: `nodeType`, and the pre-insertion validity rule that a fragment
    /// is INSERTABLE (its children are moved in) while a Document is not -- svelte 5 clones a
    /// template's content and inserts it, which a Document-typed content made a HierarchyRequestError.
    Fragment,
    /// Boxed: a document has one, and three `String`s inline made every node 72 bytes of kind.
    Doctype(Box<DoctypeData>),
    /// `name` is the qualified name as authored (the parser's adjusted SVG name, `createElementNS`'s
    /// whole `prefix:local`); `ns` is the namespace the parser or the factory put it in (see
    /// [`ElementNs`]); `attrs` are keyed by QUALIFIED attribute name (`xlink:href`, not `href`),
    /// which is the convention `setAttributeNS`/`getAttributeNS` already store and match by.
    ///
    /// The name is an [`Atom`]: 4 bytes and no heap block of its own, where a `String` was 24 and one.
    Element { name: Atom, ns: ElementNs, attrs: Vec<(String, String)> },
    Text(String),
    Comment(String),
    /// XML processing instruction (`<?target data?>`). Carries (target, data).
    ProcessingInstruction(Box<PiData>),
    /// XML CDATA section.
    CData(String),
}

/// A doctype's three strings ([`NodeKind::Doctype`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DoctypeData {
    pub name: String,
    pub public_id: String,
    pub system_id: String,
}

/// A processing instruction's target and data ([`NodeKind::ProcessingInstruction`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PiData {
    pub target: String,
    pub data: String,
}

/// The encapsulation mode recorded by a native shadow root (Korvus GS ②-A1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowRootMode {
    Open,
    Closed,
}

/// `ShadowRoot.slotAssignment` — "named" (the default: slots match by `name`/`slot` attribute,
/// [`crate::slot`]'s algorithm) or "manual" (assignment would instead be driven by
/// `HTMLSlotElement.assign()`). Recorded so the property round-trips; `slot.rs`'s algorithm
/// itself is the "named" behaviour only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowSlotAssignment {
    Named,
    Manual,
}

/// Metadata for a shadow-root arena node, keyed by the root's own [`NodeId`] in
/// [`Dom::shadow_roots`].
///
/// A shadow root owns an ordinary child list (like any other node), but is not an ordinary
/// child of its host: the root's `Node::parent` stays `None`, and the host<->root edge lives
/// only in [`Dom::shadow_roots`]/[`Dom::shadow_roots_by_host`]. This keeps light-tree
/// `parentNode`-style walks scoped to one tree; shadow-aware traversal/matching that crosses
/// the edge on purpose is Korvus GS ②-A2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowInfo {
    pub host: NodeId,
    pub mode: ShadowRootMode,
    pub delegates_focus: bool,
    pub slot_assignment: ShadowSlotAssignment,
    pub clonable: bool,
    pub serializable: bool,
    /// True only for a root the HTML parser attached from a `<template shadowrootmode>` (never
    /// for one `attachShadow()` created), and only until the FIRST subsequent `attachShadow()`
    /// call on the same host consumes it. While true, exactly one `attachShadow()` call with a
    /// MATCHING `mode` is allowed to succeed -- reusing this root (its content cleared, every
    /// other setting left exactly as the markup declared) instead of throwing -- which is how a
    /// page recovers a reference to a `mode:"closed"` declarative root. WPT
    /// `shadow-dom/declarative/declarative-shadow-dom-repeats.html`.
    pub declarative: bool,
}

/// Failure modes for [`Dom::attach_shadow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachShadowError {
    /// `host` is not an `Element` node (e.g. Text/Comment/Document/Doctype/PI/CData).
    HostNotElement,
    /// `host` already has a shadow root — a host may only ever attach one (spec:
    /// `attachShadow()` throws `NotSupportedError` on a second call).
    HostAlreadyHasShadowRoot,
}

impl std::fmt::Display for AttachShadowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::HostNotElement => "shadow host is not an element",
            Self::HostAlreadyHasShadowRoot => "shadow host already has a shadow root",
        };
        f.write_str(message)
    }
}

impl std::error::Error for AttachShadowError {}

/// One arena node. Children are stored Chrome/Blink-style as sibling links
/// (`first_child`/`last_child` on the parent + `prev_sibling`/`next_sibling` on each child)
/// rather than a per-node `Vec<NodeId>`, so a re-parent is a handful of pointer updates and a
/// node carries no owned child allocation. Walk a node's children with [`Dom::children`] (or
/// `first_child` → `next_sibling` directly). All link fields are `None` on a freshly created,
/// detached node.
#[derive(Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    /// The element's class tokens as `(hash, start, end)` into its `class` value: selector
    /// matching asks `has_class` once per class selector per candidate rule, and splitting the
    /// attribute each time was 74.5 % of a live airbnb.com load. Set wherever a
    /// class value changes (`push`, `set_attribute`, `remove_attribute`) -- the same three points
    /// `id_index` and `name_index` rely on.
    pub(crate) classes: Option<ClassTokens>,
    /// How many times the element's `style` attribute was written with another text or removed. See
    /// [`Dom::style_writes_of`].
    pub(crate) style_writes: u32,
}

/// An element's class tokens: the one a single-class value has, inline -- the common write, which then
/// allocates nothing -- or the run of several, allocated at its exact length.
#[derive(Debug, Clone)]
pub(crate) enum ClassTokens {
    One(ClassToken),
    Many(Box<[ClassToken]>),
}

impl ClassTokens {
    fn as_slice(&self) -> &[ClassToken] {
        match self {
            ClassTokens::One(t) => std::slice::from_ref(t),
            ClassTokens::Many(ts) => ts,
        }
    }
}

/// One class token of an element: [`string_hash`](crate::select_engine::string_hash) of it, the same
/// of its ASCII-lowercase form ([`folded_string_hash`](crate::select_engine::folded_string_hash), what a
/// quirks document compares), and its byte range in the `class` value.
pub(crate) type ClassToken = (u32, u32, u32, u32);

/// An element's class tokens -- `split_whitespace`, as `has_class` has always split -- or `None` for none.
fn class_tokens(value: &str) -> Option<ClassTokens> {
    let base = value.as_ptr() as usize;
    let token = |t: &str| {
        let start = (t.as_ptr() as usize - base) as u32;
        let hash = crate::select_engine::string_hash(t);
        let folded = if t.bytes().any(|b| b.is_ascii_uppercase()) { crate::select_engine::folded_string_hash(t) } else { hash };
        (hash, folded, start, start + t.len() as u32)
    };
    let mut words = value.split_whitespace();
    let first = token(words.next()?);
    if words.next().is_none() {
        return Some(ClassTokens::One(first));
    }
    let mut all = Vec::with_capacity(value.split_whitespace().count());
    all.extend(value.split_whitespace().map(token));
    Some(ClassTokens::Many(all.into_boxed_slice()))
}

/// What [`Dom::at`] returns for an id that names no node: detached, childless, empty.
/// Move `node` from `old`'s list to `new`'s in a value -> elements index (`Dom::id_index`,
/// `Dom::name_index`); either may be absent (creation carrying the attribute, or its removal).
fn reindex(map: &mut std::collections::HashMap<String, Vec<NodeId>>, node: NodeId, old: Option<&str>, new: Option<&str>) {
    if old == new { return; }
    if let Some(old) = old {
        if let Some(list) = map.get_mut(old) {
            list.retain(|&n| n != node);
            if list.is_empty() { map.remove(old); }
        }
    }
    if let Some(new) = new {
        let list = map.entry(new.to_string()).or_default();
        if !list.contains(&node) { list.push(node); }
    }
}

fn detached_node() -> &'static Node {
    static NULL: std::sync::OnceLock<Node> = std::sync::OnceLock::new();
    NULL.get_or_init(|| Node {
        kind: NodeKind::Comment(String::new()),
        parent: None,
        first_child: None,
        last_child: None,
        prev_sibling: None,
        next_sibling: None,
        classes: None,
        style_writes: 0,
    })
}

/// The memo behind [`Dom::root_of`]: each node's root, stamped with the GENERATION it was written in.
/// A structure change bumps the generation instead of clearing the vector: clearing and refilling it
/// to the arena's size on every insertion was a `memset` of the whole arena per `appendChild` --
/// 26 % of a 100k-append loop's time and the quadratic term of the `fragment_append_20k`
/// (300 ms vs Chrome's 5).
#[derive(Debug, Clone, Default)]
pub(crate) struct RootsMemo {
    revision: u64,
    generation: u32,
    roots: Vec<(NodeId, u32)>,
}

impl RootsMemo {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Wrapped: a slot stamped 0 four billion changes ago must not read as fresh.
            self.roots.clear();
            self.generation = 1;
        }
    }
}

/// Does rewriting a `style` attribute from `old` to `new` change only declarations no box depends on --
/// the transform family and `opacity`, and the paint-only colours, outlines and shadows (see
/// [`Dom::inert_style_writes`])? Both texts are split into
/// declarations (the last of a name wins, as in the cascade) and compared name by name, value as text.
/// A text this cannot split plainly -- a quote, a backslash, a comment, a brace, an unbalanced bracket,
/// a declaration without a colon -- answers `false`, which costs a layout and nothing else.
///
/// The colours, outlines and shadows: korvus-page's layout, hit testing and layout-resolved values read none of
/// them (its layout reads Display ... Visibility, PointerEvents and ScrollbarColor, never a colour, an outline or a
/// shadow; paint and `getComputedStyle` take them from the cascade, which every write moves), and in Blink none of
/// them moves a box or a geometry read either -- an outline and a shadow are ink overflow, outside the border box
/// and the scrollable overflow. A page that set `el.style.color` and then read `offsetWidth` laid the document out
/// again for it: 5.5M instructions a round on a 300-row page (2026-10-23). Left out, as moving a box or a hit test
/// in Blink: `filter` (a containing block for fixed descendants), `fill`/`stroke` (SVG hit testing), `visibility`,
/// `pointer-events`, `background-image` (it loads a resource) and the `background`/`border` shorthands.
pub(crate) fn style_change_is_inert(old: &str, new: &str) -> bool {
    const INERT: &[&str] = &[
        "transform", "translate", "rotate", "scale", "opacity",
        "color", "background-color",
        "border-color", "border-top-color", "border-right-color", "border-bottom-color", "border-left-color",
        "border-block-color", "border-block-start-color", "border-block-end-color",
        "border-inline-color", "border-inline-start-color", "border-inline-end-color",
        "outline", "outline-color", "outline-style", "outline-width", "outline-offset",
        "box-shadow", "text-shadow",
    ];
    fn push<'t>(part: &'t str, out: &mut Vec<(String, &'t str)>) -> Option<()> {
        let part = part.trim();
        if part.is_empty() {
            return Some(());
        }
        let (name, value) = part.split_once(':')?;
        out.push((name.trim().to_ascii_lowercase(), value.trim()));
        Some(())
    }
    fn last<'t>(d: &[(String, &'t str)], name: &str) -> Option<&'t str> {
        d.iter().rev().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
    fn declarations(text: &str) -> Option<Vec<(String, &str)>> {
        if text.contains(['"', '\'', '\\', '/', '{', '}']) {
            return None;
        }
        let mut out = Vec::new();
        let (mut depth, mut start) = (0i32, 0usize);
        for (i, c) in text.char_indices() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => {
                    depth -= 1;
                    if depth < 0 {
                        return None;
                    }
                }
                ';' if depth == 0 => {
                    push(&text[start..i], &mut out)?;
                    start = i + 1;
                }
                _ => {}
            }
        }
        if depth != 0 {
            return None;
        }
        push(&text[start..], &mut out)?;
        Some(out)
    }
    let (Some(a), Some(b)) = (declarations(old), declarations(new)) else { return false };
    a.iter().chain(b.iter()).all(|(name, _)| INERT.contains(&name.as_str()) || last(&a, name) == last(&b, name))
}

/// What one attribute write changes for a document ([`Dom::render_revision`]).
#[derive(Clone, Copy)]
enum RenderChange {
    /// The value it leaves is the one it found (a write over itself, a removal of an absent attribute).
    Nothing,
    /// A `style` rewrite that changed only the transform family, `opacity` and the paint-only colours, outlines and
    /// shadows ([`style_change_is_inert`]).
    Inert,
    Visible,
}

impl RenderChange {
    fn of(key: &str, old: Option<&str>, new: Option<&str>) -> RenderChange {
        if old == new {
            RenderChange::Nothing
        } else if key == "style" && style_change_is_inert(old.unwrap_or(""), new.unwrap_or("")) {
            RenderChange::Inert
        } else {
            RenderChange::Visible
        }
    }
}

/// A process-unique [`Dom::instance`].
pub(crate) fn next_instance() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A parsed document as a flat arena.
pub struct Dom {
    pub(crate) nodes: Vec<Node>,
    /// Bumped by every mutation. Consumers that cache something derived from the whole tree --
    /// layout, above all -- compare this instead of re-deriving a fingerprint, which turns an
    /// O(nodes) freshness check into a single integer compare.
    ///
    /// Deliberately coarse: this is design D9's "a global dirty flag is enough". It does not say
    /// *what* changed, so a caller invalidates everything, which is right for a scraper that
    /// measures far more often than it mutates. Over-bumping is safe (a needless recompute);
    /// under-bumping is not (a stale answer), so it is bumped on entry to every mutating method
    /// rather than where the mutation actually happens.
    pub(crate) revision: u64,
    /// How many of those bumps were a write to an element's `style` attribute. See
    /// [`Dom::match_revision`].
    pub(crate) unrevised_attribute_writes: u64,
    /// How many of those were to a `data-*` attribute. See [`Dom::unrevised_writes`].
    pub(crate) data_attribute_writes: u64,
    /// The `<template for>` patches of a PAGE document that wait for its script pass, and the one being
    /// streamed: see [`Dom::stream_template_patches`].
    pub(crate) pending_patches: crate::partial_update::PendingPatches,
    /// How many bumps were ANY attribute write -- so [`Dom::structure_revision`] can ignore them.
    pub(crate) attribute_writes: u64,
    /// How many bumps no document can see: a node being made, and a change in a tree whose
    /// root is not a document. See [`Dom::render_revision`].
    pub(crate) render_skips: u64,
    /// How many of the bumps a document CAN see were in a document other than the page's:
    /// a frame's, a `DOMParser` result's. See [`Dom::page_render_revision`].
    pub(crate) off_page_renders: u64,
    /// Those off-page bumps per document root: what a FRAME's layout is keyed on -- its own
    /// document's changes, not the page's nor another frame's.
    pub(crate) doc_renders: rustc_hash::FxHashMap<NodeId, u64>,
    /// This arena's identity, unique in the process: two documents changed the same number of times
    /// share a [`Dom::render_revision`], and a fingerprint keyed on it tells them apart by this.
    pub(crate) instance: u64,
    /// See [`Dom::inert_style_writes`].
    pub(crate) inert_style_writes: u64,
    pub(crate) root: NodeId,
    /// Document-root NodeIds whose subtree is XML (case-preserving `tagName`, not HTML-uppercased).
    pub(crate) xml_roots: rustc_hash::FxHashSet<NodeId>,
    /// Elements in a TOP-LAYER state script set: a showing popover (bit 1), a modal dialog (bit 2).
    /// The arena holds them so `:popover-open` / `:modal` match what script did (`forms.rs`).
    pub(crate) open_states: rustc_hash::FxHashMap<NodeId, u8>,
    /// Shadow-root arena nodes, keyed by the root's own id. A shadow root is an ordinary node
    /// (see `attach_shadow`) that is never linked into any `children` list — the host<->root
    /// relationship lives only here and in `shadow_roots_by_host`, so ordinary parent/children
    /// walks never cross into a shadow tree by accident.
    pub(crate) shadow_roots: rustc_hash::FxHashMap<NodeId, ShadowInfo>,
    /// Reverse index: host element -> its shadow root. A host has at most one entry.
    pub(crate) shadow_roots_by_host: rustc_hash::FxHashMap<NodeId, NodeId>,
    /// `ShadowRoot.referenceTarget` (Chrome 152), kept apart from the `Copy` [`ShadowInfo`]: a root with no
    /// entry answers `null`.
    pub(crate) shadow_reference_targets: rustc_hash::FxHashMap<NodeId, String>,
    /// `<template>` element -> its `content` DocumentFragment node (GS ②-A3's shadow-root pattern
    /// applied to the template's content edge: HTML gives a template TWO child lists -- the light
    /// one stays empty and the parsed markup lives in an unlinked fragment reachable only via
    /// `template.content`, so ordinary walks and queries never cross into it by accident).
    pub(crate) template_contents: rustc_hash::FxHashMap<NodeId, NodeId>,
    /// HTML's `[[CryptographicNonce]]` where it differs from the `nonce` content attribute:
    /// what the `nonce` IDL attribute set -- it never writes the attribute -- or what an element kept when its
    /// attribute was hidden. Absent: the slot is the attribute's value. See [`Dom::nonce`].
    pub(crate) nonce_slots: rustc_hash::FxHashMap<NodeId, Box<str>>,
    /// Documents whose CSP list holds a policy their response's HEADER delivered: an element connected to one has its
    /// `nonce` attribute hidden ([`Dom::set_nonce_hiding`]).
    pub(crate) nonce_hiding_docs: rustc_hash::FxHashSet<NodeId>,
    /// The elements the pointer is currently over: the hit element **and every element ancestor of
    /// it**, OUTERMOST FIRST (`<html>` … the hit element). Empty when the pointer is over no box.
    ///
    /// # Why it lives on the arena
    ///
    /// `:hover` is asked three different ways -- `Element.matches(":hover")`,
    /// `document.querySelectorAll(":hover")`, and the cascade, which decides whether
    /// `li:hover > ul { display:block }` opens a menu. All three reach the selector engine through
    /// [`crate::select_engine`]'s `NodeRef`, whose only state is `&Dom` and a `NodeId`. Putting the
    /// chain anywhere else would make the three read different things, which is the one way this
    /// feature can be wrong without any test noticing.
    ///
    /// # Why a chain and not an element
    ///
    /// Measured, not assumed (stock Chrome 150.0.7871.46, driven over CDP): moving the pointer into
    /// a `div#outer > div#mid > div#inner` answers
    /// `querySelectorAll(":hover") == [html, body, #outer, #mid, #inner]`, and all three `:hover`
    /// rules apply at once. A single-element implementation would leave every hover menu on the web
    /// silently shut.
    ///
    /// An ancestor CHAIN, not the box stack `elements_from_point` returns: a `position:fixed`
    /// overlay's box contains points that also lie inside unrelated content below it, and that
    /// content is not hovered. Chrome confirms the shape at the other end too -- a point past
    /// `<body>`'s box but inside `<html>`'s answers `[html]` alone.
    ///
    /// Not `pub`: written only by [`Dom::set_hover_chain`], so the revision bump cannot be skipped.
    pub(crate) hover_chain: Vec<NodeId>,
    /// The focused element and whether its focus is VISIBLE (`:focus-visible`), as the focus ring last moved
    /// ([`Dom::set_focus`]); `None` when the body has it. Not `pub`, for the same reason as `hover_chain`.
    pub(crate) focus: Option<(NodeId, bool)>,
    /// Form-control state that is not a content attribute -- checkedness, selectedness,
    /// `indeterminate`, custom validity. Absent = untouched. See `forms.rs`.
    pub(crate) controls: rustc_hash::FxHashMap<NodeId, crate::forms::ControlState>,
    /// Every `OpaqueRange` a text control's `createValueRange` made (Chrome 152), by id; see `forms.rs`.
    pub(crate) opaque_ranges: Vec<crate::forms::OpaqueRange>,
    /// Text control -> the ids of its connected `OpaqueRange`s, and the input type they were made under.
    pub(crate) opaque_by_element: rustc_hash::FxHashMap<NodeId, (Vec<u32>, &'static str)>,
    /// The bounds the NEXT value write of a control edits (`setRangeText`'s start/end), so the ranges move by
    /// that edit and not by a whole-value diff -- Blink's `ScopedSkipValueAutoDiff`. Taken by the write.
    pub(crate) value_edit_bounds: Option<(NodeId, u32, u32)>,
    /// `pattern` answers the script engine gave, by `(pattern, value)` (`Dom::set_pattern_result`).
    pub(crate) pattern_results: std::collections::HashMap<(String, String), bool>,
    /// How many elements named `base` this arena has ever made ([`Dom::has_made_base`]): none, and no `<base>` can be
    /// in any of its documents.
    pub(crate) bases_made: u32,
    /// `id` attribute value -> every element `NodeId` that currently carries (or has carried,
    /// pending a later removal) that value. Backs `get_element_by_id` in O(1)-ish time instead of
    /// an O(n) arena scan.
    ///
    /// A Vec (not a single first-wins `NodeId`) because duplicate ids are legal HTML: if the
    /// winning (lowest-NodeId, in-document) duplicate is later removed from the tree, lookups must
    /// fall through to the runner-up, which a single cached winner could not do without an
    /// additional removal-time re-scan.
    ///
    /// NOT pruned when a candidate is structurally detached from the tree (`remove_child` and
    /// friends do not touch this map) -- `get_element_by_id` filters candidates by
    /// `is_in_document` live, at query time, computed fresh from the current parent chain. So a
    /// stale entry for a detached node just fails that filter; it costs a little unbounded growth,
    /// never a wrong answer. Every path that changes what `id` an element *carries* (creation with
    /// an `id` attribute, `set_attribute`, `remove_attribute`) MUST go through `index_id` — that is
    /// the one invariant this map depends on.
    pub(crate) id_index: std::collections::HashMap<String, Vec<NodeId>>,
    /// `name` attribute value -> the elements carrying it. The named-property lookups on
    /// `window` and `document` read their candidates here instead of scanning the arena per missing
    /// name (21 µs and 72 µs per miss at 5,000 elements, measured 2026-09-29). Kept exactly like
    /// `id_index`: every path that changes what `name` an element carries (`push`, `set_attribute`,
    /// `remove_attribute`) goes through `index_name`, and nothing prunes it on detach -- a
    /// candidate's tree is the caller's question, asked at query time.
    pub(crate) name_index: std::collections::HashMap<String, Vec<NodeId>>,
    /// The document roots the HTML parser put in QUIRKS or LIMITED-QUIRKS mode. Absent
    /// is no-quirks, which is every document script creates and every document with `<!doctype html>`.
    ///
    /// The verdict is the parser's own -- html5ever implements HTML's "initial" insertion mode
    /// doctype sniffing -- and it was being thrown away: `document.compatMode` was the constant
    /// `CSS1Compat`. Measured on Chrome 150 over loopback, with one body served under each prolog:
    ///
    /// ```text
    /// no doctype                                                  BackCompat   quirks
    /// <!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01 Transitional//EN">          BackCompat   quirks
    /// ... 4.01 Transitional//EN" "http://www.w3.org/TR/html4/loose.dtd"        CSS1Compat   limited quirks
    /// ... XHTML 1.0 Transitional//EN" "...xhtml1-transitional.dtd"             CSS1Compat   limited quirks
    /// <!DOCTYPE html SYSTEM "http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd">  BackCompat
    /// ... HTML 3.2 Final//EN                                      BackCompat   quirks
    /// ... HTML 4.01//EN" strict.dtd / <!doctype html>             CSS1Compat   no quirks
    /// ```
    ///
    /// (The limited-quirks rows are told apart from no-quirks by layout alone: an image alone on a
    /// line is 10px tall in both quirks modes and 18px in no-quirks.)
    pub(crate) document_modes: rustc_hash::FxHashMap<NodeId, DocumentMode>,
    /// Where each PARSER-made `<script>` sits in the markup it was parsed from: the V8 line and column
    /// offsets of its first character (`script_position`).
    pub(crate) script_positions: rustc_hash::FxHashMap<NodeId, (u32, u32)>,
    /// [`Dom::root_of`], memoized per [`Dom::structure_revision`]. Selector matching
    /// asks for the root of the element it tests on EVERY compound -- `is_in_xml` for a type
    /// selector, the document mode for class and id case -- and each ask climbed the tree: on a
    /// quirks-mode page with thousands of rules that climb and its SipHash lookup were a third of the
    /// cascade (salesforce's replay hung on it).
    pub(crate) roots_memo: std::cell::RefCell<RootsMemo>,
    /// Every mutation a document can see, node by node. See [`crate::journal`].
    pub(crate) journal: crate::journal::Journal,
    /// The `style` attribute as the cascade reads it, where the CSSOM wrote values its serialization rounds:
    /// element -> (its [`Dom::style_writes_of`] when written, the same declarations at full precision). See
    /// [`Dom::set_inline_style`].
    pub(crate) precise_styles: rustc_hash::FxHashMap<NodeId, (u32, String)>,
    /// How many times each element's `transform` attribute was written or removed: an SVG `transform` list keeps
    /// its items as objects, and the content attribute re-parses into new ones at every write -- the same text
    /// included, as Blink's `SVGElement::AttributeChanged` does. See [`Dom::transform_writes`].
    pub(crate) transform_writes: rustc_hash::FxHashMap<NodeId, u32>,
}

/// A document's mode, as HTML's parser decides it from the doctype (HTML §13.2.6.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocumentMode {
    #[default]
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

impl Dom {
    /// The mode of the document `doc` (a Document node). No-quirks unless the parser said otherwise.
    pub fn document_mode(&self, doc: NodeId) -> DocumentMode {
        self.document_modes.get(&doc).copied().unwrap_or_default()
    }

    /// The mode of the document `node` belongs to. Free on a page with no quirks document at all,
    /// which is nearly every page: the root climb only runs when some document is in a quirks mode.
    pub fn document_mode_of(&self, node: NodeId) -> DocumentMode {
        if self.document_modes.is_empty() {
            return DocumentMode::NoQuirks;
        }
        self.document_mode(self.root_of(node))
    }

    /// Where the parser-made `<script>` `id`'s text begins in the markup it came from, as V8's
    /// `ScriptOrigin` offsets: the 0-based line, and the UTF-16 column on that first line. `None` for a
    /// script no parser made (`createElement`), whose lines count from its own first line, in Chrome too.
    pub fn script_position(&self, id: NodeId) -> Option<(u32, u32)> {
        self.script_positions.get(&id).copied()
    }

    /// Record the parser's verdict for `doc` -- or, for `document.open()` + `write()`, the verdict the
    /// written markup's parse reached, carried onto the document it was written into.
    pub fn set_document_mode(&mut self, doc: NodeId, mode: DocumentMode) {
        self.journal.push(crate::journal::Change::Other { node: Some(doc), what: "document mode" });
        match mode {
            DocumentMode::NoQuirks => {
                self.document_modes.remove(&doc);
            }
            m => {
                self.document_modes.insert(doc, m);
            }
        }
    }

    /// An empty document (just the Document root) — the default before any HTML is loaded.
    pub fn empty() -> Dom {
        let mut dom = Dom {
            nodes: Vec::new(),
            root: 0,
            revision: 0,
            unrevised_attribute_writes: 0,
            data_attribute_writes: 0,
            attribute_writes: 0,
            render_skips: 0,
            off_page_renders: 0,
            doc_renders: Default::default(),
            pending_patches: Default::default(),
            instance: next_instance(),
            inert_style_writes: 0,
            xml_roots: Default::default(),
            open_states: Default::default(),
            shadow_roots: Default::default(),
            shadow_roots_by_host: Default::default(),
            shadow_reference_targets: Default::default(),
            template_contents: Default::default(),
            nonce_slots: Default::default(),
            nonce_hiding_docs: Default::default(),
            hover_chain: Vec::new(),
            focus: None,
            controls: Default::default(),
            opaque_ranges: Vec::new(),
            opaque_by_element: Default::default(),
            value_edit_bounds: None,
            pattern_results: Default::default(),
            bases_made: 0,
            id_index: Default::default(),
            name_index: Default::default(),
            document_modes: Default::default(),
            script_positions: Default::default(),
            roots_memo: Default::default(),
            journal: Default::default(),
            precise_styles: Default::default(),
            transform_writes: Default::default(),
        };
        let root = dom.push(NodeKind::Document, None);
        dom.root = root;
        dom
    }

    /// Create a new (detached) element node and return its id. Backs `document.createElement`, which
    /// per DOM always produces an HTML-namespace element.
    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        // HTML lower-cases the tag ASCII-only ("aÇ" stays "aÇ", not "aç"): non-ASCII is left as-is.
        self.push(NodeKind::Element { name: Atom::new(&tag.to_ascii_lowercase()), ns: ElementNs::Html, attrs: Vec::new() }, None)
    }

    /// Create a detached element preserving its name case-as-is (XML / `createDocument`), in the HTML
    /// namespace — the historical default for every path that recorded no namespace of its own.
    pub fn create_element_raw(&mut self, name: &str) -> NodeId {
        self.create_element_ns(name, ElementNs::Html)
    }

    /// Create a detached element preserving its name case-as-is, in `ns`. Backs `createElementNS` and
    /// `DOMImplementation.createDocument`: the namespace they were given goes on the NODE, which is
    /// the single place anything asks for it.
    pub fn create_element_ns(&mut self, name: &str, ns: ElementNs) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::Element { name: Atom::new(name), ns, attrs: Vec::new() }, None)
    }

    /// The element's namespace, or `None` when `id` is not an element.
    pub fn element_ns(&self, id: NodeId) -> Option<&ElementNs> {
        match &self.at(id).kind {
            NodeKind::Element { ns, .. } => Some(ns),
            _ => None,
        }
    }

    /// `Element.namespaceURI`: the namespace URI, `None` for the null namespace **and** for a
    /// non-element node (both answer null).
    pub fn namespace_uri(&self, id: NodeId) -> Option<&str> {
        self.element_ns(id).and_then(|n| n.uri())
    }

    /// Whether `id` is an element in the HTML namespace. Non-elements answer `false`.
    pub fn is_html_ns(&self, id: NodeId) -> bool {
        self.element_ns(id).is_some_and(|n| n.is_html())
    }

    /// Re-record an element's namespace after the element already exists.
    ///
    /// NOTHING IN THE TREE CALLS THIS. Its one caller was a legacy op in `core/korvus-webapi`'s
    /// element bindings, which recorded a namespace onto an element that `createElementNS` had
    /// already minted and interned; that op was deleted because the factory now passes
    /// the namespace to [`Dom::create_element_ns`] at creation, so the node is never briefly of the
    /// wrong kind and there is nothing to re-record. Kept as the arena's answer to "retype this
    /// element" -- it is the only way to express that -- but it is currently unreached.
    pub fn set_element_ns(&mut self, id: NodeId, ns: ElementNs) {
        if !self.has(id) { return; }
        if let NodeKind::Element { ns: slot, .. } = &mut self.nodes[id as usize].kind {
            *slot = ns;
            self.revision += 1;
            self.unrendered_if(id);
            self.journal.push(crate::journal::Change::Other { node: Some(id), what: "namespace" });
        }
    }

    /// Create a new (detached) DocumentFragment. Backs `document.createDocumentFragment`, the
    /// `DocumentFragment` constructor, and `Range.createContextualFragment`. nodeType 11.
    pub fn create_fragment(&mut self) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::Fragment, None)
    }

    /// Create a new (detached) text node. Backs `document.createTextNode`.
    pub fn create_text_node(&mut self, text: &str) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::Text(text.to_string()), None)
    }

    /// Create a new (detached) comment node. Backs `document.createComment`.
    pub fn create_comment(&mut self, text: &str) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::Comment(text.to_string()), None)
    }

    /// Create a new (detached) CDATA-section node. Backs `document.createCDATASection` (an XML-only
    /// member the HTML parser never produces, so it only ever exists through this constructor).
    pub fn create_cdata_section(&mut self, text: &str) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::CData(text.to_string()), None)
    }

    /// Create a new (detached) DocumentType node. Backs `implementation.createDocumentType`.
    pub fn create_doctype(&mut self, name: &str, public_id: &str, system_id: &str) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(
            NodeKind::Doctype(Box::new(DoctypeData {
                name: name.to_string(),
                public_id: public_id.to_string(),
                system_id: system_id.to_string(),
            })),
            None,
        )
    }

    /// Create a new (detached) processing-instruction node. Backs `document.createProcessingInstruction`.
    pub fn create_processing_instruction(&mut self, target: &str, data: &str) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::ProcessingInstruction(Box::new(PiData { target: target.to_string(), data: data.to_string() })), None)
    }

    /// DocumentType fields (name, publicId, systemId); empty strings if `id` is not a doctype.
    pub fn doctype_fields(&self, id: NodeId) -> (String, String, String) {
        match &self.at(id).kind {
            NodeKind::Doctype(d) => (d.name.clone(), d.public_id.clone(), d.system_id.clone()),
            _ => (String::new(), String::new(), String::new()),
        }
    }

    /// PI target/data (empty if not a PI).
    pub fn pi_fields(&self, id: NodeId) -> (String, String) {
        match &self.at(id).kind {
            NodeKind::ProcessingInstruction(pi) => (pi.target.clone(), pi.data.clone()),
            _ => (String::new(), String::new()),
        }
    }

    /// Replace `old_node` with `new_node` in `parent`.
    /// WHATWG "ensure pre-insertion validity": inserting `new_node` under `parent` is a hierarchy
    /// error when `new_node` is an inclusive ancestor of `parent` (`new_node == parent`, or it sits
    /// above `parent` in the tree), because the link would make the tree cyclic. The arena must
    /// reject it: otherwise `root_of()` / `is_in_document()` — and every other parent-chain walk —
    /// spin forever on the self-referential edge (an uninterruptible 100%-CPU hang that a page's own
    /// JS can trigger via `el.append(el)`). Insertion *callbacks* additionally throw
    /// HierarchyRequestError for spec conformance; internal callers (parse, outerHTML=, …) lean on
    /// this silent guard to keep the acyclic invariant.
    fn would_create_cycle(&self, parent: NodeId, new_node: NodeId) -> bool {
        self.host_including_contains(new_node, parent)
    }

    /// DOM's "host-including inclusive ancestor": `ancestor` is `descendant`, one of its ancestors, or
    /// -- crossing each shadow root to its HOST -- one of the host's. Moving an element into the shadow
    /// root of its own descendant links the two trees into a cycle no parent walk sees but every
    /// shadow-including walk follows forever: `host.attachShadow().appendChild(document.body)` with
    /// `host` inside `body` overflowed the stack in `node::reaction_ids` (the gate, WPT
    /// `html/syntax/parsing/custom-element-reparenting-shadow-cycle.html`). Chrome throws
    /// HierarchyRequestError there, and the parser drops the element.
    pub fn host_including_contains(&self, ancestor: NodeId, descendant: NodeId) -> bool {
        let mut cur = Some(descendant);
        let mut budget = self.nodes.len();
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            if budget == 0 { break; }
            budget -= 1;
            cur = match self.at(c).parent {
                Some(p) => Some(p),
                None => self.shadow_host_of(c),
            };
        }
        false
    }

    pub fn replace_child(&mut self, parent: NodeId, new_node: NodeId, old_node: NodeId) {
        self.replace_child_unhidden(parent, new_node, old_node);
        self.hide_nonces_after_insert(new_node);
    }

    fn replace_child_unhidden(&mut self, parent: NodeId, new_node: NodeId, old_node: NodeId) {
        self.revision += 1;
        self.unrendered_if(parent);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(parent) || !self.has(new_node) || !self.has(old_node) { return; }
        if self.would_create_cycle(parent, new_node) { return; }
        // `old_node` must really be a child of `parent`. Bounds-check before indexing (defensive
        // against a sentinel/out-of-range id, same class as the `insert_before` reference guard).
        //
        // A non-child `old_node` is a NO-OP — nothing is unlinked, nothing is re-parented. The DOM
        // spec has `replaceChild` throw `NotFoundError` *before* mutating anything, so "no mutation"
        // is the closest faithful behavior at this layer.
        //
        // This deliberately drops a pre-sibling-link quirk that used to leave `new_node` with
        // `parent = Some(parent)` while unlinked. Under the old `children: Vec<NodeId>` storage that
        // state was inert; under sibling links it is CORRUPT, because `parent == Some(p)` is now the
        // membership test every mutator uses. A node in that state made `unlink` clear the parent's
        // `first_child`/`last_child` (silently dropping all of its real children, which kept their
        // parent pointers and so still reported `is_in_document`), and made `insert_before` splice a
        // second, unreachable chain onto the same parent. Found by the randomized invariant test in
        // `tests/arena_properties.rs`; see `unlinking_a_phantom_parent_node_does_not_drop_the_parents_children`.
        if (old_node as usize) >= self.nodes.len() || self.at(old_node).parent != Some(parent) {
            return;
        }
        // Replacing a node with itself is a no-op. The spec reaches that outcome by re-pointing the
        // reference child (`If referenceChild is node, set referenceChild to node's next sibling`),
        // so `n` ends up exactly where it started. Reaching it by no-op matters here because the
        // splice below reads `old_node`'s sibling links AFTER unlinking `new_node`, and then clears
        // `old_node`'s links last — when the two are the same node that final clear wipes the parent
        // pointer the splice just set, leaving the parent's `first_child`/`last_child` pointing at a
        // node whose `parent` is `None`. Found by `tests/arena_properties.rs`.
        if new_node == old_node {
            return;
        }
        // Detach new_node from its current parent (the old code's unconditional retain).
        self.unlink(new_node);
        {
            let prev = self.at(old_node).prev_sibling;
            let next = self.at(old_node).next_sibling;
            {
                let n = &mut self.nodes[new_node as usize];
                n.parent = Some(parent);
                n.prev_sibling = prev;
                n.next_sibling = next;
            }
            self.forget_roots();
            match prev {
                Some(p) => self.nodes[p as usize].next_sibling = Some(new_node),
                None => self.nodes[parent as usize].first_child = Some(new_node),
            }
            match next {
                Some(nx) => self.nodes[nx as usize].prev_sibling = Some(new_node),
                None => self.nodes[parent as usize].last_child = Some(new_node),
            }
            let n = &mut self.nodes[old_node as usize];
            n.parent = None;
            n.prev_sibling = None;
            n.next_sibling = None;
            self.forget_roots();
            self.journal.push(crate::journal::Change::Removed { parent, node: old_node, next });
            self.journal.push(crate::journal::Change::Inserted { parent, node: new_node });
        }
    }

    /// Insert `new_node` as the first child of `parent`.
    pub fn prepend_child(&mut self, parent: NodeId, new_node: NodeId) {
        self.prepend_child_unhidden(parent, new_node);
        self.hide_nonces_after_insert(new_node);
    }

    fn prepend_child_unhidden(&mut self, parent: NodeId, new_node: NodeId) {
        self.revision += 1;
        self.unrendered_if(parent);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(parent) || !self.has(new_node) { return; }
        if self.would_create_cycle(parent, new_node) { return; }
        self.unlink(new_node);
        let old_first = self.at(parent).first_child;
        {
            let n = &mut self.nodes[new_node as usize];
            n.parent = Some(parent);
            n.prev_sibling = None;
            n.next_sibling = old_first;
        }
        self.forget_roots();
        match old_first {
            // Existing first child now follows new_node.
            Some(f) => self.nodes[f as usize].prev_sibling = Some(new_node),
            // Empty parent: new_node is also the last child.
            None => self.nodes[parent as usize].last_child = Some(new_node),
        }
        self.nodes[parent as usize].first_child = Some(new_node);
        self.journal.push(crate::journal::Change::Inserted { parent, node: new_node });
    }

    /// Clone a node (optionally deep). Returns a new detached node id.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return id; }
        let kind = self.at(id).kind.clone();
        let new = self.push(kind, None);
        // An element's inline style is cloned as its declarations, not re-parsed from the attribute's
        // serialization (Blink's `CloneAttributesFrom` copies `inline_style_`): Chrome 150 lays an imported text
        // under `style.transform = "scale(1.000998)"` out at 1.000998 (the challenge's SVG probe).
        // (stamped with the clone's own count: 0, as `push` made it)
        if let Some((_, precise)) = self.precise_styles.get(&id) {
            let entry = (self.nodes[new as usize].style_writes, precise.clone());
            self.precise_styles.insert(new, entry);
        }
        // HTML's cloning steps for HTMLOrSVGElement: the clone's `[[CryptographicNonce]]` is the original's (Chrome 150:
        // a hidden nonce's clone reads `nonce=""` as an attribute and the original's value through `.nonce`)
        if let Some(n) = self.nonce_slots.get(&id).cloned() {
            self.nonce_slots.insert(new, n);
        }
        // HTML's cloning steps for `<input>` (value, dirty value flag, checkedness, dirty checkedness
        // flag) and for `<textarea>` (raw value and its dirty flag). Chrome 150: `cb.checked = true;
        // cb.cloneNode().checked` is true, and WPT's `the-input-element/clone.html` asserts a field
        // whose value script changed keeps the CHANGED value through a clone.
        if matches!(self.tag_name(id), Some("input") | Some("textarea")) {
            if let Some(state) = self.controls.get(&id).map(|s| crate::forms::ControlState {
                checked: s.checked,
                dirty_checked: s.dirty_checked,
                // Not in the spec's cloning steps, but Chrome 150 clones it and WPT's
                // `the-input-element/cloning-steps.html` asserts it per type.
                indeterminate: s.indeterminate,
                value: s.value.clone(),
                ..Default::default()
            }) {
                self.controls.insert(new, state);
            }
        }
        if deep {
            for c in self.children(id) {
                let cc = self.clone_node(c, true);
                // cc is freshly cloned (detached); link it as the next child of `new`.
                self.link_as_last_child(new, cc);
            }
            // A template's markup lives in its content fragment -- follow that edge on a deep
            // clone, else `template.cloneNode(true)` comes back empty.
            if let Some(frag) = self.template_content_of(id) {
                let new_frag = self.ensure_template_content(new);
                for c in self.children(frag) {
                    let cc = self.clone_node(c, true);
                    self.link_as_last_child(new_frag, cc);
                }
            }
        }
        new
    }

    /// Insert `new_node` before `reference` in `parent` (or append if `reference` isn't a child).
    pub fn insert_before(&mut self, parent: NodeId, new_node: NodeId, reference: NodeId) {
        self.insert_before_unhidden(parent, new_node, reference);
        self.hide_nonces_after_insert(new_node);
    }

    fn insert_before_unhidden(&mut self, parent: NodeId, new_node: NodeId, reference: NodeId) {
        self.revision += 1;
        self.unrendered_if(parent);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(parent) || !self.has(new_node) { return; }
        if self.would_create_cycle(parent, new_node) { return; }
        self.unlink(new_node);
        // `reference` is a child of `parent` iff its parent pointer says so (the old code's
        // `position(reference)` lookup). If it is, splice new_node immediately before it; if not
        // (including reference == new_node, now detached), append as the last child.
        // `reference` may be a null/absent marker encoded as an out-of-range sentinel
        // (e.g. `u32::MAX` from `insertBefore(n, null)`, which the DOM spec defines as "append").
        // Bounds-check before indexing so the sentinel falls through to the append branch instead of
        // panicking (the old `position(reference)` lookup tolerated a missing reference).
        if (reference as usize) < self.nodes.len() && self.at(reference).parent == Some(parent) {
            let prev = self.at(reference).prev_sibling;
            {
                let n = &mut self.nodes[new_node as usize];
                n.parent = Some(parent);
                n.prev_sibling = prev;
                n.next_sibling = Some(reference);
            }
            self.forget_roots();
            self.nodes[reference as usize].prev_sibling = Some(new_node);
            match prev {
                Some(p) => self.nodes[p as usize].next_sibling = Some(new_node),
                None => self.nodes[parent as usize].first_child = Some(new_node),
            }
            self.journal.push(crate::journal::Change::Inserted { parent, node: new_node });
        } else {
            self.link_as_last_child(parent, new_node);
        }
    }

    /// Detach `child` from its parent, if any: repair the neighbours' sibling links and the
    /// parent's `first_child`/`last_child`, then clear `child`'s own `parent`/`prev_sibling`/
    /// `next_sibling`. A node that is already fully detached is left untouched. The sibling-link
    /// counterpart of the old `parent.children.retain(|c| c != child)`.
    pub(crate) fn unlink(&mut self, child: NodeId) {
        let (parent, prev, next) = {
            let n = self.at(child);
            (n.parent, n.prev_sibling, n.next_sibling)
        };
        if parent.is_none() && prev.is_none() && next.is_none() {
            return;
        }
        if !self.opaque_by_element.is_empty() {
            self.opaque_ranges_subtree_removed(child);
        }
        // THE CHAIN PRIMITIVES MOVE THE REVISION THEMSELVES. A live collection
        // caches on `structure_revision`, so every path that changes a child chain has
        // to move it -- and `set_inner_html` changed chains through this and `link_as_last_child`
        // without a bump of its own: `d.innerHTML = "<i>y</i>"` left `d.childNodes[0]` answering the
        // OLD child. lyft.com's markdown renderer parses each card through one reused `<template>`,
        // read every card as the first one it parsed, and React threw hydration error #418.
        self.revision += 1;
        match parent {
            Some(p) => self.count_render_at(p),
            None => self.render_skips += 1,
        }
        match prev {
            Some(p) => self.nodes[p as usize].next_sibling = next,
            None => {
                // No previous sibling normally means `child` is the parent's first child, so the
                // head moves to `next`. But a node can legitimately carry a parent pointer WITHOUT
                // being in that parent's chain: `replace_child` documents leaving `new_node` with
                // `parent = Some(parent)` yet unlinked when `old_node` was not a child. Rewriting
                // the head from such a phantom node would drop the parent's real children on the
                // floor (they keep their parent pointers, so they stay `is_in_document` while
                // vanishing from `children()` — silent corruption). Only move the head when this
                // node really is the head.
                if let Some(par) = parent {
                    if self.at(par).first_child == Some(child) {
                        self.nodes[par as usize].first_child = next;
                    }
                }
            }
        }
        match next {
            Some(nx) => self.nodes[nx as usize].prev_sibling = prev,
            None => {
                // Symmetric guard to the head case above.
                if let Some(par) = parent {
                    if self.at(par).last_child == Some(child) {
                        self.nodes[par as usize].last_child = prev;
                    }
                }
            }
        }
        let n = &mut self.nodes[child as usize];
        n.parent = None;
        n.prev_sibling = None;
        n.next_sibling = None;
        self.forget_roots();
        if let Some(parent) = parent {
            self.journal.push(crate::journal::Change::Removed { parent, node: child, next });
        }
    }

    /// Link `child` — which MUST already be detached (parent/prev/next all `None`) — as the new
    /// last child of `parent`. The sibling-link counterpart of the old `parent.children.push(child)`.
    pub(crate) fn link_as_last_child(&mut self, parent: NodeId, child: NodeId) {
        // Moves the revision for the reason `unlink` gives.
        self.revision += 1;
        self.unrendered_if(parent);
        let old_last = self.at(parent).last_child;
        {
            let n = &mut self.nodes[child as usize];
            n.parent = Some(parent);
            n.prev_sibling = old_last;
            n.next_sibling = None;
        }
        self.forget_roots();
        match old_last {
            Some(l) => self.nodes[l as usize].next_sibling = Some(child),
            None => self.nodes[parent as usize].first_child = Some(child),
        }
        self.nodes[parent as usize].last_child = Some(child);
        self.journal.push(crate::journal::Change::Inserted { parent, node: child });
    }

    pub(crate) fn push(&mut self, kind: NodeKind, parent: Option<NodeId>) -> NodeId {
        let id = self.nodes.len() as NodeId;
        // Every node-construction path (create_element, HTML/XML parse, clone_node, the
        // innerHTML/insertAdjacentHTML fragment-copy path) funnels through here, so this is the one
        // place that must catch an element arriving already carrying an `id` attribute -- see
        // `index_id`.
        let classes = match &kind {
            NodeKind::Element { attrs, .. } => attrs.iter().find(|(k, _)| k == "class").and_then(|(_, v)| class_tokens(v)),
            _ => None,
        };
        if matches!(&kind, NodeKind::Element { name, .. } if &**name == "base") {
            self.bases_made += 1;
        }
        let (carried_id, carried_name) = match &kind {
            NodeKind::Element { attrs, .. } => (
                attrs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.clone()),
                attrs.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone()),
            ),
            _ => (None, None),
        };
        // The new node starts fully detached; if a `parent` was given, link it as that parent's
        // last child (the callers used to do a separate `parent.children.push(id)` right after).
        self.nodes.push(Node {
            kind,
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            classes,
            style_writes: 0,
        });
        if let Some(p) = parent {
            self.link_as_last_child(p, id);
        }
        if let Some(v) = carried_id {
            self.index_id(id, None, Some(&v));
        }
        if let Some(v) = carried_name {
            self.index_name(id, None, Some(&v));
        }
        id
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Has this arena ever made an element named `base`? Every node is made through [`Dom::push`], which counts them,
    /// and an element's name never changes: `false` means no document of the arena has a `<base>`, whatever the tree
    /// does -- the document base URL is the document's URL without a look at the tree.
    pub fn has_made_base(&self) -> bool {
        self.bases_made > 0
    }

    pub fn node(&self, id: NodeId) -> &Node {
        self.at(id)
    }

    /// Read a node by id, tolerating an id that names no node.
    ///
    /// The arena is APPEND-ONLY -- ids are never reused, so an id once handed to JS stays valid
    /// forever. What can miss is `u32::MAX`, which the binding layer above uses as a sentinel for TWO
    /// unrelated things:
    ///
    /// - a null argument, e.g. `insertBefore(n, null)`, which the DOM defines as "append"; and
    /// - the DOCUMENT (`webapi`'s `ON_DOCUMENT_ID`) -- `arg_node_id`/`range_point` both hand back
    ///   `u32::MAX` for a `JsDocument` rather than the document's actual arena id.
    ///
    /// Indexing the arena on either used to panic, and a panic raised inside a v8 callback frame
    /// CANNOT UNWIND: it does not surface as a JS exception, it aborts the process ("fatal runtime
    /// error: failed to initiate panic").
    ///
    /// This is the floor, not the fix. Neither meaning of the sentinel belongs to the arena, so
    /// neither can be answered correctly here -- a miss reads as a detached, empty, parentless node,
    /// which stops the abort and is neutral, but for the document it is merely NOT WRONG ENOUGH TO
    /// CRASH. The convention lives in `webapi`, so that is where it is decoded: `insert_before`
    /// special-cases the null sentinel itself, and `core::dom::shared::{node_type_of, children_of}`
    /// answer for the document before the id ever reaches the arena.
    #[inline]
    pub(crate) fn at(&self, id: NodeId) -> &Node {
        match self.nodes.get(id as usize) {
            Some(n) => n,
            None => detached_node(),
        }
    }

    /// True when `id` names a real arena node -- the entry-point check for the mutating methods
    /// below, which cannot fall back on a read-only stand-in the way [`Dom::at`] does.
    #[inline]
    pub(crate) fn has(&self, id: NodeId) -> bool {
        (id as usize) < self.nodes.len()
    }

    /// A node's children in order, materialised by walking `first_child` → `next_sibling`.
    /// (Sibling links replaced the old per-node `Vec<NodeId>`, so this now allocates; prefer a
    /// direct `first_child`/`next_sibling` walk on hot paths that only need to iterate.)
    pub fn children(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut c = self.at(id).first_child;
        while let Some(cid) = c {
            out.push(cid);
            // Defense in depth: a valid sibling chain is at most nodes.len() long; the insertion
            // guards keep it acyclic, but never spin forever on a malformed one.
            if out.len() > self.nodes.len() {
                break;
            }
            c = self.at(cid).next_sibling;
        }
        out
    }

    /// How many times this tree has been mutated. See the field's note: it is a dirty *counter*,
    /// not a description of what moved.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What changed since a position a pass read up to. See [`crate::journal`].
    pub fn journal(&self) -> &crate::journal::Journal {
        &self.journal
    }

    /// [`Dom::revision`] without ANY attribute write: it moves on every tree and text mutation and on
    /// nothing an attribute does. A collection that depends only on the tree (`children`,
    /// `getElementsByTagName`, `childNodes`) keys its cache on this, so a loop that sets an attribute
    /// on each child it walks stays linear (it was quadratic, 239 ms for 10k children).
    pub fn structure_revision(&self) -> u64 {
        self.revision - self.attribute_writes
    }

    /// [`Dom::revision`] without the writes to `style` attributes and to `data-*` ones.
    ///
    /// Everything selector matching reads -- the tree's shape, every other attribute, text (for
    /// `:empty`), the hover chain -- moves this counter, exactly as it moves `revision`. A `style` or
    /// `data-*` attribute write does not, because no selector reads such an attribute unless it NAMES it
    /// (`[style*=…]`, `[data-state=open]`), which a caller checks for itself
    /// ([`Selector::mentions_unrevised_attribute`]), and neither carries a presentational hint or moves a
    /// pseudo-class. It is what lets a style pass keep an element's matched rules across `el.style.x = …`
    /// and `el.dataset.i = …`, the writes a layout-reading loop makes, instead of re-matching the whole
    /// document for them (a web-component page: 300 `setAttribute("data-i")` + `getComputedStyle` reads,
    /// 487 ms against 285 ms for the same loop over `style`, `P-shadow-sheets`).
    ///
    /// [`Selector::mentions_unrevised_attribute`]: crate::Selector::mentions_unrevised_attribute
    pub fn match_revision(&self) -> u64 {
        self.revision - self.unrevised_attribute_writes
    }

    /// The writes [`Dom::match_revision`] leaves out, by kind: `(style, data-*)`. A cache of matched
    /// rules kept across them re-matches only the rules that name a kind written since it was filled:
    /// www.mapbox.com rewrites `style` on every frame and has `[data-*]` rules, and matching
    /// those again for every element on every style write cost it 2-3x its load.
    /// How many times `id`'s `style` attribute has been written or removed: the attribute's value is the
    /// one it had while this number is the same (`set_attribute` and `remove_attribute` are the only
    /// writers of a made element's attributes; a write of the text it holds is not counted). A cache
    /// keyed on an element's inline style compares this, not the text.
    pub fn style_writes_of(&self, id: NodeId) -> u32 {
        if self.has(id) { self.nodes[id as usize].style_writes } else { 0 }
    }

    pub fn unrevised_writes(&self) -> (u64, u64) {
        (self.unrevised_attribute_writes - self.data_attribute_writes, self.data_attribute_writes)
    }

    /// [`Dom::revision`] without the bumps no document can see: making a node, and changing
    /// one whose tree has no document at its root -- a subtree built before it is inserted, a
    /// fragment, a template's content. Inserting such a tree moves this like any other change to a
    /// document's tree (the PARENT's tree is asked), and so does taking a node out of one (`unlink`
    /// asks where the node WAS). A document's layout is keyed on this: greensock.com laid its page
    /// out 660 times before `load`, each after a DOM change, and most of those changes were to
    /// nodes no document held yet.
    ///
    /// Over-counting is a needless layout and under-counting a stale one, so a tree counts as a
    /// document's whenever its root is a Document or a shadow root, whatever the host is in, and the
    /// mutators this does not name (form state, the hover chain) count every change.
    pub fn render_revision(&self) -> u64 {
        self.revision - self.render_skips
    }

    /// [`Dom::render_revision`] less the changes to documents other than the page's: what the
    /// PAGE's layout is keyed on. A frame document lives in this arena too, and a frame script that changed
    /// its own document and then asked a style -- which resolves against the frame's viewport, its element's
    /// box in the page's layout -- relaid the whole page out each time, and so did every page-side geometry
    /// query after a frame's change: www.highcharts.com (eight frames, replay bank) took 326 G instructions to
    /// its load event, and past 300 s once F27 resolved frame styles against the frame's viewport; 12.9 G
    /// with this. A change in a shadow tree counts for the document its host is in.
    pub fn page_render_revision(&self) -> u64 {
        self.render_revision() - self.off_page_renders
    }

    /// The changes the document rooted at `root` has seen: the page's is
    /// [`Dom::page_render_revision`]; any other document's (a frame's, a `DOMParser` result's) counts only its
    /// own tree's changes -- a shadow tree's counting for the document its host is in. A frame's layout is keyed
    /// on this with its viewport (its element's box) and the sheets: keyed on the arena-wide count, every write
    /// in the page or in any other frame relaid each frame out at its next style or geometry read.
    pub fn document_render_revision(&self, root: NodeId) -> u64 {
        if root == self.root {
            self.page_render_revision()
        } else {
            self.doc_renders.get(&root).copied().unwrap_or(0)
        }
    }

    /// This arena's process-unique identity (see the field).
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// Where a change at `id` is seen: the root of the document that renders it -- a shadow
    /// tree counting for the document its host is in -- or `None` when no document renders `id`'s tree.
    fn render_home(&self, id: NodeId) -> Option<NodeId> {
        if !self.has(id) {
            return None;
        }
        let mut cur = id;
        // Bounded against a malformed cycle, as every climb here is: each hop from a shadow root to its host
        // moves to a strictly different tree, and a light climb visits each node at most once.
        for _ in 0..self.nodes.len() {
            match self.nodes[cur as usize].parent {
                Some(p) => cur = p,
                None => {
                    if !matches!(self.nodes[cur as usize].kind, NodeKind::Document) {
                        return None;
                    }
                    match self.shadow_host_of(cur) {
                        Some(host) => cur = host,
                        None => return Some(cur),
                    }
                }
            }
        }
        None
    }

    /// Count the revision bump just made at `id` (its tree's node): skipped when no document sees it, and
    /// an off-page render when a document other than the page's does.
    fn count_render_at(&mut self, id: NodeId) {
        match self.render_home(id) {
            None => self.render_skips += 1,
            Some(root) => self.count_off_page(root),
        }
    }

    /// A change the document rooted at `root` sees: nothing more for the page's, an off-page render counted
    /// for its own document otherwise.
    fn count_off_page(&mut self, root: NodeId) {
        if root != self.root {
            self.off_page_renders += 1;
            *self.doc_renders.entry(root).or_default() += 1;
        }
    }

    /// Count an attribute write's [`RenderChange`] for [`Dom::render_revision`]: nothing when
    /// it left the value as it was or `id`'s tree is no document's; the transform family and `opacity`
    /// alone as [`Dom::inert_style_writes`]. Where `id` is, is asked only of a write that changed a value.
    fn count_render_change(&mut self, id: NodeId, change: RenderChange) {
        if matches!(change, RenderChange::Nothing) {
            self.render_skips += 1;
            return;
        }
        match self.render_home(id) {
            None => self.render_skips += 1,
            // Another document's write moves its own count and the global one; the page's geometry leaves it out.
            Some(root) if root != self.root => self.count_off_page(root),
            Some(_) if matches!(change, RenderChange::Inert) => self.inert_style_writes += 1,
            Some(_) => {}
        }
    }

    /// How many [`Dom::render_revision`] steps were `style` rewrites that changed only the transform
    /// family and `opacity` (see [`style_change_is_inert`]), or the colours, outlines and shadows only paint
    /// reads. korvus-page lays out, hit-tests and
    /// resolves without either -- "transforms and filters ... this engine has neither in layout"
    /// (`layout/resolved.rs`), and hit testing is document order, not stacking contexts
    /// (`layout/hit.rs`) -- so a document's GEOMETRY may leave these out, unless a selector reads the
    /// `style` attribute itself (`[style*=...]`), which only the page can tell.
    pub fn inert_style_writes(&self) -> u64 {
        self.inert_style_writes
    }

    /// Count the revision bump just made as one no document can see, when `id`'s tree is not one.
    pub(crate) fn unrendered_if(&mut self, id: NodeId) {
        self.count_render_at(id);
    }

    /// The hover chain: the element under the pointer and its element ancestors, outermost first.
    /// Empty when the pointer is over no box. See the field's note for why it lives here.
    pub fn hover_chain(&self) -> &[NodeId] {
        &self.hover_chain
    }

    /// Does `:hover` match `id`? The one question `Element.matches`, `querySelectorAll` and the
    /// cascade all ask, so that they cannot answer it differently.
    ///
    /// A linear scan rather than a set: a hover chain is the depth of the document (a handful of
    /// entries, a few dozen on the worst real page), so the scan is a cache-resident walk of a
    /// short `u32` array and costs less than hashing the key would.
    pub fn is_hovered(&self, id: NodeId) -> bool {
        self.hover_chain.contains(&id)
    }

    /// The focused element, if one is (the body otherwise).
    pub fn focused(&self) -> Option<NodeId> {
        self.focus.map(|(n, _)| n)
    }

    /// `:focus`.
    pub fn is_focused(&self, id: NodeId) -> bool {
        self.focused() == Some(id)
    }

    /// `:focus-visible`: focused, and the focus shown (a text field always; anything else unless a pointer put it there).
    pub fn is_focus_visible(&self, id: NodeId) -> bool {
        self.focus == Some((id, true))
    }

    /// `:focus-within`: the focused element is `id` or inside it (across shadow roots, as Chrome's is).
    pub fn has_focus_within(&self, id: NodeId) -> bool {
        let mut at = self.focused();
        while let Some(n) = at {
            if n == id {
                return true;
            }
            at = self.parent(n).or_else(|| self.shadow_host_of(n));
        }
        false
    }

    /// Move the focus ring; `true` when it changed. A change restyles the document (a focus moves seldom, and `:focus`,
    /// `:focus-within` and `:has(:focus)` reach anywhere): `:focus` never matched here, so no focus style ever applied
    /// -- Naver's search box shows its placeholder only while focused.
    pub fn set_focus(&mut self, focus: Option<(NodeId, bool)>) -> bool {
        if self.focus == focus {
            return false;
        }
        self.focus = focus;
        self.revision += 1;
        self.journal.push(crate::journal::Change::Other { node: None, what: "focus" });
        true
    }

    /// Replace the hover chain; `true` when it actually changed.
    ///
    /// The revision bump is what makes `getComputedStyle` and `getBoundingClientRect` see the new
    /// state: `Page::layout_fingerprint` hashes `revision`, so the cached layout and every derived
    /// style are dropped by the same mechanism a DOM mutation uses. It is bumped ONLY on a real
    /// change, because a pointer moving inside one element is the common case and re-laying out the
    /// document for it would make every mouse move quadratic -- Chrome fires a bare `mousemove`
    /// there and restyles nothing, which is exactly this branch.
    pub fn set_hover_chain(&mut self, chain: Vec<NodeId>) -> bool {
        if self.hover_chain == chain {
            return false;
        }
        self.revision += 1;
        let old = std::mem::replace(&mut self.hover_chain, chain);
        self.journal.push_hover(&old);
        true
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Tag name for an element node, else `None`.
    pub fn tag_name(&self, id: NodeId) -> Option<&str> {
        match &self.at(id).kind {
            NodeKind::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Whether `id`'s document root is an XML document (case-preserving names).
    pub fn is_in_xml(&self, id: NodeId) -> bool {
        self.xml_roots.contains(&self.root_of(id))
    }

    /// `Element.tagName`/`nodeName` for elements: the qualified name is uppercased only when the
    /// element is in the HTML namespace AND its document is an HTML document. A foreign (SVG/MathML,
    /// null-namespace) element keeps its case, which is what makes a parsed `<clipPath>` read back
    /// `clipPath` rather than `CLIPPATH`.
    pub fn element_tag_name(&self, id: NodeId) -> Option<String> {
        match &self.at(id).kind {
            NodeKind::Element { name, ns, .. } => {
                Some(if self.is_in_xml(id) || !ns.is_html() { name.to_string() } else { name.to_uppercase() })
            }
            _ => None,
        }
    }

    /// Create an empty HTML Document root: no doctype, no `<html>`. Backs a document's inert
    /// "template contents owner document", which Chrome creates empty.
    pub fn create_html_document(&mut self) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        self.push(NodeKind::Document, None)
    }

    /// Create an empty XML Document root (marks the subtree XML). Backs `createDocument`.
    pub fn create_xml_document(&mut self) -> NodeId {
        self.revision += 1;
        self.render_skips += 1;
        let doc = self.push(NodeKind::Document, None);
        self.xml_roots.insert(doc);
        doc
    }

    /// Append a child (used by createDocument to attach doctype/root). Returns nothing.
    pub fn xml_append(&mut self, parent: NodeId, child: NodeId) {
        self.revision += 1;
        self.unrendered_if(parent);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(parent) || !self.has(child) { return; }
        self.append_child(parent, child);
    }

    /// Attribute value by name for an element node.
    pub fn get_attribute(&self, id: NodeId, key: &str) -> Option<&str> {
        match &self.at(id).kind {
            NodeKind::Element { attrs, .. } => {
                attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
            }
            _ => None,
        }
    }

    /// The DOM's "descendant text content" (WHATWG DOM §4.4), for `id` and its subtree: the Text
    /// nodes, concatenated in tree order.
    ///
    /// A **CDATASection IS a Text node** -- `interface CDATASection : Text` -- so its data counts
    /// here exactly as a Text child's does. It did not, which is why a `<p>` holding
    /// `<![CDATA[1234]]><![CDATA[5678]]>` and the text `9012` read `"9012"` and not
    /// `"123456789012"` (WPT dom/nodes/Node-textContent, `paras[5]`). A Comment and a
    /// ProcessingInstruction are CharacterData but NOT Text, so neither contributes to an
    /// ancestor's text -- that is what keeps an element holding `<!-- abc -->`, `\tDEF\t` and
    /// `<?x ghi ?>` reading `"\tDEF\t"`.
    ///
    /// This is descendant-OR-SELF: passed a Text or CDATASection node it answers that node's own
    /// data, which is what the callers reading a `<style>`/`<textarea>` child expect. The full
    /// `textContent` GETTER -- including the nodes whose answer is `null`, and a PI's or a
    /// Comment's own data -- is [`Dom::text_content_value`]; this is the piece of it that walks.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    /// `Node.textContent`'s getter, the WHOLE algorithm (WHATWG DOM §4.4), in ONE place.
    /// `None` is JS `null`.
    ///
    /// * DocumentFragment, Element -- the descendant text content ([`Dom::text_content`]);
    /// * CharacterData (Text, CDATASection, ProcessingInstruction, Comment) -- the node's own data;
    /// * **any other node -- `null`**: a Document and a DocumentType have no textContent at all.
    ///
    /// The SETTER (`core/dom/text.rs::cb_text_content_set`) has carried this per-nodeType switch
    /// all along (`if matches!(nt, 3 | 4 | 7 | 8)`); the getter was one arm of it -- a
    /// ProcessingInstruction special case -- and answered `""` for a Comment, a CDATASection and a
    /// DocumentType, where Chrome answers the data, the data and `null`.
    ///
    /// A shadow root reuses `NodeKind::Document` in the arena (see `attach_shadow`) while BEING a
    /// DocumentFragment to script, so it is routed to the fragment arm by [`Dom::is_shadow_root`]
    /// and not to the `null` one.
    pub fn text_content_value(&self, id: NodeId) -> Option<String> {
        match &self.at(id).kind {
            NodeKind::Element { .. } | NodeKind::Fragment => Some(self.text_content(id)),
            NodeKind::Text(d) | NodeKind::Comment(d) | NodeKind::CData(d) => Some(d.clone()),
            NodeKind::ProcessingInstruction(pi) => Some(pi.data.clone()),
            NodeKind::Doctype { .. } => None,
            NodeKind::Document => self.is_shadow_root(id).then(|| self.text_content(id)),
        }
    }

    /// Is `id` a **Text node**? Text(3) and CDATASection(4) are the two Text kinds --
    /// `interface CDATASection : Text` -- so both answer true; Comment(8) and
    /// ProcessingInstruction(7) are CharacterData but NOT Text, and answer false.
    ///
    /// The same question [`Dom::collect_text`] below asks, and the one a caller collecting a node's
    /// **child text content** must ask. Spelling it `node_type(c) == 3` is the recurring form of the
    /// DOM2.1 defect: it names one of the two kinds and silently drops the other.
    pub fn is_text_node(&self, id: NodeId) -> bool {
        matches!(self.at(id).kind, NodeKind::Text(_) | NodeKind::CData(_))
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        // Text(3) and CDATASection(4) -- the two Text kinds. Comment(8) and
        // ProcessingInstruction(7) are CharacterData but not Text, and contribute nothing.
        match &self.at(id).kind {
            NodeKind::Text(t) | NodeKind::CData(t) => out.push_str(t),
            _ => {}
        }
        let mut c = self.at(id).first_child;
        while let Some(cid) = c {
            self.collect_text(cid, out);
            c = self.at(cid).next_sibling;
        }
    }

    /// Set (or add) an attribute on an element node.
    pub fn set_attribute(&mut self, id: NodeId, key: &str, value: &str) {
        self.revision += 1;
        self.attribute_writes += 1;
        if unrevised_attribute(key) {
            self.unrevised_attribute_writes += 1;
            self.data_attribute_writes += (key != "style") as u64;
        }
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        // the `nonce` attribute's change steps as Blink runs them (`HTMLElement::OnNonceAttrChanged`, SVG's and
        // MathML's alike): a non-empty value becomes the slot -- here, the slot follows the attribute again --
        // and an EMPTY one leaves the slot as it was (hiding a nonce is exactly that write)
        if key == "nonce" {
            if value.is_empty() {
                let kept: Box<str> = self.nonce(id).into();
                self.nonce_slots.insert(id, kept);
            } else {
                self.nonce_slots.remove(&id);
            }
        }
        // The text the attribute already holds, written again, changes no declaration: Chrome 152 keeps the element's
        // block through `setAttribute('style', el.getAttribute('style'))` -- `setAttributeNS`, `Attr.value` and `STYLE`
        // alike, any number of times -- with the order the setters left and the values at the precision the page wrote
        // them (`scale(1.000998)` still lays out 1000.998px wide, not the attribute's 1001), and a clone made then
        // copies it (`f4same.html`, 2026-10-23). Counted as a write, it dropped the precise text and every cache keyed
        // on the count, and the element read the attribute's parse: 1001px, `margin-top` before `margin-left`.
        if key == "style" && self.get_attribute(id, "style") != Some(value) {
            let n = &mut self.nodes[id as usize];
            n.style_writes = n.style_writes.wrapping_add(1);
            self.precise_styles.remove(&id);
        }
        if key == "transform" {
            let n = self.transform_writes.entry(id).or_insert(0);
            *n = n.wrapping_add(1);
        }
        // Captured from inside the same attrs scan `set_attribute` already did (no extra pass):
        // `Some(old)` only when this is an update of an *existing* "id" attribute, `None` for a
        // fresh add (or for any non-"id" key, or a non-Element node) -- `touched` distinguishes the
        // latter two so a no-op call (e.g. set_attribute on a Text node) never reaches `index_id`.
        let mut old_id: Option<String> = None;
        let mut old_name: Option<String> = None;
        let mut touched = false;
        // What the write changes for a document, from the value it replaces -- which this
        // scan finds anyway. A write to a node that is not an element changes nothing.
        let mut change = RenderChange::Nothing;
        if let NodeKind::Element { attrs, .. } = &mut self.nodes[id as usize].kind {
            touched = true;
            if let Some(a) = attrs.iter_mut().find(|(k, _)| k == key) {
                if key == "id" { old_id = Some(a.1.clone()); }
                if key == "name" { old_name = Some(a.1.clone()); }
                change = RenderChange::of(key, Some(&a.1), Some(value));
                let was = std::mem::replace(&mut a.1, value.to_string());
                if !matches!(change, RenderChange::Nothing) {
                    self.journal.push_attribute(id, key, || matches!(key, "class" | "id").then_some(was));
                }
            } else {
                change = RenderChange::of(key, None, Some(value));
                attrs.push((key.to_string(), value.to_string()));
                self.journal.push_attribute(id, key, || None);
            }
        }
        self.count_render_change(id, change);
        if touched && key == "id" {
            self.index_id(id, old_id.as_deref(), Some(value));
        }
        if touched && key == "name" {
            self.index_name(id, old_name.as_deref(), Some(value));
        }
        if touched && key == "class" {
            self.nodes[id as usize].classes = class_tokens(value);
        }
        self.control_attribute_changed(id, key);
    }

    /// Write an element's inline style as the CSSOM does (`CSSStyleDeclaration`'s setters): the `style` attribute
    /// gets `serialized` -- the declarations as CSSOM serializes them, a number to six significant digits --
    /// while the cascade reads `precise`, the same declarations as the page wrote them. Blink keeps the parsed
    /// values and only writes the attribute from them: Chrome 150 lays an SVG text under `style.transform =
    /// "scale(1.000998)"` out at 1.000998 while `style.transform` reads `scale(1.001)`
    /// (the challenge's font probe measures exactly that text). A write of another text to the attribute ends it.
    ///
    /// The precise text is kept stamped with the element's [`Dom::style_writes_of`] after this write's last bump:
    /// every later write of another text to the attribute, or its removal, moves that count (and drops the entry),
    /// so a matching stamp says the attribute still holds this write's serialization -- what comparing the two texts
    /// said, without copying the serialized one here or comparing it on every cascade read.
    pub fn set_inline_style(&mut self, id: NodeId, serialized: &str, precise: String) {
        let attribute_unchanged = self.get_attribute(id, "style") == Some(serialized);
        // read only when the attribute write below changes nothing
        let before = if attribute_unchanged { self.style_for_cascade(id).map(str::to_string) } else { None };
        self.set_attribute(id, "style", serialized);
        if !self.has(id) || !matches!(self.nodes[id as usize].kind, NodeKind::Element { .. }) {
            return;
        }
        // A change only the precise text shows still restyles: the attribute write above saw none.
        if attribute_unchanged && before.as_deref() != Some(precise.as_str()) {
            self.revision += 1;
            self.attribute_writes += 1;
            self.unrevised_attribute_writes += 1;
            let n = &mut self.nodes[id as usize];
            n.style_writes = n.style_writes.wrapping_add(1);
            self.journal.push_attribute(id, "style", || None);
            self.count_render_change(id, RenderChange::of("style", before.as_deref(), Some(&precise)));
        }
        if precise != serialized {
            let stamp = self.nodes[id as usize].style_writes;
            self.precise_styles.insert(id, (stamp, precise));
        } else {
            // the attribute write above keeps an entry when the attribute's text stays: one the count moved past
            self.precise_styles.remove(&id);
        }
    }

    /// The `style` attribute as the cascade reads it: the full-precision declarations the CSSOM last wrote
    /// ([`Dom::set_inline_style`]) while the attribute still holds their serialization, else the attribute.
    pub fn style_for_cascade(&self, id: NodeId) -> Option<&str> {
        let attr = self.get_attribute(id, "style")?;
        match self.precise_styles.get(&id) {
            Some((stamp, precise)) if *stamp == self.style_writes_of(id) => Some(precise),
            _ => Some(attr),
        }
    }

    /// How many times `id`'s `transform` attribute has been written or removed (0 for one only the parser set).
    pub fn transform_writes(&self, id: NodeId) -> u32 {
        self.transform_writes.get(&id).copied().unwrap_or(0)
    }

    /// Remove an attribute from an element node.
    pub fn remove_attribute(&mut self, id: NodeId, key: &str) {
        self.revision += 1;
        self.attribute_writes += 1;
        if unrevised_attribute(key) {
            self.unrevised_attribute_writes += 1;
            self.data_attribute_writes += (key != "style") as u64;
        }
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        // removing the `nonce` attribute empties the slot (Blink: a null new value is not the empty atom)
        if key == "nonce" {
            self.nonce_slots.remove(&id);
        }
        if key == "style" {
            let n = &mut self.nodes[id as usize];
            n.style_writes = n.style_writes.wrapping_add(1);
            self.precise_styles.remove(&id);
        }
        if key == "transform" {
            let n = self.transform_writes.entry(id).or_insert(0);
            *n = n.wrapping_add(1);
        }
        let mut old_id: Option<String> = None;
        let mut old_name: Option<String> = None;
        let mut touched = false;
        let mut change = RenderChange::Nothing;
        if let NodeKind::Element { attrs, .. } = &mut self.nodes[id as usize].kind {
            touched = true;
            change = RenderChange::of(key, attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()), None);
            if key == "id" {
                old_id = attrs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.clone());
            }
            if key == "name" {
                old_name = attrs.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone());
            }
            if let Some(i) = attrs.iter().position(|(k, _)| k == key) {
                let (_, was) = attrs.remove(i);
                attrs.retain(|(k, _)| k != key);
                self.journal.push_attribute(id, key, || matches!(key, "class" | "id").then_some(was));
            }
        }
        self.count_render_change(id, change);
        if touched && key == "id" {
            self.index_id(id, old_id.as_deref(), None);
        }
        if touched && key == "name" {
            self.index_name(id, old_name.as_deref(), None);
        }
        if touched && key == "class" {
            self.nodes[id as usize].classes = None;
        }
        self.control_attribute_changed(id, key);
    }

    /// The two content attributes a form control's state follows while it is not dirty.
    fn control_attribute_changed(&mut self, id: NodeId, key: &str) {
        match key {
            "checked" => self.checked_attribute_changed(id),
            "selected" => self.selected_attribute_changed(id),
            "type" => self.opaque_ranges_type_changed(id),
            _ => {}
        }
    }

    /// Update [`Dom::id_index`] for `node`'s `id` attribute changing from `old` to `new` (either
    /// may be absent: creation-with-id / removal). The single choke point every id-attribute
    /// mutation path (`push`'s element-construction hook, `set_attribute`, `remove_attribute`)
    /// must call — see the field doc comment on `id_index` for why structural removal from the
    /// tree does NOT also need a call here.
    fn index_id(&mut self, node: NodeId, old: Option<&str>, new: Option<&str>) {
        reindex(&mut self.id_index, node, old, new);
    }

    /// Update [`Dom::name_index`] for `node`'s `name` attribute changing from `old` to `new`: the
    /// same choke point as `index_id`, for the same three paths.
    fn index_name(&mut self, node: NodeId, old: Option<&str>, new: Option<&str>) {
        reindex(&mut self.name_index, node, old, new);
    }

    /// Replace a node's children with a single text node (like setting `textContent`).
    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        // A ProcessingInstruction's textContent setter writes its data (no child text node).
        if let NodeKind::ProcessingInstruction(pi) = &mut self.nodes[id as usize].kind { pi.data = text.to_string(); return; }
        // Detach the outgoing children (clear their parent AND sibling links, not just unlink them
        // from the parent) so is_in_document()/getElementById no longer reach them.
        let folds = self.journal.fold_children_replaced(id);
        let mut c = self.at(id).first_child;
        let mut old = Vec::new();
        while let Some(cid) = c {
            let next = self.at(cid).next_sibling;
            let n = &mut self.nodes[cid as usize];
            n.parent = None;
            n.prev_sibling = None;
            n.next_sibling = None;
            if !folds {
                old.push(cid);
            }
            c = next;
        }
        self.forget_roots();
        self.nodes[id as usize].first_child = None;
        self.nodes[id as usize].last_child = None;
        if !folds {
            self.journal.push_replaced(id, &old);
        }
        // "string replace all with string": only a NON-empty string becomes a Text node; the empty
        // string adds nothing (so `el.textContent = ""` on an empty element is a true no-op -- no
        // spurious child text node, no MutationObserver record). -- WPT MutationObserver-childList.
        if !text.is_empty() {
            // push(Some(id)) links the new text node as id's (now only) child.
            self.push(NodeKind::Text(text.to_string()), Some(id));
        }
    }

    /// Remove `child` from `parent`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) {
        self.revision += 1;
        self.unrendered_if(parent);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(parent) || !self.has(child) { return; }
        // Only act when `child` is really a child of `parent` (the old code's parent-guard); a
        // normally-linked child is then reachable in the sibling chain, so `unlink` both drops it
        // from the chain and clears its parent pointer.
        if self.at(child).parent == Some(parent) {
            self.unlink(child);
        }
    }

    /// Append `child` to `parent` (detaches from any previous parent's child list).
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.append_child_unhidden(parent, child);
        self.hide_nonces_after_insert(child);
    }

    fn append_child_unhidden(&mut self, parent: NodeId, child: NodeId) {
        self.revision += 1;
        self.unrendered_if(parent);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(parent) || !self.has(child) { return; }
        if self.would_create_cycle(parent, child) { return; }
        self.unlink(child);
        self.link_as_last_child(parent, child);
    }

    /// The element's `[[CryptographicNonce]]` (HTML "nonce attributes"): what the `nonce` IDL
    /// attribute reads -- the attribute's value until script set the slot or the attribute was hidden.
    pub fn nonce(&self, id: NodeId) -> &str {
        match self.nonce_slots.get(&id) {
            Some(s) => s,
            None => self.get_attribute(id, "nonce").unwrap_or(""),
        }
    }

    /// The `nonce` IDL attribute's setter: the slot, never the content attribute (Chrome 150: `s.nonce = "abc"` leaves
    /// `s.attributes` empty).
    pub fn set_nonce(&mut self, id: NodeId, value: &str) {
        if self.has(id) {
            self.nonce_slots.insert(id, value.into());
        }
    }

    /// Mark document `doc` as one whose CSP list holds a header-delivered policy (or not), and hide the nonces of the
    /// elements already connected to it -- the parser's, which are all in place before the first script runs.
    pub fn set_nonce_hiding(&mut self, doc: NodeId, on: bool) {
        if on {
            self.nonce_hiding_docs.insert(doc);
            self.hide_nonces_in(doc);
        } else {
            self.nonce_hiding_docs.remove(&doc);
        }
    }

    /// HTML's steps for an element including HTMLOrSVGElement that becomes browsing-context connected, for `node`'s
    /// connected subtree: under a header-delivered CSP, a non-empty `nonce` attribute is set to "" and the slot keeps
    /// its value (Chrome 150: a parser-inserted `<script nonce=n0nce>` reads `nonce=""` as an attribute, `n0nce`
    /// through `.nonce`).
    fn hide_nonces_after_insert(&mut self, node: NodeId) {
        if self.nonce_hiding_docs.is_empty() || !self.has(node) {
            return;
        }
        let root = self.shadow_including_root(node);
        if self.nonce_hiding_docs.contains(&root) {
            self.hide_nonces_in(node);
        }
    }

    fn hide_nonces_in(&mut self, top: NodeId) {
        let mut todo = vec![top];
        let mut hide = Vec::new();
        while let Some(n) = todo.pop() {
            if matches!(self.element_ns(n), Some(ElementNs::Html | ElementNs::Svg | ElementNs::MathMl))
                && self.get_attribute(n, "nonce").is_some_and(|v| !v.is_empty())
            {
                hide.push(n);
            }
            if let Some(sr) = self.shadow_root_of(n) {
                todo.push(sr);
            }
            let mut c = self.at(n).first_child;
            while let Some(x) = c {
                todo.push(x);
                c = self.at(x).next_sibling;
            }
        }
        // Blink's `Element::HideNonce`: the attribute set to "", which leaves the slot holding the value
        for e in hide {
            self.set_attribute(e, "nonce", "");
        }
    }

    /// An element's attributes as `(qualified name, value)` pairs, in order -- borrowed, for the callers that
    /// read every attribute of every element (the cascade's presentational hints). Empty for a non-element.
    pub fn attribute_pairs(&self, id: NodeId) -> &[(String, String)] {
        match &self.at(id).kind {
            NodeKind::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }

    /// Attribute names of an element.
    pub fn attribute_names(&self, id: NodeId) -> Vec<String> {
        match &self.at(id).kind {
            NodeKind::Element { attrs, .. } => attrs.iter().map(|(k, _)| k.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Parent node id.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.at(id).parent
    }

    /// Is `descendant` inside `ancestor` (or equal to it)? Backs `node.contains`.
    pub fn contains(&self, ancestor: NodeId, descendant: NodeId) -> bool {
        let mut cur = Some(descendant);
        // A valid parent chain visits each node at most once; cap the walk at the node count so a
        // malformed cycle can never hang this predicate (which the insertion guards themselves call).
        let mut budget = self.nodes.len();
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            if budget == 0 { break; }
            budget -= 1;
            cur = self.at(c).parent;
        }
        false
    }

    /// `nodeName`.
    pub fn node_name(&self, id: NodeId) -> String {
        match &self.at(id).kind {
            NodeKind::Element { name, ns, .. } => {
                if self.is_in_xml(id) || !ns.is_html() { name.to_string() } else { name.to_uppercase() }
            }
            NodeKind::Text(_) => "#text".to_string(),
            NodeKind::Comment(_) => "#comment".to_string(),
            NodeKind::Document => "#document".to_string(),
            NodeKind::Fragment => "#document-fragment".to_string(),
            NodeKind::Doctype(d) => d.name.clone(),
            NodeKind::ProcessingInstruction(pi) => pi.target.clone(),
            NodeKind::CData(_) => "#cdata-section".to_string(),
        }
    }

    /// `document.doctype` — first Doctype node in the document tree, if any.
    pub fn doctype(&self) -> Option<NodeId> {
        self.doctype_in(self.root)
    }

    /// `document.doctype` for the document tree rooted at `root`.
    pub fn doctype_in(&self, root: NodeId) -> Option<NodeId> {
        (0..self.nodes.len() as NodeId)
            .find(|&n| matches!(self.at(n).kind, NodeKind::Doctype { .. }) && self.is_in_tree(root, n))
    }

    /// `nodeType` — DOM node-type code for the arena node kind.
    pub fn node_type(&self, id: NodeId) -> u32 {
        match &self.at(id).kind {
            NodeKind::Element { .. } => 1,
            NodeKind::Text(_) => 3,
            NodeKind::Comment(_) => 8,
            NodeKind::Document => 9,
            NodeKind::Fragment => 11,
            NodeKind::Doctype { .. } => 10,
            NodeKind::ProcessingInstruction { .. } => 7,
            NodeKind::CData(_) => 4,
        }
    }

    /// `nodeValue` — CharacterData (Text / CDATASection / Comment / ProcessingInstruction) data,
    /// else `None`. A CDATA section IS a CharacterData node in the DOM, so its `data`/`nodeValue`
    /// is its text and its LENGTH is that text's length; answering `None` here reported length 0
    /// for it everywhere (Range offset validation included).
    pub fn node_value(&self, id: NodeId) -> Option<String> {
        match &self.at(id).kind {
            NodeKind::Text(t) | NodeKind::Comment(t) | NodeKind::CData(t) => Some(t.clone()),
            NodeKind::ProcessingInstruction(pi) => Some(pi.data.clone()),
            _ => None,
        }
    }

    /// Set a CharacterData node's data (backs `nodeValue`/`CharacterData.data` setters).
    pub fn set_node_value(&mut self, id: NodeId, value: &str) {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        if matches!(self.nodes[id as usize].kind, NodeKind::Text(_) | NodeKind::CData(_)) {
            self.journal.push_text(id);
        }
        match &mut self.nodes[id as usize].kind {
            NodeKind::Text(t) | NodeKind::Comment(t) | NodeKind::CData(t) => *t = value.to_string(),
            NodeKind::ProcessingInstruction(pi) => pi.data = value.to_string(),
            _ => {}
        }
    }

    /// CharacterData substring/mutation ops. Offsets/counts are UTF-16 code-unit indices (DOM/JS
    /// semantics), so the data is round-tripped through UTF-16 to slice correctly for non-ASCII.
    pub fn substring_data(&self, id: NodeId, offset: usize, count: usize) -> String {
        let u: Vec<u16> = self.node_value(id).unwrap_or_default().encode_utf16().collect();
        let start = offset.min(u.len());
        let end = offset.saturating_add(count).min(u.len());
        String::from_utf16_lossy(&u[start..end.max(start)])
    }
    pub fn append_data(&mut self, id: NodeId, s: &str) {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        let mut d = self.node_value(id).unwrap_or_default();
        d.push_str(s);
        self.set_node_value(id, &d);
    }
    pub fn insert_data(&mut self, id: NodeId, offset: usize, s: &str) {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        let u: Vec<u16> = self.node_value(id).unwrap_or_default().encode_utf16().collect();
        let o = offset.min(u.len());
        let mut r = String::from_utf16_lossy(&u[..o]);
        r.push_str(s);
        r.push_str(&String::from_utf16_lossy(&u[o..]));
        self.set_node_value(id, &r);
    }
    pub fn delete_data(&mut self, id: NodeId, offset: usize, count: usize) {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        let u: Vec<u16> = self.node_value(id).unwrap_or_default().encode_utf16().collect();
        let o = offset.min(u.len());
        let e = offset.saturating_add(count).min(u.len());
        let mut r = String::from_utf16_lossy(&u[..o]);
        r.push_str(&String::from_utf16_lossy(&u[e.max(o)..]));
        self.set_node_value(id, &r);
    }
    pub fn replace_data(&mut self, id: NodeId, offset: usize, count: usize, s: &str) {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return; }
        let u: Vec<u16> = self.node_value(id).unwrap_or_default().encode_utf16().collect();
        let o = offset.min(u.len());
        let e = offset.saturating_add(count).min(u.len());
        let mut r = String::from_utf16_lossy(&u[..o]);
        r.push_str(s);
        r.push_str(&String::from_utf16_lossy(&u[e.max(o)..]));
        self.set_node_value(id, &r);
    }

    /// `Text.splitText(offset)` — truncate this text node at `offset`, insert a new
    /// text node with the remainder right after it, and return the new node's id.
    pub fn split_text(&mut self, id: NodeId, offset: usize) -> NodeId {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return id; }
        let rest = if let NodeKind::Text(t) = &mut self.nodes[id as usize].kind {
            // `offset` counts UTF-16 code units (the DOM's unit for character data), but the node is
            // stored as a Rust `String`, where `len()` and `split_off()` count BYTES. Splitting any
            // non-ASCII text therefore landed mid-codepoint and `split_off` panicked -- and a panic
            // raised inside a v8 callback frame cannot unwind, so it aborted the process outright
            // ("fatal runtime error: failed to initiate panic"). Convert first, exactly as the
            // sibling substring/insert/delete/replace_data methods above already do.
            let u: Vec<u16> = t.encode_utf16().collect();
            let o = offset.min(u.len());
            let head = String::from_utf16_lossy(&u[..o]);
            let rest = String::from_utf16_lossy(&u[o..]);
            *t = head;
            rest
        } else {
            String::new()
        };
        if matches!(self.nodes[id as usize].kind, NodeKind::Text(_)) {
            self.journal.push_text(id);
        }
        let new_id = self.push(NodeKind::Text(rest), None);
        if let Some(parent) = self.at(id).parent {
            match self.next_sibling(id) {
                Some(next) => self.insert_before(parent, new_id, next),
                None => self.append_child(parent, new_id),
            }
        }
        new_id
    }

    /// `Node.isEqualNode(other)` — deep structural equality (type, name, attributes
    /// regardless of order, and children in order). Text/comment compare their data.
    pub fn is_equal_node(&self, a: NodeId, b: NodeId) -> bool {
        let same_kind = {
            let (na, nb) = (self.at(a), self.at(b));
            match (&na.kind, &nb.kind) {
                (NodeKind::Text(x), NodeKind::Text(y)) => x == y,
                (NodeKind::Comment(x), NodeKind::Comment(y)) => x == y,
                (NodeKind::Document, NodeKind::Document) => true,
                (NodeKind::Fragment, NodeKind::Fragment) => true,
                (NodeKind::Doctype(x), NodeKind::Doctype(y)) => x == y,
                (NodeKind::ProcessingInstruction(x), NodeKind::ProcessingInstruction(y)) => x == y,
                (NodeKind::CData(x), NodeKind::CData(y)) => x == y,
                (
                    NodeKind::Element { name: xn, ns: xns, attrs: xa },
                    NodeKind::Element { name: yn, ns: yns, attrs: ya },
                ) => {
                    // The namespace is part of element equality (DOM "equals": namespace, prefix,
                    // local name, attributes). It lives on the node now, so `isEqualNode` gets that
                    // dimension from the arena instead of a parallel side-map walk.
                    xn == yn
                        && xns == yns
                        && xa.len() == ya.len()
                        && xa.iter().all(|(k, v)| ya.iter().any(|(k2, v2)| k == k2 && v == v2))
                }
                _ => false,
            }
        };
        if !same_kind {
            return false;
        }
        // Compare children in order by walking both sibling chains in lock-step. One chain ending
        // before the other (unequal child counts) fails, matching the old length check + zip.
        let mut ca = self.at(a).first_child;
        let mut cb = self.at(b).first_child;
        loop {
            match (ca, cb) {
                (Some(x), Some(y)) => {
                    if !self.is_equal_node(x, y) {
                        return false;
                    }
                    ca = self.at(x).next_sibling;
                    cb = self.at(y).next_sibling;
                }
                (None, None) => return true,
                _ => return false,
            }
        }
    }

    /// `Node.getRootNode()` — walk parents to the topmost ancestor.
    pub fn root_of(&self, id: NodeId) -> NodeId {
        // A parentless node is its own root -- the climb's answer, and the memo's, since every parent-link
        // write forgets the memo at once. Asked for every document and detached root a query starts
        // from, it skips the memo's borrow and its revision and generation checks.
        if self.at(id).parent.is_none() {
            return id;
        }
        let Ok(mut memo) = self.roots_memo.try_borrow_mut() else { return self.climb_to_root(id) };
        let rev = self.structure_revision();
        if memo.revision != rev {
            memo.revision = rev;
            memo.invalidate();
        }
        if memo.roots.len() < self.nodes.len() {
            // Nodes created since the last ask: a new node changes no other node's root. Amortized:
            // the vector only grows, and a stale slot is told apart by its generation.
            memo.roots.resize(self.nodes.len(), (NodeId::MAX, 0));
        }
        let generation = memo.generation;
        match memo.roots.get(id as usize) {
            Some(&(r, g)) if g == generation && r != NodeId::MAX => r,
            Some(_) => {
                let root = self.climb_to_root(id);
                // Every node on the way up shares the root: the next ask for any of them is O(1).
                let (mut cur, mut budget) = (id, self.nodes.len());
                loop {
                    if let Some(slot) = memo.roots.get_mut(cur as usize) {
                        if *slot == (root, generation) {
                            break;
                        }
                        *slot = (root, generation);
                    }
                    match self.at(cur).parent {
                        Some(p) if budget > 0 => {
                            cur = p;
                            budget -= 1;
                        }
                        _ => break,
                    }
                }
                root
            }
            None => self.climb_to_root(id),
        }
    }

    /// Drop the [`Dom::root_of`] memo: a parent link was just written. The structure revision alone
    /// is not enough -- a mutation bumps it on ENTRY, and a root asked for between the bump and the
    /// relink (a validity check, `is_in_xml`) would be remembered for the tree before the move.
    pub(crate) fn forget_roots(&mut self) {
        self.roots_memo.get_mut().invalidate();
    }

    fn climb_to_root(&self, mut id: NodeId) -> NodeId {
        // Bounded against a malformed cycle: the insertion guards keep the tree acyclic, but a
        // stealth engine must never spin forever on page-supplied DOM — bail after node-count steps.
        let mut budget = self.nodes.len();
        while let Some(p) = self.at(id).parent {
            id = p;
            if budget == 0 { break; }
            budget -= 1;
        }
        id
    }

    /// Whether `id` is attached to the main document tree by *ordinary* (light-tree) parent links
    /// only (root == self.root). Distinguishes detached nodes and DOMParser/createHTMLDocument
    /// subtrees (which have their own roots). A shadow-tree node's light root is its shadow root
    /// (`parent: None`), never the Document, so this is deliberately `false` for shadow content --
    /// which is exactly what keeps the flat-arena document queries (`query_selector*`,
    /// `get_element_by_id`, `get_elements_by_class_name`) from ever reaching into a shadow tree.
    /// The shadow-*including* connectivity that `Node.isConnected` needs lives in
    /// [`Dom::is_connected`]; do not conflate the two.
    pub fn is_in_document(&self, id: NodeId) -> bool {
        self.is_in_tree(self.root, id)
    }

    /// Is `id` attached to the document tree rooted at `root`, by ordinary (light-tree) parent
    /// links only?
    ///
    /// This is [`Dom::is_in_document`] with the root as a PARAMETER rather than baked in, and it is
    /// the whole of what makes a nested browsing context a separate document here. One arena holds
    /// several document roots (the page's, every `<iframe>`'s, every `DOMParser` result's); a
    /// document-level query belongs to exactly one of them, and asking `is_in_document` instead
    /// answered "the page's" for all of them. The shadow caveat on `is_in_document` applies
    /// unchanged: a shadow-tree node's light root is its shadow root, never a Document, so scoped
    /// queries still never reach into a shadow tree.
    pub fn is_in_tree(&self, root: NodeId, id: NodeId) -> bool {
        self.root_of(id) == root
    }

    /// Shadow-including connectivity -- the arena-level backing for `Node.isConnected` (Korvus GS
    /// ②-A2). A node is connected iff its *shadow-including* root is the Document: walk light
    /// parents to the local tree root, and each time that root is a shadow root, hop across the
    /// host<->root edge to the host's tree and keep walking. For a node with no shadow ancestor
    /// this collapses to [`Dom::is_in_document`] (the loop body never runs), so ordinary light-tree
    /// connectivity is unchanged; a node inside the shadow tree of a connected host reports
    /// connected even though `is_in_document` (light-only) reports false.
    ///
    /// Kept SEPARATE from `is_in_document` on purpose: the document-scoped query methods filter on
    /// `is_in_document`, and widening *that* to be shadow-including would break encapsulation by
    /// letting `document.querySelector` match shadow content. `isConnected` (this method) is the
    /// only place that should see across the host edge.
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.shadow_including_root(id) == self.root
    }

    /// The shadow-*including* root of `id`: walk light parents to the local tree root, then each time
    /// that root is a shadow root hop across the host<->root edge to the host's tree and keep walking.
    /// For a node with no shadow ancestor this collapses to [`Dom::root_of`] (the loop body never
    /// runs). This is the shadow-aware root that `getRootNode({composed:true})` returns and that
    /// [`Dom::is_connected`] compares against the Document (Korvus GS ②-A3).
    ///
    /// Kept distinct from `root_of` (light-only): the non-composed `getRootNode()` and every
    /// document-scoped query stay on `root_of`/`is_in_document`, so only the callers that intend to
    /// cross the host edge see across it.
    pub fn shadow_including_root(&self, id: NodeId) -> NodeId {
        let mut r = self.root_of(id);
        // Bounded against a malformed host<->root cycle: each hop moves to a strictly different
        // tree in a well-formed DOM, but never spin forever on page-supplied structure.
        let mut budget = self.nodes.len();
        while self.is_shadow_root(r) {
            match self.shadow_host_of(r) {
                Some(host) => r = self.root_of(host),
                None => break,
            }
            if budget == 0 { break; }
            budget -= 1;
        }
        r
    }

    /// Does element `id`'s class list hold `name` (whose [`string_hash`](crate::select_engine::string_hash)
    /// is `hash`)? Case-sensitively, from the cached tokens: hashes first, bytes only on a hash hit.
    pub(crate) fn has_class_token(&self, id: NodeId, hash: u32, name: &str) -> bool {
        let Some(tokens) = self.at(id).classes.as_ref().map(ClassTokens::as_slice) else { return false };
        if !tokens.iter().any(|t| t.0 == hash) {
            return false;
        }
        let Some(value) = self.get_attribute(id, "class") else { return false };
        tokens.iter().any(|&(h, _, s, e)| h == hash && value.get(s as usize..e as usize) == Some(name))
    }

    /// [`Dom::has_class_token`] without ASCII case, as a quirks document matches classes:
    /// `folded` is [`folded_string_hash`](crate::select_engine::folded_string_hash) of `name`.
    pub(crate) fn has_class_token_folded(&self, id: NodeId, folded: u32, name: &str) -> bool {
        let Some(tokens) = self.at(id).classes.as_ref().map(ClassTokens::as_slice) else { return false };
        if !tokens.iter().any(|t| t.1 == folded) {
            return false;
        }
        let Some(value) = self.get_attribute(id, "class") else { return false };
        tokens
            .iter()
            .any(|&(_, f, s, e)| f == folded && value.get(s as usize..e as usize).is_some_and(|t| t.eq_ignore_ascii_case(name)))
    }

    /// Every element carrying `name="<name>"`, whatever tree it is in (see [`Dom::name_index`]).
    pub fn elements_named(&self, name: &str) -> &[NodeId] {
        self.name_index.get(name).map_or(&[], Vec::as_slice)
    }

    /// Every element carrying `id="<id>"`, whatever tree it is in (see [`Dom::id_index`]).
    pub fn elements_with_id(&self, id: &str) -> &[NodeId] {
        self.id_index.get(id).map_or(&[], Vec::as_slice)
    }

    /// `document.getElementById` — first *in-document* element with this id.
    ///
    /// O(1)-ish via [`Dom::id_index`] instead of the old O(n) full-arena scan: candidates for `id`
    /// come straight from the index, filtered live by `is_in_document` (never stale -- see the
    /// field doc comment), and the minimum `NodeId` among them is returned. That minimum is
    /// exactly what the old `(0..self.nodes.len()).find(...)` scan would have returned too, since
    /// it walked NodeIds in ascending order and stopped at the first match -- so this is a pure
    /// speedup with the identical result, NOT a switch to true document/tree order (which, after
    /// insertBefore-style reordering, can differ from NodeId order; both the old scan and this
    /// index preserve that same NodeId-order quirk on purpose, to keep the result unchanged).
    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.get_element_by_id_in(self.root, id)
    }

    /// `document.getElementById` for the document tree rooted at `root` — the generalisation the
    /// method above is the `root == self.root()` case of. Same index, same NodeId-order tie-break;
    /// only the tree-membership filter is parameterised.
    pub fn get_element_by_id_in(&self, root: NodeId, id: &str) -> Option<NodeId> {
        // The empty string is never in the id-to-element map, so getElementById("") is always null
        // (an element with id="" must not be returned) -- WPT Document-getElementById.
        if id.is_empty() { return None; }
        match self.id_index.get(id) {
            Some(candidates) => candidates.iter().copied().filter(|&n| self.is_in_tree(root, n)).min(),
            None => None,
        }
    }

    /// `NonElementParentNode.getElementById` scoped to the subtree below `root` (a DocumentFragment):
    /// the first descendant element in depth-first pre-order (tree order) whose `id` equals `id`.
    /// The empty string never matches. Unlike the document form this ignores is_in_document, since a
    /// fragment's contents are by definition not in the document. -- WPT DocumentFragment-getElementById.
    ///
    /// Deliberately left as the plain scoped DFS (not routed through [`Dom::id_index`]): it is
    /// already bounded by the subtree's size rather than the whole arena, and reproducing its
    /// *true tree-order* first-match via the index would need a tree-order comparator among
    /// candidates -- meaningfully more moving parts for a method that isn't the O(n)-over-the-whole-
    /// document hazard this task targets. Revisit only if profiling shows fragment-scoped lookups
    /// actually matter.
    pub fn get_element_by_id_in_subtree(&self, root: NodeId, id: &str) -> Option<NodeId> {
        if id.is_empty() { return None; }
        let mut stack: Vec<NodeId> = self.children(root).into_iter().rev().collect();
        while let Some(n) = stack.pop() {
            if self.get_attribute(n, "id") == Some(id) { return Some(n); }
            stack.extend(self.children(n).into_iter().rev());
        }
        None
    }

    /// `Node.normalize()` — merge adjacent text-node children and drop empty ones,
    /// recursively over the subtree rooted at `id`.
    /// `Node.normalize()` — coalesce adjacent text nodes + drop empty ones over the subtree. Returns the
    /// removed text nodes grouped by their (former) parent, so the op layer can queue childList
    /// MutationObserver records for each removal (a `#[fast]` op couldn't reenter JS to notify).
    pub fn normalize(&mut self, id: NodeId) -> Vec<(NodeId, Vec<NodeId>)> {
        self.revision += 1;
        self.unrendered_if(id);
        // Reject a null sentinel before it reaches the arena: see [`Dom::at`]. A mutation is
        // a no-op on a node that does not exist -- never a process abort.
        if !self.has(id) { return Vec::new(); }
        let mut out = Vec::new();
        self.normalize_inner(id, &mut out);
        out
    }
    fn normalize_inner(&mut self, id: NodeId, out: &mut Vec<(NodeId, Vec<NodeId>)>) {
        // Snapshot the children up front (the loop mutates the tree via remove_child).
        let children = self.children(id);
        let mut prev_text: Option<NodeId> = None;
        let mut removed: Vec<NodeId> = Vec::new();
        for c in children {
            match &self.at(c).kind {
                NodeKind::Text(t) => {
                    let t = t.clone();
                    if t.is_empty() {
                        self.remove_child(id, c);
                        removed.push(c);
                    } else if let Some(p) = prev_text {
                        if let NodeKind::Text(pt) = &mut self.nodes[p as usize].kind {
                            pt.push_str(&t);
                        }
                        self.journal.push_text(p);
                        self.remove_child(id, c);
                        removed.push(c);
                    } else {
                        prev_text = Some(c);
                    }
                }
                _ => {
                    prev_text = None;
                    self.normalize_inner(c, out);
                }
            }
        }
        if !removed.is_empty() { out.push((id, removed)); }
    }

    /// First/last child of any node type — a direct sibling-link field read.
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.at(id).first_child
    }
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.at(id).last_child
    }

    /// Next/previous sibling of any node type — a direct sibling-link field read. A detached node
    /// (no parent) has both `None`, matching the old "no parent / not found in list => None".
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.at(id).next_sibling
    }
    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.at(id).prev_sibling
    }

    /// Child *element* ids (skips text/comment).
    pub fn child_elements(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut c = self.at(id).first_child;
        while let Some(cid) = c {
            if matches!(self.at(cid).kind, NodeKind::Element { .. }) {
                out.push(cid);
            }
            c = self.at(cid).next_sibling;
        }
        out
    }

    /// All element descendants of `root` (excluding `root`) in tree order (depth-first pre-order).
    /// Backs the namespace/case-aware getElementsByTagName matching in the collection op.
    pub fn descendant_elements(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.for_each_descendant(root, |n| {
            if matches!(self.at(n).kind, NodeKind::Element { .. }) {
                out.push(n);
            }
        });
        out
    }

    /// Whether `id` is an element node.
    pub fn is_element(&self, id: NodeId) -> bool {
        matches!(self.at(id).kind, NodeKind::Element { .. })
    }

    /// The first descendant of `root`, in tree order, for which `pred` holds.
    pub fn find_descendant(&self, root: NodeId, mut pred: impl FnMut(NodeId) -> bool) -> Option<NodeId> {
        if !self.has(root) {
            return None;
        }
        let mut cur = self.at(root).first_child;
        while let Some(n) = cur {
            if pred(n) {
                return Some(n);
            }
            if let Some(c) = self.at(n).first_child {
                cur = Some(c);
                continue;
            }
            let mut m = n;
            cur = loop {
                let at = self.at(m);
                if let Some(s) = at.next_sibling {
                    break Some(s);
                }
                match at.parent {
                    Some(p) if p != root => m = p,
                    _ => break None,
                }
            };
        }
        None
    }

    /// Every descendant of `root` in tree order, through the sibling links -- no allocation per node.
    /// The stack walk this replaced built a `Vec` of each node's children to push them, which made a
    /// `getElementsByTagName` recount after every insertion cost one allocation per node in the
    /// document.
    pub fn for_each_descendant(&self, root: NodeId, mut f: impl FnMut(NodeId)) {
        if !self.has(root) {
            return;
        }
        let mut cur = self.at(root).first_child;
        while let Some(n) = cur {
            f(n);
            if let Some(c) = self.at(n).first_child {
                cur = Some(c);
                continue;
            }
            let mut m = n;
            cur = loop {
                let at = self.at(m);
                if let Some(s) = at.next_sibling {
                    break Some(s);
                }
                match at.parent {
                    Some(p) if p != root => m = p,
                    _ => break None,
                }
            };
        }
    }

    /// [`Dom::for_each_descendant`] that also says when it leaves a subtree: `enter(n)` for every
    /// descendant of `root` in tree order -- `false` stops the walk -- and `leave(p)` once for every
    /// descendant `p` that has children, after the last node of its subtree was entered. A query walk
    /// keeps counts over the open ancestors with the two.
    pub(crate) fn walk_descendants(&self, root: NodeId, mut enter: impl FnMut(NodeId) -> bool, mut leave: impl FnMut(NodeId)) {
        if !self.has(root) {
            return;
        }
        let mut cur = self.at(root).first_child;
        while let Some(n) = cur {
            if !enter(n) {
                return;
            }
            if let Some(c) = self.at(n).first_child {
                cur = Some(c);
                continue;
            }
            let mut m = n;
            cur = loop {
                let at = self.at(m);
                if let Some(s) = at.next_sibling {
                    break Some(s);
                }
                match at.parent {
                    Some(p) if p != root => {
                        leave(p);
                        m = p;
                    }
                    _ => break None,
                }
            };
        }
    }

    /// First child element.
    pub fn first_element_child(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.at(id).first_child;
        while let Some(cid) = c {
            if matches!(self.at(cid).kind, NodeKind::Element { .. }) {
                return Some(cid);
            }
            c = self.at(cid).next_sibling;
        }
        None
    }

    /// Next sibling element (walking the following siblings of `id`).
    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.at(id).next_sibling;
        while let Some(cid) = c {
            if matches!(self.at(cid).kind, NodeKind::Element { .. }) {
                return Some(cid);
            }
            c = self.at(cid).next_sibling;
        }
        None
    }

    /// WHATWG "viable next sibling": the first following sibling of `id` (any node type) that is NOT
    /// in `exclude`. `after`/`replaceWith` use it as the *stable* insertion reference — computed once
    /// before any move, excluding the nodes being inserted, so an argument that is itself one of
    /// `id`'s siblings can't shift the reference out from under the batch (which a per-argument
    /// `next_element_sibling` reference did: WPT `after()/replaceWith() with siblings as arguments`).
    pub fn viable_next_sibling(&self, id: NodeId, exclude: &[NodeId]) -> Option<NodeId> {
        let mut c = self.at(id).next_sibling;
        while let Some(cid) = c {
            if !exclude.contains(&cid) {
                return Some(cid);
            }
            c = self.at(cid).next_sibling;
        }
        None
    }

    // --- Shadow tree (Korvus GS ②-A1): data structure + attach only. No shadow-aware
    // traversal/matching here (querySelector-style scoping, composed-tree walks, slot
    // assignment, ...) -- that is ②-A2. No webapi/JS surface -- that is ②-A3.

    /// `Element.attachShadow({ mode })` at the data-structure level: allocate a shadow-root
    /// arena node and record the host<->root edge. Does not touch `host`'s existing light
    /// children (the caller/webapi layer owns any "replace children" policy) and does not
    /// change how any existing traversal/query method treats `host` or `root`.
    ///
    /// The root reuses `NodeKind::Document` -- the same kind `create_xml_document` already
    /// allocates for a detached tree root -- rather than a new `NodeKind` variant, so no
    /// exhaustive match over `NodeKind` anywhere in this crate (or in korvus-webapi) needs to
    /// change. It has `parent: None` and an empty `children` list; it is deliberately never
    /// pushed into any node's `children`, so `is_element`/`parent_element`-style checks still
    /// see it as "not an element" (spec-correct: a `ShadowRoot` is not an `Element`), and nothing
    /// can reach it via an ordinary parent/child walk except through `shadow_root_of`.
    pub fn attach_shadow(
        &mut self,
        host: NodeId,
        mode: ShadowRootMode,
    ) -> Result<NodeId, AttachShadowError> {
        self.attach_shadow_full(host, mode, false, ShadowSlotAssignment::Named, false, false, false)
    }

    /// The full `attachShadow(init)` at the data-structure level, and also the HTML parser's
    /// declarative-shadow-root construction (`declarative: true`).
    ///
    /// A host that already carries a DECLARATIVE root (the parser attached it, no `attachShadow()`
    /// has consumed it yet) is a special case, not an ordinary "already has a shadow root" error:
    /// exactly one call with a MATCHING `mode` succeeds, reusing that root -- its children cleared,
    /// every other setting (delegatesFocus/slotAssignment/clonable/serializable) left exactly as
    /// the markup declared, `declarative` cleared so a second such call now behaves like an
    /// ordinary repeat attach (error) -- which is how a page recovers a reference to a
    /// `mode:"closed"` declarative root. A mode MISMATCH errors regardless of `declarative`. WPT
    /// `shadow-dom/declarative/declarative-shadow-dom-repeats.html`.
    pub fn attach_shadow_full(
        &mut self,
        host: NodeId,
        mode: ShadowRootMode,
        delegates_focus: bool,
        slot_assignment: ShadowSlotAssignment,
        clonable: bool,
        serializable: bool,
        declarative: bool,
    ) -> Result<NodeId, AttachShadowError> {
        if !matches!(self.at(host).kind, NodeKind::Element { .. }) {
            return Err(AttachShadowError::HostNotElement);
        }
        if let Some(&existing) = self.shadow_roots_by_host.get(&host) {
            let info = *self.shadow_roots.get(&existing).expect("shadow_roots_by_host entries always have a ShadowInfo");
            if info.declarative && info.mode == mode {
                let kids = self.children(existing);
                for c in kids {
                    self.unlink(c);
                }
                self.shadow_roots.get_mut(&existing).expect("just looked up above").declarative = false;
                self.journal.push(crate::journal::Change::Other { node: Some(host), what: "shadow root" });
                return Ok(existing);
            }
            return Err(AttachShadowError::HostAlreadyHasShadowRoot);
        }
        let root = self.push(NodeKind::Document, None);
        self.shadow_roots.insert(root, ShadowInfo { host, mode, delegates_focus, slot_assignment, clonable, serializable, declarative });
        self.shadow_roots_by_host.insert(host, root);
        self.journal.push(crate::journal::Change::Other { node: Some(host), what: "shadow root" });
        Ok(root)
    }

    /// The shadow root attached to `host`, if any. Open/closed alike -- restricting a closed
    /// root's visibility to script is a webapi-layer concern (②-A3), not a tree-structure one.
    /// Whether any shadow root exists in this arena -- attached or not. False lets a caller skip
    /// every per-node shadow question (the style matcher asked four per element).
    pub fn has_shadow_roots(&self) -> bool {
        !self.shadow_roots.is_empty()
    }

    pub fn shadow_root_of(&self, host: NodeId) -> Option<NodeId> {
        self.shadow_roots_by_host.get(&host).copied()
    }

    /// Whether `id` is itself a shadow-root arena node. Map-based (not a `NodeKind` check) --
    /// see `attach_shadow`'s doc comment for why the root's arena kind is not distinctive.
    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        self.shadow_roots.contains_key(&id)
    }

    /// The host element of shadow root `root`, if `root` is in fact a shadow root.
    pub fn shadow_host_of(&self, root: NodeId) -> Option<NodeId> {
        self.shadow_roots.get(&root).map(|info| info.host)
    }

    /// The encapsulation mode of shadow root `root`, if `root` is in fact a shadow root.
    pub fn shadow_mode(&self, root: NodeId) -> Option<ShadowRootMode> {
        self.shadow_roots.get(&root).map(|info| info.mode)
    }

    /// `ShadowRoot.delegatesFocus`.
    pub fn shadow_delegates_focus(&self, root: NodeId) -> Option<bool> {
        self.shadow_roots.get(&root).map(|info| info.delegates_focus)
    }

    /// `ShadowRoot.slotAssignment` ("named" or "manual").
    pub fn shadow_slot_assignment(&self, root: NodeId) -> Option<ShadowSlotAssignment> {
        self.shadow_roots.get(&root).map(|info| info.slot_assignment)
    }

    /// `ShadowRoot.clonable`.
    pub fn shadow_clonable(&self, root: NodeId) -> Option<bool> {
        self.shadow_roots.get(&root).map(|info| info.clonable)
    }

    /// `ShadowRoot.serializable`.
    pub fn shadow_serializable(&self, root: NodeId) -> Option<bool> {
        self.shadow_roots.get(&root).map(|info| info.serializable)
    }

    /// `ShadowRoot.referenceTarget` (Chrome 152): the ID of the element in the shadow tree that references to the
    /// host resolve to, or `None` (`null`).
    pub fn shadow_reference_target(&self, root: NodeId) -> Option<&str> {
        self.shadow_reference_targets.get(&root).map(String::as_str)
    }

    /// Set (or, with `None`, clear) a shadow root's reference target.
    pub fn set_shadow_reference_target(&mut self, root: NodeId, target: Option<&str>) {
        match target {
            Some(t) => {
                self.shadow_reference_targets.insert(root, t.to_string());
            }
            None => {
                self.shadow_reference_targets.remove(&root);
            }
        }
        self.journal.push(crate::journal::Change::Other { node: Some(root), what: "shadow root" });
    }

    /// The `content` DocumentFragment of a `<template>` element, if one has been created for it
    /// (see [`Dom::ensure_template_content`]).
    pub fn template_content_of(&self, host: NodeId) -> Option<NodeId> {
        self.template_contents.get(&host).copied()
    }

    /// The `<template>` whose `content` fragment `content` is -- [`Self::template_content_of`]
    /// read backwards.
    pub fn template_host_of(&self, content: NodeId) -> Option<NodeId> {
        self.template_contents.iter().find(|(_, &c)| c == content).map(|(&h, _)| h)
    }

    /// Is `id` the `content` fragment of some `<template>`? The host<->content edge lives only in
    /// `template_contents`, so this is that map read backwards (a `<style>` in template
    /// content is the one detached sheet whose rules still reach the cascade -- see
    /// `page::owner_contributes`).
    pub fn is_template_content(&self, id: NodeId) -> bool {
        self.template_contents.values().any(|&c| c == id)
    }

    /// Create (once) and return the `content` DocumentFragment for the `<template>` element
    /// `host`. The fragment is a `NodeKind::Fragment` (nodeType 11), the kind a detached document root
    /// and a shadow root use -- with `parent: None`, deliberately never linked into any node's
    /// `children` list, exactly like [`Dom::attach_shadow`]'s root: the host<->content edge lives
    /// only in [`Dom::template_contents`], so light-tree walks (child traversal, queries,
    /// serialization of the template's own child list) never cross it.
    pub fn ensure_template_content(&mut self, host: NodeId) -> NodeId {
        if let Some(existing) = self.template_contents.get(&host) {
            return *existing;
        }
        let frag = self.push(NodeKind::Fragment, None);
        self.template_contents.insert(host, frag);
        frag
    }

    /// Move every current child of `<template>` element `host` into its `content` fragment
    /// (creating the fragment if needed), leaving the template's own light child list empty --
    /// the state the HTML parser is supposed to leave a template in. Used by the parser fixup,
    /// by [`Dom::set_inner_html`](crate::serialize) on a template, and by the webapi `content`
    /// getter (which folds any light-list children in on access).
    pub fn move_children_into_template_content(&mut self, host: NodeId) -> NodeId {
        let frag = self.ensure_template_content(host);
        let old = self.children(host);
        for c in old {
            self.unlink(c);
            self.link_as_last_child(frag, c);
        }
        frag
    }

    /// After an HTML parse, relocate every `<template>` element's parsed children into its
    /// `content` fragment. html5ever hands a template's markup back as ordinary element children;
    /// the HTML spec puts them in the content DocumentFragment instead (a template's own child
    /// list is always empty). Idempotent for an already-correct tree (no children => no move).
    pub(crate) fn hoist_template_contents(&mut self, from: NodeId) {
        let mut templates: Vec<NodeId> = Vec::new();
        let mut stack: Vec<NodeId> = self.children(from);
        while let Some(id) = stack.pop() {
            if self.tag_name(id).is_some_and(|t| t.eq_ignore_ascii_case("template")) {
                templates.push(id);
            }
            stack.extend(self.children(id));
        }
        for t in templates {
            self.move_children_into_template_content(t);
        }
    }
}

#[cfg(test)]
mod shadow_tree_tests {
    use super::*;

    #[test]
    fn attach_shadow_to_element_succeeds() {
        let mut dom = Dom::empty();
        let host = dom.create_element("div");
        let root = dom.attach_shadow(host, ShadowRootMode::Open).expect("attach should succeed");
        assert_ne!(root, host);
    }

    #[test]
    fn attach_shadow_twice_to_same_host_errors() {
        let mut dom = Dom::empty();
        let host = dom.create_element("div");
        dom.attach_shadow(host, ShadowRootMode::Open).expect("first attach should succeed");
        let second = dom.attach_shadow(host, ShadowRootMode::Closed);
        assert_eq!(second, Err(AttachShadowError::HostAlreadyHasShadowRoot));
    }

    #[test]
    fn attach_shadow_to_non_element_errors() {
        let mut dom = Dom::empty();
        let text = dom.create_text_node("hello");
        let result = dom.attach_shadow(text, ShadowRootMode::Open);
        assert_eq!(result, Err(AttachShadowError::HostNotElement));
    }

    #[test]
    fn shadow_root_accessors_round_trip() {
        let mut dom = Dom::empty();
        let host = dom.create_element("div");
        assert_eq!(dom.shadow_root_of(host), None);
        assert!(!dom.is_shadow_root(host));

        let root = dom.attach_shadow(host, ShadowRootMode::Closed).expect("attach should succeed");

        assert_eq!(dom.shadow_root_of(host), Some(root));
        assert!(dom.is_shadow_root(root));
        assert!(!dom.is_shadow_root(host));
        assert_eq!(dom.shadow_host_of(root), Some(host));
        assert_eq!(dom.shadow_host_of(host), None);
        assert_eq!(dom.shadow_mode(root), Some(ShadowRootMode::Closed));
        assert_eq!(dom.shadow_mode(host), None);
    }

    #[test]
    fn shadow_root_accepts_ordinary_children() {
        let mut dom = Dom::empty();
        let host = dom.create_element("div");
        let root = dom.attach_shadow(host, ShadowRootMode::Open).expect("attach should succeed");

        let span = dom.create_element("span");
        dom.append_child(root, span);

        assert_eq!(dom.children(root), &[span]);
        assert_eq!(dom.parent(span), Some(root));
        // The shadow root itself stays parentless -- it is not an ordinary child of `host`.
        assert_eq!(dom.parent(root), None);
        // And it must not appear in the host's own light-tree children.
        assert!(!dom.children(host).contains(&root));
    }

    // Regression: the sibling-link `insert_before` must treat a
    // null/out-of-range reference — `insertBefore(n, null)` arrives as the `u32::MAX` sentinel — as
    // "append", not index `nodes[u32::MAX]` and SIGABRT. Triggered by WPT dom/nodes/Element-children.
    #[test]
    fn insert_before_null_reference_appends() {
        let mut dom = Dom::empty();
        let parent = dom.create_element("div");
        let a = dom.create_element("a");
        let b = dom.create_element("b");
        dom.append_child(parent, a);
        dom.append_child(parent, b);
        let n = dom.create_element("n");
        dom.insert_before(parent, n, u32::MAX); // was: index out of bounds -> SIGABRT
        assert_eq!(dom.children(parent), &[a, b, n]);
        let m = dom.create_element("m");
        dom.insert_before(parent, m, a); // a real reference still inserts before it
        assert_eq!(dom.children(parent), &[m, a, b, n]);
    }
}

#[cfg(test)]
mod id_index_tests {
    use super::*;

    /// The pre-index `get_element_by_id` behavior, reimplemented against `Dom`'s public API only
    /// (`len`/`get_attribute`/`is_in_document`), so the parity test can check the index-backed
    /// lookup against an independent oracle instead of against itself.
    fn scan_get_element_by_id(dom: &Dom, id: &str) -> Option<NodeId> {
        if id.is_empty() { return None; }
        (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "id") == Some(id) && dom.is_in_document(n))
    }

    #[test]
    fn finds_in_document_element_by_id() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "target");
        dom.append_child(root, div);

        assert_eq!(dom.get_element_by_id("target"), Some(div));
    }

    #[test]
    fn does_not_find_detached_element() {
        let mut dom = Dom::empty();
        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "orphan");
        // Never appended anywhere -- `div` stays detached.

        assert_eq!(dom.get_element_by_id("orphan"), None);
    }

    #[test]
    fn changing_id_updates_lookups() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "old-id");
        dom.append_child(root, div);
        assert_eq!(dom.get_element_by_id("old-id"), Some(div));

        dom.set_attribute(div, "id", "new-id");
        assert_eq!(dom.get_element_by_id("old-id"), None);
        assert_eq!(dom.get_element_by_id("new-id"), Some(div));
    }

    #[test]
    fn removing_id_attribute_clears_lookup() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "gone-soon");
        dom.append_child(root, div);
        assert_eq!(dom.get_element_by_id("gone-soon"), Some(div));

        dom.remove_attribute(div, "id");
        assert_eq!(dom.get_element_by_id("gone-soon"), None);
    }

    #[test]
    fn empty_id_never_matches() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "");
        dom.append_child(root, div);

        assert_eq!(dom.get_element_by_id(""), None);
    }

    #[test]
    fn duplicate_ids_return_lowest_in_document_node_id_then_fall_through() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let first = dom.create_element("div");
        dom.set_attribute(first, "id", "dup");
        let second = dom.create_element("span");
        dom.set_attribute(second, "id", "dup");
        // `first` was allocated its NodeId before `second`.
        assert!(first < second);
        dom.append_child(root, first);
        dom.append_child(root, second);

        // Same result the old O(n) ascending-NodeId scan would give: the lower id wins.
        assert_eq!(dom.get_element_by_id("dup"), Some(first));

        // Removing the winning duplicate from the tree must fall through to the runner-up --
        // proof that a detach does NOT need (and does not get) an eager index-removal hook.
        dom.remove_child(root, first);
        assert_eq!(dom.get_element_by_id("dup"), Some(second));
    }

    #[test]
    fn element_from_inner_html_fragment_parse_is_found() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let container = dom.create_element("div");
        dom.append_child(root, container);

        // Exercises the parse-path hook in `push`: `set_inner_html` parses this fragment in a
        // throwaway sub-`Dom` and copies the result into `dom` via `copy_subtree`, which never
        // calls `set_attribute` -- only `push` sees the incoming `id="from-html"` attribute.
        let (_removed, added) = dom.set_inner_html(container, "<p id=\"from-html\">hi</p>");
        assert_eq!(added.len(), 1);

        assert_eq!(dom.get_element_by_id("from-html"), Some(added[0]));
    }

    #[test]
    fn parity_with_old_full_scan_over_small_tree() {
        let mut dom = Dom::empty();
        let root = dom.root();

        let a1 = dom.create_element("div");
        dom.set_attribute(a1, "id", "a");
        let b = dom.create_element("span");
        dom.set_attribute(b, "id", "b");
        let a2 = dom.create_element("p");
        dom.set_attribute(a2, "id", "a"); // duplicate of "a", allocated after a1
        let detached = dom.create_element("i");
        dom.set_attribute(detached, "id", "c"); // never attached to the document

        dom.append_child(root, a1);
        dom.append_child(root, b);
        dom.append_child(root, a2);
        // `detached` intentionally left unattached.

        for probe in ["a", "b", "c", "missing", ""] {
            assert_eq!(
                dom.get_element_by_id(probe),
                scan_get_element_by_id(&dom, probe),
                "index/scan mismatch for id {probe:?}"
            );
        }
        // Confirms the duplicate case above is actually exercised (lowest NodeId wins).
        assert_eq!(dom.get_element_by_id("a"), Some(a1));

        // Parity must keep holding after a structural removal changes which candidates are live.
        dom.remove_child(root, a1);
        for probe in ["a", "b", "c", "missing"] {
            assert_eq!(
                dom.get_element_by_id(probe),
                scan_get_element_by_id(&dom, probe),
                "index/scan mismatch for id {probe:?} after removal"
            );
        }
        assert_eq!(dom.get_element_by_id("a"), Some(a2));
    }
}

#[cfg(test)]
mod name_index_tests {
    use super::*;

    /// The elements carrying `name=<name>`, by a walk of the whole arena: the oracle the index is
    /// checked against, never the index itself.
    fn scan_named(dom: &Dom, name: &str) -> Vec<NodeId> {
        (0..dom.len() as NodeId).filter(|&n| dom.get_attribute(n, "name") == Some(name)).collect()
    }

    fn indexed(dom: &Dom, name: &str) -> Vec<NodeId> {
        let mut v = dom.elements_named(name).to_vec();
        v.sort_unstable();
        v
    }

    #[test]
    fn the_name_index_follows_every_path_that_changes_a_name() {
        let mut dom = Dom::parse(r#"<form name="a"></form><img name="a"><iframe name="b"></iframe>"#);
        assert_eq!(indexed(&dom, "a").len(), 2, "parse (push) indexes a name carried in the markup");
        assert_eq!(indexed(&dom, "a"), scan_named(&dom, "a"));
        let form = dom.elements_named("a").iter().copied().find(|&n| dom.tag_name(n) == Some("form")).unwrap();
        dom.set_attribute(form, "name", "c");
        for n in ["a", "b", "c"] {
            assert_eq!(indexed(&dom, n), scan_named(&dom, n), "after set_attribute: {n}");
        }
        dom.remove_attribute(form, "name");
        for n in ["a", "b", "c"] {
            assert_eq!(indexed(&dom, n), scan_named(&dom, n), "after remove_attribute: {n}");
        }
        let embed = dom.create_element("embed");
        dom.set_attribute(embed, "name", "b");
        assert_eq!(indexed(&dom, "b"), scan_named(&dom, "b"), "a detached element is a candidate, as in the arena scan");
        assert!(dom.elements_named("zzz").is_empty());
    }
}

#[cfg(test)]
mod class_token_tests {
    use super::*;

    /// The split `has_class` did on every call before the cache: the oracle.
    fn split_has(dom: &Dom, id: NodeId, name: &str) -> bool {
        dom.get_attribute(id, "class").is_some_and(|v| v.split_whitespace().any(|c| c == name))
    }

    fn cached_has(dom: &Dom, id: NodeId, name: &str) -> bool {
        dom.has_class_token(id, crate::select_engine::string_hash(name), name)
    }

    #[test]
    fn cached_class_tokens_answer_what_the_split_answered() {
        let values = [
            "a b c", "  a\tb\nc  ", "x\u{b}y", "p\u{a0}q r", "", "   ", "dup dup", "Case case",
            "long-class-name-with-dashes another_one", "\u{2003}em\u{2003}space",
        ];
        let names = ["a", "b", "c", "x", "y", "x\u{b}y", "p", "q", "p\u{a0}q", "r", "dup", "Case", "case", "CASE",
                     "long-class-name-with-dashes", "another_one", "em", "space", "", "zz"];
        let mut dom = Dom::parse(r#"<div class="a b"></div>"#);
        let parsed = (0..dom.len() as NodeId).find(|&n| dom.tag_name(n) == Some("div")).unwrap();
        assert!(cached_has(&dom, parsed, "a") && cached_has(&dom, parsed, "b") && !cached_has(&dom, parsed, "c"),
                "push (the parser) caches the class a node is created with");
        let el = dom.create_element("span");
        for v in values {
            dom.set_attribute(el, "class", v);
            for n in names {
                assert_eq!(cached_has(&dom, el, n), split_has(&dom, el, n), "class={v:?} name={n:?}");
            }
        }
        dom.remove_attribute(el, "class");
        for n in names {
            assert!(!cached_has(&dom, el, n), "after remove_attribute: {n:?}");
        }
    }
}

#[cfg(test)]
mod render_revision_tests {
    use super::*;

    fn doc() -> (Dom, NodeId, NodeId) {
        let dom = Dom::parse(r#"<div id="a"></div><div id="b"></div>"#);
        let (a, b) = (dom.get_element_by_id("a").unwrap(), dom.get_element_by_id("b").unwrap());
        (dom, a, b)
    }

    /// Nothing a document can see -- a node made, a detached subtree built and changed, a
    /// fragment filled, a value written over itself -- moves `render_revision`, and `revision` still moves.
    #[test]
    fn work_no_document_can_see_does_not_move_the_render_revision() {
        let (mut dom, a, _) = doc();
        let (r0, all0) = (dom.render_revision(), dom.revision());
        let d = dom.create_element("div");
        let t = dom.create_text_node("x");
        dom.append_child(d, t);
        dom.set_attribute(d, "class", "tall");
        dom.set_text_content(d, "y");
        let f = dom.create_fragment();
        let c = dom.clone_node(d, true);
        dom.append_child(f, c);
        dom.remove_attribute(d, "class");
        let _ = dom.set_inner_html(d, "<p>z</p>");
        dom.set_attribute(a, "id", "a");
        dom.remove_attribute(a, "absent");
        assert_eq!(dom.render_revision(), r0);
        assert!(dom.revision() > all0, "every other cache still sees each of those");
    }

    /// Every change a document can see moves it: inserting a built subtree, taking a node out of the
    /// document into a detached parent (`unlink` asks where the node WAS), an attribute, text in the
    /// tree, a shadow tree's content.
    #[test]
    fn every_change_a_document_can_see_moves_it() {
        let (mut dom, a, b) = doc();
        let d = dom.create_element("div");
        let body = dom.parent(a).unwrap();
        let mut last = dom.render_revision();
        let mut moved = |dom: &Dom, what: &str| {
            assert!(dom.render_revision() > last, "{what} did not move the render revision");
            last = dom.render_revision();
        };
        dom.insert_before(body, d, b);
        moved(&dom, "inserting a detached element");
        let w = dom.create_element("div");
        dom.append_child(w, a);
        moved(&dom, "moving a document's element into a detached one");
        dom.set_attribute(b, "class", "x");
        moved(&dom, "an attribute");
        dom.set_text_content(b, "text");
        moved(&dom, "text");
        let shadow = dom.attach_shadow(d, ShadowRootMode::Open).unwrap();
        let s = dom.create_element("span");
        dom.append_child(shadow, s);
        moved(&dom, "a shadow tree's content");
    }

    /// A change to ANOTHER document of the arena -- a frame's, a `DOMParser` result's -- moves
    /// `render_revision` (that document's own layout is keyed on it) and not `page_render_revision`, which the
    /// page's layout is keyed on; a change to the page, a shadow tree of the page's included, moves both.
    #[test]
    fn another_documents_changes_leave_the_page_revision() {
        let (mut dom, a, b) = doc();
        let other = dom.parse_into(r#"<p id="x">x</p><div id="h"></div>"#);
        let x = dom.children(other).into_iter().find_map(|html| {
            let mut stack = vec![html];
            while let Some(n) = stack.pop() {
                if dom.get_attribute(n, "id") == Some("x") {
                    return Some(n);
                }
                stack.extend(dom.children(n));
            }
            None
        });
        let x = x.unwrap();
        let (r0, p0) = (dom.render_revision(), dom.page_render_revision());
        dom.set_attribute(x, "class", "c");
        dom.set_text_content(x, "y");
        let e = dom.create_element("i");
        dom.append_child(x, e);
        dom.remove_child(x, e);
        let host = dom.parent(x).unwrap();
        let sr = dom.attach_shadow(host, ShadowRootMode::Open).unwrap();
        let s = dom.create_element("span");
        dom.append_child(sr, s);
        dom.set_attribute(x, "style", "opacity: 0.5");
        assert!(dom.render_revision() > r0, "another document's changes are still changes");
        assert_eq!(dom.page_render_revision(), p0, "none of them is the page's");
        let (d1, o1) = (dom.document_render_revision(other), dom.document_render_revision(dom.root()));
        assert!(d1 > 0, "the other document counts its own changes");
        assert_eq!(o1, p0, "the page's revision is its own");
        dom.set_attribute(b, "class", "z");
        assert_eq!(dom.page_render_revision(), p0 + 1, "the page's attribute");
        assert_eq!(dom.document_render_revision(other), d1, "a page change is not the other document's");
        let psr = dom.attach_shadow(a, ShadowRootMode::Open).unwrap();
        let p1 = dom.page_render_revision();
        let t = dom.create_element("b");
        dom.append_child(psr, t);
        assert!(dom.page_render_revision() > p1, "a shadow tree of the page's counts for the page");
    }

    /// A `style` rewrite that changes only the transform family and `opacity` is counted as such; one
    /// that also changes anything else is not.
    #[test]
    fn only_transform_and_opacity_rewrites_are_inert() {
        let (mut dom, a, _) = doc();
        dom.set_attribute(a, "style", "height: 10px");
        let (r0, i0) = (dom.render_revision(), dom.inert_style_writes());
        dom.set_attribute(a, "style", "height: 10px; transform: translateX(1px); opacity: 0.5");
        dom.set_attribute(a, "style", "height: 10px; transform: translateX(2px); opacity: 0.25; scale: 2; rotate: 1deg; translate: 1px");
        assert_eq!((dom.render_revision() - r0, dom.inert_style_writes() - i0), (2, 2));
        dom.set_attribute(a, "style", "height: 11px; transform: translateX(2px)");
        assert_eq!((dom.render_revision() - r0, dom.inert_style_writes() - i0), (3, 2), "a height is not inert");
    }

    #[test]
    fn a_style_text_that_cannot_be_split_plainly_is_never_inert() {
        let inert = style_change_is_inert;
        assert!(inert("", "transform: translateX(1px)"));
        assert!(inert("Opacity: 1", "opacity: 0.5"), "names fold case");
        assert!(inert("width: 1px; opacity: 1", "opacity: 0; width: 1px"), "order is not a change");
        assert!(inert("transform: translate(1px, 2px)", "transform: translate(3px, 4px); opacity: 1"));
        assert!(!inert("width: 1px", "width: 2px"));
        assert!(!inert("", "--x: 1"), "a custom property can reach anything");
        assert!(!inert("", "background: url(a;b)"));
        assert!(!inert("", "content: 'x'"), "quotes");
        assert!(!inert("", "opacity: 1 /* c */"), "comments");
        assert!(!inert("", "transform: translate(1px"), "an unbalanced bracket");
        assert!(!inert("", "opacity"), "a declaration without a colon");
        assert!(!inert("width: 1px; width: 2px", "width: 2px; width: 1px"), "the last of a name wins");
    }

    /// The colours, outlines and shadows only paint reads are inert; whatever can move a box or a hit test is not.
    #[test]
    fn paint_only_colours_outlines_and_shadows_are_inert() {
        let inert = style_change_is_inert;
        for (old, new) in [
            ("color: red", "color: blue"),
            ("width: 1px; background-color: red", "width: 1px; background-color: rgb(0, 0, 255)"),
            ("border-top-color: red", "border-top-color: blue; border-left-color: red"),
            ("border-color: red", "border-color: blue"),
            ("border-inline-start-color: red", "border-block-end-color: red"),
            ("outline: 1px solid red", "outline: 3px dashed blue; outline-offset: 4px"),
            ("outline-width: 1px; outline-style: solid; outline-color: red", ""),
            ("box-shadow: rgb(255, 0, 0) 0px 0px 3px", "box-shadow: none; text-shadow: rgb(0, 0, 0) 1px 1px 2px"),
            ("color: red; transform: scale(2)", "color: blue; opacity: 0.5"),
        ] {
            assert!(inert(old, new), "{old} -> {new}");
        }
        for (old, new) in [
            ("background: red", "background: blue"),
            ("background-image: url(a.png)", "background-image: none"),
            ("border: 1px solid red", "border: 1px solid blue"),
            ("border-top-style: none", "border-top-style: solid"),
            ("filter: none", "filter: blur(1px)"),
            ("fill: red", "fill: none"),
            ("visibility: visible", "visibility: hidden"),
            ("pointer-events: auto", "pointer-events: none"),
            ("color: red", "color: blue; width: 1px"),
            ("-webkit-box-shadow: none", "-webkit-box-shadow: 0 0 1px red"),
        ] {
            assert!(!inert(old, new), "{old} -> {new}");
        }
    }

    /// The cascade reads a CSSOM write's precise text exactly while the attribute holds that write's serialization:
    /// a write of another text or a removal ends it, the same text written again does not, a clone keeps it, and a
    /// precise-only change counts as a style write.
    #[test]
    fn precise_style_follows_the_style_writes() {
        let (mut dom, a, _) = doc();
        dom.set_inline_style(a, "transform: scale(1.001);", "transform: scale(1.000998);".into());
        assert_eq!(dom.style_for_cascade(a), Some("transform: scale(1.000998);"));
        let c = dom.clone_node(a, false);
        assert_eq!(dom.style_for_cascade(c), Some("transform: scale(1.000998);"), "a clone keeps the declarations");
        // the text the attribute holds, written again (any number of times): no write of its declarations
        let w = dom.style_writes_of(a);
        dom.set_attribute(a, "style", "transform: scale(1.001);");
        dom.set_attribute(a, "style", "transform: scale(1.001);");
        assert_eq!(dom.style_for_cascade(a), Some("transform: scale(1.000998);"), "the same text set as an attribute");
        assert_eq!(dom.style_writes_of(a), w);
        let c2 = dom.clone_node(a, false);
        assert_eq!(dom.style_for_cascade(c2), Some("transform: scale(1.000998);"), "and a clone made then copies it");
        dom.set_attribute(a, "style", "width: 1px;");
        dom.set_attribute(a, "style", "transform: scale(1.001);");
        assert_eq!(dom.style_for_cascade(a), Some("transform: scale(1.001);"), "another text and back: the attribute's");
        assert_eq!(dom.style_writes_of(a), w.wrapping_add(2));
        dom.set_inline_style(c, "transform: scale(1.001);", "transform: scale(1.000998);".into());
        dom.remove_attribute(c, "style");
        assert_eq!(dom.style_for_cascade(c), None);
        dom.set_attribute(c, "style", "transform: scale(1.001);");
        assert_eq!(dom.style_for_cascade(c), Some("transform: scale(1.001);"), "a removal ended it");

        // a precise-only change: the attribute write sees nothing, the cascade's text moved
        dom.set_inline_style(a, "transform: scale(1.001);", "transform: scale(1.0009981);".into());
        let (w0, r0, rr0) = (dom.style_writes_of(a), dom.revision(), dom.render_revision());
        dom.set_inline_style(a, "transform: scale(1.001);", "transform: scale(1.0009982);".into());
        assert_eq!(dom.style_for_cascade(a), Some("transform: scale(1.0009982);"));
        assert_eq!((dom.style_writes_of(a) - w0, dom.revision() - r0, dom.render_revision() - rr0), (1, 2, 1));
        // the identical write: only what the attribute write itself moves (its text stays: no style write)
        let (w1, r1, rr1) = (dom.style_writes_of(a), dom.revision(), dom.render_revision());
        dom.set_inline_style(a, "transform: scale(1.001);", "transform: scale(1.0009982);".into());
        assert_eq!(dom.style_for_cascade(a), Some("transform: scale(1.0009982);"));
        assert_eq!((dom.style_writes_of(a) - w1, dom.revision() - r1, dom.render_revision() - rr1), (0, 1, 0));
        // back to a precise text that is its serialization: the entry goes
        dom.set_inline_style(a, "transform: scale(1.001);", "transform: scale(1.001);".into());
        assert_eq!(dom.style_for_cascade(a), Some("transform: scale(1.001);"));
        assert!(!dom.precise_styles.contains_key(&a));
        // a write whose precise text is its serialization keeps no entry
        dom.set_inline_style(a, "width: 1px;", "width: 1px;".into());
        assert_eq!(dom.style_for_cascade(a), Some("width: 1px;"));
        assert!(!dom.precise_styles.contains_key(&a));
    }
}

/// An attribute whose writes [`Dom::match_revision`] leaves out: `style`, and `data-*`.
fn unrevised_attribute(key: &str) -> bool {
    key == "style" || (key.len() > 5 && key.as_bytes()[..5].eq_ignore_ascii_case(b"data-"))
}

#[cfg(test)]
mod walk_descendants_tests {
    use super::*;

    // The walk a query keeps its ancestor counts with: the same nodes in the same order as
    // `for_each_descendant`, each node with children left once, after its whole subtree, and nothing
    // after `enter` says stop.
    #[test]
    fn visits_like_for_each_descendant_and_leaves_once() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..20 {
            let mut dom = Dom::empty();
            let root = dom.create_element("div");
            let mut nodes = vec![root];
            for _ in 0..200 {
                let parent = nodes[nodes.len() - 1 - next(nodes.len().min(5))];
                let n = if next(4) == 0 { dom.create_text_node("t") } else { dom.create_element("p") };
                dom.append_child(parent, n);
                nodes.push(n);
            }
            for &top in [root, nodes[next(nodes.len())]].iter() {
                let mut want = Vec::new();
                dom.for_each_descendant(top, |n| want.push(n));
                let events = std::cell::RefCell::new(Vec::new());
                dom.walk_descendants(top, |n| { events.borrow_mut().push((n, true)); true }, |n| events.borrow_mut().push((n, false)));
                let events = events.into_inner();
                let entered: Vec<NodeId> = events.iter().filter(|e| e.1).map(|e| e.0).collect();
                assert_eq!(entered, want);
                for &n in &want {
                    let left: Vec<usize> = events.iter().enumerate().filter(|(_, e)| e.0 == n && !e.1).map(|(i, _)| i).collect();
                    if dom.at(n).first_child.is_none() {
                        assert!(left.is_empty(), "a leaf is never left");
                        continue;
                    }
                    assert_eq!(left.len(), 1, "#{n} is left once");
                    let mut last = Vec::new();
                    dom.for_each_descendant(n, |d| last.push(d));
                    let last_entered = events.iter().position(|e| e.1 && e.0 == *last.last().unwrap()).unwrap();
                    assert!(left[0] > last_entered, "#{n} is left after its last descendant");
                }
                // Stopping at the k-th node enters exactly the first k.
                let k = want.len() / 2;
                let mut seen = 0;
                dom.walk_descendants(top, |_| { seen += 1; seen < k.max(1) }, |_| {});
                assert_eq!(seen, k.max(1).min(want.len()));
            }
        }
    }
}

#[cfg(test)]
mod base_made_tests {
    use super::*;

    #[test]
    fn an_arena_knows_whether_it_ever_made_a_base_element() {
        let mut dom = Dom::parse_page("<!doctype html><title>t</title><a href=x>a</a>");
        assert!(!dom.has_made_base(), "no <base> parsed, none made");
        let _ = dom.create_element("BASE");
        assert!(dom.has_made_base(), "createElement('BASE') makes one, connected or not: it may be inserted later");
        assert!(Dom::parse_page("<base href=/sub/><p>x").has_made_base(), "the parser makes them through push too");
        assert!(Dom::parse_page("<svg><base/></svg>").has_made_base(), "a foreign element named base counts, as the walk matches it");
    }
}
