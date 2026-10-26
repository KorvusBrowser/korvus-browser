//! Presentational hints: the attributes HTML maps to CSS declarations (HTML §15, "Rendering"), as
//! Blink's `CollectStyleForPresentationAttribute` writes them, for `dir`, the table family and the
//! embedded and replaced elements.
//!
//! They cascade as their own origin, below every author rule and above the user agent's (CSS Cascade 5
//! §6.1; Blink's `kAuthorPresentationalHint`): `compute` reads them as the leading declarations of the
//! element's attached block (`InlineStyle::hints`). Measured on Chrome 150.0.7871.46: a cell of a bare
//! `<table>` has `padding: 1px` (the table's shared cell style, `cellpadding` 1 by default) -- this
//! engine gave it none, so every row was 2px short; `<table border=1 cellpadding=4 cellspacing=0>`
//! puts 1px outset borders on the table, 1px inset ones on its cells, 4px padding in them and no
//! spacing between them.
//!
//! The embedded elements': `width`/`height` on `<img>`, `<iframe>`, `<object>`,
//! `<embed>`, `<video>` and an image button are CSS `width`/`height` -- a percentage included -- and an
//! `<img>`, `<video>` or image button with both also gets `aspect-ratio: auto w / h` (a `<canvas>` only
//! that, from its integers); `hspace`/`vspace` are margins, `border` a solid border, `align` a float and
//! a `vertical-align`, an iframe's `frameborder` of 0 takes its border away. This engine mapped none of
//! them and read the dimensions as the box's NATURAL size, refusing a percentage: a device-check script's device-check
//! frame `<iframe width="100%" height="100%" frameborder="0">` laid out 304 wide where Chrome 150 lays it
//! out at the page's width, so inside it `innerWidth` read 300 and the check flagged "devtools open"; an
//! `<img width="50%">` computed 0px (`fixtures/presentational-hints`).

use korvus_dom::{Dom, NodeId};

/// The presentational-hint declarations of element `node`, as one declaration block's text (empty when
/// it has none).
pub fn for_element(dom: &Dom, node: NodeId) -> String {
    if !dom.is_html_ns(node) {
        return if dom.namespace_uri(node) == Some(SVG_NS) { for_svg_element(dom, node) } else { String::new() };
    }
    let Some(raw) = dom.tag_name(node) else { return String::new() };
    let mut out = String::new();
    // `dir` on EVERY HTML element (HTMLElement::CollectStyleForPresentationAttribute): `ltr` / `rtl`
    // set `direction`; any other value but `auto` sets `ltr` on `<body>` alone. `auto` and the
    // `unicode-bidi: isolate` every `[dir]` gets are the UA sheet's.
    if let Some(v) = dom.get_attribute(node, "dir") {
        if v.eq_ignore_ascii_case("ltr") || v.eq_ignore_ascii_case("rtl") {
            push(&mut out, "direction", &v.to_ascii_lowercase());
        } else if !v.eq_ignore_ascii_case("auto") && raw.eq_ignore_ascii_case("body") {
            push(&mut out, "direction", "ltr");
        }
    }
    // `<font>` (HTMLFontElement::CollectStyleForPresentationAttribute): `size` as an absolute-size keyword,
    // `color` as a legacy colour, a non-empty `face` as a `font-family` list. Chrome 150 computes `<font
    // size=1>` 10px, `size=2` 13px, `size=7` 48px; this engine mapped none of them.
    if raw.eq_ignore_ascii_case("font") {
        if let Some(k) = dom.get_attribute(node, "size").and_then(|v| legacy_font_size(&v)) {
            push(&mut out, "font-size", k);
        }
        if let Some(c) = dom.get_attribute(node, "color").and_then(legacy_color) {
            push(&mut out, "color", &c);
        }
        if let Some(face) = dom.get_attribute(node, "face").filter(|f| !f.is_empty()) {
            if crate::style::supports::supports_declaration("font-family", &face) {
                push(&mut out, "font-family", &face);
            }
        }
        return out;
    }
    if let Some(kind) = Embedded::of(dom, node, raw) {
        embedded(dom, node, kind, &mut out);
        return out;
    }
    // every element of every cascade asks: the ones with no hints answer without allocating
    const FAMILY: [&str; 9] = ["table", "td", "th", "tr", "thead", "tbody", "tfoot", "col", "colgroup"];
    if !FAMILY.iter().any(|t| raw.eq_ignore_ascii_case(t)) {
        return out;
    }
    let tag = raw.to_ascii_lowercase();
    let attr = |name: &str| dom.get_attribute(node, name);
    match tag.as_str() {
        "table" => {
            if let Some(v) = attr("width").and_then(|v| html_length(v, true, false)) {
                push(&mut out, "width", &v);
            }
            if let Some(v) = attr("height").and_then(|v| html_length(v, true, true)) {
                push(&mut out, "height", &v);
            }
            let border = attr("border").map(border_width);
            if let Some(w) = border {
                push(&mut out, "border-width", &format!("{w}px"));
            }
            let border_color = attr("bordercolor").is_some_and(|v| !v.is_empty());
            if border_color {
                if let Some(c) = attr("bordercolor").and_then(legacy_color) {
                    push(&mut out, "border-color", &c);
                }
            }
            if let Some(c) = attr("bgcolor").and_then(legacy_color) {
                push(&mut out, "background-color", &c);
            }
            if let Some(v) = attr("background").filter(|v| !v.trim().is_empty()) {
                push(&mut out, "background-image", &format!("url(\"{}\")", v.trim().replace('"', "%22")));
            }
            if let Some(v) = attr("cellspacing").filter(|v| !v.is_empty()).and_then(|v| html_length(v, false, true)) {
                push(&mut out, "border-spacing", &v);
            }
            if let Some(v) = attr("align").filter(|v| !v.is_empty()) {
                if v.eq_ignore_ascii_case("center") {
                    push(&mut out, "margin-inline-start", "auto");
                    push(&mut out, "margin-inline-end", "auto");
                } else if v.eq_ignore_ascii_case("left") || v.eq_ignore_ascii_case("right") {
                    push(&mut out, "float", &v.to_ascii_lowercase());
                }
            }
            let rules = rules_of(dom, node);
            if rules != Rules::Unset {
                push(&mut out, "border-collapse", "collapse");
            }
            // The border's style. A `frame` names the sides that have one, each `solid` and the others `hidden`, and
            // makes the width `thin` -- unless a `border` after it in the element's attributes gives the width, the
            // later of the two winning as each attribute's declarations are collected in turn; otherwise a `border`
            // above 0 makes it `outset` -- `solid` with a `bordercolor` -- and `rules` alone `hidden`. Chrome 152
            // (`tbl.html`, `tblf.html`, `tblf1.html`, 2026-10-23): `<table bordercolor=red>` and `border=0
            // bordercolor=red` have NO border style (`none`, `0px`), `border=1 bordercolor=red` a `solid` 1px one,
            // `bordercolor=red rules=all` a `hidden` one, `frame=above` `solid 1px` on top and `hidden` around, `frame=box
            // rules=all` and `frame=box bordercolor=red` `solid 1px`, `frame=box border=3` `3px` and `border=3 frame=box`
            // `1px`, `frame=void border=3` `hidden`, `frame=bogus` none of it. A `bordercolor` alone made the border
            // `solid`, and with `medium` the width nothing set, a 3px one.
            if let Some(sides) = attr("frame").and_then(frame_sides) {
                let at = |name: &str| dom.attribute_pairs(node).iter().position(|(n, _)| n == name);
                if border.is_none() || at("frame") > at("border") {
                    push(&mut out, "border-width", "thin");
                }
                for (side, on) in ["top", "right", "bottom", "left"].iter().zip(sides) {
                    push(&mut out, &format!("border-{side}-style"), if on { "solid" } else { "hidden" });
                }
            } else if border.is_some_and(|w| w > 0) {
                push(&mut out, "border-style", if border_color { "solid" } else { "outset" });
            } else if rules != Rules::Unset {
                push(&mut out, "border-style", "hidden");
            }
        }
        "td" | "th" => {
            if attr("nowrap").is_some() {
                push(&mut out, "white-space-collapse", "collapse");
                push(&mut out, "text-wrap-mode", "nowrap");
            }
            if let Some(v) = attr("width").filter(|v| !v.is_empty()).and_then(|v| html_length(v, true, false)) {
                push(&mut out, "width", &v);
            }
            if let Some(v) = attr("height").filter(|v| !v.is_empty()).and_then(|v| html_length(v, true, false)) {
                push(&mut out, "height", &v);
            }
            table_part(dom, node, &mut out);
            if let Some(table) = parent_table(dom, node) {
                cell_style_of(dom, table, &mut out);
            }
        }
        // `HTMLTableColElement`: its `width` (any dimension), then the table part's. Chrome 150: `<col width=100>`
        // computes `100px`, `width="0"` `0px` (this engine mapped no `width` on either).
        "col" | "colgroup" => {
            if let Some(v) = attr("width").and_then(|v| html_length(v, true, true)) {
                push(&mut out, "width", &v);
            }
            table_part(dom, node, &mut out);
        }
        "tr" | "thead" | "tbody" | "tfoot" => table_part(dom, node, &mut out),
        _ => {}
    }
    out
}

