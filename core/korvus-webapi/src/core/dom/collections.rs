//! `core/dom/collections.rs` — the ten platform LIST interfaces, as native V8 exotic objects.
//!
//! Every list a page is handed — `getElementsByTagName`, `children`, `childNodes`, `querySelectorAll`,
//! `form.elements`, `select.options`, `el.attributes`, `classList`, `sheet.cssRules`,
//! `getClientRects()`, `document.styleSheets`, a table's `rows`/`cells`/`tBodies` — is an object minted
//! from an `ObjectTemplate` with indexed (and, where WebIDL says so, named) property interceptors. The
//! state is a `v8::Private` slot, so the object owns NO string or symbol key of its own: Chrome's
//! `Object.getOwnPropertyNames(document.querySelectorAll("div"))` is exactly the indices, and an
//! HTMLCollection's is the indices then the supported names, which is what these reproduce.
//!
//! WHAT THIS REPLACES. A JS `Proxy` per live collection whose traps re-ran a native re-query op
//! through a bridge slot; bare `Array`s with `item`/`namedItem` closures stapled on
//! for `form.elements`, `select.options` and the four table collections; a `Object.create(proto)` +
//! own-index factory for the static lists; and four `Promise.resolve().then(...)` patches of
//! `HTMLCollection.prototype` / `NodeList.prototype` / `CSSRuleList.prototype` / `DOMRectList.prototype`
//! that existed only because the shells were created after `dom.js`. All ten prototypes are now hand
//! `interface!` tables installed by THE installer, so there is no load order left to patch around.
//!
//! LIVENESS. A live collection stores (root NodeId, kind, selector, namespace) and re-runs its arena
//! query on EVERY access — `length`, an index, a name, `item`, `namedItem`, enumeration. It never
//! caches: `children` sees an `appendChild` without being re-read. A static list (querySelectorAll,
//! `labels`, `cssRules`, `getClientRects()`, `styleSheets`) stores the items themselves.
//!
//! IDENTITY. Every element a collection yields comes from `wrap_cached`, so `coll[0] === coll[0]` and
//! `coll[0] === document.getElementById(...)`.
//!
//! THE EXOTIC RULES are WPT's, and each is reproduced from the Proxy traps this replaces:
//!   * an array index is a canonical non-negative integer BELOW 2^32-1, so `coll["4294967295"]` reaches
//!     the NAMED getter (the element whose id is "4294967295"), not the indexed one — which is exactly
//!     where V8 routes it, since 2^32-1 is not a valid element index;
//!   * indexed properties are `{writable:false, enumerable:true, configurable:true}`; named properties
//!     are `[LegacyUnenumerableNamedProperties]`: `{writable:false, enumerable:false, configurable:true}`;
//!   * a supported index and a supported name refuse `[[Set]]`, `[[DefineOwnProperty]]` and
//!     `[[Delete]]` with Blink's own messages; an index PAST THE END deletes fine;
//!   * the named getter is NON-MASKING — it never answers a name the object or its prototype chain
//!     already has, which is WebIDL §3.9.4.1 step 5 and why `coll.length` is the attribute even when an
//!     element is named "length", and why an expando shadows a name added later;
//!   * `item`/`namedItem`/`length` are the direct getters, not property reads: an expando cannot shadow
//!     `namedItem("x")`, and `namedItem("0")` is the element with id "0", never index 0;
//!   * a foreign receiver (`Object.create(coll).length`) is a TypeError "Illegal invocation".

use deno_core::v8;
use deno_core::v8::MapFnTo;
use korvus_dom::Dom;

use crate::binding::host_dom::HostDom;
use crate::binding::protos::{
    build_fn, build_fn_data, get_prop, global_get, int_val, str_val, throw_type_error,
    wrap_cached,
};
use crate::core::css::css_style_declaration::priv_get;
use crate::core::dom::attr::make_attr;
use crate::core::dom::dom_token_list::{dtl_tokens, DTL_ATTR, DTL_ID};
use crate::core::dom::element::priv_set;
use crate::core::dom::named_node_map::NNM_EID;
use crate::core::html::html_element::with_local_name;
use crate::install::kits::define_accessor;
use crate::install::MemberDef;
use crate::install::live::method_row;

// ── instance state ──────────────────────────────────────────────────────────────────────────────

/// One of a list's three HOT state slots, read on every index and `length`: kept in an internal field of the
/// instance when its template reserved them ([`LIST_FIELDS`]), in the private `name` otherwise. Not a `&str`, so
/// `priv_get` / `priv_set` / `priv_u32` refuse it at compile time and every access goes through [`lst_get`] /
/// [`lst_set`] -- one place that decides field or private, so a read always finds the last write.
#[derive(Clone, Copy)]
struct LstKey {
    name: &'static str,
    field: usize,
}

/// The internal fields a list interface's instance template reserves: field 0 is the brand cache
/// (`binding::brand`), 1..=3 the [`LstKey`]s. No other template in the process reserves 4 (the branded ones
/// reserve 1, deno's global 2, V8's promises and array buffers at most 2), so the count says "a list's".
pub(crate) const LIST_FIELDS: usize = 4;

/// [`LstKey`] `k`'s value on `o`: the field when the template reserved it and it was written (a field starts
/// `undefined`, and no value kept here is ever `undefined`), else the private -- exactly `priv_get`'s answer
/// for an object whose state was written to the private (a foreign receiver, a list minted while the
/// snapshot maker records, `document.all`).
fn lst_get<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<v8::Object>, k: LstKey) -> Option<v8::Local<'s, v8::Value>> {
    if o.internal_field_count() == LIST_FIELDS {
        let v = o.get_internal_field(scope, k.field).and_then(|d| v8::Local::<v8::Value>::try_from(d).ok());
        if let Some(v) = v.filter(|v| !v.is_undefined()) {
            return Some(v);
        }
    }
    priv_get(scope, o, k.name)
}

/// Write [`LstKey`] `k` on `o`: into its field when the template reserved one, never while the snapshot maker
/// records (rusty_v8's internal-field serialiser reads a field as an embedder pointer: `binding::brand`'s
/// `set_field_brand` has the same rule), else into the private.
fn lst_set(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: LstKey, v: v8::Local<v8::Value>) {
    if o.internal_field_count() == LIST_FIELDS && !crate::binding::refs::recording() {
        o.set_internal_field(k.field, v.into());
        return;
    }
    priv_set(scope, o, k.name, v);
}

fn lst_u32(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, k: LstKey) -> Option<u32> {
    lst_get(scope, o, k).and_then(|v| v.uint32_value(scope))
}

/// Which state shape this list carries. Read first by every body and every interceptor; its absence is
/// what "not one of ours" (a foreign receiver) means.
const P_CLASS: LstKey = LstKey { name: "korvus_list_class", field: 1 };
/// The interface name, for Blink's own error messages ("... on 'HTMLCollection': ...").
const P_IFACE: &str = "korvus_list_iface";
/// A LIVE list's query: the arena root it runs from, its kind, its selector and its namespace.
const P_ROOT: &str = "korvus_list_root";
const P_KIND: &str = "korvus_list_kind";
const P_SEL: &str = "korvus_list_sel";
const P_NS: &str = "korvus_list_ns";
/// A STATIC list's items, as a `v8::Array` (never a page-visible property: it lives on a Private).
const P_ITEMS: LstKey = LstKey { name: "korvus_list_items", field: 3 };
/// A STATIC list's supported property NAMES and the value each answers, two parallel `v8::Array`s, in the
/// order the names enumerate. Only the plugin family has them ([`adopt_static_named`]).
const P_NAMES: &str = "korvus_list_names";
const P_NAMED: &str = "korvus_list_named";

const CLASS_LIVE: u32 = 1;
const CLASS_STATIC: u32 = 2;
const CLASS_TOKENS: u32 = 3;
const CLASS_ATTRS: u32 = 4;
/// `document.all`. It carries the SAME state a `CLASS_LIVE` list does (root + kind + selector) and
/// every reader below treats the two alike — but it is its own class so that a receiver check can
/// tell them apart, which Chrome does in BOTH directions and this used not to: measured 2026-09-21,
/// `HTMLCollection.prototype.item.call(document.all, 0)` and
/// `Object.getOwnPropertyDescriptor(HTMLAllCollection.prototype,'length').get.call(document.images)`
/// are each `TypeError: Illegal invocation` in Chrome 150.0.7871.46.
const CLASS_ALL: u32 = 5;

/// A `document.all` instance's per-name cache of the `HTMLCollection` a repeated name answers, as a
/// null-prototype dictionary object on a Private. Chrome answers the SAME object for a repeated
/// lookup (`document.all.m === document.all.m` is `true`, measured), which is why this exists; WPT's
/// `htmlallcollection.html` "collections are new live HTMLCollection instances" asserts the
/// opposite and **Chrome 150 fails that subtest**, 40 of its 41 passing (measured on the file's own
/// bytes, DA1).
const P_ALL_CACHE: &str = "korvus_all_named_cache";
/// `document.all`, cached on the Document: Chrome answers the same object every read
/// (`document.all === document.all` is `true`, measured).
pub(crate) const P_DOC_ALL: &str = "korvus_doc_all";

// ── the arena queries ───────────────────────────────────────────────────────────────────────────

/// A live collection's query kind. These are the arena scans the deleted JS Proxy's re-query op
/// dispatched on, plus the six that used to be JS array-building helpers (form controls, options, and
/// the four table collections).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub(crate) enum Kind {
    /// `Document.getElementsByTagName` — WHATWG qualified-name matching (see [`query_ids`]).
    DocByTag = 0,
    /// `Document.getElementsByClassName`.
    DocByClass = 1,
    /// `Element.getElementsByTagName` (descendants).
    ElByTag = 2,
    /// `Element.getElementsByClassName` (descendants).
    ElByClass = 3,
    /// `ParentNode.children` — the child ELEMENTS of the root.
    Children = 4,
    /// `Document.getElementsByTagNameNS`.
    DocByTagNs = 5,
    /// `Element.getElementsByTagNameNS`.
    ElByTagNs = 6,
    /// `document.forms`.
    Forms = 7,
    /// `document.images`.
    Images = 8,
    /// `document.scripts`.
    Scripts = 9,
    /// `document.embeds` and `document.plugins` — one collection in HTML, so one kind.
    Embeds = 10,
    /// `document.links`.
    Links = 11,
    /// `document.anchors` — `<a>` with a `name`.
    Anchors = 12,
    /// `document.applets` — permanently empty since `<applet>` left HTML.
    Applets = 13,
    /// `form.elements` / `fieldset.elements` — the listed controls, in tree order.
    FormControls = 14,
    /// `select.options` / `datalist.options`.
    Options = 15,
    /// `table.tBodies` — the table's own `<tbody>` children.
    TableBodies = 16,
    /// `table.rows` — head rows, then direct/`<tbody>` rows, then foot rows.
    TableRows = 17,
    /// `tr.cells` — the row's own `<td>`/`<th>` children.
    RowCells = 18,
    /// `tbody|thead|tfoot .rows` — the section's own `<tr>` children.
    SectionRows = 19,
    /// `Node.childNodes` — every child node (a LIVE NodeList).
    ChildNodes = 20,
    /// A `RadioNodeList`: the form's listed controls whose `name` or `id` is the selector.
    RadioGroup = 21,
    /// `document.all` — EVERY element in the document, in tree order, `<html>` first.
    DocAll = 22,
    /// The sub-collection `document.all`'s named getter answers when a name matches more than one
    /// element (HTML: "get the 'all'-named elements"): the same match rule, as an `HTMLCollection`.
    DocAllNamed = 23,
    /// `select.selectedOptions` -- the options whose selectedness is true.
    SelectedOptions = 25,
    /// `document.getElementsByName` — a LIVE NodeList over the HTML elements carrying that `name`.
    DocByElementName = 24,
}

/// HTML's **"all"-named elements**: the tags whose `name` attribute is a supported property name of
/// an `HTMLAllCollection`. An `id` names ANY element; a `name` names only these. Measured against
/// Chrome 150.0.7871.46 on a loopback page carrying one of each: `a`,
/// `button`, `embed`, `form`, `iframe`, `img`, `input`, `map`, `meta`, `object`, `select` and
/// `textarea` all answered their `name`; `span`, `p`, `div`, `li`, `h1` and `table` answered
/// `undefined`, and an `<svg name=…>` answered `undefined` while its `id` answered the element.
/// `frame`/`frameset` are the spec's and could not be measured — the HTML parser drops both inside a
/// `<body>`.
const ALL_NAMED_TAGS: [&str; 14] = [
    "a", "button", "embed", "form", "frame", "frameset", "iframe", "img", "input", "map", "meta", "object", "select",
    "textarea",
];

/// The listed form controls, in the selector the engine has always used for `form.elements`.
pub(crate) const LISTED_CONTROLS: &str = "input,select,textarea,button,fieldset,output";

/// Is `id` an **HTML element named `local`** — the phrase HTML uses everywhere it describes the
/// tabular tree ("its `tr` element children", "the first `caption` element child") and which means a
/// NAMESPACE and a LOCAL NAME, never a tag name.
///
/// THE TABULAR FAMILY'S ONE TAG QUESTION. The three queries below and every receiver check in
/// `core/html/html_table_elements.rs` used to ask `dom.tag_name(id).eq_ignore_ascii_case(tag)`, and a
/// bare tag name is wrong in BOTH directions at once:
///
///   * `createElementNS(htmlNS, "foo:table")` keeps the whole QUALIFIED name as its arena tag (see
///     `cb_doc_create_element_ns`), so a real HTML `<table>` was not recognised as one;
///   * `createElementNS("", "table")`'s arena tag IS `table`, so an element in NO namespace was —
///     its `<tr>` children reported `rowIndex` 0 where Chrome says -1.
///
/// The local name comes from [`local_name_in`](crate::core::html::html_element::local_name_in), DOC1.4's single local-name production: `QNameMap`'s
/// entry when `createElementNS` recorded one, the arena tag otherwise. It CANNOT be taken by
/// splitting the tag at `:` — the HTML parser builds `<a:table>` as one element whose local name is
/// the whole `a:table` and whose prefix is null (measured, headless Chrome 150 on this machine:
/// `HTMLUnknownElement`, `localName === "a:table"`), so a split would invent a prefix the DOM does
/// not have.
pub(crate) fn is_html_named(st: &deno_core::OpState, id: u32, local: &str) -> bool {
    st.borrow::<HostDom>().0.is_html_ns(id)
        && with_local_name(st, id, |n| n.eq_ignore_ascii_case(local)).unwrap_or(false)
}

