//! Shorthand expansion: `margin: 0 auto` becomes four declarations before anything else sees it.
//!
//! The cascade stores longhands only. That is not a simplification — it is what the cascade *is*.
//! `margin: 0` followed by `margin-top: 4px` has to leave the top at 4px and the rest at 0, and no
//! representation that keeps `margin` whole can express that. So a shorthand is a parse-time
//! rewrite and never a stored thing.
//!
//! # Why validation is not optional here
//!
//! `margin: red` is an invalid declaration and Chrome drops it whole, which leaves `margin-top`
//! showing whatever an *earlier* rule cascaded onto it. If we expanded it anyway and let the
//! computed-value step fail later, `margin-top` would be overwritten with something unparseable
//! that resolves to the initial value — a different answer, silently. So every component is checked
//! against what its longhand accepts, and one bad component rejects the whole shorthand.

use crate::style::property::PropertyId;
use crate::style::rules::{Declaration, Span, Stylesheet};
use crate::value::{Color, Length, LengthPercentage};
use cssparser::{ParseError, Parser, ParserInput, Token};

/// What a longhand gets from an expansion: a piece of the source, or a value the source never
/// mentions.
///
/// `border: solid` sets four widths to `medium` and four colors to `currentcolor`, and neither
/// string appears anywhere in the sheet. Keeping the two cases apart until the very end means the
/// classification pass can borrow the sheet's text while it decides, and only the final pass needs
/// to write into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    From(Span),
    Literal(&'static str),
    /// Two pieces of the sheet joined by a space. `border-radius: 10px / 20px` gives every corner
    /// both radii, and the two are never adjacent in the source.
    Pair(Span, Span),
    /// A value the SHORTHAND states though the source does not write it -- unlike [`Value::Literal`],
    /// not an omitted component: `background: center` gives the vertical axis an explicit `center`
    /// (Blink's parser), which the CSSOM then writes back.
    Fixed(&'static str),
}

/// One top-level component value of a shorthand, as a span of the sheet's text.
///
/// Splitting has to step *over* blocks rather than into them: after `next()` returns a
/// `Function` the parser sits inside it, so a naive "position after the token" would cut
/// `calc(1px + 2px)` off after `calc(`. `parse_nested_block` is what walks past the closing
/// bracket.
pub fn components(input: &mut Parser) -> Vec<Span> {
    let mut out = Vec::new();
    loop {
        input.skip_whitespace();
        let start = input.position();
        let opens_block = match input.next() {
            Err(_) => return out,
            Ok(t) => matches!(
                t,
                Token::Function(_)
                    | Token::ParenthesisBlock
                    | Token::SquareBracketBlock
                    | Token::CurlyBracketBlock
            ),
        };
        if opens_block {
            let _ = input.parse_nested_block(|inner| -> Result<(), ParseError<'_, ()>> {
                while inner.next().is_ok() {}
                Ok(())
            });
        }
        out.push(Span::of(start.byte_index()..input.position().byte_index()));
    }
}

/// What a longhand will accept, for the check described in the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accepts {
    /// `<length-percentage>`
    LengthPercentage,
    /// `<length-percentage> | auto` — margins and insets may be auto, padding may not.
    LengthPercentageOrAuto,
    /// One of a fixed set of keywords.
    Keyword(&'static [&'static str]),
    /// `<length-percentage> | normal` — the gaps.
    LengthPercentageOrNormal,
}

// `overlay` is `auto`'s legacy alias, still accepted by the shorthand: Chrome 150 keeps
// `overflow: overlay` and computes it to `auto`, where this engine dropped the declaration.
const OVERFLOW: &[&str] = &["visible", "hidden", "clip", "scroll", "auto", "overlay"];
const ALIGNMENT: &[&str] = &[
    "normal", "stretch", "center", "start", "end", "flex-start", "flex-end", "self-start",
    "self-end", "baseline", "first", "last", "safe", "unsafe", "auto", "space-between",
    "space-around", "space-evenly", "left", "right",
];
const LINE_STYLE: &[&str] =
    &["none", "hidden", "dotted", "dashed", "solid", "double", "groove", "ridge", "inset", "outset"];

fn accepts(kind: Accepts, text: &str) -> bool {
    match kind {
        Accepts::Keyword(set) => ident_is(text, set),
        Accepts::LengthPercentageOrAuto if ident_is(text, &["auto"]) => true,
        Accepts::LengthPercentageOrNormal if ident_is(text, &["normal"]) => true,
        _ => is_length_percentage(text),
    }
}

/// `<length-percentage>`, including `calc()`.
///
/// `calc()` is accepted without being understood. Syntactically it *is* a length-percentage, and
/// Chrome accepts `margin: calc(1px + 2px)`, so rejecting it here would drop a declaration Chrome
/// keeps -- the cascade divergence this whole file exists to avoid. Computing it is P1's job; until
/// then such a length resolves to its initial value, which is a wrong number rather than a wrong
/// cascade. That is the smaller of the two errors, and the one with an owner.
pub(crate) fn is_length_percentage(text: &str) -> bool {
    if is_single_calc(text) {
        return true;
    }
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    LengthPercentage::parse(&mut p).is_ok() && p.expect_exhausted().is_ok()
}

/// Exactly one math function and nothing else, of a `<length-percentage>` type (`ConsumeLengthOrPercent`:
/// the expression parses and its category is a length, a percentage or a mix of the two). Any of
/// Blink's math functions (`min()`, `round()`, `sin()`, ...) through `style::calc`, where this took any `calc(...)`
/// at all -- `calc(1px * atan2(1, 1))` included, which Chrome 150 drops. A math function holding a substitution or
/// a function `style::calc` does not port (`env()`, `anchor()`, `sibling-index()`) is taken as before.
fn is_single_calc(text: &str) -> bool {
    let t = text.trim();
    if !crate::style::math::is_math(t) {
        return false;
    }
    if crate::style::calc::has_foreign_function(t) {
        let mut pi = ParserInput::new(t);
        let mut p = Parser::new(&mut pi);
        if !matches!(p.next(), Ok(Token::Function(_))) {
            return false;
        }
        let _ = p.parse_nested_block(|inner| -> Result<(), ParseError<'_, ()>> {
            while inner.next().is_ok() {}
            Ok(())
        });
        return p.expect_exhausted().is_ok();
    }
    crate::style::calc::single_of(t, crate::style::calc::LENGTH_PERCENTAGE)
}

/// Is `text` exactly one of `set`, ASCII-case-insensitively?
fn ident_is(text: &str, set: &[&str]) -> bool {
    let t = text.trim();
    set.iter().any(|k| t.eq_ignore_ascii_case(k))
}

/// Is this a line width — `thin | medium | thick | <length>`?
fn is_line_width(text: &str) -> bool {
    if ident_is(text, &["thin", "medium", "thick"]) || is_single_calc(text) {
        return true;
    }
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    Length::parse(&mut p).is_ok() && p.expect_exhausted().is_ok()
}

fn is_color(text: &str) -> bool {
    Color::parse_str(text).is_ok()
}

/// The four sides a box shorthand distributes over, in CSS order.
type Sides = [PropertyId; 4];

/// The shorthands that distribute one to four components over the sides of a box.
///
/// The 1/2/3/4 rule is not four rules: one value goes everywhere, two split
/// vertical/horizontal, three name top, horizontal, bottom, and four go clockwise from the top.
/// Every box shorthand shares it, which is why it is written once.
fn box_sides(name: &str) -> Option<(Sides, Accepts)> {
    use PropertyId::*;
    Some(match name {
        "margin" => (
            [MarginTop, MarginRight, MarginBottom, MarginLeft],
            Accepts::LengthPercentageOrAuto,
        ),
        "padding" => (
            [PaddingTop, PaddingRight, PaddingBottom, PaddingLeft],
            Accepts::LengthPercentage,
        ),
        "inset" => ([Top, Right, Bottom, Left], Accepts::LengthPercentageOrAuto),
        "border-style" => (
            [BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle],
            Accepts::Keyword(LINE_STYLE),
        ),
        _ => return None,
    })
}

/// The shorthands that take one or two components — the first for one axis, the second for the
/// other, and a single value for both.
fn pair(name: &str) -> Option<([PropertyId; 2], Accepts)> {
    use PropertyId::*;
    Some(match name {
        // `overflow: hidden auto` is x then y, matching the writing direction.
        "overflow" => ([OverflowX, OverflowY], Accepts::Keyword(OVERFLOW)),
        // `gap` is row first, then column -- the opposite order to `overflow`, and a place people
        // reliably get backwards.
        "gap" => ([RowGap, ColumnGap], Accepts::LengthPercentageOrNormal),
        "place-items" => ([AlignItems, JustifyItems], Accepts::Keyword(ALIGNMENT)),
        "place-content" => ([AlignContent, JustifyContent], Accepts::Keyword(ALIGNMENT)),
        "place-self" => ([AlignSelf, JustifySelf], Accepts::Keyword(ALIGNMENT)),
        // The logical box shorthands (CSS Logical 1 §4): one value for both sides, or start then
        // end. They expand to LOGICAL longhands, which the cascade maps onto physical ones -- see
        // `style::logical`. Chromium's user-agent sheet writes `fieldset` and `legend` padding in
        // exactly these.
        "margin-block" => ([MarginBlockStart, MarginBlockEnd], Accepts::LengthPercentageOrAuto),
        "margin-inline" => ([MarginInlineStart, MarginInlineEnd], Accepts::LengthPercentageOrAuto),
        "padding-block" => ([PaddingBlockStart, PaddingBlockEnd], Accepts::LengthPercentage),
        "padding-inline" => ([PaddingInlineStart, PaddingInlineEnd], Accepts::LengthPercentage),
        "inset-block" => ([InsetBlockStart, InsetBlockEnd], Accepts::LengthPercentageOrAuto),
        "inset-inline" => ([InsetInlineStart, InsetInlineEnd], Accepts::LengthPercentageOrAuto),
        "scroll-margin-block" => ([ScrollMarginBlockStart, ScrollMarginBlockEnd], Accepts::LengthPercentage),
        "scroll-margin-inline" => ([ScrollMarginInlineStart, ScrollMarginInlineEnd], Accepts::LengthPercentage),
        "scroll-padding-block" => ([ScrollPaddingBlockStart, ScrollPaddingBlockEnd], Accepts::LengthPercentageOrAuto),
        "scroll-padding-inline" => ([ScrollPaddingInlineStart, ScrollPaddingInlineEnd], Accepts::LengthPercentageOrAuto),
        "border-block-style" => ([BorderBlockStartStyle, BorderBlockEndStyle], Accepts::Keyword(LINE_STYLE)),
        "border-inline-style" => ([BorderInlineStartStyle, BorderInlineEndStyle], Accepts::Keyword(LINE_STYLE)),
        _ => return None,
    })
}

/// The two-sided shorthands whose component test is a predicate rather than an [`Accepts`].
fn pair_by_predicate(name: &str) -> Option<([PropertyId; 2], fn(&str) -> bool)> {
    use PropertyId::*;
    Some(match name {
        "border-block-width" => ([BorderBlockStartWidth, BorderBlockEndWidth], is_line_width as fn(&str) -> bool),
        "border-inline-width" => ([BorderInlineStartWidth, BorderInlineEndWidth], is_line_width as fn(&str) -> bool),
        "border-block-color" => ([BorderBlockStartColor, BorderBlockEndColor], is_color as fn(&str) -> bool),
        "border-inline-color" => ([BorderInlineStartColor, BorderInlineEndColor], is_color as fn(&str) -> bool),
        _ => return None,
    })
}

/// `border-width` and `border-color` are box shorthands whose component test is not a single
/// `Accepts` variant, so they are checked by predicate instead.
fn box_sides_by_predicate(name: &str) -> Option<(Sides, fn(&str) -> bool)> {
    use PropertyId::*;
    Some(match name {
        "border-width" => (
            [BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth],
            is_line_width as fn(&str) -> bool,
        ),
        "border-color" => (
            [BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor],
            is_color as fn(&str) -> bool,
        ),
        _ => return None,
    })
}

/// Distribute components over four sides by the 1/2/3/4 rule.
fn distribute4(sides: Sides, values: &[Span]) -> Option<Vec<(PropertyId, Value)>> {
    let (t, r, b, l) = match values {
        [a] => (*a, *a, *a, *a),
        [v, h] => (*v, *h, *v, *h),
        [t, h, b] => (*t, *h, *b, *h),
        [t, r, b, l] => (*t, *r, *b, *l),
        _ => return None,
    };
    Some(vec![
        (sides[0], Value::From(t)),
        (sides[1], Value::From(r)),
        (sides[2], Value::From(b)),
        (sides[3], Value::From(l)),
    ])
}

// -------------------------------------------------------------------------------------------
// The `||` shorthands: components in any order, told apart by what they look like
// -------------------------------------------------------------------------------------------

/// Looks a component's text up in the sheet. A named alias because it appears in every
/// expansion signature, and the lifetime has to be written out for the return to borrow the sheet.
type TextOf<'a> = dyn Fn(Span) -> &'a str + 'a;

/// One slot of an order-free shorthand.
struct Slot {
    /// The longhands this slot fills — four for `border`, one for `border-top`.
    props: &'static [PropertyId],
    /// Does this component belong here?
    accept: fn(&str) -> bool,
    /// What the slot gets when the shorthand omits it.
    ///
    /// This is the **specified** initial value, which is not always the initial *computed* value
    /// `PropertyId::initial()` holds. `border: solid` leaves the width omitted, and its specified
    /// initial is `medium` — 3px once a style exists. The table's `"0px"` is what `medium` computes
    /// to when the style is `none`, which is the right answer for a bare element and the wrong one
    /// here. Getting this backwards would make every `border: solid` invisible.
    reset: &'static str,
    /// Whether the slot absorbs a run of consecutive matching components. Only
    /// `text-decoration-line` needs it: `underline overline` is one value, not two components.
    greedy: bool,
}

const fn slot(props: &'static [PropertyId], accept: fn(&str) -> bool, reset: &'static str) -> Slot {
    Slot { props, accept, reset, greedy: false }
}

fn is_line_style_kw(t: &str) -> bool {
    ident_is(t, LINE_STYLE)
}
/// `<counter-style> | <string> | none` (CSS Lists 3 §3.1): any counter style NAME -- a custom ident,
/// the predefined `disclosure-closed` and the `@counter-style` ones included -- a string, or
/// `symbols()`. Chrome 150: the UA's `list-style: disclosure-closed inside` on a `<summary>` reads
/// `disclosure-closed` / `inside`; this rejected the name and with it the whole declaration.
fn is_list_style_type(t: &str) -> bool {
    let t = t.trim();
    if t.starts_with('"') || t.starts_with('\'') || is_function_named(t, &["symbols"]) {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    let ident = !lower.is_empty()
        && !lower.starts_with(|c: char| c.is_ascii_digit())
        && lower.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii());
    ident && !matches!(lower.as_str(), "inside" | "outside" | "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default")
}
fn is_list_style_position(t: &str) -> bool {
    ident_is(t, &["inside", "outside"])
}
/// `none | <image>`: a `url()`, an image set or a generated image -- Blink's `IsGeneratedImage` list, the repeating
/// and `-webkit-` gradients, `cross-fade()` and `paint()` among them (`background:
/// repeating-linear-gradient(...)` was dropped whole).
fn is_image(t: &str) -> bool {
    ident_is(t, &["none"])
        || is_function_named(
            t,
            &[
                "url", "image-set", "-webkit-image-set", "linear-gradient", "radial-gradient", "conic-gradient",
                "repeating-linear-gradient", "repeating-radial-gradient", "repeating-conic-gradient", "-webkit-linear-gradient",
                "-webkit-radial-gradient", "-webkit-repeating-linear-gradient", "-webkit-repeating-radial-gradient",
                "-webkit-gradient", "-webkit-cross-fade", "cross-fade", "paint",
            ],
        )
}
fn is_decoration_line(t: &str) -> bool {
    ident_is(t, &["none", "underline", "overline", "line-through", "blink"])
}
fn is_decoration_style(t: &str) -> bool {
    ident_is(t, &["solid", "double", "dotted", "dashed", "wavy"])
}
fn is_flex_direction(t: &str) -> bool {
    ident_is(t, &["row", "row-reverse", "column", "column-reverse"])
}
fn is_flex_wrap(t: &str) -> bool {
    ident_is(t, &["nowrap", "wrap", "wrap-reverse"])
}

/// Is `text` a single function call with one of these names?
fn is_function_named(text: &str, names: &[&str]) -> bool {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    // `url(…)` is one `UnquotedUrl` token, not a block, so only the function form has a block to
    // step over -- asking for a nested parser after a non-block token panics.
    let is_block = match p.next() {
        Ok(Token::Function(name)) => {
            if !names.iter().any(|n| name.eq_ignore_ascii_case(n)) {
                return false;
            }
            true
        }
        Ok(Token::UnquotedUrl(_)) | Ok(Token::QuotedString(_)) if names.contains(&"url") => false,
        _ => return false,
    };
    if is_block {
        let _ = p.parse_nested_block(|inner| -> Result<(), ParseError<'_, ()>> {
            while inner.next().is_ok() {}
            Ok(())
        });
    }
    p.expect_exhausted().is_ok()
}

