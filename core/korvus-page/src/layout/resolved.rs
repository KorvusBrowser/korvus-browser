//! Resolved values: the ones `getComputedStyle` answers with the **used** value, not the computed one.
//!
//! # Why this is not part of the cascade
//!
//! CSSOM §resolved values splits `getComputedStyle`'s 475 properties in two. Most report the
//! *computed* value — `color: red` is `rgb(255, 0, 0)` whatever the layout does, and the cascade can
//! answer it alone. A short list reports the **used** value, which only layout knows: `width: 50%`
//! reads back as `720px` because the containing block turned out to be 1440px wide, and `top: 10%`
//! on a positioned box reads back in pixels for the same reason.
//!
//! Answering those from the cascade is not "slightly wrong". It is a different KIND of answer, and a
//! page can tell in one line: `getComputedStyle(el).width` returning the string `"50%"` is something
//! no browser has ever done. It is also self-inconsistent with the geometry the same element already
//! reports through `offsetWidth` and `getBoundingClientRect`, which is exactly the cross-check a
//! detector runs.
//!
//! # What is here, and what each one needs
//!
//! | property | used value from |
//! |---|---|
//! | `width` / `height`, `inline-size` / `block-size` | the box's **content** box |
//! | `margin-*`, `padding-*` (+ logical) | taffy's resolved margin/padding rects |
//! | `top` / `right` / `bottom` / `left` (+ `inset-*`) | the box's offset from its containing block |
//! | `transform-origin`, `perspective-origin` | percentages against the **border** box |
//!
//! Everything else keeps its computed value, and so does every element here that generated no box:
//! `display: none` reports `width: 50%` in a real browser too, because there is no used value to
//! report. That is not a shortcut — it is the spec's own rule, and it is what the control does.
//!
//! # Writing mode
//!
//! The logical properties map onto the physical ones for `horizontal-tb`, which is the only writing
//! mode this engine lays out. `inline-size` is `width` and `block-size` is `height`; under
//! `vertical-rl` they would swap. The mapping is one place (`physical_of`) so that day is one edit.

use crate::layout::geometry::Rect;
use crate::layout::style::CssPosition;
use crate::layout::tree::BoxTree;
use crate::style::property::PropertyId;
use crate::value::{serialize_px, LengthPercentage};
use cssparser::{Parser, ParserInput};

/// Does this property's resolved value come from layout rather than from the cascade?
///
/// The list is CSSOM's, intersected with what this engine's layout can actually answer. A property
/// that is on CSSOM's list and NOT here would be answered with a guess, which is worse than
/// answering with the computed value: the computed value is at least a value the spec sanctions for
/// a box that does not exist.
pub fn resolves_from_layout(property: PropertyId) -> bool {
    use PropertyId::*;
    matches!(
        property,
        Width
            | Height
            | InlineSize
            | BlockSize
            | MarginTop
            | MarginRight
            | MarginBottom
            | MarginLeft
            | MarginBlockStart
            | MarginBlockEnd
            | MarginInlineStart
            | MarginInlineEnd
            | PaddingTop
            | PaddingRight
            | PaddingBottom
            | PaddingLeft
            | PaddingBlockStart
            | PaddingBlockEnd
            | PaddingInlineStart
            | PaddingInlineEnd
            | Top
            | Right
            | Bottom
            | Left
            | InsetBlockStart
            | InsetBlockEnd
            | InsetInlineStart
            | InsetInlineEnd
            | TransformOrigin
            | PerspectiveOrigin
    )
}

/// The physical property a logical one maps to under `horizontal-tb` / `ltr`.
fn physical_of(property: PropertyId) -> PropertyId {
    use PropertyId::*;
    match property {
        InlineSize => Width,
        BlockSize => Height,
        MarginBlockStart => MarginTop,
        MarginBlockEnd => MarginBottom,
        MarginInlineStart => MarginLeft,
        MarginInlineEnd => MarginRight,
        PaddingBlockStart => PaddingTop,
        PaddingBlockEnd => PaddingBottom,
        PaddingInlineStart => PaddingLeft,
        PaddingInlineEnd => PaddingRight,
        InsetBlockStart => Top,
        InsetBlockEnd => Bottom,
        InsetInlineStart => Left,
        InsetInlineEnd => Right,
        other => other,
    }
}