/// The children of `root` that are HTML elements named `tag`. NOT a descendant query: a `<tr>` inside
/// a nested table is a descendant of the outer table and is emphatically not one of its rows.
pub(crate) fn kids_named(st: &deno_core::OpState, root: u32, tag: &str) -> Vec<u32> {
    st.borrow::<HostDom>()
        .0
        .child_elements(root)
        .into_iter()
        .filter(|&id| is_html_named(st, id, tag))
        .collect()
}

/// The cells of a row: its own `<td>`/`<th>` children, in tree order. ONE definition -- the
/// `RowCells` live-collection kind and `HTMLTableRowElement`'s `insertCell`/`deleteCell`/`cellIndex`
/// (`core/html/html_table_elements.rs`) read the same rule, so the two cannot drift.
pub(crate) fn row_cells(st: &deno_core::OpState, root: u32) -> Vec<u32> {
    st.borrow::<HostDom>()
        .0
        .child_elements(root)
        .into_iter()
        .filter(|&id| is_html_named(st, id, "td") || is_html_named(st, id, "th"))
        .collect()
}

/// "The rows of a table": the head section's rows, then every row that is a direct child or lives in a
/// `<tbody>` child (in tree order), then the foot section's rows. A `<tfoot>` written before the
/// `<tbody>` still sorts last, which is why this cannot be one query.
pub(crate) fn table_rows(st: &deno_core::OpState, root: u32) -> Vec<u32> {
    let (mut head, mut body, mut foot) = (Vec::new(), Vec::new(), Vec::new());
    let children = st.borrow::<HostDom>().0.child_elements(root);
    for c in children {
        if is_html_named(st, c, "tr") {
            body.push(c);
        } else if is_html_named(st, c, "thead") {
            head.extend(kids_named(st, c, "tr"));
        } else if is_html_named(st, c, "tbody") {
            body.extend(kids_named(st, c, "tr"));
        } else if is_html_named(st, c, "tfoot") {
            foot.extend(kids_named(st, c, "tr"));
        }
    }
    head.extend(body);
    head.extend(foot);
    head
}

/// Does `id` answer to `name` in an `HTMLAllCollection`? HTML: an element whose **ID** is `name`, or
/// an **"all"-named element** (see [`ALL_NAMED_TAGS`]) whose `name` attribute is `name`. The empty
/// string names nothing.
pub(crate) fn all_named_match(dom: &Dom, id: u32, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if dom.get_attribute(id, "id") == Some(name) {
        return true;
    }
    if !dom.is_html_ns(id) {
        return false;
    }
    let Some(tag) = dom.tag_name(id) else { return false };
    ALL_NAMED_TAGS.contains(&tag) && dom.get_attribute(id, "name") == Some(name)
}

/// Run one live query against the arena. This is the deleted re-query callback's body: the same
/// matching rules, reached from Rust rather than through a bridge call per property access.
/// HTML's **"list of `<local>` elements"** — what `document.images`, `forms`, `scripts`, `embeds`,
/// `links` and `anchors` each say in their own words: the elements in this document tree that are
/// HTML elements whose LOCAL name is `local`.
///
/// Every one of them used to be `Dom::get_elements_by_tag_name_in`, which compares the arena's
/// QUALIFIED name to a bare tag with no namespace filter at all — so `document.images` counted a
/// `createElementNS("http://example.org", "img")` and answered 11 where WPT (and Chrome) say 10.
/// One production, because it is one rule stated six times.
fn html_elements_named(st: &deno_core::OpState, root: u32, local: &str) -> Vec<u32> {
    let dom = &st.borrow::<HostDom>().0;
    dom.descendant_elements(root)
        .into_iter()
        .filter(|&id| {
            dom.is_html_ns(id)
                && with_local_name(st, id, |n| n.eq_ignore_ascii_case(local)).unwrap_or(false)
        })
        .collect()
}

pub(crate) fn query_ids(op_state: &std::rc::Rc<std::cell::RefCell<deno_core::OpState>>, root: u32, kind: u32, sel: &str, ns_sel: Option<&str>) -> Vec<u32> {
    let st = op_state.borrow();
    let dom = &st.borrow::<HostDom>().0;
    match kind {
        // getElementsByTagName (Document and Element): WHATWG matching. In an HTML document an
        // HTML-namespace element matches the ASCII-lowercased qualified name while a foreign
        // (SVG/MathML/null-ns) element matches the exact qualified name; in an XML document all match
        // exactly. "*" matches every element. The namespace comes off the arena node, so a parsed
        // `<feBlend>` matches `getElementsByTagName("feBlend")` and not `("feblend")`.
        k if k == Kind::DocByTag as u32 || k == Kind::ElByTag as u32 => {
            let is_html_doc = !dom.is_in_xml(root);
            let q_lower = sel.to_ascii_lowercase(); // HTML tag match is ASCII case-insensitive only
            let all = sel == "*";
            // ONE pass, no intermediate list of every element (a live collection's
            // `length` read after each append re-runs this over the whole document).
            let mut out = Vec::new();
            dom.for_each_descendant(root, |id| {
                if !dom.is_element(id) {
                    return;
                }
                let hit = all || match dom.tag_name(id) {
                    Some(name) => {
                        if is_html_doc {
                            if dom.is_html_ns(id) { name == q_lower } else { name == sel }
                        } else {
                            name == sel
                        }
                    }
                    None => false,
                };
                if hit {
                    out.push(id);
                }
            });
            out
        }
        // getElementsByTagNameNS: namespace + LOCAL name, both exact (only getElementsByTagName
        // case-folds HTML-qualified names).
        //
        // IT COMPARED THE QUALIFIED NAME. The arena stores an element's name as authored
        // (`korvus-dom/src/tree.rs:93`), so `createElementNS(ns, "t:body")` is `"t:body"` there,
        // and this filter asked whether that equalled the LOCAL name the caller passed. Measured
        // before the change, with a prefixed and an unprefixed element in one namespace:
        //
        //   getElementsByTagNameNS(ns, "body")    -> 1, must be 2   (the prefixed one is missed)
        //   getElementsByTagNameNS(ns, "t:body")  -> 1, must be 0   (a qualified name matched)
        //
        // `local_name_in` is the one production for an element's local name -- the same one
        // `Element.localName` and `html_elements_named` answer from (CONF DOC1.3/DOC1.4). Writing
        // the split a second time here is the defect this goal exists to remove; it has been found
        // eight times.
        k if k == Kind::DocByTagNs as u32 || k == Kind::ElByTagNs as u32 => {
            dom.descendant_elements(root)
                .into_iter()
                .filter(|&id| {
                    if sel != "*"
                        && !with_local_name(&st, id, |n| n == sel).unwrap_or(false)
                    {
                        return false;
                    }
                    match ns_sel {
                        Some("*") => true,
                        Some(ns) => dom.namespace_uri(id) == Some(ns),
                        None => dom.node_type(id) == 1 && dom.namespace_uri(id).is_none(), // null/"" => the no-namespace elements
                    }
                })
                .collect()
        }
        k if k == Kind::DocByClass as u32 => dom.get_elements_by_class_name_in(root, sel),
        k if k == Kind::ElByClass as u32 => dom.descendants_by_class_name(root, sel),
        k if k == Kind::Children as u32 => dom.child_elements(root),
        k if k == Kind::ChildNodes as u32 => dom.children(root).to_vec(),
        // The named HTML document collections. Each runs the SAME arena query its previous static
        // getter ran. `plugins` and `embeds` are one collection in HTML, so they share a kind;
        // `applets` has been permanently empty since <applet> left HTML and needs a kind of its own
        // precisely so that "empty" is the query's answer rather than a tag that matches nothing.
        k if k == Kind::Forms as u32 => html_elements_named(&st, root, "form"),
        k if k == Kind::Images as u32 => html_elements_named(&st, root, "img"),
        k if k == Kind::Scripts as u32 => html_elements_named(&st, root, "script"),
        k if k == Kind::Embeds as u32 => html_elements_named(&st, root, "embed"),
        k if k == Kind::Links as u32 => html_elements_named(&st, root, "a")
            .into_iter()
            .filter(|&id| dom.get_attribute(id, "href").is_some())
            .collect(),
        k if k == Kind::Anchors as u32 => html_elements_named(&st, root, "a")
            .into_iter()
            .filter(|&id| dom.get_attribute(id, "name").is_some())
            .collect(),
        // `document.getElementsByName`: HTML's "all the HTML ELEMENTS in that document that have a
        // name attribute whose value is equal to the name argument". It was a STATIC NodeList over a
        // namespace-blind arena scan, so it neither stayed live nor excluded a `<math name=x>` --
        // two WPT subtests, one query.
        k if k == Kind::DocByElementName as u32 => {
            dom.get_elements_by_name_in(root, sel).into_iter().filter(|&id| dom.is_html_ns(id)).collect()
        }
        k if k == Kind::Applets as u32 => Vec::new(),
        // `document.all`: every element in the document, in tree order. Chrome's first entry is
        // `document.documentElement` and its last is the closing `<script>`; the content of a
        // `<template>`, of a shadow root and of a nested browsing context are all absent, because
        // none of them is a descendant of THIS document's root (all four measured, DA1).
        k if k == Kind::DocAll as u32 => dom.descendant_elements(root),
        // The named sub-collection: the same match rule the named getter uses, as a LIVE query so
        // the `HTMLCollection` a repeated name answers keeps tracking the tree.
        k if k == Kind::DocAllNamed as u32 => {
            dom.descendant_elements(root).into_iter().filter(|&id| all_named_match(dom, id, sel)).collect()
        }
        k if k == Kind::FormControls as u32 => dom.descendants_matching(root, LISTED_CONTROLS).unwrap_or_default(),
        k if k == Kind::Options as u32 => dom.descendants_matching(root, "option").unwrap_or_default(),
        k if k == Kind::SelectedOptions as u32 => dom.list_of_options(root).into_iter().filter(|&o| dom.option_selectedness(o)).collect(),
        k if k == Kind::TableBodies as u32 => kids_named(&st, root, "tbody"),
        k if k == Kind::TableRows as u32 => table_rows(&st, root),
        k if k == Kind::RowCells as u32 => row_cells(&st, root),
        k if k == Kind::SectionRows as u32 => kids_named(&st, root, "tr"),
        // A RadioNodeList's members: the form's listed controls whose `name` OR `id` is the selector,
        // which is the same set `HTMLFormControlsCollection.namedItem` matched to build it.
        k if k == Kind::RadioGroup as u32 => dom
            .descendants_matching(root, LISTED_CONTROLS)
            .unwrap_or_default()
            .into_iter()
            .filter(|&id| dom.get_attribute(id, "name") == Some(sel) || dom.get_attribute(id, "id") == Some(sel))
            .collect(),
        _ => Vec::new(),
    }
}

// ── reading an instance's state ─────────────────────────────────────────────────────────────────

