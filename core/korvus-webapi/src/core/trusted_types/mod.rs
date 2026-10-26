//! `core/trusted_types/` — the Trusted Types API.
//!
//! `window.trustedTypes` is the name FP5 measured **eight sensor reads of in one a bot sensor page load**
//! and refused to fake, with the argument that generalised into the unit contract:
//!
//! > A page that finds `window.trustedTypes` and calls `createPolicy(n, o).createHTML(s)` **throws**
//! > on a stub. A page with no `trustedTypes` at all takes its fallback and works.
//!
//! So the whole point of this module is that a policy ROUND-TRIPS: `createPolicy` hands back a real
//! `TrustedTypePolicy`, whose `createHTML` hands back a real `TrustedHTML`, which `isHTML` then
//! recognises and an `innerHTML` sink accepts. Five interfaces, twenty rows, all five leaving
//! `install/generated_registry.rs` with their tables.
//!
//! [`tables`] holds the five `interface!` tables and their bodies; this file holds the one thing they
//! share — THE TYPE MAP, which is a measurement rather than a rule.
//!
//! # WHY THE TYPE MAP IS A DATA FILE
//!
//! `getAttributeType('div', 'onclick')` is `TrustedScript` and `getAttributeType('div', 'onbogus')`
//! is **`null`**, so the event-handler set is a LIST and not the rule "starts with `on`". The list
//! the browsers publish is Blink's whole event-handler-name set — 321 names — and it cannot be
//! derived from anything this engine already has: the union of every `on*` member the ACTIVE CAPTURE
//! records is 125 names and the union over korvus's whole registry, both realms, is 279. Deriving it
//! from the engine's own rows would also be a second definition competing with the browser's, and
//! it would move every time a row moved.
//!
//! It is not a bundle field either: it is a fact of the Chromium build rather than of the device, and
//! the capture rig does not record it at all. So it is read once, the way FP5.9's `[SecureContext]`
//! reading is — `tools/sync/data/trusted_types_type_map.json`, with a `why` that says what puts an
//! entry in and what takes one out — and `getTypeMapping`, `getAttributeType` and `getPropertyType`
//! are three readings of that ONE file.
//!
//! **The four browsers do not quite agree, and the file records that rather than averaging it**:
//! Edge 151 lists four names the other three do not (`onwidgetclick`, `onwidgetinstall`,
//! `onwidgetresume`, `onwidgetuninstall` — its widget service-worker handlers), 325 against 321. The
//! engine publishes the 321 all four list; the file names the other four in `not_in_all_four`, so the
//! shortfall is four names of `getTypeMapping()['*']` under the `edge-151` bundle and nothing else.
//! There is no seam through which the engine could ask: the capture does not record the list.
//!
//! # HOW A LOOKUP RESOLVES, measured one (tag, name, namespace) at a time
//!
//! The merged map `getTypeMapping()` returns cannot answer this — its `script` entry lists both `src`
//! and `href` although `getAttributeType('script', 'href')` is `null` in HTML — so the file carries a
//! second, namespaced `resolve` section beside the verbatim map, and both were read off the browsers.
//!
//! ```text
//! elementNs absent / '' / the XHTML namespace   -> the `html` element rows
//! elementNs the SVG namespace                   -> the `svg` element rows
//! elementNs anything else                       -> no element rows at all (MathML included)
//! attrNs absent / ''                            -> the NULL namespace: every row without one
//! attrNs the XLink namespace                    -> the one namespaced row, svg `script`'s `href`
//! attrNs anything else non-empty                -> null
//! the `*` ATTRIBUTE row                         -> XHTML, SVG and MathML element namespaces only
//! the `*` PROPERTY rows                         -> EVERY element namespace, without exception
//! the tag and the ATTRIBUTE name                -> ASCII-lowercased ('SCRIPT'/'SRC'/'onClick' match)
//! the PROPERTY name                             -> NOT ('INNERHTML' and 'codebase' answer null)
//! getTypeMapping() / getTypeMapping('')         -> the whole map; any namespace string -> null
//! ```
//!
//! # THE THREE RULES ABOVE THAT WERE GENERALISED RATHER THAN MEASURED (TT1, 2026-09-22)
//!
//! The first reading of this file (2026-09-20) wrote three rules from cases it had not probed, and
//! **all three were wrong in the same direction — they answered `null` where a browser answers a
//! type.** Re-measured against Brave 151.1.93.138 and Chrome 150.0.7871.46 (`--headless --dump-dom`,
//! 32 probes, the two browsers byte-identical) and read against Blink's own
//! `FindEntryInAttributeTypeVector` (`trusted_type_policy_factory.cc:180`):
//!
//! 1. *"elementNs anything else -> null"* was taken to end the lookup. It ends only the ELEMENT-row
//!    half of it. The `*` event-handler row still applies in MathML, so the twelve lines of `*`
//!    fallback below — whose own comment already said "which apply in BOTH namespaces" — were
//!    unreachable for the one namespace that has nothing else. 93 subtests.
//! 2. *"a non-null attrNs -> null"* conflated `''` with a real namespace. The spec and Blink both map
//!    an EMPTY attribute namespace to the null one, so `getAttributeType(el, a, ns, '')` is the same
//!    lookup as `..., null)`, and `''` is what `Attr.namespaceURI` hands back through the whole
//!    `support/attributes.js` corpus.
//! 3. The same rule hid the one row that DOES carry an attribute namespace: SVG `script`'s `href` in
//!    the XLink namespace.
//!
//! And `getPropertyType` had the mirror-image of (1): Blink exempts the property vector from the
//! namespace check outright, so `getPropertyType('foo','innerHTML', <any namespace at all>)` is
//! `TrustedHTML` — the `*` property rows are not restricted to the three HTML-known namespaces the
//! `*` attribute row is. Both facts now live in the data file as `attribute_element_namespaces`
//! (present) and its absence for properties (deliberate), not as a rule in this code.

