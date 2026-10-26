//! Logical properties, and the physical property each one IS on a given element.
//!
//! `margin-block-start` is not a second margin. CSS Logical 1 §4 makes every logical property one
//! spelling of a physical one, chosen by the element's `writing-mode` and `direction`: under
//! `horizontal-tb` / `ltr` it is `margin-top`, under `vertical-rl` it is `margin-right`. The two
//! share one computed value, and when both are declared the later one in cascade order wins —
//! measured on `/usr/bin/google-chrome` 150.0.7871.46:
//!
//! ```text
//! .a { margin-left: 1px; margin-inline-start: 2px }   margin-left 2px
//! .a { margin-inline-start: 2px; margin-left: 1px }   margin-left 1px
//! <p> (UA: margin-block-start: 1em)                   margin-top 16px   (korvus before: 0px)
//! ```
//!
//! That last row is why this file exists. Chromium's own user-agent sheet is written in logical
//! properties, so an engine that kept them as independent longhands gave every `<p>`, heading, list
//! and blockquote on the web zero margin and zero padding.
//!
//! # Where the mapping happens
//!
//! In the CASCADE (`compute.rs`): a logical declaration competes for its physical slot with its own
//! cascade key, so "later wins" is the ordinary comparison and no second rule is needed. Nothing is
//! ever stored under a logical property. Reading one back (`getComputedStyle(el).marginInlineStart`)
//! maps it again, in [`crate::style::computed::StyleArena::value`], against the element's own
//! computed `writing-mode` and `direction` — the same two facts, so the two directions agree.

use crate::style::property::PropertyId;

/// The four physical sides, in CSS order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// Which physical side each of the four logical sides lands on for one `writing-mode`/`direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Orientation {
    block_start: Side,
    block_end: Side,
    inline_start: Side,
    inline_end: Side,
    /// The inline axis is vertical, so `inline-size` is `height`.
    vertical: bool,
}

impl Orientation {
    /// CSS Writing Modes 4 §6.2's table. `writing_mode` and `direction` are COMPUTED values; an
    /// unknown keyword is the initial one, which is what Chrome computes it to.
    pub fn of(writing_mode: &str, direction: &str) -> Orientation {
        let rtl = direction.trim().eq_ignore_ascii_case("rtl");
        let (block_start, block_end, vertical, inline_ltr) = match writing_mode.trim().to_ascii_lowercase().as_str() {
            "vertical-rl" | "sideways-rl" => (Side::Right, Side::Left, true, (Side::Top, Side::Bottom)),
            "vertical-lr" => (Side::Left, Side::Right, true, (Side::Top, Side::Bottom)),
            // `sideways-lr` runs its inline axis bottom to top.
            "sideways-lr" => (Side::Left, Side::Right, true, (Side::Bottom, Side::Top)),
            _ => {
                return Orientation {
                    block_start: Side::Top,
                    block_end: Side::Bottom,
                    inline_start: if rtl { Side::Right } else { Side::Left },
                    inline_end: if rtl { Side::Left } else { Side::Right },
                    vertical: false,
                }
            }
        };
        let (inline_start, inline_end) = if rtl { (inline_ltr.1, inline_ltr.0) } else { inline_ltr };
        Orientation { block_start, block_end, inline_start, inline_end, vertical }
    }

    /// `horizontal-tb` / `ltr`, the initial pair.
    pub const HORIZONTAL_LTR: Orientation = Orientation {
        block_start: Side::Top,
        block_end: Side::Bottom,
        inline_start: Side::Left,
        inline_end: Side::Right,
        vertical: false,
    };
}

/// Is `property` a logical property this engine maps?
pub fn is_logical(property: PropertyId) -> bool {
    to_physical(property, Orientation::HORIZONTAL_LTR).is_some()
}