/// The four edges of a box, in the order the physical properties name them.
#[derive(Clone, Copy)]
struct Edges {
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
}

impl BoxTree {
    /// The used value of `property` for the box `index`, or `None` when layout has nothing to say.
    ///
    /// `None` is a real answer, not a failure: it means "keep the computed value", which is what a
    /// browser reports for an element that generated no box.
    pub fn resolved_value(&self, index: u32, property: PropertyId, computed: &str) -> Option<String> {
        use PropertyId::*;
        let property = physical_of(property);
        let layout = self.layout(index);
        // The box `width` and `height` report: the BORDER box under `box-sizing: border-box`, else the content box --
        // the border box less its borders, padding and any scrollbar (Blink's `ComputedStyleUtils::UsedBoxSize`).
        // Chrome 150: `width:100px; height:50px; padding:10px; border:3px solid; box-sizing:border-box;
        // overflow:scroll` reads `100px`/`50px`, where this read the content box, `59px`/`9px`.
        let border_box = self.style_of_box(index).box_sizing == taffy::style::BoxSizing::BorderBox;
        let content = |extent: f32, near: f32, far: f32, pad_near: f32, pad_far: f32, bar: f32| {
            if border_box {
                return extent;
            }
            (extent - near - far - pad_near - pad_far - bar).max(0.0)
        };
        // A NON-REPLACED INLINE keeps the computed `width` and `height`, however real its box is.
        //
        // CSSOM carves it out, and Chrome 150 was asked rather than the spec (2026-09-14): a
        // `<span>` reporting a 74.72 × 17 rect answered `getComputedStyle(...).width` of **auto**,
        // and one wrapped across two lines answered `auto` too. A `<span style="width:120px">`
        // answered `120px` — the *computed* value passing straight through, while its rect stayed
        // 37.36 wide, because `width` does not apply to an inline box at all. Both are "the
        // computed value", which is exactly what `None` here means.
        //
        // `inline-block` and a replaced inline are NOT carved out and answer their used value —
        // Chrome said `50px` for the first and `30px` for an `<img width=30>` — which is why the
        // test is on [`BoxLevel::Inline`] alone and not on "is it inline-level".
        //
        // MARGIN AND PADDING are carved out on the same box. Blink hands back a used
        // margin or padding only for an element with a box of its own, and a non-replaced inline has
        // none -- it reports the computed value, which a percentage makes visible. Chrome 150,
        // measured 2026-09-26 (`tools/realweb/seed/ss/inline.html`): `<span class=k>` under
        // `.k{padding-left:24px;margin-left:5px;padding-top:3px}` reads `24px/5px/3px`, and one with
        // `style="padding-right:7%"` reads `7%`. This engine read all four as `0px`: the inline's
        // taffy node carries no margin or padding, the inline formatting pass does. (An earlier
        // `5px` for `padding-top` on an inline was that same computed value, not a used one.)
        //
        // The insets are not carved out.
        if matches!(
            property,
            Width | Height | MarginTop | MarginRight | MarginBottom | MarginLeft | PaddingTop | PaddingRight | PaddingBottom | PaddingLeft
        ) && self.level_of(index) == crate::layout::style::BoxLevel::Inline
        {
            return None;
        }
        // A margin or padding that is a LENGTH reads back as written (Blink's `MarginTop`/`PaddingTop`
        // `CSSValueFromComputedStyleInternal`: `IsFixed()` answers the computed value); only a percentage
        // or `auto` is a used value. Chrome 150: `padding: 2.715px` reads `2.715px` -- this read the
        // LayoutUnit it was laid out at, `2.70312px`.
        if matches!(property, MarginTop | MarginRight | MarginBottom | MarginLeft | PaddingTop | PaddingRight | PaddingBottom | PaddingLeft)
            && computed.trim().ends_with("px")
        {
            return None;
        }
        match property {
            Width => Some(serialize_px(content(
                layout.size.width,
                layout.border.left,
                layout.border.right,
                layout.padding.left,
                layout.padding.right,
                layout.scrollbar_size.width,
            ))),
            Height => Some(serialize_px(content(
                layout.size.height,
                layout.border.top,
                layout.border.bottom,
                layout.padding.top,
                layout.padding.bottom,
                layout.scrollbar_size.height,
            ))),
            MarginTop if self.quirk_margin_dropped(index) => None,
            MarginTop => Some(serialize_px(layout.margin.top)),
            MarginRight => Some(serialize_px(layout.margin.right)),
            MarginBottom => Some(serialize_px(layout.margin.bottom)),
            MarginLeft => Some(serialize_px(layout.margin.left)),
            PaddingTop => Some(serialize_px(layout.padding.top)),
            PaddingRight => Some(serialize_px(layout.padding.right)),
            PaddingBottom => Some(serialize_px(layout.padding.bottom)),
            PaddingLeft => Some(serialize_px(layout.padding.left)),
            Top | Right | Bottom | Left if self.css_position(index) == CssPosition::Sticky => {
                // Blink's sticky constraints: a side that is not `auto` resolved against the nearest
                // scroll container's client box (`MinimumValueForLength` over the constraining rect),
                // an `auto` one its computed value. Chrome 150 in a 1920x950 viewport: `top: 5%`
                // reads `47.5px`, `left: calc(10% + 3px)` `195px` (they read as written here).
                let style = self.style_of_box(index);
                let (inset, horizontal) = match property {
                    Top => (style.inset.top, false),
                    Bottom => (style.inset.bottom, false),
                    Right => (style.inset.right, true),
                    _ => (style.inset.left, true),
                };
                let (w, h) = self.sticky_constraining_size(index);
                resolve_inset_unit(inset, if horizontal { w } else { h }).map(serialize_px)
            }
            Top | Right | Bottom | Left => {
                let insets = self.used_insets(index)?;
                Some(serialize_px(match property {
                    Top => insets.top,
                    Right => insets.right,
                    Bottom => insets.bottom,
                    _ => insets.left,
                }))
            }
            TransformOrigin | PerspectiveOrigin => Some(used_origin_string(
                computed,
                layout.size.width,
                layout.size.height,
                property == TransformOrigin,
            )),
            _ => None,
        }
    }