fn free_form(name: &str) -> Option<Vec<Slot>> {
    use PropertyId::*;
    const W: [PropertyId; 4] = [BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth];
    const S: [PropertyId; 4] = [BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle];
    const C: [PropertyId; 4] = [BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor];

    // `border` and its four per-side twins are the same three slots over different longhands.
    let border = |i: Option<usize>| -> Vec<Slot> {
        let pick = |all: &'static [PropertyId; 4]| -> &'static [PropertyId] {
            match i {
                Some(n) => &all[n..n + 1],
                None => &all[..],
            }
        };
        vec![
            slot(pick(&W), is_line_width, "medium"),
            slot(pick(&S), is_line_style_kw, "none"),
            slot(pick(&C), is_color, "currentcolor"),
        ]
    };

    Some(match name {
        "border" => border(None),
        "border-top" => border(Some(0)),
        "border-right" => border(Some(1)),
        "border-bottom" => border(Some(2)),
        "border-left" => border(Some(3)),
        // The logical `border-*` twins: the same three slots over logical longhands.
        "border-block" => logical_border(&[BorderBlockStartWidth, BorderBlockEndWidth], &[BorderBlockStartStyle, BorderBlockEndStyle], &[BorderBlockStartColor, BorderBlockEndColor]),
        "border-inline" => logical_border(&[BorderInlineStartWidth, BorderInlineEndWidth], &[BorderInlineStartStyle, BorderInlineEndStyle], &[BorderInlineStartColor, BorderInlineEndColor]),
        "border-block-start" => logical_border(&[BorderBlockStartWidth], &[BorderBlockStartStyle], &[BorderBlockStartColor]),
        "border-block-end" => logical_border(&[BorderBlockEndWidth], &[BorderBlockEndStyle], &[BorderBlockEndColor]),
        "border-inline-start" => logical_border(&[BorderInlineStartWidth], &[BorderInlineStartStyle], &[BorderInlineStartColor]),
        "border-inline-end" => logical_border(&[BorderInlineEndWidth], &[BorderInlineEndStyle], &[BorderInlineEndColor]),
        // `outline-style` takes `auto` as well as a line style (CSS UI 4): `outline: auto 1px` -- the UA sheet's
        // `:focus-visible` focus ring, and many a page's -- was dropped whole.
        "outline" => vec![
            slot(&[OutlineWidth], is_line_width, "medium"),
            slot(&[OutlineStyle], |v: &str| v.eq_ignore_ascii_case("auto") || is_line_style_kw(v), "none"),
            slot(&[OutlineColor], is_color, "currentcolor"),
        ],
        // `<'column-rule-width'> || <'column-rule-style'> || <'column-rule-color'>` (CSS Multicol 1
        // §4.3), the same three slots as `outline`. Chrome 150: `column-rule: 2.715px solid` reads
        // `column-rule-width` `2px`; it was not expanded here, so the width stayed `3px`.
        "column-rule" => vec![
            slot(&[ColumnRuleWidth], is_line_width, "medium"),
            slot(&[ColumnRuleStyle], is_line_style_kw, "none"),
            slot(&[ColumnRuleColor], is_color, "currentcolor"),
        ],
        // `list-style: none` needs no special case: `none` fills the type slot, and the image slot's
        // own reset value is `none` too, so both end up where the spec puts them.
        "list-style" => vec![
            slot(&[ListStyleType], is_list_style_type, "disc"),
            slot(&[ListStylePosition], is_list_style_position, "outside"),
            slot(&[ListStyleImage], is_image, "none"),
        ],
        "text-decoration" => vec![
            Slot { greedy: true, ..slot(&[TextDecorationLine], is_decoration_line, "none") },
            slot(&[TextDecorationStyle], is_decoration_style, "solid"),
            slot(&[TextDecorationColor], is_color, "currentcolor"),
        ],
        // `columns: <column-width> || <column-count>`; an omitted one is `auto` (Blink `Columns::ParseShorthand`).
        "columns" => vec![
            slot(&[ColumnWidth], |t| ident_is(t, &["auto"]) || (is_length_percentage(t) && !t.trim().ends_with('%') && t.trim().parse::<f64>().is_err()), "auto"),
            slot(&[ColumnCount], |t| ident_is(t, &["auto"]) || t.trim().parse::<u32>().is_ok_and(|n| n > 0), "auto"),
        ],
        "flex-flow" => vec![
            slot(&[FlexDirection], is_flex_direction, "row"),
            slot(&[FlexWrap], is_flex_wrap, "nowrap"),
        ],
        _ => return None,
    })
}

/// Every longhand a shorthand sets, found by expanding it: an expansion always writes every longhand
/// (an omitted component gets its reset value), so any valid value names the whole set. The samples
/// are values at least one of which each shorthand here accepts -- `16px serif` is `font`'s.
fn longhands_of(name: &str) -> Vec<PropertyId> {
    // `all`: every property but `direction` and `unicode-bidi` (CSS Cascade 4 §3.2), and no custom property
    if name == "all" {
        return crate::style::property::ALL
            .iter()
            .copied()
            .filter(|&p| !expands(p) && !matches!(p, PropertyId::Direction | PropertyId::UnicodeBidi))
            .collect();
    }
    for sample in ["0", "none", "normal", "auto", "16px serif", "1 1 auto", "row", "visible", "round"] {
        let probe = Stylesheet::new(crate::style::rules::Origin::Author, sample.to_string());
        let span = Span::of(0..sample.len());
        if let Some(list) = expand_list_shorthand(name, sample) {
            if let Some(decls) = list {
                return decls.into_iter().map(|(p, _)| p).collect();
            }
            continue;
        }
        if let Some(built) = expand_built(name, sample) {
            if let Some(decls) = built {
                return decls.into_iter().map(|(p, _, _)| p).collect();
            }
            continue;
        }
        if let Some(Some(decls)) = expand(name, span, &probe) {
            let mut out: Vec<PropertyId> = decls.into_iter().map(|(p, _)| p).collect();
            out.dedup();
            return out;
        }
    }
    Vec::new()
}

/// `border`'s three slots over logical longhands.
fn logical_border(w: &'static [PropertyId], s: &'static [PropertyId], c: &'static [PropertyId]) -> Vec<Slot> {
    vec![slot(w, is_line_width, "medium"), slot(s, is_line_style_kw, "none"), slot(c, is_color, "currentcolor")]
}

/// Assign each component to the first slot that will take it, then fill the rest with their resets.
///
/// A component that no slot accepts, or one that wants a slot already taken, invalidates the whole
/// shorthand — `border: 1px 2px solid` is not a border with two widths, it is not a border.
fn fill_slots(slots: &[Slot], parts: &[Span], at: &TextOf<'_>) -> Option<Vec<(PropertyId, Value)>> {
    let mut filled: Vec<Option<Span>> = vec![None; slots.len()];
    let mut i = 0;
    while i < parts.len() {
        let text = at(parts[i]);
        let which = slots
            .iter()
            .enumerate()
            .position(|(n, s)| filled[n].is_none() && (s.accept)(text))?;
        let mut span = parts[i];
        i += 1;
        if slots[which].greedy {
            while i < parts.len() && (slots[which].accept)(at(parts[i])) {
                span.end = parts[i].end;
                i += 1;
            }
        }
        filled[which] = Some(span);
    }
    let mut out = Vec::new();
    for (n, s) in slots.iter().enumerate() {
        let value = match filled[n] {
            Some(span) => Value::From(span),
            None => Value::Literal(s.reset),
        };
        for &prop in s.props {
            out.push((prop, value));
        }
    }
    Some(out)
}

/// `white-space` is a shorthand now, over two longhands that describe the two halves of what the
/// single keyword used to mean: whether runs of whitespace collapse, and whether lines may wrap.
/// The keyword set does not decompose — `pre-line` collapses *spaces* but keeps *newlines* — so it
/// is a table rather than a rule.
fn white_space(value: &str) -> Option<Vec<(PropertyId, Value)>> {
    let (collapse, wrap) = match value.trim().to_ascii_lowercase().as_str() {
        "normal" => ("collapse", "wrap"),
        "nowrap" => ("collapse", "nowrap"),
        "pre" => ("preserve", "nowrap"),
        "pre-wrap" => ("preserve", "wrap"),
        "pre-line" => ("preserve-breaks", "wrap"),
        "break-spaces" => ("break-spaces", "wrap"),
        _ => return None,
    };
    Some(vec![
        (PropertyId::WhiteSpaceCollapse, Value::Literal(collapse)),
        (PropertyId::TextWrapMode, Value::Literal(wrap)),
    ])
}

// -------------------------------------------------------------------------------------------
// The positional shorthands: where a component sits carries meaning
// -------------------------------------------------------------------------------------------

/// Split components on top-level `/` separators. `border-radius: 10px / 20px` is two groups;
/// `grid-area: 1 / 2 / 3 / 4` is four.
fn split_on_slash(parts: &[Span], at: &TextOf<'_>) -> Vec<Vec<Span>> {
    let mut groups = vec![Vec::new()];
    for &p in parts {
        if at(p).trim() == "/" {
            groups.push(Vec::new());
        } else {
            groups.last_mut().unwrap().push(p);
        }
    }
    groups
}

/// The span covering `parts[a..=b]` — they are contiguous in the source, so one span is enough and
/// the text between them (the whitespace the author wrote) comes along.
fn joined(parts: &[Span]) -> Span {
    Span { start: parts[0].start, end: parts[parts.len() - 1].end }
}

fn is_number(text: &str) -> bool {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    matches!(p.next(), Ok(Token::Number { .. })) && p.expect_exhausted().is_ok()
}

/// `flex-basis` accepts a width: a length-percentage, `auto`, or the intrinsic keywords.
fn is_flex_basis(text: &str) -> bool {
    ident_is(text, &["auto", "content", "min-content", "max-content", "fit-content"])
        || is_length_percentage(text)
}

/// `flex: <grow> <shrink> <basis>`, with the one-value forms that are not what they look like.
///
/// `flex: 1` is the trap: it is *not* `flex-grow: 1` with the other two left alone, it is
/// `1 1 0%`. A flex item written `flex: 1` and one written `flex-grow: 1` lay out differently,
/// because the second keeps `flex-basis: auto` and therefore sizes to its content first. Chrome
/// implements the spec's special case, so anything that does not is visibly wrong on any flex row.
fn flex(parts: &[Span], at: &TextOf<'_>) -> Option<Vec<(PropertyId, Value)>> {
    use PropertyId::*;
    let build = |grow: Value, shrink: Value, basis: Value| {
        Some(vec![(FlexGrow, grow), (FlexShrink, shrink), (FlexBasis, basis)])
    };
    let lit = Value::Literal;
    match parts {
        [a] if ident_is(at(*a), &["none"]) => build(lit("0"), lit("0"), lit("auto")),
        // The special case: a bare number sets the basis to zero, not to auto.
        [a] if is_number(at(*a)) => build(Value::From(*a), lit("1"), lit("0%")),
        [a] if is_flex_basis(at(*a)) => build(lit("1"), lit("1"), Value::From(*a)),
        [a, b] if is_number(at(*a)) && is_number(at(*b)) => {
            build(Value::From(*a), Value::From(*b), lit("0%"))
        }
        [a, b] if is_number(at(*a)) && is_flex_basis(at(*b)) => {
            build(Value::From(*a), lit("1"), Value::From(*b))
        }
        [a, b, c] if is_number(at(*a)) && is_number(at(*b)) && is_flex_basis(at(*c)) => {
            build(Value::From(*a), Value::From(*b), Value::From(*c))
        }
        // The grammar is `<grow> <shrink>? || <basis>`, and `||` means either order: `flex: 30px 2`
        // is a 30px basis that grows by 2. Reading it left to right and rejecting it would drop a
        // declaration Chrome keeps.
        [a, b] if is_flex_basis(at(*a)) && is_number(at(*b)) => {
            build(Value::From(*b), lit("1"), Value::From(*a))
        }
        [a, b, c] if is_flex_basis(at(*a)) && is_number(at(*b)) && is_number(at(*c)) => {
            build(Value::From(*b), Value::From(*c), Value::From(*a))
        }
        _ => None,
    }
}

const ABSOLUTE_SIZE: &[&str] = &[
    "xx-small", "x-small", "small", "medium", "large", "x-large", "xx-large", "xxx-large",
    "larger", "smaller",
];

fn is_font_size(text: &str) -> bool {
    ident_is(text, ABSOLUTE_SIZE) || is_length_percentage(text)
}
fn is_font_style(t: &str) -> bool {
    ident_is(t, &["normal", "italic", "oblique"])
}
fn is_font_variant(t: &str) -> bool {
    ident_is(t, &["normal", "small-caps"])
}
fn is_font_weight(t: &str) -> bool {
    ident_is(t, &["normal", "bold", "bolder", "lighter"]) || is_number(t)
}
fn is_font_stretch(t: &str) -> bool {
    ident_is(
        t,
        &[
            "normal", "ultra-condensed", "extra-condensed", "condensed", "semi-condensed",
            "semi-expanded", "expanded", "extra-expanded", "ultra-expanded",
        ],
    )
}

/// The `font` shorthand: an unordered prefix, then the size, then optionally `/ line-height`, then
/// the family.
///
/// The size is the pivot. It is the only required component and the only one whose position is
/// fixed relative to the family, so everything is found by locating it first rather than by
/// matching a grammar left to right. What sits before it is the style/variant/weight/stretch set in
/// any order; what sits after it is the family, which is one value however many commas it contains.
///
/// The shorthand also *resets* line-height, and that reset is why it is worth getting right:
/// `font: 12px serif` after a `line-height: 2` silently puts the line height back to normal.
/// The sub-properties `font` resets without being able to set them (`ConsumeFont`: "all subproperties of the font ...
/// should be reset to their initial values"), in Blink's order. They were left alone here, so `font: 12px Arial`
/// was seven longhands where Chrome 150 lists nineteen, never wrote itself back as `font: 12px Arial`, and kept an
/// inherited `font-feature-settings`.
fn font_resets() -> [(PropertyId, Value); 12] {
    use PropertyId::*;
    let lit = Value::Literal;
    [
        (FontVariantLigatures, lit("normal")),
        (FontVariantNumeric, lit("normal")),
        (FontVariantEastAsian, lit("normal")),
        (FontVariantAlternates, lit("normal")),
        (FontSizeAdjust, lit("none")),
        (FontLanguageOverride, lit("normal")),
        (FontKerning, lit("auto")),
        (FontOpticalSizing, lit("auto")),
        (FontFeatureSettings, lit("normal")),
        (FontVariationSettings, lit("normal")),
        (FontVariantPosition, lit("normal")),
        (FontVariantEmoji, lit("normal")),
    ]
}

