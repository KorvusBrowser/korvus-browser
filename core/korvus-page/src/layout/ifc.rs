//! The inline formatting context: line boxes, and the fragments an inline element is split into.
//!
//! # Why this is not taffy's
//!
//! taffy implements block, flexbox and grid. It has **no inline algorithm** and no
//! `Display::Inline`, because an inline box is not a box: a `<span>` spanning three lines is three
//! rectangles, not one, and nothing in a box-tree layout engine has that shape. So this is the one
//! formatting context written here rather than driven through taffy, and it is wired in as an extra
//! arm of `BoxTree::compute_child_layout` — an anonymous IFC box is a taffy child of its block
//! container, taffy asks it for a size like any other node, and inside that call this file lays out
//! its lines and hands back the block-axis total.
//!
//! Atomic inlines (`inline-block`, and so an `<input>` or a `<button>`) stay real taffy children of
//! the IFC box and are sized by re-entering taffy through `compute_child_layout`. That is the seam
//! that lets a control with a flex interior sit on a line without this file knowing about flex.
//!
//! # What a line box is, measured rather than reasoned
//!
//! Every number below is Chrome 150.0.7871.46's, read over CDP on 2026-09-14 from pages built for
//! the question (`--headless=new`, 1920×1080, 16px Arial unless stated). Three rules:
//!
//! **1. An inline box's fragment is as tall as its own font's CONTENT AREA — ascent + descent — and
//! not as tall as the line.** A `<span>` of 16px Arial in an 18px line box reports height **17**,
//! and it still reports 17 inside a `line-height: 40px` block. 32px Arial reports 36; 8px Arial, 9.
//!
//! **2. Every box on a line shares one baseline**, and a box's top is `baseline - ascent`. On
//! `base <span style="font-size:32px">BIG</span> <span style="font-size:8px">sm</span> base` Chrome
//! answered tops of 131, 116 and 138 for ascents of 14, 29 and 7 — one baseline at 145, three times.
//!
//! **3. The line box is the half-leading rule, with the halves split by `floor`.** For each box,
//! `leading = line-height - (ascent + descent)`; it contributes `ascent + floor(leading/2)` above
//! the baseline and `descent + (leading - floor(leading/2))` below. The line is
//! `max(above) + max(below)`. Three measurements pin the `floor`, one with *negative* leading:
//!
//! | page | leading | Chrome | this rule |
//! |---|---:|---:|---|
//! | 16px Arial, `line-height: normal` (18) | 1 | line 18, inline at the line's TOP | above 14, below 4 |
//! | `line-height: 40px`, 16px text | 23 | line 40, inline top 11px down | 14 + 11, 3 + 12 |
//! | `line-height: 30px`, 32px text beside 16px | −6 and 13 | line 36 | max(26, 20) + max(4, 10) |
//!
//! Rounding the half instead gets row 1 right by luck — both halves of 1 are 0.5, and 18 − 17 = 1
//! either way — and row 3 wrong, which is the same shape of error
//! [`crate::layout::inline::FontSpec::line_height`] documents for `normal` line spacing.
//!
//! # The strut
//!
//! A non-empty line also carries the **block's own** font as a floor on its height, which is why
//! `<div style="font-size:40px"><span style="font-size:8px">x</span></div>` is a 40px-ish line in
//! every browser and not a 9px one. A line with no content carries nothing: Chrome reports
//! `<div><span></span></div>` as a **zero-height** block and gives the span a 0×0 rect, so an empty
//! inline does not bring a line box into existence.

use crate::layout::geometry::Rect;
use crate::layout::style::TextAlignKind;
use crate::layout::tree::{AtomicBasis, BoxTree};
use taffy::geometry::Size;
use taffy::compute::BlockContext;
use taffy::style::{AvailableSpace, Clear};
use taffy::tree::{LayoutInput, LayoutOutput, RunMode};

/// One piece of inline content, in document order. An IFC box holds a flat list of these.
///
/// Flat rather than nested because line breaking is a single left-to-right walk and a nested walk
/// would have to carry its own stack anyway — `Open`/`Close` *are* that stack, made explicit, and
/// they are what lets one inline box contribute a fragment per line without being visited twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// An anonymous text box. Its text is already whitespace-collapsed.
    Text(u32),
    /// The start of a non-atomic inline box.
    Open(u32),
    /// Its end.
    Close(u32),
    /// An atomic inline: one taffy child of the IFC box, placed on the line as a unit.
    Atomic(u32),
    /// A forced break — `<br>`. It is a box of its own so that it can carry a rect, which Chrome
    /// gives it: zero wide, one content area tall, at the end of the line it broke.
    Break(u32),
}

impl Item {
    /// The box this item is about.
    pub fn box_index(self) -> u32 {
        match self {
            Item::Text(b) | Item::Open(b) | Item::Close(b) | Item::Atomic(b) | Item::Break(b) => b,
        }
    }
}

/// One box's vertical contribution to a line, by rule 3 above.
#[derive(Debug, Clone, Copy)]
pub(super) struct Vertical {
    /// From the baseline up to the top of the line box this content demands.
    pub above: f32,
    /// And down to its bottom.
    pub below: f32,
    /// The content area's own ascent — a fragment's top is `baseline - ascent`.
    pub ascent: f32,
    /// The fragment's height: ascent + descent for text, the border box for an atomic inline.
    pub height: f32,
}

/// A fragment being accumulated on the current line, before the line's baseline is known.
#[derive(Debug, Clone, Copy)]
struct Pending {
    b: u32,
    x: f32,
    width: f32,
    v: Vertical,
    /// Text or a forced break -- what the line-height quirk counts as a line's text.
    text: bool,
    /// A non-atomic inline box, which the line-height quirk leaves out of an otherwise textless line.
    inline_box: bool,
    /// How far `vertical-align` moves it down from the baseline it would sit on (up when negative),
    /// its enclosing inline boxes' shifts included -- from its anchor's baseline when it has one.
    dy: f32,
    /// The `vertical-align: top` / `bottom` box it is placed with, if any (see [`Anchor`]).
    anchor: Anchor,
}

/// A `vertical-align: top` or `bottom` box (by index): it and what it contains are aligned to the
/// line box's top or bottom edge once the rest of the line has its height (CSS 2 §10.8.1).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Anchor {
    None,
    Top(u32),
    Bottom(u32),
}

/// An inline box open across the current line: where its fragment on this line began.
#[derive(Debug, Clone, Copy)]
struct Open {
    b: u32,
    start_x: f32,
    v: Vertical,
    dy: f32,
    anchor: Anchor,
}

/// How wide the lines may be, and what the IFC's own width then is.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Fit {
    /// Wrap at this width, and be this wide. A block-level anonymous box fills its container.
    Definite(f32),
    /// Wrap at this width, and be as wide as the widest line that came out: the FIT-CONTENT answer,
    /// `min(max-content, max(min-content, available))`. This is what taffy asks when it measures a
    /// box with space on offer and no width decided — a `width: fit-content` block, an absolutely
    /// positioned box — and answering with the offered width made every such box fill its
    /// container. Chrome 150 gives `<button style="display:block">go</button>` 17.80px (16px Arial),
    /// not the 1904px this used to answer.
    Available(f32),
    /// "How wide would you be on one line" — shrink-to-fit's question. No wrapping.
    Max,
    /// Break at every opportunity; be as wide as the widest unbreakable piece.
    Min,
}

impl Fit {
    fn limit(self) -> Option<f32> {
        match self {
            Fit::Definite(w) | Fit::Available(w) => Some(w),
            Fit::Max => None,
            Fit::Min => Some(0.0),
        }
    }
}