/// The embedded and replaced elements whose presentational attributes Blink maps in their own
/// `CollectStyleForPresentationAttribute` (Chromium 150, `core/html/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Embedded {
    /// `HTMLImageElement`.
    Img,
    /// `HTMLIFrameElement`.
    Iframe,
    /// `HTMLObjectElement`: `border`, then `HTMLPlugInElement`'s.
    Object,
    /// `HTMLEmbedElement`: `hidden`, then `HTMLPlugInElement`'s.
    Embed,
    /// `HTMLVideoElement`: `width`/`height`, then `HTMLMediaElement`'s, which are `HTMLElement`'s.
    Video,
    /// `HTMLAudioElement`: `HTMLElement`'s.
    Audio,
    /// `HTMLCanvasElement`: the ratio of its bitmap's `width`/`height`, then `HTMLElement`'s.
    Canvas,
    /// `HTMLInputElement`; `image` when its type is the image button, the one type whose `width`, `height`,
    /// `align` and `border` it maps (`ImageInputType::ShouldRespectHeightAndWidthAttributes` /
    /// `ShouldRespectAlignAttribute`; the type is the attribute's ASCII-lowercased value, untrimmed).
    Input { image: bool },
}

impl Embedded {
    fn of(dom: &Dom, node: NodeId, tag: &str) -> Option<Embedded> {
        // asked of every element: a tag that cannot be one answers on its first byte
        if tag.len() > 6 || !matches!(tag.as_bytes().first().map(u8::to_ascii_lowercase), Some(b'i' | b'o' | b'e' | b'v' | b'a' | b'c')) {
            return None;
        }
        let is = |t: &str| tag.eq_ignore_ascii_case(t);
        Some(if is("img") {
            Embedded::Img
        } else if is("iframe") {
            Embedded::Iframe
        } else if is("object") {
            Embedded::Object
        } else if is("embed") {
            Embedded::Embed
        } else if is("video") {
            Embedded::Video
        } else if is("audio") {
            Embedded::Audio
        } else if is("canvas") {
            Embedded::Canvas
        } else if is("input") {
            Embedded::Input { image: dom.get_attribute(node, "type").is_some_and(|t| t.eq_ignore_ascii_case("image")) }
        } else {
            return None;
        })
    }
}