fn priv_u32(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<u32> {
    priv_get(scope, o, name).and_then(|v| v.uint32_value(scope))
}

fn priv_str(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, name: &str) -> Option<String> {
    priv_get(scope, o, name).map(|v| v.to_rust_string_lossy(scope))
}

/// The list class of `v`, or `None` when `v` is not one of ours (a foreign receiver).
fn class_of(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<u32> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    lst_u32(scope, o, P_CLASS)
}

/// The interface name stamped on the instance, for Blink's error messages.
fn iface_of(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> String {
    priv_str(scope, o, P_IFACE).unwrap_or_default()
}

/// Whether the interface name stamped on the instance is `iface`: [`iface_of`]` == iface` without a
/// Rust string per index query (the stamp is a V8 string, compared as one).
fn iface_is(scope: &mut v8::PinScope, o: v8::Local<v8::Object>, iface: &str) -> bool {
    let Some(v) = priv_get(scope, o, P_IFACE) else { return iface.is_empty() };
    if !v.is_string() {
        return v.to_rust_string_lossy(scope) == iface;
    }
    crate::binding::protos::key_str(scope, iface).is_some_and(|k| v.strict_equals(k.into()))
}

/// The arena node ids a LIVE list currently answers. `None` when the receiver is not a live list.
/// A live collection's members, memoised against the DOM's mutation `revision` (Blink's
/// CollectionIndexCache does the same job). Recomputing the whole list on EVERY `item(i)` made the
/// ubiquitous `for (i = 0; i < c.length; i++) c[i]` loop O(n²): 1,584 ms over 20 k children where Chrome
/// takes 1 ms (measured 2026-09-24). The list is shared, never copied, between reads.
#[derive(Default)]
pub(crate) struct LiveIdsCache {
    /// Per live collection object (by its `P_COLL` number): its query, and the ids it last answered
    /// with the revision they are valid for.
    /// (FxHash: the keys are the engine's own counters.)
    by_coll: std::cell::RefCell<crate::install::templates::FxMap<u32, (CollKey, bool, u64, std::rc::Rc<Vec<u32>>)>>,
    next: std::cell::Cell<u32>,
}

type CollKey = (u32, u32, String, Option<String>);

/// A live collection's private number: one slot read replaces the five its query is spelled in, on
/// every `length` and every index (those reads were most of `c[i]`'s cost).
const P_COLL: LstKey = LstKey { name: "korvus_coll_no", field: 2 };

fn live_ids(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<std::rc::Rc<Vec<u32>>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    if op_state.borrow().try_borrow::<LiveIdsCache>().is_none() {
        op_state.borrow_mut().put(LiveIdsCache::default());
    }
    let no = lst_u32(scope, this, P_COLL);
    // A hit copies out what it compares and answers -- never the query key, whose selector is a
    // `String`: only a recompute needs it.
    let known = no.and_then(|n| {
        let st = op_state.borrow();
        let c = st.borrow::<LiveIdsCache>();
        let m = c.by_coll.borrow();
        m.get(&n).map(|(_, structural, rev, ids)| (*structural, *rev, ids.clone()))
    });
    let (no, key, structural, cached) = match (no, known) {
        (Some(n), Some((structural, rev, ids))) => (n, None, structural, Some((rev, ids))),
        _ => {
            if !matches!(lst_u32(scope, this, P_CLASS)?, CLASS_LIVE | CLASS_ALL) {
                return None;
            }
            let (key, structural) = coll_key_of(scope, this)?;
            let n = {
                let st = op_state.borrow();
                let c = st.borrow::<LiveIdsCache>();
                let n = c.next.get();
                c.next.set(n.wrapping_add(1));
                n
            };
            let nv = v8::Integer::new_from_unsigned(scope, n).into();
            lst_set(scope, this, P_COLL, nv);
            (n, Some(key), structural, None)
        }
    };
    let rev = {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        if structural { dom.structure_revision() } else { dom.revision() }
    };
    if let Some((r, ids)) = cached {
        if r == rev {
            return Some(ids);
        }
    }
    let key = match key {
        Some(k) => k,
        None => {
            // the entry the hit was read from (nothing ran since that could drop it)
            let kept = {
                let st = op_state.borrow();
                let c = st.borrow::<LiveIdsCache>();
                let m = c.by_coll.borrow();
                m.get(&no).map(|(k, ..)| k.clone())
            };
            match kept {
                Some(k) => k,
                None => coll_key_of(scope, this)?.0,
            }
        }
    };
    let ids = std::rc::Rc::new(query_ids(&op_state, key.0, key.1, &key.2, key.3.as_deref()));
    let st = op_state.borrow();
    let c = st.borrow::<LiveIdsCache>();
    let mut map = c.by_coll.borrow_mut();
    if map.len() > 4096 {
        map.clear();
    }
    map.insert(no, (key, structural, rev, ids.clone()));
    Some(ids)
}

/// A live list's query as its privates spell it, and whether it keys on the structure revision.
fn coll_key_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<(CollKey, bool)> {
    let root = priv_u32(scope, this, P_ROOT)?;
    let kind = priv_u32(scope, this, P_KIND)?;
    let sel = priv_str(scope, this, P_SEL).unwrap_or_default();
    let ns = priv_get(scope, this, P_NS).filter(|v| v.is_string()).map(|v| v.to_rust_string_lossy(scope));
    // A collection that depends only on the tree and on tag names keys on the structure
    // revision, so an attribute write does not invalidate it; anything an attribute can
    // change keys on all writes.
    let structural = [Kind::DocByTag, Kind::ElByTag, Kind::Children, Kind::ChildNodes, Kind::DocByTagNs, Kind::ElByTagNs, Kind::Forms, Kind::Images, Kind::Scripts, Kind::Embeds, Kind::Applets, Kind::TableBodies, Kind::TableRows, Kind::RowCells, Kind::SectionRows]
        .iter()
        .any(|k| *k as u32 == kind);
    Some(((root, kind, sel, ns), structural))
}

/// The values this list currently answers, in order. `None` for a foreign receiver.
fn items<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    match lst_u32(scope, this, P_CLASS)? {
        CLASS_LIVE | CLASS_ALL => {
            let ids = live_ids(scope, this)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            Some(ids.iter().map(|&id| wrap_cached(scope, &op_state, id).into()).collect())
        }
        CLASS_STATIC => {
            let arr = lst_get(scope, this, P_ITEMS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
            let n = arr.length();
            Some((0..n).filter_map(|i| arr.get_index(scope, i)).collect())
        }
        CLASS_TOKENS => {
            let id = priv_u32(scope, this, DTL_ID)?;
            let attr = priv_str(scope, this, DTL_ATTR).unwrap_or_else(|| "class".to_string());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let toks = { let st = op_state.borrow(); dtl_tokens(&st.borrow::<HostDom>().0, id, &attr) };
            Some(toks.iter().map(|t| str_val(scope, t)).collect())
        }
        CLASS_ATTRS => {
            let eid = priv_u32(scope, this, NNM_EID)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let names = { let st = op_state.borrow(); st.borrow::<HostDom>().0.attribute_names(eid) };
            Some(names.iter().filter_map(|n| make_attr(scope, eid, n).map(|a| a.into())).collect())
        }
        _ => None,
    }
}

/// How many entries this list currently has. `None` for a foreign receiver.
fn item_count(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Option<usize> {
    match lst_u32(scope, this, P_CLASS)? {
        CLASS_LIVE | CLASS_ALL => live_ids(scope, this).map(|v| v.len()),
        CLASS_STATIC => lst_get(scope, this, P_ITEMS)
            .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())
            .map(|a| a.length() as usize),
        CLASS_TOKENS => {
            let id = priv_u32(scope, this, DTL_ID)?;
            let attr = priv_str(scope, this, DTL_ATTR).unwrap_or_else(|| "class".to_string());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            Some(dtl_tokens(&st.borrow::<HostDom>().0, id, &attr).len())
        }
        CLASS_ATTRS => {
            let eid = priv_u32(scope, this, NNM_EID)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            Some(st.borrow::<HostDom>().0.attribute_names(eid).len())
        }
        _ => None,
    }
}

/// One entry at `index`, or `None` for out of range / a foreign receiver.
fn item_at<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, index: u32) -> Option<v8::Local<'s, v8::Value>> {
    match lst_u32(scope, this, P_CLASS)? {
        CLASS_LIVE | CLASS_ALL => {
            let ids = live_ids(scope, this)?;
            let id = *ids.get(index as usize)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            Some(wrap_cached(scope, &op_state, id).into())
        }
        CLASS_STATIC => {
            let arr = lst_get(scope, this, P_ITEMS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
            if index >= arr.length() { return None; }
            arr.get_index(scope, index)
        }
        CLASS_TOKENS => {
            let id = priv_u32(scope, this, DTL_ID)?;
            let attr = priv_str(scope, this, DTL_ATTR).unwrap_or_else(|| "class".to_string());
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let tok = { let st = op_state.borrow(); dtl_tokens(&st.borrow::<HostDom>().0, id, &attr).get(index as usize).cloned() };
            tok.map(|t| str_val(scope, &t))
        }
        CLASS_ATTRS => {
            let eid = priv_u32(scope, this, NNM_EID)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let name = { let st = op_state.borrow(); st.borrow::<HostDom>().0.attribute_names(eid).get(index as usize).cloned() };
            name.and_then(|n| make_attr(scope, eid, &n).map(|a| a.into()))
        }
        _ => None,
    }
}

/// The SUPPORTED PROPERTY NAMES, in tree order, first occurrence winning: every element's `id`, plus an
/// HTML-namespace element's `name`. (`NamedNodeMap`'s are its attribute names.) An empty name is never
/// supported.
fn named_order(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> Vec<String> {
    match lst_u32(scope, this, P_CLASS) {
        Some(class @ (CLASS_LIVE | CLASS_ALL)) => {
            let Some(ids) = live_ids(scope, this) else { return Vec::new() };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            let dom = &st.borrow::<HostDom>().0;
            let mut out: Vec<String> = Vec::new();
            for &id in ids.iter() {
                if let Some(idv) = dom.get_attribute(id, "id").filter(|s| !s.is_empty()) {
                    if !out.iter().any(|n| n == idv) { out.push(idv.to_string()); }
                }
                // An `HTMLCollection`'s `name` names any HTML-namespace element; an
                // `HTMLAllCollection`'s names only an "all"-named one (`ALL_NAMED_TAGS`).
                let names_by_name = dom.is_html_ns(id)
                    && (class != CLASS_ALL || dom.tag_name(id).map(|t| ALL_NAMED_TAGS.contains(&t)).unwrap_or(false));
                if names_by_name {
                    if let Some(nm) = dom.get_attribute(id, "name").filter(|s| !s.is_empty()) {
                        if !out.iter().any(|n| n == nm) { out.push(nm.to_string()); }
                    }
                }
            }
            out
        }
        Some(CLASS_ATTRS) => {
            let Some(eid) = priv_u32(scope, this, NNM_EID) else { return Vec::new() };
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let st = op_state.borrow();
            st.borrow::<HostDom>().0.attribute_names(eid)
        }
        Some(CLASS_STATIC) => {
            let Some(names) = priv_get(scope, this, P_NAMES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else { return Vec::new() };
            let mut out = Vec::with_capacity(names.length() as usize);
            for i in 0..names.length() {
                if let Some(v) = names.get_index(scope, i) {
                    out.push(v.to_rust_string_lossy(scope));
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The value a supported name answers, or `None` when the name is not supported. For an
/// `HTMLFormControlsCollection` a name several controls share answers a `RadioNodeList` (HTML §4.10.3).
fn named_value<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    if name.is_empty() {
        return None; // no element can be named by ""
    }
    match lst_u32(scope, this, P_CLASS)? {
        class @ (CLASS_LIVE | CLASS_ALL) => {
            let ids = live_ids(scope, this)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let is_all = class == CLASS_ALL;
            let matches: Vec<u32> = {
                let st = op_state.borrow();
                let dom = &st.borrow::<HostDom>().0;
                ids.iter()
                    .copied()
                    .filter(|&id| {
                        if is_all {
                            return all_named_match(dom, id, name);
                        }
                        if dom.get_attribute(id, "id") == Some(name) { return true; }
                        dom.is_html_ns(id) && dom.get_attribute(id, "name") == Some(name)
                    })
                    .collect()
            };
            let first = *matches.first()?;
            if matches.len() > 1 {
                // HTML "get the 'all'-named elements": more than one match answers a LIVE
                // `HTMLCollection` over the same rule.
                if is_all {
                    let root = priv_u32(scope, this, P_ROOT)?;
                    return Some(all_named_collection(scope, this, root, name));
                }
                if iface_of(scope, this) == "HTMLFormControlsCollection" {
                    let root = priv_u32(scope, this, P_ROOT)?;
                    return Some(make_live(scope, "RadioNodeList", root, Kind::RadioGroup, name, None));
                }
            }
            Some(wrap_cached(scope, &op_state, first).into())
        }
        CLASS_ATTRS => {
            let eid = priv_u32(scope, this, NNM_EID)?;
            let op_state = deno_core::JsRuntime::op_state_from(scope);
            let has = { let st = op_state.borrow(); st.borrow::<HostDom>().0.get_attribute(eid, name).is_some() };
            if !has { return None; }
            make_attr(scope, eid, name).map(|a| a.into())
        }
        CLASS_STATIC => {
            let names = priv_get(scope, this, P_NAMES).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
            let values = priv_get(scope, this, P_NAMED).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok())?;
            for i in 0..names.length() {
                if names.get_index(scope, i).is_some_and(|v| v.to_rust_string_lossy(scope) == name) {
                    return values.get_index(scope, i);
                }
            }
            None
        }
        _ => None,
    }
}

/// Make `obj` -- a fresh instance of `iface`, whose template carries this module's interceptors ([`list_shape`])
/// -- a STATIC list of `items` with the supported property names `names`, each answering the value at the same
/// position of `named`: the plugin family (`core/html/plugins.rs`). Its indices are then
/// `{writable:false, enumerable:true, configurable:true}` and its names `[LegacyUnenumerableNamedProperties]`
/// (`enumerable:false`), neither can be redefined nor deleted, as Blink's interceptors answer -- where they were
/// own data properties, every one enumerable (Chrome 150).
pub(crate) fn adopt_static_named<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    obj: v8::Local<'s, v8::Object>,
    iface: &str,
    items: v8::Local<'s, v8::Array>,
    names: v8::Local<'s, v8::Array>,
    named: v8::Local<'s, v8::Array>,
) {
    debug_assert!(list_shape(iface).is_some(), "{iface} is adopted as a list but has no list shape");
    let c = int_val(scope, CLASS_STATIC as i64);
    lst_set(scope, obj, P_CLASS, c);
    let n = str_val(scope, iface);
    priv_set(scope, obj, P_IFACE, n);
    lst_set(scope, obj, P_ITEMS, items.into());
    priv_set(scope, obj, P_NAMES, names.into());
    priv_set(scope, obj, P_NAMED, named.into());
}

// ── the interceptors ────────────────────────────────────────────────────────────────────────────

fn cb_idx_get<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    match item_at(scope, args.holder(), index) {
        Some(v) => { rv.set(v); v8::Intercepted::kYes }
        None => v8::Intercepted::kNo,
    }
}

fn cb_idx_query<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let Some(n) = item_count(scope, args.holder()) else { return v8::Intercepted::kNo };
    if (index as usize) >= n {
        return v8::Intercepted::kNo;
    }
    // {writable:false, enumerable:true, configurable:true} — an indexed property of every one of the ten
    // but `HTMLOptionsCollection`, the one that declares an indexed SETTER, whose indices are writable.
    let holder = args.holder();
    let ro = if iface_is(scope, holder, "HTMLOptionsCollection") { v8::PropertyAttribute::NONE } else { v8::PropertyAttribute::READ_ONLY };
    let attrs = v8::Integer::new(scope, ro.as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

fn cb_idx_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let n = item_count(scope, args.holder()).unwrap_or(0);
    let arr = v8::Array::new(scope, n as i32);
    for i in 0..n {
        let k = v8::Integer::new_from_unsigned(scope, i as u32);
        arr.set_index(scope, i as u32, k.into());
    }
    rv.set(arr);
}

/// The indexed property setter: there is none, for any of the ten.
///
/// A SUPPORTED index is refused by V8's own read-only machinery -- the `query` interceptor reports it
/// `{writable:false}`, so a strict-mode store throws a TypeError and a sloppy one is a silent no-op,
/// which is Chrome's behaviour to the letter (its message is Blink's rather than V8's). This callback
/// therefore does not intercept those at all.
///
/// It exists for the index PAST THE END, which `query` reports absent: without it V8 would create a
/// real own property there and the collection would grow a key its enumerator never lists. Intercepting
/// makes that store a no-op.
///
/// Ruling: an OUT-OF-RANGE indexed store is silent in strict mode where Chrome throws
/// ("Failed to set an indexed property [99] on 'HTMLCollection': Indexed property setter is not
/// supported."). Blink produces that from `info.ShouldThrowOnError()`, and this build cannot:
/// `PropertyCallbackArguments::should_throw_on_error()` answers `false` unconditionally here -- measured
/// on a strict-mode store from both an arrow and a `function`, AND on `Object.defineProperty`, where V8
/// passes `Just(kThrowOnError)` itself. The one way to reach V8's own strict-mode throw is to have
/// `cb_idx_query` report every index as present, and that would make `99 in coll` and
/// `coll.hasOwnProperty(99)` answer TRUE, which Chrome answers false and which a bounds-checking page
/// reads far more often than it assigns past the end. Cost if wrong: `coll[99] = 1` inside `"use
/// strict"` does nothing instead of throwing (WPT dom/nodes
/// `Document-getElementsByTagName.html` "Shouldn't be able to set unsigned properties ... (strict
/// mode)", 1 subtest). The collection is unchanged either way, and no own property is created.
fn cb_idx_set<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, value: v8::Local<'s, v8::Value>, args: v8::PropertyCallbackArguments<'s>, _rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let holder = args.holder();
    // The exception: `HTMLOptionsCollection` has an indexed setter, and it answers every index --
    // `sel.options[sel.options.length] = new Option(t, v)` is the pre-`add()` way to append an option.
    if iface_is(scope, holder, "HTMLOptionsCollection") {
        options_set_index(scope, holder, "HTMLOptionsCollection", index, value);
        return v8::Intercepted::kYes;
    }
    let Some(n) = item_count(scope, holder) else { return v8::Intercepted::kNo };
    if (index as usize) < n {
        return v8::Intercepted::kNo; // read-only: V8 throws in strict mode, ignores in sloppy
    }
    v8::Intercepted::kYes
}

/// `[[DefineOwnProperty]]` on an index -- supported or not -- is refused, with Blink's message. WebIDL:
/// an interface with an indexed property getter and no indexed property SETTER returns false from
/// [[DefineOwnProperty]], and `Object.defineProperty` throws on a false return in either language mode.
fn cb_idx_define<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, desc: &v8::PropertyDescriptor, args: v8::PropertyCallbackArguments<'s>, _rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let holder = args.holder();
    if class_of(scope, holder.into()).is_none() {
        return v8::Intercepted::kNo;
    }
    let iface = iface_of(scope, holder);
    // WebIDL [[DefineOwnProperty]] with an indexed setter: a data descriptor runs the setter, an
    // accessor one is refused.
    if iface == "HTMLOptionsCollection" {
        if desc.has_value() {
            let v = desc.value();
            options_set_index(scope, holder, "HTMLOptionsCollection", index, v);
        } else {
            throw_type_error(scope, &format!("Failed to set an indexed property [{index}] on '{iface}': Accessor properties are not allowed."));
        }
        return v8::Intercepted::kYes;
    }
    throw_type_error(scope, &format!("Failed to set an indexed property [{index}] on '{iface}': Index property setter is not supported."));
    v8::Intercepted::kYes
}

/// Deleting a SUPPORTED index throws (Blink: "Index property deleter is not supported"); an index PAST
/// THE END deletes fine (WPT: "deletions out of range do not throw").
fn cb_idx_delete<'s>(scope: &mut v8::PinScope<'s, '_>, index: u32, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let holder = args.holder();
    let Some(n) = item_count(scope, holder) else { return v8::Intercepted::kNo };
    if (index as usize) >= n {
        let t = v8::Boolean::new(scope, true);
        rv.set(t);
        return v8::Intercepted::kYes;
    }
    // Refused. V8 turns a false [[Delete]] into the strict-mode TypeError itself (`should_throw_on_error`
    // is not set for an interceptor delete in this V8), so the message is V8's rather than Blink's; the
    // observable behaviour -- throws under "use strict", answers false in sloppy mode -- is Chrome's.
    let f = v8::Boolean::new(scope, false);
    rv.set(f);
    v8::Intercepted::kYes
}

fn key_string(scope: &mut v8::PinScope, key: v8::Local<v8::Name>) -> Option<String> {
    let v: v8::Local<v8::Value> = key.into();
    if !v.is_string() {
        return None; // a symbol is never a supported property name
    }
    Some(v.to_rust_string_lossy(scope))
}

fn cb_named_get<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    match named_value(scope, args.holder(), &name) {
        Some(v) => { rv.set(v); v8::Intercepted::kYes }
        None => v8::Intercepted::kNo,
    }
}