    /// `top`/`right`/`bottom`/`left` as the CSSOM reports them.
    ///
    /// `None` for a statically positioned box, which is the spec's own answer: the resolved value of
    /// an inset on a non-positioned element is its *computed* value, so the cascade's `auto` (or the
    /// author's `10%`, unresolved) is right and layout must not overwrite it.
    fn used_insets(&self, index: u32) -> Option<Edges> {
        let position = self.css_position(index);
        let style = self.style_of_box(index);
        let containing = self.containing_block(index, position)?;
        match position {
            CssPosition::Static => None,
            // A sticky box answers from its sticky constraints in Blink, and its computed value
            // otherwise; this engine keeps the computed value.
            CssPosition::Sticky => None,
            CssPosition::Relative => {
                // Blink's `ValueForPositionOffset` for a relatively positioned box: a length is its own
                // value, a percentage resolves against the containing block, and only an `auto` side
                // is derived -- the negated opposite (CSS 2.1 §9.4.3), or 0 when both are `auto`.
                // Chrome 150: `position: relative; inset: 1px 2px` reads `1px 2px` (both pairs as
                // written; this read `1px -2px -1px 2px`, the over-constrained used shift).
                // The derived side is the opposite's float value, negated (`-FloatValueForLength`).
                let pair = |a: taffy::style::LengthPercentageAuto, b: taffy::style::LengthPercentageAuto, basis: f32| {
                    match (resolve_inset_unit(a, basis), resolve_inset_unit(b, basis)) {
                        (Some(x), Some(y)) => (x, y),
                        (Some(x), None) => (x, -resolve_inset(a, basis).unwrap_or(0.0)),
                        (None, Some(y)) => (-resolve_inset(b, basis).unwrap_or(0.0), y),
                        (None, None) => (0.0, 0.0),
                    }
                };
                let (left, right) = pair(style.inset.left, style.inset.right, containing.width);
                let (top, bottom) = pair(style.inset.top, style.inset.bottom, containing.height);
                Some(Edges { top, right, bottom, left })
            }
            CssPosition::Absolute | CssPosition::Fixed => {
                // The used inset of an absolutely positioned box is the distance from the containing
                // block's padding edge to the box's MARGIN edge -- and it is a used value even when
                // the author wrote `auto`, because the box ended up somewhere. Taking it from where
                // the box actually is means `auto` and a specified length are answered by the same
                // code, and neither can disagree with `getBoundingClientRect`.
                let border_box = self.border_box(index);
                let layout = self.layout(index);
                let left = border_box.x - containing.x - layout.margin.left;
                let top = border_box.y - containing.y - layout.margin.top;
                let right = containing.width
                    - left
                    - layout.margin.left
                    - border_box.width
                    - layout.margin.right;
                let bottom = containing.height
                    - top
                    - layout.margin.top
                    - border_box.height
                    - layout.margin.bottom;
                Some(Edges { top, right, bottom, left })
            }
        }
    }

