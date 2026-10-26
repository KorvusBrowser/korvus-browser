//! The user-agent stylesheet.
//!
//! Every browser has one, and it is where most of what a page looks like before it says anything
//! comes from: that a `<div>` is a block and a `<span>` is not, that `<h1>` is 2em and bold, that
//! `<ul>` carries a bullet. Without it every element is `display: inline` with no margins, and
//! every page is a wall of text.
//!
//! It also settles a question a page can ask in one line. `getComputedStyle(el).display` on an
//! element nobody styled is answered entirely by this sheet, and Chrome's answer is Chrome's UA
//! sheet. `host.js` used to answer it from a hardcoded set of forty tag names and a guess of
//! `block` for anything else, which is right for `<div>` and wrong for `<button>`, `<progress>`,
//! `<summary>`, and everything added to HTML since.
//!
//! The sheet is embedded rather than read from disk: it is not configuration, it is part of what
//! this engine *is*, and a build that could not find it would be a browser with no defaults.

use crate::style::parse::parse_stylesheet;
use crate::style::rules::{Origin, Stylesheet};

/// Chromium's default stylesheet, filtered to what this engine understands. See
/// `tools/extract-ua-stylesheet.py` for what is dropped and why.
pub const HTML_CSS: &str = include_str!("../../ua/html.css");

/// The document's default font, which is a *browser setting* and not a spec value.
///
/// Chrome calls it the "Standard font", and on macOS -- the persona -- it is Times at 16px. The
/// property table leaves `font-family` empty for exactly this reason: there is no initial value to
/// state, only a preference to read. Applying it as a `html { … }` rule at the bottom of the
/// user-agent sheet is what Chrome does with it too, which is why an author rule overrides it and
/// an inherited one does not fight it.
///
/// Without this, `getComputedStyle(el).fontFamily` is the empty string on every element, which no
/// browser has ever reported.
///
/// # The family: the PERSONA's standard font
///
/// A document with no `font-family` uses the browser's "Standard font" preference, which Chrome seeds
/// from the host's fontconfig at first run. On the persona's host -- a `ko_KR` Linux box, where the
/// bundle `brave-151-linux-20260907T1006` was captured -- that is **Noto Sans CJK KR**: the persona's
/// own `getComputedStyle(body).fontFamily` is `"Noto Sans CJK KR"` and its `body.offsetHeight` 24
/// (ascent 2376/2048 em, descent 590/2048, no line gap: `round(1.160156 × 16) + round(0.288086 × 16)`
/// = 19 + 5). This engine said `"Times New Roman"` and 18: the family was a guess ("the persona is a
/// Mac"), not a measurement. It is a measurement now -- `fonts/metrics.json`'s `default`, captured by
/// `plugins/korvus-persona/capture/fonts-probe.html` in the persona's browser -- and `sans-serif`,
/// `serif` and `monospace` moved with it (`font_table::GENERICS`), so the body's family and every
/// generic measurement tell one story.
///
/// The FAMILY only: the size is the initial `medium`, a KEYWORD size, as Chrome's is (`FontBuilder::InitialSize`).
/// Declaring `font-size: 16px` here made every document's size absolute from the root down, so a bare
/// `monospace` element kept 16px where Chrome's `<pre>`/`<code>` compute 13px, and a `0.5em` escaped the
/// Korean UI's 10px minimum (measured).
fn default_font() -> String {
    // the worn persona's standard font (`font::worn_font_map`): a Windows persona's is Malgun Gothic, not Noto Sans CJK KR
    format!("html {{ font-family: \"{}\" }}", crate::font::worn_font_map().default_family)
}

/// `<details>`' content slot, which Blink styles from the element's user-agent shadow tree rather than
/// from `html.css`. Chrome 150: `getComputedStyle(details, "::details-content")` is
/// `display: block` with `content-visibility: hidden` when closed and `visible` when `open`.
const DETAILS_CONTENT: &str = "details::details-content { display: block; content-visibility: hidden }\n\
details[open]::details-content { content-visibility: visible }";

