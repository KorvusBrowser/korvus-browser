//! Tag name -> the interface it exposes to script, RE-EXPORTED from the one table.
//!
//! Two questions need this map and neither can be answered by guessing from the name:
//!
//!   * Which interface does a WPT spec-section directory test? `html/semantics/forms/the-input-element/`
//!     holds 177 files and one of them names the interface. ([`crate::wpt`])
//!   * How do you obtain an instance to ask a question of? Most of the DOM has no constructor;
//!     an HTMLInputElement comes from `document.createElement("input")`. ([`crate::gencases`])
//!
//! THE TABLES ARE NOT HERE ANY MORE. `tools/sync/gen_element_tags.py` wrote the same three tables into
//! this file AND into `core/korvus-dom/src/tags.rs`, so tag -> interface was stated twice and a
//! generator change could land in one copy alone (goal invariant I1). The arena crate's copy is the
//! one the ENGINE reads -- `install/dom_chain.rs::fill_dom_protos` fills `TagProtos`/`SvgTagProtos`
//! from it -- so the grader re-exports that, and the generator writes one file.
//!
//! The rule the generator applies is Blink's own
//! (`build/scripts/make_element_factory.py::_js_interface`): `JSInterfaceName` when declared -- it is
//! the JS-visible name and it DIFFERS, `wbr` being HTMLWBRElement in C++ and plain HTMLElement to
//! script -- else `interfaceName`, else the namespace's declared fallback, else
//! `<Namespace><UpperCamelCase(tag)>Element`. Eighteen HTML tags declare no `interfaceName` at all
//! (`input`, `audio`, `canvas`, `form`, ...), so reading the absent field as HTMLElement would lose
//! precisely the interfaces this exists to reach.
//!
//! The three namespaces stay SEPARATE tables because tag names collide across them: `a`, `title`,
//! `style` and `script` are all declared in both HTML and SVG, and folding them together would
//! silently attribute one namespace's tests and receivers to the other's interface.

pub use korvus_dom::tags::{HTML_TAGS, MATHML_NS, MATHML_TAGS, SVG_NS, SVG_TAGS};

/// The interface an HTML tag exposes. HTML only: see the note above about cross-namespace collisions.
pub fn html_iface_for_tag(tag: &str) -> Option<&'static str> {
    HTML_TAGS.iter().find(|(t, _)| *t == tag).map(|(_, i)| *i)
}

/// A `document.create*` expression yielding an instance of `iface`, or `None` when no element in any
/// namespace implements it.
///
/// When several tags share an interface (65 HTML tags are plain HTMLElement) the FIRST is taken. The
/// tables are sorted, so that choice is stable across regenerations -- which matters because the
/// expression becomes a corpus key that a recorded browser answer is filed under.
pub fn create_expr_for(iface: &str) -> Option<String> {
    if let Some((tag, _)) = HTML_TAGS.iter().find(|(_, i)| *i == iface) {
        return Some(format!("document.createElement(\"{tag}\")"));
    }
    if let Some((tag, _)) = SVG_TAGS.iter().find(|(_, i)| *i == iface) {
        return Some(format!("document.createElementNS(\"{SVG_NS}\", \"{tag}\")"));
    }
    if let Some((tag, _)) = MATHML_TAGS.iter().find(|(_, i)| *i == iface) {
        return Some(format!("document.createElementNS(\"{MATHML_NS}\", \"{tag}\")"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Blink's default when `interfaceName` is absent is NOT HTMLElement -- it is derived from the tag.
    // Eighteen tags rely on it, `input` among them, so getting this wrong loses the biggest entries.
    #[test]
    fn the_table_uses_blinks_own_derivation() {
        assert_eq!(html_iface_for_tag("input"), Some("HTMLInputElement"), "derived: no interfaceName");
        assert_eq!(html_iface_for_tag("audio"), Some("HTMLAudioElement"), "derived");
        assert_eq!(html_iface_for_tag("a"), Some("HTMLAnchorElement"), "declared interfaceName");
        assert_eq!(html_iface_for_tag("wbr"), Some("HTMLElement"), "JSInterfaceName overrides the C++ name");
        assert_eq!(html_iface_for_tag("fencedframe"), Some("HTMLFencedFrameElement"), "the Chrome surface exposes it");
    }

    #[test]
    fn a_receiver_is_created_in_the_right_namespace() {
        assert_eq!(create_expr_for("HTMLInputElement").as_deref(), Some(r#"document.createElement("input")"#));
        assert_eq!(
            create_expr_for("SVGSVGElement").as_deref(),
            Some(r#"document.createElementNS("http://www.w3.org/2000/svg", "svg")"#)
        );
        assert!(create_expr_for("MathMLElement").is_some_and(|e| e.contains("1998/Math/MathML")));
        assert_eq!(create_expr_for("URLSearchParams"), None, "not an element");
    }

    // `a`, `title`, `style` and `script` exist in BOTH namespaces. Folding the tables together would
    // hand SVG's tests and receivers to HTML's interface (or the reverse, depending on order).
    #[test]
    fn colliding_tags_stay_in_their_own_namespace() {
        assert_eq!(html_iface_for_tag("a"), Some("HTMLAnchorElement"));
        assert_eq!(SVG_TAGS.iter().find(|(t, _)| *t == "a").map(|(_, i)| *i), Some("SVGAElement"));
        assert_eq!(html_iface_for_tag("title"), Some("HTMLTitleElement"));
        assert_eq!(SVG_TAGS.iter().find(|(t, _)| *t == "title").map(|(_, i)| *i), Some("SVGTitleElement"));
    }
}
