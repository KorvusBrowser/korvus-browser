//! Specified value in, computed value out.
//!
//! # Why this is not optional
//!
//! `getComputedStyle` does not hand back what the page wrote. `color: red` comes back as
//! `rgb(255, 0, 0)`, `margin: 2em` as a number of pixels, `font-weight: bold` as `700`. All three
//! are readable in one line, so echoing the specified text is not a shortcut, it is a tell — and
//! for the lengths it is also the difference between having geometry and not having it.
//!
//! # The order matters
//!
//! Three properties have to be resolved before the rest, because the rest are written in terms of
//! them:
//!
//!  1. **`font-size`**, because `em` means it. It is itself resolved against the *parent's* font
//!     size, which is why it cannot be done in the same pass as everything else.
//!  2. **`color`**, because `currentColor` means it — and it is what `border-color` initially is.
//!  3. everything else, against a context those two complete.
//!
//! # What is deliberately left alone
//!
//! A percentage stays a percentage. `width: 50%` computes to `50%` in every browser, because the
//! containing block it resolves against is layout's answer, not the cascade's. Same for `auto`.

use crate::style::property::{Computed, PropertyId};
use crate::value::{serialize_number, serialize_px, Color, Length, LengthPercentage, Rgba, ResolveContext};
use cssparser::{Parser, ParserInput};

/// What the cascade knows about an element by the time the rest of its properties resolve.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// This element's own SPECIFIED `font-size`, in pixels — what `em` means here: Blink resolves a
    /// font-relative length against `SpecifiedFontSize`, the size before the minimum font size (Chrome 150,
    /// Korean UI: `font-size: 0.5em; width: 10em` computes `10px` and `80px`). See [`FontSizeState`].
    pub font_size: f32,
    /// This element's COMPUTED `font-size` -- after the minimum logical font size -- which is what
    /// `font-size` reads back as and what a `line-height` percentage is of (Chrome 150: `font-size: 0.5em;
    /// line-height: 150%` is `15px` under the Korean UI's 10px minimum).
    pub computed_font_size: f32,
    /// The root element's, for `rem`.
    pub root_font_size: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// This element's computed `color` — what `currentColor` means here.
    pub color: Rgba,
    /// The *parent's* computed `font-weight`, which is what `bolder` and `lighter` step from. The
    /// odd one out: every other field describes this element.
    pub parent_font_weight: f32,
    /// What `cqw` and `cqh` mean here; `None` is the viewport.
    pub container: Option<(f32, f32)>,
    /// This element's writing mode is vertical: an unnamed `text-emphasis-style` shape is `sesame`, not `dot`.
    pub vertical: bool,
}

impl Default for Context {
    fn default() -> Self {
        Context {
            font_size: 16.0,
            computed_font_size: 16.0,
            root_font_size: 16.0,
            viewport_width: 1024.0,
            viewport_height: 768.0,
            color: Rgba::BLACK,
            parent_font_weight: 400.0,
            container: None,
            vertical: false,
        }
    }
}

impl Context {
    fn lengths(&self) -> ResolveContext {
        let mut ctx = ResolveContext::new(self.font_size, self.root_font_size, self.viewport_width, self.viewport_height);
        if let Some(c) = self.container {
            ctx.container = c;
        }
        ctx
    }
}