pub(crate) mod tables;

use std::sync::OnceLock;

/// THE MEASUREMENT, read once. `include_str!` rather than a generated Rust table: the file is the
/// reading, a generator would be a second copy of it, and `tools/sync/data/secure_context.json` set
/// the precedent for a platform fact that lives beside the code rather than in a bundle.
const TYPE_MAP_JSON: &str = include_str!("../../../../../tools/sync/data/trusted_types_type_map.json");

fn doc() -> &'static serde_json::Value {
    static ONCE: OnceLock<serde_json::Value> = OnceLock::new();
    ONCE.get_or_init(|| {
        serde_json::from_str(TYPE_MAP_JSON).expect("tools/sync/data/trusted_types_type_map.json is JSON")
    })
}

const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// The element namespace a lookup resolves against. An absent argument and `""` are BOTH the XHTML
/// namespace — Blink normalises `elementNS.empty()` to `xhtmlNamespaceURI` before it matches anything
/// — so every rule below can be written against one string instead of an `Option`.
fn element_namespace(element_ns: Option<&str>) -> &str {
    match element_ns {
        None | Some("") => HTML_NS,
        Some(ns) => ns,
    }
}

/// Which namespace's ELEMENT rows a lookup reads, or `None` for a namespace that has none.
///
/// `None` is not "the lookup answers null": the `*` rows are a separate question, asked after this
/// one and with its own reach. MathML lands here and still answers `TrustedScript` for an event
/// handler.
fn table_for(element_ns: &str) -> Option<&'static str> {
    match element_ns {
        HTML_NS => Some("html"),
        SVG_NS => Some("svg"),
        _ => None,
    }
}

/// Does the `*` ATTRIBUTE row reach an element in `element_ns`? The three namespaces the file names
/// — XHTML, SVG and MathML — and no other. (The `*` PROPERTY rows have no such restriction; see
/// [`property_type`].)
fn star_attribute_row_applies(element_ns: &str) -> bool {
    static ONCE: OnceLock<Vec<String>> = OnceLock::new();
    ONCE.get_or_init(|| {
        doc()
            .pointer("/resolve/any_element/attribute_element_namespaces")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    })
    .iter()
    .any(|ns| ns == element_ns)
}

/// Every event-handler attribute name ALL FOUR browsers list, sorted — the `*` entry's attributes.
///
/// Derived from the file's per-name `browsers` column rather than kept as a second list, which is the
/// same shape `secure_context.json` uses: an entry that only some browsers have is recorded and not
/// published, and `not_in_all_four` is what names it.
pub(crate) fn event_handler_attributes() -> &'static [String] {
    static ONCE: OnceLock<Vec<String>> = OnceLock::new();
    ONCE.get_or_init(|| {
        let Some(o) = doc().get("event_handler_attributes").and_then(|v| v.as_object()) else { return Vec::new() };
        let mut out: Vec<String> = o
            .iter()
            .filter(|(_, bs)| bs.as_array().map(|a| a.len()) == Some(4))
            .map(|(n, _)| n.clone())
            .collect();
        out.sort();
        out
    })
}

/// Is `name` an event-handler content attribute, by the measured list?
fn is_event_handler_attribute(name: &str) -> bool {
    event_handler_attributes().binary_search_by(|p| p.as_str().cmp(name)).is_ok()
}