fn cb_named_query<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Integer>) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    if named_value(scope, args.holder(), &name).is_none() {
        return v8::Intercepted::kNo;
    }
    // [LegacyUnenumerableNamedProperties]: {writable:false, enumerable:false, configurable:true}.
    let attrs = v8::Integer::new(scope, (v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM).as_u32() as i32);
    rv.set(attrs);
    v8::Intercepted::kYes
}

fn cb_named_enum<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Array>) {
    let names = named_order(scope, args.holder());
    let arr = v8::Array::new(scope, names.len() as i32);
    for (i, n) in names.iter().enumerate() {
        let s = str_val(scope, n);
        arr.set_index(scope, i as u32, s);
    }
    rv.set(arr);
}

// There is no named property SETTER callback, deliberately. A supported name is reported
// `{writable:false}` by `cb_named_query`, so V8's own machinery refuses the store -- a TypeError under
// "use strict", a silent no-op in sloppy mode, and no own property either way, which is exactly Chrome.
// A name nothing answers never reaches an interceptor at all (NON_MASKING), so `coll.zzz = 1` is an
// ordinary expando.

/// Defining over a CURRENTLY-SUPPORTED name is refused with Blink's message; anything else -- an
/// expando, and `length` and the methods, which NON_MASKING keeps out of this callback entirely -- is
/// ordinary `defineProperty` (Chrome allows an own `length` on a collection, and so does this).
fn cb_named_define<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, _desc: &v8::PropertyDescriptor, args: v8::PropertyCallbackArguments<'s>, _rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    let holder = args.holder();
    if named_value(scope, holder, &name).is_none() {
        return v8::Intercepted::kNo;
    }
    let iface = iface_of(scope, holder);
    throw_type_error(scope, &format!("Failed to set a named property '{name}' on '{iface}': Named property setter is not supported."));
    v8::Intercepted::kYes
}

fn cb_named_delete<'s>(scope: &mut v8::PinScope<'s, '_>, key: v8::Local<'s, v8::Name>, args: v8::PropertyCallbackArguments<'s>, mut rv: v8::ReturnValue<v8::Boolean>) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else { return v8::Intercepted::kNo };
    let holder = args.holder();
    if named_value(scope, holder, &name).is_none() {
        return v8::Intercepted::kNo;
    }
    // Refused; V8 supplies the strict-mode TypeError (see `cb_idx_delete`). A named property has no
    // deleter in WebIDL unless the interface declares one, and none of the ten does.
    let f = v8::Boolean::new(scope, false);
    rv.set(f);
    v8::Intercepted::kYes
}

// ── the templates ───────────────────────────────────────────────────────────────────────────────

// The lists are made from their interfaces' templates, which carry these interceptors from the builder
// ([`list_shape`], `install::instance_shapes`). The two `ObjectTemplate`s every list was
// minted from before, one per shape and cached per isolate, are deleted; `document.all` keeps its own.

fn indexed_config<'s>() -> v8::IndexedPropertyHandlerConfiguration<'s> {
    v8::IndexedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(idx_get, holder, cb_idx_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(idx_query, holder, cb_idx_query).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(idx_keys, holder, cb_idx_enum).map_fn_to()))
        .setter_raw(crate::binding::refs::c(icpt!(idx_set, holder, cb_idx_set).map_fn_to()))
        .definer_raw(crate::binding::refs::c(icpt!(idx_define, holder, cb_idx_define).map_fn_to()))
        .deleter_raw(crate::binding::refs::c(icpt!(idx_delete, holder, cb_idx_delete).map_fn_to()))
}

/// The named-property interceptor every collection with a named getter carries.
///
/// NON_MASKING is WebIDL §3.9.4.1 step 5 made mechanical: the named getter is never entered for a
/// name the object or its prototype chain already has, so `coll.length` is the attribute, `coll.item`
/// is the method, and an expando the page wrote shadows a name the tree grows later. Measured on
/// `document.all` in Chrome with an `<input name="length">` and an `<input name="item">` in the page:
/// `document.all.length` answered the element COUNT and `document.all.item` answered the function,
/// while `namedItem("length")` and `namedItem("item")` answered the two inputs.
/// ONLY_INTERCEPT_STRINGS: a Symbol is never a supported property name.
fn named_config<'s>() -> v8::NamedPropertyHandlerConfiguration<'s> {
    v8::NamedPropertyHandlerConfiguration::new()
        .getter_raw(crate::binding::refs::c(icpt!(named_get, holder, cb_named_get).map_fn_to()))
        .query_raw(crate::binding::refs::c(icpt!(named_query, holder, cb_named_query).map_fn_to()))
        .enumerator_raw(crate::binding::refs::c(icpt!(named_keys, holder, cb_named_enum).map_fn_to()))
        .definer_raw(crate::binding::refs::c(icpt!(named_define, holder, cb_named_define).map_fn_to()))
        .deleter_raw(crate::binding::refs::c(icpt!(named_delete, holder, cb_named_delete).map_fn_to()))
        .flags(v8::PropertyHandlerFlags::NON_MASKING | v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS)
}

/// The lists this module makes, and whether each has a named getter as well as the indexed one (HTMLCollection
/// and its subclasses, NamedNodeMap): the shape the builder gives each one's instance template
/// (`install::instance_shapes`). `None` for any other interface.
pub(crate) fn list_shape(iface: &str) -> Option<bool> {
    match iface {
        "HTMLCollection" | "HTMLFormControlsCollection" | "HTMLOptionsCollection" | "NamedNodeMap" => Some(true),
        // the plugin family's three lists: their bodies are `core/html/plugins.rs`'s, their indexed and named
        // properties this module's interceptors ([`adopt_static_named`])
        "PluginArray" | "Plugin" | "MimeTypeArray" => Some(true),
        "NodeList" | "RadioNodeList" | "DOMTokenList" | "DOMRectList" | "DOMStringList" | "CSSRuleList" | "StyleSheetList" => {
            Some(false)
        }
        _ => None,
    }
}

/// The privates the engine keeps on an instance of list interface `iface` (beside the [`LstKey`] fields), declared
/// (as `undefined`) on its instance
/// template by the builder (`install::instance_shapes::declare_engine_slots`): every instance is then born with
/// them, and a mint's writes store into its own slots instead of each adding a property -- a map transition, and
/// a property-array grow, per write (an observed appendChild + removeChild, whose records each carry NodeLists:
/// 49,100 -> 44,100 instructions, 2026-10-23). Every reader here reads them with `get_private` and treats `undefined` as absent, which
/// is what an absent private reads as; none is ever asked `has_private` or deleted. Empty for any other interface.
pub(crate) fn engine_slot_names(iface: &str) -> &'static [&'static str] {
    match iface {
        // live- and static-capable lists
        "NodeList" | "HTMLCollection" => &[P_IFACE, P_ROOT, P_KIND, P_SEL, P_NS],
        // live-only lists
        "RadioNodeList" | "HTMLFormControlsCollection" | "HTMLOptionsCollection" => &[P_IFACE, P_ROOT, P_KIND, P_SEL, P_NS],
        // static-only lists
        "DOMRectList" | "DOMStringList" | "CSSRuleList" | "StyleSheetList" => &[P_IFACE],
        // the plugin family: static, with supported property names
        "PluginArray" | "Plugin" | "MimeTypeArray" => &[P_IFACE, P_NAMES, P_NAMED],
        "DOMTokenList" => &[P_IFACE, DTL_ID, DTL_ATTR],
        "NamedNodeMap" => &[P_IFACE, NNM_EID],
        _ => &[],
    }
}

/// A list interface's instance template: the indexed interceptor, and the named one when `named`.
pub(crate) fn configure_list(it: v8::Local<v8::ObjectTemplate>, named: bool) {
    it.set_indexed_property_handler(indexed_config());
    if named {
        it.set_named_property_handler(named_config());
    }
}

/// Mint a bare instance of `iface` -- from the interface's template, which carries the list's indexed (and
/// named) access ([`list_shape`]), in the realm of its prototype -- and the two privates every body reads.
///
/// The REALM's prototype for `iface` (`shell_proto`, the sealed map), never `window[iface]`: a page that
/// replaced or deleted the global still gets its lists from the interface the realm was created with,
/// which is what Chrome answers (measured: `F-list-realm-protos`). Reading the global
/// handed out lists on the page's function's prototype, or on none, and every one of them read as empty.
fn mint<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, class: u32) -> Option<v8::Local<'s, v8::Object>> {
    debug_assert!(list_shape(iface).is_some(), "{iface} is minted as a list but has no list shape");
    let proto = crate::binding::protos::shell_proto(scope, iface)?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let c = int_val(scope, class as i64);
    lst_set(scope, obj, P_CLASS, c);
    let n = crate::binding::protos::key_val(scope, iface);
    priv_set(scope, obj, P_IFACE, n);
    Some(obj)
}

/// A LIVE collection over the arena: it re-runs `kind` from `root` on every access and caches nothing.
pub(crate) fn make_live<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, root: u32, kind: Kind, sel: &str, ns: Option<&str>) -> v8::Local<'s, v8::Value> {
    let Some(obj) = mint(scope, iface, CLASS_LIVE) else { return v8::undefined(scope).into() };
    let r = int_val(scope, root as i64);
    priv_set(scope, obj, P_ROOT, r);
    let k = int_val(scope, kind as i64);
    priv_set(scope, obj, P_KIND, k);
    let s = str_val(scope, sel);
    priv_set(scope, obj, P_SEL, s);
    let nsv: v8::Local<v8::Value> = match ns {
        Some(n) => str_val(scope, n),
        None => v8::null(scope).into(),
    };
    priv_set(scope, obj, P_NS, nsv);
    obj.into()
}

/// A STATIC list over the values already in `arr` — `querySelectorAll`, `labels`, `cssRules`,
/// `getClientRects()`, `styleSheets`. The array lives on a Private, so the object's own keys stay
/// exactly the indices.
pub(crate) fn make_static<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, arr: v8::Local<'s, v8::Array>) -> v8::Local<'s, v8::Value> {
    let Some(obj) = mint(scope, iface, CLASS_STATIC) else { return arr.into() };
    lst_set(scope, obj, P_ITEMS, arr.into());
    obj.into()
}

/// Replace a static list's items in place, so a `[SameObject]` list (`document.styleSheets`) stays the
/// same object across reads while still reporting the current contents.
fn reseat_static(scope: &mut v8::PinScope, list: v8::Local<v8::Object>, arr: v8::Local<v8::Array>) {
    lst_set(scope, list, P_ITEMS, arr.into());
}