/// An embedded element's hints, attribute by attribute in the element's own attribute order, as Blink's
/// `CreatePresentationAttributeStyle` collects them -- a later declaration of a property wins (Chrome 150:
/// `<embed width=100 hidden>` is 0 wide, `<embed hidden width=120>` 120; `<img align=left valign=bottom>`
/// floats and sits at the bottom).
fn embedded(dom: &Dom, node: NodeId, kind: Embedded, out: &mut String) {
    use Embedded::*;
    let attr = |name: &str| dom.get_attribute(node, name);
    for (name, value) in dom.attribute_pairs(node) {
        let value = value.as_str();
        match (kind, name.as_str()) {
            // `AddHTMLLengthToStyle` -- and `ApplyAspectRatioToStyle` when the other attribute is there too
            // (an empty one included: Blink asks whether it is present)
            (Img | Iframe | Object | Embed | Video | Input { image: true }, "width" | "height") => {
                let width = name == "width";
                if let Some(v) = html_length(value, true, true) {
                    push(out, if width { "width" } else { "height" }, &v);
                }
                if matches!(kind, Img | Video | Input { .. }) {
                    let pair = if width { attr("height").map(|h| (value, h)) } else { attr("width").map(|w| (w, value)) };
                    if let Some(r) = pair.and_then(|(w, h)| dimension_ratio(w, h)) {
                        push(out, "aspect-ratio", &r);
                    }
                }
            }
            // `ApplyIntegerAspectRatioToStyle`: a canvas's `width`/`height` size its bitmap, never its box
            (Canvas, "width" | "height") => {
                let pair = if name == "width" { attr("height").map(|h| (value, h)) } else { attr("width").map(|w| (w, value)) };
                if let Some((w, h)) = pair.and_then(|(w, h)| Some((non_negative_integer(w)?, non_negative_integer(h)?))) {
                    push(out, "aspect-ratio", &format!("auto {w} / {h}"));
                }
            }
            // every input type's, not only the image button's
            (Img | Object | Embed | Input { .. }, "vspace") => {
                if let Some(v) = html_length(value, true, true) {
                    push(out, "margin-top", &v);
                    push(out, "margin-bottom", &v);
                }
            }
            (Img | Object | Embed | Input { .. }, "hspace") => {
                if let Some(v) = html_length(value, true, true) {
                    push(out, "margin-left", &v);
                    push(out, "margin-right", &v);
                }
            }
            (Img | Object | Input { image: true }, "border") => border_attribute(value, out),
            (Img | Iframe | Object | Embed | Input { image: true }, "align") => alignment(value, out),
            // `HTMLElement`'s own `align`: the value as a `text-align`, `middle` being `center`
            (Video | Audio | Canvas, "align") => {
                if value.eq_ignore_ascii_case("middle") {
                    push(out, "text-align", "center");
                } else if let Some(v) = css_value("text-align", value) {
                    push(out, "text-align", &v);
                }
            }
            (Img, "valign") => {
                if let Some(v) = css_value("vertical-align", value) {
                    push(out, "vertical-align", &v);
                }
            }
            // "It simply adds a presentational hint that the border should be off if set to zero"
            // (`StringToIntLoose`, which reads `no`, `yes`, an empty value and an overflowing one as 0)
            (Iframe, "frameborder") => {
                if loose_i32(value).unwrap_or(0) == 0 {
                    for p in ["border-top-width", "border-bottom-width", "border-left-width", "border-right-width"] {
                        push(out, p, "0px");
                    }
                }
            }
            // an `<embed>`'s `hidden` is a zero size, never `display: none` (the UA hint sheet leaves it out)
            (Embed, "hidden") => {
                push(out, "width", "0px");
                push(out, "height", "0px");
            }
            _ => {}
        }
    }
}

/// `ApplyAlignmentAttributeToStyle`: `left`/`right` float the element to the top of its line, the rest
/// are a `vertical-align` (`middle` and `center` are `-webkit-baseline-middle` -- Chromium 152's stable
/// `EmbeddedContentCenterAlignBaseline`; Chromium 150 made `center` `middle` -- `absmiddle` is `middle`,
/// `bottom` is `baseline`); any other value maps nothing. ASCII case-insensitive, untrimmed.
fn alignment(value: &str, out: &mut String) {
    let is = |k: &str| value.eq_ignore_ascii_case(k);
    let (float, vertical) = if is("absmiddle") || is("abscenter") {
        (None, "middle")
    } else if is("absbottom") {
        (None, "bottom")
    } else if is("left") {
        (Some("left"), "top")
    } else if is("right") {
        (Some("right"), "top")
    } else if is("top") {
        (None, "top")
    } else if is("middle") || is("center") {
        (None, "-webkit-baseline-middle")
    } else if is("bottom") {
        (None, "baseline")
    } else if is("texttop") {
        (None, "text-top")
    } else {
        return;
    };
    if let Some(f) = float {
        push(out, "float", f);
    }
    push(out, "vertical-align", vertical);
}

/// `ApplyBorderAttributeToStyle` off a `<table>`: `ParseBorderWidthAttribute`'s width -- a non-negative
/// integer, 0 when empty or unparsable -- on all four sides, and a `solid` style whatever the width (Chrome
/// 150: `<img border=0>` computes `0px solid`).
fn border_attribute(value: &str, out: &mut String) {
    let width = format!("{}px", non_negative_integer(value).unwrap_or(0));
    for p in ["border-top-width", "border-bottom-width", "border-left-width", "border-right-width"] {
        push(out, p, &width);
    }
    for p in ["border-top-style", "border-bottom-style", "border-left-style", "border-right-style"] {
        push(out, p, "solid");
    }
}

/// `ApplyAspectRatioToStyle(width, height)`: `auto w / h` when both are absolute dimensions (no percentage,
/// no relative `*`).
fn dimension_ratio(width: &str, height: &str) -> Option<String> {
    match (dimension(width)?, dimension(height)?) {
        (Dimension::Absolute(w), Dimension::Absolute(h)) => {
            Some(format!("auto {} / {}", crate::value::serialize_number_f64(w), crate::value::serialize_number_f64(h)))
        }
        _ => None,
    }
}

/// `value` as one declaration's value of `property` when it is one -- Blink parses an attribute's value alone
/// (`AddPropertyToPresentationAttributeStyle`), so `valign="top; color: red"` is no value at all -- trimmed and
/// ASCII-lowercased (keywords and lengths only reach here).
fn css_value(property: &str, value: &str) -> Option<String> {
    let v = value.trim();
    (self_contained(v) && crate::style::supports::supports_declaration(property, v)).then(|| v.to_ascii_lowercase())
}