/// `getAttributeType(tag, attribute, elementNs?, attrNs?)`.
///
/// Two questions, asked in this order and NOT short-circuiting each other: the element rows of
/// `element_ns`, then the `*` event-handler row. The second one is why MathML — which has no element
/// rows at all — still answers `TrustedScript` for an event handler.
pub(crate) fn attribute_type(
    tag: &str,
    attribute: &str,
    element_ns: Option<&str>,
    attr_ns: Option<&str>,
) -> Option<&'static str> {
    let element_ns = element_namespace(element_ns);
    // `""` is NOT a namespace. An empty attribute namespace is the null one, so `(..., ns, "")` is
    // the same lookup as `(..., ns, null)` — which is what `Attr.namespaceURI` hands back for every
    // unprefixed attribute a page reads off an element.
    let attr_ns = attr_ns.filter(|ns| !ns.is_empty());
    let tag = tag.to_ascii_lowercase();
    let attribute = attribute.to_ascii_lowercase();

    if let Some(table) = table_for(element_ns) {
        // The null-namespace rows are `attributes`; the ones that carry a namespace are under it by
        // URI, and there is exactly one of those (svg `script`'s XLink `href`).
        let rows = match attr_ns {
            None => doc().pointer(&format!("/resolve/{table}/attributes")),
            Some(ns) => {
                doc().pointer(&format!("/resolve/{table}/attributes_ns")).and_then(|v| v.get(ns))
            }
        };
        if let Some(t) =
            rows.and_then(|v| v.get(&tag)).and_then(|v| v.get(&attribute)).and_then(|v| v.as_str())
        {
            return Some(leak(t));
        }
    }

    // The `*` row. It carries no attribute namespace, so only a null one reaches it, and it applies
    // in the three element namespaces the file names — XHTML, SVG and MathML.
    if attr_ns.is_none()
        && star_attribute_row_applies(element_ns)
        && is_event_handler_attribute(&attribute)
    {
        return doc().pointer("/resolve/any_element/attribute_type").and_then(|v| v.as_str()).map(leak);
    }
    None
}

/// `getPropertyType(tag, property, elementNs?)`. The PROPERTY name keeps its case: `INNERHTML` and
/// `codebase` answer `null` where `innerHTML` and `codeBase` answer a type (measured).
///
/// The `*` rows here are NOT restricted the way [`attribute_type`]'s are: Blink exempts the property
/// vector from the namespace check outright, so `getPropertyType('foo', 'innerHTML', <anything>)` is
/// `TrustedHTML` — measured in the XUL namespace and in a made-up one. That is why the fallback sits
/// outside the `if let Some(table)` and not inside it.
pub(crate) fn property_type(tag: &str, property: &str, element_ns: Option<&str>) -> Option<&'static str> {
    let element_ns = element_namespace(element_ns);
    let tag = tag.to_ascii_lowercase();
    if let Some(table) = table_for(element_ns) {
        if let Some(t) = doc()
            .pointer(&format!("/resolve/{table}/properties"))
            .and_then(|v| v.get(&tag))
            .and_then(|v| v.get(property))
            .and_then(|v| v.as_str())
        {
            return Some(leak(t));
        }
    }
    doc()
        .pointer("/resolve/any_element/properties")
        .and_then(|v| v.get(property))
        .and_then(|v| v.as_str())
        .map(leak)
}

/// The element entries `getTypeMapping()` reports, verbatim, in the browsers' own order.
pub(crate) fn type_mapping_elements() -> &'static serde_json::Map<String, serde_json::Value> {
    static EMPTY: OnceLock<serde_json::Map<String, serde_json::Value>> = OnceLock::new();
    doc()
        .pointer("/get_type_mapping/elements")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| EMPTY.get_or_init(serde_json::Map::new))
}

/// The `*` entry's `properties` — `innerHTML` and `outerHTML`, both `TrustedHTML` on all four.
pub(crate) fn any_element_properties() -> &'static serde_json::Map<String, serde_json::Value> {
    static EMPTY: OnceLock<serde_json::Map<String, serde_json::Value>> = OnceLock::new();
    doc()
        .pointer("/get_type_mapping/any_element_properties")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| EMPTY.get_or_init(serde_json::Map::new))
}

/// The type every event-handler attribute maps to (`TrustedScript` on all four).
pub(crate) fn event_handler_type() -> &'static str {
    doc().pointer("/resolve/any_element/attribute_type").and_then(|v| v.as_str()).unwrap_or("TrustedScript")
}