/// The computed value of `specified` for `property`.
///
/// Returns the specified text unchanged when there is nothing to resolve or when resolving fails.
/// Failing quietly is right here: the value already survived the parser's validation, and a value
/// this step cannot reduce (a `calc()`, a function it has never seen) is better handed back as
/// written than replaced with a wrong number.
pub fn computed_value(property: PropertyId, specified: &str, ctx: &Context) -> String {
    // A list's `initial` ITEM -- what `background` gives a layer that leaves the longhand out (Blink's implicit
    // `CSSInitialValue`) -- computes to the longhand's initial value, item by item: `no-repeat, initial` is
    // `no-repeat, repeat`.
    if specified.contains(',') && specified.to_ascii_lowercase().contains("initial") {
        let items = top_level_items(specified);
        if items.len() > 1 && items.iter().any(|v| v.trim().eq_ignore_ascii_case("initial")) {
            let replaced: Vec<&str> = items.iter().map(|v| if v.trim().eq_ignore_ascii_case("initial") { property.initial() } else { v.trim() }).collect();
            return computed_value(property, &replaced.join(", "), ctx);
        }
    }
    // A math function that simplified to ONE value when it was parsed computes to that value
    // (`style::calc`), clamped to the property's range as `CSSMathFunctionValue::ClampToPermittedRange` clamps it: an
    // `<integer>` rounds half towards positive infinity, a duration is never negative. Chrome 150
    // (`fixtures/css-answers/calc-probe.js`): `opacity: calc(1 / 3)` computes `0.333333`, `z-index: calc(1.5)` `2`,
    // `calc(-1.5)` `-1`, `transition-duration: calc(-1s)` `0s`, `transform: rotate(atan2(1, 1))` the 45deg matrix.
    // What did not simplify -- a percentage, a relative length -- stays for the property's own step (`lengthy`).
    // ...except a math function of NUMBERS where an SVG length takes a bare number: it computes to a `calc()` of
    // pixels (Chrome 152: `x: calc(2 * 3)` and `stroke-width: calc(2 * 3)` read `calc(6px)`, where `x: 6` reads `6px`).
    if length_bearing(property) == Some(Numbers::Px) || property == PropertyId::StrokeDasharray {
        if let Some((v, _)) = super::calc::single(specified)
            .filter(|e| e.category() == super::calc::Cat::Number)
            .and_then(|e| e.as_literal())
            .filter(|(v, _)| v.is_finite())
        {
            return format!("calc({})", serialize_px(v as f32));
        }
    }
    if specified.contains('(') && super::calc::mentions_math(specified) {
        let integer = matches!(
            property,
            PropertyId::ZIndex | PropertyId::Order | PropertyId::Orphans | PropertyId::Widows | PropertyId::ColumnCount | PropertyId::WebkitLineClamp
        );
        let non_negative = matches!(property, PropertyId::TransitionDuration | PropertyId::AnimationDuration);
        let replaced = super::calc::substitute_literals(specified, integer, non_negative);
        if replaced != specified {
            return computed_value(property, &replaced, ctx);
        }
    }
    // Values Blink computes to another spelling than the one written (each measured on Chrome
    // 150 -- `fixtures/computed-style/conversions`): `condensed` is `75%`, `reverse` `auto 180deg`, `filled` `dot`,
    // `row dense` `dense`, a gradient's `red` `rgb(255, 0, 0)`; and on Chrome 152 `squircle` `superellipse(2)`.
    let converted = match property {
        PropertyId::FontStretch => font_stretch(specified),
        PropertyId::OffsetRotate => offset_rotate(specified),
        PropertyId::TextEmphasisStyle => text_emphasis_style(specified, ctx.vertical),
        PropertyId::OffsetPath => offset_path(specified),
        PropertyId::WebkitTextStrokeWidth => text_stroke_width(specified, ctx),
        PropertyId::CornerTopLeftShape
        | PropertyId::CornerTopRightShape
        | PropertyId::CornerBottomRightShape
        | PropertyId::CornerBottomLeftShape
        | PropertyId::CornerStartStartShape
        | PropertyId::CornerStartEndShape
        | PropertyId::CornerEndStartShape
        | PropertyId::CornerEndEndShape => corner_shape(specified),
        PropertyId::GridAutoFlow => grid_auto_flow(specified),
        PropertyId::PositionTryFallbacks => position_try_fallbacks(specified),
        PropertyId::OffsetPosition | PropertyId::OffsetAnchor => position_percentages(specified).map(|v| lengthy(&v, ctx)),
        PropertyId::BackgroundImage
        | PropertyId::MaskImage
        | PropertyId::BorderImageSource
        | PropertyId::ListStyleImage
        | PropertyId::ShapeOutside
        | PropertyId::WebkitMaskBoxImageSource => computed_images(specified, ctx),
        PropertyId::BoxShadow => shadow_list(specified, ctx, true),
        PropertyId::TextShadow => shadow_list(specified, ctx, false),
        PropertyId::Filter | PropertyId::BackdropFilter => filter_list(specified, ctx),
        PropertyId::AccentColor => accent_color(specified, ctx),
        PropertyId::TransitionTimingFunction | PropertyId::AnimationTimingFunction => easing_list(specified),
        PropertyId::MinWidth | PropertyId::MaxWidth | PropertyId::MinHeight | PropertyId::MaxHeight => sizing_alias(specified),
        PropertyId::AlignContent | PropertyId::AlignItems | PropertyId::AlignSelf | PropertyId::JustifyItems | PropertyId::JustifySelf => {
            first_baseline(specified)
        }
        // the SVG rendering hints are keywords, lowercased (Chrome 152: `shape-rendering: crispEdges` reads
        // `crispedges`, `color-interpolation: sRGB` `srgb`, `text-rendering: optimizeLegibility` `optimizelegibility`)
        PropertyId::ColorInterpolation
        | PropertyId::ColorInterpolationFilters
        | PropertyId::ColorRendering
        | PropertyId::ShapeRendering
        | PropertyId::TextRendering
        | PropertyId::ImageRendering => Some(specified.trim().to_ascii_lowercase()),
        PropertyId::GridRowStart | PropertyId::GridRowEnd | PropertyId::GridColumnStart | PropertyId::GridColumnEnd => {
            grid_line(specified)
        }
        PropertyId::AspectRatio => aspect_ratio(specified),
        PropertyId::Rotate => rotate_degrees(specified),
        PropertyId::Scale => scale_numbers(specified),
        PropertyId::Zoom => zoom_number(specified),
        PropertyId::ShapeImageThreshold => fraction(specified).map(|n| serialize_number(n.clamp(0.0, 1.0))),
        PropertyId::WordSpacing if specified.trim().eq_ignore_ascii_case("normal") => Some("0px".to_string()),
        PropertyId::InterestDelayStart | PropertyId::InterestDelayEnd => {
            time_in_seconds(specified.trim()).map(|s| format!("{}s", super::serialize::format_number(s)))
        }
        PropertyId::ScrollSnapType | PropertyId::TextEmphasisPosition | PropertyId::ColorScheme | PropertyId::Contain => {
            keyword_canonical(property, specified)
        }
        PropertyId::CounterReset | PropertyId::CounterSet => counter_list(specified, 0),
        PropertyId::CounterIncrement => counter_list(specified, 1),
        _ => None,
    };
    if let Some(v) = converted {
        return v;
    }
    // `overflow: overlay` is a legacy alias of `auto` (CSS Overflow 3 §3.1), and computes to it:
    // Chrome 150 reads `auto` back.
    if matches!(property, PropertyId::OverflowX | PropertyId::OverflowY) && specified.trim().eq_ignore_ascii_case("overlay") {
        return "auto".to_string();
    }
    // A `<time>` computes to SECONDS: Blink's `ValueForAnimationDuration` / `ValueForAnimationDelay` make a
    // `CSSNumericLiteralValue` in `kSeconds`, written as `CustomCSSText` writes a number (`format_number`). Chrome 150
    // (`fixtures/css-time/chrome-150.json`): `80ms` reads `0.08s`, `1e7ms` `10000s`, `0.1234567s` `0.123457s`,
    // `-1200ms` `-1.2s`, while the SPECIFIED value keeps the author's unit (`style.transitionDuration` `80ms`). Read as
    // written, `80ms` broke more than a tell: Vue's `<Transition>` takes a duration as `Number(s.slice(0, -1)) * 1000`.
    //
    // `animation-duration: auto` -- the value Blink's `animation` shorthand gives an omitted duration -- computes to
    // `0s` for a time-based animation (Chrome 150 reads `0s`), one per layer.
    if matches!(
        property,
        PropertyId::TransitionDuration | PropertyId::TransitionDelay | PropertyId::AnimationDuration | PropertyId::AnimationDelay
    ) {
        return specified
            .split(',')
            .map(|v| {
                let v = v.trim();
                if property == PropertyId::AnimationDuration && v.eq_ignore_ascii_case("auto") {
                    return "0s".to_string();
                }
                match time_in_seconds(v) {
                    Some(s) => format!("{}s", super::serialize::format_number(s)),
                    None => v.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
    }
    // A LINE WIDTH -- a border's, the outline's, the column rule's -- computes to whole pixels:
    // anything between 0 and 1 is 1, anything else is floored (Blink's `ClampLineWidth`). Chrome 150:
    // `border: 2.715px solid` reads `2px` and lays out 2px wide, `0.5px` reads `1px`.
    if matches!(
        property,
        PropertyId::BorderTopWidth | PropertyId::BorderRightWidth | PropertyId::BorderBottomWidth | PropertyId::BorderLeftWidth
            | PropertyId::OutlineWidth | PropertyId::ColumnRuleWidth | PropertyId::RowRuleWidth
    ) {
        // the keywords are lengths too: `thin` 1px, `medium` 3px, `thick` 5px (Chrome 150 reads `1px`)
        let v = match specified.trim().to_ascii_lowercase().as_str() {
            "thin" => "1px".to_string(),
            "medium" => "3px".to_string(),
            "thick" => "5px".to_string(),
            _ => lengthy(specified, ctx),
        };
        return match v.strip_suffix("px").and_then(|n| n.parse::<f64>().ok()) {
            Some(w) if w > 0.0 && w < 1.0 => "1px".to_string(),
            Some(w) => serialize_px(w.floor() as f32),
            None => v,
        };
    }
    // `outline-offset` snaps as a line width does, on either side of zero: Chrome 152 (`len.html`) reads `0.5px` as
    // `1px`, `-0.5px` as `-1px`, `2.7px` as `2px` and `-2.7px` as `-2px`.
    if property == PropertyId::OutlineOffset {
        let v = lengthy(specified, ctx);
        return match v.strip_suffix("px").and_then(|n| n.parse::<f64>().ok()) {
            Some(w) if w != 0.0 && w.abs() < 1.0 => serialize_px(w.signum() as f32),
            Some(w) => serialize_px(w.trunc() as f32),
            None => v,
        };
    }
    // An `<opacity-value>` computes to a number clamped to [0, 1], a percentage as its fraction, written
    // as Blink writes a number. Chrome 150: `opacity: 0.00001` reads `1e-05`, `5e-7` reads `5e-07`.
    if matches!(property, PropertyId::Opacity | PropertyId::FillOpacity | PropertyId::StrokeOpacity | PropertyId::FloodOpacity | PropertyId::StopOpacity) {
        let t = specified.trim();
        let n = match t.strip_suffix('%') {
            Some(p) => p.trim().parse::<f32>().ok().map(|v| v / 100.0),
            None => t.parse::<f32>().ok(),
        };
        if let Some(n) = n {
            return serialize_number(n.clamp(0.0, 1.0));
        }
    }
    // `caret-color: auto` is the element's `currentcolor` -- inherited as such: a `color: blue` child of a `caret-color:
    // auto; color: red` parent reads blue -- and `resize: auto` is `both` where text areas resize (Blink's
    // `ConvertResize`). Chrome 152 reads `rgb(10, 20, 30)` under `color: rgb(10, 20, 30)`, and `both`.
    if property == PropertyId::CaretColor && specified.trim().eq_ignore_ascii_case("auto") {
        return super::computed::CURRENT_COLOR.to_string();
    }
    if property == PropertyId::Resize && specified.trim().eq_ignore_ascii_case("auto") {
        return "both".to_string();
    }
    // `ruby-overhang: none` is the legacy alias of `spaces` (Chrome 152's `RubyOverhang::ParseSingleValue` parses it
    // to `spaces`), so it computes and reads back as `spaces`.
    if property == PropertyId::RubyOverhang {
        let t = specified.trim().to_ascii_lowercase();
        return if t == "none" { "spaces".to_string() } else { t };
    }
    // `letter-spacing: 0` computes to `normal` (Blink's `LetterSpacing::CSSValueFromComputedStyleInternal` answers
    // `normal` for a zero spacing): Chrome 150 reads `normal` for `letter-spacing="0"` on an SVG text.
    if property == PropertyId::LetterSpacing {
        let v = lengthy(specified, ctx);
        return if v.strip_suffix("px").and_then(|n| n.parse::<f64>().ok()) == Some(0.0) { "normal".to_string() } else { v };
    }
    // `stroke-dasharray` is a comma-separated list of lengths, a number one in px (Blink's
    // `StrokeDashArrayToCSSValueList`): Chrome 150 reads `5px, 10px` for `stroke-dasharray="5 10"`.
    if property == PropertyId::StrokeDasharray {
        let t = specified.trim();
        if t.eq_ignore_ascii_case("none") || t.is_empty() {
            return "none".to_string();
        }
        // cut at the top level: a math function's own spaces and commas stay in it (Chrome 152 reads `calc(10px +
        // 2em)` under `font-size: 20px` as `50px`, `calc(50% - 10px)` as written; this read `10px, +, 2em)`)
        let items: Vec<String> = top_level_items(t)
            .iter()
            .flat_map(|item| top_level_words(item))
            .map(|v| if v.parse::<f64>().is_ok() { lengthy(&format!("{v}px"), ctx) } else if v.ends_with('%') { v.to_string() } else { lengthy(v, ctx) })
            .collect();
        return items.join(", ");
    }
    // A length-valued property the table keeps as written -- its value is more than one length: keywords
    // beside it, a track list, an SVG number -- still computes its lengths to pixels. Chrome 152
    // (`getComputedStyle` with `font-size: 20px`): `scroll-margin-top: 2em` reads `40px`, `grid-template-columns:
    // 0 0 50px` `0px 0px 50px`, `contain-intrinsic-width: auto 2em` `auto 40px`, `x: 1` `1px`; korvus read them
    // back as written.
    if matches!(property, PropertyId::TransformOrigin | PropertyId::PerspectiveOrigin) {
        if let Some(v) = origin(specified, ctx) {
            return v;
        }
    }
    if let Some(v) = collapsed(property, specified, ctx) {
        return v;
    }
    if let Some(numbers) = length_bearing(property) {
        if property == PropertyId::BaselineShift && specified.trim().eq_ignore_ascii_case("baseline") {
            // Chrome 152 reads `baseline-shift: baseline` as `0px`
            return "0px".to_string();
        }
        return absolutized(specified, ctx, numbers).unwrap_or_else(|| specified.to_string());
    }
    match property.computed_kind() {
        Computed::AsSpecified => specified.to_string(),
        Computed::Keyword => specified.trim().to_ascii_lowercase(),
        Computed::Display => display(specified),
        // `currentcolor` stays the keyword, read as the element's `color` wherever it is read (`StyleArena::value`), so
        // that it inherits as itself; `color: currentcolor` is `inherit`, and the cascade resolves it before this
        Computed::Color if property != PropertyId::Color && specified.trim().eq_ignore_ascii_case("currentcolor") => {
            super::computed::CURRENT_COLOR.to_string()
        }
        Computed::Color => color(specified, ctx).unwrap_or_else(|| paint_fallback(specified, ctx)),
        Computed::Lengthy => lengthy(specified, ctx),
        Computed::FontSize => serialize_px(ctx.computed_font_size),
        Computed::FontWeight => font_weight(specified, ctx.parent_font_weight)
            .map_or_else(|| specified.to_string(), serialize_number),
        Computed::LineHeight => line_height(specified, ctx),
    }
}

/// A `<paint>` that is not a colour (`fill`, `stroke`): `none`, `context-fill`, `context-stroke` as they are, and a
/// `url()` with its fallback colour computed -- Chrome 150 reads `fill: url(#a) red` as `url("#a") rgb(255, 0, 0)`.
fn paint_fallback(specified: &str, ctx: &Context) -> String {
    let t = specified.trim();
    // the prefix compared as bytes: `t[..4]` of `"éé"` cuts the `é` (a page's `style="fill: &quot;éé&quot;"`
    // aborted the process, 2026-10-23)
    if t.len() > 4 && t.as_bytes()[..4].eq_ignore_ascii_case(b"url(") {
        if let Some((_, after)) = crate::style::urls::url_at(t, 4) {
            let (url, rest) = (&t[..after], t[after..].trim());
            if !rest.is_empty() && !rest.eq_ignore_ascii_case("none") {
                if let Some(c) = color(rest, ctx) {
                    return format!("{url} {c}");
                }
            }
        }
    }
    specified.to_string()
}

/// `display`, normalised to the single spelling Chrome serializes.
///
/// CSS Display Level 3 writes `display` as `<display-outside> || <display-inside> || list-item`, so
/// `block flex`, `flex block` and `flex` are three spellings of one value. The computed value is
/// **one** of them -- the shortest form that exists -- and `getComputedStyle` has to answer with
/// that one: Chrome reports `flex` for all three. This engine used to lowercase the text and hand it
/// back, so a page that wrote `display: block flex` read `"block flex"` back, `display(value)` in
/// `layout::style` did not recognise it as a flex container, and `INLINE_DISPLAYS` in the
/// rendered-text steps did not recognise `inline flow-root` as an inline.
///
/// # The table is RECORDED, not read off the spec
///
/// Measured on stock Chrome 150.0.7871.46 over CDP on a loopback fixture: every one-, two- and
/// three-keyword combination of `block inline run-in flow flow-root table flex grid ruby list-item`
/// -- 1,110 values, of which Chrome accepts **65**, and this function reproduces all 1,110 answers.
/// Four of them are not what the prose suggests:
///
/// * **`block flow-root` is `flow-root`, not `block`.** `flow-root` is a real single keyword and it
///   is the shorter spelling, so it wins. A reader who normalised "block + anything = block" would
///   collapse a block formatting context into a plain block.
/// * **`ruby` alone means `inline ruby`, not `block ruby`.** Every other `<display-inside>` used
///   alone takes the `block` default -- `flex` is `block flex`, `grid` is `block grid` -- and `ruby`
///   does not: `ruby` and `inline ruby` both compute `ruby`, while `block ruby` stays `block ruby`.
/// * **The order carries no information.** `flex inline`, `inline flex` and `flex block` are
///   accepted and compute exactly as the canonical spelling does, so the keywords are parsed as a
///   set. But `block block` and `flex grid` are REJECTED: each category may appear once.
/// * **`list-item` combines only with `flow` and `flow-root`.** `flex list-item` and `ruby
///   list-item` are rejected, and three of the four accepted combinations keep a multi-keyword
///   computed value because no shorter spelling exists: `flow-root list-item`, `inline list-item`
///   and `inline flow-root list-item`.
///
/// # What it deliberately leaves alone
///
/// A value this table does not accept is handed back lowercased and unchanged, which is exactly
/// what happened to it before. Chrome DROPS such a declaration -- `display: run-in`, `display:
/// ruby-base`, `display: block block` and `display: flex list-item` all leave the element at its
/// previous value -- and this engine still does not validate keyword values anywhere (see `float:
/// bogus`, pinned by P7b in `blockify_live.rs`). Normalising what Chrome accepts without learning
/// to reject what it does not is the narrow half of that job, and the wide half is a cascade-level
/// change this is not.
///
/// # How the 65 were counted, because the first count was wrong
///
/// The acceptance scan writes `display: <sentinel>; display: <value>` and asks what came back: if
/// the sentinel survives, the second declaration was dropped. The sentinel was `block` at first,
/// which cannot tell "rejected" from "accepted and computes to `block`" -- and that is precisely
/// what `display: flow`, `display: block flow` and `display: flow block` are. Three values were
/// recorded as rejected that Chrome accepts, and the bare `flow` had a special case written for it
/// here before the scan was re-run with `table-caption` as the sentinel, which no combination of
/// these ten keywords can produce. `flow` is `block flow` is `block`, with no special case.
fn display(specified: &str) -> String {
    let lower = specified.trim().to_ascii_lowercase();
    match canonical_display(&lower) {
        Some(canonical) => canonical.to_string(),
        None => lower,
    }
}

/// Is `lower` a `display` value this engine accepts -- one of the legacy single keywords, or a
/// multi-keyword spelling `canonical_display` knows? What `@supports (display: …)` asks.
pub(crate) fn is_display_value(lower: &str) -> bool {
    matches!(
        lower.trim(),
        "none" | "contents" | "inline-block" | "inline-flex" | "inline-grid" | "inline-table"
            | "table-row-group" | "table-header-group" | "table-footer-group" | "table-row"
            | "table-cell" | "table-column-group" | "table-column" | "table-caption" | "ruby-text"
            | "-webkit-box" | "-webkit-inline-box" | "math"
    ) || canonical_display(lower.trim()).is_some()
}

/// The canonical spelling of one `display` value, or `None` when there is nothing to normalise --
/// which covers both "already canonical" (`inline-block`, `table-cell`, `none`) and "Chrome does
/// not accept this at all" (`run-in`, `ruby-base`, a repeated category).
fn canonical_display(lower: &str) -> Option<&'static str> {
    let (mut outside, mut inside, mut list_item, mut count) = (None, None, false, 0usize);
    for token in lower.split_ascii_whitespace() {
        count += 1;
        match token {
            "block" | "inline" if outside.is_none() => outside = Some(token),
            "flow" | "flow-root" | "table" | "flex" | "grid" | "ruby" if inside.is_none() => {
                inside = Some(token)
            }
            "list-item" if !list_item => list_item = true,
            // A repeated category (`block block`, `flex grid`), a legacy single keyword that is
            // already canonical (`inline-block`), or something Chrome does not accept.
            _ => return None,
        }
    }
    if count == 0 {
        return None;
    }
    // `ruby` is the one `<display-inside>` whose implied outside is `inline` rather than `block`.
    let outside = outside.unwrap_or(if inside == Some("ruby") { "inline" } else { "block" });
    let inside = inside.unwrap_or("flow");
    Some(if list_item {
        match (outside, inside) {
            ("block", "flow") => "list-item",
            ("block", "flow-root") => "flow-root list-item",
            ("inline", "flow") => "inline list-item",
            ("inline", "flow-root") => "inline flow-root list-item",
            // `list-item` with a flex, grid, table or ruby inside: Chrome rejects the declaration.
            _ => return None,
        }
    } else {
        match (outside, inside) {
            ("block", "flow") => "block",
            ("block", "flow-root") => "flow-root",
            ("block", "flex") => "flex",
            ("block", "grid") => "grid",
            ("block", "table") => "table",
            ("block", "ruby") => "block ruby",
            ("inline", "flow") => "inline",
            ("inline", "flow-root") => "inline-block",
            ("inline", "flex") => "inline-flex",
            ("inline", "grid") => "inline-grid",
            ("inline", "table") => "inline-table",
            ("inline", "ruby") => "ruby",
            _ => return None,
        }
    })
}

fn color(specified: &str, ctx: &Context) -> Option<String> {
    // A colour that does not name `currentcolor` is a function of its text alone, and the same few are computed over
    // and over (a shadow's `rgba(0,0,0,.2)` and `red` on every element styled: a parse and a `serialize_number`
    // each, ~24k instructions of the ~38k a two-shadow `box-shadow` added to an element, `ops5.html` `casc_shadow`,
    // 2026-10-23), so the recent answers are kept, bounded by count and bytes (`style::memo`).
    type Seen = korvus_dom::FxHashMap<Box<str>, Option<Box<str>>>;
    thread_local! {
        static COLORS: std::cell::RefCell<Seen> = Default::default();
        static COLORS_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let pure = !specified.as_bytes().windows(12).any(|w| w.eq_ignore_ascii_case(b"currentcolor"));
    if pure {
        if let Some(hit) = COLORS.with(|c| c.borrow().get(specified).cloned()) {
            return hit.map(String::from);
        }
    }
    let answer = color_uncached(specified, ctx);
    if pure {
        COLORS.with(|c| {
            let mut c = c.borrow_mut();
            let cost = specified.len() + answer.as_ref().map_or(0, String::len);
            if COLORS_BYTES.with(|b| crate::style::memo::admit(c.len(), 512, b, cost, || c.clear())) {
                c.insert(specified.into(), answer.as_deref().map(Box::from));
            }
        });
    }
    answer
}

fn color_uncached(specified: &str, ctx: &Context) -> Option<String> {
    // `currentcolor`, and a `color-mix()` / relative colour naming it, resolve against the element's `color`.
    if let Some(c) = crate::value::resolve_color(specified, ctx.color) {
        return Some(Color::Rgba(c).serialize());
    }
    Some(Color::parse_str(specified).ok()?.serialize())
}

/// A length-valued property: lengths become pixels, everything else is left alone.
///
/// Multi-value cases (`border-spacing: 1px 2px`, `object-position: 50% 50%`) are resolved
/// component by component and rejoined with single spaces, which is also how the CSSOM writes them.
fn lengthy(specified: &str, ctx: &Context) -> String {
    // A layer list (`background-position-x: 1em, right 2em`, `background-size: 1em, cover`): each item on its
    // own, as Chrome 150 computes `16px, right 32px`. The comma used to ride on the
    // token before it, which then parsed as no length at all.
    let layers = top_level_items(specified);
    if layers.len() > 1 {
        return layers.iter().map(|item| lengthy(item.trim(), ctx)).collect::<Vec<_>>().join(", ");
    }
    let resolve = ctx.lengths();
    // A math function: its lengths resolved here, where the font and the viewport are known -- to
    // pixels when no percentage is in it (Chrome's `3px` for `calc(1px + 2px)`), else to the canonical form layout
    // finishes against its basis (`calc(100% - 20px)`, `min(100%, 300px)`).
    if super::math::is_math(specified) {
        return match super::math::reduce(specified, &resolve) {
            Some(n) => n.px().map_or_else(|| n.serialize(), serialize_px),
            None => specified.to_string(),
        };
    }
    let mut parts = Vec::new();
    for token in specified.split_ascii_whitespace() {
        let mut pi = ParserInput::new(token);
        let mut p = Parser::new(&mut pi);
        match LengthPercentage::parse(&mut p) {
            Ok(LengthPercentage::Length(l)) if p.expect_exhausted().is_ok() => {
                parts.push(serialize_px(l.to_px(&resolve)))
            }
            // A percentage resolves against a containing block layout has not established, and a
            // keyword (`auto`, `normal`, `thin`) is already computed.
            _ => parts.push(token.to_string()),
        }
    }
    parts.join(" ")
}

/// What a bare number is in a [`length_bearing`] property's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Numbers {
    /// A length in pixels: the SVG geometry and stroke properties (Chrome 152: `x: 1` reads `1px`, `stroke-width:
    /// 0.5` `0.5px`).
    Px,
    /// Only a zero, which is the unitless `<length>` 0 (Chrome 152: `scroll-margin-top: 0` reads `0px`,
    /// `grid-template-columns: 0 0 50px` `0px 0px 50px`).
    Zero,
    /// Nothing: a number is a value of its own kind there (Chrome 152: `tab-size: 0` reads `0`, `border-image-width: 1`
    /// a multiplier `1`).
    Keep,
}

/// The properties [`computed_value`] keeps as written (`Computed::AsSpecified`) whose lengths compute to pixels
/// all the same, and what a bare number is in each. Every one measured on Chrome 152 (`getComputedStyle`, `2em`
/// under `font-size: 20px` reads `40px` in each; `calc(10px + 2em)` `50px`).
fn length_bearing(property: PropertyId) -> Option<Numbers> {
    use PropertyId::*;
    Some(match property {
        X | Y | Cx | Cy | R | Rx | Ry | StrokeWidth | StrokeDashoffset | BaselineShift => Numbers::Px,
        ScrollMarginTop | ScrollMarginRight | ScrollMarginBottom | ScrollMarginLeft | ScrollMarginBlockStart
        | ScrollMarginBlockEnd | ScrollMarginInlineStart | ScrollMarginInlineEnd | ScrollPaddingTop | ScrollPaddingRight
        | ScrollPaddingBottom | ScrollPaddingLeft | ScrollPaddingBlockStart | ScrollPaddingBlockEnd
        | ScrollPaddingInlineStart | ScrollPaddingInlineEnd | ColumnWidth | ColumnHeight | Perspective | ShapeMargin
        | TextUnderlineOffset | AnimationRangeStart | AnimationRangeEnd | TimelineTriggerActivationRangeStart
        | TimelineTriggerActivationRangeEnd | TimelineTriggerActiveRangeStart | TimelineTriggerActiveRangeEnd
        | ColumnRuleInsetCapStart | ColumnRuleInsetCapEnd | ColumnRuleInsetJunctionStart | ColumnRuleInsetJunctionEnd
        | RowRuleInsetCapStart | RowRuleInsetCapEnd | RowRuleInsetJunctionStart | RowRuleInsetJunctionEnd
        | ContainIntrinsicWidth | ContainIntrinsicHeight | ContainIntrinsicBlockSize | ContainIntrinsicInlineSize
        | ContainIntrinsicSize | OverflowClipMargin | GridTemplateColumns | GridTemplateRows | GridAutoColumns
        | GridAutoRows | Translate => Numbers::Zero,
        // a transform function's lengths too: `translate(10px, 2em)` is `translate(10px, 40px)` before the list becomes
        // the `matrix()` `getComputedStyle` answers (Chrome 152: `rotate(45deg) translate(10px, 2em)` reads `matrix(0.707107,
        // 0.707107, -0.707107, 0.707107, -21.2132, 35.3553)`; it read as written, the `em` unparsed)
        TabSize | BorderImageOutset | BorderImageWidth | WebkitMaskBoxImageOutset | WebkitMaskBoxImageWidth | Transform => {
            Numbers::Keep
        }
        _ => return None,
    })
}

/// `specified` with every length in it computed to pixels -- at the top level, in each comma-separated item, and
/// inside a function that is not a math function (`minmax(2em, 1fr)`, `repeat(2, 1em)`); a math function reduced
/// as [`lengthy`] reduces one. `None` when nothing in it changes, so a value with no relative length in it stays
/// exactly as written.
fn absolutized(specified: &str, ctx: &Context, numbers: Numbers) -> Option<String> {
    if settled(specified, numbers) {
        return None;
    }
    let resolve = ctx.lengths();
    let mut changed = false;
    let out = absolutized_list(specified, &resolve, numbers, &mut changed);
    changed.then_some(out)
}

fn absolutized_list(text: &str, resolve: &ResolveContext, numbers: Numbers, changed: &mut bool) -> String {
    top_level_items(text)
        .iter()
        .map(|item| top_level_words(item).iter().map(|w| absolutized_word(w, resolve, numbers, changed)).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(", ")
}

fn absolutized_word(word: &str, resolve: &ResolveContext, numbers: Numbers, changed: &mut bool) -> String {
    // a plain word only: a function's arguments are written back one space after each comma whatever they hold
    if !word.contains('(') && settled(word, numbers) {
        return word.to_string();
    }
    // the two words the parse is asked for most, answered as it answers them: a bare `0` where a number is a length
    // (a shadow's `0 1px 5px`), and a whole number of a length unit (`2em`), exact in an `f32` either way
    if word == "0" && numbers != Numbers::Keep {
        *changed = true;
        return "0px".to_string();
    }
    if let Some(px) = whole_length(word).map(|l| l.to_px(resolve)) {
        let s = serialize_px(px);
        *changed |= s != word;
        return s;
    }
    absolutized_token(word, resolve, numbers, changed)
}

/// `word` as a length when it is a whole number of at most six digits, signed or not, and a length unit -- what
/// `LengthPercentage::parse` reads it as, the number exact either way.
fn whole_length(word: &str) -> Option<crate::value::Length> {
    let digits_at = usize::from(word.starts_with('-'));
    let unit_at = digits_at + word[digits_at..].bytes().take_while(u8::is_ascii_digit).count();
    let unit = &word[unit_at..];
    if !(1..=6).contains(&(unit_at - digits_at)) || unit.is_empty() || !unit.bytes().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    crate::value::length_from(word[..unit_at].parse::<f32>().ok()?, unit).ok()
}

/// [`absolutized_word`] by the parse.
fn absolutized_token(word: &str, resolve: &ResolveContext, numbers: Numbers, changed: &mut bool) -> String {
    if super::math::is_math(word) {
        return match super::math::reduce(word, resolve) {
            Some(n) => {
                *changed = true;
                n.px().map_or_else(|| n.serialize(), serialize_px)
            }
            None => word.to_string(),
        };
    }
    if let Some(open) = word.find('(').filter(|&o| o > 0 && word.ends_with(')')) {
        let inner = absolutized_list(&word[open + 1..word.len() - 1], resolve, numbers, changed);
        return format!("{}({inner})", &word[..open]);
    }
    let mut pi = ParserInput::new(word);
    let mut p = Parser::new(&mut pi);
    let token = p.next().ok().cloned();
    let px = match token {
        Some(cssparser::Token::Number { value, .. }) if p.is_exhausted() => match numbers {
            Numbers::Px => Some(value),
            Numbers::Zero if value == 0.0 => Some(0.0),
            _ => None,
        },
        Some(cssparser::Token::Dimension { .. }) if p.is_exhausted() => {
            let mut pi = ParserInput::new(word);
            let mut p = Parser::new(&mut pi);
            match LengthPercentage::parse(&mut p) {
                Ok(LengthPercentage::Length(l)) if p.expect_exhausted().is_ok() => Some(l.to_px(resolve)),
                _ => None,
            }
        }
        _ => None,
    };
    match px {
        Some(v) => {
            let s = serialize_px(v);
            *changed |= s != word;
            s
        }
        None => word.to_string(),
    }
}

/// Is `text` one [`absolutized`] leaves exactly as written -- every word as [`absolutized_word`] would write it --
/// told by one pass over its bytes, without a parse or an allocation: no math function and no function but the
/// transform and grid ones it reads through, no length but a pixel one already written as `serialize_px` writes it
/// (no sign but `-`, no leading or trailing zero, no exponent, at most six significant digits, from 1e-4 to 1e6),
/// no number [`Numbers`] makes a length, and no unit, string, escape or comment it does not know. `false` only means
/// the parse is asked.
///
/// The parse is a cssparser per word and per dimension, and a `String` per word, item and value: a distinct
/// `transform` written every frame paid ~20k instructions more per frame for a value with nothing to compute
/// (`ops5.html` `om_pxd`, +10%; `setAttribute` of `translate(Npx, 2em) rotate(Ndeg)` +20%), a shadow list
/// +12-33% per element styled (2026-10-23).
fn settled(text: &str, numbers: Numbers) -> bool {
    const FUNCTIONS: &[&str] = &[
        "matrix", "matrix3d", "translate", "translatex", "translatey", "translatez", "translate3d", "scale", "scalex",
        "scaley", "scalez", "scale3d", "rotate", "rotatex", "rotatey", "rotatez", "rotate3d", "skew", "skewx", "skewy",
        "perspective", "repeat", "minmax", "fit-content",
    ];
    // units a `<length-percentage>` parse refuses, so a dimension in one is never a length
    const NOT_LENGTHS: &[&str] = &["deg", "grad", "rad", "turn", "s", "ms", "hz", "khz", "fr", "dpi", "dpcm", "dppx", "x"];
    let b = text.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let number = c.is_ascii_digit()
            || (c == b'.' && digit(i + 1))
            || (c == b'-' && (digit(i + 1) || (b.get(i + 1) == Some(&b'.') && digit(i + 2))))
            || c == b'+';
        if number {
            if c == b'+' {
                return false;
            }
            let start = i;
            if c == b'-' {
                i += 1;
            }
            let int = i;
            while digit(i) {
                i += 1;
            }
            let int_digits = &text[int..i];
            let mut frac = "";
            if b.get(i) == Some(&b'.') && digit(i + 1) {
                let f = i + 1;
                i += 1;
                while digit(i) {
                    i += 1;
                }
                frac = &text[f..i];
            }
            // an exponent is a number `serialize_number` writes another way
            if matches!(b.get(i), Some(b'e' | b'E')) && (digit(i + 1) || (matches!(b.get(i + 1), Some(b'+' | b'-')) && digit(i + 2))) {
                return false;
            }
            let num = &text[start..i];
            let unit_start = i;
            while i < b.len() && (b[i].is_ascii_alphabetic()) {
                i += 1;
            }
            let unit = &text[unit_start..i];
            if b.get(i) == Some(&b'%') {
                if !unit.is_empty() {
                    return false;
                }
                i += 1;
                continue;
            }
            if i < b.len() && (ident(b[i]) || b[i] >= 0x80 || b[i] == b'\\') {
                return false;
            }
            let zero = int_digits.bytes().all(|d| d == b'0') && frac.bytes().all(|d| d == b'0');
            match unit {
                "" => match numbers {
                    Numbers::Px => return false,
                    Numbers::Zero if zero => return false,
                    _ => {}
                },
                "px" => {
                    let canonical_int = int_digits == "0" || (!int_digits.is_empty() && !int_digits.starts_with('0'));
                    let canonical_frac = frac.is_empty() || !frac.ends_with('0');
                    let lead = int_digits.trim_start_matches('0');
                    let significant = if lead.is_empty() { frac.trim_start_matches('0').len() } else { lead.len() + frac.len() };
                    let tiny = !zero && int_digits == "0" && frac.starts_with("0000");
                    if !canonical_int || !canonical_frac || (zero && num != "0") || significant > 6 || tiny {
                        return false;
                    }
                }
                u if NOT_LENGTHS.contains(&u) => {}
                _ => return false,
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' || (c == b'-' && b.get(i + 1).is_some_and(|&d| d.is_ascii_alphabetic() || d == b'-' || d == b'_')) {
            let start = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            if i < b.len() && (b[i] >= 0x80 || b[i] == b'\\') {
                return false;
            }
            if b.get(i) == Some(&b'(') && !FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(&text[start..i])) {
                return false;
            }
            continue;
        }
        match c {
            b' ' | b'\t' | b'\n' | b',' | b'(' | b')' => i += 1,
            b'/' if b.get(i + 1) != Some(&b'*') => i += 1,
            _ => return false,
        }
    }
    true
}

/// The values Blink computes into a pair or a four-sided quad and writes back as short as they read the same
/// (`CSSValuePair` / `CSSQuadValue`, components compared as computed). Chrome 152 (`pairs.html`, 2026-10-23, under
/// `font-size: 20px`): `border-top-left-radius: 1em 20px` reads `20px`, `border-image-outset: 1px 2px 1px 2px` `1px
/// 2px` (and `1em 0 20px 0px` `20px 0 20px 0px`: a number and a length are not alike), `border-image-width: auto auto`
/// `auto`, `border-image-slice: 1.5 2.5` `2 3` (a slice number is a whole one, `fill` last), `contain-intrinsic-size:
/// auto 2px auto 2px` `auto 2px`, `view-timeline-inset: 1px 1px, 2px 3px` `1px, 2px 3px`. korvus read each pair and
/// quad as long as it was written.
fn collapsed(property: PropertyId, specified: &str, ctx: &Context) -> Option<String> {
    use PropertyId::*;
    let pair = |words: Vec<String>| if words.len() == 2 && words[0] == words[1] { words[0].clone() } else { words.join(" ") };
    Some(match property {
        BorderTopLeftRadius | BorderTopRightRadius | BorderBottomRightRadius | BorderBottomLeftRadius => {
            pair(lengthy(specified, ctx).split_ascii_whitespace().map(str::to_string).collect())
        }
        ViewTimelineInset => top_level_items(&lengthy(specified, ctx))
            .iter()
            .map(|item| pair(item.split_ascii_whitespace().map(str::to_string).collect()))
            .collect::<Vec<_>>()
            .join(", "),
        BorderImageOutset | BorderImageWidth | WebkitMaskBoxImageOutset | WebkitMaskBoxImageWidth => {
            let v = absolutized(specified, ctx, Numbers::Keep).unwrap_or_else(|| specified.trim().to_string());
            quad(&top_level_words(&v).iter().map(|w| w.to_string()).collect::<Vec<_>>())?
        }
        BorderImageSlice | WebkitMaskBoxImageSlice => {
            let words = top_level_words(specified.trim());
            let fill = words.iter().any(|w| w.eq_ignore_ascii_case("fill"));
            let sides: Vec<String> = words
                .iter()
                .filter(|w| !w.eq_ignore_ascii_case("fill"))
                .map(|w| match w.parse::<f64>() {
                    // a slice number is a whole number of image pixels (Blink rounds it half up)
                    Ok(n) if n.is_finite() => serialize_number((n + 0.5).floor() as f32),
                    _ => w.to_string(),
                })
                .collect();
            let q = quad(&sides)?;
            if fill { format!("{q} fill") } else { q }
        }
        ContainIntrinsicSize => {
            let v = absolutized(specified, ctx, Numbers::Zero).unwrap_or_else(|| specified.trim().to_string());
            // its two components, each `none`, a length, or `auto` and a length
            let mut items: Vec<String> = Vec::new();
            let mut auto = false;
            for w in v.split_ascii_whitespace() {
                if w.eq_ignore_ascii_case("auto") && !auto {
                    auto = true;
                } else if auto {
                    items.push(format!("auto {w}"));
                    auto = false;
                } else {
                    items.push(w.to_string());
                }
            }
            if auto {
                return None;
            }
            pair(items)
        }
        _ => return None,
    })
}

/// One to four sides as CSS spells them, written as short as they read the same (`top right bottom left`).
fn quad(sides: &[String]) -> Option<String> {
    let [t, r, b, l] = match sides {
        [a] => [a, a, a, a],
        [a, b] => [a, b, a, b],
        [a, b, c] => [a, b, c, b],
        [a, b, c, d] => [a, b, c, d],
        _ => return None,
    };
    Some(if l != r {
        format!("{t} {r} {b} {l}")
    } else if b != t {
        format!("{t} {r} {b}")
    } else if r != t {
        format!("{t} {r}")
    } else {
        t.clone()
    })
}

/// `transform-origin` / `perspective-origin` as Blink computes them: the horizontal component, then the vertical --
/// a keyword, a percentage or pixels -- then a `transform-origin`'s z offset when one is written. Chrome 152 (a 400px
/// box, `font-size: 20px`): `2em` reads `40px 14.5px` (the box's centre line), `top` `200px 0px`, `bottom center`
/// `200px 29px`, `calc(10px + 2em)` `50px 14.5px`, `left 10px top 2em` `10px 40px`. korvus kept the text, so layout
/// took `2em` at 16px, a lone value's missing half as `0px`, and `top` as the horizontal one. `None` for a
/// value of another shape (a three-value `<position>`), which stays as written.
fn origin(specified: &str, ctx: &Context) -> Option<String> {
    let words = top_level_words(specified.trim());
    let lower: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
    let length = |w: &str| absolutized(w, ctx, Numbers::Zero).unwrap_or_else(|| w.to_string());
    let keyword = |i: usize| matches!(lower[i].as_str(), "left" | "right" | "top" | "bottom" | "center");
    let component = |i: usize| if keyword(i) { lower[i].clone() } else { length(words[i]) };
    // a component that is no keyword and no `<length-percentage>` is a value this does not model (an `attr()`)
    if (0..words.len()).any(|i| !keyword(i) && !shorthand_lp(&length(words[i]))) {
        return None;
    }
    let vertical = |i: usize| matches!(lower[i].as_str(), "top" | "bottom");
    let horizontal = |i: usize| matches!(lower[i].as_str(), "left" | "right");
    Some(match words.len() {
        1 if vertical(0) => format!("center {}", lower[0]),
        1 => format!("{} center", component(0)),
        2 | 3 => {
            let (x, y) = if vertical(0) || horizontal(1) { (1, 0) } else { (0, 1) };
            let mut v = format!("{} {}", component(x), component(y));
            if words.len() == 3 {
                v.push(' ');
                v.push_str(&length(words[2]));
            }
            v
        }
        // `<position>`'s four-value form, each side keyword with its offset from that side
        4 if keyword(0) && keyword(2) && !keyword(1) && !keyword(3) => {
            let (x, y) = if vertical(0) || horizontal(2) { (2, 0) } else { (0, 2) };
            let from = |side: usize| -> Option<String> {
                let offset = length(words[side + 1]);
                match lower[side].as_str() {
                    "left" | "top" => Some(offset),
                    // from the far side: a percentage is its complement, a length would be a `calc()`, which
                    // stays as written
                    _ => offset.strip_suffix('%').and_then(|p| p.parse::<f32>().ok()).map(|p| format!("{}%", serialize_number(100.0 - p))),
                }
            };
            format!("{} {}", from(x)?, from(y)?)
        }
        _ => return None,
    })
}

// ---- resolve.rs: computed values Blink writes differently from the specified text ----

/// `font-stretch` computes to a percentage (Chrome 150: `condensed` reads `75%`).
fn font_stretch(specified: &str) -> Option<String> {
    let pct = match specified.trim().to_ascii_lowercase().as_str() {
        "ultra-condensed" => "50%",
        "extra-condensed" => "62.5%",
        "condensed" => "75%",
        "semi-condensed" => "87.5%",
        "normal" => "100%",
        "semi-expanded" => "112.5%",
        "expanded" => "125%",
        "extra-expanded" => "150%",
        "ultra-expanded" => "200%",
        _ => return None,
    };
    Some(pct.to_string())
}

/// An `<angle>` in degrees (`0.25turn` is `90`, `1rad` `57.2958`), or `None`.
fn angle_degrees(t: &str) -> Option<f64> {
    let l = t.trim().to_ascii_lowercase();
    let (num, factor) = if let Some(n) = l.strip_suffix("deg") {
        (n, 1.0)
    } else if let Some(n) = l.strip_suffix("grad") {
        (n, 0.9)
    } else if let Some(n) = l.strip_suffix("rad") {
        (n, 180.0 / std::f64::consts::PI)
    } else if let Some(n) = l.strip_suffix("turn") {
        (n, 360.0)
    } else {
        return None;
    };
    num.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| v * factor)
}

/// `offset-rotate` (Blink `OffsetRotate::CSSValueFromComputedStyleInternal`): `auto` before the angle when the
/// rotation is automatic, `reverse` as `auto` plus 180 degrees, the angle always, in degrees.
fn offset_rotate(specified: &str) -> Option<String> {
    let (mut auto, mut angle) = (false, 0.0);
    for t in specified.split_whitespace() {
        match t.to_ascii_lowercase().as_str() {
            "auto" => auto = true,
            "reverse" => {
                auto = true;
                angle += 180.0;
            }
            _ => angle += angle_degrees(t)?,
        }
    }
    let deg = format!("{}deg", serialize_number(angle as f32));
    Some(if auto { format!("auto {deg}") } else { deg })
}

/// `text-emphasis-style` (Blink `TextEmphasisStyle::CSSValueFromComputedStyleInternal`): `filled` is left out, a
/// missing shape is `dot` in a horizontal writing mode (`sesame` in a vertical one), `open` comes first.
fn text_emphasis_style(specified: &str, vertical: bool) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("none") || t.starts_with('"') || t.starts_with('\'') {
        return None;
    }
    let (mut open, mut shape) = (false, None::<String>);
    for w in t.split_whitespace() {
        match w.to_ascii_lowercase().as_str() {
            "filled" => {}
            "open" => open = true,
            s @ ("dot" | "circle" | "double-circle" | "triangle" | "sesame") => shape = Some(s.to_string()),
            _ => return None,
        }
    }
    let shape = shape.unwrap_or_else(|| if vertical { "sesame" } else { "dot" }.to_string());
    Some(if open { format!("open {shape}") } else { shape })
}