/// A `DOMTokenList` over `attr` of element `eid` (`classList`, `relList`, `sandbox`, `htmlFor`, …).
pub(crate) fn make_token_list<'s>(scope: &mut v8::PinScope<'s, '_>, eid: u32, attr: &str) -> Option<v8::Local<'s, v8::Object>> {
    let obj = mint(scope, "DOMTokenList", CLASS_TOKENS)?;
    let idv = int_val(scope, eid as i64);
    priv_set(scope, obj, DTL_ID, idv);
    let av = str_val(scope, attr);
    priv_set(scope, obj, DTL_ATTR, av);
    Some(obj)
}

/// A `NamedNodeMap` over element `eid`'s attributes.
pub(crate) fn make_named_node_map<'s>(scope: &mut v8::PinScope<'s, '_>, eid: u32) -> Option<v8::Local<'s, v8::Object>> {
    let obj = mint(scope, "NamedNodeMap", CLASS_ATTRS)?;
    let idv = int_val(scope, eid as i64);
    priv_set(scope, obj, NNM_EID, idv);
    Some(obj)
}

// ── document.all ────────────────────────────────────────────────────────────────────────────────

/// `document.all` for the document rooted at `root`: a LIVE list of every element in the document,
/// with THREE things no other collection here has.
///
/// 1. **It is UNDETECTABLE** (`[[IsHTMLDDA]]`): `typeof document.all` is `"undefined"`,
///    `Boolean(document.all)` is `false`, `document.all == null` is `true` — and
///    `document.all === undefined` is `false` and `document.all.length` is the element count. That
///    is a bit on the V8 map, not anything JS can express; see `binding/v8_legacy.rs`.
/// 2. **It is CALLABLE**: `document.all(0)`, `document.all("id")`, `Reflect.apply(document.all, …)`.
/// 3. **Its named rule is narrower** than an `HTMLCollection`'s — only `ALL_NAMED_TAGS` are named by
///    their `name` attribute — and a name that matches more than one element answers an
///    `HTMLCollection` rather than the first match.
///
/// All three are `HTMLAllCollection`'s instance template's ([`configure_all`], applied by the builder:
/// D1), so the collection is an instance of the interface from birth. `Document.all`
/// caches it (`P_DOC_ALL`): one per document, not one per read.
///
/// ONE TEMPLATE FOR EVERY DOCUMENT. There was a template per document, carrying a per-document token
/// as the legacy caller's `data`, on the premise that `args.this()` in a call-as-function handler is
/// the receiver the caller chose -- which WPT's "legacy caller with arbitrary this value" sets to
/// `undefined`, `null`, `{}` and `document.body` in turn. Measured on V8 150, it is not:
/// V8 hands the handler the CALLED object as `this`, for `document.all(0)` and for `Reflect.apply(
/// document.all, x, ..)` with x = `undefined`, `{}`, `document.body` and another document's
/// collection, 7 of 7 -- so the handler reads `this` ([`cb_all_call`]) and the token is gone.
pub(crate) fn make_all<'s>(scope: &mut v8::PinScope<'s, '_>, root: u32) -> Option<v8::Local<'s, v8::Object>> {
    let proto = crate::binding::protos::iface_object(scope, "HTMLAllCollection")
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
        .and_then(|c| get_prop(scope, c, "prototype"))
        .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())?;
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    let c = int_val(scope, CLASS_ALL as i64);
    lst_set(scope, obj, P_CLASS, c);
    let n = crate::binding::protos::key_val(scope, "HTMLAllCollection");
    priv_set(scope, obj, P_IFACE, n);
    let r = int_val(scope, root as i64);
    priv_set(scope, obj, P_ROOT, r);
    let k = int_val(scope, Kind::DocAll as i64);
    priv_set(scope, obj, P_KIND, k);
    let s = str_val(scope, "");
    priv_set(scope, obj, P_SEL, s);
    let nul: v8::Local<v8::Value> = v8::null(scope).into();
    priv_set(scope, obj, P_NS, nul);
    Some(obj)
}

/// `HTMLAllCollection`'s instance template: a list's indexed and named access, UNDETECTABLE, and CALLABLE
/// through the legacy caller ([`cb_all_call`]) -- what `document.all` is ([`make_all`]).
pub(crate) fn configure_all(scope: &mut v8::PinScope, it: v8::Local<v8::ObjectTemplate>) {
    it.set_indexed_property_handler(indexed_config());
    it.set_named_property_handler(named_config());
    crate::binding::v8_legacy::mark_as_undetectable(it);
    let none: v8::Local<v8::Value> = v8::undefined(scope).into();
    crate::binding::v8_legacy::set_call_as_function_handler(it, cb_all_call, none);
}

/// The `HTMLCollection` a repeated name answers, one per (`document.all`, name) pair.
///
/// Chrome answers the SAME object for a repeated lookup — `document.all.m === document.all.m` is
/// `true`, measured on Chrome 150.0.7871.46 — so the collection is cached on the `document.all`
/// instance. The dictionary has a NULL prototype: a page-chosen name is used as a key, and `__proto__`
/// on an ordinary object would be the setter rather than an entry.
fn all_named_collection<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
    root: u32,
    name: &str,
) -> v8::Local<'s, v8::Value> {
    let cache = match priv_get(scope, this, P_ALL_CACHE).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        Some(c) => c,
        None => {
            let c = v8::Object::new(scope);
            let nul: v8::Local<v8::Value> = v8::null(scope).into();
            c.set_prototype(scope, nul);
            priv_set(scope, this, P_ALL_CACHE, c.into());
            c
        }
    };
    let key = str_val(scope, name);
    if let Some(hit) = cache.get(scope, key).filter(|v| v.is_object()) {
        return hit;
    }
    let coll = make_live(scope, "HTMLCollection", root, Kind::DocAllNamed, name, None);
    cache.set(scope, key, coll);
    coll
}

/// "Array index property name" (ECMA-262): the canonical decimal spelling of an integer strictly
/// below 2^32-1. `"0"` is one; `"00"`, `"042"`, `"+0"`, `"-0"`, `" 0"`, `"1.7"` and `"4294967295"`
/// are not — each of those reaches the NAMED lookup instead, which is how
/// `document.all["4294967295"]` answers `<div id="4294967295">` while `document.all["4294967294"]`
/// (a real index, out of range) answers `undefined`.
fn array_index_name(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    let n: u64 = s.parse().ok()?;
    if n >= u32::MAX as u64 {
        return None; // 2^32-1 is NOT an array index
    }
    Some(n as u32)
}

/// HTML's "get the 'all'-indexed or 'all'-named element(s)": an argument that stringifies to an array
/// index property name is an INDEX, anything else is a NAME. Shared by `item()` and the legacy
/// caller, which is why `document.all("0")` and `document.all.item("0")` are both `<html>` while
/// `document.all.namedItem("0")` is the element with `id="0"`.
fn all_index_or_name<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
    arg: v8::Local<v8::Value>,
) -> Option<v8::Local<'s, v8::Value>> {
    let s = arg.to_string(scope)?.to_rust_string_lossy(scope);
    match array_index_name(&s) {
        Some(i) => item_at(scope, this, i),
        None => named_value(scope, this, &s),
    }
}

/// `HTMLAllCollection.prototype.length`.
fn cb_all_length_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = all_receiver(scope, args.this()) else { return };
    let n = item_count(scope, this).unwrap_or(0);
    rv.set_uint32(n as u32);
}

/// `item(nameOrIndex)` — `length` 0, and the argument is an OPTIONAL `DOMString`, so `item()` and
/// `item(undefined)` are the same call and both answer `null` (WPT `htmlallcollection.html`, and
/// Chrome agrees). `item(null)` is the string `"null"`, which is a NAME.
fn cb_all_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = all_receiver(scope, args.this()) else { return };
    let arg = args.get(0);
    if args.length() == 0 || arg.is_undefined() {
        rv.set_null();
        return;
    }
    match all_index_or_name(scope, this, arg) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `namedItem(name)` — a REQUIRED `DOMString`, so it is never the index path: `namedItem("0")` is the
/// element whose id is `"0"` and `namedItem(undefined)` is the element whose id is `"undefined"`. The
/// table's `required 1` supplies the WebIDL arity TypeError for `namedItem()`.
fn cb_all_named_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = all_receiver(scope, args.this()) else { return };
    let Some(name) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    match named_value(scope, this, &name) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// The LEGACY CALLER: `document.all(0)`, `document.all("id")`. Same rule as `item()`.
///
/// Its receiver is `args.this()`: V8 hands a call-as-function handler the object called, whatever
/// `this` the call supplied (WPT: "legacy caller with arbitrary this value"; measured, see [`make_all`]).
///
/// Ruling: the HTML spec's SECOND argument (`subIndex`, "return the subIndexth element of the
/// collection this name answers") is IGNORED, because Chrome ignores it. Measured 2026-09-21 on
/// Chrome 150.0.7871.46, a page with two `<img name="m">`: `document.all("m", 0)`,
/// `document.all("m", 1)` and `document.all("m", 2)` each answered the two-element `HTMLCollection`,
/// not an element. §5 makes the installed browser the truth source, and WPT's
/// `htmlallcollection.html` does not test `subIndex` at all. Cost if wrong: a page that passes a
/// second argument gets the collection where the spec would give it a member — and would get the same
/// from Chrome.
fn cb_all_call(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if lst_u32(scope, this, P_CLASS) != Some(CLASS_ALL) {
        rv.set_null();
        return;
    }
    let arg = args.get(0);
    if args.length() == 0 || arg.is_undefined() {
        rv.set_null();
        return;
    }
    match all_index_or_name(scope, this, arg) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `Document.all` — `[SameObject]`-shaped: Chrome answers the same object every read
/// (`document.all === document.all`), so the instance is cached on the Document.
pub(crate) fn cb_doc_all(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let this = args.this();
    if let Some(hit) = priv_get(scope, this, P_DOC_ALL).filter(|v| v.is_object()) {
        rv.set(hit);
        return;
    }
    let root = doc_root_of(scope, this);
    // made in the document's realm, whichever realm's getter reads it first ([`in_relevant_realm`]): Chrome 152
    // (2026-10-23, probe r2/l3/r1/www/a1.html) answers an iframe's `Document` `all` getter called on the top
    // document first with the TOP realm's `HTMLAllCollection`, and the top getter on the frame's document with the
    // frame's
    let Some(all) = in_relevant_realm(scope, this, |scope| make_all(scope, root)) else { return };
    priv_set(scope, this, P_DOC_ALL, all.into());
    rv.set(all.into());
}

/// `HTMLAllCollection`'s three rows.
pub fn install_all_collection(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => {
            let g = build_fn(scope, cb_all_length_get, "get length", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "item" => method_row(scope, proto, member, cb_all_item),
        "namedItem" => method_row(scope, proto, member, cb_all_named_item),
        other => debug_assert!(false, "install_all_collection: unexpected member {other}"),
    }
}

// ── the member bodies ───────────────────────────────────────────────────────────────────────────

/// The receiver as one of our lists, or a TypeError "Illegal invocation" (WebIDL: a platform-object
/// member entered with a receiver that does not implement the interface). `None` => already thrown.
fn receiver<'s>(scope: &mut v8::PinScope, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    // `CLASS_ALL` is deliberately excluded: `document.all` does not implement `HTMLCollection`, and
    // `HTMLCollection.prototype.item.call(document.all, 0)` is `TypeError: Illegal invocation` in
    // Chrome 150.0.7871.46 (measured 2026-09-21).
    if matches!(lst_u32(scope, this, P_CLASS), Some(c) if c != CLASS_ALL) {
        return Some(this);
    }
    throw_type_error(scope, "Illegal invocation");
    None
}

/// The receiver as `document.all`, or a TypeError "Illegal invocation" — the mirror of [`receiver`].
/// `HTMLAllCollection.prototype`'s three rows accept nothing else, which is Chrome: the `length`
/// getter and `item` both answer `Illegal invocation` for a `document.images` receiver (measured).
fn all_receiver<'s>(scope: &mut v8::PinScope, this: v8::Local<'s, v8::Object>) -> Option<v8::Local<'s, v8::Object>> {
    if lst_u32(scope, this, P_CLASS) == Some(CLASS_ALL) {
        return Some(this);
    }
    throw_type_error(scope, "Illegal invocation");
    None
}

fn cb_length_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let n = item_count(scope, this).unwrap_or(0);
    rv.set_uint32(n as u32);
}

/// `item(index)` — the INDEXED GETTER invoked directly, not a property read: an expando cannot shadow
/// it, and `unsigned long` means ToUint32, so `item(-1)` is 4294967295 and out of range.
fn cb_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some(i) = args.get(0).uint32_value(scope) else { return };
    match item_at(scope, this, i) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// `namedItem(name)` — the NAMED GETTER invoked directly. `namedItem("0")` is the element with id "0",
/// never index 0, and `namedItem("")` is null.
fn cb_named_item(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some(name) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    match named_value(scope, this, &name) {
        Some(v) => rv.set(v),
        None => rv.set_null(),
    }
}

/// The `<select>` (or `<datalist>`) an `HTMLOptionsCollection` is a view of.
fn options_owner<'s>(scope: &mut v8::PinScope<'s, '_>, this: v8::Local<v8::Object>) -> Option<(u32, v8::Local<'s, v8::Object>)> {
    let root = priv_u32(scope, this, P_ROOT)?;
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    Some((root, wrap_cached(scope, &op_state, root)))
}

/// HTML's own ceiling on growing an `HTMLOptionsCollection` — "If the given value is greater than
/// 100,000, then return" (§4.10.7, the `length` setter, step 2.1). It is a CAP ON THE VALUE, not on
/// the difference, which is why setting 100,001 on a 3-option `<select>` is a no-op while setting
/// 100,001 on a 100,002-option one truncates.
///
/// MEASURED IN CHROME 150.0.7871.46 on 2026-09-12, `--headless=new` over a loopback server, on a
/// `<select>` with three options — the same page WPT uses:
///
/// ```text
///   -1                => length 3      (0 ms)      ToUint32(-1) is 4294967295, so: over the cap
///   100001            => length 3      (0 ms)
///   MAX_SAFE_INTEGER  => length 3      (0 ms)
///   2**32 - 1         => length 3      (0 ms)
///   100000            => length 100000 (77 ms)     at the cap, so it grows
///   +2 appendChild    => length 100002             the cap does not bound appendChild
///   100001            => length 100001 (5 ms)      value < current, so it truncates
/// ```
const OPTIONS_LENGTH_CAP: u32 = 100_000;