/// `StringToIntLoose`: leading whitespace, an optional sign, ASCII digits -- one at least -- and anything after
/// ignored; `None` without digits or past `i32`.
fn loose_i32(value: &str) -> Option<i32> {
    let t = value.trim_start_matches(char::is_whitespace);
    let (negative, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let n: i64 = t[..digits].parse().ok()?;
    i32::try_from(if negative { -n } else { n }).ok()
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// An SVG element's presentation attributes (Blink's `SVGElement::CollectStyleForPresentationAttribute`, Chromium
/// 150): an attribute in no namespace whose name is one of the CSS properties `CssPropertyIdForSVGAttributeName`
/// lists is that property's declaration, parsed in SVG attribute mode -- a unitless length is px -- and dropped
/// whole when invalid; `xml:space` on a text-content element is `white-space: pre` or `nowrap`'s longhands.
/// Measured on Chrome 150.0.7871.46: `<text font-size="20" text-anchor="middle" fill="red">` computes `20px`,
/// `middle` and `rgb(255, 0, 0)`, an author rule or the `style` attribute beats each, `<g font-size="30">` is
/// inherited, `fill="red; display:none"` sets nothing, and an HTML element's `fill` attribute is no hint. This
/// engine mapped none of them: an SVG `<text>`'s font was the inherited one whatever its attributes said.
fn for_svg_element(dom: &Dom, node: NodeId) -> String {
    let mut out = String::new();
    for (name, value) in dom.attribute_pairs(node) {
        if name == "xml:space" {
            let tag = dom.tag_name(node).unwrap_or("");
            if matches!(tag, "text" | "tspan" | "textPath") {
                push(&mut out, "white-space-collapse", if value == "preserve" { "preserve" } else { "collapse" });
                push(&mut out, "text-wrap-mode", "nowrap");
            }
            continue;
        }
        let tag = dom.tag_name(node).unwrap_or("");
        if transform_attribute(tag) == Some(name) {
            if let Some(matrix) = svg_transform_matrix(value) {
                push(&mut out, "transform", &matrix);
            }
            continue;
        }
        if is_svg_geometry_attribute(tag, name) {
            if let Some(value) = svg_geometry_value(name, value) {
                push(&mut out, name, &value);
            }
            continue;
        }
        if !is_svg_presentation_attribute(name) || !self_contained(value) {
            continue;
        }
        let value = if takes_svg_length(name) { unitless_lengths_as_px(value) } else { value.trim().to_string() };
        // a value the property does not take is no declaration (Chrome 150: `stroke="nonsense"` reads `none`)
        if crate::style::supports::supports_declaration(name, &value) {
            push(&mut out, name, &value);
        }
    }
    if sized_by_attributes(dom.tag_name(node).unwrap_or("")) {
        for side in ["width", "height"] {
            if dom.get_attribute(node, side).is_none() {
                push(&mut out, side, "0px");
            }
        }
    }
    out
}

/// The attribute an SVG element's `transform` property is mapped from (Chrome 150, measured, `svgtf.html`):
/// `transform` on every graphics element -- an `<svg>`, `<text>` and `<use>` included -- `gradientTransform` on a
/// gradient, `patternTransform` on a pattern. `getComputedStyle(g).transform` read `none` here where Chrome reads
/// the attribute's matrix.
pub fn transform_attribute(tag: &str) -> Option<&'static str> {
    match tag {
        "g" | "svg" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path" | "text" | "tspan"
        | "textPath" | "image" | "use" | "foreignObject" | "a" | "switch" | "clipPath" => Some("transform"),
        "linearGradient" | "radialGradient" => Some("gradientTransform"),
        "pattern" => Some("patternTransform"),
        _ => None,
    }
}

/// An SVG transform list as the one `matrix()` it composes to, as `transform` computes it: `translate(50 50)
/// rotate(45)` is `matrix(0.707107, 0.707107, -0.707107, 0.707107, 50, 50)`. An empty list or one that does not
/// parse is no declaration (`none`).
fn svg_transform_matrix(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        return None;
    }
    let t: svgtypes::Transform = value.parse().ok()?;
    let n = crate::style::serialize::format_number;
    Some(format!("matrix({}, {}, {}, {}, {}, {})", n(t.a), n(t.b), n(t.c), n(t.d), n(t.e), n(t.f)))
}

/// The geometry attributes Blink maps to their CSS properties, each on the elements that have it (Chrome 150,
/// measured, `svggeo.html`, `svggeo2.html`): `x`/`y` on `<svg>`, `<rect>`, `<image>`, `<foreignObject>`, `<use>` and
/// `<mask>`; `width`/`height` on `<rect>`, `<image>`, `<foreignObject>` and `<mask>` (not `<svg>` or `<use>`);
/// `cx`/`cy`/`r` on a `<circle>`, `cx`/`cy`/`rx`/`ry` on an `<ellipse>`, `rx`/`ry` on a `<rect>`; `d` on a `<path>`.
/// This engine mapped none: `getComputedStyle(rect).width` read `auto` where Chrome reads the attribute's `10px`.
pub fn is_svg_geometry_attribute(tag: &str, name: &str) -> bool {
    match name {
        "x" | "y" => matches!(tag, "svg" | "rect" | "image" | "foreignObject" | "use" | "mask"),
        "width" | "height" => sized_by_attributes(tag) || tag == "mask",
        "cx" | "cy" => matches!(tag, "circle" | "ellipse"),
        "r" => tag == "circle",
        "rx" | "ry" => matches!(tag, "rect" | "ellipse"),
        "d" => tag == "path",
        _ => false,
    }
}

/// The elements whose `width` and `height` are always declared: the attribute's length, or `0px` -- its
/// animated value's default -- when it is missing, not a length, or negative (Chrome 150: a bare `<rect>`
/// computes `0px` by `0px`, not `auto`).
fn sized_by_attributes(tag: &str) -> bool {
    matches!(tag, "rect" | "image" | "foreignObject")
}

/// A geometry attribute's declared value: an SVG length (a bare number is px, a unit in any case, `calc()`), or a
/// `<path>`'s data as `path("…")`. Not a length, `width`, `height`, `rx` and `ry` declare `0px` (`rx="auto"` too)
/// and the rest nothing; a negative `width` or `height` is `0px`, a negative `r` or `rx` is kept.
fn svg_geometry_value(name: &str, value: &str) -> Option<String> {
    if name == "d" {
        return svg_path_data(value);
    }
    let zero = matches!(name, "width" | "height" | "rx" | "ry").then(|| "0px".to_string());
    match svg_length(value) {
        Some((_, negative)) if negative && matches!(name, "width" | "height") => zero,
        Some((length, _)) => Some(length),
        None => zero,
    }
}