/// Blink's `svg.css`, the user-agent rules for SVG elements, under the `svg` and `html` prefixes [`stylesheet`]
/// declares (Blink's sheet has SVG for its default namespace; `html.css` here has none). Chrome 150, measured
/// (`svgua.html`): a nested `<svg>`, `<symbol>`, `<image>`, `<marker>`, `<pattern>` and `<foreignObject>` compute
/// `overflow: hidden`; every SVG element's `transform-origin` is `0px 0px` but an `<svg>` in HTML's (`50% 50%`, its
/// box's centre); a `<text>` and what it holds are `nowrap`. This engine had none of them, and computed `visible`,
/// `50% 50%` and `normal`.
const SVG_CSS: &str = "svg|svg:not(:root), svg|symbol, svg|image, svg|marker, svg|pattern, svg|foreignObject { overflow: hidden }\n\
svg|svg:root { width: 100%; height: 100% }\n\
svg|text, svg|foreignObject { display: block }\n\
svg|text { text-wrap-mode: nowrap }\n\
svg|* { transform-origin: 0 0 }\n\
html|* > svg|svg { transform-origin: 50% 50% }";

/// A range input's parts, which `html.css` styles with pseudo-element selectors the extractor leaves out (Blink's
/// `html.css`): the thumb's `appearance: auto` is what makes Chrome draw its theme thumb until a page sets `none`
/// (Chrome 150: `getComputedStyle(range, "::-webkit-slider-thumb").appearance` is `auto`). Without it the thumb
/// computed `none`, and no range showed one.
const SLIDER_CSS: &str = "input[type=\"range\" i]::-webkit-slider-container { appearance: inherit; flex: 1; min-inline-size: 0; box-sizing: border-box; display: flex }\n\
input[type=\"range\" i]::-webkit-slider-runnable-track { flex: 1; min-inline-size: 0; align-self: center; box-sizing: border-box; display: block }\n\
input[type=\"range\" i]::-webkit-slider-thumb { appearance: auto; box-sizing: border-box; display: block }";

/// The date and time inputs' own UA rules (Blink's `input_multiple_fields.css`, which the extractor does not read):
/// set in `monospace`, no padding but 1px at the start (Chrome 150: `getComputedStyle(date).fontFamily` is `monospace`,
/// its padding `0px 0px 0px 1px`; they read the text field's here).
const DATETIME_CSS: &str = "input[type=\"date\" i], input[type=\"datetime-local\" i], input[type=\"month\" i], input[type=\"time\" i], \
input[type=\"week\" i] { font-family: monospace; padding: 0; padding-inline-start: 1px; cursor: default }";

/// Parse the user-agent stylesheet. It goes first in the cascade order, below every author rule.
pub fn stylesheet() -> Stylesheet {
    parse_stylesheet(
        Origin::UserAgent,
        &format!(
            "@namespace svg url(http://www.w3.org/2000/svg);\n@namespace html url(http://www.w3.org/1999/xhtml);\n\
             {HTML_CSS}\n{}\n{DETAILS_CONTENT}\n{SLIDER_CSS}\n{DATETIME_CSS}\n{SVG_CSS}\n",
            default_font()
        ),
    )
}

/// Blink's quirks-mode user-agent sheet (`core/html/resources/quirks.css`), which applies to a
/// quirks-mode document and to nothing else. Reproduced as data, rule for rule, less
/// `color: -internal-quirk-inherit` (an engine-private keyword whose effect -- a table inherits its
/// colour -- is what `color` does anyway). Measured on Chrome 150, no doctype:
/// `<div style="font-size:30px;font-weight:bold"><table><tr><td>` computes the cell `16px 400`
/// (`30px 700` with `<!doctype html>`).
pub const QUIRKS_CSS: &str = r#"
img[align="left" i] { margin-right: 3px }
img[align="right" i] { margin-left: 3px }
table { white-space: normal; line-height: normal; font-weight: normal; font-size: medium; font-variant: normal; font-style: normal; text-align: start }
input:not([type=image i]), textarea { box-sizing: border-box }
form { margin-block-end: 1em }
li { list-style-position: inside }
li dir, li menu, li ol, li ul { list-style-position: outside }
:is(dir, menu, ol, ul) dir, :is(dir, menu, ol, ul) menu, :is(dir, menu, ol, ul) ol, :is(dir, menu, ol, ul) ul, :is(dir, menu, ol, ul) li { list-style-position: unset }
"#;