/// `-webkit-text-stroke-width`'s keywords (Blink `ConvertTextStrokeWidth`): the line width's 1, 3 or 5 pixels as
/// as many 48ths of an `em` -- at 16px a third of a pixel, one, five thirds; Chrome 152 under `font-size: 20px` reads
/// `thin` `0.416667px`, `medium` `1.25px`, `thick` `2.08333px` (these were the 16px answers at every size).
fn text_stroke_width(specified: &str, ctx: &Context) -> Option<String> {
    let width = match specified.trim().to_ascii_lowercase().as_str() {
        "thin" => 1.0,
        "medium" => 3.0,
        "thick" => 5.0,
        _ => return None,
    };
    Some(serialize_px(width / 48.0 * ctx.font_size))
}

/// A corner shape as Chrome 152 computes it: always `superellipse(<number>)` (Blink `ComputedStyleUtils::
/// ValueForCornerShape`, which Chrome 150 still wrote as the keyword when there was one) -- `round` is
/// `superellipse(1)`, `square` `superellipse(infinity)`, `bevel` `superellipse(0)`, `notch`
/// `superellipse(-infinity)`, `scoop` `superellipse(-1)`, `squircle` `superellipse(2)`, and a number as Blink writes
/// one (`superellipse(1e3)` reads `superellipse(1000)`). Measured on Chrome 152.0.7977.82, 2026-10-16.
fn corner_shape(specified: &str) -> Option<String> {
    let l = specified.trim().to_ascii_lowercase();
    let k = match l.as_str() {
        "round" => 1.0,
        "square" => f64::INFINITY,
        "bevel" => 0.0,
        "notch" => f64::NEG_INFINITY,
        "scoop" => -1.0,
        "squircle" => 2.0,
        _ => match l.strip_prefix("superellipse(")?.strip_suffix(')')?.trim() {
            "infinity" => f64::INFINITY,
            "-infinity" => f64::NEG_INFINITY,
            a => a.parse::<f64>().ok()?,
        },
    };
    let n = if k.is_infinite() { (if k > 0.0 { "infinity" } else { "-infinity" }).to_string() } else { crate::value::serialize_number_f64(k) };
    Some(format!("superellipse({n})"))
}