fn font(parts: &[Span], at: &TextOf<'_>) -> Option<Vec<(PropertyId, Value)>> {
    use PropertyId::*;
    // A system font keyword is the whole value (Blink `ConsumeSystemFont`), and on Linux every one is
    // `LayoutThemeFontProvider::DefaultGUIFont()` -- Arial -- at the default size, the `-webkit-*-control` ones two
    // points smaller, regular and upright (Chrome 150: `font: caption` reads `16px Arial`,
    // `font: -webkit-small-control` `13.3333px Arial`). It was dropped here.
    if let [only] = parts {
        let size = match at(*only).trim().to_ascii_lowercase().as_str() {
            "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar" => Some("medium"),
            "-webkit-mini-control" | "-webkit-small-control" | "-webkit-control" => Some("13.333333px"),
            _ => None,
        };
        if let Some(size) = size {
            let lit = Value::Literal;
            let mut out = vec![(FontStyle, lit("normal")), (FontVariantCaps, lit("normal"))];
            out.extend(font_resets());
            out.extend([
                (FontWeight, lit("normal")),
                (FontStretch, lit("normal")),
                (FontSize, lit(size)),
                (LineHeight, lit("normal")),
                (FontFamily, lit("Arial")),
            ]);
            return Some(out);
        }
    }
    let size_at = parts.iter().position(|p| is_font_size(at(*p)))?;
    let (prefix, rest) = parts.split_at(size_at);
    let size = rest[0];
    let mut rest = &rest[1..];

    // `12px/1.5` tokenizes as three components, so the slash is found the same way either way.
    let line_height = if !rest.is_empty() && at(rest[0]).trim() == "/" {
        if rest.len() < 2 {
            return None;
        }
        let lh = rest[1];
        rest = &rest[2..];
        Value::From(lh)
    } else {
        Value::Literal("normal")
    };
    if rest.is_empty() {
        return None; // a font with no family is not a font shorthand
    }

    // The prefix is order-free among four slots; `normal` is ambiguous between them, and since
    // every one of them resets to `normal` it does not matter which it lands in.
    let slots: [(PropertyId, fn(&str) -> bool, &'static str); 4] = [
        (FontStyle, is_font_style, "normal"),
        (FontVariantCaps, is_font_variant, "normal"),
        (FontWeight, is_font_weight, "normal"),
        (FontStretch, is_font_stretch, "normal"),
    ];
    let mut filled: [Option<Span>; 4] = [None; 4];
    for &p in prefix {
        let text = at(p);
        let which = slots.iter().enumerate().position(|(n, s)| filled[n].is_none() && (s.1)(text))?;
        filled[which] = Some(p);
    }

    let mut out: Vec<(PropertyId, Value)> = slots
        .iter()
        .enumerate()
        .map(|(n, s)| (s.0, filled[n].map_or(Value::Literal(s.2), Value::From)))
        .collect();
    // Blink's order (`ConsumeFont`): style and caps, the twelve resets, then weight and stretch
    let mut tail = out.split_off(2);
    out.extend(font_resets());
    out.append(&mut tail);
    out.push((FontSize, Value::From(size)));
    out.push((LineHeight, line_height));
    out.push((FontFamily, Value::From(joined(rest))));
    Some(out)
}

const BG_REPEAT: &[&str] = &["repeat", "repeat-x", "repeat-y", "no-repeat", "space", "round"];
const BG_ATTACHMENT: &[&str] = &["scroll", "fixed", "local"];
const BG_BOX: &[&str] = &["border-box", "padding-box", "content-box"];

// ── the background family: `background`, `background-position`, `mask-position` ─────────────────────────────────
//
// Blink stores a fill layer's position as two longhands, `background-position-x` and `-y` (`-webkit-mask-position-x`
// and `-y` for a mask), and `background-position` / `mask-position` are SHORTHANDS over them
// (`css_properties.json5`). This engine stored `background-position` whole and had `background` write the halves,
// so `background: url(a.png) center` left `background-position` at `0% 0%`, and `getComputedStyle` could answer
// neither half -- Chrome 150 reads `0%` for an unset `backgroundPositionX` and `calc(100% - 5px)` for `right 5px`.
// Both shorthands now write the halves, every layer of them, and the cascade keeps nothing else.

const POSITION_KEYWORDS: &[&str] = &["left", "right", "top", "bottom", "center"];

fn is_position_part(t: &str) -> bool {
    ident_is(t, POSITION_KEYWORDS) || is_length_percentage(t)
}

/// `<bg-position>` as Blink's `ConsumeBackgroundPosition` reads it (CSS Backgrounds 3 §3.6): one to four
/// components, answered as the specified horizontal and vertical halves (`right 5px`, `bottom`); a half the value
/// leaves out is an explicit `center`. `None` is not a position.
fn bg_position(parts: &[&str]) -> Option<(String, String)> {
    let kw = |t: &str, set: &[&str]| ident_is(t, set);
    let lp = |t: &str| is_length_percentage(t) && !ident_is(t, POSITION_KEYWORDS);
    match parts {
        [a] if kw(a, &["top", "bottom"]) => Some(("center".into(), a.to_string())),
        [a] if kw(a, &["left", "right", "center"]) || lp(a) => Some((a.to_string(), "center".into())),
        [a, b] if (kw(a, &["left", "right", "center"]) || lp(a)) && (kw(b, &["top", "bottom", "center"]) || lp(b)) => {
            Some((a.to_string(), b.to_string()))
        }
        // two keywords the other way round: `top left`, `center right`
        [a, b] if kw(a, &["top", "bottom", "center"]) && kw(b, &["left", "right", "center"]) => Some((b.to_string(), a.to_string())),
        [_, _, _] | [_, _, _, _] => {
            // `PositionFromThreeOrFourValues`: keywords, each edge one optionally followed by its offset, at most
            // one horizontal, one vertical and one `center`
            let (mut x, mut y, mut center) = (None::<String>, None::<String>, false);
            let mut i = 0;
            while i < parts.len() {
                let k = parts[i];
                if !kw(k, POSITION_KEYWORDS) {
                    return None;
                }
                i += 1;
                if kw(k, &["center"]) {
                    if center || parts.get(i).is_some_and(|t| lp(t)) {
                        return None;
                    }
                    center = true;
                    continue;
                }
                let mut half = k.to_string();
                if let Some(off) = parts.get(i).filter(|t| lp(t)) {
                    half = format!("{k} {off}");
                    i += 1;
                }
                let slot = if kw(k, &["left", "right"]) { &mut x } else { &mut y };
                if slot.is_some() {
                    return None;
                }
                *slot = Some(half);
            }
            match (x, y, center) {
                (Some(x), Some(y), false) => Some((x, y)),
                (Some(x), None, true) => Some((x, "center".into())),
                (None, Some(y), true) => Some(("center".into(), y)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A comma-separated value's layers, each as its top-level components; `None` when a layer is empty.
fn layers_of(text: &str) -> Option<Vec<Vec<String>>> {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let spans = components(&mut p);
    let mut layers: Vec<Vec<String>> = vec![Vec::new()];
    for s in spans {
        let piece = text[s.start as usize..s.end as usize].trim();
        if piece == "," {
            layers.push(Vec::new());
        } else if !piece.is_empty() {
            layers.last_mut()?.push(piece.to_string());
        }
    }
    if layers.iter().any(|l| l.is_empty()) { None } else { Some(layers) }
}

/// `background-position` / `mask-position`: every layer's halves, as the two longhands' lists.
fn position_shorthand(x: PropertyId, y: PropertyId, text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for layer in layers_of(text)? {
        let parts: Vec<&str> = layer.iter().map(String::as_str).collect();
        let (lx, ly) = bg_position(&parts)?;
        xs.push(lx);
        ys.push(ly);
    }
    Some(vec![(x, xs.join(", "), false), (y, ys.join(", "), false)])
}

const BG_SIZE_KW: &[&str] = &["cover", "contain"];

/// One `<bg-layer>` (the final one a `<final-bg-layer>`) as Blink's `ParseBackgroundOrMask` reads it: each component
/// at most once and in any order, a size only right after a position and a `/`, one box for both origin and clip or
/// two in that order, the colour only in the last layer. Slots: colour, image, x, y, size, repeat, attachment,
/// origin, clip. `None` is an invalid layer, which invalidates the whole declaration.
fn bg_layer(parts: &[String], last: bool) -> Option<[Option<String>; 9]> {
    let mut slot: [Option<String>; 9] = Default::default();
    let mut i = 0;
    while i < parts.len() {
        let t = parts[i].as_str();
        if slot[1].is_none() && is_image(t) {
            slot[1] = Some(t.to_string());
            i += 1;
        } else if slot[2].is_none() && is_position_part(t) {
            // the longest run of position components that is a position (Blink reads up to four)
            let run = parts[i..].iter().take(4).take_while(|p| is_position_part(p)).count();
            let (n, (x, y)) = (1..=run)
                .rev()
                .find_map(|n| bg_position(&parts[i..i + n].iter().map(String::as_str).collect::<Vec<_>>()).map(|h| (n, h)))?;
            slot[2] = Some(x);
            slot[3] = Some(y);
            i += n;
            if parts.get(i).is_some_and(|p| p == "/") {
                i += 1;
                let size = match parts.get(i).map(String::as_str) {
                    Some(s) if ident_is(s, BG_SIZE_KW) => {
                        i += 1;
                        s.to_string()
                    }
                    Some(s) if ident_is(s, &["auto"]) || is_length_percentage(s) => {
                        i += 1;
                        match parts.get(i).map(String::as_str) {
                            Some(h) if ident_is(h, &["auto"]) || (is_length_percentage(h) && !ident_is(h, POSITION_KEYWORDS)) => {
                                i += 1;
                                format!("{s} {h}")
                            }
                            _ => s.to_string(),
                        }
                    }
                    _ => return None,
                };
                slot[4] = Some(size);
            }
        } else if slot[5].is_none() && ident_is(t, BG_REPEAT) {
            // `repeat-x` / `repeat-y` alone, or one or two of the rest
            let mut v = t.to_string();
            i += 1;
            if !ident_is(t, &["repeat-x", "repeat-y"]) {
                if let Some(next) = parts.get(i).filter(|p| ident_is(p, &["repeat", "space", "round", "no-repeat"])) {
                    v = format!("{t} {next}");
                    i += 1;
                }
            }
            slot[5] = Some(v);
        } else if slot[6].is_none() && ident_is(t, BG_ATTACHMENT) {
            slot[6] = Some(t.to_string());
            i += 1;
        } else if ident_is(t, BG_BOX) && slot[8].is_none() {
            if slot[7].is_none() {
                slot[7] = Some(t.to_string());
            } else {
                slot[8] = Some(t.to_string());
            }
            i += 1;
        } else if last && slot[0].is_none() && is_color(t) {
            slot[0] = Some(t.to_string());
            i += 1;
        } else {
            return None;
        }
    }
    // one box sets both
    if slot[8].is_none() {
        slot[8] = slot[7].clone();
    }
    Some(slot)
}

/// `background`, every layer: each longhand's list in Blink's order (`css_properties.json5`: image, the position's
/// halves, size, repeat, attachment, origin, clip, colour -- `rule.style[0]` is `background-image` in Chrome 150).
/// A component one layer leaves out is Blink's implicit `CSSInitialValue`: for a single layer the longhand is
/// implicit, holding its initial value; in a list it is the item `initial` (Chrome 150 reads `initial, initial`
/// for the `background-position-x` of `background: url(a.png) no-repeat, url(b.png) red`), which computes to the
/// initial value (`style::resolve`) and which the shorthand's serialization leaves out.
fn background_layers(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let layers = layers_of(text)?;
    let n = layers.len();
    let mut parsed = Vec::with_capacity(n);
    for (i, layer) in layers.iter().enumerate() {
        parsed.push(bg_layer(layer, i + 1 == n)?);
    }
    const SLOTS: [(PropertyId, &str); 9] = [
        (BackgroundColor, "rgba(0, 0, 0, 0)"),
        (BackgroundImage, "none"),
        (BackgroundPositionX, "0%"),
        (BackgroundPositionY, "0%"),
        (BackgroundSize, "auto"),
        (BackgroundRepeat, "repeat"),
        (BackgroundAttachment, "scroll"),
        (BackgroundOrigin, "padding-box"),
        (BackgroundClip, "border-box"),
    ];
    let mut out = Vec::new();
    for k in [1, 2, 3, 4, 5, 6, 7, 8, 0] {
        let (property, initial) = SLOTS[k];
        let given: Vec<Option<&String>> = if k == 0 { vec![parsed[n - 1][0].as_ref()] } else { parsed.iter().map(|l| l[k].as_ref()).collect() };
        if let [only] = given.as_slice() {
            out.push((property, only.map_or(initial, String::as_str).to_string(), only.is_none()));
        } else {
            out.push((property, given.iter().map(|v| v.map_or("initial", String::as_str)).collect::<Vec<_>>().join(", "), false));
        }
    }
    Some(out)
}

/// `border-radius: <lp>{1,4} [ / <lp>{1,4} ]?`.
///
/// The `/` form gives every corner two radii, so a corner's value is a *pair* — text that is in the
/// sheet twice over but never adjacent, which is what [`Value::Pair`] exists for.
fn border_radius(parts: &[Span], at: &TextOf<'_>) -> Option<Vec<(PropertyId, Value)>> {
    use PropertyId::*;
    const CORNERS: [PropertyId; 4] =
        [BorderTopLeftRadius, BorderTopRightRadius, BorderBottomRightRadius, BorderBottomLeftRadius];
    let groups = split_on_slash(parts, at);
    if groups.len() > 2 || groups[0].is_empty() {
        return None;
    }
    let corners = |g: &[Span]| -> Option<[Span; 4]> {
        if !g.iter().all(|p| is_length_percentage(at(*p))) {
            return None;
        }
        Some(match g {
            [a] => [*a, *a, *a, *a],
            [a, b] => [*a, *b, *a, *b],
            [a, b, c] => [*a, *b, *c, *b],
            [a, b, c, d] => [*a, *b, *c, *d],
            _ => return None,
        })
    };
    let h = corners(&groups[0])?;
    match groups.get(1) {
        None => Some(CORNERS.iter().zip(h).map(|(&p, s)| (p, Value::From(s))).collect()),
        Some(g) => {
            let v = corners(g)?;
            Some(CORNERS.iter().zip(h.iter().zip(v)).map(|(&p, (&a, b))| (p, Value::Pair(a, b))).collect())
        }
    }
}

/// `grid-row` / `grid-column` (`<start> [/ <end>]`) and `grid-area` (four of them).
///
/// An omitted end becomes `auto`, except when the start is a plain custom identifier — then it is
/// copied, because `grid-area: header` is meant to name one named area rather than to start at
/// `header` and end wherever.
fn grid_line(name: &str, parts: &[Span], at: &TextOf<'_>) -> Option<Vec<(PropertyId, Value)>> {
    use PropertyId::*;
    let props: &[PropertyId] = match name {
        "grid-row" => &[GridRowStart, GridRowEnd],
        "grid-column" => &[GridColumnStart, GridColumnEnd],
        "grid-area" => &[GridRowStart, GridColumnStart, GridRowEnd, GridColumnEnd],
        _ => return None,
    };
    let groups = split_on_slash(parts, at);
    if groups.len() > props.len() || groups.iter().any(|g| g.is_empty()) {
        return None;
    }
    let first = joined(&groups[0]);
    let is_custom_ident = groups[0].len() == 1 && {
        let t = at(first);
        let mut pi = ParserInput::new(t);
        let mut p = Parser::new(&mut pi);
        matches!(p.next(), Ok(Token::Ident(_))) && p.expect_exhausted().is_ok() && !ident_is(t, &["auto", "span"])
    };
    Some(
        props
            .iter()
            .enumerate()
            .map(|(i, &prop)| {
                let value = match groups.get(i) {
                    Some(g) => Value::From(joined(g)),
                    None if is_custom_ident => Value::From(first),
                    None => Value::Literal("auto"),
                };
                (prop, value)
            })
            .collect(),
    )
}

/// Expand a shorthand. `None` means `name` is not a shorthand this file knows — the caller should
/// treat it as an unknown property. `Some(None)` means it is a shorthand but the value is invalid,
/// and the whole declaration is dropped.
#[allow(clippy::option_option)]
pub fn expand(name: &str, value: Span, sheet: &Stylesheet) -> Option<Option<Vec<(PropertyId, Value)>>> {
    let text = &sheet.text()[value.start as usize..value.end as usize];
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    // Component spans are relative to `text`; shift them back onto the sheet.
    let base = value.start;
    let parts: Vec<Span> = components(&mut p)
        .into_iter()
        .map(|s| Span { start: s.start + base, end: s.end + base })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let at = |s: Span| &sheet.text()[s.start as usize..s.end as usize];

    if let Some((sides, kind)) = box_sides(name) {
        if !parts.iter().all(|s| accepts(kind, at(*s))) {
            return Some(None);
        }
        return Some(distribute4(sides, &parts));
    }
    if let Some((sides, ok)) = box_sides_by_predicate(name) {
        if !parts.iter().all(|s| ok(at(*s))) {
            return Some(None);
        }
        return Some(distribute4(sides, &parts));
    }
    if let Some((props, kind)) = pair(name) {
        if !parts.iter().all(|s| accepts(kind, at(*s))) {
            return Some(None);
        }
        return Some(match parts.as_slice() {
            [a] => Some(vec![(props[0], Value::From(*a)), (props[1], Value::From(*a))]),
            [a, b] => Some(vec![(props[0], Value::From(*a)), (props[1], Value::From(*b))]),
            _ => None,
        });
    }
    if let Some((props, ok)) = pair_by_predicate(name) {
        if !parts.iter().all(|s| ok(at(*s))) {
            return Some(None);
        }
        return Some(match parts.as_slice() {
            [a] => Some(vec![(props[0], Value::From(*a)), (props[1], Value::From(*a))]),
            [a, b] => Some(vec![(props[0], Value::From(*a)), (props[1], Value::From(*b))]),
            _ => None,
        });
    }
    if name == "white-space" {
        // A shorthand with an unrecognised keyword is still a shorthand: the declaration is
        // dropped, not treated as an unknown property.
        return Some(match parts.as_slice() {
            [only] => white_space(at(*only)),
            _ => None,
        });
    }
    if name == "text-wrap" {
        // CSS Text 4's `<'text-wrap-mode'> || <'text-wrap-style'>`: one keyword of each,
        // in either order, an omitted one at its initial value. Both keyword lists are Blink's
        // (`blink_names::single_keyword`). Chrome 150: `text-wrap: pretty`, `balance` and `wrap pretty`
        // are supported, `bogus` is not.
        let is = |prop: &str, t: &str| {
            crate::style::blink_names::single_keyword(prop).is_some_and(|k| k.iter().any(|w| w.eq_ignore_ascii_case(t)))
        };
        let (mut mode, mut style) = (None, None);
        for part in &parts {
            let t = at(*part);
            if mode.is_none() && is("text-wrap-mode", t) {
                mode = Some(*part);
            } else if style.is_none() && is("text-wrap-style", t) {
                style = Some(*part);
            } else {
                return Some(None);
            }
        }
        return Some(Some(vec![
            (PropertyId::TextWrapMode, mode.map_or(Value::Literal("wrap"), Value::From)),
            (PropertyId::TextWrapStyle, style.map_or(Value::Literal("auto"), Value::From)),
        ]));
    }
    if let Some(slots) = free_form(name) {
        return Some(fill_slots(&slots, &parts, &at));
    }
    match name {
        "flex" => Some(flex(&parts, &at)),
        "font" => Some(font(&parts, &at)),
        "border-radius" => Some(border_radius(&parts, &at)),
        "grid-row" | "grid-column" | "grid-area" => Some(grid_line(name, &parts, &at)),
        "container" => Some(container(&parts, &at)),
        _ => None,
    }
}

// ── the two LIST-valued shorthands ──────────────────────────────────────────────────────────────
//
// `animation` and `transition` are unlike every other shorthand in this file: their value is a
// comma-separated list of LAYERS, and each longhand's value is itself a comma-separated list with
// one entry per layer. `animation: grow 4s, spin 2s linear` gives `animation-name: grow, spin` and
// `animation-duration: 4s, 2s` -- two strings that appear nowhere in the sheet. That is why these
// two build OWNED text instead of the `Value` spans the rest of this file deals in, and why they
// are handled in `push_expansion` (which has the `&mut Stylesheet` needed to intern) rather than in
// `expand` (which does not).
//
// WITHIN a layer the order is free, and which slot a keyword lands in is "the first slot it is
// valid for that is not already filled", with the NAME -- or, for a transition, the PROPERTY -- as
// the last resort. That rule is not read off the spec; it is what reproduces every one of these,
// recorded from `/usr/bin/google-chrome` 150.0.7871.46 over a loopback server by setting the
// shorthand and reading the longhands back out of `getComputedStyle`:
//
//   animation: forwards 4s              -> fill-mode forwards, name none
//   animation: none forwards 4s         -> fill-mode none,     name forwards
//   animation: ease ease 2s             -> timing ease,        name ease
//   animation: alternate alternate 2s   -> direction alternate, name alternate
//   animation: reverse 2s               -> direction reverse,  name none
//   animation: linear linear 2s         -> timing linear,      name linear
//   animation: steps 2s                 -> name steps          (a bare ident is not an easing)
//   animation: 4s slide 2s              -> duration 4s, delay 2s  (first time, then second)
//   animation: 4s 5s 6s                 -> INVALID (three times, two time slots)
//   animation: slide 4s 1 2             -> INVALID (two numbers, one count slot)
//   animation: spin 2s, 4s              -> name "spin, none", duration "2s, 4s"
//   transition: ease 2s                 -> timing ease,        property all
//   transition: normal 2s               -> behavior normal,    property all
//   transition: none 2s                 -> property none
//   transition: 2s 3s                   -> duration 2s, delay 3s
//   transition: all all 2s              -> INVALID

/// The longhands `animation` sets, in the order a layer's slots are filled. Blink serialises the
/// shorthand back out of exactly these eight (`el.style.cssText` after `style.animation = …`), so
/// these eight are what it covers; `animation-timeline` and `animation-range-*` are not among them.
const ANIMATION_LONGHANDS: [PropertyId; 8] = [
    PropertyId::AnimationDuration,
    PropertyId::AnimationTimingFunction,
    PropertyId::AnimationDelay,
    PropertyId::AnimationIterationCount,
    PropertyId::AnimationDirection,
    PropertyId::AnimationFillMode,
    PropertyId::AnimationPlayState,
    PropertyId::AnimationName,
];

/// The longhands `transition` sets. Five, not four: Chrome's own serialisation of
/// `transition: opacity 3s linear, width 3s linear` reports `transition-behavior: normal, normal`.
const TRANSITION_LONGHANDS: [PropertyId; 5] = [
    PropertyId::TransitionDuration,
    PropertyId::TransitionTimingFunction,
    PropertyId::TransitionDelay,
    PropertyId::TransitionBehavior,
    PropertyId::TransitionProperty,
];

/// A CSS-wide keyword, which applies to every longhand of the shorthand rather than to one slot.
fn css_wide(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    )
}

/// `<time>`: a number with `s` or `ms` on it. A bare `0` is NOT a time in CSS, and Chrome rejects
/// `animation: slide 0` for that reason, so the unit is required here too.
fn is_time(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let num = if let Some(n) = lower.strip_suffix("ms") {
        n
    } else if let Some(n) = lower.strip_suffix('s') {
        n
    } else {
        return false;
    };
    !num.is_empty() && num.parse::<f64>().is_ok()
}

/// `<easing-function>`: the seven keywords, or one of the three functions.
fn is_easing(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out" | "step-start" | "step-end"
    ) || lower.starts_with("steps(")
        || lower.starts_with("cubic-bezier(")
        || lower.starts_with("linear(")
}

/// `<single-animation-iteration-count>`: a number, or `infinite`.
fn is_iteration_count(s: &str) -> bool {
    s.eq_ignore_ascii_case("infinite") || s.parse::<f64>().is_ok()
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    // A leading `-` is allowed (`--x`, `-webkit-foo`); a leading digit is not.
    if !(first.is_alphabetic() || first == '_' || first == '-' || !first.is_ascii()) {
        return false;
    }
    s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii())
}

/// Fill one layer's slots from its components, in the measured precedence order. `None` is an
/// invalid layer, which invalidates the whole declaration.
fn animation_layer(parts: &[&str]) -> Option<[String; 8]> {
    // Slot order matches `ANIMATION_LONGHANDS`; `None` is "not filled yet".
    let mut slot: [Option<String>; 8] = Default::default();
    for part in parts {
        let p = *part;
        let idx = if is_time(p) {
            // The first time is the duration, the second the delay -- the one piece of the
            // shorthand where order does matter.
            if slot[0].is_none() {
                0
            } else if slot[2].is_none() {
                2
            } else {
                return None;
            }
        } else if is_easing(p) && slot[1].is_none() {
            1
        } else if is_iteration_count(p) && slot[3].is_none() {
            3
        } else if matches!(
            p.to_ascii_lowercase().as_str(),
            "normal" | "reverse" | "alternate" | "alternate-reverse"
        ) && slot[4].is_none()
        {
            4
        } else if matches!(p.to_ascii_lowercase().as_str(), "none" | "forwards" | "backwards" | "both")
            && slot[5].is_none()
        {
            5
        } else if matches!(p.to_ascii_lowercase().as_str(), "running" | "paused") && slot[6].is_none() {
            6
        } else if is_ident(p) && slot[7].is_none() {
            7
        } else {
            return None;
        };
        slot[idx] = Some(p.to_string());
    }
    // An omitted duration is `auto` in Blink's shorthand (`CSSAnimationData::InitialDuration`), which
    // `animation: none` writes back as `auto ease 0s ...` (Chrome 150) and computes to `0s`.
    let initial = ["auto", "ease", "0s", "1", "normal", "none", "running", "none"];
    Some(std::array::from_fn(|i| slot[i].clone().unwrap_or_else(|| initial[i].to_string())))
}

/// The same, for `transition`. The last-resort slot is the PROPERTY rather than a name, and there
/// is no iteration count or direction to compete for a keyword.
fn transition_layer(parts: &[&str]) -> Option<[String; 5]> {
    let mut slot: [Option<String>; 5] = Default::default();
    for part in parts {
        let p = *part;
        let idx = if is_time(p) {
            if slot[0].is_none() {
                0
            } else if slot[2].is_none() {
                2
            } else {
                return None;
            }
        } else if is_easing(p) && slot[1].is_none() {
            1
        } else if matches!(p.to_ascii_lowercase().as_str(), "normal" | "allow-discrete") && slot[3].is_none() {
            3
        } else if is_ident(p) && slot[4].is_none() {
            4
        } else {
            return None;
        };
        slot[idx] = Some(p.to_string());
    }
    let initial = ["0s", "ease", "0s", "normal", "all"];
    Some(std::array::from_fn(|i| slot[i].clone().unwrap_or_else(|| initial[i].to_string())))
}

/// Expand `animation` / `transition`, or `None` when `name` is neither. `Some(None)` is "a
/// shorthand whose value is invalid", the same signal `expand` uses.
#[allow(clippy::option_option)]
fn expand_list_shorthand(
    name: &str,
    text: &str,
) -> Option<Option<Vec<(PropertyId, String)>>> {
    let longhands: &[PropertyId] = match name {
        "animation" => &ANIMATION_LONGHANDS,
        "transition" => &TRANSITION_LONGHANDS,
        _ => return None,
    };
    let trimmed = text.trim();
    if css_wide(trimmed) {
        return Some(Some(longhands.iter().map(|p| (*p, trimmed.to_string())).collect()));
    }
    // Split into layers on top-level commas, then into components, both with `components()` so a
    // `cubic-bezier(.4, 0, .2, 1)` is one component and its commas are not separators.
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let spans = components(&mut p);
    if spans.is_empty() {
        return Some(None);
    }
    let at = |s: Span| text[s.start as usize..s.end as usize].trim();
    let mut layers: Vec<Vec<&str>> = vec![Vec::new()];
    for s in spans {
        let piece = at(s);
        if piece == "," {
            layers.push(Vec::new());
        } else if !piece.is_empty() {
            layers.last_mut().expect("at least one layer").push(piece);
        }
    }
    // `animation: a,` and `animation: ,a` are both invalid: an empty layer is not a layer.
    if layers.iter().any(|l| l.is_empty()) {
        return Some(None);
    }
    let mut values: Vec<Vec<String>> = vec![Vec::new(); longhands.len()];
    for layer in &layers {
        let filled: Vec<String> = if name == "animation" {
            match animation_layer(layer) {
                Some(v) => v.to_vec(),
                None => return Some(None),
            }
        } else {
            match transition_layer(layer) {
                Some(v) => v.to_vec(),
                None => return Some(None),
            }
        };
        for (i, v) in filled.into_iter().enumerate() {
            values[i].push(v);
        }
    }
    let mut out: Vec<(PropertyId, String)> = longhands.iter().zip(values).map(|(p, v)| (*p, v.join(", "))).collect();
    // Blink's `animation` also resets the scroll-driven longhands, which it then leaves out of the
    // shorthand's text (`css_properties.json5`, `GetLayeredShorthandValue`); without them here the
    // block can never be written back as `animation`.
    if name == "animation" {
        out.push((PropertyId::AnimationTimeline, "auto".into()));
        out.push((PropertyId::AnimationRangeStart, "normal".into()));
        out.push((PropertyId::AnimationRangeEnd, "normal".into()));
    }
    Some(Some(out))
}

// ── the shorthands this cascade dropped ──────────────────────────────────────────────────────
//
// Each was an unknown property here, so a page's `overscroll-behavior: contain` or `marker: url(#m)` vanished and
// its longhands kept whatever an earlier rule gave them. Each parser follows the Blink one it names, and checks every
// component against its longhand's grammar, because an invalid shorthand is dropped whole in Chrome too.

/// The components of a single-layer value (`None` for a comma list or an empty layer).
fn single_layer(text: &str) -> Option<Vec<String>> {
    let mut layers = layers_of(text)?;
    if layers.len() == 1 { layers.pop() } else { None }
}

/// `ConsumeShorthandVia2Longhands`: one or two components, each its longhand's; one sets both.
fn via_two(text: &str, props: [PropertyId; 2], accept: fn(&str) -> bool) -> Option<Vec<(PropertyId, String, bool)>> {
    let parts = single_layer(text)?;
    if !parts.iter().all(|p| accept(p)) {
        return None;
    }
    let (a, b) = match parts.as_slice() {
        [a] => (a.clone(), a.clone()),
        [a, b] => (a.clone(), b.clone()),
        _ => return None,
    };
    Some(vec![(props[0], a, false), (props[1], b, false)])
}

/// `ConsumeShorthandVia4Longhands`: one to four components over four longhands by the 1/2/3/4 rule.
fn via_four(text: &str, props: [PropertyId; 4], accept: fn(&str) -> bool) -> Option<Vec<(PropertyId, String, bool)>> {
    let parts = single_layer(text)?;
    if !parts.iter().all(|p| accept(p)) {
        return None;
    }
    let v: [&String; 4] = match parts.as_slice() {
        [a] => [a, a, a, a],
        [a, b] => [a, b, a, b],
        [a, b, c] => [a, b, c, b],
        [a, b, c, d] => [a, b, c, d],
        _ => return None,
    };
    Some(props.iter().zip(v).map(|(p, s)| (*p, s.clone(), false)).collect())
}

/// `<corner-shape-value>` (CSS Borders 4, Blink `ConsumeCornerShape`): a keyword or `superellipse(<number> |
/// infinity | -infinity)`.
fn is_corner_shape(t: &str) -> bool {
    if ident_is(t, &["round", "scoop", "bevel", "notch", "square", "squircle"]) {
        return true;
    }
    let lower = t.trim().to_ascii_lowercase();
    let Some(arg) = lower.strip_prefix("superellipse(").and_then(|r| r.strip_suffix(')')) else { return false };
    let arg = arg.trim();
    matches!(arg, "infinity" | "-infinity") || arg.parse::<f64>().is_ok_and(f64::is_finite)
}

/// `none | <time>`-like: `interest-delay-start`'s `normal | <time [0s,inf]>`.
fn is_interest_delay(t: &str) -> bool {
    ident_is(t, &["normal"]) || (is_time(t) && !t.trim_start().starts_with('-'))
}

/// `marker-start`'s `none | <url>`.
fn is_marker(t: &str) -> bool {
    ident_is(t, &["none"]) || is_function_named(t, &["url"]) && !t.trim_start().starts_with('"') && !t.trim_start().starts_with('\'')
}

/// `font-synthesis: none | [ weight || style || small-caps ]` (Blink `FontSynthesis::ParseShorthand`): a part
/// it names is `auto`, the rest `none`.
fn font_synthesis(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let parts = single_layer(text)?;
    let mut on = [false; 3];
    if let [only] = parts.as_slice() {
        if ident_is(only, &["none"]) {
            return Some([FontSynthesisWeight, FontSynthesisStyle, FontSynthesisSmallCaps].iter().map(|p| (*p, "none".to_string(), false)).collect());
        }
    }
    for p in &parts {
        let i = ["weight", "style", "small-caps"].iter().position(|k| p.eq_ignore_ascii_case(k))?;
        if on[i] {
            return None;
        }
        on[i] = true;
    }
    let state = |b: bool| if b { "auto" } else { "none" }.to_string();
    Some(vec![(FontSynthesisWeight, state(on[0]), false), (FontSynthesisStyle, state(on[1]), false), (FontSynthesisSmallCaps, state(on[2]), false)])
}

/// `marker: none | <url>` -- all three markers (Blink `Marker::ParseShorthand`).
fn marker(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let parts = single_layer(text)?;
    let [m] = parts.as_slice() else { return None };
    if !is_marker(m) {
        return None;
    }
    Some([PropertyId::MarkerStart, PropertyId::MarkerMid, PropertyId::MarkerEnd].iter().map(|p| (*p, m.clone(), false)).collect())
}

const EMPHASIS_FILL: &[&str] = &["filled", "open"];
const EMPHASIS_SHAPE: &[&str] = &["dot", "circle", "double-circle", "triangle", "sesame"];

/// `text-emphasis: <'text-emphasis-style'> || <'text-emphasis-color'>` (Blink
/// `ConsumeShorthandGreedilyViaLonghands`): the style one value -- `none`, a string, or a fill and a shape side by
/// side -- an omitted one implicitly initial.
fn text_emphasis(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let parts = single_layer(text)?;
    let (mut style, mut color) = (None::<String>, None::<String>);
    let mut i = 0;
    while i < parts.len() {
        let t = parts[i].as_str();
        if style.is_none() && (ident_is(t, &["none"]) || t.starts_with('"') || t.starts_with('\'')) {
            style = Some(t.to_string());
            i += 1;
        } else if style.is_none() && (ident_is(t, EMPHASIS_FILL) || ident_is(t, EMPHASIS_SHAPE)) {
            // a fill and a shape, either order, adjacent
            let mut v = vec![t.to_string()];
            if let Some(next) = parts.get(i + 1) {
                let pair = (ident_is(t, EMPHASIS_FILL) && ident_is(next, EMPHASIS_SHAPE)) || (ident_is(t, EMPHASIS_SHAPE) && ident_is(next, EMPHASIS_FILL));
                if pair {
                    v.push(next.clone());
                    i += 1;
                }
            }
            style = Some(v.join(" "));
            i += 1;
        } else if color.is_none() && is_color(t) {
            color = Some(t.to_string());
            i += 1;
        } else {
            return None;
        }
    }
    Some(vec![
        (PropertyId::TextEmphasisStyle, style.clone().unwrap_or_else(|| "none".into()), style.is_none()),
        (PropertyId::TextEmphasisColor, color.clone().unwrap_or_else(|| "currentcolor".into()), color.is_none()),
    ])
}

/// `-webkit-text-stroke: <line-width> || <color>`, greedily as `text-emphasis`.
fn text_stroke(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let parts = single_layer(text)?;
    let (mut width, mut color) = (None::<String>, None::<String>);
    for t in &parts {
        if width.is_none() && is_line_width(t) && !t.trim_start().starts_with('-') {
            width = Some(t.clone());
        } else if color.is_none() && is_color(t) {
            color = Some(t.clone());
        } else {
            return None;
        }
    }
    Some(vec![
        (PropertyId::WebkitTextStrokeWidth, width.clone().unwrap_or_else(|| "0".into()), width.is_none()),
        (PropertyId::WebkitTextStrokeColor, color.clone().unwrap_or_else(|| "currentcolor".into()), color.is_none()),
    ])
}

/// The legacy break shorthands (Blink `ConsumeFromPageBreakBetween`, `ConsumeFromColumnBreakBetween`,
/// `ConsumeFromColumnOrPageBreakInside`): `always` is a page (a column) break, `auto`/`avoid` (and `left`/`right`
/// for a page) themselves.
fn legacy_break(name: &str, text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let parts = single_layer(text)?;
    let [v] = parts.as_slice() else { return None };
    let v = v.to_ascii_lowercase();
    let (property, value) = match (name, v.as_str()) {
        ("page-break-after" | "page-break-before", "always") => (None, "page"),
        ("page-break-after" | "page-break-before", "auto" | "avoid" | "left" | "right") => (None, v.as_str()),
        ("-webkit-column-break-after" | "-webkit-column-break-before", "always") => (None, "column"),
        ("-webkit-column-break-after" | "-webkit-column-break-before", "auto" | "avoid") => (None, v.as_str()),
        ("page-break-inside" | "-webkit-column-break-inside", "auto" | "avoid") => (Some(BreakInside), v.as_str()),
        _ => return None,
    };
    let property = property.unwrap_or(if name.ends_with("-after") { BreakAfter } else { BreakBefore });
    Some(vec![(property, value.to_string(), false)])
}

const TEXT_BOX_TRIM: &[&str] = &["none", "trim-start", "trim-end", "trim-both"];

/// `text-box-edge`'s `auto | <text-edge>` as Blink's `ConsumeTextBoxEdge` takes it from `parts[i..]`: an over
/// edge (`text`, `cap`, `ex`) and an under one (`text`, `alphabetic`) -- left out when it is the over edge's own
/// (`text`), required for `cap` and `ex`. Answers the value and how many components it took.
fn text_box_edge(parts: &[String], i: usize) -> Option<(String, usize)> {
    let t = parts.get(i)?;
    if ident_is(t, &["auto"]) {
        return Some(("auto".into(), 1));
    }
    if !ident_is(t, &["text", "cap", "ex"]) {
        return None;
    }
    let over = t.to_ascii_lowercase();
    match parts.get(i + 1).filter(|u| ident_is(u, &["text", "alphabetic"])) {
        Some(under) if over == "text" && under.eq_ignore_ascii_case("text") => Some((over, 2)),
        Some(under) => Some((format!("{over} {}", under.to_ascii_lowercase()), 2)),
        None if over == "text" => Some((over, 1)),
        None => None,
    }
}

/// `text-box: normal | <'text-box-trim'> || <'text-box-edge'>` (Blink `TextBox::ParseShorthand`): an omitted trim
/// is `trim-both`, an omitted edge `auto`.
fn text_box(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let parts = single_layer(text)?;
    let (trim, edge) = if let [only] = parts.as_slice() {
        if ident_is(only, &["normal"]) {
            (Some("none".to_string()), Some("auto".to_string()))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };
    let (mut trim, mut edge) = (trim, edge);
    if trim.is_none() {
        let mut i = 0;
        while i < parts.len() {
            if trim.is_none() && ident_is(&parts[i], TEXT_BOX_TRIM) {
                trim = Some(parts[i].to_ascii_lowercase());
                i += 1;
            } else if edge.is_none() {
                let (v, n) = text_box_edge(&parts, i)?;
                edge = Some(v);
                i += n;
            } else {
                return None;
            }
        }
    }
    if trim.is_none() && edge.is_none() {
        return None;
    }
    Some(vec![
        (PropertyId::TextBoxTrim, trim.unwrap_or_else(|| "trim-both".into()), false),
        (PropertyId::TextBoxEdge, edge.unwrap_or_else(|| "auto".into()), false),
    ])
}

const TRY_ORDER: &[&str] = &["normal", "most-width", "most-height", "most-block-size", "most-inline-size"];
const TRY_TACTIC: &[&str] = &["flip-block", "flip-inline", "flip-start", "flip-x", "flip-y"];

/// One `position-try-fallbacks` item: `<dashed-ident> || <try-tactic>+`, or a `<position-area>` (one or two of
/// its keywords).
fn is_try_fallback(item: &[String]) -> bool {
    let dashed = item.iter().filter(|t| t.starts_with("--") && is_ident(t)).count();
    let tactics = item.iter().filter(|t| ident_is(t, TRY_TACTIC)).count();
    if dashed + tactics == item.len() {
        return dashed <= 1 && !item.is_empty() && tactics <= 3;
    }
    // `<position-area>`: one or two area keywords
    const AREA: &[&str] = &[
        "left", "center", "right", "span-left", "span-right", "x-start", "x-end", "span-x-start", "span-x-end", "self-x-start",
        "self-x-end", "span-self-x-start", "span-self-x-end", "span-all", "top", "bottom", "span-top", "span-bottom",
        "y-start", "y-end", "span-y-start", "span-y-end", "self-y-start", "self-y-end", "span-self-y-start", "span-self-y-end",
        "block-start", "block-end", "span-block-start", "span-block-end", "inline-start", "inline-end", "span-inline-start",
        "span-inline-end", "self-block-start", "self-block-end", "span-self-block-start", "span-self-block-end",
        "self-inline-start", "self-inline-end", "span-self-inline-start", "span-self-inline-end", "start", "end", "span-start",
        "span-end", "self-start", "self-end", "span-self-start", "span-self-end",
    ];
    (1..=2).contains(&item.len()) && item.iter().all(|t| ident_is(t, AREA))
}

/// `position-try: <'position-try-order'>? <'position-try-fallbacks'>` (Blink `ParsePositionTryShorthand`).
fn position_try(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let mut layers = layers_of(text)?;
    let order = if layers[0].first().is_some_and(|t| ident_is(t, TRY_ORDER)) && layers[0].len() > 1 {
        Some(layers[0].remove(0).to_ascii_lowercase())
    } else {
        None
    };
    let fallbacks = if layers.len() == 1 && layers[0].len() == 1 && ident_is(&layers[0][0], &["none"]) {
        "none".to_string()
    } else {
        if !layers.iter().all(|l| is_try_fallback(l)) {
            return None;
        }
        layers.iter().map(|l| l.join(" ")).collect::<Vec<_>>().join(", ")
    };
    Some(vec![
        (PropertyId::PositionTryOrder, order.unwrap_or_else(|| "normal".into()), false),
        (PropertyId::PositionTryFallbacks, fallbacks, false),
    ])
}

/// `scroll-timeline: [ <'scroll-timeline-name'> <'scroll-timeline-axis'>? ]#` and `view-timeline`'s
/// `[ <name> [ <axis> || <inset> ]? ]#` (Blink `ConsumeTimelineItemInto`): per item a name (`none` or a dashed
/// ident), an axis (`block` when left out) and, for a view timeline, an inset of one or two `auto` or lengths.
fn timeline_shorthand(text: &str, name: PropertyId, axis: PropertyId, inset: Option<PropertyId>) -> Option<Vec<(PropertyId, String, bool)>> {
    let (mut names, mut axes, mut insets) = (Vec::new(), Vec::new(), Vec::new());
    for layer in layers_of(text)? {
        let n = &layer[0];
        if !(ident_is(n, &["none"]) || (n.starts_with("--") && is_ident(n))) {
            return None;
        }
        let (mut ax, mut ins) = (None::<String>, None::<Vec<String>>);
        let mut i = 1;
        while i < layer.len() {
            let t = &layer[i];
            if ax.is_none() && ident_is(t, &["block", "inline", "x", "y"]) {
                ax = Some(t.to_ascii_lowercase());
                i += 1;
            } else if inset.is_some() && ins.is_none() && (ident_is(t, &["auto"]) || is_length_percentage(t)) {
                let mut v = vec![t.clone()];
                if let Some(e) = layer.get(i + 1).filter(|e| ident_is(e, &["auto"]) || is_length_percentage(e)) {
                    if !e.eq_ignore_ascii_case(t) {
                        v.push(e.clone());
                    }
                    i += 1;
                }
                ins = Some(v);
                i += 1;
            } else {
                return None;
            }
        }
        names.push(n.clone());
        axes.push(ax.unwrap_or_else(|| "block".into()));
        insets.push(ins.map(|v| v.join(" ")).unwrap_or_else(|| "auto".into()));
    }
    let mut out = vec![(name, names.join(", "), false), (axis, axes.join(", "), false)];
    if let Some(p) = inset {
        out.push((p, insets.join(", "), false));
    }
    Some(out)
}

const TIMELINE_RANGE: &[&str] = &["cover", "contain", "entry", "exit", "entry-crossing", "exit-crossing", "scroll"];

/// One `<'animation-range-start'>` from `layer[i..]`: `normal`, a length-percentage, or a range name with an
/// optional offset. Answers it and how many components it took.
fn range_offset_at(layer: &[String], i: usize) -> Option<(String, usize)> {
    let t = layer.get(i)?;
    if ident_is(t, &["normal"]) {
        return Some(("normal".into(), 1));
    }
    if is_length_percentage(t) {
        return Some((t.clone(), 1));
    }
    if !ident_is(t, TIMELINE_RANGE) {
        return None;
    }
    match layer.get(i + 1).filter(|o| is_length_percentage(o)) {
        Some(o) => Some((format!("{} {o}", t.to_ascii_lowercase()), 2)),
        None => Some((t.to_ascii_lowercase(), 1)),
    }
}

/// `animation-range: [ <'animation-range-start'> <'animation-range-end'>? ]#` (Blink
/// `ConsumeAnimationRangeItemInto`): an omitted end is the start's own range at 100% when the start names a range
/// (`GetImpliedRangeEnd`), else `normal`.
fn animation_range(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    for layer in layers_of(text)? {
        let (start, n) = range_offset_at(&layer, 0)?;
        let end = match range_offset_at(&layer, n) {
            Some((end, m)) if n + m == layer.len() => end,
            Some(_) => return None,
            None if n == layer.len() => {
                let named = start.split(' ').next().filter(|w| ident_is(w, TIMELINE_RANGE));
                named.map_or_else(|| "normal".to_string(), str::to_string)
            }
            None => return None,
        };
        starts.push(start);
        ends.push(end);
    }
    Some(vec![(PropertyId::AnimationRangeStart, starts.join(", "), false), (PropertyId::AnimationRangeEnd, ends.join(", "), false)])
}

/// A quad's shortest form (`CSSQuadValue::kSerializeAsQuad`): `1 1 1 1` is `1`.
fn quad(v: &[String]) -> Option<String> {
    let s = |i: usize| v[i].as_str();
    Some(match v.len() {
        1 => s(0).to_string(),
        2 => if s(0) == s(1) { s(0).to_string() } else { format!("{} {}", s(0), s(1)) },
        3 => sides_text([s(0), s(1), s(2), s(1)]),
        4 => sides_text([s(0), s(1), s(2), s(3)]),
        _ => return None,
    })
}

fn sides_text([t, r, b, l]: [&str; 4]) -> String {
    let show_left = r != l;
    let show_bottom = show_left || b != t;
    let show_right = show_bottom || r != t;
    let mut out = vec![t];
    if show_right {
        out.push(r);
    }
    if show_bottom {
        out.push(b);
    }
    if show_left {
        out.push(l);
    }
    out.join(" ")
}

fn is_non_negative_number(t: &str) -> bool {
    t.parse::<f64>().is_ok_and(|n| n >= 0.0 && n.is_finite())
}

fn is_non_negative_lp(t: &str) -> bool {
    is_length_percentage(t) && !t.trim_start().starts_with('-')
}

/// `border-image` (Blink `ConsumeBorderImageComponents`): an image, a repeat pair, and a slice with an optional
/// `/ width` and `/ outset` (`/ / outset` skipping the width), in any order; an omitted part is its initial value.
fn border_image(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let parts = single_layer(text)?;
    let (mut source, mut slice, mut width, mut outset, mut repeat) = (None::<String>, None::<String>, None::<String>, None::<String>, None::<String>);
    let mut i = 0;
    while i < parts.len() {
        let t = parts[i].as_str();
        if source.is_none() && is_image(t) {
            source = Some(t.to_string());
            i += 1;
        } else if repeat.is_none() && ident_is(t, &["stretch", "repeat", "round", "space"]) {
            let mut v = vec![t.to_ascii_lowercase()];
            if let Some(n) = parts.get(i + 1).filter(|n| ident_is(n, &["stretch", "repeat", "round", "space"])) {
                if !n.eq_ignore_ascii_case(t) {
                    v.push(n.to_ascii_lowercase());
                }
                i += 1;
            }
            repeat = Some(v.join(" "));
            i += 1;
        } else if slice.is_none() && (is_non_negative_number(t) || (t.ends_with('%') && is_non_negative_lp(t)) || ident_is(t, &["fill"])) {
            // `<number [0,inf]> | <percentage [0,inf]>` one to four times, `fill` before or after them
            let slice_num = |c: &str| is_non_negative_number(c) || (c.ends_with('%') && is_non_negative_lp(c));
            let (mut nums, mut fill, mut closed) = (Vec::new(), false, false);
            while let Some(c) = parts.get(i) {
                if ident_is(c, &["fill"]) && !fill {
                    fill = true;
                    // `fill` after the numbers ends them
                    closed = !nums.is_empty();
                } else if slice_num(c) && nums.len() < 4 && !closed {
                    nums.push(c.clone());
                } else {
                    break;
                }
                i += 1;
            }
            if nums.is_empty() {
                return None;
            }
            let q = quad(&nums)?;
            slice = Some(if fill { format!("{q} fill") } else { q });
            if parts.get(i).is_some_and(|p| p == "/") {
                i += 1;
                let mut w = Vec::new();
                while let Some(c) = parts.get(i).filter(|c| (ident_is(c, &["auto"]) || is_non_negative_number(c) || is_non_negative_lp(c)) && w.len() < 4) {
                    w.push(c.clone());
                    i += 1;
                }
                if !w.is_empty() {
                    width = Some(quad(&w)?);
                }
                if parts.get(i).is_some_and(|p| p == "/") {
                    i += 1;
                    let mut o = Vec::new();
                    while let Some(c) = parts.get(i).filter(|c| (is_non_negative_number(c) || (is_non_negative_lp(c) && !c.ends_with('%'))) && o.len() < 4) {
                        o.push(c.clone());
                        i += 1;
                    }
                    if o.is_empty() {
                        return None;
                    }
                    outset = Some(quad(&o)?);
                } else if w.is_empty() {
                    return None;
                }
            }
        } else {
            return None;
        }
    }
    if source.is_none() && repeat.is_none() && slice.is_none() {
        return None;
    }
    let or = |v: Option<String>, d: &str| v.unwrap_or_else(|| d.to_string());
    Some(vec![
        (BorderImageSource, or(source, "none"), false),
        (BorderImageSlice, or(slice, "100%"), false),
        (BorderImageWidth, or(width, "1"), false),
        (BorderImageOutset, or(outset, "0"), false),
        (BorderImageRepeat, or(repeat, "stretch"), false),
    ])
}

const COORD_BOX: &[&str] = &["content-box", "padding-box", "border-box", "fill-box", "stroke-box", "view-box"];

/// `offset-path`'s value from `parts[i..]`: `none`, `ray()`/`path()`/`url()`, a basic shape and/or a coord box.
fn offset_path_at(parts: &[String], i: usize) -> Option<(String, usize)> {
    let t = parts.get(i)?;
    if ident_is(t, &["none"]) {
        return Some(("none".into(), 1));
    }
    let shape = |s: &str| is_function_named(s, &["ray", "path", "url", "inset", "circle", "ellipse", "polygon", "xywh", "rect", "shape"]);
    let boxed = |s: &str| ident_is(s, COORD_BOX);
    if shape(t) {
        // a basic shape may take a coord box after it (not ray/path/url, which Blink parses alone -- kept simple)
        if let Some(b) = parts.get(i + 1).filter(|b| boxed(b)) {
            return Some((format!("{t} {}", b.to_ascii_lowercase()), 2));
        }
        return Some((t.clone(), 1));
    }
    if boxed(t) {
        if let Some(s) = parts.get(i + 1).filter(|s| shape(s)) {
            return Some((format!("{s} {}", t.to_ascii_lowercase()), 2));
        }
        return Some((t.to_ascii_lowercase(), 1));
    }
    None
}

/// `<position>` (or `normal`/`auto` where allowed) from `parts[i..]`: one to four position components that
/// `bg_position` reads as a position. Answers the text and how many components it took.
fn position_at(parts: &[String], i: usize) -> Option<(String, usize)> {
    let run = parts[i..].iter().take(4).take_while(|p| is_position_part(p)).count();
    (1..=run).rev().find_map(|n| {
        let slice: Vec<&str> = parts[i..i + n].iter().map(String::as_str).collect();
        bg_position(&slice).map(|_| (slice.join(" "), n))
    })
}

fn is_angle(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    ["deg", "grad", "rad", "turn"].iter().any(|u| l.strip_suffix(u).is_some_and(|n| !n.is_empty() && n.parse::<f64>().is_ok()))
        || l.starts_with("calc(") && ["deg", "grad", "rad", "turn"].iter().any(|u| l.contains(u))
}

/// `offset` (Blink `Offset::ParseShorthand`): `[ <'offset-position'>? [ <'offset-path'> [ <'offset-distance'> ||
/// <'offset-rotate'> ]? ]? ]! [ / <'offset-anchor'> ]?`, each omitted longhand its initial value.
fn offset(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let parts = single_layer(text)?;
    let mut i = 0;
    let position = if parts.get(i).is_some_and(|t| ident_is(t, &["normal", "auto"])) {
        i += 1;
        Some(parts[i - 1].to_ascii_lowercase())
    } else {
        position_at(&parts, i).map(|(p, n)| {
            i += n;
            p
        })
    };
    let path = offset_path_at(&parts, i).map(|(p, n)| {
        i += n;
        p
    });
    let (mut distance, mut rotate) = (None::<String>, None::<String>);
    if path.is_some() {
        loop {
            let Some(t) = parts.get(i) else { break };
            if distance.is_none() && is_length_percentage(t) {
                distance = Some(t.clone());
                i += 1;
            } else if rotate.is_none() && (ident_is(t, &["auto", "reverse"]) || is_angle(t)) {
                let mut v = vec![t.to_ascii_lowercase()];
                if let Some(n) = parts.get(i + 1) {
                    let pair = (ident_is(t, &["auto", "reverse"]) && is_angle(n)) || (is_angle(t) && ident_is(n, &["auto", "reverse"]));
                    if pair {
                        v.push(n.to_ascii_lowercase());
                        i += 1;
                    }
                }
                rotate = Some(v.join(" "));
                i += 1;
            } else {
                break;
            }
        }
    }
    let anchor = if parts.get(i).is_some_and(|p| p == "/") {
        i += 1;
        if parts.get(i).is_some_and(|t| ident_is(t, &["auto"])) {
            i += 1;
            Some("auto".to_string())
        } else {
            let (p, n) = position_at(&parts, i)?;
            i += n;
            Some(p)
        }
    } else {
        None
    };
    if i != parts.len() || (position.is_none() && path.is_none()) {
        return None;
    }
    let or = |v: Option<String>, d: &str| v.unwrap_or_else(|| d.to_string());
    Some(vec![
        (OffsetPosition, or(position, "normal"), false),
        (OffsetPath, or(path, "none"), false),
        (OffsetDistance, or(distance, "0px"), false),
        (OffsetRotate, or(rotate, "auto"), false),
        (OffsetAnchor, or(anchor, "auto"), false),
    ])
}

const MASK_BOX: &[&str] = &["border-box", "padding-box", "content-box", "fill-box", "stroke-box", "view-box"];
const MASK_COMPOSITE: &[&str] = &["add", "subtract", "intersect", "exclude"];
const MASK_MODE: &[&str] = &["alpha", "luminance", "match-source"];

/// One `<mask-layer>` (CSS Masking 1, Blink `ParseBackgroundOrMask`): a reference, a position with an optional
/// `/ size`, a repeat, an origin box and a clip box (or `no-clip`), a compositing operator and a mode, each at most
/// once. Slots: image, x, y, size, repeat, origin, clip, composite, mode.
fn mask_layer(parts: &[String]) -> Option<[Option<String>; 9]> {
    let mut slot: [Option<String>; 9] = Default::default();
    let mut i = 0;
    while i < parts.len() {
        let t = parts[i].as_str();
        if slot[0].is_none() && is_image(t) {
            slot[0] = Some(t.to_string());
            i += 1;
        } else if slot[1].is_none() && is_position_part(t) {
            let (x, y, n, size) = position_and_size(parts, i)?;
            slot[1] = Some(x);
            slot[2] = Some(y);
            slot[3] = size;
            i += n;
        } else if slot[4].is_none() && ident_is(t, BG_REPEAT) {
            let mut v = t.to_string();
            i += 1;
            if !ident_is(t, &["repeat-x", "repeat-y"]) {
                if let Some(next) = parts.get(i).filter(|p| ident_is(p, &["repeat", "space", "round", "no-repeat"])) {
                    v = format!("{t} {next}");
                    i += 1;
                }
            }
            slot[4] = Some(v);
        } else if slot[6].is_none() && (ident_is(t, MASK_BOX) || (slot[5].is_some() && ident_is(t, &["no-clip"]))) {
            if slot[5].is_none() {
                slot[5] = Some(t.to_string());
            } else {
                slot[6] = Some(t.to_string());
            }
            i += 1;
        } else if slot[6].is_none() && slot[5].is_none() && ident_is(t, &["no-clip"]) {
            slot[6] = Some(t.to_string());
            i += 1;
        } else if slot[7].is_none() && ident_is(t, MASK_COMPOSITE) {
            slot[7] = Some(t.to_string());
            i += 1;
        } else if slot[8].is_none() && ident_is(t, MASK_MODE) {
            slot[8] = Some(t.to_string());
            i += 1;
        } else {
            return None;
        }
    }
    // one box sets both
    if slot[6].is_none() {
        slot[6] = slot[5].clone();
    }
    Some(slot)
}

/// A position (one to four components) from `parts[i..]` and, after a `/`, a size: x, y, components taken, size.
fn position_and_size(parts: &[String], i: usize) -> Option<(String, String, usize, Option<String>)> {
    let run = parts[i..].iter().take(4).take_while(|p| is_position_part(p)).count();
    let (n, (x, y)) = (1..=run)
        .rev()
        .find_map(|n| bg_position(&parts[i..i + n].iter().map(String::as_str).collect::<Vec<_>>()).map(|h| (n, h)))?;
    let mut j = i + n;
    let mut size = None;
    if parts.get(j).is_some_and(|p| p == "/") {
        j += 1;
        let s = parts.get(j)?;
        if ident_is(s, BG_SIZE_KW) {
            size = Some(s.clone());
            j += 1;
        } else if ident_is(s, &["auto"]) || is_length_percentage(s) {
            j += 1;
            match parts.get(j).filter(|h| ident_is(h, &["auto"]) || (is_length_percentage(h) && !ident_is(h, POSITION_KEYWORDS))) {
                Some(h) => {
                    size = Some(format!("{s} {h}"));
                    j += 1;
                }
                None => size = Some(s.clone()),
            }
        } else {
            return None;
        }
    }
    Some((x, y, j - i, size))
}

/// `mask`, every layer, as `background` (`background_layers`): the longhands in Blink's order, a layer's omitted
/// component its implicit initial value.
fn mask_layers(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let layers = layers_of(text)?;
    let parsed: Vec<[Option<String>; 9]> = layers.iter().map(|l| mask_layer(l)).collect::<Option<_>>()?;
    const SLOTS: [(PropertyId, &str); 9] = [
        (MaskImage, "none"),
        (WebkitMaskPositionX, "0%"),
        (WebkitMaskPositionY, "0%"),
        (MaskSize, "auto"),
        (MaskRepeat, "repeat"),
        (MaskOrigin, "border-box"),
        (MaskClip, "border-box"),
        (MaskComposite, "add"),
        (MaskMode, "match-source"),
    ];
    let mut out = Vec::new();
    for (k, (property, initial)) in SLOTS.iter().enumerate() {
        let given: Vec<Option<&String>> = parsed.iter().map(|l| l[k].as_ref()).collect();
        if let [only] = given.as_slice() {
            out.push((*property, only.map_or(*initial, String::as_str).to_string(), only.is_none()));
        } else {
            out.push((*property, given.iter().map(|v| v.map_or("initial", String::as_str)).collect::<Vec<_>>().join(", "), false));
        }
    }
    Some(out)
}

/// Is `t` a `<track-size>` (or a `repeat()`/`minmax()`/`fit-content()` track) of a track list? A loose check: the
/// track list's own grammar is the layout's (`grid-template-rows` is kept as written).
fn is_track(t: &str) -> bool {
    ident_is(t, &["auto", "min-content", "max-content"])
        || is_length_percentage(t)
        || t.to_ascii_lowercase().strip_suffix("fr").is_some_and(|n| n.parse::<f64>().is_ok_and(|v| v >= 0.0))
        || is_function_named(t, &["minmax", "fit-content", "repeat"])
}

fn is_line_names(t: &str) -> bool {
    t.starts_with('[') && t.ends_with(']') && t[1..t.len() - 1].split_whitespace().all(is_ident)
}

/// A `<track-list>` (or `<explicit-track-list>`): track sizes and line-name groups, or `none`.
fn is_track_list(parts: &[String]) -> bool {
    !parts.is_empty() && (parts.len() == 1 && ident_is(&parts[0], &["none"]) || parts.iter().all(|t| is_track(t) || is_line_names(t)) && parts.iter().any(|t| is_track(t)))
}

/// A string token's contents: `s` starts with its (ASCII) quote, and the closing one is dropped only when it is
/// there -- CSS Syntax 3 ends a string token at EOF, so `"aé` is the string `aé`. Slicing `[1..len - 1]` cut a final
/// `é` and sliced a lone `"` as `[1..0]`: one `style.gridTemplate = '"aé'` (or `'"'`, a `grid`, a `CSS.supports`, a
/// `style` attribute) panicked inside the setter, which cannot unwind, and aborted the process (exit 134, 2026-10-23).
fn string_body(s: &str) -> &str {
    let body = &s[1..];
    body.strip_suffix(&s[..1]).unwrap_or(body)
}

/// The rows of `grid-template-areas` strings, each normalized as Chrome serializes it, or `None` when they are not
/// rectangular areas of one width (CSS Grid's `grid-template-areas` validity) or a cell holds a character that is
/// neither a name's nor a `.` -- Blink's `ParseGridTemplateAreasColumnNames` turns the row down then: `"a}"` is
/// invalid in Chrome 152, and so is the open `"aé}` a sheet ends with, which read `"aé"` here while the last BYTE was
/// sliced off and `"aé}"` once the whole string was read. A backslash is let through: an escape is the string's, and
/// is not decoded here.
fn template_area_rows(strings: &[String]) -> Option<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for s in strings {
        let body = string_body(s);
        let mut cells = Vec::new();
        for tok in body.split_whitespace() {
            let mut run = String::new();
            for ch in tok.chars() {
                if ch == '.' {
                    if !run.is_empty() && !run.ends_with('.') {
                        cells.push(std::mem::take(&mut run));
                    }
                    if !run.ends_with('.') {
                        run.push('.');
                    }
                } else {
                    if ch.is_ascii() && !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '\\')) {
                        return None;
                    }
                    if run.ends_with('.') {
                        cells.push(std::mem::take(&mut run));
                    }
                    run.push(ch);
                }
            }
            if !run.is_empty() {
                cells.push(run);
            }
        }
        if cells.is_empty() || rows.first().is_some_and(|r| r.len() != cells.len()) {
            return None;
        }
        rows.push(cells);
    }
    // every named area a filled rectangle
    let mut names: Vec<&str> = rows.iter().flatten().map(String::as_str).filter(|c| *c != ".").collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        let cells: Vec<(usize, usize)> = rows.iter().enumerate().flat_map(|(r, row)| row.iter().enumerate().filter(move |(_, c)| *c == name).map(move |(c, _)| (r, c))).collect();
        let (r0, r1) = (cells.iter().map(|c| c.0).min()?, cells.iter().map(|c| c.0).max()?);
        let (c0, c1) = (cells.iter().map(|c| c.1).min()?, cells.iter().map(|c| c.1).max()?);
        if cells.len() != (r1 - r0 + 1) * (c1 - c0 + 1) {
            return None;
        }
    }
    Some(rows.iter().map(|r| format!("\"{}\"", r.join(" "))).collect())
}