    /// The containing block, in viewport coordinates: the padding box of the nearest ancestor that
    /// establishes one, or the initial containing block (the viewport) when there is no such
    /// ancestor.
    ///
    /// `relative` resolves its percentages against its parent's *content* box, because it is still
    /// in flow; `absolute` measures from the nearest positioned ancestor's *padding* box. Those are
    /// two different rectangles and using one for both would put every absolutely positioned box off
    /// by the ancestor's padding.
    fn containing_block(&self, index: u32, position: CssPosition) -> Option<Rect> {
        match position {
            CssPosition::Static => None,
            // the nearest block container that is an element's own box: an inline-level box's parent here is the
            // anonymous box of its inline formatting context, as tall as its lines (reCAPTCHA's tile `<img>`,
            // `top: -100%` in a 126px tile, read -381px for Chrome's -126px)
            CssPosition::Relative | CssPosition::Sticky => {
                let mut parent = self.parent_of(index);
                while let Some(p) = parent {
                    if self.box_of(self.node_of(p)) == Some(p) {
                        break;
                    }
                    parent = self.parent_of(p);
                }
                Some(match parent {
                    Some(parent) => self.content_box(parent),
                    None => self.initial_containing_block(),
                })
            }
            // A fixed box's containing block is the viewport, full stop — no ancestor establishes
            // one for it (transforms and filters do, and this engine has neither in layout).
            CssPosition::Fixed => Some(self.initial_containing_block()),
            CssPosition::Absolute => {
                let mut current = self.parent_of(index);
                while let Some(ancestor) = current {
                    if self.css_position(ancestor).is_positioned() {
                        return Some(self.padding_box(ancestor));
                    }
                    current = self.parent_of(ancestor);
                }
                Some(self.initial_containing_block())
            }
        }
    }
}

/// One inset edge: `auto` stays `auto` (as `None`), a length or percentage becomes pixels.
impl BoxTree {
    /// The client box of the scroll container a sticky box sticks in: the nearest scrolling ancestor's
    /// padding box less its scrollbars, or the layout viewport.
    fn sticky_constraining_size(&self, index: u32) -> (f32, f32) {
        let mut cur = self.parent_of(index);
        while let Some(s) = cur {
            if self.parent_of(s).is_none() {
                break;
            }
            if self.box_of(self.node_of(s)) == Some(s) && self.is_scroll_container(s) {
                let l = self.layout(s);
                let w = l.size.width - l.border.left - l.border.right - l.scrollbar_size.width;
                let h = l.size.height - l.border.top - l.border.bottom - l.scrollbar_size.height;
                return (w.max(0.0), h.max(0.0));
            }
            cur = self.parent_of(s);
        }
        let v = self.viewport();
        (v.width, v.height)
    }
}

pub(crate) fn resolve_inset(inset: taffy::style::LengthPercentageAuto, basis: f32) -> Option<f32> {
    use taffy::style::CompactLength;
    let raw = inset.into_raw();
    match raw.tag() {
        CompactLength::LENGTH_TAG => Some(raw.value()),
        // taffy stores a percentage as a fraction, not as 0..100.
        CompactLength::PERCENT_TAG => Some(raw.value() * basis),
        _ if raw.is_calc() => Some(crate::style::math::resolve_ptr(raw.calc_value(), basis)),
        _ => None,
    }
}