/// An SVG length attribute's value as a CSS length, and whether it is negative: a number (px), a dimension or a
/// percentage, its unit lowercased, or a `calc()` -- with whitespace around it and nothing else.
fn svg_length(value: &str) -> Option<(String, bool)> {
    use cssparser::{Parser, ParserInput, Token};
    let text = value.trim();
    let mut input = ParserInput::new(text);
    let mut p = Parser::new(&mut input);
    let n = crate::style::serialize::format_number;
    let (length, negative) = match p.next().ok()?.clone() {
        Token::Number { value, .. } => (format!("{}px", n(value as f64)), value < 0.0),
        Token::Dimension { value, unit, .. } => (format!("{}{}", n(value as f64), unit.to_ascii_lowercase()), value < 0.0),
        Token::Percentage { unit_value, .. } => (format!("{}%", n(unit_value as f64 * 100.0)), unit_value < 0.0),
        Token::Function(f) if f.eq_ignore_ascii_case("calc") => {
            p.parse_nested_block(|p| -> Result<(), cssparser::ParseError<'_, ()>> {
                while p.next().is_ok() {}
                Ok(())
            })
            .ok()?;
            (text.to_string(), false)
        }
        _ => return None,
    };
    p.expect_exhausted().ok()?;
    crate::style::supports::supports_declaration("x", &length).then_some((length, negative))
}

/// Path data as the `d` property computes it (Blink's `SVGPathStringBuilder`): each segment its command letter,
/// relative ones lowercase, then its numbers, all space-separated -- `M0 0L10 10` is `path("M 0 0 L 10 10")`, an
/// implicit line after a move written as the `L` (or `l`) it is. Data in error keeps the segments before the error;
/// none at all is no declaration.
fn svg_path_data(data: &str) -> Option<String> {
    use svgtypes::PathSegment as P;
    let mut parts: Vec<String> = Vec::new();
    for segment in svgtypes::PathParser::from(data) {
        let Ok(segment) = segment else { break };
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let (letter, abs, numbers): (char, bool, Vec<f64>) = match segment {
            P::MoveTo { abs, x, y } => ('M', abs, vec![x, y]),
            P::LineTo { abs, x, y } => ('L', abs, vec![x, y]),
            P::HorizontalLineTo { abs, x } => ('H', abs, vec![x]),
            P::VerticalLineTo { abs, y } => ('V', abs, vec![y]),
            P::CurveTo { abs, x1, y1, x2, y2, x, y } => ('C', abs, vec![x1, y1, x2, y2, x, y]),
            P::SmoothCurveTo { abs, x2, y2, x, y } => ('S', abs, vec![x2, y2, x, y]),
            P::Quadratic { abs, x1, y1, x, y } => ('Q', abs, vec![x1, y1, x, y]),
            P::SmoothQuadratic { abs, x, y } => ('T', abs, vec![x, y]),
            P::EllipticalArc { abs, rx, ry, x_axis_rotation, large_arc, sweep, x, y } => {
                ('A', abs, vec![rx, ry, x_axis_rotation, flag(large_arc), flag(sweep), x, y])
            }
            // one close-path segment in Blink's byte stream, written `Z` however it was written
            P::ClosePath { .. } => ('Z', true, Vec::new()),
        };
        let mut part = String::from(if abs { letter } else { letter.to_ascii_lowercase() });
        for n in numbers {
            part.push(' ');
            part.push_str(&crate::style::serialize::format_number(n));
        }
        parts.push(part);
    }
    (!parts.is_empty()).then(|| format!("path(\"{}\")", parts.join(" ")))
}

/// `CssPropertyIdForSVGAttributeName`'s list.
fn is_svg_presentation_attribute(name: &str) -> bool {
    matches!(
        name,
        "alignment-baseline" | "baseline-shift" | "buffered-rendering" | "clip" | "clip-path" | "clip-rule" | "color"
            | "color-interpolation" | "color-interpolation-filters" | "color-rendering" | "cursor" | "direction"
            | "display" | "dominant-baseline" | "fill" | "fill-opacity" | "fill-rule" | "filter" | "flood-color"
            | "flood-opacity" | "font-family" | "font-size" | "font-stretch" | "font-style" | "font-variant"
            | "font-weight" | "image-rendering" | "letter-spacing" | "lighting-color" | "marker-end" | "marker-mid"
            | "marker-start" | "mask" | "mask-type" | "opacity" | "overflow" | "paint-order" | "pointer-events"
            | "shape-rendering" | "stop-color" | "stop-opacity" | "stroke" | "stroke-dasharray" | "stroke-dashoffset"
            | "stroke-linecap" | "stroke-linejoin" | "stroke-miterlimit" | "stroke-opacity" | "stroke-width"
            | "text-anchor" | "text-decoration" | "text-rendering" | "transform-origin" | "unicode-bidi"
            | "vector-effect" | "visibility" | "word-spacing" | "writing-mode"
    )
}

/// The listed properties whose values hold lengths, which SVG attribute mode accepts without a unit
/// (`IsUnitlessLengthParsingEnabledForMode`).
fn takes_svg_length(name: &str) -> bool {
    matches!(
        name,
        "baseline-shift" | "font-size" | "letter-spacing" | "word-spacing" | "stroke-width" | "stroke-dashoffset" | "stroke-dasharray" | "transform-origin"
    )
}