/// `grid-template` (Blink `ConsumeGridTemplateShorthand`): `none`, `<rows> / <columns>`, or the ASCII-art form
/// `[ <line-names>? <string> <track-size>? <line-names>? ]+ [ / <explicit-track-list> ]?`. Answers rows, columns,
/// areas.
fn grid_template_parts(parts: &[String]) -> Option<(String, String, String)> {
    if let [only] = parts {
        if ident_is(only, &["none"]) {
            return Some(("none".into(), "none".into(), "none".into()));
        }
    }
    let slash: Vec<usize> = parts.iter().enumerate().filter(|(_, p)| *p == "/").map(|(i, _)| i).collect();
    let has_strings = parts.iter().any(|p| p.starts_with('"') || p.starts_with('\''));
    if !has_strings {
        let [s] = slash.as_slice() else { return None };
        let (rows, cols) = (&parts[..*s], &parts[s + 1..]);
        if !is_track_list(rows) || !is_track_list(cols) {
            return None;
        }
        return Some((rows.join(" "), cols.join(" "), "none".into()));
    }
    if slash.len() > 1 {
        return None;
    }
    let (head, cols) = match slash.first() {
        Some(s) => (&parts[..*s], Some(&parts[s + 1..])),
        None => (parts, None),
    };
    let mut rows: Vec<String> = Vec::new();
    let mut strings = Vec::new();
    let mut i = 0;
    while i < head.len() {
        let mut names = String::new();
        if is_line_names(&head[i]) {
            names = head[i].clone();
            i += 1;
        }
        let s = head.get(i)?;
        if !(s.starts_with('"') || s.starts_with('\'')) {
            return None;
        }
        strings.push(format!("\"{}\"", string_body(s)));
        i += 1;
        let mut size = "auto".to_string();
        if let Some(t) = head.get(i).filter(|t| is_track(t) && !is_function_named(t, &["repeat"])) {
            size = t.clone();
            i += 1;
        }
        // line names before this row merge with the previous row's trailing ones
        if !names.is_empty() {
            match rows.last_mut().filter(|l| is_line_names(l)) {
                Some(prev) => *prev = format!("[{} {}]", &prev[1..prev.len() - 1], &names[1..names.len() - 1]),
                None => rows.push(names),
            }
        }
        rows.push(size);
        if let Some(n) = head.get(i).filter(|n| is_line_names(n)) {
            rows.push(n.clone());
            i += 1;
        }
    }
    let areas = template_area_rows(&strings)?;
    let cols = match cols {
        Some(c) if is_track_list(c) && !c.iter().any(|t| is_function_named(t, &["repeat"]) && t.to_ascii_lowercase().contains("auto-")) => c.join(" "),
        Some(_) => return None,
        None => "none".to_string(),
    };
    Some((rows.join(" "), cols, areas.join(" ")))
}