fn cb_options_length_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some(want) = args.get(0).uint32_value(scope) else { return };
    options_set_length(scope, this, want);
}

/// The `length` setter's body, for either receiver (`options.length = n`, `select.length = n`).
pub(crate) fn options_set_length(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, want: u32) {
    let Some((_root, select)) = options_owner(scope, this) else { return };
    let have = item_count(scope, this).unwrap_or(0) as u32;
    // WHATWG §4.10.7 "setting length": truncate from the end, or append that many empty `<option>`s.
    //
    // THE CAP IS NOT AN OPTIMISATION, IT IS THE SPEC — and leaving it out did not merely answer a
    // question wrongly, it wedged the process. `length` is an `unsigned long`, so
    // `options.length = -1` arrives here as ToUint32(-1) = 4,294,967,295 and the append loop below
    // asked for four billion `<option>` elements. `html/select/options-length-too-large.html` does
    // exactly that on its FIRST line: `scripts/wpt-tiers.sh core` shard 2/11 stopped there and spun
    // a core with flat RSS (137 MB, unchanged over 90 s) until the per-shard deadline killed it, so
    // the tier could not be measured. Isolated to this one statement: `options.length = -1` on a
    // three-option `<select>`, one `eval`, no WPT runner, still running at 90 s.
    if want > have {
        if want > OPTIONS_LENGTH_CAP {
            return;
        }
    } else if want < have {
        for _ in want..have {
            let n = item_count(scope, this).unwrap_or(0);
            if n == 0 { break; }
            let Some(last) = item_at(scope, this, (n - 1) as u32) else { break };
            // from ITS parent (an `<optgroup>` for a grouped option), as Blink's `setLength` removes it -- by
            // the realm's own steps, as every tree step below
            crate::binding::protos::pristine_remove(scope, last);
        }
        return;
    }
    let Some(doc) = global_get(scope, "document") else { return };
    for _ in have..want {
        let tag = str_val(scope, "option");
        let Some(opt) = crate::binding::protos::pristine_call(scope, doc, "Document.createElement", &[tag]) else { return };
        crate::binding::protos::pristine_call(scope, select.into(), "Node.appendChild", &[opt]);
    }
}

fn cb_options_selected_index_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some((_root, select)) = options_owner(scope, this) else { rv.set_int32(-1); return };
    match crate::binding::protos::pristine_get(scope, select.into(), "HTMLSelectElement.selectedIndex") {
        Some(v) => rv.set(v),
        None => rv.set_int32(-1),
    }
}

fn cb_options_selected_index_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some((_root, select)) = options_owner(scope, this) else { return };
    let v = args.get(0);
    crate::binding::protos::pristine_set(scope, select.into(), "HTMLSelectElement.selectedIndex", v);
}

/// `add(element, before)` — insert an `<option>`/`<optgroup>` before `before` (an element, or an index),
/// appending when it is absent or null. The insertion goes through `insertBefore`/`appendChild`, so the
/// MutationObserver and range notifications fire exactly as they do for a script's own insertion.
fn cb_options_add(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    options_add(scope, this, "HTMLOptionsCollection", args.get(0), args.get(1));
}

/// `add`'s body, for either receiver. `iface` names the receiver's interface in the TypeError.
///
/// Measured in Chrome 151 (`tools/realweb/seed/sel`): `select.add(div)` throws a
/// TypeError -- the argument is the WebIDL union `(HTMLOptionElement or HTMLOptGroupElement)` -- and
/// `select.add(opt, optionInsideAnOptgroup)` throws `NotFoundError`, because Blink inserts into the
/// SELECT before `before` (`HTMLSelectElement::add` -> `InsertBefore`), and an option in an optgroup
/// is not the select's child. `insertBefore` on the select is therefore the whole insertion.
pub(crate) fn options_add(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str, element: v8::Local<v8::Value>, before: v8::Local<v8::Value>) {
    let Some((_root, select)) = options_owner(scope, this) else { return };
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el_ok = crate::binding::protos::unwrap_id(scope, element).is_some_and(|id| {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.is_html_ns(id) && matches!(dom.tag_name(id), Some("option" | "optgroup"))
    });
    if !el_ok {
        throw_type_error(scope, &format!("Failed to execute 'add' on '{iface}': The provided value is not of type '(HTMLOptGroupElement or HTMLOptionElement)'."));
        return;
    }
    // `(HTMLElement or long)? before`: an HTML element is itself; anything else non-null converts
    // to a `long` (ToInt32) and names the option at that index -- none (negative, or past the end)
    // appends.
    let before_el = crate::binding::protos::unwrap_id(scope, before).filter(|&id| {
        let st = op_state.borrow();
        st.borrow::<HostDom>().0.is_html_ns(id)
    });
    let reference: v8::Local<v8::Value> = if before.is_undefined() || before.is_null() {
        v8::null(scope).into()
    } else if before_el.is_some() {
        before
    } else {
        let Some(i) = before.int32_value(scope) else { return };
        let hit = if i >= 0 { item_at(scope, this, i as u32) } else { None };
        hit.unwrap_or_else(|| v8::null(scope).into())
    };
    crate::binding::protos::pristine_call(scope, select.into(), "Node.insertBefore", &[element, reference]);
}

/// `remove(index)` — remove the option at `index`; out of range is a no-op (HTML §4.10.7).
fn cb_options_remove(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some(i) = args.get(0).int32_value(scope) else { return };
    options_remove(scope, this, i);
}

/// `remove(index)`'s body, for either receiver. `index` is a WebIDL `long`: a negative one is a no-op,
/// never ToUint32's wrap to a small positive index.
pub(crate) fn options_remove(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, index: i32) {
    if index < 0 { return; }
    let Some((_root, select)) = options_owner(scope, this) else { return };
    let Some(opt) = item_at(scope, this, index as u32) else { return };
    // The option's own parent, which is an `<optgroup>` for a grouped option: "remove the indexth
    // element in the collection from its parent" (HTML §4.10.7).
    let parent = crate::binding::protos::pristine_get(scope, opt, "Node.parentNode");
    let parent = parent.filter(|p| p.is_object()).unwrap_or_else(|| select.into());
    crate::binding::protos::pristine_call(scope, parent, "Node.removeChild", &[opt]);
}

/// The indexed property SETTER both `HTMLOptionsCollection` and `HTMLSelectElement` declare
/// (`setter undefined (unsigned long index, HTMLOptionElement? option)`), as Blink's
/// `HTMLSelectElement::SetOption` runs it -- measured in Chrome 151: on a two-option
/// select, `select[3] = new Option("H", "h")` leaves the values `a,b,,h`. An index past the end first
/// grows the list to `index` empty options (`setLength`), then appends; an index inside it removes
/// the option there and inserts before the one that followed it; `null` is `remove(index)`.
/// A value that is not an `<option>` is the WebIDL TypeError. Beyond the length cap nothing happens
/// (Blink logs a console warning and returns).
pub(crate) fn options_set_index(scope: &mut v8::PinScope, this: v8::Local<v8::Object>, iface: &str, index: u32, value: v8::Local<v8::Value>) {
    if value.is_null_or_undefined() {
        if index <= i32::MAX as u32 {
            options_remove(scope, this, index as i32);
        }
        return;
    }
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let is_option = crate::binding::protos::unwrap_id(scope, value).is_some_and(|id| {
        let st = op_state.borrow();
        let dom = &st.borrow::<HostDom>().0;
        dom.is_html_ns(id) && dom.tag_name(id) == Some("option")
    });
    if !is_option {
        throw_type_error(scope, &format!("Failed to set an indexed property [{index}] on '{iface}': parameter 2 is not of type 'HTMLOptionElement'."));
        return;
    }
    let len = item_count(scope, this).unwrap_or(0) as u64;
    if index >= OPTIONS_LENGTH_CAP || len + (index as u64).saturating_sub(len) + 1 > OPTIONS_LENGTH_CAP as u64 {
        return;
    }
    let mut before: v8::Local<v8::Value> = v8::null(scope).into();
    if (index as u64) > len {
        options_set_length(scope, this, index);
    } else if (index as u64) < len {
        if let Some(next) = item_at(scope, this, index + 1) {
            before = next;
        }
        options_remove(scope, this, index as i32);
    }
    options_add(scope, this, iface, value, before);
}

/// The options collection `HTMLSelectElement`'s list members act on: they act "like [their] namesake
/// method on that same select element's options collection" (HTML §4.10.7), so the `<select>` rows
/// reach the SAME bodies through the live collection over that select -- one rule, two receivers.
pub(crate) fn select_options<'s>(scope: &mut v8::PinScope<'s, '_>, select: u32) -> Option<v8::Local<'s, v8::Object>> {
    owned_options(scope, select, "HTMLOptionsCollection")
}

/// The `[SameObject]` `options` of a `<select>` (an `HTMLOptionsCollection`) or a `<datalist>` (an
/// `HTMLCollection`): minted on first read and kept on the element's wrapper under a Private, so
/// `sel.options === sel.options` (Chrome 151: true for both) and every read through it -- `sel[i]`,
/// `sel.length` -- lands on ONE live-ids cache entry instead of recomputing the list per access.
pub(crate) fn owned_options<'s>(scope: &mut v8::PinScope<'s, '_>, owner: u32, iface: &str) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el = wrap_cached(scope, &op_state, owner);
    let key = crate::binding::protos::private_key(scope, "korvus.options");
    if let Some(v) = el.get_private(scope, key) {
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            return Some(o);
        }
    }
    let o = v8::Local::<v8::Object>::try_from(make_live(scope, iface, owner, Kind::Options, "", None)).ok()?;
    el.set_private(scope, key, o.into());
    Some(o)
}

/// `select.selectedOptions`: ONE live HTMLCollection per select (`[SameObject]`), kept on the element.
pub(crate) fn owned_selected_options<'s>(scope: &mut v8::PinScope<'s, '_>, owner: u32) -> Option<v8::Local<'s, v8::Object>> {
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let el = wrap_cached(scope, &op_state, owner);
    let key = crate::binding::protos::private_key(scope, "korvus.selectedOptions");
    if let Some(v) = el.get_private(scope, key) {
        if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
            return Some(o);
        }
    }
    let o = v8::Local::<v8::Object>::try_from(make_live(scope, "HTMLCollection", owner, Kind::SelectedOptions, "", None)).ok()?;
    el.set_private(scope, key, o.into());
    Some(o)
}

/// How many options the collection holds.
pub(crate) fn options_len(scope: &mut v8::PinScope, coll: v8::Local<v8::Object>) -> usize {
    item_count(scope, coll).unwrap_or(0)
}

/// The option at `index`, or `None` past the end.
pub(crate) fn options_item<'s>(scope: &mut v8::PinScope<'s, '_>, coll: v8::Local<v8::Object>, index: u32) -> Option<v8::Local<'s, v8::Value>> {
    item_at(scope, coll, index)
}

/// The collection's named getter: the first option whose `id`, or `name`, is `name`.
pub(crate) fn options_named<'s>(scope: &mut v8::PinScope<'s, '_>, coll: v8::Local<v8::Object>, name: &str) -> Option<v8::Local<'s, v8::Value>> {
    named_value(scope, coll, name)
}

/// `RadioNodeList.value`: the checked radio's value, or "" when none is checked (HTML §4.10.3).
fn cb_radio_value_get(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some(vals) = items(scope, this) else { return };
    for v in vals {
        let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { continue };
        if !is_radio_input(scope, o) { continue; }
        let checked = crate::binding::protos::pristine_get(scope, o.into(), "HTMLInputElement.checked").map(|c| c.boolean_value(scope)).unwrap_or(false);
        if checked {
            if let Some(val) = crate::binding::protos::pristine_get(scope, o.into(), "HTMLInputElement.value") { rv.set(val); return; }
        }
    }
    let e = str_val(scope, "");
    rv.set(e);
}

/// Setting `RadioNodeList.value` checks the first radio whose value equals it (HTML §4.10.3).
fn cb_radio_value_set(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let want = args.get(0).to_rust_string_lossy(scope);
    let Some(vals) = items(scope, this) else { return };
    for v in vals {
        let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { continue };
        if !is_radio_input(scope, o) { continue; }
        let val = crate::binding::protos::pristine_get(scope, o.into(), "HTMLInputElement.value").map(|x| x.to_rust_string_lossy(scope)).unwrap_or_default();
        if val == want {
            let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
            crate::binding::protos::pristine_set(scope, o.into(), "HTMLInputElement.checked", t);
            return;
        }
    }
}

/// An `<input>` in the Radio Button state, read with the realm's own `type` (never the page's).
fn is_radio_input(scope: &mut v8::PinScope, o: v8::Local<v8::Object>) -> bool {
    let is_input = crate::page::tag_name_of(scope, o.into()).is_some_and(|t| t.eq_ignore_ascii_case("input"));
    is_input && crate::binding::protos::pristine_get(scope, o.into(), "HTMLInputElement.type").is_some_and(|t| t.to_rust_string_lossy(scope) == "radio")
}

// ── the install fns a table row names ───────────────────────────────────────────────────────────


/// The shared list members: `length` (readonly), `item`, `namedItem`. Named by
/// `HTMLCollection`, `NodeList`, `NamedNodeMap`, `CSSRuleList`, `DOMRectList` and `StyleSheetList`.
pub fn install_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => {
            let g = build_fn(scope, cb_length_get, "get length", 0);
            define_accessor(scope, proto, member, g, None);
        }
        "item" => method_row(scope, proto, member, cb_item),
        "namedItem" => method_row(scope, proto, member, cb_named_item),
        other => debug_assert!(false, "install_list: unexpected member {other}"),
    }
}

/// `DOMStringList`'s own member: `contains(string)` -- true when the snapshot holds that exact string.
pub fn install_dom_string_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "contains" => method_row(scope, proto, member, cb_contains),
        other => debug_assert!(false, "install_dom_string_list: unexpected member {other}"),
    }
}