/// Each bare number in `value` written as px: `5 10` is `5px 10px`.
fn unitless_lengths_as_px(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if !token.is_empty() {
            let bare = token.parse::<f64>().is_ok_and(|n| n.is_finite()) && !token.ends_with('.');
            out.push_str(token);
            if bare {
                out.push_str("px");
            }
            token.clear();
        }
    };
    for c in value.trim().chars() {
        if c.is_ascii_whitespace() || c == ',' {
            flush(&mut token, &mut out);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// Whether `value` is one declaration's value and cannot reach past it: no `;`, braces or `!`, its quotes and
/// brackets closed. An attribute value is parsed alone in Blink, so `fill="red; display:none"` is one invalid
/// value, never two declarations.
fn self_contained(value: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (_, '\\') => {
                chars.next();
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\n') => return false,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, ';' | '{' | '}' | '!') => return false,
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    quote.is_none() && depth == 0
}

/// HTML's rules for parsing a legacy font size (`<font size>`), as Blink's `ParseFontSize`: leading HTML
/// space, an optional sign, ASCII digits; `+n` is 3 + n and `-n` 3 - n; clamped to 1..7 -- and the keyword
/// `CssValueFromFontSizeNumber` makes of it (1 is `x-small` there, not the spec's `xx-small`).
fn legacy_font_size(value: &str) -> Option<&'static str> {
    let v = value.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
    let (sign, rest) = match v.as_bytes().first()? {
        b'+' => (1i64, &v[1..]),
        b'-' => (-1i64, &v[1..]),
        _ => (0i64, v),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().unwrap_or(i64::MAX);
    let size = match sign {
        1 => n.saturating_add(3),
        -1 => 3i64.saturating_sub(n),
        _ => n,
    }
    .clamp(1, 7);
    Some(["x-small", "small", "medium", "large", "x-large", "xx-large", "xxx-large"][(size - 1) as usize])
}

fn push(out: &mut String, property: &str, value: &str) {
    out.push_str(property);
    out.push(':');
    out.push_str(value);
    out.push(';');
}

/// `HTMLTablePartElement`'s: `bgcolor`, `background`, `valign`, `align`, `height`.
fn table_part(dom: &Dom, node: NodeId, out: &mut String) {
    let attr = |name: &str| dom.get_attribute(node, name);
    if let Some(c) = attr("bgcolor").and_then(legacy_color) {
        push(out, "background-color", &c);
    }
    if let Some(v) = attr("background").filter(|v| !v.trim().is_empty()) {
        push(out, "background-image", &format!("url(\"{}\")", v.trim().replace('"', "%22")));
    }
    if let Some(v) = attr("valign") {
        let lower = v.trim().to_ascii_lowercase();
        let value = match lower.as_str() {
            "top" | "middle" | "bottom" | "baseline" => lower.clone(),
            _ => v.trim().to_string(),
        };
        if !value.is_empty() {
            push(out, "vertical-align", &value);
        }
    }
    if let Some(v) = attr("align") {
        let lower = v.trim().to_ascii_lowercase();
        let value = match lower.as_str() {
            "middle" | "center" => "-webkit-center".to_string(),
            "absmiddle" => "center".to_string(),
            "left" => "-webkit-left".to_string(),
            "right" => "-webkit-right".to_string(),
            _ => lower,
        };
        if !value.is_empty() {
            push(out, "text-align", &value);
        }
    }
    if matches!(dom.tag_name(node).map(|t| t.to_ascii_lowercase()).as_deref(), Some("tr" | "thead" | "tbody" | "tfoot" | "col" | "colgroup")) {
        if let Some(v) = attr("height").filter(|v| !v.is_empty()).and_then(|v| html_length(v, true, true)) {
            push(out, "height", &v);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rules {
    Unset,
    None,
    Groups,
    Rows,
    Cols,
    All,
}

fn rules_of(dom: &Dom, table: NodeId) -> Rules {
    match dom.get_attribute(table, "rules").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("none") => Rules::None,
        Some("groups") => Rules::Groups,
        Some("rows") => Rules::Rows,
        Some("cols") => Rules::Cols,
        Some("all") => Rules::All,
        _ => Rules::Unset,
    }
}

/// The table a cell belongs to (Blink's `FindParentTable`): through its row and row group.
fn parent_table(dom: &Dom, cell: NodeId) -> Option<NodeId> {
    let mut cur = dom.parent(cell);
    for _ in 0..3 {
        let p = cur?;
        if !dom.is_html_ns(p) {
            return None;
        }
        match dom.tag_name(p).map(|t| t.to_ascii_lowercase()).as_deref() {
            Some("table") => return Some(p),
            Some("tr" | "tbody" | "thead" | "tfoot") => cur = dom.parent(p),
            _ => return None,
        }
    }
    None
}

/// The sides a `<table frame>` value draws, top, right, bottom, left (`GetBordersFromFrameAttributeValue`); `None` for
/// a value that is not one, which is as if there were no `frame` (Chrome 152: `frame=bogus` and `frame=""` draw
/// nothing, `frame=BOX` all four).
fn frame_sides(value: &str) -> Option<[bool; 4]> {
    Some(match value.to_ascii_lowercase().as_str() {
        "void" => [false, false, false, false],
        "above" => [true, false, false, false],
        "below" => [false, false, true, false],
        "hsides" => [true, false, true, false],
        "lhs" => [false, false, false, true],
        "rhs" => [false, true, false, false],
        "vsides" => [false, true, false, true],
        "box" | "border" => [true, true, true, true],
        _ => return None,
    })
}

/// The table's shared cell style (`HTMLTableElement::CreateSharedCellStyle`): the cells' borders its
/// `border`/`rules`/`bordercolor` imply, and `padding: <cellpadding>px` (1 unless it says otherwise).
fn cell_style_of(dom: &Dom, table: NodeId, out: &mut String) {
    let border = dom.get_attribute(table, "border").map(border_width).unwrap_or(0);
    let border_color = dom.get_attribute(table, "bordercolor").is_some_and(|v| !v.is_empty());
    match rules_of(dom, table) {
        Rules::None | Rules::Groups => {}
        Rules::All => {
            push(out, "border-width", "thin");
            push(out, "border-style", "solid");
            push(out, "border-color", "inherit");
        }
        Rules::Cols => {
            push(out, "border-left-width", "thin");
            push(out, "border-right-width", "thin");
            push(out, "border-left-style", "solid");
            push(out, "border-right-style", "solid");
            push(out, "border-color", "inherit");
        }
        Rules::Rows => {
            push(out, "border-top-width", "thin");
            push(out, "border-bottom-width", "thin");
            push(out, "border-top-style", "solid");
            push(out, "border-bottom-style", "solid");
            push(out, "border-color", "inherit");
        }
        Rules::Unset if border > 0 => {
            push(out, "border-width", "1px");
            push(out, "border-style", if border_color { "solid" } else { "inset" });
            push(out, "border-color", "inherit");
        }
        Rules::Unset => {}
    }
    let padding = match dom.get_attribute(table, "cellpadding") {
        Some(v) if !v.is_empty() => loose_int(v).clamp(0, u16::MAX as i64),
        _ => 1,
    };
    if padding > 0 {
        push(out, "padding", &format!("{padding}px"));
    }
}

/// `ParseBorderWidthAttribute` on a `<table>`: a non-negative integer, 1 when empty or unparsable.
fn border_width(value: &str) -> u32 {
    non_negative_integer(value).unwrap_or(1)
}

/// HTML's rules for parsing non-negative integers.
fn non_negative_integer(value: &str) -> Option<u32> {
    let t = value.trim_start_matches([' ', '\t', '\n', '\u{c}', '\r']);
    let t = t.strip_prefix('+').unwrap_or(t);
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok().map(|n| n.min(u32::MAX as u64) as u32)
}

/// `StringToIntLoose`: leading whitespace, a sign, digits; 0 when there are none.
fn loose_int(value: &str) -> i64 {
    let t = value.trim_start();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n = digits.parse::<i64>().unwrap_or(0);
    if neg { -n } else { n }
}

/// A dimension attribute's value, as Blink's `ParseDimensionValue` reads it (`html_dimension.cc`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Dimension {
    Absolute(f64),
    Percentage(f64),
    /// `<number>*`: parsed, and never mapped.
    Relative,
}

/// HTML's rules for parsing dimension values with Blink's deviations: leading HTML space, ASCII digits (one at
/// least -- no sign, no leading `.`), an optional `.` and more digits, then `%` makes it a percentage and `*` a
/// relative length; anything after is ignored. Chrome 150: `7.5.5` is 7.5px, `1e2` 1px, `5.%` 5%, `+5`, `-5`
/// and `.5` nothing, `10*` nothing.
fn dimension(value: &str) -> Option<Dimension> {
    let t = value.trim_start_matches([' ', '\t', '\n', '\u{c}', '\r']);
    let b = t.as_bytes();
    let mut end = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if end == 0 {
        return None;
    }
    if b.get(end) == Some(&b'.') {
        end += 1 + b[end + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
    }
    let n: f64 = t[..end].trim_end_matches('.').parse().ok()?;
    // `CSSValueClampingUtils::ClampDouble`
    let n = n.min(f64::MAX);
    Some(match b.get(end) {
        Some(b'%') => Dimension::Percentage(n),
        Some(b'*') => Dimension::Relative,
        _ => Dimension::Absolute(n),
    })
}

/// `AddHTMLLengthToStyle`: the dimension as a `px` length, or a percentage when `percent` allows one; `None`
/// for nothing usable, a relative length, a percentage `percent` refuses (`<table cellspacing="10%">` keeps
/// its 2px) or a 0 `zero` refuses.
fn html_length(value: &str, percent: bool, zero: bool) -> Option<String> {
    match dimension(value)? {
        Dimension::Relative => None,
        Dimension::Percentage(_) if !percent => None,
        Dimension::Absolute(n) | Dimension::Percentage(n) if n == 0.0 && !zero => None,
        Dimension::Percentage(n) => Some(format!("{}%", crate::value::serialize_number_f64(n))),
        Dimension::Absolute(n) => Some(format!("{n}px")),
    }
}

/// HTML's rules for parsing a legacy colour value: a named colour, `#rgb`, or any string turned into
/// hex digits and split in three (`bgcolor="ffcc00"`, `bgcolor="chucknorris"`). `None` for an empty
/// value or `transparent`.
fn legacy_color(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if crate::value::Color::parse_str(v).is_ok() && !v.starts_with('#') {
        return Some(v.to_string());
    }
    let chars: Vec<char> = v.chars().collect();
    if chars.len() == 4 && chars[0] == '#' && chars[1..].iter().all(|c| c.is_ascii_hexdigit()) {
        return Some(v.to_string());
    }
    let mut s: String = chars.iter().map(|&c| if (c as u32) > 0xFFFF { "00".to_string() } else { c.to_string() }).collect();
    s = s.chars().take(128).collect();
    if let Some(r) = s.strip_prefix('#') {
        s = r.to_string();
    }
    let mut s: Vec<char> = s.chars().map(|c| if c.is_ascii_hexdigit() { c } else { '0' }).collect();
    while s.is_empty() || s.len() % 3 != 0 {
        s.push('0');
    }
    let len = s.len() / 3;
    let mut parts: Vec<Vec<char>> = (0..3).map(|i| s[i * len..(i + 1) * len].to_vec()).collect();
    if len > 8 {
        for p in parts.iter_mut() {
            *p = p[p.len() - 8..].to_vec();
        }
    }
    while parts[0].len() > 2 && parts.iter().all(|p| p[0] == '0') {
        for p in parts.iter_mut() {
            p.remove(0);
        }
    }
    let hex: String = parts.iter().map(|p| p.iter().take(2).collect::<String>()).map(|p| if p.len() == 1 { format!("0{p}") } else { p }).collect();
    Some(format!("#{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(html: &str) -> String {
        let dom = Dom::parse(html);
        for_element(&dom, dom.get_element_by_id("t").expect("the element #t"))
    }

    // Blink's `ParseDimensionValue` and `AddHTMLLengthToStyle` flags (Chrome 150, `fixtures/presentational-hints`).
    #[test]
    fn dimensions_parse_as_blink_parses_them() {
        assert_eq!(html_length("7.5.5", true, true).as_deref(), Some("7.5px"));
        assert_eq!(html_length("1e2", true, true).as_deref(), Some("1px"));
        assert_eq!(html_length("40.", true, true).as_deref(), Some("40px"));
        assert_eq!(html_length("5.%", true, true).as_deref(), Some("5%"));
        assert_eq!(html_length("100.75%", true, true).as_deref(), Some("100.75%"));
        assert_eq!(html_length(" 30px", true, true).as_deref(), Some("30px"));
        for nothing in ["+5", "-5", ".5", "abc", "", "10*"] {
            assert_eq!(html_length(nothing, true, true), None, "{nothing:?}");
        }
        assert_eq!(html_length("10%", false, true), None, "a percentage the element refuses is no hint");
        assert_eq!(html_length("0", true, false), None);
        assert_eq!(html_length("0%", true, true).as_deref(), Some("0%"));
    }

    #[test]
    fn frameborder_reads_as_string_to_int_loose() {
        assert_eq!(loose_i32("0x"), Some(0));
        assert_eq!(loose_i32(" +0"), Some(0));
        assert_eq!(loose_i32("1.5"), Some(1));
        assert_eq!(loose_i32("-1"), Some(-1));
        assert_eq!(loose_i32("yes"), None);
        assert_eq!(loose_i32("99999999999"), None);
    }

    #[test]
    fn the_embedded_elements_map_their_attributes_in_attribute_order() {
        assert_eq!(
            hints(r#"<iframe id=t width="100%" height="100%" frameborder="0" border="0"></iframe>"#),
            "width:100%;height:100%;border-top-width:0px;border-bottom-width:0px;border-left-width:0px;border-right-width:0px;"
        );
        assert_eq!(hints(r#"<img id=t width=40 height=30>"#), "width:40px;aspect-ratio:auto 40 / 30;height:30px;aspect-ratio:auto 40 / 30;");
        assert_eq!(hints(r#"<img id=t width=40 height=50%>"#), "width:40px;height:50%;");
        assert_eq!(hints(r#"<canvas id=t width=40.5 height=30*></canvas>"#), "aspect-ratio:auto 40 / 30;aspect-ratio:auto 40 / 30;");
        assert_eq!(hints(r#"<img id=t align=left valign=" BOTTOM">"#), "float:left;vertical-align:top;vertical-align:bottom;");
        assert_eq!(hints(r#"<img id=t valign="top;width:9px">"#), "");
        assert_eq!(hints(r#"<video id=t align=middle></video>"#), "text-align:center;");
        assert_eq!(hints(r#"<video id=t align=top></video>"#), "");
        assert_eq!(hints(r#"<input id=t width=40 hspace=3 border=2>"#), "margin-left:3px;margin-right:3px;");
        assert_eq!(hints(r#"<embed id=t width=100 hidden>"#), "width:100px;width:0px;height:0px;");
        assert_eq!(
            hints(r#"<object id=t border=abc></object>"#),
            "border-top-width:0px;border-bottom-width:0px;border-left-width:0px;border-right-width:0px;\
             border-top-style:solid;border-bottom-style:solid;border-left-style:solid;border-right-style:solid;"
        );
        assert_eq!(hints(r#"<table><colgroup><col id=t width=0></colgroup></table>"#), "width:0px;");
        assert_eq!(hints(r#"<table id=t cellspacing="10%"></table>"#), "");
    }

    // A table's border style (Chrome 152, `tbl.html` / `tblf.html`, 2026-10-23).
    #[test]
    fn a_tables_border_style_comes_from_its_border_frame_and_rules() {
        assert_eq!(hints(r#"<table id=t bordercolor=red></table>"#), "border-color:red;");
        assert_eq!(hints(r#"<table id=t border=0 bordercolor=red></table>"#), "border-width:0px;border-color:red;");
        assert_eq!(hints(r#"<table id=t border=1 bordercolor=red></table>"#), "border-width:1px;border-color:red;border-style:solid;");
        assert_eq!(hints(r#"<table id=t border></table>"#), "border-width:1px;border-style:outset;");
        assert_eq!(hints(r#"<table id=t bordercolor=red rules=none></table>"#), "border-color:red;border-collapse:collapse;border-style:hidden;");
        assert_eq!(
            hints(r#"<table id=t frame=Above></table>"#),
            "border-width:thin;border-top-style:solid;border-right-style:hidden;border-bottom-style:hidden;border-left-style:hidden;"
        );
        assert_eq!(
            hints(r#"<table id=t frame=void border=3 rules=all></table>"#),
            "border-width:3px;border-collapse:collapse;border-top-style:hidden;border-right-style:hidden;border-bottom-style:hidden;border-left-style:hidden;"
        );
        assert_eq!(hints(r#"<table id=t frame=bogus></table>"#), "");
        assert_eq!(
            hints(r#"<table id=t border=3 frame=box></table>"#),
            "border-width:3px;border-width:thin;border-top-style:solid;border-right-style:solid;border-bottom-style:solid;border-left-style:solid;"
        );
        assert!(hints(r#"<table id=t frame=box border=3></table>"#).starts_with("border-width:3px;border-top-style:solid;"));
    }

    // Chrome 150 (`svggeo.html`).
    #[test]
    fn svg_geometry_attributes_are_their_properties_on_their_elements() {
        assert_eq!(hints("<svg><rect id=t x=1 y=2 width=10 height=11 rx=3 ry=4 /></svg>"), "x:1px;y:2px;width:10px;height:11px;rx:3px;ry:4px;");
        assert_eq!(hints("<svg><rect id=t /></svg>"), "width:0px;height:0px;", "a bare rect is 0 by 0");
        assert_eq!(hints("<svg><circle id=t cx=5 cy=6 r=7 /></svg>"), "cx:5px;cy:6px;r:7px;");
        assert_eq!(hints("<svg><ellipse id=t cx=5 r=7 rx=8 /></svg>"), "cx:5px;rx:8px;", "an ellipse has no r");
        assert_eq!(hints("<svg><svg id=t x=3 width=5 /></svg>"), "x:3px;", "an <svg>'s width is no hint");
        assert_eq!(hints("<svg><use id=t y=2 height=4 /></svg>"), "y:2px;");
        assert_eq!(hints("<svg><rect id=t width=bogus x='1px 2px' /></svg>"), "width:0px;height:0px;");
        assert_eq!(hints("<svg><rect id=t height=-5 rx=auto /></svg>"), "height:0px;rx:0px;width:0px;");
        assert_eq!(hints("<svg><circle id=t r=-2 /></svg>"), "r:-2px;");
        assert_eq!(hints("<svg><rect id=t width='5Px ' height='calc(1px + 2px)' /></svg>"), "width:5px;height:calc(1px + 2px);");
        assert_eq!(hints("<svg><image id=t width=5e /></svg>"), "width:0px;height:0px;");
        assert_eq!(hints("<svg><path id=t d='M0 0L10 10 20 20h5z' /></svg>"), "d:path(\"M 0 0 L 10 10 L 20 20 h 5 Z\");");
        assert_eq!(hints("<svg><path id=t d='m1,2a3 4 5 1 0 6 7' /></svg>"), "d:path(\"m 1 2 a 3 4 5 1 0 6 7\");");
        assert_eq!(hints("<svg><path id=t d='' /></svg>"), "");
        assert_eq!(hints("<div id=t width=5 x=1></div>"), "", "an HTML element's are not");
        assert_eq!(hints("<svg><g id=t transform='translate(50 50) rotate(45)'/></svg>"), "transform:matrix(0.707107, 0.707107, -0.707107, 0.707107, 50, 50);");
        assert_eq!(hints("<svg><g id=t transform='matrix(1 2 3 4 5 6) skewX(10)'/></svg>"), "transform:matrix(1, 2, 3.17633, 4.35265, 5, 6);");
        assert_eq!(hints("<svg><linearGradient id=t gradientTransform='rotate(10)' transform='scale(2)'/></svg>"), "transform:matrix(0.984808, 0.173648, -0.173648, 0.984808, 0, 0);");
        assert_eq!(hints("<svg><g id=t transform=''/></svg>"), "");
    }

}