fn grid_template(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    let (rows, cols, areas) = grid_template_parts(&single_layer(text)?)?;
    Some(vec![(PropertyId::GridTemplateRows, rows, false), (PropertyId::GridTemplateColumns, cols, false), (PropertyId::GridTemplateAreas, areas, false)])
}

/// `grid` (Blink `Grid::ParseShorthand`): a `grid-template`, or `<rows> / [ auto-flow && dense? ] <auto-columns>?`,
/// or `[ auto-flow && dense? ] <auto-rows>? / <columns>`; whatever it does not set, it resets.
fn grid(text: &str) -> Option<Vec<(PropertyId, String, bool)>> {
    use PropertyId::*;
    let parts = single_layer(text)?;
    let flow_at = |p: &[String]| -> Option<(bool, usize)> {
        // `auto-flow && dense?`: one or two components
        let a = p.first()?;
        let b = p.get(1);
        if ident_is(a, &["auto-flow"]) {
            Some((b.is_some_and(|b| ident_is(b, &["dense"])), if b.is_some_and(|b| ident_is(b, &["dense"])) { 2 } else { 1 }))
        } else if ident_is(a, &["dense"]) && b.is_some_and(|b| ident_is(b, &["auto-flow"])) {
            Some((true, 2))
        } else {
            None
        }
    };
    let out = |rows: String, cols: String, areas: String, flow: String, auto_rows: String, auto_cols: String| {
        vec![
            (GridTemplateRows, rows, false),
            (GridTemplateColumns, cols, false),
            (GridTemplateAreas, areas, false),
            (GridAutoFlow, flow, false),
            (GridAutoRows, auto_rows, false),
            (GridAutoColumns, auto_cols, false),
        ]
    };
    let slash: Vec<usize> = parts.iter().enumerate().filter(|(_, p)| *p == "/").map(|(i, _)| i).collect();
    if let [s] = slash.as_slice() {
        let (left, right) = (&parts[..*s], &parts[s + 1..]);
        if let Some((dense, n)) = flow_at(right) {
            let auto = &right[n..];
            if !is_track_list(left) || !auto.iter().all(|t| is_track(t)) {
                return None;
            }
            let flow = if dense { "column dense" } else { "column" };
            let auto = if auto.is_empty() { "auto".to_string() } else { auto.join(" ") };
            return Some(out(left.join(" "), "none".into(), "none".into(), flow.into(), "auto".into(), auto));
        }
        if let Some((dense, n)) = flow_at(left) {
            let auto = &left[n..];
            if !is_track_list(right) || !auto.iter().all(|t| is_track(t)) {
                return None;
            }
            let flow = if dense { "row dense" } else { "row" };
            let auto = if auto.is_empty() { "auto".to_string() } else { auto.join(" ") };
            return Some(out("none".into(), right.join(" "), "none".into(), flow.into(), auto, "auto".into()));
        }
    }
    let (rows, cols, areas) = grid_template_parts(&parts)?;
    Some(out(rows, cols, areas, "row".into(), "auto".into(), "auto".into()))
}