/// `DOMStringList.contains(string)`: an exact match against the static items (WebIDL converts the
/// argument with ToString first, so `contains(1)` looks for `"1"`).
fn cb_contains(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(this) = receiver(scope, args.this()) else { return };
    let Some(needle) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let mut found = false;
    if let Some(arr) = lst_get(scope, this, P_ITEMS).and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) {
        for i in 0..arr.length() {
            if let Some(v) = arr.get_index(scope, i) {
                if v.to_rust_string_lossy(scope) == needle {
                    found = true;
                    break;
                }
            }
        }
    }
    rv.set_bool(found);
}

/// `HTMLOptionsCollection`'s four own members: a WRITABLE `length`, `selectedIndex`, `add`, `remove`.
pub fn install_options_collection(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    match member.name {
        "length" => {
            let g = build_fn(scope, cb_length_get, "get length", 0);
            let s = build_fn(scope, cb_options_length_set, "set length", 1);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "selectedIndex" => {
            let g = build_fn(scope, cb_options_selected_index_get, "get selectedIndex", 0);
            let s = build_fn(scope, cb_options_selected_index_set, "set selectedIndex", 1);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "add" => method_row(scope, proto, member, cb_options_add),
        "remove" => method_row(scope, proto, member, cb_options_remove),
        other => debug_assert!(false, "install_options_collection: unexpected member {other}"),
    }
}

/// `RadioNodeList.value` — read/write the checked radio.
pub fn install_radio_node_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    debug_assert_eq!(member.name, "value");
    let g = build_fn(scope, cb_radio_value_get, "get value", 0);
    let s = build_fn(scope, cb_radio_value_set, "set value", 1);
    define_accessor(scope, proto, member, g, Some(s));
}

// ── the ten tables ──────────────────────────────────────────────────────────────────────────────
//
// Member names, order, kinds, lengths and required-argument counts are the capture's
// (`plugins/korvus-persona/bundles/.../surface.json`), which headless Chrome on this machine confirms
// member-for-member (2026-09-09). Every one of the ten owns `Symbol.iterator` === `Array.prototype
// .values` (writable, NOT enumerable, configurable) because it has an indexed property getter; the
// three that are `iterable<>` in IDL (NodeList, RadioNodeList through it, DOMTokenList) also own
// `entries`/`keys`/`values`/`forEach`, and they are `Array.prototype`'s own function objects — not
// approximations of them, the same objects, which is what Chrome answers.

webapi::interface! {
    "HTMLCollection" [interface, window] {
        ctor "HTMLCollection" len 0 = stub,
        attr "length" : Integer readonly = install_list,
        method "item" : Object len 1 required 1 nullable = install_list,
        method "namedItem" : Object len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    // `document.all`'s interface. NOT a subclass of `HTMLCollection` — measured in Chrome
    // 150.0.7871.46, `Object.getPrototypeOf(HTMLAllCollection.prototype)` is `Object.prototype`, and
    // its own keys are exactly `length,item,namedItem,constructor` plus
    // `Symbol.toStringTag` ("HTMLAllCollection", non-writable non-enumerable configurable) and
    // `Symbol.iterator` (=== `Array.prototype.values`). `item` has `length` 0 and NO required
    // argument; `namedItem` has `length` 1 and one, so `namedItem()` is a WebIDL TypeError while
    // `item()` answers null.
    "HTMLAllCollection" [interface, window] {
        ctor "HTMLAllCollection" len 0 = stub,
        attr "length" : Integer readonly = install_all_collection,
        method "item" : Object len 0 nullable = install_all_collection,
        method "namedItem" : Object len 1 required 1 nullable = install_all_collection,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "HTMLFormControlsCollection" : "HTMLCollection" [interface, window] {
        ctor "HTMLFormControlsCollection" len 0 = stub,
        method "namedItem" : Object len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "HTMLOptionsCollection" : "HTMLCollection" [interface, window] {
        ctor "HTMLOptionsCollection" len 0 = stub,
        attr "length" : Integer = install_options_collection,
        attr "selectedIndex" : Integer = install_options_collection,
        method "add" : Undefined len 1 required 1 = install_options_collection,
        method "remove" : Undefined len 1 required 1 = install_options_collection,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    // `[Exposed=(Window,Worker)]` -- IndexedDB's `objectStoreNames` / `indexNames` (which workers have):
    // `length`, `contains`, `item`, in the capture's order, over a STATIC snapshot of names. WebIDL gives
    // an interface with an indexed getter and an integer `length` the value iterator, so `@@iterator` is
    // `Array.prototype.values` (the bundle records `iterable: true`); there is no `iterable<>` declaration,
    // so no `entries`/`keys`/`values`/`forEach`.
    "DOMStringList" [interface, both] {
        ctor "DOMStringList" len 0 = stub,
        attr "length" : Integer readonly = install_list,
        method "contains" : Boolean len 1 required 1 = install_dom_string_list,
        method "item" : DomString len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "NodeList" [interface, window] {
        ctor "NodeList" len 0 = stub,
        method "entries" : Object len 0 = array_proto_fn,
        method "keys" : Object len 0 = array_proto_fn,
        method "values" : Object len 0 = array_proto_fn,
        method "forEach" : Undefined len 1 = array_proto_fn,
        attr "length" : Integer readonly = install_list,
        method "item" : Object len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "RadioNodeList" : "NodeList" [interface, window] {
        ctor "RadioNodeList" len 0 = stub,
        attr "value" : DomString = install_radio_node_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "NamedNodeMap" [interface, window] {
        ctor "NamedNodeMap" len 0 = stub,
        attr "length" : Integer readonly = install_list,
        method "getNamedItem" : Object len 1 required 1 nullable = install_named_node_map,
        method "getNamedItemNS" : Object len 2 required 2 nullable = install_named_node_map,
        method "item" : Object len 1 required 1 nullable = install_list,
        method "removeNamedItem" : Object len 1 required 1 = install_named_node_map,
        method "removeNamedItemNS" : Object len 2 required 2 = install_named_node_map,
        method "setNamedItem" : Object len 1 required 1 nullable = install_named_node_map,
        method "setNamedItemNS" : Object len 1 required 1 nullable = install_named_node_map,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "DOMTokenList" [interface, window] {
        ctor "DOMTokenList" len 0 = stub,
        method "entries" : Object len 0 = array_proto_fn,
        method "keys" : Object len 0 = array_proto_fn,
        method "values" : Object len 0 = array_proto_fn,
        method "forEach" : Undefined len 1 = array_proto_fn,
        attr "length" : Integer readonly = install_list,
        attr "value" : DomString = install_dom_token_list,
        method "add" : Undefined len 0 = install_dom_token_list,
        method "contains" : Boolean len 1 required 1 = install_dom_token_list,
        method "item" : DomString len 1 required 1 nullable = install_list,
        method "remove" : Undefined len 0 = install_dom_token_list,
        method "replace" : Boolean len 2 required 2 = install_dom_token_list,
        method "supports" : Boolean len 1 required 1 = install_dom_token_list,
        method "toggle" : Boolean len 1 required 1 = install_dom_token_list,
        method "toString" : DomString len 0 = install_dom_token_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "CSSRuleList" [interface, window] {
        ctor "CSSRuleList" len 0 = stub,
        attr "length" : Integer readonly = install_list,
        method "item" : Object len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "DOMRectList" [interface, window] {
        ctor "DOMRectList" len 0 = stub,
        attr "length" : Integer readonly = install_list,
        method "item" : Object len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

webapi::interface! {
    "StyleSheetList" [interface, window] {
        ctor "StyleSheetList" len 0 = stub,
        attr "length" : Integer readonly = install_list,
        method "item" : Object len 1 required 1 nullable = install_list,
        method "@@iterator" : Object len 0 hidden cfg "values" = array_proto_fn
    }
}

// ── the two interfaces whose bodies already existed ─────────────────────────────────────────────

/// `NamedNodeMap`'s seven attribute members. The bodies are `core/dom/named_node_map.rs`'s, unchanged:
/// they read the same `NNM_EID` private this file now stamps on the interceptor-backed instance.
pub fn install_named_node_map(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::dom::named_node_map::{
        cb_nnm_get_named, cb_nnm_get_named_ns, cb_nnm_remove_named, cb_nnm_remove_named_ns, cb_nnm_set_named,
        cb_nnm_set_named_ns,
    };
    match member.name {
        "getNamedItem" => method_row(scope, proto, member, cb_nnm_get_named),
        "getNamedItemNS" => method_row(scope, proto, member, cb_nnm_get_named_ns),
        "removeNamedItem" => method_row(scope, proto, member, cb_nnm_remove_named),
        "removeNamedItemNS" => method_row(scope, proto, member, cb_nnm_remove_named_ns),
        "setNamedItem" => method_row(scope, proto, member, cb_nnm_set_named),
        "setNamedItemNS" => method_row(scope, proto, member, cb_nnm_set_named_ns),
        other => debug_assert!(false, "install_named_node_map: unexpected member {other}"),
    }
}

/// `DOMTokenList`'s own members. The bodies are `core/dom/dom_token_list.rs`'s, unchanged: they read the
/// same `DTL_ID`/`DTL_ATTR` privates. `length`, `item` and the four iteration members are NOT here --
/// they are the shared list `length`/`item` and `Array.prototype`'s own functions, which is what Chrome
/// answers (`DOMTokenList.prototype.forEach === Array.prototype.forEach`).
pub fn install_dom_token_list(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    use crate::core::dom::dom_token_list::{
        cb_dtl_add, cb_dtl_contains, cb_dtl_remove, cb_dtl_replace, cb_dtl_supports, cb_dtl_to_string, cb_dtl_toggle,
        cb_dtl_value_get, cb_dtl_value_set,
    };
    match member.name {
        "value" => {
            let g = build_fn(scope, cb_dtl_value_get, "get value", 0);
            let s = build_fn(scope, cb_dtl_value_set, "set value", 1);
            define_accessor(scope, proto, member, g, Some(s));
        }
        "add" => method_row(scope, proto, member, cb_dtl_add),
        "contains" => method_row(scope, proto, member, cb_dtl_contains),
        "remove" => method_row(scope, proto, member, cb_dtl_remove),
        "replace" => method_row(scope, proto, member, cb_dtl_replace),
        "supports" => method_row(scope, proto, member, cb_dtl_supports),
        "toggle" => method_row(scope, proto, member, cb_dtl_toggle),
        "toString" => method_row(scope, proto, member, cb_dtl_to_string),
        other => debug_assert!(false, "install_dom_token_list: unexpected member {other}"),
    }
}

// ── the member sites: one native op per collection-returning member ─────────────────────────────
//
// Each of these is a FUNCTION BUILDER, the shape every accessor/method on this surface has: the op
// returns a real `v8::Function` whose body is the raw callback beside it, so the member reports
// `[native code]` by construction rather than by a mask over a JS closure.

/// A `Kind` from its discriminant. Total: every value the op sites pass is one of the constants above,
/// and anything else answers an empty list rather than reading uninitialised memory.
fn kind_from(n: u32) -> Kind {
    match n {
        0 => Kind::DocByTag,
        1 => Kind::DocByClass,
        2 => Kind::ElByTag,
        3 => Kind::ElByClass,
        4 => Kind::Children,
        5 => Kind::DocByTagNs,
        6 => Kind::ElByTagNs,
        7 => Kind::Forms,
        8 => Kind::Images,
        9 => Kind::Scripts,
        10 => Kind::Embeds,
        11 => Kind::Links,
        12 => Kind::Anchors,
        13 => Kind::Applets,
        14 => Kind::FormControls,
        15 => Kind::Options,
        16 => Kind::TableBodies,
        17 => Kind::TableRows,
        18 => Kind::RowCells,
        19 => Kind::SectionRows,
        20 => Kind::ChildNodes,
        22 => Kind::DocAll,
        23 => Kind::DocAllNamed,
        25 => Kind::SelectedOptions,
        _ => Kind::RadioGroup,
    }
}

/// The document root a Document receiver's collections query from -- ITS own root, never the page's.
/// (`document.forms` inside an `<iframe>` collected the parent page's forms for as long as every
/// document passed the sentinel "-1".)
fn doc_root_of(scope: &mut v8::PinScope, this: v8::Local<v8::Object>) -> u32 {
    use crate::core::dom::document::{doc_root_slot, resolve_doc_root};
    let slot = doc_root_slot(scope, this.into());
    let op_state = deno_core::JsRuntime::op_state_from(scope);
    let st = op_state.borrow();
    resolve_doc_root(&st.borrow::<HostDom>().0, slot)
}

/// `data` for the collection builders below: `"<Interface>|<kind>"`, so one callback serves every
/// member that differs only in which query it runs and which interface it mints.
pub(crate) fn builder_data<'s>(scope: &mut v8::PinScope<'s, '_>, iface: &str, kind: Kind) -> v8::Local<'s, v8::Value> {
    str_val(scope, &format!("{iface}|{}", kind as u32))
}

/// [`builder_data`] read back, into the caller's stack buffer: the interface names are short, so a read
/// allocates nothing (it made two `String`s per `children` / `getElementsBy*` call).
fn read_builder_data<'b>(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    buf: &'b mut [std::mem::MaybeUninit<u8>; 64],
) -> Option<(std::borrow::Cow<'b, str>, Kind)> {
    use std::borrow::Cow;
    let d = crate::binding::protos::body_data(scope, args);
    let d = match v8::Local::<v8::String>::try_from(d) {
        Ok(s) => s.to_rust_cow_lossy(scope, buf),
        Err(_) => Cow::Owned(d.to_rust_string_lossy(scope)),
    };
    match d {
        Cow::Borrowed(s) => parse_builder_data(s).map(|(i, k)| (Cow::Borrowed(i), k)),
        Cow::Owned(s) => parse_builder_data(&s).map(|(i, k)| (Cow::Owned(i.to_string()), k)),
    }
}

/// `"<Interface>|<kind>"` ([`builder_data`]) split into its interface name and kind.
fn parse_builder_data(d: &str) -> Option<(&str, Kind)> {
    let (iface, kind) = d.split_once('|')?;
    Some((iface, kind_from(kind.parse().ok()?)))
}

/// An ELEMENT-rooted collection with no argument: `children`, `form.elements`, `select.options`,
/// `table.rows`, `tr.cells`, ...
pub(crate) fn cb_el_collection(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::uninit(); 64];
    let Some((iface, kind)) = read_builder_data(scope, &args, &mut buf) else { return };
    let Some(id) = crate::core::dom::element::el_id_of(scope, args.this()) else { return };
    // `children` is `[SameObject]` (ParentNode, DOM): `el.children === el.children` in Chrome, a fragment's
    // and a shadow root's too, and an expando set on it stays. A fresh list per read answered false, and
    // missed the live-ids cache on every `el.children.length` (each new list is a new cache entry).
    let list = if kind == Kind::Children {
        same_object_list(scope, args.this(), &iface, id, kind)
    } else {
        make_live(scope, &iface, id, kind, "", None)
    };
    rv.set(list);
}

/// `[SameObject]` for an argument-less live collection: hand back the SAME object every read.
///
/// `document.embeds !== document.embeds` was the reported defect, but the row is `[SameObject]` in
/// WebIDL for every one of them -- the collection is a PROPERTY of the document, not a value minted
/// per read -- and one `make_live` call per access made all of `forms`, `images`, `scripts`,
/// `links`, `anchors`, `applets`, `embeds`/`plugins` and `children` fail the identity together.
///
/// This is `Node.childNodes`'s memo (`core/dom/shared.rs`, DOM2.3) promoted to a function so there is
/// one production of it and not a second copy: a private slot on the receiver holding the list, and
/// a second holding the ROOT it was minted over, because a Document's root moves when the arena is
/// swapped and a list over a dead root must be re-minted rather than returned. Nothing else needs
/// reseeding -- the cached object is LIVE, its length and indices re-run the arena query.
///
/// The slot is keyed by KIND, which is also why `document.plugins === document.embeds` falls out for
/// free: HTML says `plugins` "must return the same object as that returned by the `embeds`
/// attribute", and they are one `Kind::Embeds` here.
fn same_object_list<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
    iface: &str,
    root: u32,
    kind: Kind,
) -> v8::Local<'s, v8::Value> {
    use crate::core::css::css_style_declaration::priv_get;
    use crate::core::dom::element::priv_set;
    let (list_slot, root_slot) = same_object_slots(kind);
    if priv_get(scope, this, &root_slot).and_then(|v| v.uint32_value(scope)) == Some(root) {
        if let Some(list) = priv_get(scope, this, &list_slot).filter(|v| v.is_object()) {
            return list;
        }
    }
    let list = in_relevant_realm(scope, this, |scope| make_live(scope, iface, root, kind, "", None));
    priv_set(scope, this, &list_slot, list);
    let r = v8::Integer::new_from_unsigned(scope, root);
    priv_set(scope, this, &root_slot, r.into());
    list
}

/// `make` run in `this`'s RELEVANT realm -- the realm of the object a `[SameObject]` attribute is read on, which is
/// where its value is made and then kept: Blink sets an attribute's return value through
/// `ScriptState::ForRelevantRealm(info)`, the receiver's creation context, whichever realm's getter ran.
///
/// The memoised lists were minted in the CURRENT realm, the getter's, and returned to every later read: an
/// iframe's pristine `Element.prototype` `children` getter called first on a top-level `<div>` (a fingerprinting
/// script's cross-realm read) left `d.children instanceof HTMLCollection` false for good. Chrome 152 (2026-10-23,
/// both orders, and for `document.forms` ... `applets`, a fragment's and a shadow root's `children`, `childNodes`,
/// `classList`, `relList`; and `document.all` and `styleSheets`, which kept the old answer until 2026-10-23's second
/// probe, r2/l3/r1/www/a1.html): the list is the receiver realm's, and the same object, whichever getter read it first.
///
/// The receiver's own realm -- every same-realm read -- enters nothing.
pub(crate) fn in_relevant_realm<'s, R>(
    scope: &mut v8::PinScope<'s, '_>,
    this: v8::Local<v8::Object>,
    make: impl FnOnce(&mut v8::PinScope<'s, '_>) -> R,
) -> R {
    match this.get_creation_context(scope).filter(|c| *c != scope.get_current_context()) {
        Some(realm) => {
            let scope = &mut v8::ContextScope::new(scope, realm);
            make(scope)
        }
        None => make(scope),
    }
}

/// [`same_object_list`]'s two slot names for `kind`, `korvus_same_object_list_<n>` and
/// `korvus_same_object_root_<n>` (`n` = `kind as u32`): literals for the kinds mounted on a getter
/// (`document.forms` ... `children`), so a read builds no string and its keys come out of `private_key`'s
/// address memo; any other kind formats them.
fn same_object_slots(kind: Kind) -> (std::borrow::Cow<'static, str>, std::borrow::Cow<'static, str>) {
    use std::borrow::Cow::Borrowed as B;
    match kind {
        Kind::Children => (B("korvus_same_object_list_4"), B("korvus_same_object_root_4")),
        Kind::Forms => (B("korvus_same_object_list_7"), B("korvus_same_object_root_7")),
        Kind::Images => (B("korvus_same_object_list_8"), B("korvus_same_object_root_8")),
        Kind::Scripts => (B("korvus_same_object_list_9"), B("korvus_same_object_root_9")),
        Kind::Embeds => (B("korvus_same_object_list_10"), B("korvus_same_object_root_10")),
        Kind::Links => (B("korvus_same_object_list_11"), B("korvus_same_object_root_11")),
        Kind::Anchors => (B("korvus_same_object_list_12"), B("korvus_same_object_root_12")),
        Kind::Applets => (B("korvus_same_object_list_13"), B("korvus_same_object_root_13")),
        k => (
            std::borrow::Cow::Owned(format!("korvus_same_object_list_{}", k as u32)),
            std::borrow::Cow::Owned(format!("korvus_same_object_root_{}", k as u32)),
        ),
    }
}

/// A DOCUMENT-rooted collection with no argument: `document.forms`, `images`, `scripts`, `children`, ...
pub(crate) fn cb_doc_collection(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::uninit(); 64];
    let Some((iface, kind)) = read_builder_data(scope, &args, &mut buf) else { return };
    let root = doc_root_of(scope, args.this());
    let list = same_object_list(scope, args.this(), &iface, root, kind);
    rv.set(list);
}

/// `getElementsByTagName(name)` / `getElementsByClassName(name)` on a Document.
pub(crate) fn cb_doc_by_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::uninit(); 64];
    let Some((iface, kind)) = read_builder_data(scope, &args, &mut buf) else { return };
    let Some(sel) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let root = doc_root_of(scope, args.this());
    let list = make_live(scope, &iface, root, kind, &sel, None);
    rv.set(list);
}

/// `getElementsByTagName(name)` / `getElementsByClassName(name)` on an Element (its descendants).
pub(crate) fn cb_el_by_name(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::uninit(); 64];
    let Some((iface, kind)) = read_builder_data(scope, &args, &mut buf) else { return };
    let Some(sel) = args.get(0).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let Some(id) = crate::core::dom::element::el_id_of(scope, args.this()) else { return };
    let list = make_live(scope, &iface, id, kind, &sel, None);
    rv.set(list);
}

/// `getElementsByTagNameNS(ns, localName)`. `ns` is `null`/`""` for the no-namespace elements and `"*"`
/// for any. The WebIDL arity guard the row carries runs first, because Chrome throws rather than
/// stringify an absent argument into a bogus lookup.
pub(crate) fn cb_by_tag_ns(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let mut buf = [std::mem::MaybeUninit::uninit(); 64];
    let Some((iface, kind)) = read_builder_data(scope, &args, &mut buf) else { return };
    let root = if kind == Kind::DocByTagNs {
        doc_root_of(scope, args.this())
    } else {
        match crate::core::dom::element::el_id_of(scope, args.this()) { Some(i) => i, None => return }
    };
    let ns_v = args.get(0);
    let ns: Option<String> = if ns_v.is_null_or_undefined() { None } else { Some(ns_v.to_rust_string_lossy(scope)) };
    let ns = ns.filter(|s| !s.is_empty());
    let Some(local) = args.get(1).to_string(scope).map(|s| s.to_rust_string_lossy(scope)) else { return };
    let list = make_live(scope, &iface, root, kind, &local, ns.as_deref());
    rv.set(list);
}

macro_rules! coll_op {
    ($op:ident, $cb:ident) => {
        #[deno_core::op2]
        pub(crate) fn $op<'s>(scope: &mut v8::PinScope<'s, '_>, #[string] fname: &str, #[smi] length: i32, #[string] iface: &str, #[smi] kind: i32) -> v8::Local<'s, v8::Function> {
            let d = builder_data(scope, iface, kind_from(kind as u32));
            build_fn_data(scope, $cb, fname, length, d)
        }
    };
}
coll_op!(op_el_collection, cb_el_collection);
coll_op!(op_doc_collection, cb_doc_collection);
coll_op!(op_doc_by_name, cb_doc_by_name);
coll_op!(op_el_by_name, cb_el_by_name);
coll_op!(op_by_tag_ns, cb_by_tag_ns);

// `op_static_list(iface, items, reuse)`, the JS-facing form of the function below, IS GONE
// -- no caller once the JS that minted platform lists by name was deleted.

/// Mint a STATIC platform list from `arr`, or refill `reuse` in place when it is a static list of the
/// same interface. `reuse` keeps a `[SameObject]` member (`document.styleSheets`,
/// `CSSStyleSheet.cssRules`) the same object across reads; pass `None` for a member that mints a
/// fresh list every time (`getClientRects()`, which is not `[SameObject]` in Chrome either).
pub(crate) fn static_list_reuse<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    iface: &str,
    arr: v8::Local<'s, v8::Array>,
    reuse: Option<v8::Local<'s, v8::Object>>,
) -> v8::Local<'s, v8::Value> {
    if let Some(existing) = reuse {
        if lst_u32(scope, existing, P_CLASS) == Some(CLASS_STATIC) && iface_of(scope, existing) == iface {
            reseat_static(scope, existing, arr);
            return existing.into();
        }
    }
    make_static(scope, iface, arr)
}

crate::webapi_ops![op_el_collection, op_doc_collection, op_doc_by_name, op_el_by_name, op_by_tag_ns];

#[cfg(test)]
mod tests {
    use super::{same_object_slots, Kind};

    const KINDS: [Kind; 26] = [
        Kind::DocByTag, Kind::DocByClass, Kind::ElByTag, Kind::ElByClass, Kind::Children, Kind::DocByTagNs, Kind::ElByTagNs,
        Kind::Forms, Kind::Images, Kind::Scripts, Kind::Embeds, Kind::Links, Kind::Anchors, Kind::Applets, Kind::FormControls,
        Kind::Options, Kind::TableBodies, Kind::TableRows, Kind::RowCells, Kind::SectionRows, Kind::ChildNodes,
        Kind::RadioGroup, Kind::DocAll, Kind::DocAllNamed, Kind::SelectedOptions, Kind::DocByElementName,
    ];

    #[test]
    fn parse_builder_data_splits_the_interface_and_kind() {
        let d = format!("HTMLCollection|{}", Kind::Children as u32);
        assert_eq!(super::parse_builder_data(&d), Some(("HTMLCollection", Kind::Children)));
        assert_eq!(super::parse_builder_data("HTMLCollection"), None);
        assert_eq!(super::parse_builder_data("HTMLCollection|x"), None);
        assert_eq!(super::parse_builder_data("|5"), Some(("", Kind::DocByTagNs)));
    }

    #[test]
    fn engine_slot_names_cover_every_list_and_only_engine_slots() {
        use super::{engine_slot_names, list_shape, DTL_ATTR, DTL_ID, NNM_EID, P_CLASS, P_COLL, P_IFACE, P_ITEMS, P_KIND, P_NAMED, P_NAMES, P_NS, P_ROOT, P_SEL};
        let known = [P_IFACE, P_ROOT, P_KIND, P_SEL, P_NS, P_NAMES, P_NAMED, DTL_ID, DTL_ATTR, NNM_EID];
        let fields = [P_CLASS.name, P_COLL.name, P_ITEMS.name];
        let lists = [
            "HTMLCollection", "HTMLFormControlsCollection", "HTMLOptionsCollection", "NamedNodeMap", "PluginArray", "Plugin",
            "MimeTypeArray", "NodeList", "RadioNodeList", "DOMTokenList", "DOMRectList", "DOMStringList", "CSSRuleList",
            "StyleSheetList",
        ];
        for iface in lists {
            assert!(list_shape(iface).is_some(), "{iface}");
            let names = engine_slot_names(iface);
            assert!(names.contains(&P_IFACE), "{iface}: every list reads its interface");
            for (i, n) in names.iter().enumerate() {
                assert!(!names[..i].contains(n), "{iface}: {n} twice");
                assert!(known.contains(n), "{iface}: {n} is not one of the module's slots");
                assert!(!fields.contains(n), "{iface}: {n} is kept in a field");
                assert!(!n.starts_with("korvus_brand_") && *n != "korvus_iface", "{iface}: {n} is read with has_private");
            }
        }
        // `list_shape`'s every row has one (the list above is its rows)
        for iface in ["HTMLAllCollection", "Element", "MutationRecord", ""] {
            assert!(list_shape(iface).is_none() && engine_slot_names(iface).is_empty(), "{iface}");
        }
    }

    #[test]
    fn list_field_layout() {
        use super::{LIST_FIELDS, P_CLASS, P_COLL, P_ITEMS};
        let f = [P_CLASS.field, P_COLL.field, P_ITEMS.field];
        for (i, x) in f.iter().enumerate() {
            assert!((1..LIST_FIELDS).contains(x), "field {x} outside 1..{LIST_FIELDS} (0 is the brand cache)");
            assert!(!f[..i].contains(x), "field {x} twice");
        }
        assert!(![0, 1, 2].contains(&LIST_FIELDS), "the count must differ from every other template's");
        assert_eq!((P_CLASS.name, P_COLL.name, P_ITEMS.name), ("korvus_list_class", "korvus_coll_no", "korvus_list_items"));
    }

    #[test]
    fn same_object_slots_are_the_formatted_names() {
        for k in KINDS {
            let (list, root) = same_object_slots(k);
            assert_eq!(list, format!("korvus_same_object_list_{}", k as u32), "{k:?}");
            assert_eq!(root, format!("korvus_same_object_root_{}", k as u32), "{k:?}");
        }
    }
}