/// `grid-auto-flow` as Blink writes it: the direction, `row` left out beside `dense`.
fn grid_auto_flow(specified: &str) -> Option<String> {
    let words: Vec<String> = specified.split_whitespace().map(str::to_ascii_lowercase).collect();
    let dense = words.iter().any(|w| w == "dense");
    let column = words.iter().any(|w| w == "column");
    Some(match (column, dense) {
        (true, true) => "column dense",
        (true, false) => "column",
        (false, true) => "dense",
        (false, false) => "row",
    }
    .to_string())
}

/// `offset-path`'s `path()` with its data as Blink's `SVGPathStringBuilder` writes it (Chrome 150:
/// `path("M0 0L10 10")` computes `path("M 0 0 L 10 10")`); anything else as written.
fn offset_path(specified: &str) -> Option<String> {
    let t = specified.trim();
    let inner = t.strip_prefix("path(").or_else(|| t.strip_prefix("PATH("))?.strip_suffix(')')?;
    let (rule, data) = match inner.rsplit_once(',') {
        Some((r, d)) if matches!(r.trim(), "nonzero" | "evenodd") => (Some(r.trim()), d.trim()),
        _ => (None, inner.trim()),
    };
    let data = data.strip_prefix('"').and_then(|d| d.strip_suffix('"')).or_else(|| data.strip_prefix('\'').and_then(|d| d.strip_suffix('\'')))?;
    let path = path_data(data)?;
    Some(match rule {
        Some(r) => format!("path({r}, \"{path}\")"),
        None => format!("path(\"{path}\")"),
    })
}

/// SVG path data as Blink's `SVGPathStringBuilder` writes it: every segment its command letter -- an implied
/// repeat written out, a moveto's as a lineto -- and its numbers, all one space apart. `None` when the data does
/// not parse (the declaration is then not Chrome's either).
fn path_data(d: &str) -> Option<String> {
    let b = d.as_bytes();
    let mut i = 0;
    let skip = |i: &mut usize| {
        while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
            *i += 1;
        }
    };
    let number = |i: &mut usize| -> Option<f32> {
        let start = *i;
        if *i < b.len() && (b[*i] == b'+' || b[*i] == b'-') {
            *i += 1;
        }
        let digits = |i: &mut usize| {
            let s = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            *i > s
        };
        let int = digits(i);
        let frac = if *i < b.len() && b[*i] == b'.' {
            *i += 1;
            digits(i)
        } else {
            false
        };
        if !int && !frac {
            return None;
        }
        if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
            let save = *i;
            *i += 1;
            if *i < b.len() && (b[*i] == b'+' || b[*i] == b'-') {
                *i += 1;
            }
            if !digits(i) {
                *i = save;
            }
        }
        std::str::from_utf8(&b[start..*i]).ok()?.parse::<f32>().ok().filter(|v| v.is_finite())
    };
    let flag = |i: &mut usize| -> Option<f32> {
        match b.get(*i) {
            Some(b'0') => {
                *i += 1;
                Some(0.0)
            }
            Some(b'1') => {
                *i += 1;
                Some(1.0)
            }
            _ => None,
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut cmd: Option<u8> = None;
    loop {
        skip(&mut i);
        if i >= b.len() {
            break;
        }
        let c = if b[i].is_ascii_alphabetic() {
            let c = b[i];
            i += 1;
            if out.is_empty() && c != b'M' && c != b'm' {
                return None;
            }
            c
        } else {
            match cmd? {
                b'Z' | b'z' => return None,
                b'M' => b'L',
                b'm' => b'l',
                c => c,
            }
        };
        let arity = match c {
            b'Z' | b'z' => 0,
            b'H' | b'h' | b'V' | b'v' => 1,
            b'M' | b'm' | b'L' | b'l' | b'T' | b't' => 2,
            b'S' | b's' | b'Q' | b'q' => 4,
            b'C' | b'c' => 6,
            b'A' | b'a' => 7,
            _ => return None,
        };
        let mut seg = vec![(c as char).to_string()];
        for k in 0..arity {
            skip(&mut i);
            let v = if (c == b'A' || c == b'a') && (k == 3 || k == 4) { flag(&mut i)? } else { number(&mut i)? };
            seg.push(serialize_number(v));
        }
        out.push(seg.join(" "));
        cmd = Some(c);
    }
    if out.is_empty() { None } else { Some(out.join(" ")) }
}

/// `position-try-fallbacks` as Blink writes it: an item's dashed ident before its tactics, a `<position-area>`'s
/// keywords horizontal before vertical (`top left` is `left top`) and block before inline, a logical pair in its
/// short form (`inline-end block-start` is `start end`).
fn position_try_fallbacks(specified: &str) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("none") {
        return None;
    }
    const Y: &[&str] = &["top", "bottom", "span-top", "span-bottom", "y-start", "y-end", "span-y-start", "span-y-end", "self-y-start", "self-y-end", "span-self-y-start", "span-self-y-end"];
    let items: Vec<String> = top_level_items(t)
        .iter()
        .map(|item| {
            let mut words: Vec<String> = item.split_whitespace().map(str::to_string).collect();
            if words.iter().any(|w| w.starts_with("--")) {
                words.sort_by_key(|w| !w.starts_with("--"));
                return words.join(" ");
            }
            if let [a, b] = words.as_slice() {
                let (al, bl) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
                if Y.contains(&al.as_str()) && !Y.contains(&bl.as_str()) {
                    return format!("{b} {a}");
                }
                let logical = |w: &str| w.contains("block-") || w.contains("inline-");
                if logical(&al) && logical(&bl) {
                    let (block, inline) = if al.contains("block-") { (al, bl) } else { (bl, al) };
                    let short = |w: &str| w.replace("block-", "").replace("inline-", "");
                    return format!("{} {}", short(&block), short(&inline));
                }
            }
            words.join(" ")
        })
        .collect();
    Some(items.join(", "))
}

/// `<position>` with its keywords as the percentages they are (`left top` is `0% 0%`, `center` `50% 50%`) --
/// `offset-position` and `offset-anchor` compute so (Chrome 150); `normal` and `auto` stay.
fn position_percentages(specified: &str) -> Option<String> {
    let words: Vec<&str> = specified.split_whitespace().collect();
    if words.len() == 1 && matches!(words[0].to_ascii_lowercase().as_str(), "normal" | "auto") {
        return None;
    }
    let pct = |w: &str| match w.to_ascii_lowercase().as_str() {
        "left" | "top" => Some("0%"),
        "center" => Some("50%"),
        "right" | "bottom" => Some("100%"),
        _ => None,
    };
    let (x, y) = match words.as_slice() {
        [a] if matches!(a.to_ascii_lowercase().as_str(), "top" | "bottom") => ("50%".to_string(), pct(a)?.to_string()),
        [a] => (pct(a).map_or_else(|| a.to_string(), str::to_string), "50%".to_string()),
        [a, b] if matches!(a.to_ascii_lowercase().as_str(), "top" | "bottom") || matches!(b.to_ascii_lowercase().as_str(), "left" | "right") => {
            (pct(b).map_or_else(|| b.to_string(), str::to_string), pct(a).map_or_else(|| a.to_string(), str::to_string))
        }
        [a, b] => (pct(a).map_or_else(|| a.to_string(), str::to_string), pct(b).map_or_else(|| b.to_string(), str::to_string)),
        _ => return None,
    };
    Some(format!("{x} {y}"))
}

/// A computed `<image>` list: each gradient's colours computed (`red` is `rgb(255, 0, 0)`, `currentcolor` the
/// element's colour), its lengths in pixels, its angle in degrees, and the defaults Blink leaves out left out --
/// a linear gradient's `to bottom` / `180deg`, a radial or conic one's `at center` (Chrome 150).
fn computed_images(specified: &str, ctx: &Context) -> Option<String> {
    if !specified.contains("gradient(") {
        return None;
    }
    let items = top_level_items(specified);
    Some(items.iter().map(|i| computed_image(i.trim(), ctx)).collect::<Vec<_>>().join(", "))
}