impl BoxTree {
    /// Lay out one inline formatting context, and return its size to taffy.
    ///
    /// The fragments are written onto the inline and text boxes as a side effect, in coordinates
    /// relative to this IFC box's own origin — which is what makes them cheap to read back:
    /// [`crate::layout::geometry`] adds `origin(ifc)` and nothing else, because an inline box's
    /// `parent` IS its IFC box and taffy leaves its own `final_layout` at zero.
    ///
    /// Only a `PerformLayout` run writes them. taffy calls a node several times to *measure* it
    /// before laying it out, and a measuring call's positions are provisional — the same contract
    /// `compute_leaf_layout` has, and the reason `getBoundingClientRect` is never read off a
    /// speculative pass.
    ///
    /// `floats`: the block formatting context this IFC is in, when it holds floats -- each line is then laid in the
    /// space they leave at its height.
    pub(crate) fn layout_ifc(&mut self, node: taffy::NodeId, inputs: LayoutInput, floats: Option<&BlockContext<'_>>) -> LayoutOutput {
        let index = usize::from(node) as u32;
        let fit = match inputs.known_dimensions.width {
            Some(w) => Fit::Definite(w),
            None => match inputs.available_space.width {
                AvailableSpace::Definite(w) => Fit::Available(w),
                AvailableSpace::MaxContent => Fit::Max,
                AvailableSpace::MinContent => Fit::Min,
            },
        };
        // An atomic inline's percentages: its margins of the containing block's inline size -- the line's
        // when it is laid out at a decided width, nothing while only measured -- and a percentage height of
        // the containing block's height, which taffy hands this anonymous box as its parent's.
        let basis = AtomicBasis {
            margin: match fit {
                Fit::Definite(w) => w,
                _ => 0.0,
            },
            height: inputs.parent_size.height,
        };
        let (mut size, mut fragments, mut last_baseline, mut line_starts, lines) = self.flow_lines(index, fit, inputs.run_mode, basis, floats, None);
        // `text-wrap-style: balance`: a paragraph of two to six lines (Chromium balances no more) breaks at the
        // narrowest width that keeps its number of lines -- and is still aligned in the whole width. GitHub's
        // heading broke "The future of building happens / together" where Chrome sets "The future of building /
        // happens together".
        if let (true, Some(w), None) = (self.font_spec_of(index).balance && (2..=6).contains(&lines), fit.limit().filter(|w| *w > 0.0), floats) {
            let (mut lo, mut hi) = (0.0f32, w);
            for _ in 0..14 {
                let mid = (lo + hi) / 2.0;
                if self.flow_lines(index, fit, RunMode::ComputeSize, basis, None, Some(mid)).4 <= lines {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            (size, fragments, last_baseline, line_starts, _) = self.flow_lines(index, fit, inputs.run_mode, basis, floats, Some(hi));
        }
        if inputs.run_mode == RunMode::PerformLayout {
            for item in self.ifc_items(index).iter() {
                if let Item::Text(b) = item {
                    self.maps.text_lines.remove(b);
                }
            }
            for (b, start) in line_starts {
                self.maps.text_lines.entry(b).or_default().push(start);
            }
            self.shift_relative(index, &mut fragments, size.width, basis.height);
            self.commit_fragments(index, fragments, basis.margin);
            self.set_last_baseline(index, last_baseline);
        }
        LayoutOutput::from_sizes(
            size,
            taffy::geometry::Rect { left: 0.0, right: size.width, top: 0.0, bottom: size.height },
        )
    }

    /// Relative positioning (CSS 2.1 §9.4.3) of this context's inline-level boxes: each fragment moved by its box's
    /// offset and by those of the relatively positioned inline boxes it sits in, after the lines are built -- the offset
    /// moves what is drawn, never the line. Percentages are of the block container: its width for `left`/`right`, its
    /// height for `top`/`bottom` when that is definite (`height`, the one taffy hands this box), else nothing. A text
    /// box carries its parent element's style and takes the offset through that parent only.
    ///
    /// reCAPTCHA cuts its image grid with this: each tile is the whole picture at `width: 300%` in an `overflow:
    /// hidden` box, an inline `<img>` at `position: relative; top: -100%; left: -200%`. Unshifted, every tile showed
    /// the picture's top-left corner (Chrome 150 shifts each tile's image by -126px steps in a 126px tile).
    fn shift_relative(&self, ifc: u32, fragments: &mut [(u32, Rect)], width: f32, height: Option<f32>) {
        let mut memo: std::collections::HashMap<u32, (f32, f32)> = std::collections::HashMap::new();
        for (b, r) in fragments.iter_mut() {
            let (dx, dy) = self.relative_shift(*b, ifc, width, height, &mut memo);
            r.x += dx;
            r.y += dy;
        }
    }

    fn relative_shift(&self, b: u32, ifc: u32, width: f32, height: Option<f32>, memo: &mut std::collections::HashMap<u32, (f32, f32)>) -> (f32, f32) {
        if b == ifc {
            return (0.0, 0.0);
        }
        if let Some(s) = memo.get(&b) {
            return *s;
        }
        let base = match self.parent_of(b) {
            Some(p) if p != ifc => self.relative_shift(p, ifc, width, height, memo),
            _ => (0.0, 0.0),
        };
        let own = if self.text_of(b).is_none() && self.css_position(b) == crate::layout::style::CssPosition::Relative {
            use crate::layout::resolved::resolve_inset_unit;
            let inset = self.style_of_box(b).inset;
            // the first side set wins, the other its negation; a percentage of an indefinite height is `auto`
            let pick = |a: Option<f32>, b: Option<f32>| match (a, b) {
                (Some(a), _) => a,
                (None, Some(b)) => -b,
                (None, None) => 0.0,
            };
            let hb = height.unwrap_or(f32::NAN);
            let v = |x: Option<f32>| x.filter(|v| v.is_finite());
            (
                pick(resolve_inset_unit(inset.left, width), resolve_inset_unit(inset.right, width)),
                pick(v(resolve_inset_unit(inset.top, hb)), v(resolve_inset_unit(inset.bottom, hb))),
            )
        } else {
            (0.0, 0.0)
        };
        let s = (base.0 + own.0, base.1 + own.1);
        memo.insert(b, s);
        s
    }

    /// The line-breaking walk. Returns the IFC's size and every fragment it produced, in IFC-local
    /// coordinates, and how many lines it made. `cap`: a width the lines break at, narrower than the one they are
    /// aligned in (a balanced paragraph's).
    #[allow(clippy::type_complexity)]
    fn flow_lines(
        &mut self,
        index: u32,
        fit: Fit,
        run_mode: RunMode,
        basis: AtomicBasis,
        floats: Option<&BlockContext<'_>>,
        cap: Option<f32>,
    ) -> (Size<f32>, Vec<(u32, Rect)>, Option<f32>, Vec<(u32, usize)>, usize) {
        let cap = cap.unwrap_or(f32::INFINITY);
        let items = self.ifc_items(index);
        let mut last_baseline: Option<f32> = None;
        let align = self.ifc_align(index);
        let indent = self.ifc_indent(index);
        let strut = self.vertical_of(index);
        // The space a line at `y` has: all of it, or what the floats beside it leave (CSS 2.1 §9.5: "the current and
        // subsequent line boxes created next to the float are shortened as necessary to make room").
        let space_at = |y: f32| -> (f32, Option<f32>) {
            match (floats, fit.limit()) {
                (Some(f), Some(w)) if f.has_active_floats(y) => {
                    let slot = f.find_content_slot(y, Clear::None, None);
                    let left = slot.x.clamp(0.0, w);
                    (left, Some((slot.x + slot.width).min(w).max(left) - left))
                }
                (_, limit) => (0.0, limit),
            }
        };
        let (mut left, mut limit) = space_at(0.0);
        let quirk = self.line_height_quirk();

        let mut pending: Vec<Pending> = Vec::new();
        let mut open: Vec<Open> = Vec::new();
        let mut fragments: Vec<(u32, Rect)> = Vec::new();
        let mut x = indent;
        let mut y = 0.0f32;
        let mut widest = 0.0f32;
        // How much of `x` is collapsible space since the last real content — the part that hangs
        // past a line's end rather than being in it.
        let mut trailing = 0.0f32;
        let mut line_has_content = false;
        // Where the line's last content ends: an inline box opened at or after it holds nothing on the line yet.
        let mut content_x = f32::NEG_INFINITY;
        // `(text box, index of its first atom on a line)`, once per line the box is on -- in the order
        // its fragments are made, which is line order (the caret position).
        let mut line_starts: Vec<(u32, usize)> = Vec::new();
        // Where each line box ends, for a clamping legacy box.
        let mut bottoms: Vec<f32> = Vec::new();
        // Whether a line ended because the next piece did not fit (a SOFT wrap): the content's
        // max-content width is then wider than the space on offer (`Fit::Available`).
        let mut soft_wrapped = false;

        for item in &items {
            match *item {
                Item::Open(b) => {
                    let v = self.vertical_of(b);
                    let (dy, anchor) = self.aligned(b, v, open.last(), index, strut);
                    let start_x = x;
                    let edge = self.inline_edge(b, true);
                    x += edge;
                    if edge > 0.0 {
                        trailing = 0.0;
                    }
                    open.push(Open { b, start_x, v, dy, anchor });
                }
                Item::Close(b) => {
                    let edge = self.inline_edge(b, false);
                    x += edge;
                    if edge > 0.0 {
                        trailing = 0.0;
                    }
                    // The walk is balanced by construction, so the top of the stack is `b`.
                    if let Some(o) = open.pop() {
                        pending.push(Pending { b: o.b, x: o.start_x, width: x - o.start_x, v: o.v, text: false, inline_box: true, dy: o.dy, anchor: o.anchor });
                    }
                }
                Item::Break(b) => {
                    let v = self.vertical_of(b);
                    let (dy, anchor) = open.last().map_or((0.0, Anchor::None), |o| (o.dy, o.anchor));
                    pending.push(Pending { b, x, width: 0.0, v, text: true, inline_box: false, dy, anchor });
                    line_has_content = true;
                    Self::close_line(
                        &mut pending, &mut open, &mut fragments, &mut x, &mut y, &mut widest,
                        &mut trailing, &mut line_has_content, strut, align, limit, left, quirk, &mut last_baseline, &mut bottoms,
                    );
                    (left, limit) = space_at(y);
                }
                Item::Atomic(b) => {
                    let size = self.measure_atomic(b, run_mode, limit, basis);
                    let (ml, mt, mb, mr) = self.atomic_margins(b, basis.margin);
                    // The break BEFORE an atomic inline is a soft wrap opportunity only where the
                    // content around it may wrap: `white-space: nowrap` keeps five 100px inline-blocks
                    // on one 500px line in a 300px box (Chrome 150, `tools/realweb/seed/sw`),
                    // where this wrapped them onto two lines.
                    let wraps = self.parent_of(b).is_none_or(|p| self.font_spec_of(p).wrap);
                    if let Some(w) = limit.map(|w| w.min(cap)) {
                        if wraps && line_has_content && x + ml + size.width > w {
                            soft_wrapped = true;
                            let carried = self.take_empty_opens(&mut open, &mut x, content_x);
                            Self::close_line(
                                &mut pending, &mut open, &mut fragments, &mut x, &mut y,
                                &mut widest, &mut trailing, &mut line_has_content, strut, align,
                                limit, left, quirk, &mut last_baseline, &mut bottoms,
                            );
                            (left, limit) = space_at(y);
                            self.reopen(&mut open, &mut x, carried);
                        }
                    }
                    x += ml;
                    // An atomic inline's baseline is its BOTTOM MARGIN EDGE. Measured three ways on
                    // Chrome 150 — `<img width=20 height=10>`, an empty 20×30 `inline-block`, and a
                    // 30×20 one with 4px padding and a 1px border — each of whose bottom edge landed
                    // exactly on the surrounding text's baseline. It is also the spec's rule for an
                    // `overflow` that is not `visible`, which is every `<input>` (the UA sheet
                    // computes `overflow: clip` on them). See this unit's report for the case it
                    // approximates: an `inline-block` holding text, whose baseline should be that of
                    // its own last line.
                    let outer = size.height + mt + mb;
                    // A themed checkbox or radio button sits on its border box's bottom (Blink's
                    // `InlineBlockBaselineEdge::kBorderBox`): its bottom margin hangs below the line's
                    // baseline. Chrome 150: a 13px checkbox in 14px text leaves the line 22px tall.
                    let base = if self.is_theme_part_box(b) {
                        Some(size.height)
                    } else {
                        self.text_control_baseline(b, size.height)
                            .or_else(|| self.atomic_baseline(b))
                            .or_else(|| self.synthesized_baseline(b, size.height))
                    };
                    let v = match base {
                        Some(base) => Vertical { above: mt + base, below: outer - mt - base, ascent: base, height: size.height },
                        // the bottom MARGIN edge on the baseline: the border box starts its top margin below the
                        // margin box's top (Chrome 150: Naver's 4px tab dots, `margin-top: 9px; vertical-align: top`,
                        // sit 9px down their 23px line; they sat at its top)
                        None => Vertical { above: outer, below: 0.0, ascent: outer - mt, height: size.height },
                    };
                    let (dy, anchor) = self.aligned(b, v, open.last(), index, strut);
                    pending.push(Pending { b, x, width: size.width, v, text: false, inline_box: false, dy, anchor });
                    x += size.width + mr;
                    trailing = 0.0;
                    line_has_content = true;
                    content_x = x;
                }
                Item::Text(b) => {
                    let v = self.vertical_of(b);
                    let (dy, anchor) = open.last().map_or((0.0, Anchor::None), |o| (o.dy, o.anchor));
                    // Where this text item's part on the current line began: its width is snapped to
                    // a layout unit when the part ends (see `snap_text_part`).
                    let mut part_start = x;
                    let atoms = self.text_atoms(b);
                    for (atom_index, atom) in atoms.iter().copied().enumerate() {
                        if atom.forces_break {
                            Self::snap_text_part(&mut pending, b, part_start, &mut x, &mut trailing);
                            if !pending.iter().any(|p| p.b == b) {
                                line_starts.push((b, atom_index));
                            }
                            pending.push(Pending { b, x, width: 0.0, v, text: true, inline_box: false, dy, anchor });
                            line_has_content = true;
                            Self::close_line(
                                &mut pending, &mut open, &mut fragments, &mut x, &mut y,
                                &mut widest, &mut trailing, &mut line_has_content, strut, align,
                                limit, left, quirk, &mut last_baseline, &mut bottoms,
                            );
                            (left, limit) = space_at(y);
                            part_start = x;
                            continue;
                        }
                        // A collapsible space at a line's start disappears. Chrome reports a
                        // whitespace-only `<span>` as 4.45px wide mid-line and **0** at a line
                        // start, and a wrapped line is never indented by the space it broke at.
                        if atom.space && !line_has_content {
                            continue;
                        }
                        if let Some(w) = limit.map(|w| w.min(cap)) {
                            // A trailing space HANGS past the line's end rather than wrapping it,
                            // which is why a line that just fits is not broken by the space after
                            // its last word.
                            if !atom.space && atom.wrappable && line_has_content && x + atom.width > w
                            {
                                soft_wrapped = true;
                                Self::snap_text_part(&mut pending, b, part_start, &mut x, &mut trailing);
                                let carried = self.take_empty_opens(&mut open, &mut x, content_x);
                                Self::close_line(
                                    &mut pending, &mut open, &mut fragments, &mut x, &mut y,
                                    &mut widest, &mut trailing, &mut line_has_content, strut, align,
                                limit, left, quirk, &mut last_baseline, &mut bottoms,
                                );
                                (left, limit) = space_at(y);
                                self.reopen(&mut open, &mut x, carried);
                                part_start = x;
                            }
                        }
                        let start = x;
                        x += atom.width;
                        if atom.space {
                            trailing += atom.width;
                        } else {
                            trailing = 0.0;
                            line_has_content = true;
                            content_x = x;
                        }
                        if !pending.iter().any(|p| p.b == b) {
                            line_starts.push((b, atom_index));
                        }
                        pending.push(Pending { b, x: start, width: atom.width, v, text: true, inline_box: false, dy, anchor });
                    }
                    Self::snap_text_part(&mut pending, b, part_start, &mut x, &mut trailing);
                }
            }
        }
        Self::close_line(
            &mut pending, &mut open, &mut fragments, &mut x, &mut y, &mut widest,
            &mut trailing, &mut line_has_content, strut, align, limit, left, quirk, &mut last_baseline, &mut bottoms,
        );

        let width = match fit {
            Fit::Definite(w) => w,
            // fit-content is `min(max-content, max(min-content, available))`. Unwrapped, the widest
            // line IS the max-content width; once a line wrapped, the content did not fit and the box
            // takes the space on offer -- or its widest unbreakable piece where that overflows it.
            // Chrome 150: an absolutely positioned box of wrapped text in a
            // 100px containing block is 100 wide, where this answered its widest line's 97.875.
            Fit::Available(w) if soft_wrapped => widest.max(w),
            Fit::Available(_) | Fit::Max | Fit::Min => widest,
        };
        // A `-webkit-box` that clamps lines ends after its N-th line box: Chrome 150 lays
        // two 20 px lines out 40 px tall whatever follows (`tools/realweb/seed/webkitbox`). The lines
        // after it are still laid out -- `innerText` keeps their text, as Chrome's does.
        let clamp = self.line_clamp_of(index) as usize;
        let height = if clamp > 0 && bottoms.len() > clamp { bottoms[clamp - 1] } else { y };
        let lines = bottoms.len();
        (Size { width, height }, fragments, last_baseline, line_starts, lines)
    }

    /// The inline boxes opened since the line's last content (`content_x`), taken off the stack with their start
    /// edges taken back out of `x`, for a soft wrap: the break opportunity is before them -- the space in their
    /// parent's text -- so they begin on the next line, edge and all, and leave nothing on this one. Chrome 150:
    /// `such as <code style="padding:1px 4px;border:1px solid">https://...</code>` wrapping before the code is one
    /// box on the next line; here the code's 5px start edge stayed at this line's end as a box of its own
    /// (Wikipedia's inline code drew a sliver after "such as"), and a link's start a fragment -6px wide.
    fn take_empty_opens(&self, open: &mut Vec<Open>, x: &mut f32, content_x: f32) -> Vec<Open> {
        let mut carried = Vec::new();
        while open.last().is_some_and(|o| o.start_x >= content_x) {
            let o = open.pop().unwrap();
            *x -= self.inline_edge(o.b, true);
            carried.push(o);
        }
        carried.reverse();
        carried
    }

    /// Put the boxes [`BoxTree::take_empty_opens`] took back on the stack at the new line's start, their edges with them.
    fn reopen(&self, open: &mut Vec<Open>, x: &mut f32, carried: Vec<Open>) {
        for o in carried {
            open.push(Open { start_x: *x, ..o });
            *x += self.inline_edge(o.b, true);
        }
    }

    /// Round the part of text item `b` that sits on one line, which began at `start`, UP to a whole
    /// layout unit (1/64 px), moving everything after it.
    ///
    /// Blink sizes each inline item result in `LayoutUnit`s, taking the ceiling of the shaped width.
    /// Measured on Chrome 150.0.7871.46 over 16px Arial: `aaaa ` is 40.0390625px of
    /// advances and the span after it starts at **40.046875** (reported `-9.95` from a scroll of 50,
    /// where the unrounded sum reads `-9.96`); `cccc dddd`, 72.0390625px, is a **72.046875**-wide
    /// fragment. A space advance at 16px is 4.4453125 = 284.5/64, so any run with one in it lands
    /// between two layout units, and the rounding shows in the second decimal.
    fn snap_text_part(pending: &mut [Pending], b: u32, start: f32, x: &mut f32, trailing: &mut f32) {
        let width = *x - start;
        if width <= 0.0 {
            return;
        }
        // `LayoutUnit::FromFloatCeil`, exactly: a run's advances are whole 16.16 values summed as integers
        // (`FontMetrics::width_px_kerned`), so there is no float noise to forgive -- and forgiving it rounded
        // a run 1/65536 px past a unit DOWN (one 16px Noto Sans Mono CJK glyph, 8.0000153: Chrome 8.015625)
        let snapped = (width * 64.0).ceil() / 64.0;
        let delta = snapped - width;
        if delta <= 0.0 {
            return;
        }
        if let Some(p) = pending.iter_mut().rev().find(|p| p.b == b && p.text) {
            p.width += delta;
            // The part's last atom was a collapsible space: the rounding hangs with it.
            if *trailing > 0.0 {
                *trailing += delta;
            }
        }
        *x += delta;
    }

    /// End the current line: fix every pending fragment's `y`, and start the next one.
    ///
    /// An inline box open across the break contributes a fragment for the part of it on THIS line
    /// and starts a fresh one at the next line's left edge — which is what makes a wrapped `<span>`
    /// two rectangles rather than one, and is the whole reason `getClientRects` returns a list.
    #[allow(clippy::too_many_arguments)]
    fn close_line(
        pending: &mut Vec<Pending>,
        open: &mut [Open],
        fragments: &mut Vec<(u32, Rect)>,
        x: &mut f32,
        y: &mut f32,
        widest: &mut f32,
        trailing: &mut f32,
        line_has_content: &mut bool,
        strut: Vertical,
        align: TextAlignKind,
        limit: Option<f32>,
        left: f32,
        quirk: bool,
        last_baseline: &mut Option<f32>,
        bottoms: &mut Vec<f32>,
    ) {
        // A trailing collapsible space HANGS past the line's end: it is not in the line's width and
        // it is not in any fragment. Chrome, on `aaa <span>one … seven</span> zzz` in a 200px
        // block, reports the span's first fragment as 158.31 wide — the width up to `seven`, with
        // the space that followed it left out — while the line itself broke after that space.
        let mut end = *x - *trailing;
        // The hanging space is TRIMMED off its text item's part, and the part without it is a run of its
        // own -- snapped up to a layout unit on its own (`ShapeResult::SnappedWidth` of the trimmed result).
        // The part's snap, made with the space in it, hung with the space (`snap_text_part`): Chrome 150's
        // min-content of `aaaa bbbbbbbb ... eeeeeeeeeee ff` in 16px Arial is the 11 e's snapped, 97.890625,
        // where this answered their exact 97.8828125.
        if *trailing > 0.0 {
            if let Some(last) = pending.iter().rposition(|p| p.text) {
                let b = pending[last].b;
                let start = pending[..=last].iter().rev().take_while(|p| p.b == b && p.text).map(|p| p.x).fold(f32::INFINITY, f32::min);
                let content = end - start;
                if start.is_finite() && content > 0.0 {
                    end = start + (content * 64.0).ceil() / 64.0;
                }
            }
        }
        for o in open.iter_mut() {
            pending.push(Pending { b: o.b, x: o.start_x, width: end - o.start_x, v: o.v, text: false, inline_box: true, dy: o.dy, anchor: o.anchor });
            o.start_x = 0.0;
        }
        *x = end;
        *trailing = 0.0;
        for p in pending.iter_mut() {
            // Only a piece that runs past the end -- by more than float noise -- is cut. `end` is
            // `x - trailing`, and a trailing space added and taken off again leaves it a few ulps
            // short of where the last piece ended: that clipped a 30px `<select>` at x = 572.45 to
            // 29.99994 (the snapped advances put x on fractions f32 cannot hold), where
            // Chrome's positions are LayoutUnits (1/64 px) and it stays 30. Half a LayoutUnit of
            // tolerance cannot hide a real overflow. An atomic inline is never cut: its box is its own, and a
            // negative right margin puts the line's end inside it (Chrome 150: `margin-right: -4px` on a wrapping
            // inline-block in a 300px line leaves it 304 wide; it was cut to 300).
            let atomic = !p.text && !p.inline_box;
            if !atomic && p.x + p.width > end + 1.0 / 128.0 {
                p.width = (end - p.x).max(0.0);
            }
        }
        if pending.is_empty() {
            *x = 0.0;
            return;
        }
        if !*line_has_content {
            // No line box exists, so nothing has a height and nothing advances `y`. Chrome:
            // `<div><span></span></div>` is a zero-height block whose span reports a 0×0 rect.
            for p in pending.drain(..) {
                Self::merge(fragments, p.b, Rect { x: p.x, y: *y, width: p.width, height: 0.0 });
            }
            *x = 0.0;
            return;
        }
        // THE LINE HEIGHT CALCULATION QUIRK (Quirks Mode Standard §3.5): in both quirks
        // modes a line with no text takes no strut, and its empty inline boxes add nothing either.
        // Chrome 150: `<div><img style="width:10px;height:10px"></div>` is 10px tall with no doctype
        // and with a limited-quirks one, 18px with `<!doctype html>`.
        let textless = quirk && !pending.iter().any(|p| p.text);
        let floor = if textless { (0.0, 0.0) } else { (strut.above, strut.below) };
        // The baseline-aligned content first, each piece moved by its `vertical-align` (CSS 2
        // §10.8.1): it reaches `above - dy` up from the baseline and `below + dy` down.
        let (mut above, mut below) = pending
            .iter()
            .filter(|p| !(textless && p.inline_box) && p.anchor == Anchor::None)
            .fold(floor, |acc, p| (acc.0.max(p.v.above - p.dy), acc.1.max(p.v.below + p.dy)));
        // Then each `top`/`bottom` box with what it contains: aligned to the line box's edge, and
        // growing the line at the other edge when it is taller than the rest.
        let mut groups: Vec<(Anchor, f32, f32)> = Vec::new();
        for p in pending.iter().filter(|p| !(textless && p.inline_box) && p.anchor != Anchor::None) {
            let (a, b) = (p.v.above - p.dy, p.v.below + p.dy);
            match groups.iter_mut().find(|g| g.0 == p.anchor) {
                Some(g) => {
                    g.1 = g.1.max(a);
                    g.2 = g.2.max(b);
                }
                None => groups.push((p.anchor, a, b)),
            }
        }
        for &(anchor, a, b) in &groups {
            let extra = (a + b) - (above + below);
            if extra > 0.0 {
                match anchor {
                    Anchor::Bottom(_) => above += extra,
                    _ => below += extra,
                }
            }
        }
        let baseline = *y + above;
        let line_bottom = baseline + below;
        let base_of = |anchor: Anchor| match anchor {
            Anchor::None => baseline,
            Anchor::Top(_) => *y + groups.iter().find(|g| g.0 == anchor).map_or(0.0, |g| g.1),
            Anchor::Bottom(_) => line_bottom - groups.iter().find(|g| g.0 == anchor).map_or(0.0, |g| g.2),
        };
        *last_baseline = Some(baseline);
        // `text-align` moves the whole line inside the space it was given. With no definite limit
        // there is no leftover space and nothing to move.
        let shift = match limit {
            Some(w) if align != TextAlignKind::Start => (w - *x).max(0.0) * align.leading_fraction(),
            _ => 0.0,
        };
        for p in pending.drain(..) {
            let rect = Rect {
                x: left + p.x + shift,
                y: base_of(p.anchor) + p.dy - p.v.ascent,
                width: p.width,
                height: p.v.height,
            };
            Self::merge(fragments, p.b, rect);
        }
        *widest = widest.max(*x);
        *y += above + below;
        bottoms.push(*y);
        *x = 0.0;
        *line_has_content = false;
    }

    /// Add `rect` to `b`'s fragments, unioning it with the one it shares a line with.
    ///
    /// **One rect per line box**, which is what makes `getClientRects()` on a two-line `<span>`
    /// return two. A text run contributes a piece per word to the same line, and they are one
    /// fragment; the same box on a *different* line is a second. Two fragments share a line when
    /// their tops match, which after `close_line` is exact rather than approximate — every fragment
    /// on a line is placed from the one baseline.
    fn merge(fragments: &mut Vec<(u32, Rect)>, b: u32, rect: Rect) {
        if let Some((_, existing)) = fragments
            .iter_mut()
            .rev()
            .find(|(other, r)| *other == b && r.y == rect.y && r.height == rect.height)
        {
            *existing = existing.union(rect);
            return;
        }
        fragments.push((b, rect));
    }

    /// Where `vertical-align` puts box `b` (whose own contribution is `v`) inside `parent` -- the
    /// innermost open inline box, or the block (`ifc`, whose strut is `strut`): its `dy` from the
    /// baseline it would otherwise sit on, its parent's own shift included, and the `top`/`bottom` box
    /// it is placed with. Blink's `InlineLayoutStateStack::ApplyBaselineShift`, in LayoutUnits:
    /// `sub` is the parent's font size / 5 + 1 down, `super` / 3 + 1 up, a length or a percentage of the
    /// box's own line-height up, `middle` the box's midpoint at half the parent's x-height above the
    /// baseline, `text-top`/`text-bottom` its edge on the parent's content area. Chrome 150 (`fixtures/
    /// form-controls/vertical-align.js`): `<progress>`'s UA `-0.2em` puts it 1.796875px below a 14px
    /// line's top, where it sat on the baseline.
    fn aligned(&self, b: u32, v: Vertical, parent: Option<&Open>, ifc: u32, strut: Vertical) -> (f32, Anchor) {
        use crate::layout::style::{layout_unit, VAlign};
        let (pdy, panchor) = parent.map_or((0.0, Anchor::None), |o| (o.dy, o.anchor));
        let parent_index = parent.map_or(ifc, |o| o.b);
        let pv = parent.map_or(strut, |o| o.v);
        let lu = |px: f32| (layout_unit(px) * 64.0).round() as i64;
        let px = |raw: i64| raw as f32 / 64.0;
        let parent_size = || crate::font::used_size(self.font_spec_of(parent_index).size);
        let own = match self.valign_of(b) {
            VAlign::Baseline => 0.0,
            VAlign::Sub => px(lu(parent_size()) / 5 + 64),
            VAlign::Super => -px(lu(parent_size()) / 3 + 64),
            VAlign::Length(l) => -layout_unit(l),
            VAlign::Percent(f) => {
                let spec = self.font_spec_of(b);
                let metrics = self.fonts_metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
                -layout_unit(f * spec.line_height(metrics))
            }
            VAlign::Middle => {
                let spec = self.font_spec_of(parent_index);
                let metrics = self.fonts_metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
                let half_x = (metrics.x_height * crate::font::used_size(spec.size) / 2.0 * 64.0).round() / 64.0;
                px((lu(v.above) - lu(v.below)) / 2) - half_x
            }
            VAlign::TextTop => v.above - pv.ascent,
            VAlign::TextBottom => (pv.height - pv.ascent) - v.below,
            VAlign::Top => return (0.0, Anchor::Top(b)),
            VAlign::Bottom => return (0.0, Anchor::Bottom(b)),
        };
        (pdy + own, panchor)
    }

    /// The vertical contribution of the box at `index`, by rule 3 in the module note.
    #[allow(clippy::wrong_self_convention)]
    pub(super) fn vertical_of(&self, index: u32) -> Vertical {
        let spec = self.font_spec_of(index);
        let metrics = self.fonts_metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
        let size = crate::font::used_size(spec.size);
        let ascent = (metrics.ascent * size).round();
        let descent = (metrics.descent * size).round();
        let leading = spec.line_height(metrics) - (ascent + descent);
        let half = (leading / 2.0).floor();
        let mut v = Vertical {
            above: ascent + half,
            below: descent + (leading - half),
            ascent,
            height: ascent + descent,
        };
        // `line-height: normal` takes every font the text is set in into account, each with its own
        // line spacing (Blink's `AccumulateUsedFonts`): a Korean word in a Latin face is set in the
        // fallback CJK face, and its taller ascent opens the line -- Chrome 150: `확인 중...` in 14px
        // `system-ui` makes a 20px line around a 17px text box, 3px down. The text box itself keeps the
        // primary font's metrics.
        if spec.line_height.is_none() {
            if let Some(text) = self.text_of(index) {
                for fb in self.fallback_faces(&spec, metrics, text) {
                    let a = (fb.ascent * size).round();
                    let d = (fb.descent * size).round();
                    let l = (fb.line_gap * size).round();
                    let h = (l / 2.0).floor();
                    v.above = v.above.max(a + h);
                    v.below = v.below.max(d + (l - h));
                }
            }
        }
        v
    }

    /// The height of a line of `text` under `line-height: normal`: the primary font's line spacing, opened by each
    /// fallback face the text is set in, as a line box's is (a control's internal line: a file input's Korean label is
    /// set in the CJK fallback, and its button is that much taller).
    pub(crate) fn normal_line_height(&self, spec: &crate::layout::inline::FontSpec, metrics: &'static crate::font::FontMetrics, text: &str) -> f32 {
        let size = crate::font::used_size(spec.size);
        let split = |m: &crate::font::FontMetrics| {
            let (a, d, l) = ((m.ascent * size).round(), (m.descent * size).round(), (m.line_gap * size).round());
            let h = (l / 2.0).floor();
            (a + h, d + (l - h))
        };
        let (mut above, mut below) = split(metrics);
        for fb in self.fallback_faces(spec, metrics, text) {
            let (a, b) = split(fb);
            above = above.max(a);
            below = below.max(b);
        }
        above + below
    }

    /// The faces `text` falls back to from `metrics` (its first available face): a script the face has
    /// no glyphs for is set in the fallback the platform picks -- the persona's `sans-serif` face for
    /// CJK, its `emoji` face for emoji. Read off the capture: a face without the glyphs was measured at
    /// the fallback's advance for the whole script (`FontMetrics::class_advances`).
    fn fallback_faces(&self, spec: &crate::layout::inline::FontSpec, metrics: &'static crate::font::FontMetrics, text: &str) -> Vec<&'static crate::font::FontMetrics> {
        use crate::font::ScriptClass;
        let mut out: Vec<&'static crate::font::FontMetrics> = Vec::new();
        for ch in text.chars() {
            let Some(class) = ScriptClass::of(ch) else { continue };
            let generic = if class == ScriptClass::Emoji { "emoji" } else { "sans-serif" };
            let fb = self.fonts_metrics_styled(generic, spec.bold, spec.italic);
            if std::ptr::eq(fb, metrics) || out.iter().any(|o| std::ptr::eq(*o, fb)) {
                continue;
            }
            let own = metrics.class_advances[class as usize];
            if own == 0.0 || (own - fb.class_advances[class as usize]).abs() < 1e-6 {
                out.push(fb);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::style::media::MediaContext;
    use crate::{LaidOut, Page};
    use korvus_dom::{Dom, NodeId};

    /// Lay `html` out at Chrome's own viewport, so an expectation can be Chrome's own number.
    ///
    /// **1920×993** is what `/usr/bin/google-chrome` 150.0.7871.46 reported for `innerWidth` and
    /// `innerHeight` under `--headless=new --window-size=1920,1080` on 2026-09-14, and
    /// `documentElement.clientWidth` was 1920 on every probe page — no scrollbar was reserved, so
    /// there is none to subtract. Every number asserted below was read back from that browser over
    /// CDP on that day. The pages all set `font: 16px Arial`, because the persona's default family
    /// is Times New Roman and Chrome's on this host is Arial: naming the family is what makes the
    /// two engines measure the same text rather than two different fonts.
    fn chrome_viewport(html: &str) -> (Dom, LaidOut) {
        let page = Page::default();
        let dom = Dom::parse(html);
        let media = MediaContext {
            viewport_width: 1920.0,
            viewport_height: 993.0,
            ..MediaContext::default()
        };
        let out = page.layout(&dom, &media);
        (dom, out)
    }

    fn el(dom: &Dom, id: &str) -> NodeId {
        (0..dom.len() as NodeId)
            .find(|&n| dom.get_attribute(n, "id") == Some(id))
            .unwrap_or_else(|| panic!("no #{id}"))
    }

    /// `text-wrap: balance` breaks a paragraph of a few lines at the narrowest width that keeps their number, and still
    /// aligns them in the whole width: three words then one become two and two (GitHub's heading, Chrome 152: "The
    /// future of building / happens together", 554 and 476 px, where wrapping alone made 799 and 228).
    #[test]
    fn a_balanced_paragraph_evens_its_lines() {
        let lines = |style: &str| {
            let html = format!("<body style='margin:0;font:20px Arial'><p id=p style='margin:0;width:300px;{style}'>aaaaaaaa bbbbbbbb cccccccc dddddddd</p></body>");
            let (dom, out) = chrome_viewport(&html);
            let p = out.tree.box_of(el(&dom, "p")).unwrap();
            let text = (p..out.tree.len() as u32).find(|&b| !out.tree.text_fragments(b, &out.scroll).is_empty()).unwrap();
            out.tree.text_fragments(text, &out.scroll).into_iter().map(|(s, r)| (s.trim().to_string(), r.x)).collect::<Vec<_>>()
        };
        let plain = lines("");
        assert_eq!(plain.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["aaaaaaaa bbbbbbbb cccccccc", "dddddddd"]);
        let balanced = lines("text-wrap:balance");
        assert_eq!(balanced.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["aaaaaaaa bbbbbbbb", "cccccccc dddddddd"]);
        // centred in the whole 300px, not in the narrower width it broke at
        let centred = lines("text-wrap:balance;text-align:center");
        assert!(centred[0].1 > 30.0, "{centred:?}");
    }

    /// An inline box whose first word wraps begins on the next line, start edge and all: the break opportunity is the
    /// space before it, in its parent's text. Chrome 150 lays `such as <code style="padding:1px 4px;border:1px solid">`
    /// wrapping before the code as one box on the next line; its 5px start edge stayed at this line's end as a box of
    /// its own (Wikipedia's inline code drew a sliver after "such as").
    #[test]
    fn an_inline_box_that_wraps_before_its_first_word_starts_on_the_next_line() {
        let (dom, out) = chrome_viewport(
            "<body style='margin:0;font:20px Arial'><p style='margin:0;width:200px'>aaaa bbbb cccc <code id=c \
             style='padding:1px 4px;border:1px solid'>dddddddd</code> e</p></body>",
        );
        let rects = out.client_rects(el(&dom, "c"));
        assert_eq!(rects.len(), 1, "one fragment, on the second line: {rects:?}");
        assert_eq!(rects[0].x, 0.0, "{rects:?}");
        assert!(rects[0].y > 15.0, "{rects:?}");
    }

    /// A shrink-to-fit inline-block whose text wraps fills its line less its margins -- both of them, a negative one
    /// widening it. Chrome 150, a 300px block: `margin: 0 -4px` is 308 wide, `margin-right: -4px` alone 304
    /// (minea's gradient heading, `padding: 3.6px; margin: -3.6px`, was 3.6px narrow and its lines off centre).
    #[test]
    fn a_wrapping_inline_block_fills_its_line_less_both_margins() {
        let text = "Long text that will certainly wrap inside the box here";
        let (dom, out) = chrome_viewport(&format!(
            "<body style='margin:20px;font:20px Arial'>\
             <div style='width:300px'><span id=a style='display:inline-block;margin:0 -4px'>{text}</span></div>\
             <div style='width:300px'><span id=b style='display:inline-block;margin:0 -4px 0 0'>{text}</span></div>\
             <div style='width:300px'><span id=c style='display:inline-block;margin:0 0 0 -4px'>{text}</span></div>"
        ));
        assert_eq!([rect(&out, &dom, "a")[0], rect(&out, &dom, "a")[2]], [16.0, 308.0]);
        assert_eq!([rect(&out, &dom, "b")[0], rect(&out, &dom, "b")[2]], [20.0, 304.0]);
        assert_eq!([rect(&out, &dom, "c")[0], rect(&out, &dom, "c")[2]], [16.0, 304.0]);
    }

    /// A text range's rects are its characters' own, one per line: Chrome 150 on 16px/1.4 Arial, read over CDP.
    #[test]
    fn a_text_range_covers_its_characters_line_by_line() {
        let (dom, out) = chrome_viewport(
            "<body style='margin:20px;font:16px/1.4 Arial'><div id=a>Hello range world</div>\
             <div style='width:200px'>aaa <span id=s>one two three four five six seven</span> zzz tail</div></body>",
        );
        let text = |id: &str| dom.first_child(el(&dom, id)).expect("its text");
        let r = |n, a, b| out.text_range_rects(&dom, n, a, b).into_iter().map(|r| [r.x, r.y, r.width, r.height].map(|v| (v * 100.0).round() / 100.0)).collect::<Vec<_>>();
        assert_eq!(r(text("a"), 0, 17), [[20.0, 22.0, 124.52, 17.0]]);
        assert_eq!(r(text("a"), 2, 9), [[40.45, 22.0, 43.58, 17.0]]);
        // across a wrap: each line's part, the space the line hangs left out
        assert_eq!(r(text("s"), 0, 33), [[51.14, 44.39, 158.31, 17.0], [20.0, 66.78, 66.7, 17.0]]);
        assert_eq!(r(text("s"), 4, 20), [[82.28, 44.39, 106.72, 17.0]]);
    }

    /// A specified line height is whole layout units, the fraction truncated: Chrome 150 stacks `28px/1.4` lines
    /// 39.1875 apart (39.2 * 64 = 2508.8 units).
    #[test]
    fn a_line_height_is_whole_layout_units() {
        let (dom, out) = chrome_viewport("<body style='margin:0;font:28px/1.4 Arial'><div id=a>one</div><div id=b>two</div></body>");
        assert_eq!(rect(&out, &dom, "a")[3], 39.19);
        assert_eq!(out.bounding_rect(el(&dom, "b")).y, 39.1875);
    }

    /// An atomic inline with no baseline of its own is placed by its MARGIN box: its vertical margins move its border
    /// box down from wherever `vertical-align` puts the margin box. Chrome 150, 4x4 inline-blocks on 23px lines.
    #[test]
    fn an_atomic_inline_is_placed_by_its_margin_box() {
        let dot = |id: &str, style: &str| format!("<div>tab<i id={id} style='display:inline-block;width:4px;height:4px;{style}'></i>text</div>");
        let (dom, out) = chrome_viewport(&format!(
            "<body style='margin:20px;font:16.8px/23px Arial'>{}{}{}{}</body>",
            dot("t", "margin-top:9px;vertical-align:top"),
            dot("b", "margin-bottom:5px;vertical-align:bottom"),
            dot("l", "margin-top:6px"),
            dot("x", "margin-top:9px;vertical-align:text-top"),
        ));
        // each line's top, then where the dot's border box starts
        assert_eq!(rect(&out, &dom, "t")[1], 20.0 + 9.0);
        assert_eq!(rect(&out, &dom, "b")[1], 43.0 + 23.0 - 4.0 - 5.0);
        // on the baseline, its bottom border edge on it whatever its top margin
        let plain = {
            let (dom, out) = chrome_viewport("<body style='margin:20px;font:16.8px/23px Arial'><div>tab<i id=l style='display:inline-block;width:4px;height:4px'></i>text</div></body>");
            rect(&out, &dom, "l")[1]
        };
        assert_eq!(rect(&out, &dom, "l")[1] - 66.0, plain - 20.0);
        // the text top: 16.8px Arial's 19px content area centred on the 23px line, 2px down
        assert_eq!(rect(&out, &dom, "x")[1] - 89.0, 2.0 + 9.0);
    }

    /// A relatively positioned inline-level box is drawn shifted by its offsets, its line left where it was: an inline
    /// `<img>` by percentages of its block container (reCAPTCHA's tile, `top: -100%; left: -200%` in a 100px box), and
    /// an inline box with the text it holds. Chrome 150: the image at `-200, -100`, the span's text 5px right and 3px
    /// down of where it sits unshifted, the line below it unmoved.
    #[test]
    fn a_relatively_positioned_inline_is_drawn_at_its_offsets() {
        let (dom, out) = chrome_viewport(
            "<body style='margin:0;font:16px Arial'><div style='width:100px;height:100px;overflow:hidden'><img id=i style='position:relative;top:-100%;left:-200%;width:400px;height:400px'></div><div>a<span id=s style='position:relative;top:3px;left:5px'>b</span></div><div id=n>c</div></body>",
        );
        let i = rect(&out, &dom, "i");
        assert_eq!((i[0], i[1]), (-200.0, -100.0));
        let (plain, below) = {
            let (dom, out) = chrome_viewport("<body style='margin:0;font:16px Arial'><div style='width:100px;height:100px'></div><div>a<span id=s>b</span></div><div id=n>c</div></body>");
            (rect(&out, &dom, "s"), rect(&out, &dom, "n"))
        };
        let s = rect(&out, &dom, "s");
        assert_eq!((s[0] - plain[0], s[1] - plain[1]), (5.0, 3.0));
        assert_eq!(rect(&out, &dom, "n"), below);
    }

    /// `[x, y, width, height]`, rounded to 2dp the way the probe printed Chrome's.
    fn rect(out: &LaidOut, dom: &Dom, id: &str) -> [f32; 4] {
        let r = out.bounding_rect(el(dom, id));
        [r.x, r.y, r.width, r.height].map(|v| (v * 100.0).round() / 100.0)
    }

    fn rects(out: &LaidOut, dom: &Dom, id: &str) -> Vec<[f32; 4]> {
        out.client_rects(el(dom, id))
            .into_iter()
            .map(|r| [r.x, r.y, r.width, r.height].map(|v| (v * 100.0).round() / 100.0))
            .collect()
    }

    /// How far an **inline-axis** number here may sit from the browser's: 0.03px.
    ///
    /// It is a statement about the font table, not about layout, and it is deliberately confined to
    /// `x` and `width`. Those two are sums of glyph advances, and the advances come from a persona
    /// capture stored as em fractions — so `link` measures 24.01 here against Chrome's 24.02, a
    /// disagreement of about 1.3 font units in 2048 on one glyph, and it accumulates along a line:
    /// the largest gap over every case on this page is **0.02px**, at the `x` of the second inline
    /// in the row, five glyphs in. 0.03 is that plus the last bit of the `f32` subtraction itself.
    /// `inline::tests` already pins a text width the same way, for the same reason.
    ///
    /// **`y` and `height` are compared EXACTLY**, with no allowance at all, because they are not
    /// advances: they are the line-box arithmetic this unit exists to get right — integer ascents
    /// and descents, a `floor`ed half-leading, and one baseline per line. A wrong line box, a wrong
    /// baseline or a missing fragment is wrong by whole pixels, and the defect this unit closes put
    /// every inline at x = 0: neither has anywhere to hide in three hundredths of a pixel.
    const ADVANCE_PRECISION: f32 = 0.03;

    #[track_caller]
    fn same(got: [f32; 4], want: [f32; 4]) {
        for i in [0, 2] {
            assert!(
                (got[i] - want[i]).abs() <= ADVANCE_PRECISION,
                "got {got:?}, Chrome says {want:?} (inline-axis component {i})"
            );
        }
        for i in [1, 3] {
            assert_eq!(got[i], want[i], "got {got:?}, Chrome says {want:?} (block-axis {i})");
        }
    }

    #[track_caller]
    fn same_all(got: &[[f32; 4]], want: &[[f32; 4]]) {
        assert_eq!(got.len(), want.len(), "got {got:?}, Chrome says {want:?}");
        for (g, w) in got.iter().zip(want) {
            same(*g, *w);
        }
    }

    const BODY: &str = "<style>body{margin:8px;font:16px Arial}div{font:16px Arial}</style>";

    /// THE defect this unit exists for: a row of inlines lays out ACROSS a line.
    ///
    /// Chrome 150, on `<div><a>link</a> <span>span text</span> <b>bold</b>end</div>`:
    ///
    /// ```text
    /// a     [8,      8, 24.02, 17]
    /// span  [36.47,  8, 64.94, 17]
    /// b     [105.86, 8, 33.77, 17]
    /// ```
    ///
    /// Three different x values on ONE y. Before this unit all three were `[0, 0, 0, 0]`, and the
    /// engine's own comment said why: an inline box generated no box at all rather than a
    /// confidently wrong one. The fix is not to give it a block's box — `<b>` at x = 105.86 is not
    /// a block's x — it is to give it the line box it always needed.
    #[test]
    fn a_row_of_inlines_lays_out_across_a_line_at_chromes_own_x() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='A'><a id='a' href='#'>link</a> <span id='s'>span text</span> \
             <b id='b'>bold</b>end</div></body>"
        ));
        same(rect(&out, &dom, "a"), [8.0, 8.0, 24.02, 17.0]);
        same(rect(&out, &dom, "s"), [36.47, 8.0, 64.94, 17.0]);
        // The third box's X is Chrome's. Its width is the bold face's since V2, which
        // `a_bold_run_is_measured_in_the_bold_face` below checks against Chrome (33.77).
        assert!(
            (rect(&out, &dom, "b")[0] - 105.86).abs() <= ADVANCE_PRECISION,
            "got {:?}",
            rect(&out, &dom, "b")
        );
        // And the line box itself is 18 tall, not 17: the inline's content area is not the line.
        same(rect(&out, &dom, "A"), [8.0, 8.0, 1904.0, 18.0]);
    }

    /// Bold text is measured in the BOLD face. Chrome reports `<b>bold</b>` in 16px Arial
    /// as **33.77** wide and the same word in the regular face as 30.25; this engine measured both at
    /// 30.25 until the persona bundle carried a bold capture and `FontProvider` took the weight. (This
    /// test pinned the defect, and said it would stop failing the day the bold face landed.)
    #[test]
    fn a_bold_run_is_measured_in_the_bold_face() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div><b id='b'>bold</b></div><div><span id='s'>bold</span></div></body>"
        ));
        let (bold, plain) = (rect(&out, &dom, "b")[2], rect(&out, &dom, "s")[2]);
        assert!((plain - 30.25).abs() <= ADVANCE_PRECISION, "regular Arial: {plain}");
        assert!((bold - 33.77).abs() <= ADVANCE_PRECISION, "bold Arial, Chrome 150 says 33.77: {bold}");
    }