/// The physical property `property` is under `o`, or `None` for a property that is not logical.
pub fn to_physical(property: PropertyId, o: Orientation) -> Option<PropertyId> {
    use PropertyId::*;
    // One logical side -> the property of that family on the physical side.
    let side = |logical: Side, family: [PropertyId; 4]| -> PropertyId {
        match logical {
            Side::Top => family[0],
            Side::Right => family[1],
            Side::Bottom => family[2],
            Side::Left => family[3],
        }
    };
    const MARGIN: [PropertyId; 4] = [MarginTop, MarginRight, MarginBottom, MarginLeft];
    const PADDING: [PropertyId; 4] = [PaddingTop, PaddingRight, PaddingBottom, PaddingLeft];
    const INSET: [PropertyId; 4] = [Top, Right, Bottom, Left];
    const BW: [PropertyId; 4] = [BorderTopWidth, BorderRightWidth, BorderBottomWidth, BorderLeftWidth];
    const BS: [PropertyId; 4] = [BorderTopStyle, BorderRightStyle, BorderBottomStyle, BorderLeftStyle];
    const BC: [PropertyId; 4] = [BorderTopColor, BorderRightColor, BorderBottomColor, BorderLeftColor];
    const SM: [PropertyId; 4] = [ScrollMarginTop, ScrollMarginRight, ScrollMarginBottom, ScrollMarginLeft];
    const SP: [PropertyId; 4] = [ScrollPaddingTop, ScrollPaddingRight, ScrollPaddingBottom, ScrollPaddingLeft];
    // A pair of axis properties: (horizontal, vertical).
    let axis = |inline: bool, pair: (PropertyId, PropertyId)| -> PropertyId {
        // The inline axis is horizontal unless the writing mode is vertical.
        if inline != o.vertical {
            pair.0
        } else {
            pair.1
        }
    };
    // A corner named (block side, inline side): the physical corner is the vertical side of the two
    // plus the horizontal one.
    let corner = |block: Side, inline: Side| -> PropertyId {
        let (v, h) = if matches!(block, Side::Top | Side::Bottom) { (block, inline) } else { (inline, block) };
        match (v, h) {
            (Side::Top, Side::Left) => BorderTopLeftRadius,
            (Side::Top, _) => BorderTopRightRadius,
            (_, Side::Left) => BorderBottomLeftRadius,
            _ => BorderBottomRightRadius,
        }
    };
    Some(match property {
        MarginBlockStart => side(o.block_start, MARGIN),
        MarginBlockEnd => side(o.block_end, MARGIN),
        MarginInlineStart => side(o.inline_start, MARGIN),
        MarginInlineEnd => side(o.inline_end, MARGIN),
        PaddingBlockStart => side(o.block_start, PADDING),
        PaddingBlockEnd => side(o.block_end, PADDING),
        PaddingInlineStart => side(o.inline_start, PADDING),
        PaddingInlineEnd => side(o.inline_end, PADDING),
        InsetBlockStart => side(o.block_start, INSET),
        InsetBlockEnd => side(o.block_end, INSET),
        InsetInlineStart => side(o.inline_start, INSET),
        InsetInlineEnd => side(o.inline_end, INSET),
        BorderBlockStartWidth => side(o.block_start, BW),
        BorderBlockEndWidth => side(o.block_end, BW),
        BorderInlineStartWidth => side(o.inline_start, BW),
        BorderInlineEndWidth => side(o.inline_end, BW),
        BorderBlockStartStyle => side(o.block_start, BS),
        BorderBlockEndStyle => side(o.block_end, BS),
        BorderInlineStartStyle => side(o.inline_start, BS),
        BorderInlineEndStyle => side(o.inline_end, BS),
        BorderBlockStartColor => side(o.block_start, BC),
        BorderBlockEndColor => side(o.block_end, BC),
        BorderInlineStartColor => side(o.inline_start, BC),
        BorderInlineEndColor => side(o.inline_end, BC),
        ScrollMarginBlockStart => side(o.block_start, SM),
        ScrollMarginBlockEnd => side(o.block_end, SM),
        ScrollMarginInlineStart => side(o.inline_start, SM),
        ScrollMarginInlineEnd => side(o.inline_end, SM),
        ScrollPaddingBlockStart => side(o.block_start, SP),
        ScrollPaddingBlockEnd => side(o.block_end, SP),
        ScrollPaddingInlineStart => side(o.inline_start, SP),
        ScrollPaddingInlineEnd => side(o.inline_end, SP),
        InlineSize => axis(true, (Width, Height)),
        BlockSize => axis(false, (Width, Height)),
        MinInlineSize => axis(true, (MinWidth, MinHeight)),
        MinBlockSize => axis(false, (MinWidth, MinHeight)),
        MaxInlineSize => axis(true, (MaxWidth, MaxHeight)),
        MaxBlockSize => axis(false, (MaxWidth, MaxHeight)),
        ContainIntrinsicInlineSize => axis(true, (ContainIntrinsicWidth, ContainIntrinsicHeight)),
        ContainIntrinsicBlockSize => axis(false, (ContainIntrinsicWidth, ContainIntrinsicHeight)),
        OverflowInline => axis(true, (OverflowX, OverflowY)),
        OverflowBlock => axis(false, (OverflowX, OverflowY)),
        OverscrollBehaviorInline => axis(true, (OverscrollBehaviorX, OverscrollBehaviorY)),
        OverscrollBehaviorBlock => axis(false, (OverscrollBehaviorX, OverscrollBehaviorY)),
        BorderStartStartRadius => corner(o.block_start, o.inline_start),
        BorderStartEndRadius => corner(o.block_start, o.inline_end),
        BorderEndStartRadius => corner(o.block_end, o.inline_start),
        BorderEndEndRadius => corner(o.block_end, o.inline_end),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use PropertyId::*;

    #[test]
    fn horizontal_ltr_is_the_mapping_the_ua_sheet_relies_on() {
        let o = Orientation::of("horizontal-tb", "ltr");
        assert_eq!(to_physical(MarginBlockStart, o), Some(MarginTop));
        assert_eq!(to_physical(MarginInlineStart, o), Some(MarginLeft));
        assert_eq!(to_physical(PaddingInlineStart, o), Some(PaddingLeft));
        assert_eq!(to_physical(InlineSize, o), Some(Width));
        assert_eq!(to_physical(BorderStartEndRadius, o), Some(BorderTopRightRadius));
        assert_eq!(to_physical(MarginTop, o), None, "a physical property is not logical");
    }

    #[test]
    fn rtl_swaps_the_inline_sides_and_vertical_modes_turn_the_axes() {
        let rtl = Orientation::of("horizontal-tb", "rtl");
        assert_eq!(to_physical(MarginInlineStart, rtl), Some(MarginRight));
        assert_eq!(to_physical(BorderStartStartRadius, rtl), Some(BorderTopRightRadius));
        let vrl = Orientation::of("vertical-rl", "ltr");
        assert_eq!(to_physical(MarginBlockStart, vrl), Some(MarginRight));
        assert_eq!(to_physical(MarginInlineStart, vrl), Some(MarginTop));
        assert_eq!(to_physical(InlineSize, vrl), Some(Height));
        assert_eq!(to_physical(BorderStartStartRadius, vrl), Some(BorderTopRightRadius));
        let vlr = Orientation::of("vertical-lr", "rtl");
        assert_eq!(to_physical(PaddingBlockEnd, vlr), Some(PaddingRight));
        assert_eq!(to_physical(PaddingInlineStart, vlr), Some(PaddingBottom));
    }
}