/// Does `getTypeMapping(ns)` answer the map at all? Only for no argument and for `""` — every
/// namespace STRING answers `null`, the XHTML one included (measured).
pub(crate) fn type_mapping_applies(ns: Option<&str>) -> bool {
    matches!(ns, None | Some(""))
}

/// The document is a `'static` singleton, so a `&str` inside it is too; this is the one place that
/// is spelled out rather than threaded through every signature.
fn leak(s: &str) -> &'static str {
    // SAFETY-free: `doc()` is a `OnceLock` that is never cleared, so every string it owns outlives
    // the process. Expressed as a lookup rather than a transmute.
    match s {
        "TrustedHTML" => "TrustedHTML",
        "TrustedScript" => "TrustedScript",
        "TrustedScriptURL" => "TrustedScriptURL",
        // A type the file names and this build does not know: reported rather than silently dropped,
        // because it would mean the measurement grew a fourth Trusted type.
        other => {
            debug_assert!(false, "trusted_types: unknown type {other} in the measured map");
            "TrustedScript"
        }
    }
}

#[cfg(test)]
mod tests {
    //! The FILE is the measurement; these assert that this module reads it the way the browsers
    //! answered, so a lookup rule cannot drift from the reading it was derived from.

    #[test]
    fn the_measured_file_parses_and_carries_the_four_browsers_reading() {
        let d = super::doc();
        assert!(d.get("why").and_then(|v| v.as_str()).is_some_and(|s| s.len() > 200), "a measurement carries its why");
        assert_eq!(d.pointer("/counts/chrome-150").and_then(|v| v.as_u64()), Some(321));
        assert_eq!(d.pointer("/counts/edge-151").and_then(|v| v.as_u64()), Some(325));
        assert_eq!(d.get("common").and_then(|v| v.as_u64()), Some(321));
        assert_eq!(super::event_handler_attributes().len(), 321, "the engine publishes what all four list");
    }

    #[test]
    fn a_lookup_answers_what_the_browsers_answered() {
        use super::{attribute_type as a, property_type as p, HTML_NS, SVG_NS};
        // Chrome 150 / Brave 151, 2026-09-20, one (tag, name, namespace) at a time.
        assert_eq!(a("script", "src", None, None), Some("TrustedScriptURL"));
        assert_eq!(a("script", "src", Some(HTML_NS), None), Some("TrustedScriptURL"));
        assert_eq!(a("script", "src", Some(SVG_NS), None), None);
        assert_eq!(a("script", "href", None, None), None);
        assert_eq!(a("script", "href", Some(SVG_NS), None), Some("TrustedScriptURL"));
        assert_eq!(a("iframe", "srcdoc", None, None), Some("TrustedHTML"));
        assert_eq!(a("iframe", "srcdoc", Some(SVG_NS), None), None);
        assert_eq!(a("object", "codebase", None, None), Some("TrustedScriptURL"));
        assert_eq!(a("div", "onclick", None, None), Some("TrustedScript"));
        assert_eq!(a("div", "onclick", Some(SVG_NS), None), Some("TrustedScript"));
        assert_eq!(a("div", "onbogus", None, None), None, "the handler set is a LIST, not `starts with on`");
        assert_eq!(a("SCRIPT", "SRC", None, None), Some("TrustedScriptURL"), "tag and attribute are lowercased");
        assert_eq!(a("div", "onClick", None, None), Some("TrustedScript"));
        assert_eq!(a("bogus-element", "onclick", None, None), Some("TrustedScript"));
        assert_eq!(a("script", "src", Some(HTML_NS), Some(HTML_NS)), None, "a foreign attrNs answers null");
        assert_eq!(a("script", "src", Some("urn:x"), None), None);
        assert_eq!(a("div", "id", None, None), None);
        assert_eq!(a("div", "innerHTML", None, None), None, "innerHTML is a PROPERTY, not an attribute");

        assert_eq!(p("div", "innerHTML", None), Some("TrustedHTML"));
        assert_eq!(p("div", "outerHTML", None), Some("TrustedHTML"));
        assert_eq!(p("div", "innerHTML", Some(SVG_NS)), Some("TrustedHTML"));
        assert_eq!(p("div", "INNERHTML", None), None, "a PROPERTY name keeps its case");
        assert_eq!(p("object", "codeBase", None), Some("TrustedScriptURL"));
        assert_eq!(p("object", "codebase", None), None);
        assert_eq!(p("script", "text", None), Some("TrustedScript"));
        assert_eq!(p("script", "textContent", None), Some("TrustedScript"));
        assert_eq!(p("script", "innerText", None), Some("TrustedScript"));
        assert_eq!(p("script", "src", None), Some("TrustedScriptURL"));
        assert_eq!(p("script", "src", Some(SVG_NS)), None);
        assert_eq!(p("script", "href", Some(SVG_NS)), Some("TrustedScriptURL"));
        assert_eq!(p("div", "onclick", None), None, "a handler is not a property");
        assert_eq!(p("div", "id", None), None);
    }