/// The quirks sheet, DISABLED: the page enables it for a quirks-mode document.
pub fn quirks_stylesheet() -> Stylesheet {
    let mut sheet = parse_stylesheet(Origin::UserAgent, QUIRKS_CSS);
    sheet.disabled = true;
    sheet
}

/// The presentational hints HTML attributes map to (HTML §15.3.1), as a sheet of their own origin.
/// Blink maps these in `HTMLElement::CollectStyleForPresentationAttribute`, not in its
/// `html.css`, so the UA-sheet extractor never saw them -- and `<div hidden>` was laid out and read by
/// `innerText`. Chrome 150: `<div hidden>` computes `display: none`, and `<div hidden
/// style="display:block">` is shown (an author declaration beats a hint). `:where()` because a hint
/// has no specificity. An `<embed>` maps `hidden` itself, to a zero size (`HTMLEmbedElement`,
/// `style::hints`): Chrome 150 lays `<embed hidden>` out inline, 0x0, `display: inline`, whatever the value.
pub const HINTS_CSS: &str = r#"
:where([hidden]:not([hidden="until-found" i]):not(embed)) { display: none }
:where([hidden="until-found" i]:not(embed)) { content-visibility: hidden }
"#;

pub fn hints_stylesheet() -> Stylesheet {
    parse_stylesheet(Origin::PresentationalHint, HINTS_CSS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::compute::{compute, ComputeContext, ParentFormatting};
    use crate::style::computed::StyleArena;
    use crate::style::matching::Matcher;
    use crate::style::media::MediaContext;
    use crate::style::property::PropertyId;
    use korvus_dom::{Dom, NodeId, NodeKind};

    /// The computed value of `property` on the first `<tag>` in `html`, with only the UA sheet
    /// applied — which is the question `getComputedStyle` asks about an unstyled element.
    fn ua_value(html: &str, tag: &str, property: PropertyId) -> String {
        let sheets = vec![stylesheet()];
        let matcher = Matcher::build(&sheets);
        let dom = Dom::parse(html);
        let mut arena = StyleArena::default();
        let media = MediaContext::default();
        let mut matched = Vec::new();

        let target = (0..dom.len() as NodeId)
            .find(|&n| match &dom.node(n).kind {
                NodeKind::Element { name, .. } => name.eq_ignore_ascii_case(tag),
                _ => false,
            })
            .unwrap_or_else(|| panic!("no <{tag}>"));

        let mut chain = vec![target];
        let mut cur = dom.node(target).parent;
        while let Some(n) = cur {
            chain.push(n);
            cur = dom.node(n).parent;
        }
        chain.reverse();

        let mut parent = None;
        let mut formatting = ParentFormatting::Flow;
        for n in chain {
            if !matches!(dom.node(n).kind, NodeKind::Element { .. }) {
                continue;
            }
            matcher.match_element(&sheets, &dom, n, &media, &mut matched);
            let key = compute(
                &sheets, &matched, None, parent, formatting, &ComputeContext::default(), &mut arena,
            );
            formatting = ParentFormatting::of_children(&arena, key, formatting);
            parent = Some(key);
        }
        arena.value(parent.unwrap(), property).to_string()
    }

    fn display(html: &str, tag: &str) -> String {
        ua_value(html, tag, PropertyId::Display)
    }

    // Chromium's sheet writes its box defaults in LOGICAL properties, and the extractor used to drop
    // them. Chrome 150, no author CSS, a `<!doctype html>` page:
    //   <p> margin 16px 16px · <h1> 21.44px · <ul> 16px + padding-left 40px
    //   <input>  13.3333px Arial, padding 1px 2px, border 2px inset rgb(118, 118, 118), bg white
    //   <button> padding 1px 6px, border 2px outset · <textarea> monospace, padding 2px, border 1px solid
    #[test]
    fn the_logical_box_defaults_reach_the_elements_they_describe() {
        assert_eq!(ua_value("<p>x</p>", "p", PropertyId::MarginTop), "16px");
        assert_eq!(ua_value("<p>x</p>", "p", PropertyId::MarginBottom), "16px");
        assert_eq!(ua_value("<h1>x</h1>", "h1", PropertyId::MarginTop), "21.44px");
        assert_eq!(ua_value("<ul><li>x</li></ul>", "ul", PropertyId::MarginTop), "16px");
        assert_eq!(ua_value("<ul><li>x</li></ul>", "ul", PropertyId::PaddingLeft), "40px");
    }

    #[test]
    fn a_form_control_gets_the_ua_font_padding_and_border_chrome_applies() {
        let input = |p| ua_value("<input>", "input", p);
        assert_eq!(input(PropertyId::FontFamily), "Arial");
        assert_eq!(input(PropertyId::FontSize), "13.3333px");
        assert_eq!(input(PropertyId::PaddingTop), "1px");
        assert_eq!(input(PropertyId::PaddingLeft), "2px");
        assert_eq!(input(PropertyId::BorderTopWidth), "2px");
        assert_eq!(input(PropertyId::BorderTopStyle), "inset");
        assert_eq!(input(PropertyId::BorderTopColor), "rgb(118, 118, 118)");
        assert_eq!(input(PropertyId::BackgroundColor), "rgb(255, 255, 255)");
        assert_eq!(ua_value("<textarea></textarea>", "textarea", PropertyId::FontFamily), "monospace");
        assert_eq!(ua_value("<textarea></textarea>", "textarea", PropertyId::PaddingTop), "2px");
        assert_eq!(ua_value("<button>go</button>", "button", PropertyId::BorderTopStyle), "outset");
        assert_eq!(ua_value("<button>go</button>", "button", PropertyId::PaddingLeft), "6px");
    }

    // The default font is a browser *setting*, and no browser reports an empty `font-family`.
    #[test]
    fn the_document_has_a_default_font() {
        // The persona's standard font: Brave 151 on its ko_KR host answers
        // `getComputedStyle(p).fontFamily` with `"Noto Sans CJK KR"` (fonts-probe capture, `domDefault`).
        assert_eq!(ua_value("<p>x</p>", "p", PropertyId::FontFamily), "\"Noto Sans CJK KR\"");
        assert_eq!(ua_value("<p>x</p>", "p", PropertyId::FontSize), "16px");
    }

    #[test]
    fn the_sheet_parses_into_rules() {
        let sheet = stylesheet();
        assert!(sheet.rules().len() > 80, "got {} rules", sheet.rules().len());
        assert_eq!(sheet.origin, Origin::UserAgent);
    }

    #[test]
    fn the_blocks_are_blocks_and_the_inlines_are_inline() {
        assert_eq!(display("<div>x</div>", "div"), "block");
        assert_eq!(display("<p>x</p>", "p"), "block");
        assert_eq!(display("<section>x</section>", "section"), "block");
        assert_eq!(display("<span>x</span>", "span"), "inline");
        assert_eq!(display("<em>x</em>", "em"), "inline", "the initial value, unmentioned");
    }

    // The tags the old hardcoded map got wrong, which is the reason this file exists.
    #[test]
    fn the_tags_a_forty_name_list_did_not_know() {
        assert_eq!(display("<li>x</li>", "li"), "list-item", "not `block`");
        assert_eq!(display("<table><tr><td>x</td></tr></table>", "table"), "table");
        assert_eq!(display("<table><tr><td>x</td></tr></table>", "tr"), "table-row");
        assert_eq!(display("<table><tr><td>x</td></tr></table>", "td"), "table-cell");
        assert_eq!(display("<table><tbody><tr><td>x</td></tr></tbody></table>", "tbody"), "table-row-group");
        assert_eq!(display("<details><summary>x</summary></details>", "details"), "block");
        assert_eq!(display("<fieldset>x</fieldset>", "fieldset"), "block");
        assert_eq!(display("<figure>x</figure>", "figure"), "block");
    }

    #[test]
    fn the_invisible_elements_are_display_none() {
        for tag in ["head", "script", "style", "title", "meta", "link", "template"] {
            assert_eq!(display(&format!("<{tag}></{tag}>"), tag), "none", "<{tag}>");
        }
    }

    // The sheet writes `2em` and `bold`; what comes out is what Chrome reports -- the em resolved
    // against the 16px `<body>` inherits, and the keyword as its number.
    #[test]
    fn headings_carry_their_size_and_weight() {
        assert_eq!(ua_value("<h1>x</h1>", "h1", PropertyId::FontSize), "32px");
        assert_eq!(ua_value("<h1>x</h1>", "h1", PropertyId::FontWeight), "700");
        assert_eq!(ua_value("<h3>x</h3>", "h3", PropertyId::FontSize), "18.72px");
    }

    // The 8px on `<body>` is why a page has a margin at all, and it is the first number anybody
    // measuring geometry sees.
    #[test]
    fn body_has_its_eight_pixel_margin() {
        for property in [
            PropertyId::MarginTop,
            PropertyId::MarginRight,
            PropertyId::MarginBottom,
            PropertyId::MarginLeft,
        ] {
            assert_eq!(ua_value("<body>x</body>", "body", property), "8px");
        }
    }

    #[test]
    fn lists_get_their_markers() {
        assert_eq!(ua_value("<ul><li>x</li></ul>", "ul", PropertyId::ListStyleType), "disc");
        assert_eq!(ua_value("<ol><li>x</li></ol>", "ol", PropertyId::ListStyleType), "decimal");
    }

    // A system colour that does not resolve makes the whole `border` shorthand it sits in invalid,
    // so a `<fieldset>` would silently lose its border.
    #[test]
    fn the_rules_written_in_system_colours_survive() {
        assert_eq!(ua_value("<fieldset>x</fieldset>", "fieldset", PropertyId::BorderTopWidth), "2px");
        assert_eq!(ua_value("<fieldset>x</fieldset>", "fieldset", PropertyId::BorderTopStyle), "groove");
        assert_eq!(ua_value("<input>", "input", PropertyId::Color), "rgb(0, 0, 0)");
    }

    // Inherited UA values reach down the tree like any other.
    #[test]
    fn a_table_cell_inherits_what_the_table_sets() {
        assert_eq!(
            ua_value("<table><tr><th>x</th></tr></table>", "th", PropertyId::FontWeight),
            "700"
        );
    }

    // Chrome 150 (`svgua.html`): Blink's `svg.css`, under its namespaces.
    #[test]
    fn svg_elements_take_blinks_svg_sheet() {
        let page = "<!doctype html><body><svg><g><rect/></g><foreignObject><div>a</div></foreignObject><svg></svg><text>t</text></svg>";
        assert_eq!(ua_value(page, "svg", PropertyId::TransformOrigin), "50% 50%", "an <svg> in HTML is about its centre");
        assert_eq!(ua_value(page, "rect", PropertyId::TransformOrigin), "0px 0px");
        assert_eq!(ua_value(page, "foreignObject", PropertyId::OverflowX), "hidden");
        assert_eq!(ua_value(page, "g", PropertyId::OverflowX), "visible");
        assert_eq!(ua_value(page, "div", PropertyId::TransformOrigin), "50% 50%", "an HTML element in a foreignObject is not SVG");
        assert_eq!(ua_value(page, "text", PropertyId::TextWrapMode), "nowrap");
    }

}