fn computed_image(item: &str, ctx: &Context) -> String {
    let Some(open) = item.find('(') else { return item.to_string() };
    let name = item[..open].trim().to_ascii_lowercase();
    if !name.ends_with("gradient") || !item.ends_with(')') {
        return item.to_string();
    }
    let args: Vec<String> = top_level_items(&item[open + 1..item.len() - 1]).iter().map(|a| a.trim().to_string()).collect();
    let webkit = name.starts_with("-webkit-");
    let mut out: Vec<String> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let words: Vec<&str> = top_level_words(arg);
        let is_stop = color(words.first().copied().unwrap_or(""), ctx).is_some() || (words.len() == 1 && shorthand_lp(words[0]));
        if i == 0 && !is_stop {
            // the gradient's configuration: direction, shape, position
            if webkit {
                out.push(arg.clone());
                continue;
            }
            let lower = arg.to_ascii_lowercase();
            let config = if name.contains("linear") {
                match angle_degrees(&lower) {
                    Some(d) if (d - 180.0).abs() < 1e-9 => None,
                    Some(d) => Some(format!("{}deg", serialize_number(d as f32))),
                    None if lower == "to bottom" => None,
                    None => Some(arg.clone()),
                }
            } else {
                let stripped = lower.replace(" at center center", "").replace(" at center", "").replace("at center", "").trim().to_string();
                let mut words: Vec<String> = Vec::new();
                for w in stripped.split_whitespace() {
                    words.push(match angle_degrees(w) {
                        Some(d) => format!("{}deg", serialize_number(d as f32)),
                        None => lengthy(w, ctx),
                    });
                }
                if words.is_empty() { None } else { Some(words.join(" ")) }
            };
            if let Some(c) = config {
                out.push(c);
            }
            continue;
        }
        // a colour stop (colour and positions) or a hint (a position)
        let mut parts = Vec::new();
        for w in &words {
            parts.push(match color(w, ctx) {
                Some(c) => c,
                // a conic stop's bare `0` is an angle (Chrome 152: `repeating-conic-gradient(#333 0 25%, ...)` reads
                // `rgb(51, 51, 51) 0deg`), where it read `0px`
                None => match angle_degrees(w).or_else(|| (w.trim() == "0").then_some(0.0)) {
                    Some(d) if name.contains("conic") => format!("{}deg", serialize_number(d as f32)),
                    _ => lengthy(w, ctx),
                },
            });
        }
        // a stop with two positions is two stops of its colour (Chrome 152: `repeating-linear-gradient(red 0 10px,
        // blue 10px 20px)` reads `repeating-linear-gradient(rgb(255, 0, 0) 0px, rgb(255, 0, 0) 10px, rgb(0, 0, 255)
        // 10px, rgb(0, 0, 255) 20px)`)
        if parts.len() == 3 && color(words[0], ctx).is_some() {
            out.push(format!("{} {}", parts[0], parts[1]));
            out.push(format!("{} {}", parts[0], parts[2]));
            continue;
        }
        out.push(parts.join(" "));
    }
    format!("{name}({})", out.join(", "))
}

/// A shadow list as Blink computes it (`CSSShadowValue` from `ShadowData`): each shadow's colour first --
/// `currentcolor` when none is written -- then its offsets, blur and (a box's) spread in pixels, the omitted ones
/// `0px`, then `inset`. Chrome 152 (`font-size: 20px; color: rgb(10, 20, 30)`): `box-shadow: 0 0 10px red` reads
/// `rgb(255, 0, 0) 0px 0px 10px 0px`, `inset 1em 1em currentcolor` `rgb(10, 20, 30) 20px 20px 0px 0px inset`,
/// `text-shadow: 1em 0 hsl(0 100% 50%)` `rgb(255, 0, 0) 20px 0px 0px`. `currentcolor` stays the keyword, read as the
/// READING element's `color` (`computed::current_color_in`): it inherits as itself, and a `color` change is no change
/// of the shadow -- Chrome 152 starts no `box-shadow` transition for it (`ctr.html`, 2026-10-23), where the colour
/// baked in here started one and an inherited `text-shadow` read its parent's colour (`c1rev.html`). `None` for a
/// value of another shape, which stays as written.
fn shadow_list(specified: &str, ctx: &Context, spread: bool) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("none") {
        return None;
    }
    let shadows: Option<Vec<String>> = top_level_items(t).iter().map(|item| shadow(item, ctx, spread, true)).collect();
    Some(shadows?.join(", "))
}

/// One shadow of [`shadow_list`] (`drop-shadow()`'s too, which takes neither a spread nor `inset`).
fn shadow(item: &str, ctx: &Context, spread: bool, may_inset: bool) -> Option<String> {
    let resolve = ctx.lengths();
    let (mut colour, mut inset, mut lengths) = (None, false, Vec::new());
    for w in top_level_words(item.trim()) {
        if may_inset && w.eq_ignore_ascii_case("inset") && !inset {
            inset = true;
        } else if colour.is_none() && w.eq_ignore_ascii_case(super::computed::CURRENT_COLOR) {
            colour = Some(super::computed::CURRENT_COLOR.to_string());
        } else if let Some(c) = (colour.is_none() && !numeric(w)).then(|| color(w, ctx)).flatten() {
            colour = Some(c);
        } else {
            let mut changed = false;
            let px = absolutized_word(w, &resolve, Numbers::Zero, &mut changed);
            if !plain_px(&px) && !shorthand_lp(&px) {
                return None;
            }
            lengths.push(px);
        }
    }
    let most = if spread { 4 } else { 3 };
    if !(2..=most).contains(&lengths.len()) {
        return None;
    }
    lengths.resize(most, "0px".to_string());
    let colour = colour.unwrap_or_else(|| super::computed::CURRENT_COLOR.to_string());
    Some(format!("{colour} {}{}", lengths.join(" "), if inset { " inset" } else { "" }))
}

/// A `filter` / `backdrop-filter` list as Blink computes it (`ComputedStyleUtils::ValueForFilter`): `blur()` in
/// pixels, the amount functions as numbers (a percentage its fraction, an empty one its default `1`), `hue-rotate()`
/// in degrees, `drop-shadow()` colour first. Chrome 152 (`font-size: 20px`): `blur(1em) brightness(50%)` reads
/// `blur(20px) brightness(0.5)`, `blur()` `blur(0px)`, `hue-rotate(0.5turn)` `hue-rotate(180deg)`, `hue-rotate(0)`
/// `hue-rotate(0deg)`, `drop-shadow(1px 1px red)` `drop-shadow(rgb(255, 0, 0) 1px 1px 0px)`. A function this does not
/// know (`url()`) stays as written.
fn filter_list(specified: &str, ctx: &Context) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("none") || !t.contains('(') {
        return None;
    }
    let resolve = ctx.lengths();
    let mut out = Vec::new();
    for w in top_level_words(t) {
        let Some(open) = w.find('(').filter(|_| w.ends_with(')')) else { return None };
        let (name, arg) = (w[..open].to_ascii_lowercase(), w[open + 1..w.len() - 1].trim());
        let v = match name.as_str() {
            "blur" if arg.is_empty() => "0px".to_string(),
            "blur" => {
                let mut changed = false;
                absolutized_word(arg, &resolve, Numbers::Zero, &mut changed)
            }
            "brightness" | "contrast" | "grayscale" | "invert" | "opacity" | "saturate" | "sepia" => {
                if arg.is_empty() {
                    "1".to_string()
                } else if let Some(n) = arg.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()) {
                    serialize_number(n / 100.0)
                } else if let Ok(n) = arg.parse::<f32>() {
                    serialize_number(n)
                } else {
                    arg.to_string()
                }
            }
            "hue-rotate" if arg.is_empty() || arg == "0" => "0deg".to_string(),
            "hue-rotate" => match angle_degrees(arg) {
                Some(d) => format!("{}deg", serialize_number(d as f32)),
                None => arg.to_string(),
            },
            "drop-shadow" => shadow(arg, ctx, false, false)?,
            _ => {
                out.push(w.to_string());
                continue;
            }
        };
        out.push(format!("{name}({v})"));
    }
    Some(out.join(" "))
}

/// `accent-color`: `auto`, or a colour computed as any other (`currentcolor` kept, to be read as the element's
/// `color`). Chrome 152: `red` reads `rgb(255, 0, 0)`, `currentcolor` the element's `rgb(10, 20, 30)`.
fn accent_color(specified: &str, ctx: &Context) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Some("auto".to_string());
    }
    if t.eq_ignore_ascii_case("currentcolor") {
        return Some(super::computed::CURRENT_COLOR.to_string());
    }
    color(t, ctx)
}

/// A `<easing-function>` list as Blink computes it (`CSSTimingData` back to text): `step-start` is `steps(1, start)`,
/// `step-end` and `steps(n, end)` / `steps(n, jump-end)` are `steps(n)`, a `cubic-bezier()`'s numbers as Blink writes
/// numbers, and a `linear()` with every stop's position filled in (CSS Easing 2's canonical control points). Chrome
/// 152 (`kw.html`, 2026-10-23): `STEPS(3, END)` reads `steps(3)`, `linear(0, 0.2, 0.5 60%, 0.7, 1)` `linear(0 0%, 0.2
/// 30%, 0.5 60%, 0.7 80%, 1 100%)`, `linear(0, 1.5 20% 40%, 0.25)` `linear(0 0%, 1.5 20%, 1.5 40%, 0.25 100%)`,
/// `linear(0 10%, 1 5%)` `linear(0 10%, 1 10%)`. Anything else stays as written.
fn easing_list(specified: &str) -> Option<String> {
    let items: Vec<String> = top_level_items(specified).iter().map(|i| easing(i.trim())).collect::<Option<_>>()?;
    Some(items.join(", "))
}

fn easing(item: &str) -> Option<String> {
    let lower = item.to_ascii_lowercase();
    let n = |t: &str| t.trim().parse::<f64>().ok().filter(|v| v.is_finite());
    let fmt = super::serialize::format_number;
    Some(match lower.as_str() {
        "step-start" => "steps(1, start)".to_string(),
        "step-end" => "steps(1)".to_string(),
        "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out" => lower,
        _ => {
            let open = lower.find('(').filter(|_| lower.ends_with(')'))?;
            let (name, args) = (&lower[..open], &lower[open + 1..lower.len() - 1]);
            let args: Vec<&str> = args.split(',').map(str::trim).collect();
            match name {
                "steps" => match args.as_slice() {
                    [count] | [count, "end" | "jump-end"] => format!("steps({})", fmt(n(count)?)),
                    [count, position] => format!("steps({}, {position})", fmt(n(count)?)),
                    _ => return None,
                },
                "cubic-bezier" if args.len() == 4 => {
                    format!("cubic-bezier({})", args.iter().map(|a| n(a).map(fmt)).collect::<Option<Vec<_>>>()?.join(", "))
                }
                "linear" => linear_easing(&args)?,
                _ => return None,
            }
        }
    })
}

/// `linear()`'s stops with every position filled in: the first at 0% and the last at 100% when they have none, a
/// stop with two positions as two stops, no position behind an earlier one, and the stops between two positioned
/// ones spread evenly between them.
fn linear_easing(args: &[&str]) -> Option<String> {
    let mut stops: Vec<(f64, Option<f64>)> = Vec::new();
    for a in args {
        let words: Vec<&str> = a.split_ascii_whitespace().collect();
        let value = words.first()?.parse::<f64>().ok().filter(|v| v.is_finite())?;
        let pct = |w: &str| w.strip_suffix('%').and_then(|p| p.parse::<f64>().ok()).filter(|v| v.is_finite());
        match words.len() {
            1 => stops.push((value, None)),
            2 => stops.push((value, Some(pct(words[1])?))),
            3 => {
                stops.push((value, Some(pct(words[1])?)));
                stops.push((value, Some(pct(words[2])?)));
            }
            _ => return None,
        }
    }
    if stops.len() < 2 {
        return None;
    }
    let last = stops.len() - 1;
    stops[0].1.get_or_insert(0.0);
    stops[last].1.get_or_insert(100.0);
    let mut largest = f64::NEG_INFINITY;
    for s in stops.iter_mut() {
        if let Some(p) = s.1.as_mut() {
            *p = p.max(largest);
            largest = *p;
        }
    }
    let mut i = 0;
    while i < stops.len() {
        if stops[i].1.is_none() {
            let start = i - 1;
            let end = (i..stops.len()).find(|&j| stops[j].1.is_some())?;
            let (from, to) = (stops[start].1?, stops[end].1?);
            for (k, j) in (i..end).enumerate() {
                stops[j].1 = Some(from + (to - from) * (k + 1) as f64 / (end - start) as f64);
            }
            i = end;
        }
        i += 1;
    }
    let fmt = super::serialize::format_number;
    Some(format!("linear({})", stops.iter().map(|(v, p)| format!("{} {}%", fmt(*v), fmt(p.unwrap_or(0.0)))).collect::<Vec<_>>().join(", ")))
}

/// A `min-*` / `max-*` size's prefixed sizing keyword computes to the standard one (Chrome 152: `min-width:
/// -webkit-fill-available` reads `stretch`, `max-block-size: -webkit-max-content` `max-content`).
fn sizing_alias(specified: &str) -> Option<String> {
    Some(
        match specified.trim().to_ascii_lowercase().as_str() {
            "-webkit-fill-available" => "stretch",
            "-webkit-min-content" => "min-content",
            "-webkit-max-content" => "max-content",
            "-webkit-fit-content" => "fit-content",
            _ => return None,
        }
        .to_string(),
    )
}

/// An alignment's `first baseline` is `baseline` (Chrome 152: `align-items: first  baseline` and `justify-self: FIRST
/// BASELINE` read `baseline`; `last baseline` stays).
fn first_baseline(specified: &str) -> Option<String> {
    let words: Vec<String> = specified.split_ascii_whitespace().map(str::to_ascii_lowercase).collect();
    (words.len() == 2 && words[0] == "first" && words[1] == "baseline").then(|| "baseline".to_string())
}

/// A grid line as Blink writes it: `span` first, then the integer, then the name (Chrome 152: `grid-row-start: c 1` reads
/// `1 c`, `span c 2` `span 2 c`, `2 span` `span 2`). `None` for anything else.
fn grid_line(specified: &str) -> Option<String> {
    let words: Vec<&str> = specified.split_ascii_whitespace().collect();
    if !(2..=3).contains(&words.len()) {
        return None;
    }
    let span = words.iter().any(|w| w.eq_ignore_ascii_case("span"));
    let integer = words.iter().find(|w| w.parse::<i32>().is_ok());
    let name = words.iter().find(|w| !w.eq_ignore_ascii_case("span") && w.parse::<i32>().is_err());
    if words.len() != usize::from(span) + usize::from(integer.is_some()) + usize::from(name.is_some()) {
        return None;
    }
    let out: Vec<&str> = [span.then_some("span"), integer.copied(), name.copied()].into_iter().flatten().collect();
    Some(out.join(" "))
}

/// A lone number in `aspect-ratio` is a ratio over 1 (Chrome 152: `1.5` reads `1.5 / 1`, `auto 2` `auto 2 / 1`).
fn aspect_ratio(specified: &str) -> Option<String> {
    let words: Vec<&str> = specified.split_ascii_whitespace().collect();
    let (auto, number) = match words.as_slice() {
        [n] => (false, *n),
        [a, n] | [n, a] if a.eq_ignore_ascii_case("auto") => (true, *n),
        _ => return None,
    };
    let n = number.parse::<f32>().ok().filter(|n| n.is_finite())?;
    Some(format!("{}{} / 1", if auto { "auto " } else { "" }, serialize_number(n)))
}

/// `rotate`'s angle in degrees (Chrome 152: `0.5turn` reads `180deg`, `-1rad` `-57.2958deg`; `x 45deg` as written).
fn rotate_degrees(specified: &str) -> Option<String> {
    let words: Vec<&str> = specified.split_ascii_whitespace().collect();
    let last = words.last()?;
    if last.to_ascii_lowercase().ends_with("deg") {
        return None;
    }
    let d = angle_degrees(last)?;
    let mut out: Vec<String> = words[..words.len() - 1].iter().map(|w| w.to_string()).collect();
    out.push(format!("{}deg", serialize_number(d as f32)));
    Some(out.join(" "))
}

/// A number or a percentage as the number it is (`25%` is `0.25`).
fn fraction(t: &str) -> Option<f32> {
    let t = t.trim();
    match t.strip_suffix('%') {
        Some(p) => p.trim().parse::<f32>().ok().map(|v| v / 100.0),
        None => t.parse::<f32>().ok(),
    }
    .filter(|v| v.is_finite())
}