/// The shorthands whose longhands get BUILT text -- layer lists, and position halves no source writes as such -- as
/// (longhand, text, implicit) triples, or `None` when `name` is not one of them (`Some(None)`: an invalid value).
#[allow(clippy::option_option)]
fn expand_built(name: &str, text: &str) -> Option<Option<Vec<(PropertyId, String, bool)>>> {
    use PropertyId::*;
    Some(match name {
        "background" => background_layers(text),
        "background-position" => position_shorthand(BackgroundPositionX, BackgroundPositionY, text),
        "mask-position" => position_shorthand(WebkitMaskPositionX, WebkitMaskPositionY, text),
        "mask" => mask_layers(text),
        "overscroll-behavior" => via_two(text, [OverscrollBehaviorX, OverscrollBehaviorY], |t| ident_is(t, &["auto", "contain", "none"])),
        "interest-delay" => via_two(text, [InterestDelayStart, InterestDelayEnd], is_interest_delay),
        "corner-shape" => via_four(text, [CornerTopLeftShape, CornerTopRightShape, CornerBottomRightShape, CornerBottomLeftShape], is_corner_shape),
        "corner-top-shape" => via_two(text, [CornerTopLeftShape, CornerTopRightShape], is_corner_shape),
        "corner-right-shape" => via_two(text, [CornerTopRightShape, CornerBottomRightShape], is_corner_shape),
        "corner-bottom-shape" => via_two(text, [CornerBottomLeftShape, CornerBottomRightShape], is_corner_shape),
        "corner-left-shape" => via_two(text, [CornerTopLeftShape, CornerBottomLeftShape], is_corner_shape),
        "corner-block-start-shape" => via_two(text, [CornerStartStartShape, CornerStartEndShape], is_corner_shape),
        "corner-block-end-shape" => via_two(text, [CornerEndStartShape, CornerEndEndShape], is_corner_shape),
        "corner-inline-start-shape" => via_two(text, [CornerStartStartShape, CornerEndStartShape], is_corner_shape),
        "corner-inline-end-shape" => via_two(text, [CornerStartEndShape, CornerEndEndShape], is_corner_shape),
        "font-synthesis" => font_synthesis(text),
        "marker" => marker(text),
        "text-emphasis" => text_emphasis(text),
        "-webkit-text-stroke" => text_stroke(text),
        "page-break-after" | "page-break-before" | "page-break-inside" | "-webkit-column-break-after"
        | "-webkit-column-break-before" | "-webkit-column-break-inside" => legacy_break(name, text),
        "text-box" => text_box(text),
        "position-try" => position_try(text),
        "scroll-timeline" => timeline_shorthand(text, ScrollTimelineName, ScrollTimelineAxis, None),
        "view-timeline" => timeline_shorthand(text, ViewTimelineName, ViewTimelineAxis, Some(ViewTimelineInset)),
        "animation-range" => animation_range(text),
        "border-image" => border_image(text),
        "offset" => offset(text),
        "grid-template" => grid_template(text),
        "grid" => grid(text),
        _ => return None,
    })
}

/// Is `p` a property the table knows that a declaration still writes as a shorthand? `background-position` and
/// `mask-position` keep their rows -- Chrome enumerates both -- but write their halves (see `position_shorthand`).
pub fn expands(p: PropertyId) -> bool {
    matches!(p, PropertyId::BackgroundPosition | PropertyId::MaskPosition | PropertyId::TextDecoration)
}

/// Is `name` a shorthand this file can expand? Used by the parser to tell "unknown property" from
/// "shorthand with a bad value", which are dropped the same way but for different reasons.
pub fn is_shorthand(name: &str) -> bool {
    // `all` takes only the CSS-wide keywords, which `push_expansion` spreads over every longhand: it was no property
    // at all here, so `all: unset` -- a reset layer's first line -- changed nothing (Chrome 150 unsets every property)
    name == "all"
        || box_sides(name).is_some()
        || box_sides_by_predicate(name).is_some()
        || pair(name).is_some()
        || pair_by_predicate(name).is_some()
        || free_form(name).is_some()
        || matches!(
            name,
            "white-space" | "text-wrap" | "flex" | "font" | "background" | "border-radius" | "grid-row"
                | "grid-column" | "grid-area" | "animation" | "transition" | "container" | "background-position"
                | "mask-position"
        )
        || expand_built(name, "initial").is_some()
}

/// `container: <'container-name'> [ / <'container-type'> ]?` (CSS Containment 3 §6.1).
/// Chrome 150: `container: card / inline-size` computes `container-name: card` and
/// `container-type: inline-size`; without the `/ type` part the type resets to `normal`.
fn container(parts: &[Span], at: &TextOf<'_>) -> Option<Vec<(PropertyId, Value)>> {
    let slash = parts.iter().position(|p| at(*p) == "/");
    let (names, ty) = match slash {
        Some(i) => (&parts[..i], parts.get(i + 1..)),
        None => (parts, None),
    };
    if names.is_empty() {
        return None;
    }
    let ident = |t: &str| {
        let mut pi = ParserInput::new(t);
        let mut p = Parser::new(&mut pi);
        matches!(p.next(), Ok(cssparser::Token::Ident(_))) && p.is_exhausted()
    };
    if !names.iter().all(|n| ident(at(*n))) {
        return None;
    }
    let reserved = |t: &str| ident_is(t, &["and", "or", "not", "default", "initial", "inherit", "unset"]);
    if names.len() > 1 && names.iter().any(|n| ident_is(at(*n), &["none"])) || names.iter().any(|n| reserved(at(*n))) {
        return None;
    }
    let name = Value::From(Span { start: names[0].start, end: names[names.len() - 1].end });
    let ty = match ty {
        None => Value::Literal("normal"),
        Some([t]) if ident_is(at(*t), &["normal", "size", "inline-size", "scroll-state"]) => Value::From(*t),
        _ => return None,
    };
    Some(vec![(PropertyId::ContainerName, name), (PropertyId::ContainerType, ty)])
}

/// Push the expansion of a shorthand into `sheet`, or nothing if the value is invalid.
/// The order Blink's parser adds a shorthand's longhands to a declaration block in, where it is not the order this
/// file expands them in -- what `style.item(i)` lists and what a block is serialized from (Chrome 150:
/// `outline: 0` is `outline-color,outline-style,outline-width`, `list-style: none` `list-style-position,
/// list-style-image,list-style-type` (`css_properties.json5`), `transition: opacity .3s` `transition-behavior,
/// transition-duration,transition-timing-function,transition-delay,transition-property` (`ConsumeTransition`)).
fn blink_order(name: &str) -> Option<&'static [PropertyId]> {
    use PropertyId::*;
    Some(match name {
        "outline" => &[OutlineColor, OutlineStyle, OutlineWidth],
        "list-style" => &[ListStylePosition, ListStyleImage, ListStyleType],
        "transition" => &[TransitionBehavior, TransitionDuration, TransitionTimingFunction, TransitionDelay, TransitionProperty],
        _ => return None,
    })
}

/// `decls` in [`blink_order`] when `name` has one (a stable sort: anything it does not list keeps its place last).
fn in_blink_order<T>(name: &str, decls: &mut [(PropertyId, T)]) {
    if let Some(order) = blink_order(name) {
        decls.sort_by_key(|(p, _)| order.iter().position(|o| o == p).unwrap_or(usize::MAX));
    }
}