    /// The case `getBoundingClientRect` alone cannot express, and the reason D11 returns a list.
    ///
    /// Chrome, on `<div style="width:200px">aaa <span>one two … seven</span> zzz</div>` on the
    /// second line of the page: **two** rects, `[39.14, 26, 158.31, 17]` and `[8, 44, 66.7, 17]`,
    /// whose union is the bounding rect `[8, 26, 189.45, 35]`.
    #[test]
    fn an_inline_wrapping_two_lines_is_two_rects_whose_union_is_the_bounding_rect() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div style='height:18px'></div>\
             <div id='B' style='width:200px'>aaa \
             <span id='s'>one two three four five six seven</span> zzz</div></body>"
        ));
        same_all(
            &rects(&out, &dom, "s"),
            &[[39.14, 26.0, 158.31, 17.0], [8.0, 44.0, 66.7, 17.0]],
        );
        same(rect(&out, &dom, "s"), [8.0, 26.0, 189.45, 35.0]); // "and the union of the two"
    }

    /// Three lines, three rects. Chrome, on a 200px block holding one long inline:
    /// `[8, +0, 171.67, 17]`, `[8, +18, 182.36, 17]`, `[8, +36, 43.59, 17]`.
    #[test]
    fn an_inline_over_three_lines_is_three_rects() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='H' style='width:200px'>\
             <span id='s'>alpha beta gamma delta epsilon zeta eta theta iota kappa</span></div></body>"
        ));
        let got = rects(&out, &dom, "s");
        assert_eq!(got.len(), 3, "{got:?}");
        same(got[0], [8.0, 8.0, 171.67, 17.0]);
        same(got[1], [8.0, 26.0, 182.36, 17.0]);
        same(got[2], [8.0, 44.0, 43.59, 17.0]);
    }

    /// One baseline per line, whatever each box's font size is — rule 2 in the module note.
    ///
    /// Chrome, on `base <span style="font-size:32px">BIG</span> <span style="font-size:8px">sm
    /// </span> base`: tops of 131, 116 and 138 for ascents of 14, 29 and 7 — a baseline of 145
    /// three times — in a line box 37 tall.
    #[test]
    fn boxes_of_different_font_sizes_share_one_baseline() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='E'>base <span id='big' style='font-size:32px'>BIG</span> \
             <span id='sm' style='font-size:8px'>sm</span> base</div></body>"
        ));
        let (big, sm) = (rect(&out, &dom, "big"), rect(&out, &dom, "sm"));
        same(big, [47.14, 8.0, 55.13, 36.0]);
        same(sm, [106.72, 30.0, 10.67, 9.0]);
        assert_eq!(big[1] + 29.0, sm[1] + 7.0, "ascents 29 and 7: one baseline");
        assert_eq!(rect(&out, &dom, "E")[3], 37.0, "the line grew for the 32px box");
    }

    /// An explicit `line-height` moves the box inside the line and does NOT change its height.
    ///
    /// Chrome: `<div style="line-height:40px">n <span>mid</span> n</div>` is a 40px block whose
    /// span reports height **17** at 11px below the line's top — `floor((40 - 17) / 2)`.
    #[test]
    fn line_height_moves_the_fragment_without_resizing_it() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='N' style='line-height:40px'>n <span id='s'>mid</span> n</div>\
             </body>"
        ));
        assert_eq!(rect(&out, &dom, "N")[3], 40.0);
        let s = rect(&out, &dom, "s");
        assert_eq!(s[3], 17.0, "the content area, not the line");
        assert_eq!(s[1], 8.0 + 11.0, "floor((40 - 17) / 2) below the line's top");
    }

    /// Vertical padding and border on an inline grow its RECT and not the line.
    ///
    /// Chrome, on `x<span style="padding:10px;border:3px solid">pad</span>y`: the block stays 18
    /// tall and the span reports a 52.7 × 43 rect (17 + 20 + 6) starting 13px above the line's top.
    #[test]
    fn an_inlines_padding_grows_its_rect_and_not_the_line() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='F'>x<span id='s' style='padding:10px;border:3px solid red'>pad\
             </span>y</div></body>"
        ));
        assert_eq!(rect(&out, &dom, "F")[3], 18.0, "the line box is untouched");
        same(rect(&out, &dom, "s"), [16.0, -5.0, 52.7, 43.0]); // "13px above the line's top"
    }

    /// An empty inline still has a rect — zero wide, one content area tall — and a whitespace-only
    /// one is as wide as a space MID-LINE and zero at a line's start.
    ///
    /// Chrome: `before<span></span>after` → `[53.38, y, 0, 17]`; `before<span> </span>after` →
    /// `[53.38, y, 4.45, 17]`; `<span>   </span>after` → `[8, y, 0, 17]`.
    #[test]
    fn an_empty_inline_has_a_zero_width_rect_and_a_space_counts_only_mid_line() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div>before<span id='empty'></span>after</div>\
             <div>before<span id='space'> </span>after</div>\
             <div><span id='lead'>   </span>after</div></body>"
        ));
        same(rect(&out, &dom, "empty"), [53.38, 8.0, 0.0, 17.0]);
        assert_eq!(rects(&out, &dom, "empty").len(), 1, "one rect, not none");
        same(rect(&out, &dom, "space"), [53.38, 26.0, 4.45, 17.0]);
        same(rect(&out, &dom, "lead"), [8.0, 44.0, 0.0, 17.0]); // "collapsed at the line start"
    }

    /// An empty inline alone in a block makes NO line box: Chrome reports the block as zero-height
    /// and the span's rect as 0×0. An empty inline does not bring a line into existence.
    #[test]
    fn an_empty_inline_alone_in_a_block_makes_no_line_box() {
        let (dom, out) =
            chrome_viewport(&format!("{BODY}<body><div id='d'><span id='s'></span></div></body>"));
        assert_eq!(rect(&out, &dom, "d")[3], 0.0, "no content, no line");
        same(rect(&out, &dom, "s"), [8.0, 8.0, 0.0, 0.0]);
    }

    /// `<br>` ends a line and carries a rect of its own: Chrome gives it zero width and one content
    /// area of height, at the end of the line it broke, and the block becomes two lines tall.
    #[test]
    fn a_br_ends_the_line_and_has_a_rect() {
        let (dom, out) =
            chrome_viewport(&format!("{BODY}<body><div id='K'>one<br id='br'>two</div></body>"));
        assert_eq!(rect(&out, &dom, "K")[3], 36.0, "two lines");
        same(rect(&out, &dom, "br"), [34.7, 8.0, 0.0, 17.0]);
    }

    /// `text-align` moves the whole line. Chrome, in a 400px block: `center` puts a `mid` span at
    /// x = 195.11 and `right` at x = 372.44.
    #[test]
    fn text_align_moves_the_line_inside_the_block() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div style='text-align:center;width:400px'>c <span id='c'>mid</span> c\
             </div><div style='text-align:right;width:400px'>r <span id='r'>mid</span> r</div>\
             </body>"
        ));
        assert!((rect(&out, &dom, "c")[0] - 195.11).abs() <= ADVANCE_PRECISION);
        assert!((rect(&out, &dom, "r")[0] - 372.44).abs() <= ADVANCE_PRECISION);
    }

    /// `text-indent` moves the first line only. Chrome: x = 46 for a span after one `x` in a block
    /// indented 30px — 8 + 30 + 8.
    #[test]
    fn text_indent_moves_the_first_line() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div style='text-indent:30px'>x<span id='s'>aa</span>y</div></body>"
        ));
        same(rect(&out, &dom, "s"), [46.0, 8.0, 17.8, 17.0]);
    }

    /// An atomic inline sits ON the line, with its bottom margin edge on the baseline.
    ///
    /// Chrome, on `x<span style="display:inline-block;width:20px;height:30px"></span>y` beside 16px
    /// text: the inline-block is 20 × 30 at the line's top, and the line grows to 34 to hold it.
    #[test]
    fn an_inline_block_sits_on_the_line_and_its_bottom_edge_is_the_baseline() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='d'>x<span id='ib' \
             style='display:inline-block;width:20px;height:30px'></span>y</div></body>"
        ));
        assert_eq!(rect(&out, &dom, "d")[3], 34.0, "the line grew for the 30px box");
        same(rect(&out, &dom, "ib"), [16.0, 8.0, 20.0, 30.0]);
    }

    /// A block-level element inside an inline SPLITS it. Chrome, on
    /// `t<span>pre<div>BLOCK</div>post</span>t`: the block is its own full-width line and the span
    /// reports the halves on either side of it.
    #[test]
    fn a_block_inside_an_inline_splits_it_into_fragments_around_the_block() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='T'>t<span id='s'>pre<div id='d'>BLOCK</div>post</span>t</div>\
             </body>"
        ));
        assert_eq!(rect(&out, &dom, "T")[3], 54.0, "three lines: before, the block, after");
        same(rect(&out, &dom, "d"), [8.0, 26.0, 1904.0, 18.0]); // "the block is full width"
        let got = rects(&out, &dom, "s");
        assert_eq!(got.len(), 2, "the two halves the block split the span into: {got:?}");
        same(got[0], [12.45, 8.0, 23.13, 17.0]);
        same(got[1], [8.0, 44.0, 30.25, 17.0]);
    }

    /// Hit testing lands on the FRAGMENT, not on the union.
    ///
    /// Chrome makes the distinction itself: `elementFromPoint` at the centre of a two-line span's
    /// **bounding** rect answered the enclosing block — the point falls in the leading between the
    /// lines — while the centre of each of its two `getClientRects()` entries answered the span.
    #[test]
    fn a_point_at_each_fragments_centre_finds_the_inline() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='B' style='width:200px'>aaa \
             <span id='s'>one two three four five six seven</span> zzz</div></body>"
        ));
        let s = el(&dom, "s");
        for r in out.client_rects(s) {
            let hit = out.element_from_point(r.x + r.width / 2.0, r.y + r.height / 2.0);
            assert_eq!(hit, Some(s), "the centre of {r:?} is on the span");
        }
    }

    /// A `<span>` in a flex container is blockified, which is what Chrome reports too
    /// (`getComputedStyle(span).display` answers `block` there) — so the flex path is untouched.
    #[test]
    fn a_flex_container_blockifies_its_inline_children() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div id='f' style='display:flex'>x<span id='s'>aa</span>y</div></body>"
        ));
        let s = rect(&out, &dom, "s");
        assert!(s[2] > 0.0 && s[3] > 0.0, "a real box: {s:?}");
        assert_eq!(rects(&out, &dom, "s").len(), 1, "a block, so one rect");
    }

    /// A non-breaking space is NOT collapsible whitespace, and Chrome measures two of them at
    /// 8.89px — two space advances — rather than collapsing them to one or to none.
    #[test]
    fn a_non_breaking_space_does_not_collapse() {
        let (dom, out) = chrome_viewport(&format!(
            "{BODY}<body><div>x<span id='s'>\u{a0}\u{a0}</span>y</div></body>"
        ));
        same(rect(&out, &dom, "s"), [16.0, 8.0, 8.89, 17.0]);
    }
}