/// `scale`'s factors as numbers, a third of 1 left out and then a second equal to the first (Chrome 152: `25%` reads
/// `0.25`, `50% 50%` `0.5`, `2 2` and `2 2 1` `2`, `50% 25%` `0.5 0.25`, `1 1 0%` `1 1 0`, `1 2` as is).
fn scale_numbers(specified: &str) -> Option<String> {
    let words: Vec<&str> = specified.split_ascii_whitespace().collect();
    if words.is_empty() || words.len() > 3 {
        return None;
    }
    let mut n: Vec<String> = words.iter().map(|w| fraction(w).map(serialize_number)).collect::<Option<_>>()?;
    if n.len() == 3 && n[2] == "1" {
        n.pop();
    }
    if n.len() == 2 && n[0] == n[1] {
        n.pop();
    }
    Some(n.join(" "))
}

/// `zoom` as a number: a percentage its fraction, `normal` and `0` the factor 1 (Chrome 152: `25%` reads `0.25`,
/// `normal` `1`, `0` `1`).
fn zoom_number(specified: &str) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("normal") {
        return Some("1".to_string());
    }
    let n = fraction(t)?;
    Some(if n == 0.0 { "1".to_string() } else { serialize_number(n) })
}

/// A keyword list Blink computes to its canonical spelling (Chrome 152, the sweep and `canon.html`):
/// `scroll-snap-type: y proximity` reads `y` (`proximity` is the default strictness), `text-emphasis-position: over
/// right` `over` (`right` the default side), `color-scheme: only dark light` `dark light only`, `contain: layout paint
/// style` (any order) `content` and `size layout paint style` `strict`. `None` for anything else.
fn keyword_canonical(property: PropertyId, specified: &str) -> Option<String> {
    let words: Vec<String> = specified.split_ascii_whitespace().map(str::to_ascii_lowercase).collect();
    let has = |k: &str| words.iter().any(|w| w == k);
    match property {
        PropertyId::ScrollSnapType if words.len() == 2 && words[1] == "proximity" => Some(words[0].clone()),
        // `right` is the default side: `over right` and `right over` read `over`, `under right` `under`
        PropertyId::TextEmphasisPosition if words.len() == 2 && has("right") && (has("over") || has("under")) => {
            Some(if has("over") { "over" } else { "under" }.to_string())
        }
        PropertyId::ColorScheme if words.len() >= 2 && words[0] == "only" => {
            let mut v: Vec<String> = specified.split_ascii_whitespace().skip(1).map(str::to_string).collect();
            v.push("only".to_string());
            Some(v.join(" "))
        }
        PropertyId::Contain if words.len() == 3 && has("layout") && has("paint") && has("style") => Some("content".to_string()),
        PropertyId::Contain if words.len() == 4 && has("size") && has("layout") && has("paint") && has("style") => {
            Some("strict".to_string())
        }
        _ => None,
    }
}

/// `counter-reset` / `counter-set` / `counter-increment`: every counter with its integer, the omitted ones `default`
/// (0 to reset or set, 1 to increment). Chrome 152: `counter-reset: c` reads `c 0`, `counter-increment: a b 2` `a 1 b
/// 2`, `counter-set: list-item` `list-item 0`. `None` -- as written -- for `none` and for a value with anything but
/// names and integers in it (`reversed(c)`).
fn counter_list(specified: &str, default: i32) -> Option<String> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("none") {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let mut pending: Option<&str> = None;
    for w in t.split_ascii_whitespace() {
        if let Ok(n) = w.parse::<i32>() {
            out.push(format!("{} {n}", pending.take()?));
        } else {
            if !w.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii()) || w.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            if let Some(name) = pending.replace(w) {
                out.push(format!("{name} {default}"));
            }
        }
    }
    if let Some(name) = pending {
        out.push(format!("{name} {default}"));
    }
    (!out.is_empty()).then(|| out.join(" "))
}

/// A value's words at its top-level spaces (`rgb(255, 0, 0) 10%` is two).
fn top_level_words(v: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, None::<usize>, Vec::new());
    for (i, c) in v.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&v[s..i]);
                }
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

fn shorthand_lp(t: &str) -> bool {
    crate::style::shorthand::is_length_percentage(t)
}

/// A word that starts as a number does (a digit, a `.`, a sign before either), which no colour does: a shadow's
/// lengths are not offered to the colour parser (a parse each, 2026-10-23).
fn numeric(w: &str) -> bool {
    let b = w.as_bytes();
    let starts = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b'.');
    starts(0) || (matches!(b.first(), Some(b'-' | b'+')) && starts(1))
}

/// One pixel length written as `serialize_px` writes it (`12px`, `-0.5px`): a `<length-percentage>` without a parse.
fn plain_px(w: &str) -> bool {
    numeric(w) && w.ends_with("px") && !w.contains('(') && settled(w, Numbers::Keep)
}

/// A value's items at its top-level commas (a function's own commas stay inside it).
fn top_level_items(v: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, 0usize, Vec::new());
    for (i, c) in v.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&v[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&v[start..]);
    out
}

/// `font-weight` computes to a number. `bolder` and `lighter` are relative to the parent's, in the
/// steps CSS Fonts 4 §2.2.1 defines — not simply +/-100, which is why they are a table.
pub fn font_weight(specified: &str, parent: f32) -> Option<f32> {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("normal") {
        return Some(400.0);
    }
    if t.eq_ignore_ascii_case("bold") {
        return Some(700.0);
    }
    if t.eq_ignore_ascii_case("bolder") {
        return Some(match parent {
            w if w < 350.0 => 400.0,
            w if w < 550.0 => 700.0,
            w if w < 900.0 => 900.0,
            _ => 900.0,
        });
    }
    if t.eq_ignore_ascii_case("lighter") {
        return Some(match parent {
            w if w < 550.0 => 100.0,
            w if w < 750.0 => 400.0,
            _ => 700.0,
        });
    }
    t.parse::<f32>().ok().map(|w| w.clamp(1.0, 1000.0))
}

/// `line-height`: `normal` stays, a bare number stays a number (it is inherited as a *factor*, so
/// resolving it to pixels here would freeze the wrong value into a child with a different font),
/// and a length or percentage becomes pixels against this element's font size.
fn line_height(specified: &str, ctx: &Context) -> String {
    let t = specified.trim();
    if t.eq_ignore_ascii_case("normal") {
        return "normal".to_string();
    }
    if let Ok(n) = t.parse::<f32>() {
        return serialize_number(n);
    }
    // a length-percentage math function: its percentage is of the font size (Chrome 150: `calc(100% + 2px)` at
    // 16px is `18px`)
    if super::math::is_math(t) {
        if let Some(n) = super::math::reduce(t, &ctx.lengths()) {
            return serialize_px(n.eval(ctx.computed_font_size));
        }
    }
    let mut pi = ParserInput::new(t);
    let mut p = Parser::new(&mut pi);
    match LengthPercentage::parse(&mut p) {
        Ok(lp) if p.expect_exhausted().is_ok() => serialize_px(lp.to_px(&ctx.lengths(), ctx.computed_font_size)),
        _ => t.to_string(),
    }
}

