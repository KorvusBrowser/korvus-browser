//! Where a box actually is, in the coordinates a page asks in.
//!
//! taffy answers in parent-relative coordinates, because that is what layout computes.
//! `getBoundingClientRect` answers in *viewport* coordinates, because that is what a page can act
//! on. The difference is a walk up the box tree, and this is where it happens — once, in one place,
//! rather than at each of the half-dozen members that need it.
//!
//! # Fragments, not a rect
//!
//! `getClientRects` returns a *list*: an inline element spanning three lines has three rectangles,
//! and `getBoundingClientRect` is their union. v1's block boxes each produce exactly one, so the
//! list has one entry today — but the shape is a list from the start (design D11), because P3's
//! inline flow adds the rest without a caller changing. Computing one rect now and a list later
//! would mean rewriting every consumer.

use crate::layout::style::CssPosition;
use crate::layout::tree::BoxTree;
use korvus_dom::NodeId;
use std::collections::HashMap;
use taffy::geometry::Size;

/// How far everything has been scrolled: the viewport, and each scroll container by its element.
///
/// # Why this is an input and not part of the layout
///
/// Layout places boxes in DOCUMENT coordinates, and scrolling does not move a single one of them: it
/// moves the window the document is seen through. `getBoundingClientRect` answers in CLIENT
/// coordinates -- relative to that window -- so it is the layout less the scroll offsets above the
/// box. Measured on Chrome 150.0.7871.46: `#a` absolutely positioned at `top: 1000px`
/// answers `offsetTop` 1000 and `getBoundingClientRect().top` **100** after `scrollTo(0, 900)`.
/// Korvus answered 1000 for both, because nothing between the layout and the member knew the page
/// had scrolled. Keeping the offsets out of the layout also keeps a scroll from costing one: the
/// laid-out document is cached on the DOM's revision, and a scroll changes no revision.
///
/// The offsets live in the web-API layer (`scrollTop`/`scrollLeft`, `window.scrollX`/`scrollY`); the
/// caller hands the current ones over each time it asks about the layout.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrollSnapshot {
    /// The viewport's scroll -- `(scrollX, scrollY)` of the document this layout is.
    pub viewport: (f32, f32),
    /// `(scrollLeft, scrollTop)` of every element that is scrolled at all. An absent element is at
    /// `(0, 0)`.
    pub elements: HashMap<NodeId, (f32, f32)>,
}

impl ScrollSnapshot {
    fn of(&self, node: NodeId) -> (f32, f32) {
        self.elements.get(&node).copied().unwrap_or((0.0, 0.0))
    }
}

/// A rectangle in CSS pixels, in viewport coordinates. The same four numbers `DOMRect` carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// The smallest rectangle containing both. `getBoundingClientRect` is this over every fragment.
    pub fn union(self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            width: self.right().max(other.right()) - x,
            height: self.bottom().max(other.bottom()) - y,
        }
    }

    /// Is this point inside? Half-open on the right and bottom, as hit testing wants: two boxes
    /// that share an edge must not both claim it.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

impl BoxTree {
    /// A box's offset from the viewport origin, by walking up its ancestors.
    fn origin(&self, index: u32) -> (f32, f32) {
        let (mut x, mut y) = (0.0, 0.0);
        let mut current = Some(index);
        while let Some(b) = current {
            let location = self.layout(b).location;
            x += location.x;
            y += location.y;
            current = self.parent_of(b);
        }
        (x, y)
    }

    /// The border box, in viewport coordinates — what `getBoundingClientRect` returns for a block.
    ///
    /// For an inline box it is the **union of its fragments**, which is the same thing said about a
    /// box that has more than one of them. Chrome, on a `<span>` wrapping across two lines: two
    /// rects of `[39.14, 26, 158.31, 17]` and `[8, 44, 66.7, 17]`, and a bounding rect of
    /// `[8, 26, 189.45, 35]` — exactly their union, to the pixel.
    pub fn border_box(&self, index: u32) -> Rect {
        if self.level_of(index).is_inline_level() && !self.fragments_of(index).is_empty() {
            return self.bounding_rect(index);
        }
        let (x, y) = self.origin(index);
        let size = self.layout(index).size;
        Rect { x, y, width: size.width, height: size.height }
    }