    /// The three rules the 2026-09-20 reading GENERALISED rather than probed, plus the one they hid.
    ///
    /// Every assertion here is a line a browser printed: Brave 151.1.93.138 and Chrome 150.0.7871.46,
    /// `--headless --dump-dom` on a local page, 2026-09-22, byte-identical to each other over all 32
    /// probes. The controls beside them are the cases that must NOT move.
    #[test]
    fn the_namespace_rules_answer_what_brave_151_and_chrome_150_answered() {
        use super::{attribute_type as a, property_type as p, HTML_NS, SVG_NS};
        const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";
        const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
        const FOO_NS: &str = "https://example.com/namespace";
        const XUL_NS: &str = "http://www.mozilla.org/keymaster/gatekeeper/there.is.only.xul";

        // 1. The `*` attribute row reaches MathML, which has no element rows of its own.
        assert_eq!(a("mrow", "onmousedown", Some(MATHML_NS), None), Some("TrustedScript"));
        assert_eq!(a("mrow", "onmousedown", Some(MATHML_NS), Some("")), Some("TrustedScript"));
        assert_eq!(a("g", "ondblclick", Some(SVG_NS), None), Some("TrustedScript"), "control: SVG");
        assert_eq!(a("div", "onclick", Some(HTML_NS), None), Some("TrustedScript"), "control: XHTML");
        // ... and NOWHERE else. These two are the control on the rule above.
        assert_eq!(a("foo", "onmouseup", Some(FOO_NS), None), None);
        assert_eq!(a("div", "onclick", Some(XUL_NS), None), None);
        // A namespaced attribute never reaches the `*` row, whatever the element namespace.
        assert_eq!(a("mrow", "onmousedown", Some(MATHML_NS), Some(MATHML_NS)), None);
        assert_eq!(a("div", "onclick", Some(HTML_NS), Some(FOO_NS)), None);

        // 2. An EMPTY attrNs is the null one, not a namespace that fails to match.
        assert_eq!(a("div", "onclick", Some(HTML_NS), Some("")), Some("TrustedScript"));
        assert_eq!(a("script", "src", Some(HTML_NS), Some("")), Some("TrustedScriptURL"));
        assert_eq!(a("script", "src", Some(""), Some("")), Some("TrustedScriptURL"));
        assert_eq!(a("iframe", "srcdoc", None, Some("")), Some("TrustedHTML"));
        assert_eq!(a("script", "href", Some(SVG_NS), Some("")), Some("TrustedScriptURL"));

        // 3. The one row that carries an attribute namespace, and its controls.
        assert_eq!(a("script", "href", Some(SVG_NS), Some(XLINK_NS)), Some("TrustedScriptURL"));
        assert_eq!(a("script", "href", Some(SVG_NS), Some(FOO_NS)), None);
        assert_eq!(a("script", "href", Some(HTML_NS), Some(XLINK_NS)), None, "the element must be SVG");
        assert_eq!(a("script", "src", Some(SVG_NS), Some(XLINK_NS)), None, "only `href` is namespaced");
        // Both namespaces are compared case-SENSITIVELY, and the attribute is a LOCAL name.
        assert_eq!(a("script", "href", Some(&SVG_NS.to_ascii_uppercase()), Some(XLINK_NS)), None);
        assert_eq!(a("script", "href", Some(SVG_NS), Some(&XLINK_NS.to_ascii_uppercase())), None);
        assert_eq!(a("script", "xlink:href", Some(SVG_NS), Some(XLINK_NS)), None);

        // 4. The `*` PROPERTY rows are not restricted to the three namespaces the `*` attribute row
        //    is — they apply everywhere, which is the mirror image of (1) and a separate reading.
        assert_eq!(p("foo", "innerHTML", Some(XUL_NS)), Some("TrustedHTML"));
        assert_eq!(p("foo", "innerHTML", Some(MATHML_NS)), Some("TrustedHTML"));
        assert_eq!(p("foo", "outerHTML", Some(FOO_NS)), Some("TrustedHTML"));
        // The ELEMENT property rows still are: only their own namespace answers.
        assert_eq!(p("iframe", "srcdoc", Some(FOO_NS)), None);
        assert_eq!(p("script", "src", Some(MATHML_NS)), None);
        assert_eq!(p("script", "href", Some(SVG_NS)), Some("TrustedScriptURL"), "control");
    }
}