/// Blink's absolute-size keyword tables (`font_size_functions.cc`, `kStrictFontSizeTable` and
/// `kQuirksFontSizeTable`), the rows for Chrome's default font size (16) and default fixed font size (13):
/// `xx-small` through `xxx-large`. NOT the CSS spec's scaling factors -- Chrome 150 computes `small` as
/// 13px where 8/9 of 16 is 14.22, `large` as 18 where 6/5 is 19.2, and a `monospace` element's `medium`
/// as 13 (measured).
const STRICT_16: [f32; 8] = [9.0, 10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
const STRICT_13: [f32; 8] = [9.0, 10.0, 12.0, 13.0, 16.0, 20.0, 26.0, 39.0];
const QUIRKS_16: [f32; 8] = [9.0, 10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
const QUIRKS_13: [f32; 8] = [9.0, 9.0, 10.0, 13.0, 16.0, 20.0, 26.0, 40.0];

/// Chrome's default font size and default FIXED font size (`prefs::kWebKitDefaultFontSize` 16,
/// `kWebKitDefaultFixedFontSize` 13): the second is a bare `monospace` family's `medium`.
pub const DEFAULT_FONT_SIZE: f32 = 16.0;
pub const DEFAULT_FIXED_FONT_SIZE: f32 = 13.0;

/// The absolute-size keyword `t` names, 1 (`xx-small`) to 8 (`xxx-large`).
fn size_keyword(t: &str) -> Option<u8> {
    Some(match t.to_ascii_lowercase().as_str() {
        "xx-small" => 1,
        "x-small" => 2,
        "small" => 3,
        "medium" => 4,
        "large" => 5,
        "x-large" => 6,
        "xx-large" => 7,
        "xxx-large" => 8,
        _ => return None,
    })
}

/// `FontSizeFunctions::FontSizeForKeyword`: keyword `k` (1-8) for a bare `monospace` family or another.
pub fn keyword_font_size(k: u8, monospace: bool, quirks: bool) -> f32 {
    let row = match (monospace, quirks) {
        (false, false) => &STRICT_16,
        (true, false) => &STRICT_13,
        (false, true) => &QUIRKS_16,
        (true, true) => &QUIRKS_13,
    };
    row[(k.clamp(1, 8) - 1) as usize]
}

/// What Blink's `FontDescription::Size` carries down the tree beside the computed `font-size`:
/// the SPECIFIED size, the keyword it came from, and whether it was written in an
/// absolute unit anywhere up the chain -- plus whether the family is a bare `monospace`. Three of Chrome's
/// rules need them, and the computed px alone cannot answer them:
///
/// * a family change to or from a bare `monospace` RE-SIZES a size that was not absolute
///   (`FontBuilder::CheckForGenericFamilyChange`): a keyword size is looked up again in the fixed font's
///   row, any other scales by 13/16 -- `<code>` under the default 16px is 13px, under `font-size: 1.25em`
///   16.25px, under `font-size: 20px` 20px;
/// * the minimum LOGICAL font size (10px under a Korean UI, 6px otherwise) applies only to a size that was
///   not absolute (`GetComputedSizeFromSpecifiedSize`'s smart minimum): `0.5em` computes 10px, `5px` 5px;
/// * `em` everywhere means the SPECIFIED size, not the minimum's (`CSSToLengthConversionData::FontSizes`).
///
/// Kept in the inherited half under [`FontSizeState::KEY`], a name no stylesheet can write.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontSizeState {
    pub specified: f32,
    /// 1 (`xx-small`) to 8 (`xxx-large`), else 0.
    pub keyword: u8,
    pub absolute: bool,
    /// The family list is the bare generic `monospace` (`FontDescription::IsMonospace`).
    pub monospace: bool,
}

impl FontSizeState {
    /// The document's initial font (`FontBuilder::InitialSize`): `medium`, in no monospace family.
    pub const ROOT: FontSizeState = FontSizeState { specified: DEFAULT_FONT_SIZE, keyword: 4, absolute: false, monospace: false };
    pub const KEY: &'static str = "\u{0}font-size";

    pub fn encode(&self) -> String {
        format!("{} {} {} {}", self.specified, self.keyword, self.absolute as u8, self.monospace as u8)
    }

    pub fn decode(s: &str) -> Option<FontSizeState> {
        let mut it = s.split(' ');
        let specified = it.next()?.parse().ok()?;
        let keyword = it.next()?.parse().ok()?;
        let absolute = it.next()? == "1";
        let monospace = it.next()? == "1";
        Some(FontSizeState { specified, keyword, absolute, monospace })
    }

    /// The computed size: the minimum logical font size for a size that is not absolute
    /// (`FontSizeFunctions::GetComputedSizeFromSpecifiedSize` at zoom 1, Chrome's hard minimum being 0).
    pub fn computed(&self, min_logical: f32) -> f32 {
        if self.specified.abs() < f32::EPSILON {
            return 0.0;
        }
        if self.specified < min_logical && !self.absolute {
            return min_logical;
        }
        self.specified
    }
}

/// Is `family` (a computed `font-family`) the bare generic `monospace` -- one family, unquoted?
pub fn is_bare_monospace(family: &str) -> bool {
    family.trim().eq_ignore_ascii_case("monospace")
}

/// `font-size: specified` against the parent's state, as Blink's `ConvertFontSize` turns it into a
/// `FontDescription::Size`: a keyword is the keyword (its px looked up by the caller, once the family is
/// known); `larger`/`smaller` the parent's specified size times or over 1.2; a percentage or a
/// font-relative length the parent's specified size scaled, as absolute as the parent's; `rem`, any
/// other unit and any math function absolute. `monospace` is left as the parent's.
pub fn font_size_state(specified: &str, parent: &FontSizeState, root_px: f32, viewport: (f32, f32)) -> FontSizeState {
    let t = specified.trim();
    let sized = |specified: f32, absolute: bool| FontSizeState { specified, keyword: 0, absolute, monospace: parent.monospace };
    if let Some(k) = size_keyword(t) {
        return FontSizeState { specified: 0.0, keyword: k, absolute: false, monospace: parent.monospace };
    }
    if t.eq_ignore_ascii_case("larger") {
        return sized(parent.specified * 1.2, parent.absolute);
    }
    if t.eq_ignore_ascii_case("smaller") {
        return sized(parent.specified / 1.2, parent.absolute);
    }
    if t.eq_ignore_ascii_case("math") {
        return sized(parent.specified, parent.absolute);
    }
    let ctx = ResolveContext::new(parent.specified, root_px, viewport.0, viewport.1);
    // a math function is absolute whatever it is written in (Blink's `IsMathFunctionValue()`, crbug.com/979895)
    if super::math::is_math(t) {
        return match super::math::reduce(t, &ctx) {
            Some(n) => sized(n.px().unwrap_or_else(|| super::math::resolve_ptr(super::math::intern(n), parent.specified)), true),
            None => sized(parent.specified, parent.absolute),
        };
    }
    let mut pi = ParserInput::new(t);
    let mut p = Parser::new(&mut pi);
    match LengthPercentage::parse(&mut p) {
        Ok(LengthPercentage::Percentage(pct)) if p.expect_exhausted().is_ok() => sized(pct / 100.0 * parent.specified, parent.absolute),
        Ok(LengthPercentage::Length(l)) if p.expect_exhausted().is_ok() => {
            let font_relative = matches!(l.unit, crate::value::LengthUnit::Em | crate::value::LengthUnit::Ex | crate::value::LengthUnit::Ch);
            sized(l.to_px(&ctx), parent.absolute || !font_relative)
        }
        _ => sized(parent.specified, parent.absolute),
    }
}

/// `font-size`, resolved against the parent's: [`font_size_state`] from a parent of `parent_px` written
/// in px, its keyword looked up for a family that is not a bare `monospace`. The cascade goes through
/// the state itself; this is the one-shot answer for a caller with no tree.
pub fn font_size(specified: &str, parent_px: f32, root_px: f32, viewport: (f32, f32)) -> f32 {
    let parent = FontSizeState { specified: parent_px, keyword: 0, absolute: true, monospace: false };
    let s = font_size_state(specified, &parent, root_px, viewport);
    if s.keyword > 0 {
        keyword_font_size(s.keyword, false, false)
    } else {
        s.specified
    }
}

/// Parse a computed `font-size` back out of the string form the arena keeps it in.
pub fn px_of(text: &str) -> Option<f32> {
    let mut pi = ParserInput::new(text.trim());
    let mut p = Parser::new(&mut pi);
    let len = Length::parse(&mut p).ok()?;
    p.expect_exhausted().ok()?;
    Some(len.to_px(&ResolveContext::default()))
}

/// A `<time>` token's value in seconds: `<number>s` or `<number>ms`, any case. `None` for anything else.
fn time_in_seconds(v: &str) -> Option<f64> {
    let lower = v.to_ascii_lowercase();
    let (number, scale) = match lower.strip_suffix("ms") {
        Some(n) => (n, 1000.0),
        None => (lower.strip_suffix('s')?, 1.0),
    };
    let n: f64 = number.parse().ok().filter(|n: &f64| n.is_finite())?;
    Some(n / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::default()
    }

    fn c(property: PropertyId, specified: &str) -> String {
        computed_value(property, specified, &ctx())
    }

    // The three a page reads in one line.
    #[test]
    fn the_computed_value_is_not_the_specified_one() {
        assert_eq!(c(PropertyId::Color, "red"), "rgb(255, 0, 0)");
        assert_eq!(c(PropertyId::MarginTop, "2em"), "32px");
        assert_eq!(c(PropertyId::FontWeight, "bold"), "700");
    }

    #[test]
    fn colours_resolve_however_they_were_written() {
        assert_eq!(c(PropertyId::Color, "#f00"), "rgb(255, 0, 0)");
        assert_eq!(c(PropertyId::BackgroundColor, "hsl(120 100% 50%)"), "rgb(0, 255, 0)");
        assert_eq!(c(PropertyId::Color, "FieldText"), "rgb(0, 0, 0)", "and the system colours too");
        assert_eq!(c(PropertyId::BackgroundColor, "rgba(1, 2, 3, 0.5)"), "rgba(1, 2, 3, 0.5)");
    }

    /// A `<paint>` the colour parse turns down, with a multi-byte character across byte 4: `fill: "éé"` from a
    /// style attribute or a sheet reaches here unvalidated and aborted the process (2026-10-23).
    #[test]
    fn a_non_ascii_paint_is_left_as_written() {
        assert_eq!(c(PropertyId::Fill, "\"éé\""), "\"éé\"");
        assert_eq!(c(PropertyId::Stroke, "aéé"), "aéé");
        assert_eq!(c(PropertyId::Fill, "url(#a) red"), "url(#a) rgb(255, 0, 0)");
    }

    // `currentcolor` computes to itself (the arena reads it as the element's `color`); inside a function it is
    // resolved here.
    #[test]
    fn current_color_computes_to_itself() {
        let ctx = Context { color: Rgba::opaque(1, 2, 3), ..Context::default() };
        assert_eq!(computed_value(PropertyId::BorderTopColor, "currentColor", &ctx), "currentcolor");
        assert_eq!(computed_value(PropertyId::Fill, "currentcolor", &ctx), "currentcolor");
        assert_eq!(computed_value(PropertyId::BorderTopColor, "color-mix(in srgb, currentcolor 50%, rgb(3, 4, 5))", &ctx), "color(srgb 0.00784314 0.0117647 0.0156863)");
    }

    #[test]
    fn lengths_become_pixels_against_the_right_font() {
        let ctx = Context { font_size: 20.0, root_font_size: 16.0, ..Context::default() };
        assert_eq!(computed_value(PropertyId::MarginTop, "2em", &ctx), "40px");
        assert_eq!(computed_value(PropertyId::MarginTop, "2rem", &ctx), "32px");
        assert_eq!(computed_value(PropertyId::Width, "1in", &ctx), "96px");
        assert_eq!(computed_value(PropertyId::Width, "50vw", &ctx), "512px");
    }

    // A percentage resolves against a containing block layout has not established yet, so every
    // browser reports it as written.
    #[test]
    fn a_percentage_and_a_keyword_are_left_alone() {
        assert_eq!(c(PropertyId::Width, "50%"), "50%");
        assert_eq!(c(PropertyId::Width, "auto"), "auto");
        assert_eq!(c(PropertyId::LetterSpacing, "normal"), "normal");
    }

    // A line width computes to whole pixels, its keywords included (Blink's `ClampLineWidth`; Chrome 150
    // reads `thin` as `1px`, `2.715px` as `2px`, `0.5px` as `1px`).
    #[test]
    fn a_line_width_computes_to_whole_pixels() {
        assert_eq!(c(PropertyId::BorderTopWidth, "thin"), "1px");
        assert_eq!(c(PropertyId::BorderTopWidth, "thick"), "5px");
        assert_eq!(c(PropertyId::OutlineWidth, "2.715px"), "2px");
        assert_eq!(c(PropertyId::ColumnRuleWidth, "0.5px"), "1px");
        assert_eq!(c(PropertyId::BorderLeftWidth, "0px"), "0px");
    }

    // Chrome 152 under `font-size: 20px` (`len.html`, 2026-10-23): a length-valued property kept as written
    // computes its lengths all the same, inside functions too, and a bare number as the property takes it.
    #[test]
    fn a_length_kept_as_written_still_computes_to_pixels() {
        let ctx = Context { font_size: 20.0, root_font_size: 16.0, ..Context::default() };
        let c = |p, v| computed_value(p, v, &ctx);
        assert_eq!(c(PropertyId::ScrollMarginTop, "2em"), "40px");
        assert_eq!(c(PropertyId::ScrollMarginTop, "0"), "0px");
        assert_eq!(c(PropertyId::ScrollPaddingTop, "calc(50% - 2em)"), "calc(50% - 40px)");
        assert_eq!(c(PropertyId::ColumnWidth, "calc(10px + 2em)"), "50px");
        assert_eq!(c(PropertyId::ContainIntrinsicSize, "auto 2em auto 1rem"), "auto 40px auto 16px");
        assert_eq!(c(PropertyId::GridTemplateColumns, "0 0 50px"), "0px 0px 50px");
        assert_eq!(c(PropertyId::GridTemplateColumns, "minmax(0, 1fr)"), "minmax(0px, 1fr)");
        assert_eq!(c(PropertyId::GridTemplateColumns, "repeat(2, 2em)"), "repeat(2, 40px)");
        assert_eq!(c(PropertyId::GridTemplateColumns, "[a] 2em [b c] 1fr"), "[a] 40px [b c] 1fr");
        assert_eq!(c(PropertyId::GridAutoRows, "minmax(1rem,auto)"), "minmax(16px, auto)");
        assert_eq!(c(PropertyId::GridTemplateColumns, "repeat(3,  1fr)"), "repeat(3,  1fr)", "nothing to compute: as written");
        assert_eq!(c(PropertyId::Translate, "2em 1rem 3px"), "40px 16px 3px");
        assert_eq!(c(PropertyId::X, "1.5"), "1.5px");
        assert_eq!(c(PropertyId::X, "calc(2em + 1px)"), "41px");
        assert_eq!(c(PropertyId::Rx, "auto"), "auto");
        assert_eq!(c(PropertyId::StrokeWidth, "10PX"), "10px");
        assert_eq!(c(PropertyId::BaselineShift, "baseline"), "0px");
        assert_eq!(c(PropertyId::TabSize, "0"), "0");
        assert_eq!(c(PropertyId::TabSize, "2em"), "40px");
        assert_eq!(c(PropertyId::BorderImageWidth, "1 2em"), "1 40px");
        assert_eq!(c(PropertyId::BorderImageOutset, "0"), "0");
        assert_eq!(c(PropertyId::Perspective, "none"), "none");
        assert_eq!(c(PropertyId::AnimationRangeStart, "entry 2em"), "entry 40px");
        assert_eq!(c(PropertyId::RowRuleWidth, "2.7px"), "2px");
        assert_eq!(c(PropertyId::OutlineOffset, "0.5px"), "1px");
        assert_eq!(c(PropertyId::OutlineOffset, "-0.5px"), "-1px");
        assert_eq!(c(PropertyId::OutlineOffset, "-2.7px"), "-2px");
        assert_eq!(c(PropertyId::OutlineOffset, "2em"), "40px");
        assert_eq!(c(PropertyId::Transform, "rotate(45deg) translate(10px, 2em)"), "rotate(45deg) translate(10px, 40px)");
        assert_eq!(c(PropertyId::Transform, "translateX(0) scale(1.5)"), "translateX(0) scale(1.5)");
        assert_eq!(c(PropertyId::StrokeDasharray, "calc(10px + 2em)"), "50px");
        assert_eq!(c(PropertyId::StrokeDasharray, "calc(50% - 10px)"), "calc(50% - 10px)");
        assert_eq!(c(PropertyId::StrokeDasharray, "1 2em, 3"), "1px, 40px, 3px");
        assert_eq!(c(PropertyId::TransformOrigin, "2em"), "40px center");
        assert_eq!(c(PropertyId::TransformOrigin, "top"), "center top");
        assert_eq!(c(PropertyId::TransformOrigin, "bottom center"), "center bottom");
        assert_eq!(c(PropertyId::TransformOrigin, "10px 20% 2em"), "10px 20% 40px");
        assert_eq!(c(PropertyId::PerspectiveOrigin, "calc(10px + 2em)"), "50px center");
        assert_eq!(c(PropertyId::PerspectiveOrigin, "left 10px top 2em"), "10px 40px");
        assert_eq!(c(PropertyId::PerspectiveOrigin, "right 20% top 2em"), "80% 40px");
        assert_eq!(c(PropertyId::TransformOrigin, "attr(x)"), "attr(x)", "not a position: as written");
        assert_eq!(c(PropertyId::X, "calc(2 * 3)"), "calc(6px)");
        assert_eq!(c(PropertyId::StrokeDasharray, "calc(2 * 3)"), "calc(6px)");
        assert_eq!(c(PropertyId::StrokeMiterlimit, "calc(2 * 3)"), "6");
        assert_eq!(c(PropertyId::WebkitTextStrokeWidth, "thin"), "0.416667px");
        assert_eq!(c(PropertyId::WebkitTextStrokeWidth, "thick"), "2.08333px");
        assert_eq!(c(PropertyId::CaretColor, "auto"), "currentcolor");
        assert_eq!(c(PropertyId::Resize, "auto"), "both");
    }

    // `settled` says a word is left as written only where the parse leaves it so, in each kind of number.
    #[test]
    fn a_settled_word_is_one_the_parse_leaves_alone() {
        let ctx = Context { font_size: 20.0, root_font_size: 16.0, ..Context::default() };
        let resolve = ctx.lengths();
        let words = [
            "10px", "-5px", "0px", "0", "-0", "0.5px", "-0.25px", ".5px", "+5px", "1.50px", "1e3px", "1E3px", "10PX", "1234567px",
            "123456px", "999999px", "0.123456px", "0.1234567px", "0.00001px", "0.0001px", "-0px", "00px", "01px", "10em", "2rem",
            "1in", "10deg", "0.5turn", "50%", "-5%", "1.5", "2", "0.0", "auto", "none", "1fr", "10.123456px", "100.5px", "3x",
            "5q", "1cqw", "7vh", "-webkit-fill-available", "10px2", "1e", "a", "1\\70x", "1/2", "#fff", "\"x\"",
        ];
        let mut settled_ones = 0;
        for numbers in [Numbers::Px, Numbers::Zero, Numbers::Keep] {
            for w in words {
                let mut changed = false;
                let out = absolutized_token(w, &resolve, numbers, &mut changed);
                if settled(w, numbers) {
                    settled_ones += 1;
                    assert!(!changed && out == w, "{w:?} ({numbers:?}) is not settled: the parse writes {out:?}");
                }
            }
        }
        assert!(settled_ones > 40, "{settled_ones}");
        // the words answered without the parse answer as it does
        for numbers in [Numbers::Px, Numbers::Zero, Numbers::Keep] {
            for w in words.iter().chain(&["2em", "-3rem", "123456vw", "1234567em", "10Q", "7deg", "0", "12in", "5x", "-0em"]) {
                let (mut a, mut b) = (false, false);
                assert_eq!(absolutized_word(w, &resolve, numbers, &mut a), absolutized_token(w, &resolve, numbers, &mut b), "{w:?}");
                assert_eq!(a, b, "{w:?} ({numbers:?})");
            }
        }
        assert!(settled("translate(10px, -2px) rotate(45deg) scale(1.5)", Numbers::Keep));
        assert!(settled("repeat(2, minmax(10px, 1fr)) 25%", Numbers::Zero));
        assert!(!settled("translate(10px, 2em)", Numbers::Keep));
        assert!(!settled("translate(calc(10px), 2px)", Numbers::Keep));
        assert!(!settled("repeat(2, 0)", Numbers::Zero));
        assert!(!settled("url(a.png)", Numbers::Keep));
        // the kept colours are the parse's, `currentcolor` never kept
        let red = Context { color: Rgba::opaque(255, 0, 0), ..ctx };
        for w in ["rgba(0,0,0,.2)", "red", "#0f08", "hsl(120 50% 50%)", "nope", "currentColor", "color-mix(in srgb, currentcolor 50%, blue)"] {
            for _ in 0..2 {
                assert_eq!(color(w, &ctx), color_uncached(w, &ctx), "{w}");
                assert_eq!(color(w, &red), color_uncached(w, &red), "{w}");
            }
        }
        // the fast path changes no answer
        let c = |p, v| computed_value(p, v, &ctx);
        assert_eq!(c(PropertyId::Transform, "translate(1.50px,2em) rotate(45deg)"), "translate(1.5px, 40px) rotate(45deg)");
        assert_eq!(c(PropertyId::Transform, "translate(10px,2px)"), "translate(10px,2px)");
        assert_eq!(c(PropertyId::GridTemplateColumns, "repeat(2,10px) 1em"), "repeat(2, 10px) 20px");
        assert_eq!(c(PropertyId::BoxShadow, "0 1px 5px rgba(0,0,0,.2), inset 0 0 1px red"), "rgba(0, 0, 0, 0.2) 0px 1px 5px 0px, rgb(255, 0, 0) 0px 0px 1px 0px inset");
    }

    // Chrome 152 under `font-size: 20px; color: rgb(10, 20, 30)` (`shadow.html`, 2026-10-23): shadows colour first
    // with every length, filters in their computed units, `accent-color` and gradients' colours computed.
    #[test]
    fn shadows_filters_and_gradients_compute_their_colours_and_lengths() {
        let ctx = Context { font_size: 20.0, root_font_size: 16.0, color: Rgba::opaque(10, 20, 30), ..Context::default() };
        let c = |p, v| computed_value(p, v, &ctx);
        // a shadow with no colour, or `currentcolor`, keeps the keyword: the reading element's `color` resolves it
        assert_eq!(c(PropertyId::BoxShadow, "1px 2px"), "currentcolor 1px 2px 0px 0px");
        assert_eq!(c(PropertyId::BoxShadow, "1px 2px CurrentColor"), "currentcolor 1px 2px 0px 0px");
        assert_eq!(c(PropertyId::Filter, "drop-shadow(1px 1px)"), "drop-shadow(currentcolor 1px 1px 0px)");
        assert_eq!(c(PropertyId::BoxShadow, "inset 1px 2px red"), "rgb(255, 0, 0) 1px 2px 0px 0px inset");
        assert_eq!(c(PropertyId::BoxShadow, "calc(1px + 1em) 0 #fff"), "rgb(255, 255, 255) 21px 0px 0px 0px");
        assert_eq!(
            c(PropertyId::BoxShadow, "0 0 0 1px rgba(0,0,0,.1), 0 2px 4px rgba(0,0,0,.2)"),
            "rgba(0, 0, 0, 0.1) 0px 0px 0px 1px, rgba(0, 0, 0, 0.2) 0px 2px 4px 0px"
        );
        assert_eq!(c(PropertyId::BoxShadow, "none"), "none");
        assert_eq!(c(PropertyId::TextShadow, "1em 0 hsl(0 100% 50%)"), "rgb(255, 0, 0) 20px 0px 0px");
        assert_eq!(c(PropertyId::TextShadow, "0 0 1px currentcolor, 1px 1px red"), "currentcolor 0px 0px 1px, rgb(255, 0, 0) 1px 1px 0px");
        assert_eq!(c(PropertyId::Filter, "blur(1em)"), "blur(20px)");
        assert_eq!(c(PropertyId::Filter, "blur()"), "blur(0px)");
        assert_eq!(c(PropertyId::Filter, "brightness() contrast(0)"), "brightness(1) contrast(0)");
        assert_eq!(c(PropertyId::Filter, "opacity(50%) invert(25%) sepia(1) saturate(200%) grayscale(0.5)"), "opacity(0.5) invert(0.25) sepia(1) saturate(2) grayscale(0.5)");
        assert_eq!(c(PropertyId::Filter, "hue-rotate(0.5turn) hue-rotate(0) hue-rotate(1rad)"), "hue-rotate(180deg) hue-rotate(0deg) hue-rotate(57.2958deg)");
        assert_eq!(c(PropertyId::Filter, "drop-shadow(1px 1px red)"), "drop-shadow(rgb(255, 0, 0) 1px 1px 0px)");
        assert_eq!(c(PropertyId::BackdropFilter, "blur(1rem) drop-shadow(0 0 1px #000)"), "blur(16px) drop-shadow(rgb(0, 0, 0) 0px 0px 1px)");
        assert_eq!(c(PropertyId::Filter, "url(#a) blur(2px)"), "url(#a) blur(2px)");
        assert_eq!(c(PropertyId::Filter, "blur(calc(1px + 1em))"), "blur(21px)");
        assert_eq!(c(PropertyId::AccentColor, "red"), "rgb(255, 0, 0)");
        assert_eq!(c(PropertyId::AccentColor, "auto"), "auto");
        assert_eq!(c(PropertyId::AccentColor, "currentColor"), "currentcolor");
        assert_eq!(
            c(PropertyId::BackgroundImage, "repeating-linear-gradient(red 0 10px, blue 10px 20px)"),
            "repeating-linear-gradient(rgb(255, 0, 0) 0px, rgb(255, 0, 0) 10px, rgb(0, 0, 255) 10px, rgb(0, 0, 255) 20px)"
        );
        assert_eq!(c(PropertyId::ListStyleImage, "linear-gradient(red 1em 2em, blue)"), "linear-gradient(rgb(255, 0, 0) 20px, rgb(255, 0, 0) 40px, rgb(0, 0, 255))");
        assert_eq!(c(PropertyId::ShapeOutside, "linear-gradient(red, blue)"), "linear-gradient(rgb(255, 0, 0), rgb(0, 0, 255))");
        assert_eq!(
            c(PropertyId::BackgroundImage, "repeating-conic-gradient(#333 0 25%, #eee 0 50%)"),
            "repeating-conic-gradient(rgb(51, 51, 51) 0deg, rgb(51, 51, 51) 25%, rgb(238, 238, 238) 0deg, rgb(238, 238, 238) 50%)"
        );
    }

    // Chrome 152 (`pairs.html`, 2026-10-23, `font-size: 20px`): a pair or a quad as short as it reads the same.
    #[test]
    fn a_pair_or_a_quad_reads_as_short_as_it_can() {
        let ctx = Context { font_size: 20.0, ..Context::default() };
        let c = |p, v| computed_value(p, v, &ctx);
        assert_eq!(c(PropertyId::BorderTopLeftRadius, "2px 2px"), "2px");
        assert_eq!(c(PropertyId::BorderTopLeftRadius, "1em 20px"), "20px");
        assert_eq!(c(PropertyId::BorderTopLeftRadius, "10px 20%"), "10px 20%");
        assert_eq!(c(PropertyId::BorderImageOutset, "1px 2px 1px 2px"), "1px 2px");
        assert_eq!(c(PropertyId::BorderImageOutset, "1px 1px 1px"), "1px");
        assert_eq!(c(PropertyId::BorderImageOutset, "1 2 3 2"), "1 2 3");
        assert_eq!(c(PropertyId::BorderImageOutset, "1em 0 20px 0px"), "20px 0 20px 0px", "a number is not a length");
        assert_eq!(c(PropertyId::BorderImageWidth, "auto auto"), "auto");
        assert_eq!(c(PropertyId::WebkitMaskBoxImageWidth, "2 2 2"), "2");
        assert_eq!(c(PropertyId::BorderImageSlice, "10% 10% fill"), "10% fill");
        assert_eq!(c(PropertyId::BorderImageSlice, "fill 5"), "5 fill");
        assert_eq!(c(PropertyId::BorderImageSlice, "1.5 2.5"), "2 3");
        assert_eq!(c(PropertyId::BorderImageSlice, "33.3%"), "33.3%");
        assert_eq!(c(PropertyId::ContainIntrinsicSize, "2px 2px"), "2px");
        assert_eq!(c(PropertyId::ContainIntrinsicSize, "auto 2px auto 2px"), "auto 2px");
        assert_eq!(c(PropertyId::ContainIntrinsicSize, "auto 2px 2px"), "auto 2px 2px");
        assert_eq!(c(PropertyId::ViewTimelineInset, "1px 1px, 2px 3px"), "1px, 2px 3px");
        assert_eq!(c(PropertyId::ViewTimelineInset, "auto auto"), "auto");
    }

    // Chrome 152 (`kw.html`, 2026-10-23): legacy and alias spellings computed to the one Blink writes.
    #[test]
    fn a_keyword_computes_to_the_spelling_blink_writes() {
        assert_eq!(c(PropertyId::TransitionTimingFunction, "STEPS(3, END)"), "steps(3)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "steps(4, jump-end)"), "steps(4)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "steps(3, jump-start)"), "steps(3, jump-start)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "step-start"), "steps(1, start)");
        assert_eq!(c(PropertyId::AnimationTimingFunction, "ease, step-end"), "ease, steps(1)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "cubic-bezier(.25,.1,.25,1)"), "cubic-bezier(0.25, 0.1, 0.25, 1)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "linear(0, 0.5, 1)"), "linear(0 0%, 0.5 50%, 1 100%)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "linear(0, 0.2, 0.5 60%, 0.7, 1)"), "linear(0 0%, 0.2 30%, 0.5 60%, 0.7 80%, 1 100%)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "linear(0 10%, 1 5%)"), "linear(0 10%, 1 10%)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "linear(0, 1.5 20% 40%, 0.25)"), "linear(0 0%, 1.5 20%, 1.5 40%, 0.25 100%)");
        assert_eq!(c(PropertyId::TransitionTimingFunction, "ease-in, linear(0, 1)"), "ease-in, linear(0 0%, 1 100%)");
        assert_eq!(c(PropertyId::MinWidth, "-webkit-fill-available"), "stretch");
        assert_eq!(c(PropertyId::MaxHeight, "-webkit-max-content"), "max-content");
        assert_eq!(c(PropertyId::AlignItems, "first  baseline"), "baseline");
        assert_eq!(c(PropertyId::JustifySelf, "FIRST BASELINE"), "baseline");
        assert_eq!(c(PropertyId::AlignSelf, "last baseline"), "last baseline");
        assert_eq!(c(PropertyId::ShapeRendering, "crispEdges"), "crispedges");
        assert_eq!(c(PropertyId::ColorInterpolationFilters, "sRGB"), "srgb");
    }

    // Chrome 152 (the getComputedStyle sweep, 2026-10-23): a value computed into its canonical form.
    #[test]
    fn a_value_computes_to_its_canonical_form() {
        assert_eq!(c(PropertyId::GridRowStart, "c 1"), "1 c");
        assert_eq!(c(PropertyId::GridColumnEnd, "span c 2"), "span 2 c");
        assert_eq!(c(PropertyId::GridRowEnd, "2 span"), "span 2");
        assert_eq!(c(PropertyId::GridRowStart, "-1 c"), "-1 c");
        assert_eq!(c(PropertyId::GridRowStart, "auto"), "auto");
        assert_eq!(c(PropertyId::AspectRatio, "1.5"), "1.5 / 1");
        assert_eq!(c(PropertyId::AspectRatio, "calc(2 * 3)"), "6 / 1");
        assert_eq!(c(PropertyId::AspectRatio, "16 / 9"), "16 / 9");
        assert_eq!(c(PropertyId::Rotate, "0.5turn"), "180deg");
        assert_eq!(c(PropertyId::Rotate, "-1rad"), "-57.2958deg");
        assert_eq!(c(PropertyId::Rotate, "x 45deg"), "x 45deg");
        assert_eq!(c(PropertyId::Scale, "25%"), "0.25");
        assert_eq!(c(PropertyId::Scale, "50% 50%"), "0.5");
        assert_eq!(c(PropertyId::Scale, "50% 25%"), "0.5 0.25");
        assert_eq!(c(PropertyId::Scale, "1 1 0%"), "1 1 0");
        assert_eq!(c(PropertyId::Scale, "1 2"), "1 2");
        assert_eq!(c(PropertyId::Scale, "2 2"), "2");
        assert_eq!(c(PropertyId::Scale, "2 2 1"), "2");
        assert_eq!(c(PropertyId::AspectRatio, "auto 2"), "auto 2 / 1");
        assert_eq!(c(PropertyId::TextEmphasisPosition, "under right"), "under");
        assert_eq!(c(PropertyId::Contain, "style paint layout size"), "strict");
        assert_eq!(c(PropertyId::Zoom, "25%"), "0.25");
        assert_eq!(c(PropertyId::Zoom, "normal"), "1");
        assert_eq!(c(PropertyId::Zoom, "0"), "1");
        assert_eq!(c(PropertyId::ShapeImageThreshold, "150%"), "1");
        assert_eq!(c(PropertyId::ShapeImageThreshold, "-2"), "0");
        assert_eq!(c(PropertyId::ShapeImageThreshold, "62.5%"), "0.625");
        assert_eq!(c(PropertyId::WordSpacing, "normal"), "0px");
        assert_eq!(c(PropertyId::InterestDelayStart, "200ms"), "0.2s");
        assert_eq!(c(PropertyId::InterestDelayEnd, "normal"), "normal");
        assert_eq!(c(PropertyId::ScrollSnapType, "y proximity"), "y");
        assert_eq!(c(PropertyId::ScrollSnapType, "x mandatory"), "x mandatory");
        assert_eq!(c(PropertyId::TextEmphasisPosition, "over right"), "over");
        assert_eq!(c(PropertyId::TextEmphasisPosition, "under left"), "under left");
        assert_eq!(c(PropertyId::ColorScheme, "only light"), "light only");
        assert_eq!(c(PropertyId::Contain, "layout paint style"), "content");
        assert_eq!(c(PropertyId::Contain, "size layout"), "size layout");
    }

    // Chrome 152 (the getComputedStyle sweep, 2026-10-23): every counter with its integer.
    #[test]
    fn a_counter_list_computes_every_counters_integer() {
        assert_eq!(c(PropertyId::CounterReset, "c"), "c 0");
        assert_eq!(c(PropertyId::CounterSet, "list-item"), "list-item 0");
        assert_eq!(c(PropertyId::CounterIncrement, "inline flex"), "inline 1 flex 1");
        assert_eq!(c(PropertyId::CounterIncrement, "a b 2"), "a 1 b 2");
        assert_eq!(c(PropertyId::CounterReset, "c -1 d"), "c -1 d 0");
        assert_eq!(c(PropertyId::CounterReset, "none"), "none");
        assert_eq!(c(PropertyId::CounterReset, "reversed(c)"), "reversed(c)");
        assert_eq!(c(PropertyId::PerspectiveOrigin, "right 20% bottom 2em"), "right 20% bottom 2em", "a length from the far side: as written");
    }

    #[test]
    fn a_multi_value_length_resolves_component_by_component() {
        assert_eq!(c(PropertyId::BorderSpacing, "1em 2px"), "16px 2px");
        assert_eq!(c(PropertyId::ObjectPosition, "50% 1em"), "50% 16px");
    }

    #[test]
    fn keywords_come_back_lowercase() {
        assert_eq!(c(PropertyId::Display, "BLOCK"), "block");
        assert_eq!(c(PropertyId::Position, "Absolute"), "absolute");
    }

    // Relative weights are steps on a table, not arithmetic: `bolder` from 400 is 700, and from
    // 700 it is 900, not 800.
    #[test]
    fn bolder_and_lighter_are_steps_not_addition() {
        assert_eq!(font_weight("bolder", 400.0), Some(700.0));
        assert_eq!(font_weight("bolder", 700.0), Some(900.0));
        assert_eq!(font_weight("bolder", 900.0), Some(900.0));
        assert_eq!(font_weight("lighter", 400.0), Some(100.0));
        assert_eq!(font_weight("lighter", 700.0), Some(400.0));
        assert_eq!(font_weight("lighter", 900.0), Some(700.0));
        assert_eq!(font_weight("600", 400.0), Some(600.0));
    }

    // A bare `line-height` is inherited as a *factor*: resolving it to pixels here would freeze the
    // parent's font size into a child with a different one.
    #[test]
    fn a_unitless_line_height_stays_unitless() {
        assert_eq!(c(PropertyId::LineHeight, "1.5"), "1.5");
        assert_eq!(c(PropertyId::LineHeight, "normal"), "normal");
        assert_eq!(c(PropertyId::LineHeight, "2em"), "32px");
        assert_eq!(c(PropertyId::LineHeight, "150%"), "24px", "a percentage is of the font size");
    }

    #[test]
    fn font_size_resolves_against_the_parent_not_itself() {
        assert_eq!(font_size("1.5em", 20.0, 16.0, (1024.0, 768.0)), 30.0);
        assert_eq!(font_size("150%", 20.0, 16.0, (1024.0, 768.0)), 30.0);
        assert_eq!(font_size("2rem", 20.0, 16.0, (1024.0, 768.0)), 32.0);
        assert_eq!(font_size("12px", 20.0, 16.0, (1024.0, 768.0)), 12.0);
    }

    // Blink's table, not the spec's factors (Chrome 150): `small` 13, `large` 18,
    // `x-small` 10; a bare `monospace` family's row starts from 13.
    #[test]
    fn the_absolute_size_keywords_follow_chromes_scale() {
        let at = |kw: &str| font_size(kw, 16.0, 16.0, (1024.0, 768.0));
        assert_eq!(at("medium"), 16.0);
        assert_eq!(at("large"), 18.0);
        assert_eq!(at("small"), 13.0);
        assert_eq!(at("x-small"), 10.0);
        assert_eq!(at("xx-large"), 32.0);
        assert_eq!(keyword_font_size(4, true, false), 13.0);
        assert_eq!(keyword_font_size(3, true, false), 12.0);
        // `larger`/`smaller` are relative to the parent, not to medium.
        assert_eq!(font_size("larger", 10.0, 16.0, (1024.0, 768.0)), 12.0);
        assert_eq!(font_size("calc(5px)", 10.0, 16.0, (1024.0, 768.0)), 5.0, "a math function computes (it read the parent's)");
    }

    // A value the parser accepted but this step cannot reduce is handed back as written, rather
    // than replaced with a wrong number.
    #[test]
    fn something_unresolvable_survives_untouched() {
        assert_eq!(c(PropertyId::FontFamily, "Helvetica, sans-serif"), "Helvetica, sans-serif");
        assert_eq!(c(PropertyId::Transform, "translateX(1px)"), "translateX(1px)");
    }

    // Math with no percentage computes to the length it sums to; with one it stays a canonical
    // `calc()` for layout to resolve against its basis (Chrome 150 reads `3px`, the
    // seed fixture L-css-math-layout).
    #[test]
    fn math_reduces_the_way_chrome_computes_it() {
        assert_eq!(c(PropertyId::MarginTop, "calc(1px + 2px)"), "3px");
        assert_eq!(c(PropertyId::Width, "min(100%, 300px)"), "min(100%, 300px)");
        assert_eq!(c(PropertyId::Width, "calc(20px + 50%)"), "calc(50% + 20px)");
    }

    #[test]
    fn a_computed_length_can_be_read_back_out() {
        assert_eq!(px_of("16px"), Some(16.0));
        assert_eq!(px_of("0"), Some(0.0));
        assert_eq!(px_of("auto"), None);
    }
}