    /// Every rectangle this box contributes, in viewport coordinates.
    ///
    /// **One per line box it occupies.** A block has one; an inline `<a>` broken across three lines
    /// has three, which is the shape this has returned since design D11 and the reason the day it
    /// became true cost no caller a change.
    ///
    /// The fragments are stored relative to the box's IFC, and an inline box's `parent` IS that
    /// IFC, so `origin` is the whole translation — taffy never writes an inline box's own
    /// `final_layout`, which stays at the zero it was built with.
    pub fn client_rects(&self, index: u32) -> Vec<Rect> {
        let fragments = self.fragments_of(index);
        if fragments.is_empty() {
            if self.level_of(index) == crate::layout::style::BoxLevel::Inline {
                // An inline box that reached no line — inside a subtree nothing laid out, or before
                // the first layout — has no rectangle at all rather than a zero one.
                return Vec::new();
            }
            return vec![self.border_box(index)];
        }
        let mut rects: Vec<Rect> = fragments
            .iter()
            .map(|(ifc, r)| {
                let (x, y) = self.origin(*ifc);
                Rect { x: r.x + x, y: r.y + y, ..*r }
            })
            .collect();
        // Line order, which is document order for inline content. A box split around a block-level
        // child collects its halves from two different anonymous boxes, and nothing about the order
        // they were laid out in guarantees the halves arrive top to bottom.
        rects.sort_by(|a, b| {
            a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal).then(
                a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        rects
    }

    /// The union of a box's fragments — `getBoundingClientRect`.
    ///
    /// Not the border box directly, so that the day a box has three fragments this keeps answering
    /// correctly without being revisited.
    pub fn bounding_rect(&self, index: u32) -> Rect {
        let mut rects = self.client_rects(index).into_iter();
        let Some(first) = rects.next() else { return Rect::ZERO };
        rects.fold(first, Rect::union)
    }

    /// The padding box: the border box less its borders. What `clientWidth`/`clientHeight` measure,
    /// and where a scrollbar would eat into.
    pub fn padding_box(&self, index: u32) -> Rect {
        let layout = self.layout(index);
        let border = layout.border;
        let (x, y) = self.origin(index);
        Rect {
            x: x + border.left,
            y: y + border.top,
            width: (layout.size.width - border.left - border.right - layout.scrollbar_size.width)
                .max(0.0),
            height: (layout.size.height - border.top - border.bottom - layout.scrollbar_size.height)
                .max(0.0),
        }
    }

    /// The content box: the padding box less its padding. What the *resolved* `width` and `height`
    /// report, and the containing block a relatively positioned child resolves its insets against.
    pub fn content_box(&self, index: u32) -> Rect {
        let layout = self.layout(index);
        let padding = layout.padding;
        let r = self.padding_box(index);
        Rect {
            x: r.x + padding.left,
            y: r.y + padding.top,
            width: (r.width - padding.left - padding.right).max(0.0),
            height: (r.height - padding.top - padding.bottom).max(0.0),
        }
    }

    /// The initial containing block: the viewport, at the origin. What a `position: fixed` box — and
    /// an absolutely positioned one with no positioned ancestor — measures its insets from.
    pub fn initial_containing_block(&self) -> Rect {
        let viewport = self.viewport();
        Rect { x: 0.0, y: 0.0, width: viewport.width, height: viewport.height }
    }

    /// `offsetWidth`/`offsetHeight`: the border box, as integers.
    ///
    /// Rounded rather than truncated, and rounded *here* rather than by the caller, because
    /// `offsetWidth` is specified to be an integer and a page comparing it against a
    /// `getBoundingClientRect().width` of 100.5 expects 101 and 100.5 respectively.
    ///
    /// An inline box reports its **bounding** box, which is what Chrome does: a `<span>` whose two
    /// fragments span 189.45px of a 200px block reads `offsetWidth` 189, and the 64.94px one beside
    /// it reads 65 — the union, rounded, not either fragment.
    pub fn offset_size(&self, index: u32) -> (i32, i32) {
        let size = if self.level_of(index) == crate::layout::style::BoxLevel::Inline {
            let r = self.bounding_rect(index);
            Size { width: r.width, height: r.height }
        } else {
            self.layout(index).size
        };
        (size.width.round() as i32, size.height.round() as i32)
    }

    /// `clientWidth`/`clientHeight`: the padding box, as integers — **except for the root box**,
    /// where both are the VIEWPORT.
    ///
    /// That exception is not the spec being quoted; it is Chrome 150.0.7871.46 measured over CDP on
    /// five pages built for the question (2026-09-13, viewport 1920×993):
    ///
    /// | page | `documentElement.offsetHeight` | `documentElement.clientHeight` |
    /// |---|---:|---:|
    /// | 50px of content | 50 | **993** |
    /// | 5000px of content | 5000 | **993** |
    /// | `html { height: 300px }` over 5000px | 300 | **993** |
    /// | `html { padding: 40px }` over 5000px | 5080 | **993** |
    /// | `html { overflow: hidden }` over 5000px | 5000 | **993** |
    ///
    /// One number on every page, and it is the one `innerHeight` reports. Korvus answered the root
    /// box's own padding box instead — 50, 5000, 220, 4300 on the four pages measured here, which is
    /// the shape seen on the corpus (`clientHeight` reading 3370, 6933, 24467, 77537 while
    /// `scrollHeight` read 0). Those were never the viewport; they were the document's height under
    /// another name, and a page that sizes a sticky header off `documentElement.clientHeight` got
    /// the whole document.
    ///
    /// `offsetWidth`/`offsetHeight` and `getBoundingClientRect` are NOT special-cased — Chrome
    /// answers the root's real box for all three (`de_rect` is `[0, 0, 1920, 50]` on the short
    /// page), so this is the one member that swaps.
    ///
    /// Quirks mode is where a browser moves this to `<body>` instead. Korvus parses no document in
    /// quirks mode at all (`Document.compatMode` is the constant `CSS1Compat`, and
    /// `scrollingElement` is always the document element), so there is no second case to write.
    ///
    /// A non-atomic **inline** box reports `(0, 0)`, whatever its fragments are. That is the spec
    /// and it is also measured: every inline element on the probe pages — `<a>`, `<span>`, `<b>`, a
    /// `<span>` with 10px of padding and a 3px border, one broken across three lines — answered
    /// `clientWidth` 0 and `clientHeight` 0 in Chrome 150 while reporting a real rect. An inline
    /// box has no padding box to measure; it has a run of line boxes.
    pub fn client_size(&self, index: u32) -> (i32, i32) {
        if self.level_of(index) == crate::layout::style::BoxLevel::Inline {
            return (0, 0);
        }
        if self.is_root(index) {
            let v = self.viewport();
            return (v.width.round() as i32, v.height.round() as i32);
        }
        let r = self.padding_box(index);
        (r.width.round() as i32, r.height.round() as i32)
    }

    /// `clientTop`/`clientLeft`: the top and left BORDER widths, as integers.
    ///
    /// Read off the used border rather than derived as `(offsetHeight - clientHeight) / 2`, which is
    /// what the member's getter used to do. That derivation assumes the two borders on an axis are
    /// equal and that nothing else eats into the padding box, and it stops being true the moment
    /// [`client_size`](Self::client_size) answers the viewport for the root: on a 50px-tall page it
    /// produced `(50 - 993) / 2 = -471` where Chrome answers `0`.
    pub fn border_widths(&self, index: u32) -> (i32, i32) {
        let b = self.layout(index).border;
        (b.top.round() as i32, b.left.round() as i32)
    }

    /// `scrollWidth`/`scrollHeight`: the size of this box's scrolling area, as integers.
    ///
    /// The floor is the padding box ([`client_size`](Self::client_size)) and the content is taffy's
    /// `scrollable_overflow_rect` — the CSS *scrollable overflow region*, measured from the box's
    /// scroll origin, which taffy 0.14 computes for every box and which is exactly the rectangle
    /// this member reports the far edge of. Overflow *before* the scroll origin is unreachable by
    /// scrolling and does not count, which is why only `right`/`bottom` appear here (taffy keeps
    /// `left`/`top` at or below zero for the same reason). Chrome agrees: an absolutely positioned
    /// box at `left: -500px` leaves `documentElement.scrollWidth` at the viewport width.
    ///
    /// Measured against Chrome 150.0.7871.46 (2026-09-13):
    ///
    /// | box | Chrome `scrollHeight` | why |
    /// |---|---:|---|
    /// | `div` 100px tall, 900px child, `overflow: visible` | 900 | overflow counts even where nothing scrolls |
    /// | the same with `overflow: hidden` or `clip` | 900 | the same number; only *scrolling* differs |
    /// | `div` 200×300 content, 7px padding, 3px border, 1234×2345 child | 1248 / 2359 | child + BOTH paddings |
    /// | 50px of content in a 993px viewport | 993 | the root's floor is the viewport |
    /// | `html { padding: 40px }` over 5000px | 5080 | the root's own end padding is in the region |
    ///
    /// The `1248 = 1234 + 7 + 7` row is the interesting one: a scroll container's own end padding is
    /// part of its scrollable overflow region, and taffy already adds it for a box whose `overflow`
    /// makes it a scroll container. The root is a scroll container whatever its `overflow` says —
    /// the viewport scrolls it — so the last row's padding is added here, where taffy would not.
    ///
    /// Rounded, and rounded HALF UP, which is also measured rather than assumed: Chrome turns a
    /// content height of 2000.390625 into 2000, 2000.5 into **2001** and 2000.59375 into 2001, on
    /// the root and on an element alike. `f32::round` is that rule for a non-negative number, and it
    /// is the same one [`offset_size`](Self::offset_size) already uses.
    pub fn scroll_size(&self, index: u32) -> (i32, i32) {
        let (w, h) = self.scroll_extent_f32(index);
        (w.round() as i32, h.round() as i32)
    }

    /// [`scroll_size`](Self::scroll_size) before rounding — the scrolling area in CSS pixels.
    pub(crate) fn scroll_extent_f32(&self, index: u32) -> (f32, f32) {
        // An inline box has no scrolling area, for the same reason it has no padding box: measured
        // on Chrome 150, every inline element on the probe pages answered `scrollWidth` 0 and
        // `scrollHeight` 0 while reporting a real rect.
        if self.level_of(index) == crate::layout::style::BoxLevel::Inline {
            return (0.0, 0.0);
        }
        let layout = self.layout(index);
        let overflow = self.overflow_of(index);
        let (mut right, mut bottom) = (overflow.right, overflow.bottom);
        // The root is the viewport's scrolling box whatever its own `overflow` is, so its end
        // padding joins the region; taffy adds that only for a box its `overflow` already makes a
        // scroll container, so for a root with the usual `overflow: visible` it has to be added
        // here. Chrome: `html { padding: 40px }` over 5000px of content answers 5080.
        if self.is_root(index) && !self.is_scroll_container(index) {
            right += layout.padding.right;
            bottom += layout.padding.bottom;
        }
        // An `<input>`'s value is content the box tree never saw -- it lives in an attribute, not in
        // a child node -- so it is measured once at build time and joins the region here. Chrome:
        // `scrollWidth` 387 against a `clientWidth` of 104 for a 57-character value in a 100px
        // field, where every box below reports 100. See `LayoutBox::text_overflow_width`.
        right = right.max(self.text_overflow_width(index));
        // And the line content of the inline formatting context this box establishes, which taffy
        // never saw. Stored from the border-box origin; the region is measured from the
        // padding edge, the scroll origin.
        right = right.max(self.inline_content_right(index) - layout.border.left);
        // The root's region is the VIEWPORT's (Blink's LayoutView overflow): from the canvas origin, so
        // it starts at the root's border box, and it holds the root's whole margin box and every box
        // the initial containing block lays out besides it but a fixed one (which the viewport's
        // scroll never moves). Chrome 150: `<html>` 3016px tall with `margin: 20px 20px 50px` answers
        // 3086 (it read 3008, the body's border box); a 5000px-wide child of a `margin: 20px` root 5020.
        if self.is_root(index) {
            let (x, y) = (layout.location.x + layout.border.left, layout.location.y + layout.border.top);
            right = (x + right).max(layout.location.x + layout.size.width + layout.margin.right);
            bottom = (y + bottom).max(layout.location.y + layout.size.height + layout.margin.bottom);
            let (start, end) = self.icb_children();
            for i in start..end {
                let c = self.icb_child(i);
                if self.css_position(c) == CssPosition::Fixed {
                    continue;
                }
                let r = self.border_box(c);
                let l = self.layout(c);
                let visible = !self.is_scroll_container(c);
                let over = self.overflow_of(c);
                let (w, h) = if visible { (l.size.width.max(l.border.left + over.right), l.size.height.max(l.border.top + over.bottom)) } else { (l.size.width, l.size.height) };
                // through its transform, as a box in flow contributes (`BoxTree::overflow_of`)
                let (r_right, r_bottom) = match self.effects().and_then(|fx| fx.local(c)).filter(|m| !m.is_identity()) {
                    Some(m) => {
                        let m = m.about(r.x as f64, r.y as f64);
                        let corners = [(r.x, r.y), (r.x + w, r.y), (r.x, r.y + h), (r.x + w, r.y + h)].map(|(x, y)| m.apply(x as f64, y as f64));
                        (corners.iter().map(|p| p.0 as f32).fold(f32::NEG_INFINITY, f32::max), corners.iter().map(|p| p.1 as f32).fold(f32::NEG_INFINITY, f32::max))
                    }
                    None => (r.x + w, r.y + h),
                };
                right = right.max(r_right);
                bottom = bottom.max(r_bottom);
            }
        }
        let floor = if self.is_root(index) {
            self.viewport()
        } else {
            let r = self.padding_box(index);
            Size { width: r.width, height: r.height }
        };
        (floor.width.max(right), floor.height.max(bottom))
    }

    /// How far this box can actually be scrolled on each axis — `scrollWidth - clientWidth` and
    /// `scrollHeight - clientHeight`, floored at zero.
    ///
    /// Zero on a box that is not a scroll container, because such a box does not scroll at all even
    /// though it reports the overflow: Chrome leaves `scrollTop` at 0 after `el.scrollTop = 2e6` on
    /// an `overflow: visible` div whose `scrollHeight` is 900, and at 0 on an `overflow: clip` one —
    /// while the same div with `overflow: hidden` clamps at 800. The ROOT is excluded from that test
    /// because the viewport scrolls it regardless: `html { overflow: hidden }` over 5000px of
    /// content still takes `window.scrollTo(0, 2e6)` to 4007.
    pub fn scroll_extent(&self, index: u32) -> (i32, i32) {
        if !self.is_root(index) && !self.is_scroll_container(index) {
            return (0, 0);
        }
        let (sw, sh) = self.scroll_size(index);
        let (cw, ch) = self.client_size(index);
        ((sw - cw).max(0), (sh - ch).max(0))
    }

    /// Is this the outermost box — `<html>`'s?
    ///
    /// By its missing parent rather than by index 0, because "the box with no containing box" is the
    /// property the answer actually depends on. Every other box has one: a fixed or absolutely
    /// positioned box with no positioned ancestor is given the root as its container (see
    /// `BoxTree::add`), so `None` here is the root and nothing else.
    fn is_root(&self, index: u32) -> bool {
        self.parent_of(index).is_none()
    }

    /// Does this box scroll at all? `overflow: hidden` and `scroll`/`auto` do; `visible` and `clip`
    /// do not. Taffy's own predicate, so it is the same test that decided whether this box's end
    /// padding joined its scrollable overflow region.
    ///
    /// A text field scrolls whatever its `overflow` says, and that is measured rather than reasoned:
    /// Chrome computes `overflow: clip` on every `<input>`, a `<div>` with `overflow: clip` does NOT
    /// scroll (`scrollTop` stays 0 after `= 2e6`), and yet `scrollLeft = 10` sticks on
    /// `<input type=text>` and does not on `<input type=submit>`. The scroller there is the
    /// control's internal editable text container, which no `overflow` keyword names.
    pub fn is_scroll_container(&self, index: u32) -> bool {
        if self.scrolls_own_text(index) {
            return true;
        }
        let overflow = self.style_of_box(index).overflow;
        overflow.x.is_scroll_container() || overflow.y.is_scroll_container()
    }

    /// `offsetLeft`/`offsetTop` when `offsetParent` is null: from the canvas origin.
    pub fn offset_position(&self, index: u32) -> (i32, i32) {
        self.offset_position_from(index, None)
    }

    /// `offsetLeft`/`offsetTop` against `offset_parent`, the box of the element `offsetParent` answers
    /// (`None` when it is null) -- Blink's `LayoutBoxModelObject::AdjustedPositionRelativeTo`, rounded
    /// once at the end as `PixelSnappedOffsetLeft` rounds:
    ///
    /// * `<body>` is 0, 0;
    /// * with no offset parent, the border box from the canvas origin (`<html>` with `margin: 20px` is
    ///   20, 20; a fixed box its place in the viewport);
    /// * an absolutely positioned box from its containing block's border box; any other walks its
    ///   containing blocks up to the offset parent, and past it to `<body>`'s own container when the
    ///   offset parent is a `<body>` that is not positioned -- so an in-flow box is measured from
    ///   `<html>`'s border box (Chrome 150: 26 for a box at 46 under a root at 20);
    /// * less the offset parent's border, unless it is `<body>`: from its padding edge.
    pub fn offset_position_from(&self, index: u32, offset_parent: Option<u32>) -> (i32, i32) {
        let node = self.node_of(index);
        if self.body_node() == Some(node) {
            return (0, 0);
        }
        let own = self.border_box(index);
        let (mut x, mut y) = (own.x, own.y);
        if let Some(p) = offset_parent {
            let parent_is_body = self.body_node() == Some(self.node_of(p));
            let out_of_flow = matches!(self.css_position(index), CssPosition::Absolute | CssPosition::Fixed);
            let base = if out_of_flow {
                // its location in its containing block: the viewport's origin for the initial one
                match self.parent_of(index) {
                    Some(c) if !self.in_icb(index) => self.origin(c),
                    _ => (0.0, 0.0),
                }
            } else if parent_is_body && !self.css_position(p).is_positioned() {
                self.parent_of(p).map_or((0.0, 0.0), |c| self.origin(c))
            } else {
                let r = self.border_box(p);
                (r.x, r.y)
            };
            x -= base.0;
            y -= base.1;
            if !parent_is_body && !self.level_of(p).is_inline_level() {
                let b = self.layout(p).border;
                x -= b.left;
                y -= b.top;
            }
        }
        (x.round() as i32, y.round() as i32)
    }

    // ── client coordinates: the layout, seen through what has been scrolled ─────────
    //
    // Everything above answers in DOCUMENT coordinates, which is what `offsetTop`, `scrollHeight` and
    // the resolved values want. What follows answers in CLIENT coordinates, which is what
    // `getBoundingClientRect`, `getClientRects` and the hit test want. The translation between the two
    // is a walk up the CONTAINING-BLOCK chain -- the box tree's `parent` -- and not the DOM's, which is
    // the whole difference for a positioned box. Measured on Chrome 150.0.7871.46, a document scrolled
    // to `(50, 900)`:
    //
    // ```text
    // #a      absolute, top 1000 left 10                          [-40, 100, 60, 30]  (the document's scroll)
    // #fx     fixed, top 10 left 20                               [20, 10, 30, 30]    (none)
    // #sc-in  in an absolute overflow:auto box scrolled 50        [50, 250, 50, 500]  (both)
    // #scs-abs absolute in a STATIC scroller scrolled 20, whose
    //         containing block is outside it                      [200, 550, 30, 10]  (the document's only)
    // #st     sticky, top 5, in a box that has scrolled past      [450, 5, 100, 30]   (held at the edge)
    // ```

    /// How far this box's OWN scroll moves the boxes it contains. The root's is the viewport's (the
    /// viewport scrolls it whatever its `overflow` is); any other box's is its element's offset when it
    /// is that element's box and a scroll container. An anonymous box shares its element's node and is
    /// not the element's box, so it scrolls nothing.
    fn own_scroll(&self, index: u32, scroll: &ScrollSnapshot) -> (f32, f32) {
        if self.is_root(index) {
            return scroll.viewport;
        }
        let node = self.node_of(index);
        if self.box_of(node) != Some(index) || !self.is_scroll_container(index) {
            return (0.0, 0.0);
        }
        scroll.of(node)
    }

    /// The translation of child `child`, given its containing box `parent`'s translation.
    ///
    /// A `position: fixed` box's containing box is the root (`BoxTree::add`), and it is the one child
    /// of the root that the viewport's scroll does not move: Chrome keeps `#fx` at `[20, 10]` at every
    /// scroll position.
    fn child_translation(&self, parent: u32, parent_translation: (f32, f32), child: u32, scroll: &ScrollSnapshot) -> (f32, f32) {
        let (sx, sy) = if self.is_root(parent) && self.css_position(child) == CssPosition::Fixed {
            (0.0, 0.0)
        } else {
            self.own_scroll(parent, scroll)
        };
        let t = (parent_translation.0 - sx, parent_translation.1 - sy);
        let (dx, dy) = self.sticky_offset(child, t, scroll);
        (t.0 + dx, t.1 + dy)
    }

    /// What to ADD to a box's document-coordinate border box to get its client-coordinate one: minus
    /// the scroll of every scroll container on its containing-block chain, plus any sticky offset.
    pub fn translation(&self, index: u32, scroll: &ScrollSnapshot) -> (f32, f32) {
        let mut chain = vec![index];
        let mut current = self.parent_of(index);
        while let Some(b) = current {
            chain.push(b);
            current = self.parent_of(b);
        }
        let mut t = (0.0, 0.0);
        for pair in chain.windows(2).rev() {
            t = self.child_translation(pair[1], t, pair[0], scroll);
        }
        t
    }

    /// The translation of the boxes `container` contains: its own translation less its own scroll.
    fn contents_translation(&self, container: u32, own: (f32, f32), scroll: &ScrollSnapshot) -> (f32, f32) {
        let (sx, sy) = self.own_scroll(container, scroll);
        (own.0 - sx, own.1 - sy)
    }

    /// Every box's translation, in one pass. Every box is pushed after its containing box
    /// ([`BoxTree::root_box`]), so a parent's entry is always written before its children read it.
    fn translations(&self, scroll: &ScrollSnapshot) -> Vec<(f32, f32)> {
        let mut out: Vec<(f32, f32)> = Vec::with_capacity(self.len());
        for index in 0..self.len() as u32 {
            let t = match self.parent_of(index) {
                Some(p) if (p as usize) < out.len() => self.child_translation(p, out[p as usize], index, scroll),
                Some(_) => self.translation(index, scroll),
                None => (0.0, 0.0),
            };
            out.push(t);
        }
        out
    }

    /// The sticky offset of `index`, whose containing box contents are translated by `t`.
    ///
    /// taffy lays a sticky box out as `relative` and offsets it by its insets, so the offset first
    /// takes that back out: a sticky box's normal position has none. Chrome, a `top: 5px` sticky box at
    /// `top: 800` in a document scrolled to 0 answers `y` **800**, not 805. Then the box is held inside
    /// its nearest scrollport less its insets, as far as its containing block lets it go (CSS Position
    /// 3 §3.4): scrolled to 900, the same box answers `y` 5 while its containing block still covers
    /// that edge.
    fn sticky_offset(&self, index: u32, t: (f32, f32), scroll: &ScrollSnapshot) -> (f32, f32) {
        if self.css_position(index) != CssPosition::Sticky {
            return (0.0, 0.0);
        }
        let Some(parent) = self.parent_of(index) else { return (0.0, 0.0) };
        let style = self.style_of_box(index);
        let cb = self.content_box(parent);
        let pick = |a: Option<f32>, b: Option<f32>| match (a, b) {
            (Some(a), _) => a,
            (None, Some(b)) => -b,
            (None, None) => 0.0,
        };
        use crate::layout::resolved::resolve_inset;
        let rel_x = pick(resolve_inset(style.inset.left, cb.width), resolve_inset(style.inset.right, cb.width));
        let rel_y = pick(resolve_inset(style.inset.top, cb.height), resolve_inset(style.inset.bottom, cb.height));
        let b = self.border_box(index);
        let r = Rect { x: b.x + t.0 - rel_x, y: b.y + t.1 - rel_y, ..b };
        let cb = Rect { x: cb.x + t.0, y: cb.y + t.1, ..cb };
        let margin = self.layout(index).margin;
        // The nearest scrollport: the viewport's, or a scroll container's padding box less its bars.
        let mut s = parent;
        while !self.is_root(s) && !(self.box_of(self.node_of(s)) == Some(s) && self.is_scroll_container(s)) {
            match self.parent_of(s) {
                Some(p) => s = p,
                None => break,
            }
        }
        let port = if self.is_root(s) {
            let v = self.viewport();
            Rect { x: 0.0, y: 0.0, width: v.width, height: v.height }
        } else {
            let ts = self.translation(s, scroll);
            let pb = self.padding_box(s);
            Rect { x: pb.x + ts.0, y: pb.y + ts.1, ..pb }
        };
        let (mut dx, mut dy) = (0.0f32, 0.0f32);
        if let Some(top) = resolve_inset(style.inset.top, port.height) {
            let limit = port.y + top;
            if r.y < limit {
                dy = (limit - r.y).min((cb.bottom() - r.bottom() - margin.bottom).max(0.0));
            }
        }
        if let Some(bottom) = resolve_inset(style.inset.bottom, port.height) {
            let limit = port.bottom() - bottom;
            if r.bottom() + dy > limit {
                dy -= (r.bottom() + dy - limit).min((r.y + dy - margin.top - cb.y).max(0.0));
            }
        }
        if let Some(left) = resolve_inset(style.inset.left, port.width) {
            let limit = port.x + left;
            if r.x < limit {
                dx = (limit - r.x).min((cb.right() - r.right() - margin.right).max(0.0));
            }
        }
        if let Some(right) = resolve_inset(style.inset.right, port.width) {
            let limit = port.right() - right;
            if r.right() + dx > limit {
                dx -= (r.right() + dx - limit).min((r.x + dx - margin.left - cb.x).max(0.0));
            }
        }
        (dx - rel_x, dy - rel_y)
    }

    /// [`BoxTree::client_rects`] in client coordinates, with the translations of the boxes involved
    /// read from `translation_of`.
    pub(crate) fn viewport_rects_with(&self, index: u32, scroll: &ScrollSnapshot, translation_of: &dyn Fn(u32) -> (f32, f32)) -> Vec<Rect> {
        let fragments = self.fragments_of(index);
        if fragments.is_empty() {
            if self.level_of(index) == crate::layout::style::BoxLevel::Inline {
                return Vec::new();
            }
            let b = self.border_box(index);
            let (tx, ty) = translation_of(index);
            return vec![Rect { x: b.x + tx, y: b.y + ty, ..b }];
        }
        let mut rects: Vec<Rect> = fragments
            .iter()
            .map(|(ifc, r)| {
                let (x, y) = self.origin(*ifc);
                let (tx, ty) = self.contents_translation(*ifc, translation_of(*ifc), scroll);
                Rect { x: r.x + x + tx, y: r.y + y + ty, ..*r }
            })
            .collect();
        rects.sort_by(|a, b| {
            a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal).then(
                a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        rects
    }

    /// `getClientRects`: [`BoxTree::client_rects`] in client coordinates, as laid out. The transforms over them are the
    /// webapi's to apply (`geometry_queries::transformed`, Blink's quad mapping, measured to the float).
    pub fn viewport_client_rects(&self, index: u32, scroll: &ScrollSnapshot) -> Vec<Rect> {
        self.viewport_rects_with(index, scroll, &|b| self.translation(b, scroll))
    }

    // ── transforms (D77, `crate::effects`) ──────────────────────────────────────────────────────────

    /// Keep what each box's style does beyond layout, read from the style whose fingerprint is `key`.
    pub fn set_effects(&mut self, key: u64, effects: crate::effects::BoxEffects) {
        self.effects = Some((key, std::sync::Arc::new(effects)));
        self.overflow_fx = std::cell::OnceCell::new();
    }

    /// The fingerprint of the style the kept effects were read from.
    pub fn effects_key(&self) -> Option<u64> {
        self.effects.as_ref().map(|(k, _)| *k)
    }

    /// The kept effects, when they are this tree's (one entry per box).
    pub(crate) fn effects(&self) -> Option<&crate::effects::BoxEffects> {
        self.effects.as_ref().map(|(_, e)| &**e).filter(|e| e.local.len() == self.len())
    }

    /// Box `index`'s own transform in client coordinates: its local one about its border box as scrolled to `t`.
    fn own_map(&self, fx: &crate::effects::BoxEffects, index: u32, t: (f32, f32)) -> Option<crate::effects::Affine> {
        let local = fx.local(index)?;
        let b = self.border_box(index);
        Some(local.about((b.x + t.0) as f64, (b.y + t.1) as f64))
    }

    /// Where box `index` shows: the map from its untransformed client coordinates to the screen -- its own transform
    /// and those of every box above it, outermost last. `None` when none of them transforms.
    pub fn screen_map(&self, index: u32, scroll: &ScrollSnapshot) -> Option<crate::effects::Affine> {
        let fx = self.effects().filter(|f| f.any_transform)?;
        let mut chain = vec![index];
        let mut current = self.parent_of(index);
        while let Some(b) = current {
            chain.push(b);
            current = self.parent_of(b);
        }
        let mut m: Option<crate::effects::Affine> = None;
        for &b in chain.iter().rev() {
            if fx.local(b).is_none() {
                continue;
            }
            let own = self.own_map(fx, b, self.translation(b, scroll))?;
            m = Some(match m {
                Some(outer) => outer.then(&own),
                None => own,
            });
        }
        m
    }

    /// Every box's [`BoxTree::screen_map`], in one pass over the box translations of one scroll.
    pub(crate) fn screen_maps(&self, translations: &[(f32, f32)]) -> Vec<Option<crate::effects::Affine>> {
        let n = self.len();
        let Some(fx) = self.effects().filter(|f| f.any_transform) else { return vec![None; n] };
        let mut out: Vec<Option<crate::effects::Affine>> = Vec::with_capacity(n);
        for index in 0..n as u32 {
            let outer = self.parent_of(index).and_then(|p| out.get(p as usize).copied().flatten());
            let own = self.own_map(fx, index, translations[index as usize]);
            out.push(match (outer, own) {
                (Some(o), Some(m)) => Some(o.then(&m)),
                (o, m) => o.or(m),
            });
        }
        out
    }

    /// Every box's translation (the scroll of its containers, its sticky offset), for the paint walk.
    pub(crate) fn box_translations(&self, scroll: &ScrollSnapshot) -> Vec<(f32, f32)> {
        self.translations(scroll)
    }

    /// Does box `index` clip what it contains (`overflow` other than `visible`)? For the paint walk.
    pub(crate) fn clips_contents(&self, index: u32) -> bool {
        self.clips(index)
    }

    /// Where each fragment of block `root`'s inline formatting context starts, in viewport coordinates as
    /// [`BoxTree::text_fragments`] places them: `(top, bottom, left)` per fragment, of every box laid out in its lines.
    pub fn ifc_fragment_starts(&self, root: u32, scroll: &ScrollSnapshot) -> Vec<(f32, f32, f32)> {
        let (x, y) = self.origin(root);
        let (tx, ty) = self.contents_translation(root, self.translation(root, scroll), scroll);
        let inside = |b: u32| {
            let mut up = self.parent_of(b);
            while let Some(p) = up {
                if p == root {
                    return true;
                }
                if p < root {
                    return false;
                }
                up = self.parent_of(p);
            }
            false
        };
        let mut out = Vec::new();
        let mut b = root + 1;
        while (b as usize) < self.len() && inside(b) {
            out.extend(self.fragments_of(b).iter().filter(|(ifc, _)| *ifc == root).map(|(_, r)| (r.y + y + ty, r.y + y + ty + r.height, r.x + x + tx)));
            b += 1;
        }
        out
    }

    /// The laid-out text of a text box, one entry per line fragment IN LINE ORDER —
    /// `(the fragment's string, its Rect in viewport coords)`. Empty for a non-text box. Unlike
    /// [`viewport_client_rects`] this does NOT sort (sorting would break the rect↔string pairing): the
    /// string of fragment `i` is the atoms from `text_lines[i]` up to `text_lines[i+1]`, taken from
    /// [`text_atom_strings`] (atom-for-atom with the measured atoms). The compositor's paint walk reads
    /// this to emit `FillText` per line.
    pub fn text_fragments(&self, index: u32, scroll: &ScrollSnapshot) -> Vec<(String, Rect)> {
        let fragments = self.fragments_of(index);
        if fragments.is_empty() {
            return Vec::new();
        }
        let atoms = self.text_atom_strings(index); // empty for a non-text box
        if atoms.is_empty() {
            return Vec::new();
        }
        let starts = self.maps.text_lines.get(&index).cloned().unwrap_or_default();
        if starts.is_empty() {
            return Vec::new();
        }

        // Fragments are sub-line pieces (one box can emit several per line), so there are generally MORE
        // fragments than lines; `text_lines` has one atom-start per VISUAL line. Map each fragment to
        // viewport coords, then group fragments into lines by y (a line's fragments share a top), in line
        // order (top to bottom). Each line's rect is the union of its fragments; its string is the atoms
        // from `starts[i]` up to `starts[i+1]`.
        let mut rects: Vec<Rect> = fragments
            .iter()
            .map(|(ifc, r)| {
                let (x, y) = self.origin(*ifc);
                let (tx, ty) = self.contents_translation(*ifc, self.translation(*ifc, scroll), scroll);
                Rect { x: r.x + x + tx, y: r.y + y + ty, ..*r }
            })
            .collect();
        rects.sort_by(|a, b| {
            a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
        let mut lines: Vec<Rect> = Vec::new();
        for r in rects {
            match lines.last_mut() {
                Some(last) if (r.y - last.y).abs() <= 1.0 => *last = last.union(r),
                _ => lines.push(r),
            }
        }
        if std::env::var_os("RENDER_DBG").is_some() {
            eprintln!(
                "RENDER-DBG tf idx={} fragments={} lines={} starts={:?} atoms={}",
                index, self.fragments_of(index).len(), lines.len(), starts, atoms.len()
            );
        }

        let mut out = Vec::with_capacity(lines.len());
        for (i, rect) in lines.iter().enumerate() {
            let a = starts.get(i).copied().unwrap_or(0);
            let b = starts.get(i + 1).copied().unwrap_or(atoms.len());
            let s: String = atoms.get(a..b).unwrap_or(&[]).concat();
            if !s.trim().is_empty() {
                out.push((s, *rect));
            }
        }
        out
    }

    /// `getBoundingClientRect`: the union of [`BoxTree::viewport_client_rects`].
    pub fn viewport_bounding_rect(&self, index: u32, scroll: &ScrollSnapshot) -> Rect {
        let mut rects = self.viewport_client_rects(index, scroll).into_iter();
        let Some(first) = rects.next() else { return Rect::ZERO };
        rects.fold(first, Rect::union)
    }

    /// The padding box less its scrollbars, in client coordinates -- the area a scroll container
    /// shows its content through, and an intersection observer's explicit root.
    pub fn viewport_padding_box(&self, index: u32, scroll: &ScrollSnapshot) -> Rect {
        let (tx, ty) = self.translation(index, scroll);
        let r = self.padding_box(index);
        Rect { x: r.x + tx, y: r.y + ty, ..r }
    }

    /// Does this box clip what it contains? Any `overflow` other than `visible` does -- except on the
    /// root, whose `overflow` is the viewport's (and a `<body>` whose `overflow` was propagated there
    /// already reads `visible` here). Chrome: `elementFromPoint` on a point where a child of an
    /// `overflow: auto` box has been scrolled out of its scrollport answers what is BEHIND the box.
    fn clips(&self, index: u32) -> bool {
        if self.is_root(index) || self.box_of(self.node_of(index)) != Some(index) {
            return false;
        }
        let overflow = self.style_of_box(index).overflow;
        overflow.x != taffy::style::Overflow::Visible || overflow.y != taffy::style::Overflow::Visible
    }

    /// Every box's rectangles in client coordinates, clipped by the boxes that clip them: the input
    /// of a hit test. `None` for a box a clip removed entirely.
    pub(crate) fn hit_geometry(&self, scroll: &ScrollSnapshot) -> Vec<(Vec<Rect>, Option<Rect>)> {
        let translations = self.translations(scroll);
        let mut clips: Vec<Option<Rect>> = Vec::with_capacity(self.len());
        let mut out = Vec::with_capacity(self.len());
        for index in 0..self.len() as u32 {
            let clip = match self.parent_of(index) {
                Some(p) if (p as usize) < clips.len() => {
                    let inherited = clips[p as usize];
                    if self.clips(p) {
                        let (tx, ty) = translations[p as usize];
                        let pb = self.padding_box(p);
                        let own = Rect { x: pb.x + tx, y: pb.y + ty, ..pb };
                        Some(match inherited {
                            Some(c) => intersect(c, own),
                            None => own,
                        })
                    } else {
                        inherited
                    }
                }
                _ => None,
            };
            clips.push(clip);
            let rects = self.viewport_rects_with(index, scroll, &|b| translations[b as usize]);
            out.push((rects, clip));
        }
        out
    }
}

/// The overlap of two rectangles; a zero-sized one where they do not overlap.
fn intersect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    Rect { x, y, width: (right - x).max(0.0), height: (bottom - y).max(0.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_union_covers_both_rectangles() {
        let a = Rect { x: 0.0, y: 0.0, width: 10.0, height: 10.0 };
        let b = Rect { x: 20.0, y: 5.0, width: 10.0, height: 10.0 };
        assert_eq!(a.union(b), Rect { x: 0.0, y: 0.0, width: 30.0, height: 15.0 });
        assert_eq!(a.union(a), a);
    }

    // Half-open on the right and bottom, so two boxes sharing an edge do not both claim the point.
    #[test]
    fn containment_is_half_open_at_the_far_edges() {
        let r = Rect { x: 10.0, y: 10.0, width: 10.0, height: 10.0 };
        assert!(r.contains(10.0, 10.0), "the near edges are inside");
        assert!(r.contains(19.9, 19.9));
        assert!(!r.contains(20.0, 15.0), "the far edges are not");
        assert!(!r.contains(15.0, 20.0));
        assert!(!r.contains(9.9, 15.0));
    }
}