pub fn push_expansion(sheet: &mut Stylesheet, name: &str, value: Span, important: bool) -> bool {
    // A CSS-WIDE KEYWORD sets every longhand of the shorthand to itself (CSS Cascade 4 §7.3), and no
    // component grammar accepts one, so it used to invalidate the whole declaration. Chromium's UA
    // sheet writes `input[type=checkbox] { padding: initial; border: initial }`: dropping both left
    // a checkbox with the text field's 2px padding and 2px inset border -- Chrome 150 measures a
    // 100px checkbox at clientWidth 100, and this engine answered 96.
    {
        let text = sheet.text()[value.start as usize..value.end as usize].trim().to_ascii_lowercase();
        if matches!(text.as_str(), "initial" | "inherit" | "unset" | "revert" | "revert-layer") {
            let mut longhands = longhands_of(name);
            if longhands.is_empty() {
                return false;
            }
            // `border` is `border-image` as well (see the end of this function): `border: initial` sets all seventeen
            // longhands in Blink, and `removeProperty('border')` takes all seventeen away
            if name == "border" {
                longhands.extend([
                    PropertyId::BorderImageSource,
                    PropertyId::BorderImageSlice,
                    PropertyId::BorderImageWidth,
                    PropertyId::BorderImageOutset,
                    PropertyId::BorderImageRepeat,
                ]);
            }
            let keyword = sheet.intern(&text);
            for property in longhands {
                sheet.push_declaration(Declaration { property, value: keyword, important, implicit: false });
            }
            return true;
        }
    }
    // The two list-valued shorthands first: their longhand values are built strings, so they need
    // the interning `&mut Stylesheet` that `expand`'s borrow cannot give them.
    {
        let text = sheet.text()[value.start as usize..value.end as usize].to_string();
        if let Some(expansion) = expand_list_shorthand(name, &text) {
            if let Some(mut decls) = expansion {
                in_blink_order(name, &mut decls);
                for (property, text) in decls {
                    let value = sheet.intern(&text);
                    sheet.push_declaration(Declaration { property, value, important, implicit: false });
                }
            }
            return true;
        }
    }
    {
        let text = sheet.text()[value.start as usize..value.end as usize].to_string();
        if let Some(expansion) = expand_built(name, &text) {
            for (property, text, implicit) in expansion.unwrap_or_default() {
                let value = sheet.intern(&text);
                sheet.push_declaration(Declaration { property, value, important, implicit });
            }
            return true;
        }
    }
    let Some(expansion) = expand(name, value, sheet) else {
        return false;
    };
    // The shorthands whose omitted components Blink fills with `CSSInitialValue` rather than an
    // explicit initial value (`ConsumeShorthandGreedilyViaLonghands`, `ListStyle`, `ParseBackgroundOrMask`).
    let implicit_fill = matches!(name, "outline" | "column-rule" | "list-style" | "text-emphasis" | "-webkit-text-stroke");
    if let Some(mut decls) = expansion {
        in_blink_order(name, &mut decls);
        for (property, value) in decls {
            let implicit = implicit_fill && matches!(value, Value::Literal(_));
            let value = match value {
                Value::From(span) => span,
                Value::Literal(text) | Value::Fixed(text) => sheet.intern(text),
                Value::Pair(a, b) => {
                    let joined = format!("{} {}", &sheet.text()[a.start as usize..a.end as usize],
                                         &sheet.text()[b.start as usize..b.end as usize]);
                    sheet.intern(&joined)
                }
            };
            sheet.push_declaration(Declaration { property, value, important, implicit });
        }
        // `border` resets `border-image` to its initial value (Blink `Border::ParseShorthand`, CSS
        // Backgrounds "border"); without these the block is never written back as `border`.
        if name == "border" {
            for (property, text) in [
                (PropertyId::BorderImageSource, "none"),
                (PropertyId::BorderImageSlice, "100%"),
                (PropertyId::BorderImageWidth, "1"),
                (PropertyId::BorderImageOutset, "0"),
                (PropertyId::BorderImageRepeat, "stretch"),
            ] {
                let value = sheet.intern(text);
                sheet.push_declaration(Declaration { property, value, important, implicit: false });
            }
        }
    }
    true // it was a shorthand, whether or not it produced anything
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::rules::Origin;

    /// A shorthand that is also in the property table (`text-decoration`) is still expanded into its longhands: the
    /// parser found its name there and stored it whole, so `text-decoration: underline` left `text-decoration-line`
    /// at `none` -- no link was underlined, and `getComputedStyle` said so.
    #[test]
    fn every_shorthand_expands() {
        let whole: Vec<&str> = crate::style::property::ALL
            .iter()
            .map(|p| p.name())
            .filter(|n| is_shorthand(n))
            .filter(|n| !expands(PropertyId::from_name(n).unwrap()))
            .collect();
        assert!(whole.is_empty(), "shorthands stored whole: {whole:?}");
    }
    /// Expand `decl` (written as it would appear in CSS) and return (property, value) pairs.
    fn ex(decl: &str) -> Option<Vec<(&'static str, String)>> {
        let (name, value) = decl.split_once(':').unwrap();
        let text = value.trim().to_string();
        let sheet = Stylesheet::new(Origin::Author, text.clone());
        let span = Span::of(0..text.len());
        let out = expand(name.trim(), span, &sheet).expect("a shorthand")?;
        Some(
            out.into_iter()
                .map(|(p, v)| {
                    let at = |s: Span| sheet.text()[s.start as usize..s.end as usize].to_string();
                    let text = match v {
                        Value::From(s) => at(s),
                        Value::Literal(t) | Value::Fixed(t) => t.to_string(),
                        Value::Pair(a, b) => format!("{} {}", at(a), at(b)),
                    };
                    (p.name(), text)
                })
                .collect(),
        )
    }

    #[test]
    fn one_value_goes_to_every_side() {
        assert_eq!(
            ex("margin: 4px"),
            Some(vec![
                ("margin-top", "4px".into()),
                ("margin-right", "4px".into()),
                ("margin-bottom", "4px".into()),
                ("margin-left", "4px".into()),
            ])
        );
    }

    #[test]
    fn two_values_split_vertical_and_horizontal() {
        assert_eq!(
            ex("margin: 0 auto"),
            Some(vec![
                ("margin-top", "0".into()),
                ("margin-right", "auto".into()),
                ("margin-bottom", "0".into()),
                ("margin-left", "auto".into()),
            ])
        );
    }

    #[test]
    fn three_values_name_top_horizontal_bottom() {
        assert_eq!(
            ex("padding: 1px 2px 3px").unwrap(),
            vec![
                ("padding-top", "1px".into()),
                ("padding-right", "2px".into()),
                ("padding-bottom", "3px".into()),
                ("padding-left", "2px".into()),
            ]
        );
    }

    #[test]
    fn four_values_go_clockwise_from_the_top() {
        assert_eq!(
            ex("padding: 1px 2px 3px 4px").unwrap(),
            vec![
                ("padding-top", "1px".into()),
                ("padding-right", "2px".into()),
                ("padding-bottom", "3px".into()),
                ("padding-left", "4px".into()),
            ]
        );
    }

    #[test]
    fn five_values_is_not_a_box() {
        assert_eq!(ex("padding: 1px 2px 3px 4px 5px"), None);
    }

    // The reason validation is here at all: Chrome drops the whole declaration, leaving whatever an
    // earlier rule cascaded. Expanding it anyway would overwrite four longhands with nonsense.
    #[test]
    fn one_bad_component_rejects_the_whole_shorthand() {
        assert_eq!(ex("margin: 4px red"), None);
        assert_eq!(ex("padding: 4px auto"), None, "padding has no auto, margin does");
        assert_eq!(ex("margin: 4px auto").is_some(), true);
        assert_eq!(ex("border-style: solid banana"), None);
        assert_eq!(ex("border-color: red notacolor"), None);
        assert_eq!(ex("overflow: hidden sideways"), None);
    }

    #[test]
    fn border_width_takes_lengths_and_the_three_keywords() {
        assert!(ex("border-width: thin").is_some());
        assert!(ex("border-width: 1px medium 2em thick").is_some());
        assert_eq!(ex("border-width: red"), None);
    }

    #[test]
    fn border_color_takes_anything_the_color_parser_takes() {
        assert!(ex("border-color: red").is_some());
        assert!(ex("border-color: #fff rgb(0 0 0 / 50%) hsl(120 50% 50%) currentColor").is_some());
    }

    // Two pairs whose order is opposite, and which people reliably get backwards.
    #[test]
    fn overflow_is_x_then_y_but_gap_is_row_then_column() {
        assert_eq!(
            ex("overflow: hidden auto").unwrap(),
            vec![("overflow-x", "hidden".into()), ("overflow-y", "auto".into())]
        );
        assert_eq!(
            ex("gap: 1px 2px").unwrap(),
            vec![("row-gap", "1px".into()), ("column-gap", "2px".into())]
        );
    }

    #[test]
    fn a_single_value_pair_sets_both() {
        assert_eq!(
            ex("gap: 1px").unwrap(),
            vec![("row-gap", "1px".into()), ("column-gap", "1px".into())]
        );
        assert_eq!(
            ex("place-items: center").unwrap(),
            vec![("align-items", "center".into()), ("justify-items", "center".into())]
        );
    }

    #[test]
    fn inset_is_a_box_over_the_four_offsets() {
        assert_eq!(
            ex("inset: 0").unwrap(),
            vec![("top", "0".into()), ("right", "0".into()), ("bottom", "0".into()), ("left", "0".into())]
        );
    }

    // Splitting must step over a block, not into it.
    // Splitting must step over a block, and `calc()` must survive validation even though nothing
    // evaluates it yet -- Chrome accepts it, so rejecting it would drop a declaration Chrome keeps.
    #[test]
    fn a_function_is_one_component_not_a_prefix_of_one() {
        assert_eq!(
            ex("margin: calc(1px + 2px) 0").unwrap(),
            vec![
                ("margin-top", "calc(1px + 2px)".into()),
                ("margin-right", "0".into()),
                ("margin-bottom", "calc(1px + 2px)".into()),
                ("margin-left", "0".into()),
            ]
        );
    }

    // -------- the order-free shorthands --------

    #[test]
    fn border_takes_its_three_parts_in_any_order() {
        let want = vec![
            ("border-top-width", "1px".to_string()),
            ("border-right-width", "1px".to_string()),
            ("border-bottom-width", "1px".to_string()),
            ("border-left-width", "1px".to_string()),
            ("border-top-style", "solid".to_string()),
            ("border-right-style", "solid".to_string()),
            ("border-bottom-style", "solid".to_string()),
            ("border-left-style", "solid".to_string()),
            ("border-top-color", "red".to_string()),
            ("border-right-color", "red".to_string()),
            ("border-bottom-color", "red".to_string()),
            ("border-left-color", "red".to_string()),
        ];
        assert_eq!(ex("border: 1px solid red").unwrap(), want);
        assert_eq!(ex("border: red 1px solid").unwrap(), want, "order does not matter");
        assert_eq!(ex("border: solid red 1px").unwrap(), want);
    }

    // The reset value is the *specified* initial, `medium`. `border: solid` is a 3px border once a style
    // exists; a side whose style is none reads 0px whatever its width (`StyleArena::computed_text`). Getting
    // this backwards makes every `border: solid` invisible.
    #[test]
    fn an_omitted_component_gets_its_specified_initial_not_its_computed_one() {
        let got = ex("border-top: solid").unwrap();
        assert_eq!(
            got,
            vec![
                ("border-top-width", "medium".into()),
                ("border-top-style", "solid".into()),
                ("border-top-color", "currentcolor".into()),
            ]
        );
        assert_eq!(PropertyId::BorderTopWidth.initial(), "3px", "the table's initial is the computed medium");
    }

    #[test]
    fn each_side_shorthand_touches_only_its_own_side() {
        let got = ex("border-left: 2px dashed blue").unwrap();
        assert_eq!(
            got,
            vec![
                ("border-left-width", "2px".into()),
                ("border-left-style", "dashed".into()),
                ("border-left-color", "blue".into()),
            ]
        );
    }

    #[test]
    fn two_components_wanting_the_same_slot_invalidate_the_shorthand() {
        assert_eq!(ex("border: 1px 2px solid"), None);
        assert_eq!(ex("border: solid dashed"), None);
        assert_eq!(ex("border: 1px solid red blue"), None);
        assert_eq!(ex("border: banana"), None);
    }

    #[test]
    fn outline_is_border_over_one_property_each() {
        assert_eq!(
            ex("outline: thin dotted").unwrap(),
            vec![
                ("outline-width", "thin".into()),
                ("outline-style", "dotted".into()),
                ("outline-color", "currentcolor".into()),
            ]
        );
    }

    // `none` fills the type slot, and the image slot's own reset is `none` too, so the spec's
    // "whichever is not otherwise given" falls out without a special case.
    #[test]
    fn list_style_none_lands_on_both_type_and_image() {
        assert_eq!(
            ex("list-style: none").unwrap(),
            vec![
                ("list-style-type", "none".into()),
                ("list-style-position", "outside".into()),
                ("list-style-image", "none".into()),
            ]
        );
        assert_eq!(
            ex("list-style: none inside url(dot.png)").unwrap(),
            vec![
                ("list-style-type", "none".into()),
                ("list-style-position", "inside".into()),
                ("list-style-image", "url(dot.png)".into()),
            ]
        );
    }

    // `underline overline` is one value for one longhand, not two components for two slots.
    #[test]
    fn text_decoration_line_absorbs_a_run_of_keywords() {
        assert_eq!(
            ex("text-decoration: underline overline wavy red").unwrap(),
            vec![
                ("text-decoration-line", "underline overline".into()),
                ("text-decoration-style", "wavy".into()),
                ("text-decoration-color", "red".into()),
            ]
        );
    }

    #[test]
    fn flex_flow_is_direction_and_wrap_in_either_order() {
        let want = vec![("flex-direction", "column".to_string()), ("flex-wrap", "wrap".to_string())];
        assert_eq!(ex("flex-flow: column wrap").unwrap(), want);
        assert_eq!(ex("flex-flow: wrap column").unwrap(), want);
        assert_eq!(
            ex("flex-flow: column").unwrap(),
            vec![("flex-direction", "column".into()), ("flex-wrap", "nowrap".into())]
        );
    }

    // The keyword set does not decompose -- `pre-line` collapses spaces but keeps newlines -- so
    // this is a table, and every row of it is checked.
    #[test]
    fn white_space_splits_into_collapsing_and_wrapping() {
        for (input, collapse, wrap) in [
            ("normal", "collapse", "wrap"),
            ("nowrap", "collapse", "nowrap"),
            ("pre", "preserve", "nowrap"),
            ("pre-wrap", "preserve", "wrap"),
            ("pre-line", "preserve-breaks", "wrap"),
            ("break-spaces", "break-spaces", "wrap"),
        ] {
            assert_eq!(
                ex(&format!("white-space: {input}")).unwrap(),
                vec![
                    ("white-space-collapse", collapse.to_string()),
                    ("text-wrap-mode", wrap.to_string()),
                ],
                "white-space: {input}"
            );
        }
        assert_eq!(ex("white-space: sideways"), None);
        assert_eq!(ex("white-space: pre pre"), None, "it takes exactly one keyword");
    }

    // -------- the positional shorthands --------

    // `flex: 1` is not `flex-grow: 1`. It is `1 1 0%`, and an item written the other way keeps
    // `flex-basis: auto` and sizes to its content first -- a visibly different row.
    #[test]
    fn flex_one_sets_the_basis_to_zero_not_to_auto() {
        assert_eq!(
            ex("flex: 1").unwrap(),
            vec![("flex-grow", "1".into()), ("flex-shrink", "1".into()), ("flex-basis", "0%".into())]
        );
        assert_eq!(
            ex("flex: auto").unwrap(),
            vec![("flex-grow", "1".into()), ("flex-shrink", "1".into()), ("flex-basis", "auto".into())]
        );
        assert_eq!(
            ex("flex: none").unwrap(),
            vec![("flex-grow", "0".into()), ("flex-shrink", "0".into()), ("flex-basis", "auto".into())]
        );
        assert_eq!(
            ex("flex: 200px").unwrap(),
            vec![("flex-grow", "1".into()), ("flex-shrink", "1".into()), ("flex-basis", "200px".into())]
        );
    }

    #[test]
    fn flex_two_and_three_value_forms_are_told_apart_by_type() {
        assert_eq!(
            ex("flex: 2 3").unwrap(),
            vec![("flex-grow", "2".into()), ("flex-shrink", "3".into()), ("flex-basis", "0%".into())]
        );
        assert_eq!(
            ex("flex: 2 30px").unwrap(),
            vec![("flex-grow", "2".into()), ("flex-shrink", "1".into()), ("flex-basis", "30px".into())]
        );
        assert_eq!(
            ex("flex: 2 3 30px").unwrap(),
            vec![("flex-grow", "2".into()), ("flex-shrink", "3".into()), ("flex-basis", "30px".into())]
        );
        // `<grow> <shrink>? || <basis>` -- the `||` means the basis may come first.
        assert_eq!(
            ex("flex: 30px 2").unwrap(),
            vec![("flex-grow", "2".into()), ("flex-shrink", "1".into()), ("flex-basis", "30px".into())]
        );
        assert_eq!(
            ex("flex: 30px 2 3").unwrap(),
            vec![("flex-grow", "2".into()), ("flex-shrink", "3".into()), ("flex-basis", "30px".into())]
        );
        assert_eq!(ex("flex: 1 2 3"), None, "three numbers is not a basis");
    }

    /// `font`'s nineteen longhands in Blink's order (`ConsumeFont`): style and caps, the twelve sub-properties it only
    /// resets, then the rest -- Chrome 150 lists `font: 12px Arial` as these nineteen.
    fn font_longhands(style: &str, weight: &str, size: &str, line_height: &str, family: &str) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = vec![("font-style", style.into()), ("font-variant-caps", "normal".into())];
        for (p, v) in [
            ("font-variant-ligatures", "normal"),
            ("font-variant-numeric", "normal"),
            ("font-variant-east-asian", "normal"),
            ("font-variant-alternates", "normal"),
            ("font-size-adjust", "none"),
            ("font-language-override", "normal"),
            ("font-kerning", "auto"),
            ("font-optical-sizing", "auto"),
            ("font-feature-settings", "normal"),
            ("font-variation-settings", "normal"),
            ("font-variant-position", "normal"),
            ("font-variant-emoji", "normal"),
        ] {
            out.push((p, v.into()));
        }
        out.extend([
            ("font-weight", weight.into()),
            ("font-stretch", "normal".into()),
            ("font-size", size.into()),
            ("line-height", line_height.into()),
            ("font-family", family.into()),
        ]);
        out
    }

    #[test]
    fn font_is_found_by_locating_the_size_first() {
        assert_eq!(
            ex("font: italic bold 12px/1.5 Helvetica, sans-serif").unwrap(),
            font_longhands("italic", "bold", "12px", "1.5", "Helvetica, sans-serif")
        );
    }

    // The reset is the part that bites: `font: 12px serif` after a `line-height: 2` puts the line
    // height back to normal, and a shorthand that forgot to say so would leave it at 2.
    #[test]
    fn font_resets_the_components_it_omits_including_line_height() {
        assert_eq!(ex("font: 12px serif").unwrap(), font_longhands("normal", "normal", "12px", "normal", "serif"));
    }

    #[test]
    fn a_font_needs_a_size_and_a_family() {
        assert_eq!(ex("font: serif"), None);
        assert_eq!(ex("font: 12px"), None);
        assert_eq!(ex("font: caption").map(|v| v.into_iter().find(|(p, _)| *p == "font-family").map(|(_, v)| v)), Some(Some("Arial".into())), "a system font");
        assert_eq!(ex("font: caption 12px"), None, "a system font is the whole value");
        assert_eq!(ex("font: 12px/"), None);
    }

    /// A built expansion (`expand_built`) as (property, value) pairs.
    fn built(decl: &str) -> Option<Vec<(&'static str, String)>> {
        let (name, value) = decl.split_once(':').unwrap();
        let out = expand_built(name.trim(), value.trim()).expect("a built shorthand")?;
        Some(out.into_iter().map(|(p, v, _)| (p.name(), v)).collect())
    }

    #[test]
    fn background_fills_nine_longhands_from_whatever_it_was_given() {
        let got = built("background: url(a.png) no-repeat center / cover").unwrap();
        let map: std::collections::HashMap<_, _> = got.into_iter().collect();
        assert_eq!(map["background-image"], "url(a.png)");
        assert_eq!(map["background-repeat"], "no-repeat");
        assert_eq!(map["background-position-x"], "center");
        assert_eq!(map["background-size"], "cover");
        assert_eq!(map["background-color"], "rgba(0, 0, 0, 0)", "omitted, so reset");
        assert_eq!(map["background-attachment"], "scroll");
    }

    #[test]
    fn a_lone_background_color_still_resets_the_other_eight() {
        let got = built("background: red").unwrap();
        assert_eq!(got.len(), 9);
        let map: std::collections::HashMap<_, _> = got.into_iter().collect();
        assert_eq!(map["background-color"], "red");
        assert_eq!(map["background-image"], "none");
    }

    // One box keyword sets both origin and clip; two set them in order.
    #[test]
    fn a_single_background_box_keyword_sets_origin_and_clip_alike() {
        let got: std::collections::HashMap<_, _> =
            built("background: content-box").unwrap().into_iter().collect();
        assert_eq!(got["background-origin"], "content-box");
        assert_eq!(got["background-clip"], "content-box");
    }

    // Every layer, the colour in the last (Chrome 150: `background: url(a.png), url(b.png) red` computes
    // `background-color: rgb(255, 0, 0)` and two of each other longhand; it was dropped whole).
    #[test]
    fn a_multi_layer_background_fills_every_layer() {
        let got: std::collections::HashMap<_, _> =
            built("background: url(a.png) no-repeat, url(b.png) red").unwrap().into_iter().collect();
        assert_eq!(got["background-image"], "url(a.png), url(b.png)");
        assert_eq!(got["background-repeat"], "no-repeat, initial");
        assert_eq!(got["background-color"], "red");
        assert_eq!(got["background-position-x"], "initial, initial");
        assert_eq!(built("background: red, url(b.png)"), None, "a colour only in the last layer");
        let got: std::collections::HashMap<_, _> =
            built("background: repeating-linear-gradient(red, blue 10px), -webkit-gradient(linear, left top, left bottom, from(red), to(blue))")
                .expect("Blink's generated images")
                .into_iter()
                .collect();
        assert_eq!(got["background-image"], "repeating-linear-gradient(red, blue 10px), -webkit-gradient(linear, left top, left bottom, from(red), to(blue))");
        assert_eq!(built("background: url(a.png),"), None, "no empty layer");
    }


    /// Each expansion against the longhands Chrome 150 computes for it (`fixtures/computed-style/conversions`).
    #[test]
    fn the_shorthands_this_cascade_dropped_expand_as_blink_parses_them() {
        let map = |d: &str| -> Option<std::collections::HashMap<&'static str, String>> { built(d).map(|v| v.into_iter().collect()) };
        let m = map("overscroll-behavior: contain none").unwrap();
        assert_eq!((m["overscroll-behavior-x"].as_str(), m["overscroll-behavior-y"].as_str()), ("contain", "none"));
        assert!(map("overscroll-behavior: contain bogus").is_none());
        let m = map("interest-delay: 1s").unwrap();
        assert_eq!((m["interest-delay-start"].as_str(), m["interest-delay-end"].as_str()), ("1s", "1s"));
        let m = map("corner-shape: bevel squircle").unwrap();
        assert_eq!(m["corner-bottom-left-shape"], "squircle");
        assert_eq!(map("corner-block-start-shape: scoop squircle").unwrap()["corner-start-end-shape"], "squircle");
        let m = map("font-synthesis: style small-caps").unwrap();
        assert_eq!((m["font-synthesis-weight"].as_str(), m["font-synthesis-style"].as_str()), ("none", "auto"));
        assert!(map("font-synthesis: weight weight").is_none());
        assert_eq!(map("marker: url(#m)").unwrap()["marker-end"], "url(#m)");
        let m = map("text-emphasis: filled circle red").unwrap();
        assert_eq!((m["text-emphasis-style"].as_str(), m["text-emphasis-color"].as_str()), ("filled circle", "red"));
        assert!(map("text-emphasis: filled red circle").is_none(), "the style is one value");
        assert_eq!(map("-webkit-text-stroke: 2px red").unwrap()["-webkit-text-stroke-width"], "2px");
        assert_eq!(map("page-break-after: always").unwrap()["break-after"], "page");
        assert_eq!(map("-webkit-column-break-before: always").unwrap()["break-before"], "column");
        assert!(map("page-break-inside: always").is_none());
        let m = map("text-box: trim-both cap alphabetic").unwrap();
        assert_eq!((m["text-box-trim"].as_str(), m["text-box-edge"].as_str()), ("trim-both", "cap alphabetic"));
        assert!(map("text-box: cap").is_none(), "cap names no under edge of its own");
        assert_eq!(map("text-box: text text").unwrap()["text-box-edge"], "text");
        let m = map("position-try: most-height top").unwrap();
        assert_eq!((m["position-try-order"].as_str(), m["position-try-fallbacks"].as_str()), ("most-height", "top"));
        let m = map("scroll-timeline: --a x, --b").unwrap();
        assert_eq!((m["scroll-timeline-name"].as_str(), m["scroll-timeline-axis"].as_str()), ("--a, --b", "x, block"));
        let m = map("view-timeline: --v 1em 2em y").unwrap();
        assert_eq!((m["view-timeline-axis"].as_str(), m["view-timeline-inset"].as_str()), ("y", "1em 2em"));
        let m = map("animation-range: entry 10% exit 90%").unwrap();
        assert_eq!((m["animation-range-start"].as_str(), m["animation-range-end"].as_str()), ("entry 10%", "exit 90%"));
        assert_eq!(map("animation-range: contain").unwrap()["animation-range-end"], "contain");
        let m = map("border-image: url(x.png) 10 20 / 5px / 2px round stretch").unwrap();
        assert_eq!((m["border-image-slice"].as_str(), m["border-image-width"].as_str(), m["border-image-outset"].as_str()), ("10 20", "5px", "2px"));
        assert_eq!(map("border-image: url(c.png) fill 5 5 5 5").unwrap()["border-image-slice"], "5 fill");
        assert_eq!(map("border-image: 30% / / 4px").unwrap()["border-image-outset"], "4px");
        let m = map("offset: path(\"M 0 0 H 100\") 50px auto 30deg").unwrap();
        assert_eq!((m["offset-distance"].as_str(), m["offset-rotate"].as_str()), ("50px", "auto 30deg"));
        assert_eq!(map("offset: 10px 20px").unwrap()["offset-position"], "10px 20px");
        let m = map("mask: url(x.png) no-repeat center / contain").unwrap();
        assert_eq!((m["mask-size"].as_str(), m["mask-repeat"].as_str(), m["-webkit-mask-position-x"].as_str()), ("contain", "no-repeat", "center"));
        let m = map("grid-template: \"a a\" 20px \"b c\" 30px / 1fr 1fr").unwrap();
        assert_eq!((m["grid-template-rows"].as_str(), m["grid-template-areas"].as_str()), ("20px 30px", "\"a a\" \"b c\""));
        assert!(map("grid-template: \"a b\" \"b a\"").is_none(), "areas are rectangles");
        let m = map("grid: 100px / auto-flow dense 50px").unwrap();
        assert_eq!((m["grid-auto-flow"].as_str(), m["grid-auto-columns"].as_str()), ("column dense", "50px"));
    }

    /// A string the page left open is read to its end, not sliced through: a lone `"` and a final multi-byte
    /// character aborted the process from `style.gridTemplate` / `style.grid` (2026-10-23).
    #[test]
    fn an_open_template_string_is_read_to_its_end() {
        let map = |d: &str| -> Option<std::collections::HashMap<&'static str, String>> { built(d).map(|v| v.into_iter().collect()) };
        assert_eq!(string_body("\"a\""), "a");
        assert_eq!(string_body("'a'"), "a");
        assert_eq!(string_body("\"aé"), "aé");
        assert_eq!(string_body("\""), "");
        assert_eq!(string_body("'a\""), "a\"", "only the opening quote closes it");
        // a cell of a character no name has is no area (Blink), the open string's last one too
        assert!(map("grid-template: \"aé}").is_none());
        assert!(map("grid-template: \"a}\"").is_none());
        assert!(map("grid-template: 'a\"").is_none());
        assert_eq!(map("grid-template: \"a-1 _b . é\"").unwrap()["grid-template-areas"], "\"a-1 _b . é\"");
        assert!(map("grid-template: \"").is_none(), "an empty row is no area");
        assert!(map("grid: \"").is_none());
        assert_eq!(map("grid-template: \"aé").unwrap()["grid-template-areas"], "\"aé\"");
        assert_eq!(map("grid: \"é").unwrap()["grid-template-areas"], "\"é\"");
        let m = map("grid-template: \"a\" \"bé").unwrap();
        assert_eq!((m["grid-template-rows"].as_str(), m["grid-template-areas"].as_str()), ("auto auto", "\"a\" \"bé\""));
        assert!(crate::style::supports::supports_declaration("grid-template", "\"aé"));
        assert!(!crate::style::supports::supports_declaration("grid-template", "\""));
    }

    // `<bg-position>`'s one- to four-value forms (Blink `ConsumeBackgroundPosition`); the halves are kept as written.
    #[test]
    fn background_positions_split_into_their_halves() {
        let halves = |v: &str| {
            let got: std::collections::HashMap<_, _> = built(&format!("background-position: {v}"))?.into_iter().collect();
            Some((got["background-position-x"].clone(), got["background-position-y"].clone()))
        };
        assert_eq!(halves("top"), Some(("center".into(), "top".into())));
        assert_eq!(halves("10px 20%"), Some(("10px".into(), "20%".into())));
        assert_eq!(halves("top left"), Some(("left".into(), "top".into())));
        assert_eq!(halves("right 5px bottom"), Some(("right 5px".into(), "bottom".into())));
        assert_eq!(halves("bottom 10px right 5px"), Some(("right 5px".into(), "bottom 10px".into())));
        assert_eq!(halves("center top 5px"), Some(("center".into(), "top 5px".into())));
        assert_eq!(halves("1px 2px, 3px 4px"), Some(("1px, 3px".into(), "2px, 4px".into())));
        assert_eq!(halves("top 10px"), None);
        assert_eq!(halves("left right"), None);
        assert_eq!(halves("center 5px top"), None, "center takes no offset");
        assert_eq!(halves("10px 20px 30px"), None);
    }

    #[test]
    fn border_radius_gives_every_corner_both_radii_when_it_is_asked_to() {
        assert_eq!(
            ex("border-radius: 4px").unwrap(),
            vec![
                ("border-top-left-radius", "4px".into()),
                ("border-top-right-radius", "4px".into()),
                ("border-bottom-right-radius", "4px".into()),
                ("border-bottom-left-radius", "4px".into()),
            ]
        );
        assert_eq!(
            ex("border-radius: 10px / 20px").unwrap(),
            vec![
                ("border-top-left-radius", "10px 20px".into()),
                ("border-top-right-radius", "10px 20px".into()),
                ("border-bottom-right-radius", "10px 20px".into()),
                ("border-bottom-left-radius", "10px 20px".into()),
            ]
        );
        assert_eq!(ex("border-radius: solid"), None);
    }

    #[test]
    fn a_grid_line_without_an_end_gets_auto_unless_it_names_an_area() {
        assert_eq!(
            ex("grid-column: 1 / 3").unwrap(),
            vec![("grid-column-start", "1".into()), ("grid-column-end", "3".into())]
        );
        assert_eq!(
            ex("grid-column: span 2").unwrap(),
            vec![("grid-column-start", "span 2".into()), ("grid-column-end", "auto".into())]
        );
        // A bare custom ident names one area, so the end is copied rather than left open.
        assert_eq!(
            ex("grid-area: header").unwrap(),
            vec![
                ("grid-row-start", "header".into()),
                ("grid-column-start", "header".into()),
                ("grid-row-end", "header".into()),
                ("grid-column-end", "header".into()),
            ]
        );
    }

    #[test]
    fn grid_area_takes_up_to_four_lines() {
        assert_eq!(
            ex("grid-area: 1 / 2 / 3 / 4").unwrap(),
            vec![
                ("grid-row-start", "1".into()),
                ("grid-column-start", "2".into()),
                ("grid-row-end", "3".into()),
                ("grid-column-end", "4".into()),
            ]
        );
        assert_eq!(ex("grid-area: 1 / 2 / 3 / 4 / 5"), None);
    }

    #[test]
    fn a_longhand_is_not_a_shorthand() {
        assert!(!is_shorthand("margin-top"));
        assert!(!is_shorthand("color"));
        assert!(is_shorthand("margin"));
        assert!(is_shorthand("gap"));
        assert!(is_shorthand("border"));
        assert!(is_shorthand("white-space"));
        assert!(is_shorthand("animation"));
        assert!(is_shorthand("transition"));
    }

    // ── `animation` and `transition` ────────────────────────────────────────────────────────────
    //
    // EVERY expected value below was read out of `/usr/bin/google-chrome` 150.0.7871.46: the
    // shorthand was assigned to `el.style` and the eight (or five) longhands read back off
    // `getComputedStyle`. None of it is from the spec.

    /// Expand a list shorthand into `(longhand name, value)` pairs, in table order.
    fn lex(decl: &str) -> Option<Vec<(&'static str, String)>> {
        let (name, value) = decl.split_once(':').unwrap();
        let out = expand_list_shorthand(name.trim(), value.trim()).expect("a list shorthand")?;
        Some(out.into_iter().map(|(p, v)| (p.name(), v)).collect())
    }

    /// The eight, as `name|duration|timing|delay|count|direction|fill|play-state`, which is the
    /// column order of the recorded table and easier to compare against it by eye.
    fn anim(decl: &str) -> Option<String> {
        let out = lex(decl)?;
        let get = |n: &str| out.iter().find(|(p, _)| *p == n).map(|(_, v)| v.clone()).unwrap();
        Some(
            [
                "animation-name",
                "animation-duration",
                "animation-timing-function",
                "animation-delay",
                "animation-iteration-count",
                "animation-direction",
                "animation-fill-mode",
                "animation-play-state",
            ]
            .map(get)
            .join("|"),
        )
    }

    fn trans(decl: &str) -> Option<String> {
        let out = lex(decl)?;
        let get = |n: &str| out.iter().find(|(p, _)| *p == n).map(|(_, v)| v.clone()).unwrap();
        Some(
            [
                "transition-property",
                "transition-duration",
                "transition-timing-function",
                "transition-delay",
                "transition-behavior",
            ]
            .map(get)
            .join("|"),
        )
    }

    #[test]
    fn animation_shorthand_matches_chrome() {
        assert_eq!(
            anim("animation: slide 4s linear 0s 1 normal none running").as_deref(),
            Some("slide|4s|linear|0s|1|normal|none|running")
        );
        // Order within a layer is free: the same eight values, written differently.
        assert_eq!(
            anim("animation: 4s linear slide").as_deref(),
            Some("slide|4s|linear|0s|1|normal|none|running")
        );
        assert_eq!(anim("animation: slide 4s").as_deref(), Some("slide|4s|ease|0s|1|normal|none|running"));
        // The first <time> is the duration and the second is the delay, wherever they sit.
        assert_eq!(anim("animation: 4s slide 2s").as_deref(), Some("slide|4s|ease|2s|1|normal|none|running"));
        assert_eq!(anim("animation: 3s 1s slide").as_deref(), Some("slide|3s|ease|1s|1|normal|none|running"));
        assert_eq!(anim("animation: slide 4s -1s").as_deref(), Some("slide|4s|ease|-1s|1|normal|none|running"));
        // The name keeps its case; it is a <custom-ident>, not a keyword.
        assert_eq!(anim("animation: Slide 4s").as_deref(), Some("Slide|4s|ease|0s|1|normal|none|running"));
        assert_eq!(anim("animation: 4s").as_deref(), Some("none|4s|ease|0s|1|normal|none|running"));
        // An omitted duration is `auto` (Chrome 150 writes `animation: none` back as `auto ease 0s ...`).
        assert_eq!(anim("animation: spin").as_deref(), Some("spin|auto|ease|0s|1|normal|none|running"));
    }

    #[test]
    fn a_keyword_takes_its_own_slot_before_it_is_a_name() {
        // Each of these is a keyword that COULD be an animation name, and Chrome gives it to the
        // other property -- but only while that property is unfilled.
        assert_eq!(anim("animation: reverse 2s").as_deref(), Some("none|2s|ease|0s|1|reverse|none|running"));
        assert_eq!(anim("animation: infinite 2s spin").as_deref(), Some("spin|2s|ease|0s|infinite|normal|none|running"));
        assert_eq!(anim("animation: forwards 4s").as_deref(), Some("none|4s|ease|0s|1|normal|forwards|running"));
        assert_eq!(anim("animation: paused 2s").as_deref(), Some("none|2s|ease|0s|1|normal|none|paused"));
        assert_eq!(anim("animation: ease 2s").as_deref(), Some("none|2s|ease|0s|1|normal|none|running"));
        // …and the SECOND one becomes the name, because the slot is taken.
        assert_eq!(anim("animation: none forwards 4s").as_deref(), Some("forwards|4s|ease|0s|1|normal|none|running"));
        assert_eq!(anim("animation: ease ease 2s").as_deref(), Some("ease|2s|ease|0s|1|normal|none|running"));
        assert_eq!(anim("animation: alternate alternate 2s").as_deref(), Some("alternate|2s|ease|0s|1|alternate|none|running"));
        assert_eq!(anim("animation: linear linear 2s").as_deref(), Some("linear|2s|linear|0s|1|normal|none|running"));
        // A bare `steps` is an ident, not an easing function -- the parens are what make it one.
        assert_eq!(anim("animation: steps 2s").as_deref(), Some("steps|2s|ease|0s|1|normal|none|running"));
    }

    #[test]
    fn animation_layers_become_comma_lists() {
        assert_eq!(
            anim("animation: grow 4s linear, spin 2s ease-in 1s infinite alternate both").as_deref(),
            Some("grow, spin|4s, 2s|linear, ease-in|0s, 1s|1, infinite|normal, alternate|none, both|running, running")
        );
        // A layer with no name still contributes its slot, as `none`.
        assert_eq!(
            anim("animation: spin 2s, 4s").as_deref(),
            Some("spin, none|2s, 4s|ease, ease|0s, 0s|1, 1|normal, normal|none, none|running, running")
        );
        // A comma inside a function is not a layer separator.
        assert_eq!(
            anim("animation: spin 2s cubic-bezier(0.1, 0.7, 1.0, 0.1)").as_deref(),
            Some("spin|2s|cubic-bezier(0.1, 0.7, 1.0, 0.1)|0s|1|normal|none|running")
        );
        assert_eq!(
            anim("animation: spin 2s steps(4, end) infinite").as_deref(),
            Some("spin|2s|steps(4, end)|0s|infinite|normal|none|running")
        );
    }

    #[test]
    fn an_animation_shorthand_chrome_rejects_is_dropped_whole() {
        // Three times for two time slots, and two numbers for one iteration-count slot. Chrome
        // leaves `el.style.cssText` empty for both.
        assert_eq!(anim("animation: 4s 5s 6s"), None);
        assert_eq!(anim("animation: slide 4s 1 2"), None);
        assert_eq!(anim("animation: spin 2s,"), None);
        // …but it is still a SHORTHAND, so the parser drops the declaration rather than treating
        // `animation` as an unknown property.
        assert!(is_shorthand("animation"));
    }

    #[test]
    fn a_css_wide_keyword_reaches_every_longhand() {
        assert_eq!(anim("animation: initial").as_deref(), Some("initial|initial|initial|initial|initial|initial|initial|initial"));
        assert_eq!(trans("transition: inherit").as_deref(), Some("inherit|inherit|inherit|inherit|inherit"));
    }

    #[test]
    fn transition_shorthand_matches_chrome() {
        assert_eq!(trans("transition: opacity 3s linear").as_deref(), Some("opacity|3s|linear|0s|normal"));
        assert_eq!(trans("transition: all 3s").as_deref(), Some("all|3s|ease|0s|normal"));
        assert_eq!(trans("transition: 3s").as_deref(), Some("all|3s|ease|0s|normal"));
        assert_eq!(trans("transition: opacity 3s 1s").as_deref(), Some("opacity|3s|ease|1s|normal"));
        assert_eq!(trans("transition: opacity").as_deref(), Some("opacity|0s|ease|0s|normal"));
        assert_eq!(trans("transition: none").as_deref(), Some("none|0s|ease|0s|normal"));
        assert_eq!(trans("transition: 2s ease-in-out 0.5s opacity").as_deref(), Some("opacity|2s|ease-in-out|0.5s|normal"));
        assert_eq!(trans("transition: 2s 3s").as_deref(), Some("all|2s|ease|3s|normal"));
        // `ease`, `linear` and `normal` are taken by their own slots, so the property stays `all`.
        assert_eq!(trans("transition: ease 2s").as_deref(), Some("all|2s|ease|0s|normal"));
        assert_eq!(trans("transition: linear 2s").as_deref(), Some("all|2s|linear|0s|normal"));
        assert_eq!(trans("transition: normal 2s").as_deref(), Some("all|2s|ease|0s|normal"));
        assert_eq!(trans("transition: opacity 3s allow-discrete").as_deref(), Some("opacity|3s|ease|0s|allow-discrete"));
        assert_eq!(
            trans("transition: opacity 3s linear, width 3s linear").as_deref(),
            Some("opacity, width|3s, 3s|linear, linear|0s, 0s|normal, normal")
        );
        assert_eq!(
            trans("transition: width 2s, height 3s ease 1s").as_deref(),
            Some("width, height|2s, 3s|ease, ease|0s, 1s|normal, normal")
        );
        // Two properties in one layer, and three times in one layer: both rejected by Chrome.
        assert_eq!(trans("transition: all all 2s"), None);
        assert_eq!(trans("transition: opacity 2s 3s 4s"), None);
    }
}