/// [`resolve_inset`] as Blink's `ValueForLength` answers a side's OWN inset: a length as written, a
/// percentage or `calc()` truncated to a `LayoutUnit` (`5%` of 96px is 4.79688px, not 4.8px).
pub(crate) fn resolve_inset_unit(inset: taffy::style::LengthPercentageAuto, basis: f32) -> Option<f32> {
    let v = resolve_inset(inset, basis)?;
    Some(if inset.into_raw().tag() == taffy::style::CompactLength::LENGTH_TAG { v } else { crate::layout::style::layout_unit(v) })
}

/// `transform-origin` / `perspective-origin`, with percentages resolved against the border box.
///
/// The computed value is kept as written (`50% 50%`, `left top`, `10px 2em 5px`), so this parses it
/// back rather than re-deriving it: the cascade already turned `2em` into pixels, and the only thing
/// left to do is what layout knows -- what 50% of this box is. A third component is a length by
/// definition (a z-offset cannot be a percentage) and is passed through; Chrome drops it when it is
/// zero for `transform-origin` and never writes one for `perspective-origin`, which has only two.
fn used_origin_string(computed: &str, box_width: f32, box_height: f32, keep_z: bool) -> String {
    let bases = [box_width, box_height, 0.0];
    let mut parts: Vec<String> = Vec::new();
    for (i, token) in computed.split_ascii_whitespace().take(3).enumerate() {
        let basis = bases[i.min(2)];
        let resolved = match token.to_ascii_lowercase().as_str() {
            "left" | "top" => Some(0.0),
            "center" => Some(basis / 2.0),
            "right" | "bottom" => Some(basis),
            _ => {
                let mut pi = ParserInput::new(token);
                let mut p = Parser::new(&mut pi);
                match LengthPercentage::parse(&mut p) {
                    Ok(lp) if p.expect_exhausted().is_ok() => {
                        // The cascade has already made every length absolute, so `to_px` here only
                        // ever has a percentage left to do.
                        Some(lp.to_px(&Default::default(), basis))
                    }
                    _ => None,
                }
            }
        };
        match resolved {
            Some(px) => parts.push(serialize_px(px)),
            // A component we cannot resolve means the whole value is one we do not model; handing
            // back a half-resolved string would be worse than handing back what the cascade said.
            None => return computed.to_string(),
        }
    }
    while parts.len() < 2 {
        parts.push(serialize_px(0.0));
    }
    if !keep_z && parts.len() > 2 {
        parts.truncate(2);
    }
    if keep_z && parts.len() == 3 && parts[2] == "0px" {
        parts.truncate(2);
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_resolves_its_percentages_against_the_border_box() {
        assert_eq!(used_origin_string("50% 50%", 1012.0, 392.0, true), "506px 196px");
        assert_eq!(used_origin_string("50% 50%", 1440.0, 24.0, true), "720px 12px");
        assert_eq!(used_origin_string("left top", 100.0, 40.0, true), "0px 0px");
        assert_eq!(used_origin_string("right bottom", 100.0, 40.0, true), "100px 40px");
    }

    // Chrome writes two components when the z-offset is zero, and three when it is not.
    #[test]
    fn a_zero_z_offset_is_not_written_and_a_real_one_is() {
        assert_eq!(used_origin_string("50% 50% 0px", 100.0, 40.0, true), "50px 20px");
        assert_eq!(used_origin_string("50% 50% 5px", 100.0, 40.0, true), "50px 20px 5px");
        // `perspective-origin` has no third component at all.
        assert_eq!(used_origin_string("50% 50% 5px", 100.0, 40.0, false), "50px 20px");
    }

    // A value we do not model must come back untouched rather than half-resolved.
    #[test]
    fn an_unmodelled_origin_is_left_alone() {
        assert_eq!(used_origin_string("calc(1px + 2%)", 100.0, 40.0, true), "calc(1px + 2%)");
    }
}
