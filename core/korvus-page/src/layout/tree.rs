//! The box tree, and taffy driven over it.
//!
//! # Bring your own tree
//!
//! taffy ships a `TaffyTree` that owns its nodes and a `Style` for each. Using it would mean every
//! element's computed style existed twice — once in the cascade's arena and once in taffy's node —
//! which is design D8's objection. Instead taffy is used through its traits over *this* arena: it
//! stores nothing, it allocates nothing, and it asks for a style by node when it needs one.
//!
//! # A box is not an element
//!
//! The DOM and the box tree are different trees, and conflating them is the mistake that makes
//! layout engines wrong in ways that are hard to unpick later. `display: none` produces no box for
//! an element *or its subtree*; an inline element produces line boxes rather than one box; a
//! `<style>` element is in the DOM and nowhere on the page. So a box records which element it came
//! from, an element may have no box, and the map between them is a parallel `Vec` (design D4)
//! rather than a hash map, because `NodeId` is already an index.

use crate::font::{BuiltinFonts, FontMetrics, FontProvider};
use crate::layout::geometry::Rect;
use crate::layout::ifc::Item;
use crate::layout::inline::{self, FontSpec};
use crate::layout::replaced::{self, Natural};
use crate::layout::style::{
    generates_box, AutoScroll, BoxLevel, CssPosition, LayoutStyles, TextAlignKind, Variant,
};
use crate::style::computed::{StyleArena, StyleKey};
use crate::style::property::PropertyId;
use korvus_dom::{Dom, NodeId, NodeKind};
use taffy::geometry::{Line, Point, Size};
use taffy::style::{AvailableSpace, Dimension, Display, ExpandedDimension, Float, LengthPercentage, LengthPercentageAuto, Overflow, Style};
use taffy::CacheTree;
use taffy::tree::{
    Cache, Layout, LayoutBlockContainer, LayoutFlexboxContainer, LayoutGridContainer, LayoutInput,
    LayoutOutput, LayoutPartialTree, PrintTree, RequestedAxis, RoundTree, RunMode, SizingMode,
    TraversePartialTree, TraverseTree,
};

/// A box's layout cache: taffy's, and behind it the measurements taffy's slots have given up.
///
/// taffy keeps nine measurements a box and replaces one when a tenth comes, while a flex or block container asks a
/// child at more constraints than that in one layout -- min-content, max-content, its hypothetical and its final size.
/// A child asked again at an evicted constraint measures its whole subtree again, and its children do the same below
/// it. On www.costco.com's home page (2026-10-22, layouts of 14-18k boxes) that was 27-30 computations a box a layout,
/// a box 30 levels down measured 309 times in one layout, and a layout 370-430 ms; with these kept, 20-22 computations
/// a box and 290-320 ms a layout (same session, interleaved; peak page RSS 813-895 MB against 853-866 MB without).
///
/// Every measurement taffy may cache is kept here as well, by its EXACT input, and asked when taffy misses. An answer is
/// only handed back for an input equal to the one it was computed for -- bit for bit, stricter than taffy's own match --
/// and only until the box's cache is cleared, which clears both: it is the answer laying the box out again would give.
///
/// A measurement taffy declines to cache -- one carrying margin-collapse state, which taffy's size-only slots cannot
/// rebuild -- is kept WHOLE in `whole`, by its exact input, and handed back field for field. Every `<p>`, heading, list
/// and margin card measures with its own margins in that state (taffy's block algorithm puts a box's non-zero margins
/// there), so none of them was ever answered from a cache: their parent's every measurement measured them and their
/// subtrees again -- 3,880 of a costco-like page's 5,451 computations a relayout (2026-10-23).
#[derive(Debug, Clone, Default)]
struct BoxCache {
    taffy: Cache,
    /// The most recent measurements, oldest first, at most [`SPILLED_MEASUREMENTS`].
    spilled: Vec<(LayoutInput, Size<f32>)>,
    /// The most recent measurements carrying margin-collapse state, whole, oldest first, at most [`WHOLE_MEASUREMENTS`].
    whole: Vec<(LayoutInput, LayoutOutput)>,
}

/// How many margin-carrying measurements a box keeps whole. A box is measured at a handful of constraints a layout
/// (min-content, max-content, a hypothetical and a final width), so 8 holds them with room for a second container pass.
const WHOLE_MEASUREMENTS: usize = 8;

/// How many measurements a box keeps beyond taffy's nine. 16 bounds what a box can hold; 64 saved only 7 % more
/// computations on costco.com (20.2 a box against 21.7).
const SPILLED_MEASUREMENTS: usize = 16;

impl BoxCache {
    fn get(&mut self, input: &LayoutInput) -> Option<LayoutOutput> {
        if let Some(out) = self.taffy.get(input) {
            return Some(out);
        }
        if input.run_mode != RunMode::ComputeSize {
            return None;
        }
        if let Some((_, size)) = self.spilled.iter().rev().find(|(k, _)| same_input(k, input)) {
            return Some(LayoutOutput::from_outer_size(*size));
        }
        let (_, out) = self.whole.iter().rev().find(|(k, _)| same_input(k, input))?;
        crate::perf_counters::add(&crate::perf_counters::MARGIN_CACHE_HITS, 1);
        Some(*out)
    }

    /// Keep `output` for `input`. Answers whether it was a MEASUREMENT refused for carrying margin-collapse state
    /// ([`LayoutStats::margin_refused`]); a laying-out store is never one.
    fn store(&mut self, input: &LayoutInput, output: LayoutOutput) -> bool {
        self.taffy.store(input, output);
        if input.run_mode != RunMode::ComputeSize {
            return false;
        }
        // taffy's own rule: a measurement is rebuilt from its size alone, so one carrying margin-collapse state is not kept
        // there -- it is kept whole.
        if output.margins_can_collapse_through
            || output.top_margin != taffy::CollapsibleMarginSet::ZERO
            || output.bottom_margin != taffy::CollapsibleMarginSet::ZERO
        {
            if !knob(Knob::MarginCache) {
                return true;
            }
            if let Some(at) = self.whole.iter().position(|(k, _)| same_input(k, input)) {
                self.whole.remove(at);
            } else if self.whole.len() == WHOLE_MEASUREMENTS {
                self.whole.remove(0);
            }
            self.whole.push((*input, output));
            return true;
        }
        if let Some(at) = self.spilled.iter().position(|(k, _)| same_input(k, input)) {
            self.spilled.remove(at);
        } else if self.spilled.len() == SPILLED_MEASUREMENTS {
            self.spilled.remove(0);
        }
        self.spilled.push((*input, output.size));
        false
    }

    fn clear(&mut self) {
        self.taffy.clear();
        self.spilled.clear();
        self.whole.clear();
    }
}

/// Two layout inputs equal in every field, lengths compared bit for bit (no `-0 == 0`, no NaN surprises).
fn same_input(a: &LayoutInput, b: &LayoutInput) -> bool {
    let len = |x: Option<f32>, y: Option<f32>| x.map(f32::to_bits) == y.map(f32::to_bits);
    let space = |x: AvailableSpace, y: AvailableSpace| match (x, y) {
        (AvailableSpace::Definite(p), AvailableSpace::Definite(q)) => p.to_bits() == q.to_bits(),
        (AvailableSpace::MinContent, AvailableSpace::MinContent) | (AvailableSpace::MaxContent, AvailableSpace::MaxContent) => true,
        _ => false,
    };
    a.run_mode == b.run_mode
        && a.sizing_mode == b.sizing_mode
        && a.axis == b.axis
        && len(a.known_dimensions.width, b.known_dimensions.width)
        && len(a.known_dimensions.height, b.known_dimensions.height)
        && a.known_dimensions_are_definite == b.known_dimensions_are_definite
        && len(a.parent_size.width, b.parent_size.width)
        && len(a.parent_size.height, b.parent_size.height)
        && space(a.available_space.width, b.available_space.width)
        && space(a.available_space.height, b.available_space.height)
        && a.vertical_margins_are_collapsible == b.vertical_margins_are_collapsible
}

/// What one [`BoxTree::compute`] did, counted on the tree itself: deterministic numbers a single-threaded test can
/// assert on, where the process-wide [`crate::perf_counters`] count every thread's work and only in a diag build.
/// Zeroed at the start of each `compute`.
#[derive(Clone, Copy, Default, Debug)]
pub struct LayoutStats {
    /// Box layouts computed (every cache miss).
    pub computed: u64,
    /// Of those, the MEASURING ones (`RunMode::ComputeSize`).
    pub measures: u64,
    /// taffy root runs: the first pass, every percentage-padding pass, every content-visibility and scrollbar pass.
    pub root_runs: u64,
    /// Boxes whose percentage padding moved between two passes.
    pub padding_moved: u64,
    /// Measurements taffy's cache refused because they carry margin-collapse state.
    pub margin_refused: u64,
    /// Flex items whose cache a container's definiteness flip cleared.
    pub flex_item_clears: u64,
    /// Final-pass states a scrollbar pass took over from the tree laid out before.
    pub states_reused: u64,
    /// Boxes whose laid-out answer the scrollbar pass refused because a carried state is below them.
    pub final_stale_marked: u64,
}

/// The test-only switches of the incremental layout's fast paths: each is `true` (on) outside `cfg(test)`, and a
/// test turns one off to lay a page out the way the code before it did and compare the two.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) enum Knob {
    /// A flex container's definiteness flip clears only its replaced items' caches.
    FlexFlip,
    /// Measurements carrying margin-collapse state are cached whole.
    MarginCache,
    /// The full-window pass and the final pass keep a state each.
    TwoStates,
    /// Cache lookups by the canonical form of an input.
    Canonical,
    /// Whole-tree scans of a pass walk maintained lists.
    Lists,
    /// A style-only change patches the kept box tree in place.
    Patch,
}

#[cfg(test)]
thread_local! {
    /// The switches turned OFF on this thread, one bit per [`Knob`].
    static KNOBS_OFF: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Is the fast path `k` on? Always, outside a test.
#[inline(always)]
pub(crate) fn knob(k: Knob) -> bool {
    #[cfg(test)]
    return KNOBS_OFF.with(|c| c.get() & (1 << k as u8) == 0);
    #[cfg(not(test))]
    {
        let _ = k;
        true
    }
}

/// Run `f` with the switches `off` turned off on this thread, and turn them back on after.
#[cfg(test)]
pub(crate) fn with_knobs<R>(off: &[Knob], f: impl FnOnce() -> R) -> R {
    let before = KNOBS_OFF.with(|c| c.get());
    let bits = off.iter().fold(before, |b, k| b | (1 << *k as u8));
    KNOBS_OFF.with(|c| c.set(bits));
    struct Restore(u8);
    impl Drop for Restore {
        fn drop(&mut self) {
            KNOBS_OFF.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(before);
    f()
}

/// What the image loader knows of one `<img>`'s current request (A/B C8). The loader is
/// `korvus-webapi`'s; it hands these over before each layout ([`crate::Page::set_natural_images`]), and an
/// image it says nothing about has no request settled yet -- in flight, deferred, or never made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageState {
    /// The image is available, with its decoded natural size.
    Available(f32, f32),
    /// The request failed (or named nothing to fetch): Blink renders the element's FALLBACK content
    /// instead of an image -- `HTMLImageElement::EnsureCollapsedOrFallbackContent`, see
    /// [`BoxTree::image_fallback`].
    Broken,
}

/// What one element's pseudo-elements add to layout: the `::before` and `::after`
/// boxes its `content` generates, and a `::-webkit-scrollbar` thickness per axis.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Generated {
    pub before: Option<PseudoBox>,
    pub after: Option<PseudoBox>,
    /// `(vertical bar width, horizontal bar height)` from `::-webkit-scrollbar`; an axis that is
    /// `None` keeps the platform theme's thickness.
    pub scrollbar: Option<(Option<f32>, Option<f32>)>,
}

/// How two box trees of one document differ: see [`BoxTree::first_difference`].
#[derive(Debug, Clone)]
pub struct BoxDifference {
    /// The element (or text node) of the first box that differs; `None` for the tree as a whole.
    pub node: Option<NodeId>,
    pub what: String,
}

impl BoxDifference {
    /// The difference, with the node named by `name`.
    pub fn describe(&self, name: impl Fn(NodeId) -> String) -> String {
        match self.node {
            Some(n) => format!("{}: {}", name(n), self.what),
            None => self.what.clone(),
        }
    }
}

/// One generated box: its computed style and the text its `content` renders as.
#[derive(Debug, Clone, PartialEq)]
pub struct PseudoBox {
    pub key: StyleKey,
    pub text: String,
}

/// `<noscript>` renders nothing when scripting is enabled, which it always is here -- and its
/// contents are the raw text the parser kept for exactly that case. Chrome 150:
/// `<noscript><p>no script text</p></noscript>` has an `offsetHeight` of 0 and no line of the page's
/// `innerText`, while its computed `display` is still `inline`: Blink skips the box, not the style.
///
/// A shadow host's light child that NO SLOT renders is the other case. A host renders its
/// shadow tree, and a light child of it only where a `<slot>` puts it; a host whose shadow tree has no
/// matching slot renders that child nowhere. Chrome, measured by WPT
/// `shadow-dom/untriaged/.../methods/test-002.html`: a `<span>` inside a host answers `offsetTop` 0
/// after `attachShadow({mode:"open"})`. This engine lays the SHADOW tree out nowhere at all -- the
/// style walk never enters it -- so a host whose shadow tree does have a slot keeps rendering its light
/// children where the slot would have put them, which is the closer of the two wrong answers and the
/// one every framework's slotted content depends on.
/// An element whose `display` computes to `contents` and that renders at all: it generates no box, and
/// its children are laid out in its place.
fn is_contents(dom: &Dom, arena: &StyleArena, style_of: &dyn Fn(NodeId) -> Option<StyleKey>, node: NodeId) -> bool {
    style_of(node).is_some_and(|k| arena.value(k, PropertyId::Display) == "contents") && !renders_nothing(dom, node)
}

pub fn renders_nothing(dom: &Dom, node: NodeId) -> bool {
    if dom.is_html_ns(node) && dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("noscript")) {
        return true;
    }
    dom.parent(node).is_some_and(|p| dom.shadow_root_of(p).is_some()) && dom.assigned_slot(node).is_none()
}

/// Is `child` inside the closed part of a `<details>` -- anything but its first `<summary>` child
/// while `open` is absent? Those children are slotted into `::details-content`, whose
/// `content-visibility` is `hidden`: they are laid out, inside a slot that takes no
/// space, and they render no text. Chrome 150 on `<details><summary>sum</summary><div
/// style="width:100px;height:100px"></div><p>para text</p></details>`: the details is 18px tall (the
/// summary's line), `getBoundingClientRect()` puts the div at (8, 26) 100x100 and the `<p>` at y 142
/// (WPT `closed-details-layout-apis.tentative.html` asserts the same), and `innerText` is `sum`.
pub fn closed_details_content(dom: &Dom, parent: NodeId, child: NodeId) -> bool {
    let is = |n: NodeId, tag: &str| dom.is_html_ns(n) && dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case(tag));
    if !is(parent, "details") || dom.get_attribute(parent, "open").is_some() {
        return false;
    }
    let first_summary = dom.children(parent).into_iter().find(|&c| is(c, "summary"));
    first_summary != Some(child)
}

/// Blink's quirky-margin elements: their UA `margin-block-start` is written in `__qem`, which a
/// quirks-mode document ignores at the top of `<body>` and of a table cell. Chrome 150,
/// no doctype: `<body><p>` puts the paragraph at y 8, not 16.
fn quirky_margin_top_factor(dom: &Dom, node: NodeId) -> Option<f32> {
    if !dom.is_html_ns(node) {
        return None;
    }
    Some(match dom.tag_name(node)?.to_ascii_lowercase().as_str() {
        "p" | "blockquote" | "ul" | "menu" | "dir" | "ol" | "dl" | "pre" | "xmp" | "plaintext" | "listing" | "h3" => 1.0,
        "h1" => 0.67,
        "h2" => 0.83,
        "h4" => 1.33,
        "h5" => 1.67,
        "h6" => 2.33,
        _ => return None,
    })
}

/// What an `<input>`'s `type` makes of its value, for the scrolling area only.
///
/// Measured on Chrome 150.0.7871.46 over CDP (2026-09-13): 22 `type` values, one 57-character value
/// each, in a 100×50 field, reading `scrollWidth/clientWidth` and then whether `scrollLeft = 10`
/// sticks.
///
/// ```text
/// text search url tel email      387/104 (search 400/96)   scrollLeft 10   TextEntry
/// password                       275/104                   scrollLeft 10   Password (bullets)
/// submit button reset            389/96                    scrollLeft  0   Button
/// number                         104/104                   scrollLeft  0   Other
/// date time month week           101/101                   scrollLeft  0   Other
/// datetime-local color file      101/101, 98/98, 100/100   scrollLeft  0   Other
/// checkbox radio range image     100/100                   scrollLeft  0   Other
/// hidden                           0/0                     scrollLeft  0   Other
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    /// `text`, `search`, `url`, `tel`, `email` — and a missing or unrecognised `type`, which is the
    /// attribute's own invalid-value default.
    TextEntry,
    /// `password`: the same, except that what it renders is one U+2022 per character.
    Password,
    /// `submit`, `button`, `reset`: the label overflows the box and is reported by `scrollWidth`,
    /// but the control is not a scroller and `scrollLeft` stays 0.
    Button,
    /// Every other type: the value contributes nothing to the scrolling area.
    Other,
}

impl InputKind {
    fn of(attr: Option<&str>) -> InputKind {
        let kind = attr.unwrap_or("text").trim().to_ascii_lowercase();
        match kind.as_str() {
            "password" => InputKind::Password,
            "search" | "url" | "tel" | "email" => InputKind::TextEntry,
            "submit" | "button" | "reset" => InputKind::Button,
            // Every type this engine knows renders SOMETHING other than a plain value; an
            // unrecognised one is `text`, which is the HTML attribute's invalid-value default.
            "number" | "date" | "time" | "month" | "week" | "datetime-local" | "color" | "file"
            | "checkbox" | "radio" | "range" | "image" | "hidden" => InputKind::Other,
            _ => InputKind::TextEntry,
        }
    }

    fn value_widens_the_scrolling_area(self) -> bool {
        matches!(self, InputKind::TextEntry | InputKind::Password | InputKind::Button)
    }

    /// Can the control be scrolled through that area? Text fields yes, buttons no — which is the
    /// second column of the table above, and the reason the two questions are separate.
    fn scrolls_its_value(self) -> bool {
        matches!(self, InputKind::TextEntry | InputKind::Password)
    }
}

/// One box. Twelve bytes of identity plus taffy's own two fields, which is what taffy would have
/// stored per node anyway — the difference is that the *style* is not among them.
#[derive(Debug)]
struct LayoutBox {
    /// The element this box came from.
    node: NodeId,
    /// Into [`LayoutStyles`], not a style of its own.
    style: u32,
    /// A range of [`BoxTree::child_list`].
    children: (u32, u32),
    /// The containing box. Kept because every geometry question a page asks is in *viewport*
    /// coordinates and taffy answers in parent-relative ones, so the walk up has to be cheap.
    parent: Option<u32>,
    /// A text run, whitespace already collapsed. `None` for an element's own box.
    ///
    /// Text gets a box of its own rather than being a property of its parent, because a block can
    /// hold text *and* elements (`<p>some <b>bold</b> text</p>` is three runs) and each of them has
    /// to be placed. It is an anonymous block here, which stacks the runs; making them share a line
    /// is the rest of P3.
    text: Option<Box<str>>,
    /// How wide this box's own **unlaid-out** content is, for `scrollWidth` only.
    ///
    /// One element has content the child walk above cannot see: `<input>` keeps its value in an
    /// ATTRIBUTE, so unlike `<textarea>` — whose value is a text child and is measured like any
    /// other run — the walk finds nothing inside it and the control's scrolling area comes out
    /// exactly its own padding box. Chrome's is its value: 387 against a `clientWidth` of 104 for a
    /// 57-character string in a 100px field (measured, 150.0.7871.46, 2026-09-13).
    ///
    /// It is a NUMBER ON THE BOX rather than a text child, because a text child would be laid out —
    /// it would give the field an intrinsic width and height it does not have, and a browser's
    /// `<input>` is sized by its `size` attribute and the UA sheet, never by its value. Nothing in
    /// the layout reads this; only [`BoxTree::scroll_size`] does.
    text_overflow_width: f32,
    /// Is this box a scroll container because of what it IS rather than because of its `overflow`?
    ///
    /// A text field is. Chrome's UA sheet computes `overflow: clip` on every `<input>` — measured,
    /// and a `<div>` with `overflow: clip` does NOT scroll (`scrollTop` stays 0 after `= 2e6`) — yet
    /// `scrollLeft = 10` sticks on a text field and does not on a submit button. The scroller is the
    /// control's internal editable text container, which no `overflow` keyword describes.
    scrolls_own_text: bool,
    /// The natural size of a replaced element or form control, for the boxes that are one and no
    /// other. `Some` routes this box through [`crate::layout::replaced::compute_replaced_layout`]
    /// instead of the leaf measure, which had no size to give it. Boxed because it is rare: a page
    /// of 5,000 boxes and 40 images pays eight bytes a box, not twenty.
    natural: Option<Box<Natural>>,
    /// Which formatting context this box belongs to. An `Inline` box is **not** a taffy child of
    /// anything: it has no single rectangle, only the fragments below.
    level: BoxLevel,
    /// The inline content this box establishes a formatting context over, for an anonymous IFC box
    /// and nothing else. `None` on every other box, which is the overwhelming majority.
    ifc: Option<Box<[Item]>>,
    /// An absolutely positioned or fixed box's place in normal flow, which an `auto` inset puts it at
    /// (CSS 2 §10.3.7's static position): the box its parent element generated, and the in-flow box
    /// before it there, if any.
    static_flow: Option<(u32, Option<u32>)>,
    /// Out of flow, written in a line of inline content before anything in flow on it: its static position is that
    /// line's top, not below it (Chromium places a block-level one there; apple.com's region check mark, the first
    /// thing in its label, sat a line below where Chrome draws it).
    static_line_start: bool,
    /// A checkbox or radio button drawn by the theme: its baseline is its border box's bottom (Blink's
    /// `AdjustCheckboxStyle` sets `InlineBlockBaselineEdge::kBorderBox`).
    theme_part: bool,
    /// A control whose baseline is its own line of text's -- a text field, a button-type `<input>`, a
    /// drop-down `<select>` -- not its bottom margin edge.
    text_control: bool,
    /// A `<button>`, which Blink lays out as a flexbox: with no line of its own its baseline is
    /// synthesized from its content box, as a flex or grid container's is.
    button: bool,
    /// A table cell's `colspan` and `rowspan` (HTML §4.9.11: colspan 1..1000, rowspan 0..65534, 0 = to
    /// the end of its row group), `(1, 1)` for anything else.
    pub(super) spans: (u16, u16),
}

impl LayoutBox {
    /// The facts the builder recorded on this box that its style does not say: a cell's spans, whether it is a
    /// theme-drawn part, a text control or a `<button>`, and whether it is out of flow with a static position (the
    /// position itself names boxes, which two trees number differently). A tree built with any of them different
    /// lays out differently: `td.colSpan = 2` moves no style key and leaves every box, node and origin as it was.
    /// Packed in one word: one compare.
    fn built_facts(&self) -> u64 {
        let flags = self.theme_part as u64
            | (self.text_control as u64) << 1
            | (self.button as u64) << 2
            | (self.static_flow.is_some() as u64) << 3
            | (self.static_line_start as u64) << 4;
        self.spans.0 as u64 | (self.spans.1 as u64) << 16 | flags << 32
    }

    /// A box with nothing decided about it yet, for the fields every constructor repeats.
    fn new(node: NodeId, style: u32, parent: Option<u32>, level: BoxLevel) -> LayoutBox {
        LayoutBox {
            node,
            style,
            children: (0, 0),
            parent,
            text: None,
            text_overflow_width: 0.0,
            scrolls_own_text: false,
            natural: None,
            level,
            ifc: None,
            static_flow: None,
            static_line_start: false,
            theme_part: false,
            text_control: false,
            button: false,
            spans: (1, 1),
        }
    }
}

/// What one PASS of a layout leaves on a box: where it put it, what it cached, the paddings it resolved. Kept apart
/// from [`LayoutBox`] -- what the box IS -- because a layout under a classic viewport scrollbar has two passes that
/// want different answers: the full-window pass that decides whether the bar shows, and the final pass at the
/// narrower width. One set of answers per box made each evict the other's on every relayout: a 1,600-cell flex grid
/// computed 7,244 box layouts a relayout for one cell's padding write (2026-10-23). [`BoxTree::compute`] keeps one
/// state per pass ([`BoxTree::st`] and [`BoxTree::alt`]) and carries both.
#[derive(Debug, Clone)]
struct BoxState {
    /// This box's style with its percentage paddings already turned into lengths, for the boxes
    /// that have any. `None` — the overwhelmingly common case — means the interned style is used
    /// as it is, and no page without percentage padding pays a byte more than an `Option` for this.
    ///
    /// It is per *box* and not per style because the number it holds is a fact about where the box
    /// sits: two elements with the identical style in containing blocks of different widths have
    /// different used paddings.
    padding_override: Option<Box<Style>>,
    /// The inline size `padding_override` was resolved against, or `NaN` before the first pass.
    /// The override is a pure function of the style and this number, so comparing bases is the
    /// same test as comparing overrides and does not need `Style` to be comparable.
    padding_basis: f32,
    /// Is the box a flex or grid item, whose `padding_override` gives way to the shared style only while its container
    /// measures its own inner width from its content ([`BoxTree::style_of_box`])? Set with the override. Any other
    /// box's gives way while its own width or its container's is being measured. taffy measures a flex item's base
    /// size with no containing-block width either way -- to keep a percentage SIZE out of it -- and Chrome 152
    /// counts the item's percentage padding there against the container's width when that is definite (a 400px row:
    /// 247.11 wide for 147.11 of text) and as nothing when the container is sizing itself from its items (an
    /// `inline-flex` around the same item: 147.11 wide, the item two lines). A grid item's resolves against its grid
    /// area, which this engine does not give it.
    padding_item: bool,
    /// For a box that establishes an inline formatting context: the right edge of its widest line
    /// content, from its border-box origin, as the last IFC pass placed it. Taffy's
    /// scrollable overflow never sees line boxes -- the IFC is this crate's own pass -- so a
    /// `white-space: nowrap` line of five 100px inline-blocks in a 300px box read `scrollWidth` 300
    /// where Chrome 150 reads 500 (`tools/realweb/seed/sw`). Only [`BoxTree::scroll_size`] reads it.
    inline_right: f32,
    /// Where this box ended up, one rectangle per line box it occupies, **relative to its IFC
    /// box's origin**. Empty for every block-level box, whose one rectangle is `final_layout`.
    ///
    /// Relative to the IFC rather than to the viewport because the IFC box moves — a percentage
    /// padding pass, or a second `compute`, relays out the page — and re-walking every fragment on
    /// each move would make the cheap case pay for the rare one. An inline box's `parent` IS its
    /// IFC box and taffy never writes its `final_layout`, so `origin()` adds exactly the right
    /// offset with no special case.
    ///
    /// Each fragment names the IFC box it came from, because a box can belong to **two**: a
    /// block-level element inside a `<span>` splits that span, and CSS puts the halves in the
    /// anonymous boxes on either side of the block. The two have different origins, so the owner is
    /// not derivable from `parent` — `parent` is only the first of them.
    fragments: Vec<(u32, Rect)>,
    /// For a flex container, while taffy lays its items out: whether its cross size is DEFINITE, the
    /// condition under which a stretched item's cross size is definite too (CSS Flexbox §9.8). Read by
    /// [`BoxTree::indefinite_stretch`]; meaningless on any other box.
    flex_cross_definite: bool,
    unrounded: Layout,
    final_layout: Layout,
    cache: BoxCache,
    /// The cached LAID-OUT answer must not be reused: a line measuring an atomic inline
    /// below this box laid that box out at the measuring width, so what this box last laid out below
    /// it is no longer there. Its next laying-out call runs again; its measurements stay cached.
    final_stale: bool,
    /// The style origin ([`LayoutStyles::origin`]) this state's cache entries and positions were computed under:
    /// before every pass, a box whose style moved since forgets them ([`BoxTree::sync_origins`]).
    origin: u64,
    /// A final-pass state carried over from the tree laid out before (see [`BoxTree::compute`]).
    carried: bool,
    /// Carried from another tree, and not yet checked against this tree's styles ([`BoxTree::sync_origins`]).
    inherited: bool,
    /// Written by the pass running now, so its snapped copy is owed ([`BoxTree::touch`]). False between passes.
    touched: bool,
    /// How far a table cell's children stand moved by its `vertical-align` ([`BoxTree::shift_children`]); 0 once a
    /// layout of the box places them again.
    valign_shift: f32,
}

impl Default for BoxState {
    fn default() -> Self {
        BoxState {
            padding_override: None,
            padding_basis: f32::NAN,
            padding_item: false,
            inline_right: 0.0,
            fragments: Vec::new(),
            flex_cross_definite: false,
            unrounded: Layout::new(),
            final_layout: Layout::new(),
            cache: BoxCache::default(),
            final_stale: false,
            origin: 0,
            carried: false,
            inherited: false,
            touched: false,
            valign_shift: 0.0,
        }
    }
}

/// The per-pass facts kept by box rather than on it: each IFC's last baseline and each text box's line starts.
#[derive(Debug, Clone, Default)]
pub(super) struct PassMaps {
    /// Each inline formatting context's last line's baseline, from its top, after its last layout --
    /// what an `inline-block` holding text is aligned by.
    pub(super) last_baselines: std::collections::HashMap<u32, f32>,
    /// For each text box, the index of the first text atom on each of its lines, in line order --
    /// one entry per fragment the box has.
    pub(super) text_lines: std::collections::HashMap<u32, Vec<usize>>,
}

/// The tree [`BoxTree::carry`] carries from: shared, its states copied; or owned, its states moved out.
enum Kept<'a> {
    Shared(&'a BoxTree),
    Owned(&'a mut BoxTree),
}

/// A state vector of the tree carried from, laid out by this tree's boxes: box `a` of each pair `(a, b)` of `sorted`
/// -- going up in both -- takes `old[b]` through `take`, every other box `fresh(a)`. `old` is walked once.
fn in_order(
    old: Vec<BoxState>,
    sorted: &[(u32, u32)],
    n: usize,
    mut take: impl FnMut(usize, BoxState) -> BoxState,
    mut fresh: impl FnMut(usize) -> BoxState,
) -> Vec<BoxState> {
    let mut out = Vec::with_capacity(n);
    let mut old = old.into_iter().enumerate();
    let mut pairs = sorted.iter().peekable();
    for a in 0..n {
        match pairs.peek() {
            Some(&&(pa, pb)) if pa as usize == a => {
                pairs.next();
                // the states between are not carried, and are dropped
                match old.by_ref().find(|(j, _)| *j == pb as usize) {
                    Some((_, state)) => out.push(take(a, state)),
                    None => out.push(fresh(a)),
                }
            }
            _ => out.push(fresh(a)),
        }
    }
    out
}

impl Kept<'_> {
    fn tree(&self) -> &BoxTree {
        match self {
            Kept::Shared(t) => t,
            Kept::Owned(t) => t,
        }
    }

    /// Box `b`'s state, from `alt` or `st`: copied, or moved out.
    fn state(&mut self, alt: bool, b: usize) -> BoxState {
        match self {
            Kept::Shared(t) => (if alt { &t.alt } else { &t.st })[b].clone(),
            Kept::Owned(t) => std::mem::take(&mut (if alt { &mut t.alt } else { &mut t.st })[b]),
        }
    }

    /// The measured text atoms: copied, or taken.
    fn atoms(&mut self) -> Vec<Option<(u32, std::sync::Arc<[TextAtom]>)>> {
        match self {
            Kept::Shared(t) => t.atoms.borrow().clone(),
            Kept::Owned(t) => std::mem::take(&mut *t.atoms.borrow_mut()),
        }
    }

    /// All of `alt`'s or `st`'s states and their maps: copied, or taken.
    fn states(&mut self, alt: bool) -> (Vec<BoxState>, PassMaps) {
        match self {
            Kept::Shared(t) => {
                if alt {
                    (t.alt.clone(), t.alt_maps.clone())
                } else {
                    (t.st.clone(), t.maps.clone())
                }
            }
            Kept::Owned(t) => {
                if alt {
                    (std::mem::take(&mut t.alt), std::mem::take(&mut t.alt_maps))
                } else {
                    (std::mem::take(&mut t.st), std::mem::take(&mut t.maps))
                }
            }
        }
    }

    /// Box `b`'s entries of the maps that go with those states, under box `a` in `to`.
    fn maps_into(&mut self, alt: bool, b: usize, to: &mut PassMaps, a: usize) {
        let (b, a) = (b as u32, a as u32);
        match self {
            Kept::Shared(t) => {
                let from = if alt { &t.alt_maps } else { &t.maps };
                if let Some(&base) = from.last_baselines.get(&b) {
                    to.last_baselines.insert(a, base);
                }
                if let Some(lines) = from.text_lines.get(&b) {
                    to.text_lines.insert(a, lines.clone());
                }
            }
            Kept::Owned(t) => {
                let from = if alt { &mut t.alt_maps } else { &mut t.maps };
                if let Some(base) = from.last_baselines.remove(&b) {
                    to.last_baselines.insert(a, base);
                }
                if let Some(lines) = from.text_lines.remove(&b) {
                    to.text_lines.insert(a, lines);
                }
            }
        }
    }
}

/// The boxes a document generates, and where they end up.
pub struct BoxTree {
    /// What each box's style does beyond layout -- `transform`, `opacity` (D77, `crate::effects`) -- and the
    /// fingerprint of the style it was read from. Set by the caller after layout; `None` until then.
    pub(crate) effects: Option<(u64, std::sync::Arc<crate::effects::BoxEffects>)>,
    /// The `content-visibility: auto` boxes, each with the style it has when RELEVANT.
    /// Every one starts skipped; [`BoxTree::compute`] unlocks the ones near the viewport.
    cv_auto: Vec<(u32, u32)>,
    /// Each text box's measured atoms, with the style they were measured in. Taffy sizes a
    /// flex or grid item several times per layout (min-content, max-content, the final width), and
    /// every sizing re-measured the box's text word by word -- 5.5 % of hubspot.com's load CPU
    /// (2026-09-29). A box's text is fixed once the tree is built; a style change (the margin
    /// variants) is caught by the style id.
    atoms: std::cell::RefCell<Vec<Option<(u32, std::sync::Arc<[TextAtom]>)>>>,
    boxes: Vec<LayoutBox>,
    /// Each box's state from the pass that ran last, parallel to `boxes`: what every reader reads.
    st: Vec<BoxState>,
    /// The per-pass maps that go with `st`.
    pub(super) maps: PassMaps,
    /// The OTHER pass's states, parallel to `boxes`, or empty: after a compute that ran a final scrollbar pass
    /// (`live_final`), the full-window pass's; before one, the final states carried from the tree before. See
    /// [`BoxTree::compute`].
    alt: Vec<BoxState>,
    alt_maps: PassMaps,
    /// `st` is the FINAL pass's state of a compute that ran a scrollbar pass and `alt` the full-window pass's.
    live_final: bool,
    /// States were carried into this tree -- from the tree before it, or from its own last layout by a patch: it is
    /// a RELAYOUT, and its compute keeps a state per pass (see [`BoxTree::compute`]).
    relaid: bool,
    /// Every box's children, flattened. One allocation for the whole tree instead of one per box.
    child_list: Vec<u32>,
    styles: LayoutStyles,
    /// Where text measurements come from — the persona's font metrics.
    fonts: Box<dyn FontProvider>,
    /// Which web font table `fonts` measures with ([`crate::font::WebFonts::id`]; 0 for none): a tree built with
    /// another measured its text in other faces, and none of its layout carries over ([`BoxTree::carry_from`]).
    fonts_id: u64,
    /// Element → box, indexed by `NodeId`. A parallel `Vec` and not a `HashMap` (design D4):
    /// `NodeId` is already an arena index, so a hash would be paying for a lookup we already have.
    box_of_node: Vec<Option<u32>>,
    /// The viewport the last [`BoxTree::compute`] laid out into — the *initial containing block*.
    ///
    /// Kept because a resolved value needs it and cannot get it any other way: the used `left` of a
    /// `position: fixed` box is measured from the viewport edge, and the root box's own border box
    /// is not the same rectangle (a root with `width: 50%` is half of it).
    ///
    /// It is the LAYOUT viewport: the window's inner size less any classic scrollbar the viewport
    /// shows. Chrome 150 on a page taller than its 780px window answers `innerWidth`
    /// 780 and `documentElement.clientWidth` 765, and a `position: fixed; right: 0` box sits at 755.
    viewport: Size<f32>,
    /// What the viewport's scrollbars do, per axis: the root's `overflow`, or the `<body>`'s when it
    /// is propagated (CSS Overflow 3 §3.3). See [`ViewportScroll`].
    viewport_scroll: Point<ViewportScroll>,
    /// The thickness of the viewport's scrollbars: the root's used `scrollbar-width` under the theme.
    viewport_bar: f32,
    /// The viewport shows no scrollbars (`MediaContext::scrollbars_off`, an `<iframe scrolling=no>`'s document).
    viewport_bars_off: bool,
    /// Which viewport scrollbars an earlier pass of this [`BoxTree::compute`] has shown. Sticky, as a
    /// box's are: a scrollbar that narrows the page cannot make it overflow less, and letting one
    /// disappear again is how a layout oscillates.
    viewport_bars: Point<bool>,
    /// Each element's generated boxes and scrollbar thickness.
    generated: std::collections::HashMap<NodeId, Generated>,
    /// The box each `::before` (`false`) / `::after` (`true`) generated.
    pseudo_boxes: std::collections::HashMap<(NodeId, bool), u32>,
    /// The document's mode: quirks mode sizes `<html>`/`<body>` to the viewport and ignores quirky
    /// margins; both quirks modes apply the line-height quirk.
    mode: korvus_dom::DocumentMode,
    /// The root's first `<body>` child, for the quirks that name it.
    body_node: Option<NodeId>,
    /// While a closed `<details>`' content is being collected into its zero-height slot: that element.
    details_slot: Option<NodeId>,
    /// The flat tree the boxes are built from: a host's box holds its shadow tree's, a slot's its
    /// assigned light children's.
    flat: crate::flat::FlatTree,
    /// Boxes whose quirky UA top margin a quirks-mode document dropped. Their `margin-top` still
    /// RESOLVES to the computed value: Chrome 150 reports `16px` for the `<p>` it placed at y 8.
    quirk_margin_boxes: std::collections::HashSet<u32>,
    /// The DOM text node each text box was built from, for the boxes that were built from one --
    /// what a caret position names. Generated content has no text node and no entry.
    pub(super) text_nodes: std::collections::HashMap<u32, NodeId>,
    /// The state of each `<img>` whose request has settled: available with its decoded natural size,
    /// or broken (A/B C8).
    natural_images: std::collections::HashMap<NodeId, ImageState>,
    /// Each box's style origin as it was BUILT ([`LayoutStyles::origin`]), before `compute` moved some
    /// (scrollbars, the quirks fill): what [`BoxTree::same_build`] compares.
    built: Vec<u64>,
    /// The viewport the last [`BoxTree::compute`] was asked for (before any scrollbar narrowed it).
    requested: Option<Size<f32>>,
    /// Lay out every pass from empty caches, as: the reference `KORVUS_VERIFY_INCREMENTAL`
    /// checks the cache-keeping passes against ([`BoxTree::compute_from_nothing`]).
    exhaustive: bool,
    /// How many calls on the layout stack are only MEASURING (`RunMode::ComputeSize`). A box laid out
    /// while one is -- taffy's block algorithm lays its children out to size itself, and a line lays
    /// its atomic inlines out to read their baselines -- is left laid out at a measuring size, so every
    /// box above it forgets its laid-out answer (`LayoutBox::final_stale`).
    measuring: u32,
    /// The calls on the layout stack that are computing rather than answering from a cache, innermost last: the box,
    /// whether the call measures it with no containing-block width given (`RunMode::ComputeSize`, no `parent_size`
    /// width), and whether it measures its own width from its content (no known width, and for a flex or grid container
    /// none from its style either: [`BoxTree::width_from_style`]). [`BoxTree::style_of_box`]
    /// reads the innermost to resolve a percentage padding against nothing while the width it is a percentage of is
    /// being measured -- what the call's own inputs say, so its cached answers stay keyed on them.
    sizing: Vec<(u32, bool, bool)>,
    /// A fingerprint of what this tree was BUILT from -- the style-key version, the DOM's
    /// match revision, the theme and the natural-image generation. When the next layout computes the
    /// same fingerprint, `build_with_origins` would produce a `same_build`-identical tree, so the build
    /// (and, if the viewport is unchanged, the whole layout) is skipped and this tree is kept. `None` on
    /// a tree not built through that path (a test, the from-nothing reference). See [`BoxTree::build_fp`].
    build_fp: Option<u64>,
    /// The initial containing block: what taffy lays out as the root (see [`Icb`]).
    icb: Icb,
    /// What the last [`BoxTree::compute`] did.
    stats: LayoutStats,
    /// Each box's carry hash ([`BoxTree::carry_hashes`]): a function of what the tree was built with, made once.
    carry_h: std::cell::OnceCell<Vec<u64>>,
    /// Each box's scrollable overflow with the kept effects' transforms taken ([`BoxTree::overflow_of`]), made once
    /// per effects (`set_effects` empties it); `None` where taffy's stands.
    pub(super) overflow_fx: std::cell::OnceCell<Vec<Option<taffy::geometry::Rect<f32>>>>,
    /// A later tree MOVED its states out of this one ([`BoxTree::carry_from_moving`]): this tree's layout is gone,
    /// and nothing may answer a question from it.
    gutted: bool,
    /// The boxes the pass running now wrote a layout or fragments of, each once: what the end of the pass snaps.
    touched: Vec<u32>,
    /// The boxes a pass's whole-tree scans act on, in box order, made at the start of each compute (and again when a
    /// `content-visibility` unlock moves styles): a percentage padding, an `overflow: auto` axis, a static position.
    /// Each scan still tests every box it visits against the current style.
    pct_boxes: Vec<u32>,
    auto_boxes: Vec<u32>,
    abs_boxes: Vec<u32>,
    /// Each box's style as BUILT -- before a compute showed a scrollbar or filled the quirks viewport -- parallel to
    /// `boxes` (`built` is these styles' origins): what a patched tree is computed again from.
    built_style: Vec<u32>,
    /// Every element's computed style the tree was built (or last patched) from, by node, and what else it rests on:
    /// see [`BoxTree::patch_styles`]. Empty on a tree not built through a page's layout pass.
    built_keys: Vec<Option<StyleKey>>,
    patch_basis: Option<PatchBasis>,
    /// How many layout styles the table held when the tree was built: a tree patched so often that its table has
    /// grown far past that is built again, which starts a table of the styles in use.
    styles_at_build: usize,
    /// Per box: whether a float is somewhere below it, as the styles are at this layout (`lay_out` reads them
    /// again: a style patch can float a box in place). Such a box is laid out in its parent's block formatting
    /// context, uncached, so its floats reach the boxes after it ([`BoxTree::compute_block_child_layout`]).
    floats_within: Vec<bool>,
}

/// What a box tree was built from besides its elements' styles, for [`BoxTree::patch_styles`]: the DOM's structure
/// revision and instance, the journal position, the document root and the natural-image generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PatchBasis {
    pub structure: u64,
    pub seq: u64,
    pub instance: u64,
    pub root: NodeId,
    pub images: u64,
}

/// What [`BoxTree::patch_styles`] did: whether any box's layout row moved (a relayout is owed), and whether every
/// patched box was laid out under its built style (then a tree no box of which moved is the layout as it is).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Patched {
    pub moved: bool,
    pub simple: bool,
}

/// taffy's node id for the INITIAL CONTAINING BLOCK, which is no box of this tree.
const ICB_NODE: usize = u32::MAX as usize;

/// The initial containing block, laid out as taffy's root: a viewport-sized block whose in-flow child
/// is `<html>`'s box and whose other children are the absolutely positioned and fixed boxes no
/// positioned ancestor contains (CSS 2 §10.1). Measured against Chrome 150.0.7871.46 at the persona's
/// 1920x965 viewport: `position: absolute; bottom: 0; height: 50%` with no positioned ancestor is
/// 482.5px tall at y 482.5 whatever the document's height (this engine had laid it out in `<html>`'s
/// 116px-tall box: 58px at y 58), and `html { margin: 20px }` puts `<html>`'s border box at (20, 20)
/// -- taffy's own root placement ignores the root's margins (it was at (0, 0)).
///
/// Those boxes stay `<html>`'s children in [`BoxTree::parent_of`], which is what every geometry walk
/// reads: their taffy location (from the viewport origin) is made `<html>`-relative after each pass.
/// When `<html>` is itself positioned it contains them, and they stay its taffy children too.
#[derive(Default)]
struct Icb {
    /// Its children, as a range of `child_list`: `<html>` (box 0), then the positioned ones.
    children: (u32, u32),
    /// A block the viewport's size.
    style: Style,
    /// `<html>`'s style as the initial containing block lays it out: a `flow-root` when it is a block,
    /// because the root's margins never collapse with its children's (CSS 2 §8.3.1) and taffy would
    /// collapse a block child's.
    root_style: Style,
    layout: Layout,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportScroll {
    /// A scrollbar when the document overflows: `visible` and `auto`, the overwhelmingly common case.
    Auto,
    /// Always a scrollbar: `overflow: scroll`.
    Scroll,
    /// Never one: `hidden` and `clip`. The document still scrolls by script; it shows nothing.
    Hidden,
}

impl ViewportScroll {
    fn of(overflow: Overflow, auto: bool) -> ViewportScroll {
        match overflow {
            Overflow::Scroll => ViewportScroll::Scroll,
            Overflow::Hidden if auto => ViewportScroll::Auto,
            Overflow::Hidden | Overflow::Clip => ViewportScroll::Hidden,
            Overflow::Visible => ViewportScroll::Auto,
        }
    }
}

/// How far past its client box a scroll container's content must reach before `auto` shows a
/// scrollbar: half of Blink's 1/64 px layout unit, so float noise in an exactly-fitting box is not
/// overflow and a real 1/64 px overflow is.
const OVERFLOW_EPSILON: f32 = 0.5 / 64.0;

/// How many times [`BoxTree::compute`] will reveal scrollbars and lay out again. One pass settles a
/// vertical scrollbar; the narrower box can then need a horizontal one, and a viewport scrollbar can
/// narrow a box that then overflows. Three covers the chains Chrome was measured on (`200x100` with a
/// `190x95` child and a `300x1` one: 185 × 85, both bars) and bounds the pathological rest.
const SCROLLBAR_PASSES: usize = 3;

/// How many times [`BoxTree::compute`] re-lays a page out to unlock `content-visibility: auto` boxes
/// that came within range. Each pass unlocks every box then in range at once; a page
/// whose unlocked boxes keep pulling more into range is bounded here.
const CV_PASSES: usize = 4;

impl Default for BoxTree {
    fn default() -> Self {
        BoxTree {
            effects: None,
            cv_auto: Vec::new(),
            atoms: Default::default(),
            boxes: Vec::new(),
            st: Vec::new(),
            maps: PassMaps::default(),
            alt: Vec::new(),
            alt_maps: PassMaps::default(),
            live_final: false,
            relaid: false,
            child_list: Vec::new(),
            styles: LayoutStyles::default(),
            fonts: Box::new(BuiltinFonts),
            fonts_id: 0,
            box_of_node: Vec::new(),
            viewport: Size { width: 0.0, height: 0.0 },
            viewport_scroll: Point { x: ViewportScroll::Auto, y: ViewportScroll::Auto },
            viewport_bar: 0.0,
            viewport_bars_off: false,
            viewport_bars: Point { x: false, y: false },
            generated: std::collections::HashMap::new(),
            pseudo_boxes: std::collections::HashMap::new(),
            mode: korvus_dom::DocumentMode::NoQuirks,
            body_node: None,
            details_slot: None,
            flat: Default::default(),
            quirk_margin_boxes: std::collections::HashSet::new(),
            text_nodes: std::collections::HashMap::new(),
            natural_images: std::collections::HashMap::new(),
            built: Vec::new(),
            requested: None,
            exhaustive: false,
            measuring: 0,
            sizing: Vec::new(),
            build_fp: None,
            icb: Icb::default(),
            stats: LayoutStats::default(),
            carry_h: std::cell::OnceCell::new(),
            overflow_fx: std::cell::OnceCell::new(),
            gutted: false,
            touched: Vec::new(),
            pct_boxes: Vec::new(),
            auto_boxes: Vec::new(),
            abs_boxes: Vec::new(),
            built_style: Vec::new(),
            built_keys: Vec::new(),
            patch_basis: None,
            styles_at_build: 0,
            floats_within: Vec::new(),
        }
    }
}

impl std::fmt::Debug for BoxTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxTree")
            .field("boxes", &self.boxes.len())
            .field("styles", &self.styles.len())
            .finish()
    }
}

impl BoxTree {
    /// Build the boxes for a document.
    ///
    /// `style_of` answers with an element's computed style, or `None` for an element that has none
    /// — which is how the caller says "this element is not in the flow".
    pub fn build(
        dom: &Dom,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
    ) -> BoxTree {
        BoxTree::build_with_theme(dom, arena, style_of, spec::ThemeProfile::default())
    }

    /// [`BoxTree::build`] under the worn device's platform theme — the scrollbar thickness every
    /// scroll container gives up and the form-control theme paddings. The default
    /// theme is none at all, which is what [`BoxTree::build`] lays out with.
    pub fn build_with_theme(
        dom: &Dom,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        theme: spec::ThemeProfile,
    ) -> BoxTree {
        BoxTree::build_generated(dom, dom.root(), arena, style_of, theme, &std::collections::HashMap::new(), &std::collections::HashMap::new())
    }

    /// [`BoxTree::build_with_theme`] with the elements' generated content, for the
    /// document whose arena root is `root`.
    ///
    /// The root is a parameter because one arena holds several documents: the page's, and every
    /// nested browsing context's. A frame's document is laid out by this same walk
    /// from ITS root, into the frame's viewport, and nothing outside that tree reaches its boxes.
    pub fn build_generated(
        dom: &Dom,
        root: NodeId,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        theme: spec::ThemeProfile,
        generated: &std::collections::HashMap<NodeId, Generated>,
        natural_images: &std::collections::HashMap<NodeId, ImageState>,
    ) -> BoxTree {
        BoxTree::build_with_origins(dom, root, arena, style_of, theme, generated, natural_images, None, Default::default())
    }

    /// [`BoxTree::build_generated`] starting from the style rows an earlier tree of the same theme
    /// worked out ([`LayoutStyles::take_key_rows`]).
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_origins(
        dom: &Dom,
        root: NodeId,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        theme: spec::ThemeProfile,
        generated: &std::collections::HashMap<NodeId, Generated>,
        natural_images: &std::collections::HashMap<NodeId, ImageState>,
        web_fonts: Option<&std::sync::Arc<crate::font::WebFonts>>,
        key_rows: crate::layout::style::KeyRows,
    ) -> BoxTree {
        let mut tree = BoxTree {
            box_of_node: vec![None; dom.len()],
            generated: generated.clone(),
            natural_images: natural_images.clone(),
            mode: dom.document_mode(root),
            ..Default::default()
        };
        tree.styles.set_theme(theme);
        tree.styles.set_key_rows(key_rows);
        // The worn persona's font names: the theme carries which bundle's they are. The document's web fonts come
        // first under the families its `@font-face` rules declare.
        let persona = crate::font::PersonaFonts::for_key(theme.font_map);
        tree.fonts = match web_fonts {
            Some(web) => Box::new(crate::font::WithWebFonts { persona, web: web.clone() }),
            None => Box::new(persona),
        };
        tree.fonts_id = web_fonts.map_or(0, |w| w.id());
        // The document element, not the document node: `<html>` is the outermost box.
        let Some(element) = first_element(dom, root) else { return tree };
        tree.body_node = dom.children(element).into_iter().find(|&c| {
            dom.is_html_ns(c) && dom.tag_name(c).is_some_and(|t| t.eq_ignore_ascii_case("body"))
        });
        // One child list per box while the tree is being walked, because a box's children are not
        // all discovered while that box is on the stack: an absolutely positioned descendant is
        // attached to its containing block, which may be several frames further up.
        let mut pending: Vec<Vec<u32>> = Vec::new();
        tree.add(dom, arena, style_of, element, None, None, &mut pending);
        // Flatten into the one allocation the tree actually reads (design D4). The boxes the initial
        // containing block contains -- every fixed one, and every absolute one when `<html>` is not
        // positioned -- are laid out as ITS children (see `Icb`), after `<html>`.
        let root_positioned = tree.boxes.first().is_some_and(|b| tree.styles.position(b.style).is_positioned());
        let mut icb = Vec::new();
        for (index, mut children) in pending.into_iter().enumerate() {
            if index == 0 {
                let styles = &tree.styles;
                let boxes = &tree.boxes;
                let (out, kept): (Vec<u32>, Vec<u32>) = children.into_iter().partition(|&c| match styles.position(boxes[c as usize].style) {
                    CssPosition::Fixed => true,
                    CssPosition::Absolute => !root_positioned,
                    _ => false,
                });
                children = kept;
                icb = out;
            }
            let start = tree.child_list.len() as u32;
            tree.child_list.extend(children);
            tree.boxes[index].children = (start, tree.child_list.len() as u32);
        }
        if !tree.boxes.is_empty() {
            let start = tree.child_list.len() as u32;
            tree.child_list.push(0);
            tree.child_list.extend(icb);
            tree.icb.children = (start, tree.child_list.len() as u32);
        }
        if tree.mode == korvus_dom::DocumentMode::Quirks {
            tree.drop_quirky_margins(dom, arena, style_of);
        }
        tree.built = tree.boxes.iter().map(|b| tree.styles.origin(b.style)).collect();
        tree.built_style = tree.boxes.iter().map(|b| b.style).collect();
        tree.styles_at_build = tree.styles.len();
        // The states are made by the carry or the compute (`ensure_states`): a carry of the same structure takes the
        // last tree's whole, and a tree kept unlaid-out needs none.
        tree
    }

    /// What the tree was built from, for a later layout to patch it in place ([`BoxTree::patch_styles`]).
    pub(crate) fn set_patch_basis(&mut self, basis: PatchBasis, keys: Vec<Option<StyleKey>>) {
        self.patch_basis = Some(basis);
        self.built_keys = keys;
    }

    pub(crate) fn patch_basis(&self) -> Option<PatchBasis> {
        self.patch_basis
    }

    pub(crate) fn theme(&self) -> spec::ThemeProfile {
        self.styles.theme()
    }

    /// PATCH this laid-out tree to elements' computed styles `keys`, where only attributes that move styles have been
    /// written since it was built (the caller's guard: the DOM's structure revision, the journal, the root, the theme,
    /// the viewport, the generated content and the images as they were). Each element whose style moved and that
    /// has a box is a group: its box and everything inside it are built again, by the UNCHANGED builder, into a
    /// scratch tree holding its parent's box -- and when that comes out box for box as this tree has it, the new
    /// styles are installed, and nothing else changes. `None` when any group is not that simple (an inline-level or
    /// out-of-flow box, a table part, the root or `<body>`, a `display: contents` parent, a closed `<details>`, a
    /// shadow tree, a `content-visibility` box...) or comes out otherwise: the caller builds the tree as before.
    ///
    /// A style write followed by a geometry read built the whole box tree again -- the builder, the carry hashes,
    /// the pairing -- for one element: half of such a write's cycles on a 1,600-cell flex grid (2026-10-23).
    pub(crate) fn patch_styles(&mut self, dom: &Dom, arena: &StyleArena, keys: &[Option<StyleKey>]) -> Option<Patched> {
        if keys.len() != self.built_keys.len() || self.st.len() != self.boxes.len() || self.boxes.is_empty() {
            return None;
        }
        if self.styles.len() > 2 * self.styles_at_build + 1024 {
            return None;
        }
        let changed: Vec<NodeId> = (0..keys.len()).filter(|&n| keys[n] != self.built_keys[n]).map(|n| n as NodeId).collect();
        let set: korvus_dom::FxHashSet<NodeId> = changed.iter().copied().collect();
        // `display: none` -- not merely no box: a `display: contents` element has none either, but the builder reads
        // its style for its own text runs (`collect_inline`) and a flex or grid parent's anonymous items. Skipped as
        // box-less, its font write kept the old lines, and `none` <-> `contents` kept its text hidden or shown, until
        // the next build (2026-10-23).
        let none = |k: Option<StyleKey>| k.is_none_or(|k| arena.value(k, PropertyId::Display) == "none");
        let mut roots: Vec<(NodeId, u32)> = Vec::new();
        for &e in &changed {
            let mut up = dom.parent(e);
            let mut covered = false;
            while let Some(a) = up {
                if set.contains(&a) && self.box_of(a).is_some() {
                    covered = true;
                    break;
                }
                up = dom.parent(a);
            }
            if covered {
                continue;
            }
            match self.box_of(e) {
                Some(b) => roots.push((e, b)),
                // `none` before and now: nothing of the tree is its
                None if none(keys[e as usize]) && none(self.built_keys[e as usize]) => {}
                None => return None,
            }
        }
        let mut installs: Vec<(usize, u32, u64)> = Vec::new();
        for (e, b) in roots {
            self.patch_group(dom, arena, keys, e, b, &mut installs)?;
        }
        let mut moved = false;
        let mut simple = true;
        for &(i, _, origin) in &installs {
            moved |= self.built[i] != origin;
            simple &= self.boxes[i].style == self.built_style[i];
        }
        for &(i, style, origin) in &installs {
            self.built[i] = origin;
            self.built_style[i] = style;
            if simple && !moved {
                self.boxes[i].style = style;
            }
        }
        self.built_keys.clear();
        self.built_keys.extend_from_slice(keys);
        // the carry hashes take the built origins
        self.carry_h = std::cell::OnceCell::new();
        Some(Patched { moved, simple })
    }

    /// Build the group of element `e` (box `b`) again and compare it with this tree's: see [`BoxTree::patch_styles`].
    /// Appends `(box, style, origin)` per box of the group to `installs`.
    fn patch_group(&mut self, dom: &Dom, arena: &StyleArena, keys: &[Option<StyleKey>], e: NodeId, b: u32, installs: &mut Vec<(usize, u32, u64)>) -> Option<()> {
        let bx = &self.boxes[b as usize];
        if b == 0 || bx.static_flow.is_some() || Some(e) == self.body_node || bx.level != BoxLevel::Block || self.cv_auto.iter().any(|&(i, _)| i == b) {
            return None;
        }
        let p = bx.parent?;
        let dp = dom.parent(e)?;
        if p == 0 || self.box_of(dp) != Some(p) || self.boxes[p as usize].node != dp || self.boxes[p as usize].natural.is_some() {
            return None;
        }
        let parent_style = self.built_style[p as usize];
        let display = self.styles.get(parent_style).display;
        let items = matches!(display, Display::Flex | Display::Grid) && !self.styles.legacy_flex(parent_style);
        if !items && (!matches!(display, Display::Block | Display::FlowRoot) || self.styles.legacy_flex(parent_style)) {
            return None;
        }
        let table = |s: u32| self.styles.table(s).role != crate::layout::style::TableRole::None;
        if table(parent_style) || table(self.built_style[b as usize]) || closed_details_content(dom, dp, e) || self.image_fallback(dom, dp) {
            return None;
        }
        // The scratch tree: a root standing in for this one's (so no rule for the root or `<body>` applies), the
        // parent's box under it as built, and the group added there as its parent's own walk adds it.
        let mut scratch = BoxTree {
            box_of_node: vec![None; dom.len()],
            generated: std::mem::take(&mut self.generated),
            natural_images: std::mem::take(&mut self.natural_images),
            mode: self.mode,
            body_node: self.body_node,
            flat: std::mem::take(&mut self.flat),
            styles: std::mem::take(&mut self.styles),
            fonts: std::mem::replace(&mut self.fonts, Box::new(BuiltinFonts)),
            ..Default::default()
        };
        scratch.boxes.push(LayoutBox::new(self.boxes[0].node, self.built_style[0], None, BoxLevel::Block));
        scratch.boxes.push(LayoutBox::new(dp, parent_style, Some(0), self.boxes[p as usize].level));
        let mut pending: Vec<Vec<u32>> = vec![Vec::new(), Vec::new()];
        let built = scratch.add(dom, arena, &|n| keys.get(n as usize).copied().flatten(), e, Some(1), Some(0), &mut pending);
        if items {
            // a flex or grid item is blockified (`add_items`)
            if let Some(nb) = built {
                scratch.boxes[nb as usize].level = BoxLevel::Block;
            }
        }
        self.generated = std::mem::take(&mut scratch.generated);
        self.natural_images = std::mem::take(&mut scratch.natural_images);
        self.flat = std::mem::take(&mut scratch.flat);
        self.styles = std::mem::take(&mut scratch.styles);
        self.fonts = std::mem::replace(&mut scratch.fonts, Box::new(BuiltinFonts));
        if built != Some(2) || !pending[0].is_empty() || pending[1] != [2] || !scratch.cv_auto.is_empty() {
            return None;
        }
        let count = scratch.boxes.len() - 2;
        let base = b as usize;
        if base + count > self.boxes.len() || self.cv_auto.iter().any(|&(i, _)| (i as usize) >= base && (i as usize) < base + count) {
            return None;
        }
        // scratch box -> this tree's
        let map = |s: u32| -> Option<u32> { (s >= 2).then(|| s - 2 + b) };
        let mut kids: Vec<Vec<u32>> = pending;
        let natural = |n: &Option<Box<Natural>>| n.as_ref().map(|n| (n.width.map(f32::to_bits), n.height.map(f32::to_bits), n.ratio.map(f32::to_bits)));
        for k in 0..count {
            let (sb, ob) = (&scratch.boxes[2 + k], &self.boxes[base + k]);
            let parent_ok = if k == 0 { sb.parent == Some(1) && ob.parent == Some(p) } else { sb.parent.and_then(map) == ob.parent && sb.parent.is_some() };
            if !parent_ok
                || sb.node != ob.node
                || sb.text != ob.text
                || sb.level != ob.level
                || natural(&sb.natural) != natural(&ob.natural)
                || sb.text_overflow_width.to_bits() != ob.text_overflow_width.to_bits()
                || sb.scrolls_own_text != ob.scrolls_own_text
                || sb.built_facts() != ob.built_facts()
                || sb.static_flow.is_some()
                || ob.static_flow.is_some()
            {
                return None;
            }
            let (os, oe) = ob.children;
            let own: &[u32] = &self.child_list[os as usize..oe as usize];
            let theirs = std::mem::take(&mut kids[2 + k]);
            if own.len() != theirs.len() || theirs.iter().zip(own).any(|(&c, &o)| map(c) != Some(o)) {
                return None;
            }
            match (&sb.ifc, &ob.ifc) {
                (None, None) => {}
                (Some(x), Some(y)) => {
                    if x.len() != y.len()
                        || x.iter().zip(y.iter()).any(|(p, q)| std::mem::discriminant(p) != std::mem::discriminant(q) || map(p.box_index()) != Some(q.box_index()))
                    {
                        return None;
                    }
                }
                _ => return None,
            }
            // what names the box: the element-to-box map, a text box's text node, a generated box's element
            let sk = (2 + k) as u32;
            let (is_own_s, is_own_o) = (scratch.box_of_node[sb.node as usize] == Some(sk), self.box_of_node[ob.node as usize] == Some(base as u32 + k as u32));
            if is_own_s != is_own_o || scratch.text_nodes.get(&sk) != self.text_nodes.get(&(base as u32 + k as u32)) {
                return None;
            }
        }
        for (&(node, after), &sbox) in &scratch.pseudo_boxes {
            if self.pseudo_boxes.get(&(node, after)).copied() != map(sbox) {
                return None;
            }
        }
        for k in 0..count {
            let style = scratch.boxes[2 + k].style;
            installs.push((base + k, style, self.styles.origin(style)));
        }
        Some(())
    }

    /// Before a patched tree is computed again: every box back to its built style, and each pass's state carried
    /// from the tree's own last compute as [`BoxTree::carry_from`] carries it from another tree -- the full-window
    /// state to start the first pass, the final one for the final pass, each checked against the style it was
    /// computed under before it answers (`sync_origins`, `enter_final_pass`), no cache for a box sized off its
    /// container's height.
    pub(crate) fn prepare_recompute(&mut self) {
        for (b, &style) in self.boxes.iter_mut().zip(&self.built_style) {
            b.style = style;
        }
        let n = self.boxes.len();
        if self.live_final && knob(Knob::TwoStates) {
            std::mem::swap(&mut self.st, &mut self.alt);
            std::mem::swap(&mut self.maps, &mut self.alt_maps);
            for s in &mut self.alt {
                s.carried = true;
            }
        } else if !knob(Knob::TwoStates) || self.alt.len() != n {
            self.alt = Vec::new();
            self.alt_maps = PassMaps::default();
        }
        self.live_final = false;
        self.relaid = true;
        // What a carry does not carry starts from nothing, as after a build: carried from the tree's own last layout
        // instead, a shrink-to-fit fixed box around a percentage padding -- whose padding passes do not settle, the
        // padding growing the box it is a percentage of -- went one pass further on each relayout than from nothing
        // (196.14 wide for 195.95), and a `content-visibility: auto` box kept a padded style resolved while it was
        // skipped (118px tall for 46; 2026-10-23).
        let uncarried = self.uncarried();
        if uncarried.iter().any(|&u| u) {
            for i in 0..n {
                if uncarried[i] {
                    self.st[i] = BoxState { origin: self.built[i], ..BoxState::default() };
                    if let Some(s) = self.alt.get_mut(i) {
                        *s = BoxState::default();
                    }
                    for maps in [&mut self.maps, &mut self.alt_maps] {
                        maps.last_baselines.remove(&(i as u32));
                        maps.text_lines.remove(&(i as u32));
                    }
                } else {
                    // an inline box's fragments in an IFC laid out from nothing are written there again
                    for states in [&mut self.st, &mut self.alt] {
                        if let Some(s) = states.get_mut(i) {
                            s.fragments.retain(|(ifc, _)| !uncarried[*ifc as usize]);
                        }
                    }
                }
            }
        }
        for i in 0..n {
            let follows_height = block_percent(self.styles.get(self.boxes[i].style));
            for states in [&mut self.st, &mut self.alt] {
                if let Some(s) = states.get_mut(i) {
                    s.inherited = true;
                    if follows_height {
                        s.cache = BoxCache::default();
                    }
                }
            }
            self.st[i].carried = false;
        }
    }

    /// The boxes [`BoxTree::carry_from`] never carries, even into a tree built as this one: every box of the initial
    /// containing block's subtrees besides `<html>`'s -- `pair_with` pairs from `<html>` down -- and each
    /// `content-visibility: auto` box with its ancestors, laid out where they land.
    fn uncarried(&self) -> Vec<bool> {
        let n = self.boxes.len();
        let mut out = vec![false; n];
        for &(index, _) in &self.cv_auto {
            let mut at = Some(index);
            while let Some(b) = at {
                if std::mem::replace(&mut out[b as usize], true) {
                    break;
                }
                at = self.boxes[b as usize].parent;
            }
        }
        let (s, e) = self.icb_children();
        let mut stack: Vec<u32> = (s..e).map(|k| self.child_list[k as usize]).collect();
        let mut seen = vec![false; if stack.is_empty() { 0 } else { n }];
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut seen[i as usize], true) {
                continue;
            }
            out[i as usize] = true;
            stack.extend(self.laid_out_by(i));
        }
        out
    }

    /// THE QUIRKY MARGIN QUIRK: in a quirks-mode document the first in-flow child of `<body>` or of a
    /// table cell loses a UA `__qem` top margin. Recognised as an element Blink gives one whose used
    /// margin-top is still that UA value (an author margin of the same size is indistinguishable,
    /// and rare).
    fn drop_quirky_margins(&mut self, dom: &Dom, arena: &StyleArena, style_of: &dyn Fn(NodeId) -> Option<StyleKey>) {
        let is = |n: NodeId, tags: &[&str]| dom.is_html_ns(n) && dom.tag_name(n).is_some_and(|t| tags.iter().any(|x| t.eq_ignore_ascii_case(x)));
        for index in 0..self.boxes.len() as u32 {
            let b = &self.boxes[index as usize];
            if b.text.is_some() || b.level != BoxLevel::Block || !is(b.node, &["body", "td", "th"]) {
                continue;
            }
            if self.box_of_node.get(b.node as usize).copied().flatten() != Some(index) {
                continue;
            }
            let (start, end) = b.children;
            let first = self.child_list[start as usize..end as usize].iter().copied().find(|&c| {
                let cb = &self.boxes[c as usize];
                cb.level == BoxLevel::Block && !matches!(self.styles.position(cb.style), CssPosition::Absolute | CssPosition::Fixed)
            });
            let Some(first) = first else { continue };
            let node = self.boxes[first as usize].node;
            // An anonymous box around inline content carries its block's node; it has no margin.
            if self.box_of_node.get(node as usize).copied().flatten() != Some(first) {
                continue;
            }
            let Some(factor) = quirky_margin_top_factor(dom, node) else { continue };
            let Some(key) = style_of(node) else { continue };
            let font = crate::style::resolve::px_of(arena.value(key, PropertyId::FontSize)).unwrap_or(16.0);
            let top = crate::style::resolve::px_of(arena.value(key, PropertyId::MarginTop));
            if top.is_some_and(|t| (t - factor * font).abs() < 0.01) {
                let style = self.boxes[first as usize].style;
                self.boxes[first as usize].style = self.styles.intern_variant(style, Variant::NoMarginTop);
                self.quirk_margin_boxes.insert(first);
            }
        }
    }

    /// Add a box for `node` and, recursively, for its children. Returns the new box, or `None` if
    /// this element generates none — in which case its subtree generates none either, which is what
    /// `display: none` means.
    ///
    /// `parent` is the box the element's *parent element* generated; `containing` is the nearest
    /// ancestor box that establishes a containing block for absolutely positioned descendants. They
    /// are usually the same box and, for an absolutely positioned element, deliberately are not:
    /// CSS 2.1 §10.1 puts such a box in the padding box of its nearest **positioned** ancestor, and
    /// taffy lays an absolute child out against whichever box it is a child of. So the box tree —
    /// not the DOM tree — is where that rule is expressed.
    fn add(
        &mut self,
        dom: &Dom,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        node: NodeId,
        parent: Option<u32>,
        containing: Option<u32>,
        pending: &mut Vec<Vec<u32>>,
    ) -> Option<u32> {
        let key = style_of(node)?;
        if !generates_box(arena.value(key, PropertyId::Display)) || renders_nothing(dom, node) {
            return None;
        }
        let mut style = self.element_style(dom, arena, node, key);
        let natural = self.natural_size(dom, node, style);
        let theme_part = self.is_theme_part(dom, node, style);
        if theme_part {
            style = self.styles.intern_variant(style, Variant::ThemePart);
        }
        if natural.is_none() && self.image_fallback(dom, node) {
            style = self.fallback_host_style(style);
        }
        if natural.is_some() {
            style = self.styles.intern_variant(style, Variant::Replaced);
            // A replaced GRID item keeps its natural size under `normal` alignment.
            if let Some(p) = parent {
                let container = self.styles.get(self.boxes[p as usize].style);
                if container.display == taffy::style::Display::Grid {
                    let item = self.styles.get(style);
                    let justify = item.justify_self.is_none() && container.justify_items.is_none();
                    let align = item.align_self.is_none() && container.align_items.is_none();
                    if justify || align {
                        style = self.styles.intern_variant(style, Variant::ReplacedGridItem { justify, align });
                    }
                }
            }
        } else if parent.is_some_and(|p| self.shrinks_to_fit_in(dom, node, style, p)) {
            style = self.styles.intern_variant(style, Variant::ShrinkToFit);
        }
        // An item of a legacy `-webkit-box` flexbox takes its `-webkit-box-flex`.
        if let Some(p) = parent.filter(|p| self.styles.legacy_flex(self.boxes[*p as usize].style)) {
            let variant = self.styles.webkit_box_item(self.boxes[p as usize].style);
            style = self.styles.intern_variant(style, variant);
        }
        if parent.is_none() {
            // The root's `overflow` is the viewport's (CSS Overflow 3 §3.3), and so is its
            // `scrollbar-width`. Recorded from the root now; a propagated `<body>` replaces it below.
            self.record_viewport_scroll(style);
            self.viewport_bar = self.styles.get(style).scrollbar_width;
            style = self.styles.intern_variant(style, Variant::ViewportRoot);
        }
        if parent == Some(0) && self.overflow_goes_to_the_viewport(dom, node, style) {
            self.record_viewport_scroll(style);
            style = self.styles.intern_variant(style, Variant::OverflowVisible);
        }
        // `content-visibility` skips the contents of a box that size containment applies
        // to -- not a non-atomic inline, where it is inert (as `innerText` already treats it).
        if self.styles.level(style) != BoxLevel::Inline {
            match self.styles.content_visibility(style) {
                crate::layout::style::ContentVisibility::Hidden => {
                    style = self.styles.intern_variant(style, Variant::Skipped);
                }
                crate::layout::style::ContentVisibility::Auto => {
                    let relevant = style;
                    style = self.styles.intern_variant(style, Variant::Skipped);
                    self.cv_auto.push((self.boxes.len() as u32, relevant));
                }
                crate::layout::style::ContentVisibility::Visible => {}
            }
        }
        let position = self.styles.position(style);
        let index = self.boxes.len() as u32;
        // The box this one is laid out inside. For everything in flow that is its parent; for an
        // absolutely positioned box it is the nearest positioned ancestor, and for a fixed one the
        // root box, which is this engine's stand-in for the initial containing block (it is at the
        // viewport origin and as wide as the viewport). `parent.is_some()` guards the root itself:
        // the outermost box has no container whatever it is positioned as.
        let layout_parent = match position {
            _ if parent.is_none() => None,
            CssPosition::Absolute => containing.or(Some(0)),
            CssPosition::Fixed => Some(0),
            _ => parent,
        };
        let mut b = LayoutBox::new(node, style, layout_parent, self.styles.level(style));
        if let (Some(p), CssPosition::Absolute | CssPosition::Fixed) = (parent, position) {
            let prev = pending[p as usize].iter().rev().copied().find(|&c| {
                !matches!(self.styles.position(self.boxes[c as usize].style), CssPosition::Absolute | CssPosition::Fixed)
            });
            b.static_flow = Some((p, prev));
        }
        b.text_overflow_width = self.input_value_width(dom, node, style);
        b.scrolls_own_text = self.is_text_entry_control(dom, node);
        let lays_out_children = natural.is_none() || lays_out_its_value(dom, node);
        b.natural = natural.map(Box::new);
        b.theme_part = theme_part;
        b.text_control = natural.is_some() && is_text_control(dom, node);
        b.button = dom.is_html_ns(node) && dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("button"));
        b.spans = cell_spans(dom, node);
        self.boxes.push(b);
        pending.push(Vec::new());
        if let Some(p) = layout_parent {
            pending[p as usize].push(index);
        }
        self.box_of_node[node as usize] = Some(index);

        // A positioned box is the containing block for its own absolutely positioned descendants;
        // a static one passes the question through to whatever answered it here.
        let containing = if position.is_positioned() { Some(index) } else { containing };
        if lays_out_children {
            self.add_children(dom, arena, style_of, node, index, containing, pending);
        }
        Some(index)
    }

    /// Give `block`'s children boxes, splitting them into block-level children and **anonymous
    /// boxes over runs of inline content** — CSS 2.1 §9.2.1.1.
    ///
    /// The split is what makes `<div>text<p>para</p>more</div>` three children and not three
    /// stacked text runs: the two text runs become two anonymous block boxes, each establishing its
    /// own inline formatting context, with the `<p>` between them. Chrome measures that document at
    /// 18 + 10 + 18 = 46px tall, and its `<p>` at y = 18.
    ///
    /// A **flex or grid container blockifies its children**, so it takes the old path unchanged: no
    /// lines, no anonymous IFC, every child a taffy child. Chrome agrees —
    /// `getComputedStyle` on a `<span>` inside `display: flex` answers `block`.
    fn add_children(
        &mut self,
        dom: &Dom,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        node: NodeId,
        block: u32,
        containing: Option<u32>,
        pending: &mut Vec<Vec<u32>>,
    ) {
        // A broken `<img>` that is not replaced lays out its fallback content, whatever its `display`
        // (Blink makes the host a `LayoutBlockFlow` either way).
        if self.boxes[block as usize].natural.is_none() && self.image_fallback(dom, node) {
            self.add_image_fallback(dom, node, block, pending);
            return;
        }
        let style = self.boxes[block as usize].style;
        // A legacy `-webkit-box` flexbox does not blockify its children (Chromium 152): its block-level
        // children are its items and each run of inline content is ONE anonymous item, an inline formatting
        // context of its own (Blink's `LayoutFlexibleBox` wraps inline children in an anonymous block) --
        // which is the block container's split below, its anonymous boxes taking the item variant.
        // Chrome 152, `tools/realweb/seed/webkitbox` / `wboxitems`: two `<span>`s in a horizontal box share
        // one line ("onetwo"), and a 232px run in a 100px box wraps on three lines of 72px.
        // a control laying out its own value (a `<textarea>`'s) lays it out as a block whatever its `display`: Chrome's
        // inner editor is a block inside the control's box (Google's search box is a `display: flex` textarea, and its
        // query painted nothing here)
        let control = self.boxes[block as usize].natural.is_some();
        if matches!(self.styles.get(style).display, Display::Flex | Display::Grid) && !self.styles.legacy_flex(style) && !control {
            self.add_items(dom, arena, style_of, node, style, block, containing, pending);
            return;
        }
        let mut run = InlineRun::default();
        self.collect_inline(dom, arena, style_of, node, block, containing, pending, &mut run);
        self.flush_run(&mut run);
    }

    /// The ITEMS of flex or grid container `block`: `node`'s children, blockified -- and in place of a
    /// `display: contents` child, ITS children and generated boxes, which is what "no box of its own"
    /// means (CSS Display 3 §2.5, Chrome 150 lays `<ul style=display:flex><li
    /// style=display:contents><a style=display:block>` out with the `<a>` as the flex item; this engine
    /// dropped the whole subtree). `style` is the style `node`'s own text runs take.
    #[allow(clippy::too_many_arguments)]
    fn add_items(
        &mut self,
        dom: &Dom,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        node: NodeId,
        style: u32,
        block: u32,
        containing: Option<u32>,
        pending: &mut Vec<Vec<u32>>,
    ) {
        if let Some(pb) = self.generated.get(&node).and_then(|g| g.before.clone()) {
            self.add_pseudo_item(node, false, &pb, arena, block, pending);
        }
        for child in self.flat.children(dom, node) {
            if closed_details_content(dom, node, child) {
                continue;
            }
            match &dom.node(child).kind {
                NodeKind::Element { .. } => {
                    if is_contents(dom, arena, style_of, child) {
                        if let Some(own) = self.styles_of_element(style_of, arena, child) {
                            self.add_items(dom, arena, style_of, child, own, block, containing, pending);
                        }
                        continue;
                    }
                    let b = self.add(dom, arena, style_of, child, Some(block), containing, pending);
                    // A flex or grid ITEM is blockified: CSS Display 3 §2.7, and Chrome agrees
                    // — `getComputedStyle(span).display` inside `display: flex` answers
                    // `block`. Without this a `<span>` item would still be looking for a line.
                    if let Some(b) = b {
                        self.boxes[b as usize].level = BoxLevel::Block;
                    }
                }
                NodeKind::Text(text) => {
                    let spec = self.styles.font(style);
                    if let Some(collapsed) = inline::collapse(text, spec.preserve) {
                        // An ANONYMOUS item (CSS Flexbox §4, Grid §6): it inherits the container's
                        // font but none of its box -- with the container's own style its padding,
                        // border and size were applied a second time. Chrome 150 (`L-flex-text-item`):
                        // a padded flex link with one 24px line is 56 high, korvus made it 88, and
                        // europa.eu's ECL menu then widened each link a pixel at a time to 1000px --
                        // ~6,000 forced layouts, a 13 s freeze.
                        //
                        // The item is an anonymous BLOCK box whose text is laid out in lines by its own inline
                        // formatting context, as a block container's is (CSS Flexbox §4: an anonymous flex item
                        // wraps the run). A text box that was itself the item was measured as a leaf and had no
                        // line fragments: the text of every flex or grid container -- a `<button
                        // style="display:flex">`'s label, GitHub's menu, Wikipedia's search button -- was laid out
                        // and never drawn.
                        let anonymous = self.styles.intern_anonymous(style);
                        let item = self.boxes.len() as u32;
                        self.boxes.push(LayoutBox::new(node, anonymous, Some(block), BoxLevel::Block));
                        pending.push(Vec::new());
                        pending[block as usize].push(item);
                        let mut run = InlineRun { ifc: Some(item), ..Default::default() };
                        let b = self.add_text(node, style, collapsed, Some(item), pending, false);
                        self.text_nodes.insert(b, child);
                        run.items.push(Item::Text(b));
                        self.flush_run(&mut run);
                    }
                }
                _ => {}
            }
        }
        if let Some(pb) = self.generated.get(&node).and_then(|g| g.after.clone()) {
            self.add_pseudo_item(node, true, &pb, arena, block, pending);
        }
    }

    /// Walk one element's children into `run`, recursing **through** inline boxes so that a
    /// block-level element buried inside a `<span>` still interrupts the run.
    ///
    /// That interruption is not an edge case: `<a href="…"><div>…</div></a>` is how every card link
    /// on the modern web is written, and Chrome splits the `<a>` into a fragment above the block, a
    /// full-width block, and a fragment below — three rects from one `<a>`, measured.
    #[allow(clippy::too_many_arguments)]
    fn collect_inline(
        &mut self,
        dom: &Dom,
        arena: &StyleArena,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        node: NodeId,
        block: u32,
        containing: Option<u32>,
        pending: &mut Vec<Vec<u32>>,
        run: &mut InlineRun,
    ) {
        let slot_pass = self.details_slot == Some(node);
        // An `inside` list marker starts the list item's first line, before its `::before`.
        if !slot_pass && self.boxes[block as usize].node == node {
            let base = self.boxes[block as usize].style;
            if let Some(width) = self.inside_marker_width(arena, style_of, node, base) {
                let style = self.styles.intern_variant(base, Variant::InsideMarker { width });
                let ifc = self.ensure_ifc(block, pending, run);
                // the marker is the element's, but it is not the element's box
                let own = self.box_of_node[node as usize];
                let b = self.add_inline_box(dom, node, style, BoxLevel::AtomicInline, ifc, pending);
                self.box_of_node[node as usize] = own;
                pending[ifc as usize].push(b);
                run.items.push(Item::Atomic(b));
            }
        }
        if !slot_pass {
            if let Some(pb) = self.generated.get(&node).and_then(|g| g.before.clone()) {
                self.add_pseudo_inline(node, false, &pb, arena, block, containing, pending, run);
            }
        }
        let mut slotted = false;
        // A `<details>` with `display: contents` has no box to hold a `::details-content`: Chrome 150 lays its
        // closed content out like any other child (measured).
        let contents = is_contents(dom, arena, style_of, node);
        for child in self.flat.children(dom, node) {
            // The slot pass takes exactly the closed content; the ordinary pass everything else.
            if (closed_details_content(dom, node, child) && !contents) != slot_pass {
                slotted |= !slot_pass;
                continue;
            }
            if renders_nothing(dom, child) {
                continue;
            }
            match &dom.node(child).kind {
                NodeKind::Text(text) => {
                    let parent_style = self.styles_of_element(style_of, arena, node);
                    let Some(style) = parent_style else { continue };
                    let spec = self.styles.font(style);
                    // Collapsible white space alone, before any inline content of the run -- the newline between
                    // two blocks -- is removed at its line's start (CSS Text 3 §4.1.2), and Chrome makes no layout
                    // object for it. An anonymous box over it held its siblings' margins apart: `<body>\n<p>`
                    // put the `<p>` 20px lower than Chrome, and every `<p>` after it 20px more.
                    if run.ifc.is_none() && !spec.preserve && text.chars().all(inline::is_collapsible) {
                        continue;
                    }
                    let Some(collapsed) = inline::collapse_inline(text, spec.preserve) else {
                        continue;
                    };
                    let ifc = self.ensure_ifc(block, pending, run);
                    let b = self.add_text(node, style, collapsed, Some(ifc), pending, false);
                    self.text_nodes.insert(b, child);
                    run.items.push(Item::Text(b));
                }
                NodeKind::Element { .. } => {
                    let Some(key) = style_of(child) else { continue };
                    let display = arena.value(key, PropertyId::Display);
                    if display == "contents" {
                        // No box of its own (CSS Display 3 §2.5): its children and its
                        // `::before`/`::after` join this run as if they were `node`'s -- a block among
                        // them breaks it, as one written here would.
                        if !renders_nothing(dom, child) {
                            self.collect_inline(dom, arena, style_of, child, block, containing, pending, run);
                        }
                        continue;
                    }
                    if !generates_box(display) || renders_nothing(dom, child) {
                        continue;
                    }
                    let mut style = self.element_style(dom, arena, child, key);
                    let natural = self.natural_size(dom, child, style);
                    let theme_part = self.is_theme_part(dom, child, style);
                    if theme_part {
                        style = self.styles.intern_variant(style, Variant::ThemePart);
                    }
                    if natural.is_some() {
                        style = self.styles.intern_variant(style, Variant::Replaced);
                    } else if self.image_fallback(dom, child) {
                        style = self.fallback_host_style(style);
                    }
                    let level = self.styles.level(style);
                    // Out of flow: an absolutely positioned box is blockified and attached to its
                    // containing block, and it does NOT break the line it was written on.
                    if matches!(
                        self.styles.position(style),
                        CssPosition::Absolute | CssPosition::Fixed
                    ) {
                        let b =
                            self.add(dom, arena, style_of, child, Some(block), containing, pending);
                        // An absolutely positioned box is blockified too (CSS Display 3 §2.7), and
                        // it does NOT break the line it was written on: it is out of flow.
                        if let Some(b) = b {
                            self.boxes[b as usize].level = BoxLevel::Block;
                            // written before anything in flow on its line: placed at the line's top
                            let content = run.items.iter().any(|i| matches!(i, Item::Text(_) | Item::Atomic(_) | Item::Break(_)));
                            self.boxes[b as usize].static_line_start = run.ifc.is_some() && !content;
                        }
                        continue;
                    }
                    // A float is out of the flow too, and does not break its line either: a block child of the
                    // container, placed by the block layout, the lines around it shortened (`layout_ifc`).
                    if self.styles.get(style).float != Float::None {
                        if let Some(b) = self.add(dom, arena, style_of, child, Some(block), containing, pending) {
                            self.hoist_float(block, b, run, pending);
                        }
                        continue;
                    }
                    match level {
                        BoxLevel::Block => {
                            // The run ends here, and resumes after this box in a second anonymous
                            // box. `flush_run` clears `run.ifc`, so the next inline item makes one.
                            self.flush_run(run);
                            self.add(dom, arena, style_of, child, Some(block), containing, pending);
                        }
                        BoxLevel::AtomicInline => {
                            let ifc = self.ensure_ifc(block, pending, run);
                            let b = self.add_inline_box(dom, child, style, level, ifc, pending);
                            pending[ifc as usize].push(b);
                            run.items.push(Item::Atomic(b));
                            // An atomic inline is a formatting context of its own.
                            let inner = if self.styles.position(style).is_positioned() {
                                Some(b)
                            } else {
                                containing
                            };
                            // A replaced element's children are not rendered: a `<canvas>`'s
                            // fallback content, a `<video>`'s `<source>`s, an `<svg>`'s shapes
                            // (which are SVG layout, not CSS boxes) and a `<select>`'s options.
                            let lays_out_children = natural.is_none() || lays_out_its_value(dom, child);
                            self.boxes[b as usize].natural = natural.map(Box::new);
                            self.boxes[b as usize].theme_part = theme_part;
                            self.boxes[b as usize].text_control = natural.is_some() && is_text_control(dom, child);
                            self.boxes[b as usize].button = dom.is_html_ns(child) && dom.tag_name(child).is_some_and(|t| t.eq_ignore_ascii_case("button"));
                            if lays_out_children {
                                self.add_children(dom, arena, style_of, child, b, inner, pending);
                            }
                        }
                        BoxLevel::Inline => {
                            let ifc = self.ensure_ifc(block, pending, run);
                            let b = self.add_inline_box(dom, child, style, level, ifc, pending);
                            if dom.tag_name(child).is_some_and(|t| t.eq_ignore_ascii_case("br")) {
                                run.items.push(Item::Break(b));
                                continue;
                            }
                            run.items.push(Item::Open(b));
                            run.open.push(b);
                            let inner = if self.styles.position(style).is_positioned() {
                                Some(b)
                            } else {
                                containing
                            };
                            self.collect_inline(
                                dom, arena, style_of, child, block, inner, pending, run,
                            );
                            run.open.pop();
                            // A block-level descendant flushed the run and `flush_run` already
                            // closed this box on it; the run it left behind has no IFC until
                            // something else needs one, and a `Close` there would be unbalanced.
                            if run.ifc.is_some() {
                                run.items.push(Item::Close(b));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if slot_pass {
            return;
        }
        if slotted {
            // `::details-content`: a block after the summary that the content is laid out in and
            // that takes no space (size containment under `content-visibility: hidden`).
            self.flush_run(run);
            let base = self.boxes[block as usize].style;
            let anonymous = self.styles.intern_anonymous(base);
            let style = self.styles.intern_variant(anonymous, Variant::DetailsSlot);
            let slot = self.boxes.len() as u32;
            self.boxes.push(LayoutBox::new(node, style, Some(block), BoxLevel::Block));
            pending.push(Vec::new());
            pending[block as usize].push(slot);
            self.details_slot = Some(node);
            let mut slot_run = InlineRun::default();
            self.collect_inline(dom, arena, style_of, node, slot, containing, pending, &mut slot_run);
            self.flush_run(&mut slot_run);
            self.details_slot = None;
        }
        if let Some(pb) = self.generated.get(&node).and_then(|g| g.after.clone()) {
            self.add_pseudo_inline(node, true, &pb, arena, block, containing, pending, run);
        }
    }

    /// A generated box as a flex or grid ITEM: blockified, a taffy child of `block`, its text in an
    /// inline formatting context of its own.
    fn add_pseudo_item(&mut self, node: NodeId, after: bool, pb: &PseudoBox, arena: &StyleArena, block: u32, pending: &mut Vec<Vec<u32>>) {
        if !generates_box(arena.value(pb.key, PropertyId::Display)) {
            return;
        }
        let style = self.styles.intern(arena, pb.key);
        let index = self.boxes.len() as u32;
        self.boxes.push(LayoutBox::new(node, style, Some(block), BoxLevel::Block));
        pending.push(Vec::new());
        pending[block as usize].push(index);
        self.pseudo_boxes.insert((node, after), index);
        self.fill_with_text(node, index, style, &pb.text, pending);
    }

    /// A generated box in normal flow: a block interrupts the run, an inline-block is an atomic
    /// inline, an inline is a box open around its text, and an absolutely positioned one is attached
    /// to its containing block -- the same four routes an element takes.
    #[allow(clippy::too_many_arguments)]
    fn add_pseudo_inline(
        &mut self,
        node: NodeId,
        after: bool,
        pb: &PseudoBox,
        arena: &StyleArena,
        block: u32,
        containing: Option<u32>,
        pending: &mut Vec<Vec<u32>>,
        run: &mut InlineRun,
    ) {
        if !generates_box(arena.value(pb.key, PropertyId::Display)) {
            return;
        }
        let style = self.styles.intern(arena, pb.key);
        let level = self.styles.level(style);
        let position = self.styles.position(style);
        if matches!(position, CssPosition::Absolute | CssPosition::Fixed) {
            let parent = if position == CssPosition::Fixed { Some(0) } else { containing.or(Some(0)) };
            let index = self.boxes.len() as u32;
            self.boxes.push(LayoutBox::new(node, style, parent, BoxLevel::Block));
            pending.push(Vec::new());
            if let Some(p) = parent {
                pending[p as usize].push(index);
            }
            self.pseudo_boxes.insert((node, after), index);
            self.fill_with_text(node, index, style, &pb.text, pending);
            return;
        }
        match level {
            BoxLevel::Block => {
                // a floated `::before` (an icon) leaves its line whole, as a floated element does
                let floated = self.styles.get(style).float != Float::None;
                if !floated {
                    self.flush_run(run);
                }
                let index = self.boxes.len() as u32;
                self.boxes.push(LayoutBox::new(node, style, Some(block), BoxLevel::Block));
                pending.push(Vec::new());
                pending[block as usize].push(index);
                if floated {
                    self.hoist_float(block, index, run, pending);
                }
                self.pseudo_boxes.insert((node, after), index);
                self.fill_with_text(node, index, style, &pb.text, pending);
            }
            BoxLevel::AtomicInline => {
                let ifc = self.ensure_ifc(block, pending, run);
                let index = self.boxes.len() as u32;
                self.boxes.push(LayoutBox::new(node, style, Some(ifc), level));
                pending.push(Vec::new());
                pending[ifc as usize].push(index);
                run.items.push(Item::Atomic(index));
                self.pseudo_boxes.insert((node, after), index);
                self.fill_with_text(node, index, style, &pb.text, pending);
            }
            BoxLevel::Inline => {
                let spec = self.styles.font(style);
                let collapsed = inline::collapse_inline(&pb.text, spec.preserve);
                let ifc = self.ensure_ifc(block, pending, run);
                let index = self.boxes.len() as u32;
                self.boxes.push(LayoutBox::new(node, style, Some(ifc), level));
                pending.push(Vec::new());
                self.pseudo_boxes.insert((node, after), index);
                run.items.push(Item::Open(index));
                if let Some(text) = collapsed {
                    let t = self.add_text(node, style, text, Some(ifc), pending, false);
                    run.items.push(Item::Text(t));
                }
                run.items.push(Item::Close(index));
            }
        }
    }

    /// Give a generated block-container box its text, in an inline formatting context of its own.
    fn fill_with_text(&mut self, node: NodeId, index: u32, style: u32, text: &str, pending: &mut Vec<Vec<u32>>) {
        let spec = self.styles.font(style);
        let Some(collapsed) = inline::collapse_inline(text, spec.preserve) else { return };
        let mut run = InlineRun::default();
        let ifc = self.ensure_ifc(index, pending, &mut run);
        let t = self.add_text(node, style, collapsed, Some(ifc), pending, false);
        run.items.push(Item::Text(t));
        self.flush_run(&mut run);
    }

    /// Was this box's top margin dropped by the quirky-margin quirk?
    pub(crate) fn quirk_margin_dropped(&self, index: u32) -> bool {
        self.quirk_margin_boxes.contains(&index)
    }

    /// Record an inline formatting context's last baseline.
    pub(super) fn set_last_baseline(&mut self, index: u32, baseline: Option<f32>) {
        match baseline {
            Some(b) => {
                self.maps.last_baselines.insert(index, b);
            }
            None => {
                self.maps.last_baselines.remove(&index);
            }
        }
    }

    /// The baseline of an ATOMIC inline, from its border-box top: its last line's, when it is an
    /// `overflow: visible` block container with lines in flow (CSS 2.1 §10.8.1). `None` answers the
    /// bottom margin edge, which is right for a replaced element, an empty box and a scroll container.
    /// Chrome 150: `<div>` holding a 24px-tall `inline-block` `::before` with the text "PRE", in a
    /// `16px/18px` font, is 24px tall -- the box's text sits on the line's baseline (28 when it is
    /// empty).
    pub(super) fn atomic_baseline(&self, index: u32) -> Option<f32> {
        let b = &self.boxes[index as usize];
        if b.natural.is_some() {
            return None;
        }
        let style = self.style_of_box(index);
        // A flex or grid container's baseline is its first item's that has one (CSS Flexbox §8.5, Grid
        // §11.7), whatever its `overflow`. Chrome 150: `<span style="display:inline-flex"><i>a</i>
        // </span>` sits on the line by its text; it sat on its bottom margin edge here.
        if matches!(style.display, Display::Flex | Display::Grid) {
            let (start, end) = b.children;
            for &child in self.child_list.get(start as usize..end as usize)? {
                let c = &self.boxes[child as usize];
                if matches!(self.styles.position(c.style), CssPosition::Absolute | CssPosition::Fixed) {
                    continue;
                }
                // a run of bare text laid out as the item itself (a text leaf, no line walk of its own): its first
                // line's baseline, as an IFC's would be (Chrome 150: `display:inline-flex` over bare 16px text with
                // `line-height: 1` sits 4px down a 16px line, where it sat on its bottom edge here)
                let base = if c.ifc.is_some() {
                    self.maps.last_baselines.get(&child).copied()
                } else if c.text.is_some() {
                    Some(self.vertical_of(child).above)
                } else {
                    self.atomic_baseline(child)
                };
                if let Some(base) = base {
                    return Some(self.st[child as usize].unrounded.location.y + base);
                }
            }
            return None;
        }
        // A SCROLL CONTAINER synthesizes its baseline off its bottom margin edge; `overflow: clip`
        // makes none (Blink's `PhysicalBoxFragment::ForceInlineBaselineSynthesis` asks
        // `IsScrollContainer`). Chrome 150: a broken `<img alt=x>` -- `overflow: clip` from the UA
        // sheet -- sits on the line by its alt text (`broken_img_live`).
        let scrolls = |o: Overflow| matches!(o, Overflow::Hidden | Overflow::Scroll);
        if scrolls(style.overflow.x) || scrolls(style.overflow.y) {
            return None;
        }
        let (start, end) = b.children;
        for &child in self.child_list.get(start as usize..end as usize)?.iter().rev() {
            let c = &self.boxes[child as usize];
            if c.ifc.is_none() {
                continue;
            }
            if let Some(base) = self.maps.last_baselines.get(&child) {
                return Some(self.st[child as usize].unrounded.location.y + base);
            }
        }
        None
    }

    /// Is box `index` a checkbox or radio button the theme draws, whose baseline is its border box's
    /// bottom (see `LayoutBox::theme_part`)?
    pub(super) fn is_theme_part_box(&self, index: u32) -> bool {
        self.boxes[index as usize].theme_part
    }

    /// The baseline an atomic inline with no line of its own SYNTHESIZES off its
    /// `InlineBlockBaselineEdge` (Blink's `LogicalBoxFragment::BaselineMetrics`): a `<button>`'s is its
    /// content box's bottom (`HTMLButtonElement` sets `kContentBox`) -- `height` its border-box height.
    /// Chrome 150: an empty `<button>` (16 x 6) sits with its top 3px above the line's baseline, where it
    /// sat on its bottom margin edge. `None` for any other box: the initial edge is the margin box, the
    /// bottom margin edge of CSS 2.1 §10.8.1 (an empty `inline-flex` too: Chrome's line is 20px around a
    /// 16px one).
    pub(super) fn synthesized_baseline(&self, index: u32, height: f32) -> Option<f32> {
        let b = &self.boxes[index as usize];
        if !b.button {
            return None;
        }
        let u = &self.st[index as usize].unrounded;
        let (border, padding) = (u.border, u.padding);
        Some(height - border.bottom - padding.bottom)
    }

    /// A text control's baseline from its border-box top, once it has been measured (`height` its
    /// border-box height): its line of text's, centred in its content box -- Blink lays the inner editor
    /// of a text field, and a button's label, out that way. Chrome 150: a 21px `<input>` in 14px text
    /// has its top 15px above the line's baseline (it sat on its bottom margin edge here, which pushed
    /// every other box on the line 5px down). `None` for any other box.
    pub(super) fn text_control_baseline(&self, index: u32, height: f32) -> Option<f32> {
        let b = &self.boxes[index as usize];
        if !b.text_control {
            return None;
        }
        let u = &self.st[index as usize].unrounded;
        let (border, padding) = (u.border, u.padding);
        let v = self.vertical_of(index);
        let line = v.above + v.below;
        let content = height - border.top - border.bottom - padding.top - padding.bottom;
        let centre = ((content - line) / 2.0).max(0.0);
        Some(border.top + padding.top + crate::layout::style::layout_unit(centre) + v.above)
    }

    /// Does this document apply the line-height quirk? Both quirks modes do.
    pub(super) fn line_height_quirk(&self) -> bool {
        self.mode != korvus_dom::DocumentMode::NoQuirks
    }

    /// Every `::before` / `::after` box: its element, whether it is the `::after`, and its index (the paint walk).
    pub(crate) fn pseudo_box_list(&self) -> Vec<(NodeId, bool, u32)> {
        self.pseudo_boxes.iter().map(|(&(node, after), &index)| (node, after, index)).collect()
    }

    /// The box a `::before` (`after` false) or `::after` generated, if any.
    pub fn pseudo_box(&self, node: NodeId, after: bool) -> Option<u32> {
        self.pseudo_boxes.get(&(node, after)).copied()
    }

    /// Every generated box this tree built, read-only: `(element, after, text, level,
    /// out of flow, preserves white space)`. `innerText` never writes this text, but the text is laid
    /// out, so it takes part in the white-space collapsing around it.
    pub fn generated_boxes(&self) -> impl Iterator<Item = (NodeId, bool, &str, BoxLevel, bool, bool)> + '_ {
        self.pseudo_boxes.iter().filter_map(|(&(node, after), &index)| {
            let g = self.generated.get(&node)?;
            let pb = if after { g.after.as_ref()? } else { g.before.as_ref()? };
            let b = &self.boxes[index as usize];
            let out_of_flow = matches!(self.styles.position(b.style), CssPosition::Absolute | CssPosition::Fixed);
            Some((node, after, pb.text.as_str(), b.level, out_of_flow, self.styles.font(b.style).preserve))
        })
    }

    /// An element's content-box size, for a container query.
    pub fn content_size(&self, node: NodeId) -> Option<(f32, f32)> {
        let index = self.box_of(node)?;
        let l = &self.st[index as usize].final_layout;
        let bars = self.scrollbar_gutter(index);
        Some((
            (l.size.width - l.padding.left - l.padding.right - l.border.left - l.border.right - bars.width).max(0.0),
            (l.size.height - l.padding.top - l.padding.bottom - l.border.top - l.border.bottom - bars.height).max(0.0),
        ))
    }

    /// A box for an inline-level element. It is **not** a taffy child of anything — its parent is
    /// the IFC box that will place it, which is what makes `origin()` answer correctly for a
    /// fragment without a special case.
    fn add_inline_box(
        &mut self,
        dom: &Dom,
        node: NodeId,
        style: u32,
        level: BoxLevel,
        ifc: u32,
        pending: &mut Vec<Vec<u32>>,
    ) -> u32 {
        let index = self.boxes.len() as u32;
        let mut b = LayoutBox::new(node, style, Some(ifc), level);
        b.text_overflow_width = self.input_value_width(dom, node, style);
        b.scrolls_own_text = self.is_text_entry_control(dom, node);
        self.boxes.push(b);
        pending.push(Vec::new());
        self.box_of_node[node as usize] = Some(index);
        index
    }

    /// The anonymous box for the run of inline content being collected, making it on first use.
    ///
    /// It carries the block's **node** so that a hit on the line's leading lands on the block, and
    /// an anonymous style so that the block's padding, border and width are not applied twice.
    fn ensure_ifc(&mut self, block: u32, pending: &mut Vec<Vec<u32>>, run: &mut InlineRun) -> u32 {
        if let Some(i) = run.ifc {
            return i;
        }
        let index = self.boxes.len() as u32;
        let base = self.boxes[block as usize].style;
        let mut style = self.styles.intern_anonymous(base);
        // the anonymous box of a legacy `-webkit-box` is one of its items (see [`BoxTree::add_children`])
        if self.styles.legacy_flex(base) {
            let variant = self.styles.webkit_box_item(base);
            style = self.styles.intern_variant(style, variant);
        }
        let node = self.boxes[block as usize].node;
        // Deliberately NOT recorded in `box_of_node`: this box belongs to the same element as its
        // parent, and claiming the entry would make `getBoundingClientRect` on that element answer
        // with the anonymous box instead of the element's own.
        self.boxes.push(LayoutBox::new(node, style, Some(block), BoxLevel::Block));
        pending.push(Vec::new());
        pending[block as usize].push(index);
        run.ifc = Some(index);
        // Every inline box a block-level child interrupted re-opens here, outermost first, so that
        // the content after the interruption is still inside them. That is CSS's own answer: the
        // inline box is SPLIT, and Chrome reports the two halves as two of its three rects.
        for b in run.open.clone() {
            run.items.push(Item::Open(b));
        }
        index
    }

    /// Float `b`, just made a child of `block`, written inside the inline content of `run`: it goes before the run's
    /// anonymous box, so the block layout places it at the top of that content and the lines beside it flow around
    /// it -- `<h2>Title <span style="float:right">more</span></h2>` keeps "more" on the title's line, at its right.
    /// (Chrome places a float at the top of the line it was written on; after a run of several lines this places
    /// it at the run's first.) With no content before it in the run, it simply comes next.
    fn hoist_float(&self, block: u32, b: u32, run: &InlineRun, pending: &mut [Vec<u32>]) {
        let Some(ifc) = run.ifc else { return };
        let list = &mut pending[block as usize];
        if let (Some(at), Some(from)) = (list.iter().position(|&c| c == ifc), list.iter().position(|&c| c == b)) {
            if from > at {
                let moved = list.remove(from);
                list.insert(at, moved);
            }
        }
    }

    /// Hand the collected items to their IFC box and start a fresh run.
    ///
    /// Any inline box still open is closed on the way out — the run is ending, so its fragment
    /// ends with it — and [`BoxTree::ensure_ifc`] re-opens it on whatever run comes next.
    fn flush_run(&mut self, run: &mut InlineRun) {
        if let Some(ifc) = run.ifc.take() {
            for b in run.open.iter().rev() {
                run.items.push(Item::Close(*b));
            }
            let items = std::mem::take(&mut run.items);
            self.boxes[ifc as usize].ifc = Some(items.into_boxed_slice());
        }
        run.items.clear();
    }

    /// An element's own layout style: its computed style interned, with the one adjustment Blink's
    /// style adjuster makes that the cascade here does not -- a `<textarea>`'s `visible` overflow is
    /// `auto` (see [`Variant::TextareaOverflow`]).
    fn element_style(&mut self, dom: &Dom, arena: &StyleArena, node: NodeId, key: StyleKey) -> u32 {
        let mut style = self.styles.intern(arena, key);
        // `::-webkit-scrollbar` sizes a classic scrollbar per axis -- unless a standard
        // `scrollbar-width` or `scrollbar-color` is set, which wins. Chrome 150 on a
        // 200x100 `overflow: scroll` box: `{ width: 8px }` 192x85, `{ height: 6px }` 185x94, `{ width:
        // 8px; height: 8px }` under `scrollbar-width: thin` 190x90, `{ display: none }` 200x100.
        if let Some((w, h)) = self.generated.get(&node).and_then(|g| g.scrollbar) {
            let standard = arena.value(key, PropertyId::ScrollbarWidth).trim() != "auto"
                || arena.value(key, PropertyId::ScrollbarColor).trim() != "auto";
            if !standard && self.styles.theme().scrollbar_thickness > 0.0 {
                let theme = self.styles.theme().scrollbar_thickness;
                let q = |v: Option<f32>| (v.unwrap_or(theme).max(0.0) * 64.0).round() as u32;
                style = self.styles.intern_variant(style, Variant::CustomScrollbar { vertical: q(w), horizontal: q(h) });
            }
        }
        let is_textarea = dom.is_html_ns(node)
            && dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("textarea"));
        if is_textarea {
            self.styles.intern_variant(style, Variant::TextareaOverflow)
        } else {
            style
        }
    }

    /// The interned layout style of an element, for a text node that has to be measured in its
    /// parent's font. `None` for an element the caller did not style.
    fn styles_of_element(
        &mut self,
        style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
        arena: &StyleArena,
        node: NodeId,
    ) -> Option<u32> {
        let key = style_of(node)?;
        Some(self.styles.intern(arena, key))
    }

    /// How wide an `<input>`'s own value is, or `0.0` for every other element.
    ///
    /// `<input>` is the one element whose rendered content the box tree cannot reach: its value is
    /// an attribute, and the child walk only sees nodes. `<textarea>`'s value IS a text child and is
    /// already measured like any other run, which is why it is not here.
    ///
    /// WHICH TYPES, measured on Chrome 150.0.7871.46 over CDP (2026-09-13) — 22 types, one 57-character
    /// value, each in a 100×50 field, reading `scrollWidth/clientWidth` and then whether
    /// `scrollLeft = 10` sticks:
    ///
    /// ```text
    /// text search url tel email   387/104 (search 400/96)   scrollLeft 10   the value is a scrolling area
    /// password                    275/104                   scrollLeft 10   ...of BULLETS, not the value
    /// submit button reset         389/96                    scrollLeft  0   overflows, does not scroll
    /// number                      104/104                   scrollLeft  0
    /// date time month week        101/101                   scrollLeft  0
    /// datetime-local color file   101/101, 98/98, 100/100    scrollLeft  0
    /// checkbox radio range image  100/100                   scrollLeft  0
    /// hidden                        0/0                     scrollLeft  0
    /// ```
    ///
    /// So the value widens the scrolling area for the six text-entry types and for the three button
    /// types, and for nothing else. Whether the control can then be SCROLLED through that area is a
    /// second question, answered by [`BoxTree::is_scroll_container`].
    ///
    /// A PASSWORD FIELD IS MEASURED AS BULLETS, one U+2022 per character, which is what it renders
    /// and what Chrome's 275 (against text's 387 for the same string) is the width of. Measuring the
    /// real characters would also have put the *shape of the password* into a number any script can
    /// read off `scrollWidth`.
    fn input_value_width(&self, dom: &Dom, node: NodeId, style: u32) -> f32 {
        if !dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("input")) {
            return 0.0;
        }
        let kind = InputKind::of(dom.get_attribute(node, "type"));
        if !kind.value_widens_the_scrolling_area() {
            return 0.0;
        }
        let value = match dom.get_attribute(node, "value") {
            Some(v) if !v.is_empty() => v,
            _ => return 0.0,
        };
        let spec = self.styles.font(style);
        let rendered = if kind == InputKind::Password {
            "\u{2022}".repeat(value.chars().count())
        } else {
            value.to_string()
        };
        // `None` available width: one line, never wrapped. A single-line control does not wrap, and
        // neither does a button's label.
        inline::measure_block(self.fonts.as_ref(), spec, &rendered, None).width
    }

    /// Does this box scroll its own value, the way a text field does? See [`InputKind`].
    fn is_text_entry_control(&self, dom: &Dom, node: NodeId) -> bool {
        dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("input"))
            && InputKind::of(dom.get_attribute(node, "type")).scrolls_its_value()
    }

    /// The natural size of a replaced element or a form control, or `None` for a box whose size
    /// comes from its content. See [`crate::layout::replaced`] for what is done with it.
    ///
    /// WHERE THE ATTRIBUTES GO: NOT HERE. `width`/`height` on `<img>`, `<video>`, `<iframe>`, `<embed>`,
    /// `<object>` and an image button are presentational hints -- CSS `width`/`height` at the bottom of the
    /// author level, with `aspect-ratio: auto w / h` on an image or a video that has both
    /// (`style::hints`) -- so the box's STYLE carries them and this answers only what the element itself
    /// is. They used to be read here as its natural size, for want of hints in the cascade, which refused
    /// a percentage (`<iframe width="100%">` was 300 wide, `<img width="50%">` 0) and let one axis an
    /// author sized override the hint on the other (`<img width=40 height=30 style="width:100px">` was
    /// 100x75; Chrome 150 lays it out 100x30, `fixtures/presentational-hints`).
    ///
    /// An `<img>` whose image is AVAILABLE has its decoded natural size, which the caller
    /// hands over (`Page::set_natural_images`); one that has not loaded is 0x0, Chrome's answer until it
    /// has, and the hints size it. It said "an <img> never has a decoded natural size in this engine":
    /// hubspot.com's logo carousel then cloned slides until its 64-iteration cap on every call, and the
    /// page never loaded.
    fn natural_size(&self, dom: &Dom, node: NodeId, style: u32) -> Option<Natural> {
        let name = dom.tag_name(node)?;
        // Asked of every element the tree builds, so the overwhelmingly common answer is found
        // without allocating: every tag below is at most eight bytes and starts with one of these.
        let first = name.as_bytes().first().map(u8::to_ascii_lowercase);
        if name.len() > 8 || !matches!(first, Some(b'a' | b'i' | b'c' | b'v' | b's' | b'o' | b'e' | b't')) {
            return None;
        }
        let tag = name.to_ascii_lowercase();
        if !dom.is_html_ns(node) {
            return (tag == "svg" && self.is_outermost_svg(dom, node)).then(|| svg_natural(dom, node));
        }
        let attr = |name: &str| dom.get_attribute(node, name);
        let spec = self.styles.font(style);
        let metrics = || self.fonts.metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
        Some(match tag.as_str() {
            "img" => {
                // A broken image is not replaced at all unless it already has dimensions (see
                // [`BoxTree::image_fallback`]): its box is the fallback content's.
                let fallback = self.image_fallback(dom, node);
                if fallback && !self.fallback_is_replaced(dom, node, style) {
                    return None;
                }
                match self.natural_images.get(&node) {
                    Some(&ImageState::Available(nw, nh)) if nw > 0.0 && nh > 0.0 => Natural::sized(nw, nh),
                    _ if fallback && self.mode == korvus_dom::DocumentMode::Quirks => {
                        // A quirks-mode fallback host with ONE dimension takes it for both
                        // (`HTMLImageFallbackHelper::AdjustHostStyle` copies the specified length across).
                        let s = self.styles.get(style);
                        let (w, h) = (s.size.width.into_option(), s.size.height.into_option());
                        Natural { width: Some(w.or(h).unwrap_or(0.0)), height: Some(h.or(w).unwrap_or(0.0)), ratio: None }
                    }
                    _ => Natural::sized(0.0, 0.0),
                }
            }
            // A canvas's natural size IS its bitmap's, and the bitmap is its attributes -- 300 and
            // 150 when absent or invalid, which is HTML's own default, not a browser's.
            "canvas" => Natural::sized(
                attr("width").and_then(non_negative_integer).unwrap_or(300) as f32,
                attr("height").and_then(non_negative_integer).unwrap_or(150) as f32,
            ),
            // No metadata is ever read on this path (R7), so a video has no natural size of its own: CSS's
            // default object size, with no ratio (Chrome 150: `<video width=100>` is 100x150).
            "video" => Natural::default(),
            // `<audio controls>`: nothing natural; the UA sheet's `width: 300px; height: 54px` sizes
            // it, which an inline box would ignore. (Without `controls` it is `display: none`.)
            "audio" => Natural::default(),
            // A frame has no natural size: 300x150 unless its style says otherwise (Chrome 150: `<iframe
            // width=40 height=30 style="height:auto">` is 40x150).
            "iframe" | "object" => Natural::default(),
            // An `<embed>` with nothing to embed renders nothing in Chrome (its rect is 0×0 at the
            // origin), so only one with a `src` is a replaced box here.
            "embed" if attr("src").is_some() => Natural::default(),
            "input" => {
                let kind = attr("type").unwrap_or("text").trim().to_ascii_lowercase();
                match kind.as_str() {
                    "submit" | "reset" | "button" => {
                        // `submit` and `reset` with no `value` show the browser's LOCALISED default label: "제출" under
                        // a Korean UI (the theme's, from the persona's locale). Left unsized, the button was a sliver.
                        let theme = self.styles.theme();
                        let label = match attr("value") {
                            Some(v) => v,
                            None if kind == "button" => "",
                            None if kind == "submit" => if theme.submit_label.is_empty() { "Submit" } else { theme.submit_label },
                            None => if theme.reset_label.is_empty() { "Reset" } else { theme.reset_label },
                        };
                        replaced::input_button(metrics(), spec, label)
                    }
                    // The types that render a text editor. Every other type is a theme part
                    // (checkbox, radio, range, colour) or a composite (date, file) whose size is not
                    // a function of the font alone.
                    // A checkbox or radio button the theme draws: the native theme part's size, 13 x 13 on
                    // Linux (Blink's `ThemePartIntrinsicSize`, NativeThemeBase), as its intrinsic
                    // CONTENT size -- no ratio: `width: 20px` leaves it 13 tall. Chrome 150: 13 x 13, and
                    // `getComputedStyle` reads `13px` (it was 0 x 0 here).
                    "checkbox" | "radio" if self.styles.themed(style) => Natural { width: Some(13.0), height: Some(13.0), ratio: None },
                    // A range: Blink's default track length by its thumb's height, whatever its `appearance` and font
                    // (Chrome 150: 129 x 16; it was 0 x 0 here, and a page's slider collapsed).
                    "range" => Natural { width: Some(129.0), height: Some(16.0), ratio: None },
                    "date" | "month" | "week" | "time" | "datetime-local" => {
                        // the line opens for every face its fields' text is set in, the placeholder's too
                        let (widest, fields, placeholder) = replaced::datetime_pattern(&self.styles.theme(), &kind)?;
                        let line = self.normal_line_height(spec, metrics(), &format!("{widest}{placeholder}"));
                        replaced::datetime_input(metrics(), spec, widest, fields, kind == "time", line)
                    }
                    "file" => {
                        let (button, none) = replaced::file_labels(&self.styles.theme(), attr("multiple").is_some());
                        let line = self.normal_line_height(spec, metrics(), button);
                        replaced::file_input(metrics(), spec, button, none, line)
                    }
                    _ if sizes_as_text_field(&kind) => {
                        let size = attr("size").and_then(non_negative_integer).filter(|&n| n > 0);
                        replaced::text_field(metrics(), spec, size.unwrap_or(20))
                    }
                    _ => return None,
                }
            }
            "textarea" => {
                let cols = attr("cols").and_then(non_negative_integer).filter(|&n| n > 0);
                let rows = attr("rows").and_then(non_negative_integer).filter(|&n| n > 0);
                // Blink ALWAYS adds the scrollbar's thickness to the width when the block axis is
                // `auto` or `scroll`, and to the height only for `overflow-x: scroll` (layout_box.cc
                // TextAreaIntrinsicInlineSize / BlockSize) -- whether or not anything overflows.
                let s = self.styles.get(style);
                let auto = self.styles.auto_scroll(style);
                let bars = Size {
                    width: if s.overflow.y == Overflow::Scroll || auto.y { s.scrollbar_width } else { 0.0 },
                    height: if s.overflow.x == Overflow::Scroll { s.scrollbar_width } else { 0.0 },
                };
                replaced::textarea(metrics(), spec, cols.unwrap_or(20), rows.unwrap_or(2), bars)
            }
            // A drop-down. A list box (`multiple`, or `size` > 1) is sized by its rows, which are
            // laid-out option boxes, and is not modelled here.
            "select" => {
                let size = attr("size").and_then(non_negative_integer).unwrap_or(0);
                if attr("multiple").is_some() || size > 1 {
                    return None;
                }
                let labels = option_labels(dom, node);
                // The theme's internal paddings apply only to a control that has an appearance:
                // Chrome 150 gives `appearance: none` on an empty 100px-Arial `<select>` 0 × 112
                // where the themed one is 20 × 114.
                let themed = self.styles.themed(style);
                let theme = self.styles.theme();
                let padding = if themed {
                    Size { width: theme.menu_list_inline_padding, height: theme.menu_list_block_padding }
                } else {
                    Size::ZERO
                };
                replaced::menu_list(metrics(), spec, labels.iter().map(String::as_str), padding)
            }
            _ => return None,
        })
    }

    /// The inline advance of element `node`'s `inside` list marker, in 1/64 px, when it is a list item
    /// whose marker is a SYMBOL -- Blink's `ListMarker`: a disclosure triangle is
    /// `DisclosureSymbolSize` (0.66em of the specified size) plus a 0.4em end margin
    /// (`kClosureMarkerMarginEm`); a disc, circle or square `(ascent * 2/3 + 1) / 2 + 2` in whole
    /// pixels of the font's ascent (`WidthOfSymbol`) with margins of -1 and 1em (`kCUAMarkerMarginEm`).
    /// Chrome 150: a closed `<summary>` in 14px text starts its text 14.828125px in, `square inside` 19px
    /// in; korvus drew no marker. A counter marker (`decimal` and the rest) is text, not handled here.
    fn inside_marker_width(&self, arena: &StyleArena, style_of: &dyn Fn(NodeId) -> Option<StyleKey>, node: NodeId, style: u32) -> Option<i32> {
        let key = style_of(node)?;
        if !arena.value(key, PropertyId::Display).split_whitespace().any(|w| w == "list-item") {
            return None;
        }
        if arena.value(key, PropertyId::ListStylePosition).trim() != "inside" || arena.value(key, PropertyId::ListStyleImage).trim() != "none" {
            return None;
        }
        let size = arena.value(key, PropertyId::FontSize).trim().trim_end_matches("px").parse::<f32>().ok()?;
        let lu = |px: f32| (px * 64.0) as i32;
        match arena.value(key, PropertyId::ListStyleType).trim() {
            "disclosure-closed" | "disclosure-open" => Some(lu(size * 0.66) + lu(0.4 * size)),
            "disc" | "circle" | "square" => {
                let spec = self.styles.font(style);
                let metrics = self.fonts_metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
                let ascent = (metrics.ascent * crate::font::used_size(spec.size)).round() as i32;
                Some(-64 + ((ascent * 2 / 3 + 1) / 2 + 2) * 64 + lu(size))
            }
            _ => None,
        }
    }

    /// Is this an `<input>` checkbox or radio button the theme draws (`appearance` not `none`)?
    fn is_theme_part(&self, dom: &Dom, node: NodeId, style: u32) -> bool {
        dom.is_html_ns(node)
            && dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("input"))
            && dom.get_attribute(node, "type").is_some_and(|t| matches!(t.trim().to_ascii_lowercase().as_str(), "checkbox" | "radio"))
            && self.styles.themed(style)
    }

    /// Is this `<svg>` the outermost one — the one that is a CSS replaced box? An `<svg>` nested in
    /// another is SVG layout, and never reaches here anyway, because its parent's children are not
    /// given boxes.
    fn is_outermost_svg(&self, dom: &Dom, node: NodeId) -> bool {
        let svg_ns = dom.namespace_uri(node) == Some("http://www.w3.org/2000/svg");
        svg_ns
            && dom.node(node).parent.is_none_or(|p| {
                dom.namespace_uri(p) != Some("http://www.w3.org/2000/svg")
            })
    }

    /// Does this `<img>` render Blink's FALLBACK content instead of an image (A/B C8)?
    ///
    /// Blink's `HTMLImageElement` has a layout disposition: PRIMARY content (a `LayoutImage`) while
    /// its image is "potentially available" -- loaded, in flight, or lazily deferred -- and FALLBACK
    /// content once it is not (`ResetLayoutDisposition`, run when a source is selected and when a
    /// load completes): a `LayoutBlockFlow` over a user-agent shadow tree of a broken-image icon and
    /// the alt text (`HTMLImageFallbackHelper::CreateAltTextShadowTree`). An `<img>` that never
    /// selected a source goes to its fallback content only when it is inserted with alt text
    /// (`HTMLImageElement::InsertedInto` -> `HTMLImageLoader::NoImageResourceToLoad`), so a bare
    /// `<img>` stays an empty replaced box. Chrome 150, measured (`broken_img_live`): a 404 image is
    /// 16x16, the same with `alt="x"` is 23.97x24, `<img alt=x>` with no `src` is 23.97x24, a bare
    /// `<img>` 0x0. A request this engine's loader reports broken is [`ImageState::Broken`].
    pub(crate) fn image_fallback(&self, dom: &Dom, node: NodeId) -> bool {
        if !dom.is_html_ns(node) || !dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("img")) {
            return false;
        }
        match self.natural_images.get(&node) {
            Some(ImageState::Broken) => true,
            Some(ImageState::Available(..)) => false,
            None => {
                dom.get_attribute(node, "src").is_none()
                    && dom.get_attribute(node, "srcset").is_none()
                    && !dom.parent(node).is_some_and(|p| dom.is_html_ns(p) && dom.tag_name(p).is_some_and(|t| t.eq_ignore_ascii_case("picture")))
                    && !image_alt_text(dom, node).is_empty()
            }
        }
    }

    /// `TreatImageAsReplaced` (`html_image_fallback_helper.cc`): a fallback host that already has
    /// dimensions -- a `width` and a `height`, or one of them and an `aspect-ratio` -- stays a REPLACED
    /// box of that size, its content clipped inside, when it has no `alt` attribute or the document is
    /// in quirks mode (where one dimension is both, `AdjustHostStyle`). Its dimensions are its style's:
    /// CSS or the attributes' presentational hints (`style::hints`). Chrome 150: `<img src=bad width=30
    /// height=40>` is 30x40, the same with `alt="x"` is the alt text's 23.97x24, with `width=30` alone the
    /// icon's 16x16.
    fn fallback_is_replaced(&self, dom: &Dom, node: NodeId, style: u32) -> bool {
        let s = self.styles.get(style);
        let quirks = self.mode == korvus_dom::DocumentMode::Quirks;
        let mut has_width = !(s.size.width == Dimension::auto() || s.size.width == Dimension::fit_content());
        let mut has_height = s.size.height != Dimension::auto();
        if quirks {
            let either = has_width || has_height;
            (has_width, has_height) = (either, either);
        }
        let dimensions = (has_width && has_height) || (s.aspect_ratio.is_some() && (has_width || has_height));
        dimensions && (quirks || dom.get_attribute(node, "alt").is_none_or(str::is_empty))
    }

    /// A non-replaced fallback host's layout style: see [`Variant::ImageFallback`].
    fn fallback_host_style(&mut self, style: u32) -> u32 {
        self.styles.intern_variant(style, Variant::ImageFallback)
    }

    /// A fallback host's content (`HTMLImageFallbackHelper`): the broken-image icon and the alt text.
    ///
    /// The icon is hidden when the image "represents nothing" (`ImageRepresentsNothing`: a `src` with
    /// an empty alt text, or no `src` and no alt text) -- an empty `alt` is how a page says an image
    /// is decorative, and Chrome 150 lays `<img src=bad alt="">` out 0x0. Otherwise it is shown, and the
    /// alt text (the `alt` attribute, else `title`: `HTMLImageElement::AltText`) follows it on its line.
    fn add_image_fallback(&mut self, dom: &Dom, node: NodeId, block: u32, pending: &mut Vec<Vec<u32>>) {
        let alt = image_alt_text(dom, node);
        let alt_set = dom.get_attribute(node, "alt").is_some() || dom.get_attribute(node, "title").is_some();
        let src_set = dom.get_attribute(node, "src").is_some_and(|s| !s.is_empty());
        let represents_nothing = if src_set { alt_set && alt.is_empty() } else { !alt_set || alt.is_empty() };
        if represents_nothing {
            return;
        }
        let host = self.boxes[block as usize].style;
        // white space alone is no line: it would only collapse away beside the floated icon
        let text = inline::collapse(alt, self.styles.font(host).preserve);
        let base = self.styles.intern_anonymous(host);
        let icon = Some(Box::new(Natural::sized(BROKEN_IMAGE_ICON, BROKEN_IMAGE_ICON)));
        let Some(text) = text else {
            let style = self.styles.intern_variant(base, Variant::BrokenImageIcon { inline: false });
            let index = self.boxes.len() as u32;
            let mut b = LayoutBox::new(node, style, Some(block), BoxLevel::Block);
            b.natural = icon;
            self.boxes.push(b);
            pending.push(Vec::new());
            pending[block as usize].push(index);
            return;
        };
        let mut run = InlineRun::default();
        let ifc = self.ensure_ifc(block, pending, &mut run);
        let style = self.styles.intern_variant(base, Variant::BrokenImageIcon { inline: true });
        let index = self.boxes.len() as u32;
        let mut b = LayoutBox::new(node, style, Some(ifc), BoxLevel::AtomicInline);
        b.natural = icon;
        self.boxes.push(b);
        pending.push(Vec::new());
        pending[ifc as usize].push(index);
        run.items.push(Item::Atomic(index));
        let t = self.add_text(node, host, text, Some(ifc), pending, false);
        run.items.push(Item::Text(t));
        self.flush_run(&mut run);
    }

    /// A block-level `<button>` in a block container shrinks to fit. See [`Variant::ShrinkToFit`].
    ///
    /// Only in a BLOCK container: a flex or grid item is sized by its container's algorithm, and a
    /// `fit-content` width there would stop a column flexbox from stretching the button.
    fn shrinks_to_fit_in(&self, dom: &Dom, node: NodeId, style: u32, parent: u32) -> bool {
        dom.is_html_ns(node)
            && dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("button"))
            && self.styles.level(style) == BoxLevel::Block
            && matches!(
                self.styles.get(self.boxes[parent as usize].style).display,
                Display::Block | Display::FlowRoot
            )
    }

    /// Is `node` the `<body>` whose `overflow` is propagated to the viewport?
    ///
    /// CSS Overflow 3 §3.3: when the root element's `overflow` is `visible`, the `<body>`'s is
    /// applied to the viewport instead, and the body's own used `overflow` is `visible`. Measured on
    /// Chrome 150.0.7871.46 with `html, body { height: 100% } body { overflow-x: hidden }` over a
    /// 5000px child: `html.scrollHeight` 5000, `body.scrollHeight` 5000, `scrollTo(0, 2e6)` lands at
    /// 4007. Korvus made the body the scroller, so the document never scrolled (965 / 5000 / 0) —
    /// the signature on `science.nasa.gov`, `readthedocs.org` and `quilljs.com` in L15's sample.
    ///
    /// The computed value is untouched: `getComputedStyle(document.body).overflowX` stays `hidden`
    /// in Chrome too. Only the box taffy lays out changes.
    fn overflow_goes_to_the_viewport(&self, dom: &Dom, node: NodeId, style: u32) -> bool {
        let is_tag = |n: NodeId, tag: &str| {
            dom.is_html_ns(n) && dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case(tag))
        };
        let Some(root) = self.boxes.first() else { return false };
        if !is_tag(node, "body") || !is_tag(root.node, "html") {
            return false;
        }
        // "The first body element child of the root": a second `<body>` does not propagate.
        let first_body = dom.children(root.node).into_iter().find(|&c| is_tag(c, "body"));
        if first_body != Some(node) {
            return false;
        }
        let visible = |o: Point<taffy::style::Overflow>| {
            o.x == taffy::style::Overflow::Visible && o.y == taffy::style::Overflow::Visible
        };
        visible(self.styles.get(root.style).overflow) && !visible(self.styles.get(style).overflow)
    }

    /// Take the viewport's scrolling behaviour from `style` — the root's, or a propagated `<body>`'s.
    fn record_viewport_scroll(&mut self, style: u32) {
        let o = self.styles.get(style).overflow;
        let auto: AutoScroll = self.styles.auto_scroll(style);
        self.viewport_scroll = Point {
            x: ViewportScroll::of(o.x, auto.x),
            y: ViewportScroll::of(o.y, auto.y),
        };
    }

    /// An anonymous box holding one run of text.
    ///
    /// `taffy_child` says whether it is laid out as a box in its own right — which it is inside a
    /// flex or grid container, where it is an anonymous item — or placed on a line by the inline
    /// formatting context, which is every other case and where taffy must never see it.
    fn add_text(
        &mut self,
        node: NodeId,
        style: u32,
        text: String,
        parent: Option<u32>,
        pending: &mut Vec<Vec<u32>>,
        taffy_child: bool,
    ) -> u32 {
        let index = self.boxes.len() as u32;
        let mut b = LayoutBox::new(node, style, parent, BoxLevel::Inline);
        // laid out and painted in the case `text-transform` gives it, at the offsets the DOM's text has
        let text = match self.styles.font(style).transform.apply(&text) {
            std::borrow::Cow::Owned(t) => t,
            std::borrow::Cow::Borrowed(_) => text,
        };
        b.text = Some(text.into_boxed_str());
        self.boxes.push(b);
        pending.push(Vec::new());
        if taffy_child {
            if let Some(p) = parent {
                pending[p as usize].push(index);
            }
        }
        // Deliberately *not* recorded in `box_of_node`: this box belongs to the same element as its
        // parent, and overwriting the entry would make `getBoundingClientRect` on that element
        // return the text's box instead of the element's.
        index
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    /// Was `laid` -- a tree [`BoxTree::compute`] has laid out into `viewport` -- built from exactly what
    /// this unlaid tree was, box for box? `compute` reads nothing but the tree and the
    /// viewport, so then `laid`'s layout IS this tree's, and laying this one out again would only
    /// repeat it. Compared exactly, not by hash: the boxes' nodes, built style origins, places, text,
    /// natural sizes and builder facts, and what the tree as a whole was built with. (`box_of_node` is
    /// written from the boxes as they are made, so equal boxes give it equal entries; its LENGTH is the
    /// arena's, which a node made anywhere moves, and a lookup past the end answers "no box".)
    /// This tree's build fingerprint, and the viewport its layout was computed for. A later
    /// layout whose build fingerprint equals [`BoxTree::build_fp`] and whose viewport equals this one
    /// would build and lay out identically, so it keeps this tree instead of rebuilding it.
    pub(crate) fn build_fp(&self) -> Option<u64> {
        self.build_fp
    }
    pub(crate) fn set_build_fp(&mut self, fp: u64) {
        self.build_fp = Some(fp);
    }
    pub(crate) fn requested_viewport(&self) -> Option<Size<f32>> {
        self.requested
    }
    /// This tree's generated content (`::before`/`::after`/scrollbar boxes), by element. B0 compares it
    /// so a changed `attr()` or counter in generated content -- which moves no style key and whose
    /// attribute the match revision may exclude -- still rebuilds.
    pub(crate) fn generated_map(&self) -> &std::collections::HashMap<NodeId, Generated> {
        &self.generated
    }

    pub fn same_build(&self, laid: &BoxTree, viewport: Size<f32>) -> bool {
        if laid.requested != Some(viewport)
            || self.boxes.len() != laid.boxes.len()
            || self.built != laid.built
            || self.child_list != laid.child_list
            || self.styles.theme() != laid.styles.theme()
            || self.viewport_bars_off != laid.viewport_bars_off
            || self.fonts_id != laid.fonts_id
            || (self.mode, self.body_node, self.details_slot) != (laid.mode, laid.body_node, laid.details_slot)
            || self.cv_auto.len() != laid.cv_auto.len()
            || self.quirk_margin_boxes != laid.quirk_margin_boxes
            || self.text_nodes != laid.text_nodes
            || self.pseudo_boxes != laid.pseudo_boxes
        {
            return false;
        }
        let bits = |n: &Option<Box<crate::layout::replaced::Natural>>| {
            n.as_ref().map(|n| (n.width.map(f32::to_bits), n.height.map(f32::to_bits), n.ratio.map(f32::to_bits)))
        };
        self.boxes.iter().zip(&laid.boxes).all(|(a, b)| {
            a.node == b.node
                && a.children == b.children
                && a.parent == b.parent
                && a.text == b.text
                && a.level == b.level
                && a.ifc == b.ifc
                && a.text_overflow_width.to_bits() == b.text_overflow_width.to_bits()
                && a.scrolls_own_text == b.scrolls_own_text
                && bits(&a.natural) == bits(&b.natural)
                && a.built_facts() == b.built_facts()
                && a.static_flow == b.static_flow
        }) && self.cv_auto.iter().zip(&laid.cv_auto).all(|(a, b)| a.0 == b.0 && self.styles.origin(a.1) == laid.styles.origin(b.1))
    }

    /// Each box's SUBTREE hash: what its layout depends on that the tree was built with --
    /// its node, where its style came from ([`LayoutStyles::origin`]), its text, natural size and the
    /// facts the builder recorded on it -- combined with its children's, in order. Two boxes of two
    /// trees of one document with equal hashes lay out the same under the same constraints. Taken
    /// before `compute`, which moves some boxes' styles (scrollbars, the quirks fill).
    pub fn subtree_hashes(&self) -> Vec<u64> {
        use std::hash::{Hash, Hasher};
        let mut out = vec![0u64; self.boxes.len()];
        // A box is made after the box it is laid out in, so children have the higher indices.
        for i in (0..self.boxes.len()).rev() {
            let b = &self.boxes[i];
            let mut h = std::collections::hash_map::DefaultHasher::new();
            b.node.hash(&mut h);
            self.styles.origin(b.style).hash(&mut h);
            b.text.hash(&mut h);
            std::mem::discriminant(&b.level).hash(&mut h);
            b.text_overflow_width.to_bits().hash(&mut h);
            b.scrolls_own_text.hash(&mut h);
            match &b.natural {
                Some(n) => (n.width.map(f32::to_bits), n.height.map(f32::to_bits), n.ratio.map(f32::to_bits)).hash(&mut h),
                None => 0u8.hash(&mut h),
            }
            b.ifc.is_some().hash(&mut h);
            let (start, end) = b.children;
            for &c in &self.child_list[start as usize..end as usize] {
                out[c as usize].hash(&mut h);
            }
            out[i] = h.finish();
        }
        out
    }

    /// Carry the LAYOUT of every subtree this tree built exactly as `kept` did over from `kept` --
    /// the tree the last layout of this document produced, into the same viewport -- so [`BoxTree::compute`] lays
    /// out only what changed and the boxes on the way up to it. A carried box answers its parent's call from the
    /// cache taffy filled for it in `kept`, its descendants' layouts already in place, so a constraint that is the
    /// same costs nothing and one that moved lays it out again as before. Matched from the root down: a box whose
    /// carry hash ([`BoxTree::carry_hashes`]) is its counterpart's and whose subtree is the same box for box
    /// ([`BoxTree::same_shape`]) is carried whole; another is not, and its children are paired by node, in order.
    /// The hashes are of what each tree was BUILT with, so a subtree whose styles a pass moved (the scrollbars, the
    /// quirks fill) still matches a fresh build -- each state carried is checked against the style it was computed
    /// under before every pass ([`BoxTree::sync_origins`]) -- and one holding a `content-visibility: auto` box --
    /// unlocked by where it lands -- is not carried at all. A font-probe script that rewrites an off-screen
    /// `position: absolute` span's `font-family` and reads its `offsetWidth` hundreds of times (www.wayfair.com's
    /// search page, ~20 a second for 30-40 s) laid the whole document out for each.
    ///
    /// Returns how many boxes were carried.
    pub(crate) fn carry_from(&mut self, kept: &BoxTree) -> usize {
        self.carry(Kept::Shared(kept))
    }

    /// [`BoxTree::carry_from`], MOVING the carried states out of `kept` instead of copying them -- caches, fragments,
    /// line starts -- which leaves `kept` gutted ([`BoxTree::gutted`]). For a caller that replaces `kept` with this
    /// tree: copying a 16k-box tree's states, then dropping them with it, was the most of what a carried relayout
    /// cost once little was left to lay out.
    pub(crate) fn carry_from_moving(&mut self, kept: &mut BoxTree) -> usize {
        self.carry(Kept::Owned(kept))
    }

    /// Was a later tree's state moved out of this one? See [`BoxTree::gutted`].
    pub(crate) fn is_gutted(&self) -> bool {
        self.gutted
    }

    fn carry(&mut self, mut kept: Kept<'_>) -> usize {
        let pairs = {
            let kept = kept.tree();
            if self.boxes.is_empty()
                || kept.boxes.is_empty()
                || kept.st.len() != kept.boxes.len()
                || (kept.live_final && kept.alt.len() != kept.boxes.len())
                || self.mode != kept.mode
                || self.styles.theme() != kept.styles.theme()
                // a web font that arrived (or went) since: every text measured there was in other faces -- Chrome
                // 152 re-measures `font: 56px W, sans-serif` once W loads, where this carried the fallback's width
                // until something else moved the box (apple.com's hero headline, 308 px for 281)
                || self.fonts_id != kept.fonts_id
                || self.boxes[0].node != kept.boxes[0].node
            {
                return 0;
            }
            self.pair_with(kept)
        };
        if pairs.is_empty() {
            return 0;
        }
        self.relaid = true;
        // `kept`'s box -> this tree's, for what names a box: a fragment names the IFC box that placed it.
        let mut to_new = vec![u32::MAX; kept.tree().boxes.len()];
        for &(a, b) in &pairs {
            to_new[b as usize] = a;
        }
        // Each pass's state from the same pass of `kept` (see `compute`): the full-window pass's is `kept.alt` when
        // kept ran a final pass, else its only one; the final pass's is `kept.st` then, else the final states `kept`
        // itself carried and never used. The switch off carries the one state as before.
        let two = knob(Knob::TwoStates);
        let (live_final, kept_alt) = (kept.tree().live_final, kept.tree().alt.len() == kept.tree().boxes.len());
        let probe_from_alt = two && live_final;
        let finals: Option<bool> = match (two, live_final) {
            (true, true) => Some(false),
            (true, false) if kept_alt => Some(true),
            _ => None,
        };
        // The same structure, every carried box at its own index: the last tree's state vectors are TAKEN whole, and
        // only the boxes not carried are reset -- moving 9k states of 672 bytes one by one was 38 % of a carried
        // relayout's cycles on a costco-like page (2026-10-23).
        if kept.tree().boxes.len() == self.boxes.len() && pairs.iter().all(|&(a, b)| a == b) {
            self.carry_whole(kept, &pairs, probe_from_alt, finals);
            return pairs.len();
        }
        // Every carried box in the same ORDER in both trees -- a subtree inserted or removed between them: each state
        // vector of `kept` is walked once, its carried states moved in order into this tree's and the rest dropped,
        // and the maps go entry by entry. Box by box, each state was written three times over (a default made here, one
        // left behind there, the move) and both maps were looked up per box: an inserted `<div>` cost 16.6 M cycles a
        // write+read on the 1,600-cell grid, 29 % of them in memmove (2026-10-23).
        let mut sorted = pairs.clone();
        sorted.sort_unstable();
        if sorted.windows(2).all(|w| w[0].1 < w[1].1) {
            self.carry_in_order(kept, &sorted, &to_new, probe_from_alt, finals);
            return pairs.len();
        }
        self.ensure_states();
        if finals.is_some() {
            self.alt = (0..self.boxes.len()).map(|_| BoxState::default()).collect();
            self.alt_maps = PassMaps::default();
        }
        let fix = |mut s: BoxState, follows_height: bool, carried: bool| {
            if follows_height {
                s.cache = BoxCache::default();
            }
            // An IFC that was not carried lays its items out again and writes its own fragments then.
            s.fragments.retain(|(ifc, _)| to_new[*ifc as usize] != u32::MAX);
            for (ifc, _) in &mut s.fragments {
                *ifc = to_new[*ifc as usize];
            }
            s.carried = carried;
            s.inherited = true;
            s
        };
        for &(a, b) in &pairs {
            let (a, b) = (a as usize, b as usize);
            // A box sized off its containing block's HEIGHT keeps no cache: taffy keys a cache on the box's
            // known size and available space, and a percentage height follows its parent's size, which neither holds.
            // Carried, a `height: 100%` chain under a container whose height moved answered from the layout before,
            // and a float shrink-wrapping an `aspect-ratio` child kept its old width (WPT css-sizing/
            // aspect-ratio-affects-container-width-when-height-changes, which Chrome 150 passes;
            // `KORVUS_VERIFY_INCREMENTAL` named the carried `height: 100%` box). Everything else of it is carried: a
            // parent that answers from its own cache never asks it again, so its layout stands; a parent laid out again
            // asks it, and with no cache it is laid out against the parent's size as it is now. Its ancestors keep
            // theirs -- one whose constraints did not move contains the same answer (R25's first draft carried none of
            // them, and the offline wayfair input replay went from ~0.06 s to 0.12-0.30 s).
            let follows_height = block_percent(self.styles.get(self.boxes[a].style));
            self.st[a] = fix(kept.state(probe_from_alt, b), follows_height, false);
            kept.maps_into(probe_from_alt, b, &mut self.maps, a);
            if let Some(from_alt) = finals {
                if !from_alt || kept.tree().alt[b].carried {
                    self.alt[a] = fix(kept.state(from_alt, b), follows_height, true);
                    kept.maps_into(from_alt, b, &mut self.alt_maps, a);
                }
            }
            // a text box's measured atoms, under this tree's id for the same style (equal hashes: equal origins)
            let measured = kept.tree().atoms.borrow().get(b).cloned().flatten();
            if let Some((_, atoms)) = measured {
                let style = self.boxes[a].style;
                let mut mine = self.atoms.borrow_mut();
                if mine.len() <= a {
                    mine.resize(a + 1, None);
                }
                mine[a] = Some((style, atoms));
            }
        }
        if let Kept::Owned(t) = kept {
            t.gutted = true;
        }
        pairs.len()
    }

    /// [`BoxTree::carry`] when the pairs `sorted` (by this tree's box) go up in `kept`'s boxes too: each state vector is
    /// walked once in order, and each state made what carrying it box by box would make it.
    fn carry_in_order(&mut self, mut kept: Kept<'_>, sorted: &[(u32, u32)], to_new: &[u32], probe_from_alt: bool, finals: Option<bool>) {
        let n = self.boxes.len();
        let follows: Vec<bool> = self.boxes.iter().map(|b| block_percent(self.styles.get(b.style))).collect();
        let fix = |mut s: BoxState, follows_height: bool, carried: bool| {
            if follows_height {
                s.cache = BoxCache::default();
            }
            // An IFC that was not carried lays its items out again and writes its own fragments then.
            s.fragments.retain(|(ifc, _)| to_new[*ifc as usize] != u32::MAX);
            for (ifc, _) in &mut s.fragments {
                *ifc = to_new[*ifc as usize];
            }
            s.carried = carried;
            s.inherited = true;
            s
        };
        let remap = |from: PassMaps, keep: &dyn Fn(usize) -> bool| PassMaps {
            last_baselines: from.last_baselines.into_iter().filter(|&(b, _)| to_new[b as usize] != u32::MAX && keep(b as usize)).map(|(b, v)| (to_new[b as usize], v)).collect(),
            text_lines: from.text_lines.into_iter().filter(|(b, _)| to_new[*b as usize] != u32::MAX && keep(*b as usize)).map(|(b, v)| (to_new[b as usize], v)).collect(),
        };
        let (old, old_maps) = kept.states(probe_from_alt);
        let built = &self.built;
        self.st = in_order(old, sorted, n, |a, s| fix(s, follows[a], false), |a| BoxState { origin: built[a], ..BoxState::default() });
        self.maps = remap(old_maps, &|_| true);
        if let Some(from_alt) = finals {
            let (old, old_maps) = kept.states(from_alt);
            let carried: Vec<bool> = if from_alt { old.iter().map(|s| s.carried).collect() } else { Vec::new() };
            // what is not carried is replaced at the final pass's start (`enter_final_pass`): only the flag matters
            self.alt = in_order(old, sorted, n, |a, s| if !from_alt || s.carried { fix(s, follows[a], true) } else { BoxState::default() }, |_| BoxState::default());
            self.alt_maps = remap(old_maps, &|b| !from_alt || carried[b]);
        }
        // the text boxes' measured atoms, under this tree's ids for the same styles (equal hashes: equal origins)
        let atoms = kept.atoms();
        let mut mine = self.atoms.borrow_mut();
        mine.clear();
        mine.resize(n, None);
        for (b, entry) in atoms.into_iter().enumerate() {
            if let (Some((_, measured)), Some(&a)) = (entry, to_new.get(b).filter(|&&a| a != u32::MAX)) {
                mine[a as usize] = Some((self.boxes[a as usize].style, measured));
            }
        }
        drop(mine);
        if let Kept::Owned(t) = kept {
            t.gutted = true;
        }
    }

    /// [`BoxTree::carry`] when every carried box is at its own index in both trees: the states of `kept` are taken (or
    /// copied, from a shared tree) whole, and each is then made what carrying it box by box would make it.
    fn carry_whole(&mut self, mut kept: Kept<'_>, pairs: &[(u32, u32)], probe_from_alt: bool, finals: Option<bool>) {
        let n = self.boxes.len();
        let mut paired = vec![false; n];
        for &(a, _) in pairs {
            paired[a as usize] = true;
        }
        let atoms = kept.atoms();
        let (mut st, mut maps) = kept.states(probe_from_alt);
        for i in 0..n {
            if paired[i] {
                let follows_height = block_percent(self.styles.get(self.boxes[i].style));
                let s = &mut st[i];
                if follows_height {
                    s.cache = BoxCache::default();
                }
                // An IFC that was not carried lays its items out again and writes its own fragments then.
                s.fragments.retain(|(ifc, _)| paired[*ifc as usize]);
                s.carried = false;
                s.inherited = true;
            } else {
                st[i] = BoxState { origin: self.built[i], ..BoxState::default() };
                maps.last_baselines.remove(&(i as u32));
                maps.text_lines.remove(&(i as u32));
            }
        }
        (self.st, self.maps) = (st, maps);
        if let Some(from_alt) = finals {
            let (mut alt, alt_maps) = kept.states(from_alt);
            for i in 0..n {
                let s = &mut alt[i];
                // what is not carried is replaced at the final pass's start (`enter_final_pass`): only the flag matters
                if paired[i] && (!from_alt || s.carried) {
                    if block_percent(self.styles.get(self.boxes[i].style)) {
                        s.cache = BoxCache::default();
                    }
                    s.fragments.retain(|(ifc, _)| paired[*ifc as usize]);
                    s.carried = true;
                    s.inherited = true;
                } else {
                    s.carried = false;
                }
            }
            (self.alt, self.alt_maps) = (alt, alt_maps);
        }
        // the text boxes' measured atoms, under this tree's ids for the same styles (equal hashes: equal origins)
        let mut mine = self.atoms.borrow_mut();
        mine.clear();
        mine.extend(atoms.into_iter().enumerate().map(|(i, a)| a.filter(|_| i < n && paired[i]).map(|(_, atoms)| (self.boxes[i].style, atoms))));
        drop(mine);
        if let Kept::Owned(t) = kept {
            t.gutted = true;
        }
    }

    /// The boxes of this tree to carry from `kept`, each with its counterpart there: see [`BoxTree::carry_from`].
    fn pair_with(&self, kept: &BoxTree) -> Vec<(u32, u32)> {
        let new_h = self.carry_h();
        let old_h = kept.carry_h();
        // A box whose subtree holds a `content-visibility: auto` box is laid out where it lands: never carried.
        let mut blocked = vec![false; self.boxes.len()];
        for &(index, _) in &self.cv_auto {
            let mut at = Some(index);
            while let Some(b) = at {
                if std::mem::replace(&mut blocked[b as usize], true) {
                    break;
                }
                at = self.boxes[b as usize].parent;
            }
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        let mut seen = vec![false; self.boxes.len()];
        let mut stack = vec![(0u32, 0u32)];
        // Pair the children by what they were built from, in order: the node, text or not, the level.
        let key = |t: &BoxTree, c: u32| {
            let b = &t.boxes[c as usize];
            (b.node, b.text.is_some(), std::mem::discriminant(&b.level))
        };
        let mut olds: Vec<Option<u32>> = Vec::new();
        while let Some((i, j)) = stack.pop() {
            if new_h[i as usize] == old_h[j as usize] && !blocked[i as usize] && self.same_shape(kept, i, j) {
                self.collect_pairs(kept, i, j, &mut seen, &mut pairs);
                continue;
            }
            // Each child with the FIRST remaining old child of an equal key: in order, so the cursor's entry is
            // usually it, and an inserted or removed child costs one scan past it.
            let (os, oe) = kept.boxes[j as usize].children;
            olds.clear();
            olds.extend(kept.child_list[os as usize..oe as usize].iter().map(|&o| Some(o)));
            let mut first = 0usize;
            let (ns, ne) = self.boxes[i as usize].children;
            for k in ns..ne {
                let c = self.child_list[k as usize];
                let want = key(self, c);
                let Some(at) = (first..olds.len()).find(|&p| olds[p].is_some_and(|o| key(kept, o) == want)) else { continue };
                stack.push((c, olds[at].take().expect("found")));
                while first < olds.len() && olds[first].is_none() {
                    first += 1;
                }
            }
        }
        pairs
    }

    /// The boxes `index` lays out: its `child_list` children, then the boxes of its inline formatting context's
    /// items -- the text runs, inline boxes and breaks an IFC box places through its `ifc` list, which are not its
    /// children. An atomic inline is both; [`BoxTree::carry_from`]'s walks take it twice and pair it once.
    fn laid_out_by(&self, index: u32) -> impl Iterator<Item = u32> + '_ {
        let b = &self.boxes[index as usize];
        let (s, e) = b.children;
        self.child_list[s as usize..e as usize]
            .iter()
            .copied()
            .chain(b.ifc.iter().flat_map(|items| items.iter().map(|it| it.box_index())))
    }

    /// This tree's carry hashes, made once: [`BoxTree::carry_hashes`] reads only what the tree was built with.
    fn carry_h(&self) -> &[u64] {
        self.carry_h.get_or_init(|| self.carry_hashes())
    }

    /// Each box's hash for [`BoxTree::carry_from`]: [`BoxTree::subtree_hashes`]' facts about the box -- its BUILT
    /// style origin -- combined with those of every box it lays out ([`BoxTree::laid_out_by`]), its IFC's items
    /// included: a text run whose text or style moved changes what its IFC box lays out. Post-order over the boxes
    /// reachable from the root, so no hash is read before it is made. Fx, and the items' kinds streamed: SipHash and a
    /// `Vec` a box made the two trees' hashes 15 % of a carried relayout (2026-10-23). Only a filter: `same_shape`
    /// decides, field for field.
    fn carry_hashes(&self) -> Vec<u64> {
        use std::hash::{BuildHasher, Hash, Hasher};
        let fx = korvus_dom::FxHashMap::<u8, u8>::default();
        let mut out = vec![0u64; self.boxes.len()];
        if self.boxes.is_empty() {
            return out;
        }
        let mut done = vec![false; self.boxes.len()];
        let mut stack: Vec<(u32, bool)> = vec![(0, false)];
        while let Some((i, expanded)) = stack.pop() {
            if done[i as usize] {
                continue;
            }
            if !expanded {
                stack.push((i, true));
                for c in self.laid_out_by(i) {
                    if !done[c as usize] {
                        stack.push((c, false));
                    }
                }
                continue;
            }
            let b = &self.boxes[i as usize];
            let mut h = fx.hasher().build_hasher();
            b.node.hash(&mut h);
            self.built[i as usize].hash(&mut h);
            b.text.hash(&mut h);
            std::mem::discriminant(&b.level).hash(&mut h);
            b.text_overflow_width.to_bits().hash(&mut h);
            b.scrolls_own_text.hash(&mut h);
            match &b.natural {
                Some(n) => (n.width.map(f32::to_bits), n.height.map(f32::to_bits), n.ratio.map(f32::to_bits)).hash(&mut h),
                None => 0u8.hash(&mut h),
            }
            // Not the builder facts (`LayoutBox::built_facts`): `same_shape` compares them exactly -- a subtree that
            // differs only there is refused by it and its children are paired, as a differing hash would have them.
            match &b.ifc {
                Some(items) => {
                    items.len().hash(&mut h);
                    for it in items.iter() {
                        std::mem::discriminant(it).hash(&mut h);
                    }
                }
                None => u64::MAX.hash(&mut h),
            }
            for c in self.laid_out_by(i) {
                out[c as usize].hash(&mut h);
            }
            out[i as usize] = h.finish();
            done[i as usize] = true;
        }
        out
    }

    /// Do the subtrees at `i` here and `j` in `kept` have the same boxes, node for node, child for child and item
    /// for item, built from the same styles with the same facts? Equal carry hashes say so; this says it, comparing
    /// every field the hash takes, without trusting a hash before anything is carried.
    fn same_shape(&self, kept: &BoxTree, i: u32, j: u32) -> bool {
        let natural = |n: &Option<Box<Natural>>| n.as_ref().map(|n| (n.width.map(f32::to_bits), n.height.map(f32::to_bits), n.ratio.map(f32::to_bits)));
        let mut stack = vec![(i, j)];
        // Each pair once. An atomic inline is both a child and an item of its IFC ([`BoxTree::laid_out_by`]), so the
        // walk reaches its subtree twice, and each nesting level doubled it: a page of inline-blocks nested some thirty
        // deep compared 2^30 pairs in one call, and www.nzherald.co.nz's page process spun in here past the 30 s
        // deadline at its first relayout (2026-10-23: 98.5 % of its samples in pair_with, which this is inlined into,
        // at constant memory).
        let mut compared: korvus_dom::FxHashSet<(u32, u32)> = Default::default();
        while let Some((i, j)) = stack.pop() {
            if !compared.insert((i, j)) {
                continue;
            }
            let (a, b) = (&self.boxes[i as usize], &kept.boxes[j as usize]);
            if a.node != b.node
                || self.built[i as usize] != kept.built[j as usize]
                || a.text != b.text
                || std::mem::discriminant(&a.level) != std::mem::discriminant(&b.level)
                || a.children.1 - a.children.0 != b.children.1 - b.children.0
                || a.ifc.as_ref().map(|x| x.len()) != b.ifc.as_ref().map(|x| x.len())
                || a.built_facts() != b.built_facts()
                || a.text_overflow_width.to_bits() != b.text_overflow_width.to_bits()
                || a.scrolls_own_text != b.scrolls_own_text
                || natural(&a.natural) != natural(&b.natural)
            {
                return false;
            }
            if let (Some(x), Some(y)) = (&a.ifc, &b.ifc) {
                if x.iter().zip(y.iter()).any(|(p, q)| std::mem::discriminant(p) != std::mem::discriminant(q)) {
                    return false;
                }
            }
            for (c, d) in self.laid_out_by(i).zip(kept.laid_out_by(j)) {
                stack.push((c, d));
            }
        }
        true
    }

    /// Every box of the subtree at `i` (children and IFC items) with its counterpart at `j` in `kept`, each once
    /// (the same shape: [`BoxTree::same_shape`]).
    fn collect_pairs(&self, kept: &BoxTree, i: u32, j: u32, seen: &mut [bool], out: &mut Vec<(u32, u32)>) {
        let mut stack = vec![(i, j)];
        while let Some((i, j)) = stack.pop() {
            if std::mem::replace(&mut seen[i as usize], true) {
                continue;
            }
            out.push((i, j));
            for (c, d) in self.laid_out_by(i).zip(kept.laid_out_by(j)) {
                stack.push((c, d));
            }
        }
    }

    /// The element whose own box `index` is, if it is one (not text, not anonymous, not generated).
    /// The style rows this tree worked out, for the next tree of the same theme.
    pub fn take_key_rows(&mut self) -> crate::layout::style::KeyRows {
        self.styles.take_key_rows()
    }

    /// Hand the style rows worked out so far back to this tree's table, for styles a patch interns.
    pub(crate) fn set_key_rows(&mut self, rows: crate::layout::style::KeyRows) {
        self.styles.set_key_rows(rows);
    }

    pub fn element_of_box(&self, index: u32) -> Option<NodeId> {
        let node = self.boxes.get(index as usize)?.node;
        (self.box_of_node.get(node as usize).copied().flatten() == Some(index)).then_some(node)
    }

    /// The first way this tree differs from `other`, built from the same document: a box's
    /// node, style, place in the tree, text or level, its geometry to the bit, or what the whole tree
    /// derived from them (the viewport's scrollbars, line starts, baselines). `None` when they agree.
    pub fn first_difference(&self, other: &BoxTree) -> Option<BoxDifference> {
        let tree = |what: String| Some(BoxDifference { node: None, what });
        if self.boxes.len() != other.boxes.len() {
            return tree(format!("{} boxes, full pass {}", self.boxes.len(), other.boxes.len()));
        }
        for (i, (a, b)) in self.boxes.iter().zip(&other.boxes).enumerate() {
            let at = |what: String| Some(BoxDifference { node: Some(a.node), what: format!("box {i}: {what}") });
            let (sa, sb) = (&self.st[i], &other.st[i]);
            if a.node != b.node {
                return at(format!("node {}, full pass {}", a.node, b.node));
            }
            if self.styles.get(a.style) != other.styles.get(b.style) {
                return at(format!("style {:?}, full pass {:?}", self.styles.get(a.style), other.styles.get(b.style)));
            }
            if (a.children, a.parent, a.level) != (b.children, b.parent, b.level) || a.text != b.text {
                return at(format!(
                    "children {:?} parent {:?} level {:?} text {:?}, full pass {:?} {:?} {:?} {:?}",
                    a.children, a.parent, a.level, a.text, b.children, b.parent, b.level, b.text
                ));
            }
            if sa.final_layout != sb.final_layout {
                return at(format!("layout {:?}, full pass {:?}", sa.final_layout, sb.final_layout));
            }
            // the snapped copies alike: say which unsnapped one differs, or the line reads as two equal layouts
            if sa.unrounded != sb.unrounded {
                return at(format!("unsnapped layout {:?}, full pass {:?}", sa.unrounded, sb.unrounded));
            }
            if sa.fragments != sb.fragments {
                return at(format!("fragments {:?}, full pass {:?}", sa.fragments, sb.fragments));
            }
            // Not `flex_cross_definite`: a flex container sets it at the start of each of its runs for
            // its items to read during that run (a change clears their caches), so after layout it says
            // only which run executed last -- which a cache hit skips.
            let bits = |x: &LayoutBox, s: &BoxState| (s.padding_basis.to_bits(), s.padding_item, x.text_overflow_width.to_bits(), s.inline_right.to_bits(), x.scrolls_own_text);
            if bits(a, sa) != bits(b, sb) || format!("{:?}{:?}{:?}", a.ifc, a.natural, sa.padding_override) != format!("{:?}{:?}{:?}", b.ifc, b.natural, sb.padding_override) {
                return at("its inline items, natural size, padding override or line facts".to_string());
            }
        }
        // Its length is the arena's when the tree was built; past the end a lookup answers "no box".
        let common = self.box_of_node.len().min(other.box_of_node.len());
        let tail_is_empty = |m: &[Option<u32>]| m[common..].iter().all(Option::is_none);
        if self.box_of_node[..common] != other.box_of_node[..common] || !tail_is_empty(&self.box_of_node) || !tail_is_empty(&other.box_of_node) {
            return tree("the element-to-box map".to_string());
        }
        if self.viewport_bars != other.viewport_bars {
            return tree(format!("viewport scrollbars {:?}, full pass {:?}", self.viewport_bars, other.viewport_bars));
        }
        let sorted = |m: &std::collections::HashMap<u32, Vec<usize>>| {
            let mut v: Vec<_> = m.iter().map(|(k, v)| (*k, v.clone())).collect();
            v.sort();
            v
        };
        if sorted(&self.maps.text_lines) != sorted(&other.maps.text_lines) {
            return tree("the line starts of a text".to_string());
        }
        let baselines = |m: &std::collections::HashMap<u32, f32>| {
            let mut v: Vec<_> = m.iter().map(|(k, v)| (*k, v.to_bits())).collect();
            v.sort();
            v
        };
        if baselines(&self.maps.last_baselines) != baselines(&other.maps.last_baselines) {
            return tree("a last baseline".to_string());
        }
        None
    }

    /// How many distinct layout styles the whole document needed.
    pub fn distinct_styles(&self) -> usize {
        self.styles.len()
    }

    /// The box an element generated, if it generated one.
    pub fn box_of(&self, node: NodeId) -> Option<u32> {
        *self.box_of_node.get(node as usize)?
    }

    /// How wide this box's own unlaid-out content is — an `<input>`'s value, and nothing else.
    /// See [`LayoutBox::text_overflow_width`].
    pub(crate) fn text_overflow_width(&self, index: u32) -> f32 {
        self.boxes[index as usize].text_overflow_width
    }

    /// The right edge of the widest line of inline content an ELEMENT's box holds, from its border-box
    /// origin (see [`LayoutBox::inline_right`]): its own IFC's, and that of every anonymous IFC box
    /// around its inline runs -- the usual case, and the one taffy cannot see through, because
    /// `intern_anonymous` gives such a box its element's `overflow`, so an `overflow: hidden` element's
    /// line content was clipped at the anonymous box before it reached the element.
    pub(crate) fn inline_content_right(&self, index: u32) -> f32 {
        let b = &self.boxes[index as usize];
        let mut right = self.st[index as usize].inline_right;
        let (start, end) = b.children;
        for &c in self.child_list.get(start as usize..end as usize).unwrap_or(&[]) {
            let cb = &self.boxes[c as usize];
            if cb.ifc.is_some() && cb.node == b.node {
                let cs = &self.st[c as usize];
                right = right.max(cs.unrounded.location.x + cs.inline_right);
            }
        }
        right
    }

    /// Is this box a scroll container because of what it is rather than because of its `overflow`?
    /// See [`LayoutBox::scrolls_own_text`].
    pub(crate) fn scrolls_own_text(&self, index: u32) -> bool {
        self.boxes[index as usize].scrolls_own_text
    }

    // ---------------------------------------------------------------------------------------
    // What `crate::layout::ifc` reads and writes. The inline formatting context is an algorithm
    // over this arena, so it lives beside the others in `layout/`, and these are its accessors
    // rather than a second copy of the arena's internals.
    // ---------------------------------------------------------------------------------------

    /// The inline content this box establishes a formatting context over, or `None`.
    pub(super) fn ifc_items(&self, index: u32) -> Box<[Item]> {
        match &self.boxes[index as usize].ifc {
            Some(items) => items.clone(),
            None => Box::new([]),
        }
    }

    /// Does this box establish an inline formatting context?
    pub(super) fn is_ifc(&self, index: u32) -> bool {
        self.boxes[index as usize].ifc.is_some()
    }

    /// Where this IFC's lines sit in the space they were given, and how far the first one starts in.
    pub(super) fn ifc_align(&self, index: u32) -> TextAlignKind {
        self.styles.align(self.boxes[index as usize].style)
    }

    pub(super) fn ifc_indent(&self, index: u32) -> f32 {
        self.styles.indent(self.boxes[index as usize].style)
    }

    /// The font a box's own text is measured in.
    pub(super) fn font_spec_of(&self, index: u32) -> FontSpec {
        self.styles.font(self.boxes[index as usize].style).clone()
    }

    pub(super) fn fonts_metrics_styled(&self, families: &str, bold: bool, italic: bool) -> &'static FontMetrics {
        self.fonts.metrics_styled(families, bold, italic)
    }

    /// [`BoxTree::fonts_metrics_styled`] at a `font-weight`, which picks among a web font's faces.
    pub(super) fn fonts_metrics_at(&self, families: &str, weight: u16, italic: bool, size_px: f32) -> &'static FontMetrics {
        self.fonts.metrics_at(families, weight, italic, size_px)
    }

    /// Which formatting context a box belongs to.
    pub(super) fn level_of(&self, index: u32) -> BoxLevel {
        self.boxes[index as usize].level
    }

    /// The fragments a box ended up as, each with the IFC box it is measured from. Empty for a
    /// block-level box, whose one rectangle is its `final_layout`.
    pub(super) fn fragments_of(&self, index: u32) -> &[(u32, Rect)] {
        &self.st[index as usize].fragments
    }

    /// The box whose inline formatting context lays box `index`'s line fragments out (the block its lines are in);
    /// `None` for a box with none.
    pub(crate) fn ifc_of(&self, index: u32) -> Option<u32> {
        self.fragments_of(index).first().map(|f| f.0)
    }

    /// How much an inline box's own margin, border and padding add on one side.
    ///
    /// The inline axis only. **Vertical** padding and border on an inline box do not change the
    /// line's height — Chrome keeps an 18px line box around a `<span>` with `padding: 10px` and a
    /// 3px border while reporting that span's own rect as 43 tall — so they are in the fragment's
    /// height (see [`BoxTree::inline_edge_block`]) and nowhere else.
    pub(super) fn inline_edge(&self, index: u32, start: bool) -> f32 {
        let style = self.style_of_box(index);
        if start {
            used_auto(style.margin.left) + used(style.border.left) + used(style.padding.left)
        } else {
            used_auto(style.margin.right) + used(style.border.right) + used(style.padding.right)
        }
    }

    /// The same on the block axis: what an inline box's own border and padding add above and below
    /// its content area. Chrome, on `<span style="padding:10px; border:3px solid">pad</span>`:
    /// a rect 43 tall (17 + 20 + 6) starting 13px above the line's content area, in an 18px line.
    pub(super) fn inline_edge_block(&self, index: u32) -> (f32, f32) {
        let style = self.style_of_box(index);
        (
            used(style.border.top) + used(style.padding.top),
            used(style.border.bottom) + used(style.padding.bottom),
        )
    }

    /// An atomic inline's margins: left, top, bottom, right -- a percentage, on either axis, of `basis`,
    /// its containing block's inline size (the block container the line is in), which a pass that is only
    /// measuring the line's intrinsic size gives as 0 (CSS Sizing 3 §5.2.1: a cyclic percentage margin
    /// contributes nothing). Chrome 150: `<img hspace="10%">` and `<input vspace="10%">` in a 400px block
    /// have 40px margins (`fixtures/presentational-hints`); every percentage read 0 here.
    pub(super) fn atomic_margins(&self, index: u32, basis: f32) -> (f32, f32, f32, f32) {
        let m = taffy::ResolveOrZero::resolve_or_zero(self.style_of_box(index).margin, Some(basis), crate::style::math::resolve_ptr);
        (m.left, m.top, m.bottom, m.right)
    }

    /// Size an atomic inline by re-entering taffy, shrink-to-fit.
    ///
    /// CSS's shrink-to-fit is `min(max(preferred minimum, available), preferred)`, and the two
    /// "preferred" numbers are exactly taffy's max-content and min-content sizes. Asking for the
    /// max-content width first and then laying out at the width that survives is what keeps an
    /// `<input>` from filling its paragraph the way a block child would.
    ///
    /// The box's own `width` decides which of them (CSS 2 §10.3.9, CSS Sizing 3 §3.2): a definite one --
    /// a length, a percentage of the line -- is what the probes already answered, and it is NEVER
    /// clamped to the line: an inline-block wider than its line overflows it. `auto` (spelt
    /// `fit-content` for an inline-block, see `layout::style`) and `fit-content` shrink to fit,
    /// `min-content`/`max-content` are those sizes, `stretch` fills the line. Chrome 150
    /// (`display:inline-block; width:fit-content; white-space:pre` text in a 0-wide box, the
    /// challenge's font probe): the text's width, 192.016; `width:100px` there, 100. This answered 0 for
    /// both -- it took `min(max-content, line)` and dropped the min-content floor.
    ///
    /// `basis` is what the box's percentages resolve against: its margins' (see [`BoxTree::atomic_margins`])
    /// and, for a percentage HEIGHT, the containing block's height when that is definite -- the block
    /// container the line is in, the anonymous box between them not counting (CSS 2 §10.1). Chrome 150:
    /// `<img height="50%">` in a 300px-tall block is 150px tall; it was 0 here, the line giving no height.
    pub(super) fn measure_atomic(
        &mut self,
        index: u32,
        run_mode: RunMode,
        limit: Option<f32>,
        basis: AtomicBasis,
    ) -> Size<f32> {
        let node = taffy::NodeId::from(index as usize);
        let probe = |space: AvailableSpace, known: Size<Option<f32>>, mode: RunMode| LayoutInput {
            run_mode: mode,
            sizing_mode: SizingMode::InherentSize,
            axis: RequestedAxis::Both,
            known_dimensions: known,
            known_dimensions_are_definite: Size { width: true, height: true },
            parent_size: Size { width: limit, height: basis.height },
            available_space: Size { width: space, height: AvailableSpace::MaxContent },
            vertical_margins_are_collapsible: Line::FALSE,
        };
        let max = LayoutPartialTree::compute_child_layout(
            self,
            node,
            probe(AvailableSpace::MaxContent, Size::NONE, RunMode::ComputeSize),
        )
        .size;
        let min_content = |tree: &mut Self| {
            LayoutPartialTree::compute_child_layout(tree, node, probe(AvailableSpace::MinContent, Size::NONE, RunMode::ComputeSize))
                .size
                .width
        };
        let (ml, _, _, mr) = self.atomic_margins(index, basis.margin);
        let available = limit.map(|w| (w - ml - mr).max(0.0));
        let width = match self.style_of_box(index).size.width.expand() {
            ExpandedDimension::Length(_) | ExpandedDimension::Percent(_) | ExpandedDimension::Calc(_) => max.width,
            ExpandedDimension::MaxContent => max.width,
            ExpandedDimension::MinContent => min_content(self),
            ExpandedDimension::Stretch => available.unwrap_or(max.width),
            // `auto`, `fit-content`: shrink-to-fit. The min-content probe only matters when the
            // max-content width does not fit.
            _ => match available {
                Some(avail) if max.width > avail => min_content(self).max(avail).min(max.width),
                _ => max.width,
            },
        };
        // Laid out, not measured, whatever the line is doing: the line reads the box's
        // baseline (`atomic_baseline`) off its children's places and its last line, which only a
        // laying-out call writes. A measuring call left the previous layout's there, so a line
        // measured before the box's first layout and one measured after it disagreed --
        // datatracker.ietf.org's sidebar measured 105.2 px, then 99.8 px, at one width -- and a
        // layout that keeps its caches between passes changed which of the two a page got.
        let out = LayoutPartialTree::compute_child_layout(
            self,
            node,
            probe(
                AvailableSpace::Definite(width),
                Size { width: Some(width), height: None },
                RunMode::PerformLayout,
            ),
        );
        // When the line is only measuring, that leaves the box laid out at the measuring width: see
        // `BoxTree::measuring` (en.wikipedia.org's pinned-menu labels, laid out 0 px wide by a
        // measure at 0).
        // taffy sets a node's own `unrounded` from its PARENT, and this box's parent is a line
        // rather than a taffy container, so the size, border and padding are recorded here and the
        // position by `commit_fragments` once the line's baseline is known.
        //
        // Only by a call that LAYS the box out: a measuring one (`RunMode::ComputeSize`)
        // computes no overflow, and had it come last the box would report an empty one. Laying every
        // pass out from empty caches, the last call is a laying-out one (the replay bank's 504 pages: no
        // box reports otherwise); a layout that keeps its caches between passes changes which call comes
        // last (www.freebsd.org's atomic inlines, 7 px).
        if run_mode == RunMode::PerformLayout {
            let style = self.style_of_box(index);
            let (border, padding) = (rect_of(style.border), rect_of(style.padding));
            let bars = self.scrollbar_gutter(index);
            self.touch(index as usize);
            let u = &mut self.st[index as usize].unrounded;
            u.scrollbar_size = bars;
            u.size = out.size;
            u.border = border;
            u.padding = padding;
            u.scrollable_overflow_rect = out.scrollable_overflow_rect;
        }
        out.size
    }

    /// Write one IFC pass's fragments onto the boxes that own them.
    ///
    /// An atomic inline is a real box, so its fragment becomes its taffy `Layout` and every
    /// geometry member reads it the way it reads a block's. Everything else keeps the list.
    /// `margin_basis` is what the pass resolved the atomic inlines' percentage margins against.
    pub(super) fn commit_fragments(&mut self, index: u32, fragments: Vec<(u32, Rect)>, margin_basis: f32) {
        self.st[index as usize].inline_right = fragments.iter().map(|(_, r)| r.x + r.width).fold(0.0, f32::max);
        for item in self.ifc_items(index).iter() {
            // Only THIS context's fragments: a box split around a block-level child keeps the ones
            // its other anonymous box produced, which a blanket clear would delete.
            self.touch(item.box_index() as usize);
            self.st[item.box_index() as usize].fragments.retain(|(ifc, _)| *ifc != index);
        }
        for (b, rect) in fragments {
            if self.boxes[b as usize].level == BoxLevel::AtomicInline {
                // its used margins too, which the line placed it by: `getComputedStyle` reads them
                // (Chrome 150: a checkbox's `margin` is `3px 3px 3px 4px`; it read `0px` here)
                let (left, top, bottom, right) = self.atomic_margins(b, margin_basis);
                self.touch(b as usize);
                let layout = &mut self.st[b as usize].unrounded;
                layout.margin = taffy::geometry::Rect { left, right, top, bottom };
                layout.location = Point { x: rect.x, y: rect.y };
                layout.size = Size { width: rect.width, height: rect.height };
                let copy = *layout;
                self.st[b as usize].final_layout = copy;
                continue;
            }
            // A text box carries its parent element's style, so its padding belongs to the element
            // and must not be added to the text's own fragment a second time.
            let (top, bottom) = match self.boxes[b as usize].text {
                Some(_) => (0.0, 0.0),
                None => self.inline_edge_block(b),
            };
            let grown = Rect {
                x: rect.x,
                y: rect.y - top,
                width: rect.width,
                height: rect.height + top + bottom,
            };
            self.touch(b as usize);
            self.st[b as usize].fragments.push((index, grown));
        }
        // In the contexts' tree order (their index), whatever order they were laid out in: a box split
        // around a block-level child has fragments from several, and a layout that keeps its caches
        // does not lay every one of them out again (www.wix.com). A stable sort keeps each
        // context's own line order.
        for item in self.ifc_items(index).iter() {
            let f = &mut self.st[item.box_index() as usize].fragments;
            if f.iter().any(|(ifc, _)| *ifc != index) {
                f.sort_by_key(|(ifc, _)| *ifc);
            }
        }
    }

    /// One text box's content, split into the pieces a line can be broken between.
    ///
    /// Collapsed text is words separated by single spaces, so the split is exactly that: a word is
    /// unbreakable, a space is both a break opportunity and the thing that disappears at a line's
    /// start. `pre` text is split at the author's own newlines and at nothing else.
    pub(super) fn text_atoms(&self, index: u32) -> std::sync::Arc<[TextAtom]> {
        let style = self.boxes[index as usize].style;
        if let Some(Some((s, atoms))) = self.atoms.borrow().get(index as usize) {
            if *s == style {
                return atoms.clone();
            }
        }
        let atoms: std::sync::Arc<[TextAtom]> = self.measure_text_atoms(index).into();
        let mut cache = self.atoms.borrow_mut();
        if cache.len() <= index as usize {
            cache.resize(index as usize + 1, None);
        }
        cache[index as usize] = Some((style, atoms.clone()));
        atoms
    }

    /// [`BoxTree::text_atoms`], measured.
    fn measure_text_atoms(&self, index: u32) -> Vec<TextAtom> {
        let b = &self.boxes[index as usize];
        let Some(text) = b.text.as_deref() else { return Vec::new() };
        let spec = self.styles.font(b.style);
        let metrics = self.fonts.metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
        // U+00A0 renders as a space and advances like one, but the metrics table covers printable
        // ASCII only, so `FontMetrics::advance` would fall back to the DIGIT advance for it — 8.896
        // at 16px Arial where Chrome measures 4.45. Substituting here keeps that fallback for the
        // characters it is actually meant for. (The same substitution belongs in `font::measure`,
        // so `ctx.measureText("\u{a0}")` stops answering a digit's width; that file is another
        // unit's, and this unit's report names it.)
        let width_of = |s: &str| {
            if s.contains('\u{a0}') {
                spec.advance_of(metrics, &s.replace('\u{a0}', " "))
            } else {
                spec.advance_of(metrics, s)
            }
        };
        let mut atoms = Vec::new();
        if spec.preserve {
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    atoms.push(TextAtom { width: 0.0, space: false, wrappable: false, forces_break: true });
                }
                if line.is_empty() {
                    continue;
                }
                if !spec.wrap {
                    atoms.push(TextAtom { width: width_of(line), space: false, wrappable: false, forces_break: false });
                    continue;
                }
                // `pre-wrap` / `break-spaces`: every space kept, and a line may break after each (its leading ones
                // are the author's indent and stay)
                let mut seen_word = false;
                for (body, space) in preserved_pieces(line) {
                    if !body.is_empty() {
                        seen_word = true;
                        atoms.push(TextAtom { width: width_of(body), space: false, wrappable: true, forces_break: false });
                    }
                    if space {
                        atoms.push(TextAtom { width: width_of(" "), space: seen_word, wrappable: true, forces_break: false });
                    }
                }
            }
            return atoms;
        }
        for piece in text.split_inclusive(' ') {
            let word = piece.strip_suffix(' ');
            let body = word.unwrap_or(piece);
            if !body.is_empty() {
                atoms.push(TextAtom {
                    width: width_of(body),
                    space: false,
                    wrappable: spec.wrap,
                    forces_break: false,
                });
            }
            if word.is_some() {
                atoms.push(TextAtom {
                    width: width_of(" "),
                    space: true,
                    wrappable: spec.wrap,
                    forces_break: false,
                });
            }
        }
        atoms
    }

    /// The text of each of [`BoxTree::text_atoms`], atom for atom: a word, a space, a preserved line,
    /// or `"\n"` for a forced break. The same split, so index `i` here is atom `i` there.
    pub(super) fn text_atom_strings(&self, index: u32) -> Vec<String> {
        let b = &self.boxes[index as usize];
        let Some(text) = b.text.as_deref() else { return Vec::new() };
        let mut out = Vec::new();
        let spec = self.styles.font(b.style);
        if spec.preserve {
            for (i, line) in text.split('\n').enumerate() {
                if i > 0 {
                    out.push("\n".to_string());
                }
                if line.is_empty() {
                    continue;
                }
                if !spec.wrap {
                    out.push(line.to_string());
                    continue;
                }
                for (body, space) in preserved_pieces(line) {
                    if !body.is_empty() {
                        out.push(body.to_string());
                    }
                    if space {
                        out.push(" ".to_string());
                    }
                }
            }
            return out;
        }
        for piece in text.split_inclusive(' ') {
            let word = piece.strip_suffix(' ');
            let body = word.unwrap_or(piece);
            if !body.is_empty() {
                out.push(body.to_string());
            }
            if word.is_some() {
                out.push(" ".to_string());
            }
        }
        out
    }

    /// How far `s` advances in text box `index`'s font -- the measure [`BoxTree::text_atoms`] uses.
    pub(super) fn text_advance(&self, index: u32, s: &str) -> f32 {
        let spec = self.styles.font(self.boxes[index as usize].style);
        let metrics = self.fonts.metrics_at(&spec.families, spec.weight, spec.italic, spec.size);
        spec.advance_of(metrics, &s.replace('\u{a0}', " "))
    }

    /// Is text box `index` laid out with its white space preserved?
    pub(super) fn text_preserved(&self, index: u32) -> bool {
        self.styles.font(self.boxes[index as usize].style).preserve
    }

    /// The outermost box — `<html>`'s — or `None` for a document that generated none at all.
    ///
    /// It is index 0 because [`BoxTree::build`] pushes the document element's box first and every
    /// later box is pushed after its container.
    pub fn root_box(&self) -> Option<u32> {
        (!self.boxes.is_empty()).then_some(0)
    }

    /// The element a box came from.
    /// The metrics of the font a `font-family` list picks, as layout measured its text with them: where the paint walk
    /// puts a text decoration from the baseline.
    pub(crate) fn font_metrics(&self, families: &str, bold: bool, italic: bool) -> &'static crate::font::FontMetrics {
        self.fonts.metrics_styled(families, bold, italic)
    }

    /// [`BoxTree::font_metrics`] at a `font-weight`, which picks among a web font's faces.
    pub(crate) fn font_metrics_at(&self, families: &str, weight: u16, italic: bool, size_px: f32) -> &'static crate::font::FontMetrics {
        self.fonts.metrics_at(families, weight, italic, size_px)
    }

    pub fn node_of(&self, index: u32) -> NodeId {
        self.boxes[index as usize].node
    }

    /// Can a point land on this box?
    ///
    /// `visibility: hidden` and `pointer-events: none` both keep an element out of a hit test while
    /// leaving it in the layout -- it still takes up space, it just cannot be pointed at. That is
    /// the difference between them and `display: none`, which has no box to test in the first place.
    pub fn is_hit_testable(&self, index: u32) -> bool {
        let b = &self.boxes[index as usize];
        let style = self.styles.get(b.style);
        style.display != Display::None && self.styles.hittable(b.style)
    }

    /// A box's border-box rect after layout, relative to its parent.
    pub fn layout(&self, index: u32) -> &Layout {
        &self.st[index as usize].final_layout
    }

    /// Box `index`'s part in a table.
    pub(super) fn table_role(&self, index: u32) -> crate::layout::style::TableRole {
        self.styles.table(self.boxes[index as usize].style).role
    }

    /// Box `index`'s table properties.
    pub(super) fn table_info(&self, index: u32) -> crate::layout::style::TableInfo {
        self.styles.table(self.boxes[index as usize].style)
    }

    /// A text box's text.
    pub(super) fn text_of(&self, index: u32) -> Option<&str> {
        self.boxes[index as usize].text.as_deref()
    }

    /// A cell's `(colspan, rowspan)`.
    pub(super) fn spans_of(&self, index: u32) -> (u16, u16) {
        self.boxes[index as usize].spans
    }

    /// Move every child of box `index` down by `dy` -- a table cell's content to its `vertical-align`.
    /// Move cell `index`'s children down to `dy` below where its layout put them. Only the difference from the shift
    /// they already carry is applied: a cell whose layout came from its cache keeps its children where the last pass
    /// left them -- shifted -- and reCAPTCHA's label, shifted again, sat 57px down where Chrome centres it at 29.
    pub(super) fn shift_children(&mut self, index: u32, dy: f32) {
        let delta = dy - self.st[index as usize].valign_shift;
        self.st[index as usize].valign_shift = dy;
        if delta == 0.0 {
            return;
        }
        let (start, end) = self.boxes[index as usize].children;
        for i in start..end {
            let c = self.child_list[i as usize] as usize;
            self.touch(c);
            self.st[c].unrounded.location.y += delta;
        }
    }

    /// Box `index`'s scrollable overflow rect, in taffy's terms (from its padding edge, as its children contribute it),
    /// with every transform below it taken: a transformed box contributes its TRANSFORMED rect, as Chrome's does --
    /// `left: 50%; transform: translateX(-50%)` centres a wide box without widening the page (minea's 1205px-wide
    /// centred banner made its page 1838 wide here, and a horizontal scrollbar showed). taffy laid the boxes out
    /// untransformed; the transforms are the kept effects' (`set_effects`), else -- while the layout itself asks, before
    /// any are kept -- each style's own ([`BoxTree::style_transform`]).
    pub(crate) fn overflow_of(&self, index: u32) -> taffy::geometry::Rect<f32> {
        let own = self.st[index as usize].final_layout.scrollable_overflow_rect;
        let made = match self.effects() {
            Some(fx) if fx.any_transform => self.overflow_fx.get_or_init(|| self.transformed_overflows(&|b| fx.local(b))),
            Some(_) => return own,
            None if self.styles.any_transform() => self.overflow_fx.get_or_init(|| self.transformed_overflows(&|b| self.style_transform(b))),
            None => return own,
        };
        made.get(index as usize).copied().flatten().unwrap_or(own)
    }

    /// Box `index`'s own transform from its style, at its laid-out size: what the paint's effects will hold for it
    /// (`Page::box_effects`). A non-replaced inline box is not transformable.
    fn style_transform(&self, index: u32) -> Option<crate::effects::Affine> {
        let b = &self.boxes[index as usize];
        let source = self.styles.transform_source(b.style)?;
        if b.level == BoxLevel::Inline {
            return None;
        }
        let l = &self.st[index as usize].final_layout;
        source.local((l.size.width as f64, l.size.height as f64), (self.viewport.width as f64, self.viewport.height as f64))
    }

    /// [`BoxTree::overflow_of`] for every box whose overflow a transform below it changes, children first: a box's
    /// overflow is made again from its children's contributions (taffy's `compute_scrollable_overflow_contribution`,
    /// a transformed child's mapped through its transform), each edge taffy's children set taken from them now and an
    /// edge something else set (a scroll container's end padding) kept.
    fn transformed_overflows(&self, local: &dyn Fn(u32) -> Option<crate::effects::Affine>) -> Vec<Option<taffy::geometry::Rect<f32>>> {
        use taffy::geometry::Rect;
        let n = self.boxes.len();
        let mut over: Vec<Option<Rect<f32>>> = vec![None; n];
        let mut moved = vec![false; n];
        let transform = |b: usize| local(b as u32).filter(|m| !m.is_identity());
        let union = |a: Rect<f32>, b: Rect<f32>| Rect { left: a.left.min(b.left), right: a.right.max(b.right), top: a.top.min(b.top), bottom: a.bottom.max(b.bottom) };
        // a child's contribution from the parent's padding edge: its border box and, where its overflow lets it
        // escape, its own overflow; through its transform when it has one
        let contribution = |loc: (f32, f32), c: usize, own: Rect<f32>, parent_scrolls: bool, m: Option<crate::effects::Affine>| -> Rect<f32> {
            let l = &self.st[c].final_layout;
            let s = self.style_of_box(c as u32);
            let scrolls = s.overflow.x.is_scroll_container() || s.overflow.y.is_scroll_container();
            let contained = s.contain.contains_scrollable_overflow();
            let (px, py) = (!scrolls && !contained && s.overflow.x == Overflow::Visible, !scrolls && !contained && s.overflow.y == Overflow::Visible);
            let (w, h) = (if px { l.size.width.max(own.right) } else { l.size.width }, if py { l.size.height.max(own.bottom) } else { l.size.height });
            if w <= 0.0 || h <= 0.0 {
                return Rect::ZERO;
            }
            let (sx, sy) = (if px { own.left.min(0.0) } else { 0.0 }, if py { own.top.min(0.0) } else { 0.0 });
            let mut r = Rect { left: loc.0 + sx, right: loc.0 + w, top: loc.1 + sy, bottom: loc.1 + h };
            if let Some(m) = m {
                let m = m.about(loc.0 as f64, loc.1 as f64);
                let corners = [(r.left, r.top), (r.right, r.top), (r.left, r.bottom), (r.right, r.bottom)].map(|(x, y)| m.apply(x as f64, y as f64));
                let xs = corners.map(|p| p.0 as f32);
                let ys = corners.map(|p| p.1 as f32);
                r = Rect {
                    left: xs.iter().copied().fold(f32::INFINITY, f32::min),
                    right: xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                    top: ys.iter().copied().fold(f32::INFINITY, f32::min),
                    bottom: ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                };
            }
            if parent_scrolls && (r.right <= 0.0 || r.bottom <= 0.0) {
                return Rect::ZERO;
            }
            r
        };
        // children before parents, iteratively (a document can be deeper than the stack)
        let mut state = vec![0u8; n];
        for start in 0..n {
            if state[start] != 0 {
                continue;
            }
            let mut stack = vec![start];
            while let Some(&b) = stack.last() {
                if state[b] == 0 {
                    state[b] = 1;
                    let (from, to) = self.boxes[b].children;
                    stack.extend(self.child_list[from as usize..to as usize].iter().map(|&c| c as usize).filter(|&c| state[c] == 0));
                    continue;
                }
                stack.pop();
                if state[b] == 2 {
                    continue;
                }
                state[b] = 2;
                let (from, to) = self.boxes[b].children;
                let kids = &self.child_list[from as usize..to as usize];
                let l = &self.st[b].final_layout;
                let s = self.style_of_box(b as u32);
                if kids.iter().any(|&c| moved[c as usize]) && s.direction != taffy::style::Direction::Rtl {
                    let scrolls = s.overflow.x.is_scroll_container() || s.overflow.y.is_scroll_container();
                    let (mut then, mut now) = (Rect::ZERO, Rect::ZERO);
                    for &c in kids {
                        let c = c as usize;
                        let cl = &self.st[c].final_layout;
                        let loc = (cl.location.x - l.border.left, cl.location.y - l.border.top);
                        then = union(then, contribution(loc, c, cl.scrollable_overflow_rect, scrolls, None));
                        now = union(now, contribution(loc, c, over[c].unwrap_or(cl.scrollable_overflow_rect), scrolls, transform(c)));
                    }
                    let o = l.scrollable_overflow_rect;
                    let edge = |o: f32, then: f32, now: f32, far: bool| match ((o - then).abs() < 0.01, far) {
                        (true, _) => now,
                        (false, true) => o.max(now),
                        (false, false) => o.min(now),
                    };
                    let made = Rect {
                        left: edge(o.left, then.left, now.left, false),
                        right: edge(o.right, then.right, now.right, true),
                        top: edge(o.top, then.top, now.top, false),
                        bottom: edge(o.bottom, then.bottom, now.bottom, true),
                    };
                    if made != o {
                        over[b] = Some(made);
                    }
                }
                moved[b] = transform(b).is_some() || over[b].is_some();
            }
        }
        over
    }

    /// Box `index`'s layout as the pass running now left it, before snapping: relative to its parent.
    pub(super) fn unrounded_of(&self, index: u32) -> &Layout {
        &self.st[index as usize].unrounded
    }

    /// Box `index`'s children, as box indices.
    pub(super) fn children_of(&self, index: u32) -> Vec<u32> {
        let (start, end) = self.boxes[index as usize].children;
        self.child_list[start as usize..end as usize].to_vec()
    }

    /// Box `index`'s `vertical-align`.
    pub(super) fn valign_of(&self, index: u32) -> crate::layout::style::VAlign {
        self.styles.valign(self.boxes[index as usize].style)
    }

    /// Box `index`'s level (block, inline, atomic inline).
    pub(crate) fn box_level(&self, index: u32) -> BoxLevel {
        self.boxes[index as usize].level
    }

    /// Does box `index`'s style transform it?
    pub(crate) fn box_transformed(&self, index: u32) -> bool {
        self.styles.transformed(self.boxes[index as usize].style)
    }

    /// The box of the root's first `<body>` child.
    pub(crate) fn body_node(&self) -> Option<NodeId> {
        self.body_node
    }

    /// The boxes the initial containing block lays out besides `<html>` (see `Icb`), as a range of
    /// box indices to read through [`BoxTree::icb_child`].
    pub(crate) fn icb_children(&self) -> (u32, u32) {
        let (start, end) = self.icb.children;
        (start.saturating_add(1).min(end), end)
    }

    /// The `i`th entry of [`BoxTree::icb_children`]'s range.
    pub(crate) fn icb_child(&self, i: u32) -> u32 {
        self.child_list[i as usize]
    }

    /// The containing box, or `None` for the document element.
    pub fn parent_of(&self, index: u32) -> Option<u32> {
        self.boxes[index as usize].parent
    }

    /// The layout style of a box — taffy's view of it, shared with every box that styles the same
    /// unless this one's percentage paddings have been resolved, in which case it is its own.
    ///
    /// Not while it is read by a measurement of the size its percentages are a percentage of: there the shared
    /// style's percentages, which taffy resolves against the basis it has not been given, contribute nothing
    /// (CSS Sizing 3 §5.2.1: a cyclic percentage padding resolves against zero for intrinsic contributions). The
    /// resolved one counted the padding the last pass gave the box, which grew the shrink-to-fit width it is a
    /// percentage of, pass after pass: `display:inline-block` around `<div><div style="padding-left:25%">beta gamma
    /// and kappa` was 195.95 wide from nothing and 196.14 after a relayout -- Chrome 152.0.7977.82 is 147.11, the
    /// text's own width, the padding then taking a quarter of it and the text two lines (2026-10-23).
    pub fn style_of_box(&self, index: u32) -> &Style {
        let s = &self.st[index as usize];
        match &s.padding_override {
            Some(style) if !self.padding_basis_measured(index, s.padding_item) => style,
            _ => self.styles.get(self.boxes[index as usize].style),
        }
    }

    /// Is the innermost computing call measuring the width box `index`'s percentage paddings are a percentage of? The
    /// box's own measurement with no containing-block width given -- taffy's block algorithm sizing a container from
    /// its content measures each child so, a line measuring its max-content width each atomic inline, a table each
    /// cell's column widths -- or its container's measurement of its own width from its content, reading the boxes it
    /// lays out: its children, and the inline boxes of its inline formatting context however deep in spans.
    /// A flex or grid item (`item`, see `BoxState::padding_item`) asks only whether its container is measuring its own
    /// inner width from its content.
    fn padding_basis_measured(&self, index: u32, item: bool) -> bool {
        if item {
            return self.container_measured(index);
        }
        let Some(&(node, own, content)) = self.sizing.last() else {
            return false;
        };
        if node == index {
            return own;
        }
        if !content {
            return false;
        }
        let mut up = self.boxes[index as usize].parent;
        while let Some(p) = up {
            if p == node {
                return true;
            }
            if self.boxes[p as usize].level != BoxLevel::Inline {
                return false;
            }
            up = self.boxes[p as usize].parent;
        }
        false
    }

    /// Is flex or grid item `index`'s container measuring its own inner width from its content: the computing call below
    /// the item's own, or the innermost when the item is not computing (the container reading its items' styles, or
    /// asking for an item's cached answer)?
    fn container_measured(&self, index: u32) -> bool {
        let Some(parent) = self.boxes[index as usize].parent else {
            return false;
        };
        let mut calls = self.sizing.iter().rev();
        let mut call = calls.next();
        if call.is_some_and(|&(node, ..)| node == index) {
            call = calls.next();
        }
        call.is_some_and(|&(node, _, content)| node == parent && content)
    }

    /// A flex or grid item read through its shared style while its container measures its width from its content
    /// (`BoxState::padding_item`) neither answers from its cache nor stores in it: taffy hands an item the same input
    /// there as in a container of a definite width -- a flex base size is measured under `max-content` with no width
    /// either way -- where the item's padding differs, so an answer kept for one would be handed to the other.
    fn measured_apart(&self, index: usize) -> bool {
        let s = &self.st[index];
        s.padding_item && s.padding_override.is_some() && self.container_measured(index as u32)
    }

    /// Does flex or grid container `node`, called under `inputs` with no known width, take one from its own style --
    /// taffy's `styled_based_known_dimensions` (flexbox) and `outer_node_size` (grid): a `width` that resolves (under
    /// `SizingMode::InherentSize`; through `aspect-ratio` from a height too), or, in flexbox, a `min-width` that a no
    /// larger `max-width` caps? Its items are then laid out in a definite inner width even while it is measured, and
    /// count their percentage paddings against it. Any other box: no.
    fn width_from_style(&self, node: usize, inputs: &LayoutInput) -> bool {
        let style = self.styles.get(self.boxes[node].style);
        let flex = match style.display {
            Display::Flex => true,
            Display::Grid => false,
            _ => return false,
        };
        let calc = crate::style::math::resolve_ptr;
        let size: Size<Option<f32>> = taffy::MaybeResolve::maybe_resolve(style.size, inputs.parent_size, calc);
        if inputs.sizing_mode == SizingMode::InherentSize && size.maybe_apply_aspect_ratio(style.aspect_ratio).width.is_some() {
            return true;
        }
        let min: Size<Option<f32>> = taffy::MaybeResolve::maybe_resolve(style.min_size, inputs.parent_size, calc);
        let max: Size<Option<f32>> = taffy::MaybeResolve::maybe_resolve(style.max_size, inputs.parent_size, calc);
        let (min, max) = (min.maybe_apply_aspect_ratio(style.aspect_ratio).width, max.maybe_apply_aspect_ratio(style.aspect_ratio).width);
        flex && matches!((min, max), (Some(min), Some(max)) if max <= min)
    }

    /// See `BoxState::padding_item`: is box `index`'s container -- its parent, past the inline boxes an inline box is
    /// in -- a flex or grid container?
    fn padding_item(&self, index: u32) -> bool {
        let mut up = self.boxes[index as usize].parent;
        while let Some(p) = up {
            if self.boxes[p as usize].level != BoxLevel::Inline {
                break;
            }
            up = self.boxes[p as usize].parent;
        }
        up.is_some_and(|p| matches!(self.styles.get(self.boxes[p as usize].style).display, Display::Flex | Display::Grid))
    }

    /// The CSS `position` keyword of a box. See [`CssPosition`] for why it is not `Style::position`.
    pub fn css_position(&self, index: u32) -> CssPosition {
        self.styles.position(self.boxes[index as usize].style)
    }

    /// The viewport the document was laid out into: the initial containing block.
    pub fn viewport(&self) -> Size<f32> {
        self.viewport
    }

    /// Lay the document out into `viewport`.
    ///
    /// The root is given the viewport as *definite* available space rather than as a size, which is
    /// the difference between "the page is this wide" and "the root element is this wide" — a root
    /// with `width: 50%` has to come out half the viewport, not all of it.
    /// [`BoxTree::compute`] with every pass laid out from empty caches (the reference for I4).
    /// Lay the viewport out with no scrollbars whatever its content (an `<iframe scrolling=no>`'s document).
    pub fn set_viewport_bars_off(&mut self, off: bool) {
        self.viewport_bars_off = off;
    }

    pub fn compute_from_nothing(&mut self, viewport: Size<f32>) {
        self.exhaustive = true;
        self.compute(viewport);
    }

    /// What the last [`BoxTree::compute`] did.
    pub fn stats(&self) -> LayoutStats {
        self.stats
    }

    /// How many states a box keeps: two after a compute that kept the full-window pass's aside, else one.
    #[cfg(test)]
    pub(crate) fn states_kept(&self) -> usize {
        if !self.boxes.is_empty() && self.alt.len() == self.boxes.len() { 2 } else { 1 }
    }

    /// Every box's state, as nothing carried leaves it -- empty, at its built origin -- unless the states are made.
    fn ensure_states(&mut self) {
        if self.st.len() != self.boxes.len() {
            self.st = self.built.iter().map(|&origin| BoxState { origin, ..BoxState::default() }).collect();
        }
    }

    pub fn compute(&mut self, viewport: Size<f32>) {
        self.ensure_states();
        self.stats = LayoutStats::default();
        self.requested = Some(viewport);
        self.viewport = viewport;
        self.viewport_bars = Point { x: false, y: false };
        if self.boxes.is_empty() {
            return;
        }
        // A second compute of one tree starts where the first left it -- its final pass's state -- as one state,
        // which is the code before two states.
        if self.live_final {
            self.alt = Vec::new();
            self.alt_maps = PassMaps::default();
            self.live_final = false;
        }
        if self.mode == korvus_dom::DocumentMode::Quirks {
            self.fill_viewport_quirk(viewport);
        }
        self.scan_lists();
        self.sync_origins();
        self.lay_out(viewport);
        // CONTENT-VISIBILITY: AUTO. Every such box starts skipped; one whose box comes
        // within Blink's relevance margin of the viewport -- 150% of the viewport's size, applied to
        // the box (`display_lock_document_state.cc`, kViewportMarginPercentage) -- is unlocked and the
        // page laid out again, until nothing more comes into range (unlocking can grow a box and push
        // its neighbours out, so it settles rather than flipping back).
        //
        // WHEN it unlocks is not Chrome's: Blink decides relevance at a rendering update, after that frame's
        // animation frame callbacks, and a box keeps its state between updates, where this decides it at every
        // layout. A read before the document's first rendering update -- a parsing script, DOMContentLoaded, load,
        // a timer after load, the first rAF -- sees a box skipped in Chrome 152.0.7977.82 and unlocked here: three
        // boxes in a 300px block (`contain-intrinsic-size: auto 100px`; `padding: 4% 2%`, `10px`, none) read 124,
        // 120, 100 there until the second rAF and 40, 36, 16 from it, and 40, 36, 16 here throughout (2026-10-23).
        // Keeping them skipped until then needs the frame task (korvus-webapi's `cb_frame`) to say when one ran.
        let mut cv_ran = false;
        for _ in 0..CV_PASSES {
            if !self.unlock_relevant(viewport) {
                break;
            }
            cv_ran = true;
            crate::perf_counters::add(&crate::perf_counters::EXTRA_PASSES_CV, 1);
            for s in &mut self.st {
                s.cache.clear();
            }
            self.scan_lists();
            self.sync_origins();
            self.lay_out(viewport);
        }
        // SCROLLBARS. A classic scrollbar is not known until the content has been laid
        // out, and showing one moves the content -- so, as Blink does, lay out, show the scrollbars
        // the result calls for, and lay out again. A page with no `auto` box that overflows and a
        // document that fits (or a theme with no classic scrollbar) stops after the first check.
        //
        // The FIRST such pass lays out from a state of its own (`enter_final_pass`): the full-window pass's state is
        // kept aside for the next layout of the document to carry, and this one starts from the final states carried
        // from the last layout's final pass. With one state, each relayout of a page taller than its window laid
        // everything out twice -- at the window's width, then at the width less the bar, each evicting the other's
        // answers -- and resolved every viewport-relative percentage padding at both. The reference from nothing
        // (`exhaustive`), a page that unlocked `content-visibility: auto` boxes, and the switch off keep one state.
        //
        // So does a FIRST layout, which nothing was carried into (`relaid`): its full-window state would be a copy of
        // every box's state kept for a relayout that may never come. Copying them -- 672 bytes a box, its cached
        // measurements cloned -- made the first layout of a page of 16,000 `<div>`s 35 % slower than one state (50 ->
        // 68 ms) and its peak RSS 31 % larger (143 -> 188 MB; 2026-10-23). The first relayout carries the one state as
        // before, and keeps two from then on.
        let mut split = !self.exhaustive && !cv_ran && knob(Knob::TwoStates) && self.relaid;
        for _ in 0..SCROLLBAR_PASSES {
            let reveals = self.overflowing_scrollbars();
            let layout_viewport = self.layout_viewport(viewport);
            if reveals.is_empty() && layout_viewport == self.viewport {
                break;
            }
            self.viewport = layout_viewport;
            crate::perf_counters::add(&crate::perf_counters::EXTRA_PASSES_SCROLLBAR, 1);
            if std::mem::take(&mut split) {
                self.enter_final_pass(&reveals);
            } else {
                self.show_scrollbar_styles(&reveals);
                self.show_scrollbar_overrides(&reveals);
                // Only what the scrollbars moved forgets its layout; a narrower viewport is a
                // different constraint at the root, which the caches are keyed on.
                if self.exhaustive {
                    self.st.iter_mut().for_each(|s| { s.cache.clear(); });
                }
                for &(index, _) in &reveals {
                    self.clear_up(index);
                }
            }
            self.sync_origins();
            self.lay_out(layout_viewport);
        }
        // made from the last pass's boxes, not those a viewport decision read
        self.overflow_fx = std::cell::OnceCell::new();
    }

    /// Make the lists the passes' whole-tree scans walk (see `pct_boxes`): one scan here instead of one per pass.
    /// The scrollbar variants a pass shows move neither padding nor position, and take `auto` away rather than add it.
    fn scan_lists(&mut self) {
        let (mut pct, mut auto, mut abs) = (std::mem::take(&mut self.pct_boxes), std::mem::take(&mut self.auto_boxes), std::mem::take(&mut self.abs_boxes));
        pct.clear();
        auto.clear();
        abs.clear();
        if knob(Knob::Lists) {
            for (i, b) in self.boxes.iter().enumerate() {
                if b.text.is_none() && self.styles.has_percentage_padding(b.style) {
                    pct.push(i as u32);
                }
                let a = self.styles.auto_scroll(b.style);
                if i > 0 && (a.x || a.y) && self.styles.level(b.style) != BoxLevel::Inline {
                    auto.push(i as u32);
                }
                if b.static_flow.is_some() {
                    abs.push(i as u32);
                }
            }
        } else {
            pct.extend(0..self.boxes.len() as u32);
            auto.extend(1..self.boxes.len() as u32);
            abs.extend(0..self.boxes.len() as u32);
        }
        (self.pct_boxes, self.auto_boxes, self.abs_boxes) = (pct, auto, abs);
    }

    /// Note that the pass running now wrote box `index`'s layout or fragments: its snapped copy is owed.
    #[inline]
    fn touch(&mut self, index: usize) {
        let s = &mut self.st[index];
        if !s.touched {
            s.touched = true;
            self.touched.push(index as u32);
        }
    }

    /// Before a pass: every box whose style moved since its state was computed (`BoxState::origin`) forgets that
    /// state's answers, and the boxes laid out around it theirs ([`BoxTree::clear_up_layers`]). The explicit clears
    /// (a revealed scrollbar, a content-visibility unlock) already cover what the passes of one compute move; this
    /// is what makes a state carried from another tree -- computed under another style, if the box's has moved since
    /// -- safe to answer from.
    ///
    /// A state computed under another style -- carried from another tree, or from this one before a patch moved its
    /// styles (a scrollbar variant this tree has not shown yet, a class the patch replaced), or this tree's own before a
    /// `content-visibility: auto` box was unlocked -- has its resolved-padding style made from that one, so it is made
    /// again from the box's style as it is, at the same basis -- what resolving it under this style would have given --
    /// or dropped when this style has no percentage padding, as a box without one never had one. An unlocked box kept
    /// the skipped style's: `<p style="content-visibility:auto; contain-intrinsic-size:auto 100px; padding:4% 2%">` in
    /// a 300px block stayed 124 tall, its intrinsic 100 and the padding, where Chrome 152.0.7977.82 lays it out at 40
    /// once it is relevant (2026-10-23).
    fn sync_origins(&mut self) {
        for i in 0..self.boxes.len() {
            let style = self.boxes[i].style;
            let origin = self.styles.origin(style);
            let s = &mut self.st[i];
            if s.origin != origin {
                if s.padding_override.is_some() {
                    if self.styles.has_percentage_padding(style) {
                        s.padding_override = Some(Box::new(crate::layout::style::with_resolved_padding(self.styles.get(style), s.padding_basis)));
                    } else {
                        // a style patched in place that has no percentage padding has none to resolve
                        s.padding_override = None;
                        s.padding_basis = f32::NAN;
                        s.padding_item = false;
                    }
                }
                s.origin = origin;
                s.inherited = false;
                self.clear_up_layers(i as u32);
            } else {
                s.inherited = false;
            }
        }
    }

    /// Switch from the full-window pass's state to the final pass's, which the scrollbars `reveals` start (see
    /// `compute`). The full-window state goes to `alt`, for the next layout of the document to carry.
    ///
    /// The final state of each box is the one carried from the last layout's final pass when there is one and it was
    /// computed under the box's style as it is now (the scrollbars shown) and with the padding this pass resolved
    /// (else the box and the boxes laid out around it forget theirs); any other box starts from the full-window
    /// state, which is where the single state started. A box starting from that state with a carried box below it --
    /// through its children or its inline formatting context's items -- does not answer a laying-out call from it:
    /// what it laid out below it there is not what is there now.
    fn enter_final_pass(&mut self, reveals: &[(u32, Vec<(Variant, bool)>)]) {
        self.show_scrollbar_styles(reveals);
        let n = self.boxes.len();
        let mut invalid: Vec<u32> = Vec::new();
        if self.alt.len() != n {
            self.alt = self.st.clone();
            self.alt_maps = self.maps.clone();
            for s in &mut self.alt {
                s.carried = false;
            }
        } else {
            for i in 0..n {
                let carried = self.alt[i].carried;
                if carried && self.alt[i].origin == self.styles.origin(self.boxes[i].style) {
                    let padding = |s: &BoxState| (s.padding_basis.to_bits(), s.padding_override.is_some(), s.padding_item);
                    if padding(&self.alt[i]) != padding(&self.st[i]) {
                        self.alt[i].padding_override = self.st[i].padding_override.clone();
                        self.alt[i].padding_basis = self.st[i].padding_basis;
                        self.alt[i].padding_item = self.st[i].padding_item;
                        invalid.push(i as u32);
                    }
                    self.stats.states_reused += 1;
                    continue;
                }
                if carried {
                    // computed under another style: as a box whose style moved, its chain forgets
                    invalid.push(i as u32);
                }
                self.alt[i] = self.st[i].clone();
                self.alt[i].carried = false;
                let key = i as u32;
                match self.maps.last_baselines.get(&key) {
                    Some(&b) => self.alt_maps.last_baselines.insert(key, b),
                    None => self.alt_maps.last_baselines.remove(&key),
                };
                match self.maps.text_lines.get(&key) {
                    Some(l) => self.alt_maps.text_lines.insert(key, l.clone()),
                    None => self.alt_maps.text_lines.remove(&key),
                };
            }
            crate::perf_counters::add(&crate::perf_counters::SLOT_STATES_REUSED, self.stats.states_reused);
            let reach = self.reaches_carried(&self.alt);
            for i in 0..n {
                if reach[i] && !self.alt[i].carried {
                    self.alt[i].final_stale = true;
                    self.stats.final_stale_marked += 1;
                }
            }
        }
        std::mem::swap(&mut self.st, &mut self.alt);
        std::mem::swap(&mut self.maps, &mut self.alt_maps);
        self.live_final = true;
        self.show_scrollbar_overrides(reveals);
        for &(index, _) in reveals {
            if self.st[index as usize].carried {
                // Its own answers and its carried ancestors' were computed with its scrollbar shown; the full-window
                // states above it measured it without.
                let mut up = self.boxes[index as usize].parent;
                while let Some(p) = up {
                    if !self.st[p as usize].carried {
                        self.st[p as usize].cache.clear();
                    }
                    up = self.boxes[p as usize].parent;
                }
            } else {
                self.clear_up(index);
            }
        }
        for i in invalid {
            self.clear_up_layers(i);
        }
    }

    /// Which boxes have a carried state in `states` at or below them, through what each lays out
    /// ([`BoxTree::laid_out_by`]: its children and its inline formatting context's items). Post-order from the root.
    fn reaches_carried(&self, states: &[BoxState]) -> Vec<bool> {
        let n = self.boxes.len();
        let mut reach = vec![false; n];
        let mut done = vec![false; n];
        let (s, e) = self.icb.children;
        let roots: Vec<u32> = self.child_list.get(s as usize..e as usize).map(|r| r.to_vec()).unwrap_or_default();
        for root in roots {
            let mut stack: Vec<(u32, bool)> = vec![(root, false)];
            while let Some((i, expanded)) = stack.pop() {
                if done[i as usize] {
                    continue;
                }
                if !expanded {
                    stack.push((i, true));
                    for c in self.laid_out_by(i) {
                        if !done[c as usize] {
                            stack.push((c, false));
                        }
                    }
                    continue;
                }
                reach[i as usize] = states[i as usize].carried || self.laid_out_by(i).any(|c| reach[c as usize]);
                done[i as usize] = true;
            }
        }
        reach
    }

    /// THE HTML AND BODY FILL THE VIEWPORT QUIRKS (Quirks Mode Standard §3.6, §3.7): in a
    /// quirks-mode document an `auto`-height `<html>` is at least the viewport tall, and an
    /// `auto`-height `<body>` at least `<html>`'s content box less its own margins, borders and
    /// paddings. Chrome 150, no doctype, a 513px-tall viewport: `html.offsetHeight` 513,
    /// `body.offsetHeight` 497 over a few short lines (196 and 172 with `<!doctype html>`).
    fn fill_viewport_quirk(&mut self, viewport: Size<f32>) {
        let px = |v: LengthPercentage| {
            let raw = v.into_raw();
            if raw.tag() == taffy::style::CompactLength::LENGTH_TAG { raw.value() } else { 0.0 }
        };
        let pxa = |v: LengthPercentageAuto| {
            let raw = v.into_raw();
            if raw.tag() == taffy::style::CompactLength::LENGTH_TAG { raw.value() } else { 0.0 }
        };
        let root_style = self.styles.get(self.boxes[0].style).clone();
        if root_style.size.height != Dimension::auto() {
            return;
        }
        let q = |h: f32| (h.max(0.0) * 64.0).round() as u32;
        let root_fill = viewport.height - pxa(root_style.margin.top) - pxa(root_style.margin.bottom);
        let s0 = self.boxes[0].style;
        self.boxes[0].style = self.styles.intern_variant(s0, Variant::QuirkMinHeight(q(root_fill)));
        let (start, end) = self.boxes[0].children;
        let _ = (start, end);
        let root_content = root_fill
            - px(root_style.padding.top) - px(root_style.padding.bottom)
            - px(root_style.border.top) - px(root_style.border.bottom);
        // The body box: the first block-level child of the root that is `<body>`'s own box.
        let body = (1..self.boxes.len() as u32).find(|&i| {
            let b = &self.boxes[i as usize];
            b.parent == Some(0) && b.text.is_none() && self.box_of_node.get(b.node as usize).copied().flatten() == Some(i)
                && self.body_node == Some(b.node)
        });
        let Some(body) = body else { return };
        let bs = self.styles.get(self.boxes[body as usize].style).clone();
        if bs.size.height != Dimension::auto() {
            return;
        }
        let mut fill = root_content - pxa(bs.margin.top) - pxa(bs.margin.bottom);
        if bs.box_sizing == taffy::style::BoxSizing::ContentBox {
            fill -= px(bs.padding.top) + px(bs.padding.bottom) + px(bs.border.top) + px(bs.border.bottom);
        }
        let s = self.boxes[body as usize].style;
        self.boxes[body as usize].style = self.styles.intern_variant(s, Variant::QuirkMinHeight(q(fill)));
        // THE PERCENTAGE HEIGHT QUIRK, one level down: a `height: 50%` child of the body resolves
        // against the filled body. Chrome 150, no doctype: `<body><div style="height:50%">` is 249px
        // under a 497px body (0 with `<!doctype html>`).
        let (start, end) = self.boxes[body as usize].children;
        let children: Vec<u32> = self.child_list.get(start as usize..end as usize).map(|c| c.to_vec()).unwrap_or_default();
        for child in children {
            let cs = self.boxes[child as usize].style;
            let raw = self.styles.get(cs).size.height.into_raw();
            if raw.tag() == taffy::style::CompactLength::PERCENT_TAG {
                let h = raw.value() * fill.max(0.0);
                self.boxes[child as usize].style = self.styles.intern_variant(cs, Variant::QuirkHeight(q(h)));
            }
        }
    }

    /// One whole layout into `viewport`: the root pass, then the percentage-padding passes.
    fn lay_out(&mut self, viewport: Size<f32>) {
        if self.boxes.is_empty() {
            return;
        }
        let root = taffy::NodeId::from(ICB_NODE);
        let available = Size {
            width: AvailableSpace::Definite(viewport.width),
            height: AvailableSpace::Definite(viewport.height),
        };
        self.icb.style = Style {
            display: Display::Block,
            size: Size { width: Dimension::length(viewport.width), height: Dimension::length(viewport.height) },
            ..Style::DEFAULT
        };
        self.refresh_icb_root_style();
        self.note_floats();
        crate::perf_counters::add(&crate::perf_counters::TAFFY_ROOT_RUNS, 1);
        self.stats.root_runs += 1;
        taffy::compute_root_layout(self, root, available);
        // A percentage padding resolves against the containing block's INLINE size on all four
        // sides, and that size is not known until the containing block has been laid out. taffy
        // resolves the block-axis pair per-axis instead (see `style::with_resolved_padding`), so
        // the boxes that have one are handed a style with the percentages already turned into
        // lengths — and the number to turn them into comes from the pass just finished.
        //
        // Hence the loop rather than a single correction. A box's used padding depends on its
        // container's used width, which depends on ITS container's, so one pass settles one level
        // and the tree settles top-down. `refresh_percentage_padding` returns `false` as soon as
        // nothing moved, which is immediately on a page with no percentage padding — the common
        // case pays one scan and no second layout.
        for _ in 0..PERCENTAGE_PADDING_PASSES {
            let moved = self.refresh_percentage_padding();
            if moved.is_empty() {
                break;
            }
            self.stats.padding_moved += moved.len() as u64;
            if self.exhaustive {
                self.st.iter_mut().for_each(|s| { s.cache.clear(); });
            }
            for index in moved {
                self.clear_up_layers(index);
            }
            crate::perf_counters::add(&crate::perf_counters::EXTRA_PASSES_PADDING, 1);
            crate::perf_counters::add(&crate::perf_counters::TAFFY_ROOT_RUNS, 1);
            self.stats.root_runs += 1;
            self.refresh_icb_root_style();
            taffy::compute_root_layout(self, root, available);
        }
        // The initial containing block's other children were placed from the viewport origin; they are
        // `<html>`'s in `parent_of`, so from its border box (see `Icb`).
        let at = self.st[0].unrounded.location;
        let (start, end) = self.icb_children();
        for i in start..end {
            let c = self.child_list[i as usize] as usize;
            self.touch(c);
            let l = &mut self.st[c].unrounded.location;
            *l = Point { x: l.x - at.x, y: l.y - at.y };
        }
        self.place_at_static_positions();
        // THE LAYOUT IS NOT ROUNDED. The line this replaced was `taffy::round_layout(self, root)`,
        // and the comment above it said "skipping it gives fractional rects that no browser
        // reports". That was the one claim in this file nobody had measured, and Chrome
        // 150.0.7871.46 over CDP (2026-09-14) says the opposite on every row:
        //
        // | page                        | `getBoundingClientRect()`     | `offsetWidth`/`Height` |
        // |---|---|---:|
        // | `width: 22.5px; height: 10.25px` | 22.5 × **10.25**         | 23 × 10 |
        // | `width: 33.333%`                 | **634.6562** wide        | 635 |
        // | `height: 3.125px`                | **3.125** tall, y **31** | 3 |
        // | `padding: 2.5px` round a 4.5px child | child at x **10.5**, y **38.75** | 1899 × 5 |
        // | `width: 100.5px; border: 1.5px`  | **102.5 × 14.5**         | 103 × 15, client 101 × 13 |
        //
        // So a browser rounds at the INTEGER MEMBERS and nowhere else, which is exactly where this
        // engine already rounds: [`BoxTree::offset_size`], [`BoxTree::client_size`],
        // [`BoxTree::scroll_size`], [`BoxTree::border_widths`] and [`BoxTree::offset_position`] each
        // call `.round()` on their own answer, and `getComputedStyle().width` reports the fractional
        // used length (Chrome: `634.656px`, not `635px`). Rounding the layout underneath them made
        // every one of those members round a number that had already been rounded once, and made
        // `getBoundingClientRect` — the member that must NOT be rounded — integral.
        //
        // Measured on a page that found `<div style="width: 22.5px">` reading
        // `23px` here against Chrome's `22.5px` and left it alone because this file was not its.
        //
        // `round_layout` was also the only writer of `final_layout`, so the copy is here now. The
        // two fields stay because they are still different questions during layout —
        // `percentage_basis` reads `unrounded` BETWEEN passes, before this copy has happened.
        // EVERY LENGTH A `LayoutUnit`: what taffy derived itself -- a percentage of its
        // basis, a flex share, a sum -- is put on Blink's 1/64 px grid too, truncated toward zero as
        // `FixedPoint(float)` does, now that no pass is left to read the unsnapped values. The inputs
        // were snapped on the way in (`style::layout_unit`).
        // The snapped copy is `final_layout`; `unrounded` keeps taffy's own numbers: a box a
        // later pass does not lay out again -- its cached answer still holds -- must read as one it
        // does, and those read `unrounded` unsnapped. What reads a box after a pass reads
        // `final_layout`.
        //
        // Only the boxes this pass WROTE (`touch`): every other box's copy was snapped from the same `unrounded` --
        // snapping is a function of it -- and a carried or cloned state carries the snapped copy with it. Snapping
        // every box of a 9k-box page took longer than laying out the 44 a relayout recomputed (2026-10-23).
        let snap = |b: &mut BoxState| {
            let u = crate::layout::style::layout_unit;
            let mut snapped = b.unrounded;
            snap_layout(&mut snapped);
            b.final_layout = snapped;
            // An inline box's rectangles per line, too (`getClientRects` of a `<span>`).
            for (_, r) in &mut b.fragments {
                *r = Rect { x: u(r.x), y: u(r.y), width: u(r.width), height: u(r.height) };
            }
            b.touched = false;
        };
        // A pass that wrote most boxes -- a first layout -- snaps them in box order: through the list, each state was a
        // cache miss or two (its fragments and its layouts are 0.5 KB apart, and the list's order is the order taffy
        // wrote them in), and the first layout of a page of 16,000 `<div>`s took 250 M cycles where snapping in box
        // order takes 218 M, the same instructions (2026-10-23).
        if knob(Knob::Lists) && self.touched.len() * 16 < self.st.len() {
            for k in 0..self.touched.len() {
                snap(&mut self.st[self.touched[k] as usize]);
            }
        } else if knob(Knob::Lists) {
            self.st.iter_mut().filter(|s| s.touched).for_each(snap);
        } else {
            self.st.iter_mut().for_each(snap);
        }
        self.touched.clear();
    }

    /// Put every absolutely positioned box with an `auto` inset pair at its static position on that
    /// axis: where it would have been in its parent element's flow -- below the in-flow box before
    /// it, else at the parent's content edge -- plus its own margin. taffy measures it in its
    /// containing block's flow instead, which is another box's. Chrome 150: an `position: absolute`
    /// box after a 100px block in `<body>` is at (8, 108); it was at (0, 116), after `<html>`'s
    /// content. A box whose previous sibling is a line of inline content keeps taffy's place (its
    /// static position is on that line).
    fn place_at_static_positions(&mut self) {
        // A relatively positioned box is DRAWN shifted but keeps its unshifted place in the flow (CSS 2.1 §9.4.3),
        // and taffy's location carries the shift: take it back off before a following box's static position is
        // measured from it, resolved as taffy applied it (a percentage only against a definite parent height).
        // Chrome 150 (`S-computed-style-map`, three runs alike): an absolute box after `position: relative; top:
        // 10%` in a 200px container sits at 72px, three lines down; korvus put it at 92, 20px lower.
        let relative_shift_y = |tree: &BoxTree, s: u32, parent: u32| -> f32 {
            if tree.css_position(s) != CssPosition::Relative {
                return 0.0;
            }
            let inset = tree.style_of_box(s).inset;
            let definite = !tree.style_of_box(parent).size.height.is_auto();
            let p = tree.st[parent as usize].unrounded;
            let basis = p.size.height - p.border.top - p.border.bottom - p.padding.top - p.padding.bottom;
            let side = |v: LengthPercentageAuto| {
                if !definite && v.into_raw().tag() == taffy::style::CompactLength::PERCENT_TAG {
                    None
                } else {
                    crate::layout::resolved::resolve_inset(v, basis)
                }
            };
            match (side(inset.top), side(inset.bottom)) {
                (Some(t), _) => t,
                (None, Some(b)) => -b,
                (None, None) => 0.0,
            }
        };
        // Where box `b` is in box `cb` (`None`: the document): the places from `b` up to `cb`, as the box placed in
        // `cb` takes them. Summed from `cb` and not from the document, so a box placed in its static parent's own
        // padding box -- the common case -- is placed exactly where its margin puts it.
        let origin = |tree: &BoxTree, mut b: u32, cb: Option<u32>| {
            let (mut x, mut y) = (0.0, 0.0);
            loop {
                if Some(b) == cb {
                    return (x, y);
                }
                let l = &tree.st[b as usize].unrounded;
                x += l.location.x;
                y += l.location.y;
                match tree.boxes[b as usize].parent {
                    Some(p) => b = p,
                    None => break,
                }
            }
            // `cb` is not above `b`: from the document's origin, less `cb`'s
            let Some(cb) = cb else { return (x, y) };
            let mut c = Some(cb);
            while let Some(a) = c {
                let l = &tree.st[a as usize].unrounded;
                x -= l.location.x;
                y -= l.location.y;
                c = tree.boxes[a as usize].parent;
            }
            (x, y)
        };
        for k in 0..self.abs_boxes.len() {
            let index = self.abs_boxes[k];
            let Some((parent, prev)) = self.boxes[index as usize].static_flow else { continue };
            let in_items = matches!(self.style_of_box(parent).display, Display::Flex | Display::Grid);
            // A flex or grid container that is the box's containing block placed it already, aligned as its sole item
            // (taffy): `justify-content: center` centres it. Moved to the content edge here, Naver's "AI" mark sat
            // 35px right of where Chrome centres it.
            if in_items && self.boxes[index as usize].parent == Some(parent) {
                continue;
            }
            // after a line of inline content its static position is on that line, where taffy put it -- unless it was
            // written before anything in flow there, when it is at the line's top
            let line_start = self.boxes[index as usize].static_line_start && prev.is_some_and(|p| self.is_ifc(p));
            if !in_items && !line_start && prev.is_some_and(|p| self.is_ifc(p) || self.boxes[p as usize].level.is_inline_level()) {
                continue;
            }
            let style = self.style_of_box(index);
            let (free_x, free_y) = (style.inset.left.is_auto() && style.inset.right.is_auto(), style.inset.top.is_auto() && style.inset.bottom.is_auto());
            if !free_x && !free_y {
                continue;
            }
            let own = self.st[index as usize].unrounded;
            let p = self.st[parent as usize].unrounded;
            // its containing block: after `add`'s re-parenting, its parent box
            let cb = self.boxes[index as usize].parent;
            let (px, py) = origin(self, parent, cb);
            let content = (px + p.border.left + p.padding.left, py + p.border.top + p.padding.top);
            // A flex or grid container's absolutely positioned child is placed as if it were its
            // container's sole item (css-flexbox §4.1): at the content edge, whatever came before.
            let prev = if matches!(self.style_of_box(parent).display, Display::Flex | Display::Grid) { None } else { prev };
            let y = match prev {
                // the top of the line it was written at the start of
                Some(s) if line_start => origin(self, s, cb).1,
                Some(s) => {
                    let sl = self.st[s as usize].unrounded;
                    origin(self, s, cb).1 - relative_shift_y(self, s, parent) + sl.size.height + sl.margin.bottom
                }
                None => content.1,
            };
            // ... and aligned in it as its sole item would be: `justify-content` along the main axis, `align-items` across
            let (ax, ay) = if in_items && self.style_of_box(parent).display == Display::Flex {
                let ps = self.style_of_box(parent);
                let inner = (
                    p.size.width - p.border.left - p.border.right - p.padding.left - p.padding.right,
                    p.size.height - p.border.top - p.border.bottom - p.padding.top - p.padding.bottom,
                );
                let outer = (own.size.width + own.margin.left + own.margin.right, own.size.height + own.margin.top + own.margin.bottom);
                let main = |j: Option<taffy::style::JustifyContent>, free: f32| {
                    use taffy::style::AlignContentKeyword as K;
                    match j.map(|j| j.keyword()) {
                        Some(K::Center | K::SpaceAround | K::SpaceEvenly) => free / 2.0,
                        Some(K::FlexEnd | K::End) => free,
                        _ => 0.0,
                    }
                };
                let cross = |a: Option<taffy::style::AlignItems>, free: f32| {
                    use taffy::style::AlignItemsKeyword as K;
                    match a.map(|a| a.keyword()) {
                        Some(K::Center) => free / 2.0,
                        Some(K::FlexEnd | K::End) => free,
                        _ => 0.0,
                    }
                };
                let align = style.align_self.or(ps.align_items);
                if matches!(ps.flex_direction, taffy::style::FlexDirection::Column | taffy::style::FlexDirection::ColumnReverse) {
                    (cross(align, inner.0 - outer.0), main(ps.justify_content, inner.1 - outer.1))
                } else {
                    (main(ps.justify_content, inner.0 - outer.0), cross(align, inner.1 - outer.1))
                }
            } else {
                (0.0, 0.0)
            };
            // Set from where the boxes above it are, not moved from where it was: the move -- its place plus the
            // target less its document origin -- kept a float residue of the place it was moved from, taffy's or the
            // one a pass that answered its containing block from the cache had left, so a relayout put it a few ulps
            // from where a layout from nothing did, and across a snapping edge 1/64px off: target.com's visually
            // hidden `<img>` (`margin: -1px` at its container's top) at y -1 relaid out and -0.984375 from nothing,
            // where Chrome 152.0.7977.82 has it at -1 (2026-10-23).
            self.touch(index as usize);
            let l = &mut self.st[index as usize].unrounded.location;
            if free_x {
                l.x = content.0 + ax + own.margin.left;
            }
            if free_y {
                l.y = y + ay + own.margin.top;
            }
        }
    }

    /// `<html>`'s style as the initial containing block lays it out (see `Icb::root_style`).
    fn refresh_icb_root_style(&mut self) {
        let mut style = self.style_of_box(0).clone();
        if style.display == Display::Block {
            style.display = Display::FlowRoot;
        }
        self.icb.root_style = style;
    }

    /// Whether box `index` is one the initial containing block lays out (see `Icb`).
    pub(crate) fn in_icb(&self, index: u32) -> bool {
        let (start, end) = self.icb.children;
        start < end && self.child_list[start as usize + 1..end as usize].contains(&index)
    }

    /// The scrollbars to show: on every `overflow: auto` axis whose content the last pass did not fit, each box's
    /// variants in the order they are applied (the vertical one first). Empty when no box changes, which is when no
    /// other pass is owed for the boxes.
    ///
    /// Measured against the box's CLIENT box -- the padding box less any scrollbar already shown --
    /// and the scrollable overflow taffy computed for it, which for a scroll container includes its
    /// end padding. So a box whose content exactly fits shows nothing, and Chrome agrees: a 200x100
    /// `auto` box holding a 200x100 child is `clientWidth` 200.
    fn overflowing_scrollbars(&self) -> Vec<(u32, Vec<(Variant, bool)>)> {
        let mut out = Vec::new();
        // The root is skipped: its scrollbars are the viewport's (see `layout_viewport`); `auto_boxes` starts at 1.
        for &index in &self.auto_boxes {
            let id = self.boxes[index as usize].style;
            let auto = self.styles.auto_scroll(id);
            if !(auto.x || auto.y) || self.styles.level(id) == BoxLevel::Inline {
                continue;
            }
            if self.style_of_box(index).scrollbar_width <= 0.0 {
                continue;
            }
            let bar = self.scrollbar_gutter(index);
            let l = &self.st[index as usize].final_layout;
            let client_w = l.size.width - l.border.left - l.border.right - bar.width;
            let client_h = l.size.height - l.border.top - l.border.bottom - bar.height;
            let over = l.scrollable_overflow_rect;
            let mut variants = Vec::new();
            if auto.y && over.bottom > client_h + OVERFLOW_EPSILON {
                variants.push((Variant::ScrollbarY, true));
            }
            if auto.x && over.right > client_w + OVERFLOW_EPSILON {
                variants.push((Variant::ScrollbarX, false));
            }
            if !variants.is_empty() {
                out.push((index, variants));
            }
        }
        out
    }

    /// Give each box of `reveals` its scrollbar style variants.
    fn show_scrollbar_styles(&mut self, reveals: &[(u32, Vec<(Variant, bool)>)]) {
        for (index, variants) in reveals {
            for &(variant, _) in variants {
                let b = &mut self.boxes[*index as usize];
                b.style = self.styles.intern_variant(b.style, variant);
            }
        }
    }

    /// And their resolved-padding styles, in the current state, the same `overflow`.
    fn show_scrollbar_overrides(&mut self, reveals: &[(u32, Vec<(Variant, bool)>)]) {
        for (index, variants) in reveals {
            if let Some(padded) = self.st[*index as usize].padding_override.as_deref_mut() {
                for &(_, vertical) in variants {
                    if vertical {
                        padded.overflow.y = Overflow::Scroll;
                    } else {
                        padded.overflow.x = Overflow::Scroll;
                    }
                }
            }
        }
    }

    /// Every box above `index` forgets its laid-out answer, not its measurements: `index`
    /// is being laid out at a measuring size. Stops at a box already marked (its ancestors are).
    fn mark_final_stale_above(&mut self, index: u32) {
        let mut up = self.boxes[index as usize].parent;
        while let Some(p) = up {
            let s = &mut self.st[p as usize];
            if s.final_stale {
                break;
            }
            s.final_stale = true;
            up = self.boxes[p as usize].parent;
        }
    }

    /// Forget the cached layouts of `index` and of every box it is laid out inside: its
    /// style or padding moved, which no cache key says, and every box above it was sized with it.
    /// Everything else keeps its cache, which taffy keys on the box's constraints.
    fn clear_up(&mut self, index: u32) {
        let mut cur = Some(index);
        while let Some(b) = cur {
            self.st[b as usize].cache.clear();
            cur = self.boxes[b as usize].parent;
        }
    }

    /// [`BoxTree::clear_up`] for every box that lays `index` out. An inline box split around a block-level child is
    /// laid out by the anonymous IFC boxes on either side, and its `parent` names only the first: the second's
    /// chain forgets too, found through the fragments the last pass gave it there. A `<span style="padding: 0 10%">`
    /// split by a `<div>`, whose second half is an `<i>`'s text, kept the second half's first-pass fragment (padding
    /// still 0, 8px wide) through the percentage-padding pass, where a layout from nothing made it 68px.
    fn clear_up_layers(&mut self, index: u32) {
        self.clear_up(index);
        if self.boxes[index as usize].level != BoxLevel::Inline {
            return;
        }
        let first = self.boxes[index as usize].parent;
        let mut others: Vec<u32> = Vec::new();
        for &(ifc, _) in &self.st[index as usize].fragments {
            if Some(ifc) != first && !others.contains(&ifc) {
                others.push(ifc);
            }
        }
        for ifc in others {
            self.clear_up(ifc);
        }
    }

    /// The layout viewport for a window of `window` px: less the thickness of each scrollbar the
    /// viewport shows. A vertical one is shown when the document is taller than the layout viewport
    /// (and the viewport's `overflow-y` is not `hidden`), a horizontal one when it is wider.
    fn layout_viewport(&mut self, window: Size<f32>) -> Size<f32> {
        let bar = self.viewport_bar;
        if bar <= 0.0 || self.viewport_bars_off {
            self.viewport_bars = Point { x: false, y: false };
            return window;
        }
        // the extent this pass laid out, transformed boxes where they show (`overflow_of`)
        self.overflow_fx = std::cell::OnceCell::new();
        let (width, height) = self.scroll_extent_f32(0);
        let shows = |kind: ViewportScroll, content: f32, room: f32| match kind {
            ViewportScroll::Scroll => true,
            ViewportScroll::Hidden => false,
            ViewportScroll::Auto => content > room + OVERFLOW_EPSILON,
        };
        let y = self.viewport_bars.y || shows(self.viewport_scroll.y, height, self.viewport.height);
        let x = self.viewport_bars.x || shows(self.viewport_scroll.x, width, self.viewport.width);
        self.viewport_bars = Point { x, y };
        Size {
            width: (window.width - if y { bar } else { 0.0 }).max(0.0),
            height: (window.height - if x { bar } else { 0.0 }).max(0.0),
        }
    }

    /// The viewport's scrollbars: their thickness, and whether it shows a vertical and a horizontal one.
    pub(crate) fn viewport_scrollbars(&self) -> (f32, bool, bool) {
        (self.viewport_bar, self.viewport_bars.y, self.viewport_bars.x)
    }

    /// Does a `::-webkit-scrollbar` rule style `node`'s scrollbars?
    pub(crate) fn custom_scrollbar(&self, node: NodeId) -> bool {
        self.generated.get(&node).is_some_and(|g| g.scrollbar.is_some())
    }

    /// The space this box's own scrollbars take: the vertical one's thickness as `width`, the
    /// horizontal one's as `height`. Read from the box's own style, because that is where the
    /// decision lives -- taffy writes `Layout::scrollbar_size` from the PARENT's copy of it, gathered
    /// before a pass could change it, and a line box writes none at all.
    pub(crate) fn scrollbar_gutter(&self, index: u32) -> Size<f32> {
        let s = self.style_of_box(index);
        let horizontal = self.styles.horizontal_bar(self.boxes[index as usize].style);
        Size {
            width: if s.overflow.y == Overflow::Scroll { s.scrollbar_width } else { 0.0 },
            height: if s.overflow.x == Overflow::Scroll { horizontal } else { 0.0 },
        }
    }

    /// Re-resolve every percentage padding against the width the last pass gave its containing
    /// block. Returns whether any of them moved — that is, whether another layout pass is owed.
    fn refresh_percentage_padding(&mut self) -> Vec<u32> {
        let mut changed = Vec::new();
        for k in 0..self.pct_boxes.len() {
            let index = self.pct_boxes[k];
            let id = self.boxes[index as usize].style;
            // A text run carries its element's style, whose padding is the element's box's: the line never reads a
            // text box's (`inline_edge` is asked of inline boxes, a fragment of a text box gets none), so it resolves
            // none. Resolving it anyway cost a padding pass of the whole page whenever its basis -- the element's
            // content width -- moved by a truncated padding.
            if self.boxes[index as usize].text.is_some() || !self.styles.has_percentage_padding(id) {
                continue;
            }
            let basis = self.percentage_basis(index);
            let item = self.padding_item(index);
            // `!=` rather than an epsilon: the basis is the previous pass's own output, so it is
            // either bit-identical or genuinely different. NaN (the first time round) is never
            // equal to anything, which is exactly the "no override yet" answer wanted here.
            if self.st[index as usize].padding_basis == basis && self.st[index as usize].padding_item == item {
                continue;
            }
            let resolved = crate::layout::style::with_resolved_padding(self.styles.get(id), basis);
            let b = &mut self.st[index as usize];
            b.padding_override = Some(Box::new(resolved));
            b.padding_basis = basis;
            b.padding_item = item;
            changed.push(index);
        }
        changed
    }

    /// The inline size a box's percentage paddings resolve against: its containing block's.
    ///
    /// Read from `unrounded` and not from `final_layout`, because this runs between layout passes
    /// and only the rounding pass writes `final_layout` — and because a browser resolves against
    /// the unrounded used width anyway.
    fn percentage_basis(&self, index: u32) -> f32 {
        let Some(parent) = self.boxes[index as usize].parent else {
            // The root's containing block is the viewport.
            return self.viewport.width;
        };
        let l = &self.st[parent as usize].unrounded;
        let inside_border = l.size.width - l.border.left - l.border.right - l.scrollbar_size.width;
        match self.css_position(index) {
            // An absolutely positioned box measures against its containing block's PADDING box,
            // and after `add`'s re-parenting its parent box IS its containing block.
            CssPosition::Absolute | CssPosition::Fixed => inside_border.max(0.0),
            // Everything in flow resolves against its parent's CONTENT box. The parent's own
            // padding here is already a length: the inline axis was never the broken one, and a
            // parent with percentage padding of its own was settled by an earlier pass -- or by this
            // refresh, which walks the boxes parents first and has just resolved it: then it is that
            // padding the next pass lays the parent out with, and not the last pass's. The first pass
            // of a layout from nothing has none yet and taffy resolves the percentage unrounded (3% of
            // 640: 19.2, a LayoutUnit 19.1875), so the child's basis moved once more after the padding
            // pass and the whole page took another.
            _ => {
                let (left, right) = match &self.st[parent as usize].padding_override {
                    Some(style) => (used(style.padding.left), used(style.padding.right)),
                    None => (l.padding.left, l.padding.right),
                };
                (inside_border - left - right).max(0.0)
            }
        }
    }
}

/// How many times [`BoxTree::compute`] will re-resolve percentage paddings and lay out again.
///
/// The tree settles one level per pass, so this is a depth limit and not a convergence one: four
/// covers `html > body > div > div`, which is deeper than any percentage-padding chain seen in the
/// probe corpus, and it bounds the pathological case — a percentage padding inside a shrink-to-fit
/// container is circular in CSS itself, and a browser breaks the cycle by treating it as zero
/// rather than by iterating forever.
const PERCENTAGE_PADDING_PASSES: usize = 4;

/// The run of inline content being collected under one block container.
///
/// `open` is the stack of inline boxes the walk is currently inside. It exists for one case: a
/// block-level element buried in a `<span>` ends the run, and the span has to be **closed** on the
/// run it interrupted and **re-opened** on the next one. Without the stack, the content after the
/// block would silently fall out of the span.
#[derive(Default)]
struct InlineRun {
    ifc: Option<u32>,
    items: Vec<Item>,
    open: Vec<u32>,
}

/// A preserved line's words and spaces, as [`BoxTree::text_atoms`] and [`BoxTree::text_atom_strings`] both split it:
/// each word with whether one space follows it (a run of spaces is words of nothing, each with its space).
fn preserved_pieces(line: &str) -> impl Iterator<Item = (&str, bool)> {
    line.split_inclusive(' ').map(|piece| match piece.strip_suffix(' ') {
        Some(body) => (body, true),
        None => (piece, false),
    })
}

/// One piece of a text run, as the line breaker sees it.
#[derive(Debug, Clone, Copy)]
pub(super) struct TextAtom {
    pub width: f32,
    /// A collapsible space: a break opportunity, and it disappears at a line's start.
    pub space: bool,
    /// May a line break here at all? `nowrap` and `pre` say no.
    pub wrappable: bool,
    /// An author's own newline, in preserved whitespace.
    pub forces_break: bool,
}

/// A used length in pixels. A percentage reads as zero — the same honest failure
/// `style::px_of` takes for a value it cannot turn into a length without a containing block.
fn used(value: LengthPercentage) -> f32 {
    let raw = value.into_raw();
    if raw.tag() == taffy::style::CompactLength::LENGTH_TAG {
        raw.value()
    } else {
        0.0
    }
}

/// The same for a margin, whose extra value is `auto` — which on an inline box is zero, per CSS.
pub(super) fn used_auto(value: LengthPercentageAuto) -> f32 {
    let raw = value.into_raw();
    if raw.tag() == taffy::style::CompactLength::LENGTH_TAG {
        raw.value()
    } else {
        0.0
    }
}

/// What an atomic inline's percentages resolve against ([`BoxTree::measure_atomic`]): its margins' basis,
/// the containing block's inline size (0 while a line is only measured for its intrinsic size), and its
/// height's, the containing block's height when that is definite.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AtomicBasis {
    pub margin: f32,
    pub height: Option<f32>,
}

/// A style rect of lengths, in pixels.
pub(super) fn rect_of(r: taffy::geometry::Rect<LengthPercentage>) -> taffy::geometry::Rect<f32> {
    taffy::geometry::Rect {
        left: used(r.left),
        right: used(r.right),
        top: used(r.top),
        bottom: used(r.bottom),
    }
}

/// Does this sized-by-what-it-is box still lay its children out? Only a `<textarea>`: its value IS
/// its text child, and laying it out inside the control's own size is what gives the control a
/// scrolling area. Every other replaced element's children are not rendered.
fn lays_out_its_value(dom: &Dom, node: NodeId) -> bool {
    dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("textarea"))
}

/// The `<input type>`s that render a single-line text editor and are sized as one (Blink's
/// `TextFieldInputType` family). An unknown or missing type is `text`, the attribute's
/// invalid-value default. Measured on Chrome 150: `text`, `search`, `password` and `number` are all
/// 185×21 in the UA sheet.
/// A `<td>`/`<th>`'s `colspan` and `rowspan` (HTML's rules for parsing non-negative integers, clamped as
/// HTML §4.9.11 says), `(1, 1)` for any other element.
fn cell_spans(dom: &Dom, node: NodeId) -> (u16, u16) {
    if !dom.is_html_ns(node) || !dom.tag_name(node).is_some_and(|t| t.eq_ignore_ascii_case("td") || t.eq_ignore_ascii_case("th")) {
        return (1, 1);
    }
    let attr = |name: &str| dom.get_attribute(node, name).and_then(non_negative_integer);
    let col = attr("colspan").map_or(1, |n| n.clamp(1, 1000)) as u16;
    let row = attr("rowspan").map_or(1, |n| n.min(65534)) as u16;
    (col, row)
}

/// A control laid out around one line of its own text: a text field, a button-type `<input>`, a
/// drop-down `<select>`.
fn is_text_control(dom: &Dom, node: NodeId) -> bool {
    if !dom.is_html_ns(node) {
        return false;
    }
    match dom.tag_name(node).map(|t| t.to_ascii_lowercase()).as_deref() {
        Some("input") => {
            let kind = dom.get_attribute(node, "type").unwrap_or("text").trim().to_ascii_lowercase();
            sizes_as_text_field(&kind) || matches!(kind.as_str(), "submit" | "reset" | "button")
        }
        Some("select") => true,
        _ => false,
    }
}

fn sizes_as_text_field(kind: &str) -> bool {
    !matches!(
        kind,
        "hidden" | "checkbox" | "radio" | "range" | "color" | "file" | "image" | "submit" | "reset"
            | "button" | "date" | "time" | "month" | "week" | "datetime-local"
    )
}

/// The broken-image icon's size: `CreateAltTextShadowTree` gives its `<img>` `width=16 height=16`.
const BROKEN_IMAGE_ICON: f32 = 16.0;

/// `HTMLImageElement::AltText`: the `alt` attribute, else the `title` attribute, else nothing.
fn image_alt_text(dom: &Dom, node: NodeId) -> &str {
    dom.get_attribute(node, "alt").or_else(|| dom.get_attribute(node, "title")).unwrap_or("")
}

/// HTML's "rules for parsing non-negative integers".
fn non_negative_integer(value: &str) -> Option<u32> {
    let text = value.trim_start_matches([' ', '\t', '\n', '\u{c}', '\r']);
    let text = text.strip_prefix('+').unwrap_or(text);
    let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    text[..end].parse::<u64>().ok().map(|n| n.min(u32::MAX as u64) as u32)
}

/// An outer `<svg>`'s natural size: its `width`/`height` when they are absolute lengths, and a
/// ratio from those or else from its `viewBox`.
///
/// Measured on Chrome 150.0.7871.46: `<svg width=30 height=30>` 30×30; `<svg width=100>` 100×150
/// (no height, no ratio: the default object height); `<svg viewBox="0 0 10 5">` in a 1889px body
/// 1889×944.5 (a ratio and nothing else stretches); the same with `width=100` 100×50.
fn svg_natural(dom: &Dom, node: NodeId) -> Natural {
    let length = |name: &str| {
        let v = dom.get_attribute(node, name)?.trim();
        let v = v.strip_suffix("px").unwrap_or(v).trim();
        v.parse::<f32>().ok().filter(|n| n.is_finite() && *n >= 0.0)
    };
    let (width, height) = (length("width"), length("height"));
    let view_box = dom.get_attribute(node, "viewBox").and_then(|v| {
        let n: Vec<f32> = v
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<f32>().ok())
            .collect::<Option<Vec<f32>>>()?;
        (n.len() == 4 && n[2] > 0.0 && n[3] > 0.0).then(|| n[2] / n[3])
    });
    let ratio = match (width, height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some(w / h),
        _ => view_box,
    };
    Natural { width, height, ratio }
}

/// What a drop-down shows for each of its options: the `label` attribute if it is non-empty, else
/// the text with whitespace stripped and collapsed — and, for an option in an `<optgroup>`, that
/// label indented by four spaces (Blink's `TextIndentedToRespectGroupLabel`).
fn option_labels(dom: &Dom, select: NodeId) -> Vec<String> {
    let is = |n: NodeId, tag: &str| dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case(tag));
    let label = |option: NodeId| match dom.get_attribute(option, "label") {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => dom.text_content(option).split_ascii_whitespace().collect::<Vec<_>>().join(" "),
    };
    let mut out = Vec::new();
    for child in dom.children(select) {
        if is(child, "option") {
            out.push(label(child));
        } else if is(child, "optgroup") {
            for grandchild in dom.children(child) {
                if is(grandchild, "option") {
                    out.push(format!("    {}", label(grandchild)));
                }
            }
        }
    }
    out
}

/// The first element child of `node`, depth-first. The document node's children are the doctype and
/// the document element, and only the second generates a box.
fn first_element(dom: &Dom, node: NodeId) -> Option<NodeId> {
    for child in dom.children(node) {
        if matches!(dom.node(child).kind, NodeKind::Element { .. }) {
            return Some(child);
        }
    }
    None
}

impl TraversePartialTree for BoxTree {
    type ChildIter<'a> = std::iter::Map<std::slice::Iter<'a, u32>, fn(&u32) -> taffy::NodeId>;

    fn child_ids(&self, parent: taffy::NodeId) -> Self::ChildIter<'_> {
        let (start, end) = self.children_range(parent);
        self.child_list[start as usize..end as usize]
            .iter()
            .map(|&b| taffy::NodeId::from(b as usize))
    }

    fn child_count(&self, parent: taffy::NodeId) -> usize {
        let (start, end) = self.children_range(parent);
        (end - start) as usize
    }

    fn get_child_id(&self, parent: taffy::NodeId, index: usize) -> taffy::NodeId {
        let (start, _) = self.children_range(parent);
        taffy::NodeId::from(self.child_list[start as usize + index] as usize)
    }
}

impl TraverseTree for BoxTree {}

impl BoxTree {
    /// A taffy node's children as a range of `child_list` -- the initial containing block's included.
    fn children_range(&self, node: taffy::NodeId) -> (u32, u32) {
        if usize::from(node) == ICB_NODE {
            return self.icb.children;
        }
        self.boxes[usize::from(node)].children
    }
}

impl LayoutPartialTree for BoxTree {
    type CoreContainerStyle<'a> = &'a Style;
    type CustomIdent = String;

    /// A math function over a percentage: taffy's default answered 0 for every one. The
    /// result is a `LayoutUnit` like every other length (L13).
    fn resolve_calc_value(&self, val: *const (), basis: f32) -> f32 {
        crate::layout::style::layout_unit(crate::style::math::resolve_ptr(val, basis))
    }

    fn get_core_container_style(&self, node: taffy::NodeId) -> &Style {
        if usize::from(node) == ICB_NODE {
            return &self.icb.style;
        }
        // taffy's `NodeId` is a box index, and box indices are `u32` everywhere else in this file
        // (`child_list` is a `Vec<u32>`), so the narrowing is exact by construction.
        self.style_of_box(usize::from(node) as u32)
    }

    fn set_unrounded_layout(&mut self, node: taffy::NodeId, layout: &Layout) {
        if usize::from(node) == ICB_NODE {
            self.icb.layout = *layout;
            return;
        }
        let mut layout = *layout;
        layout.scrollbar_size = self.scrollbar_gutter(usize::from(node) as u32);
        self.touch(usize::from(node));
        self.st[usize::from(node)].unrounded = layout;
    }

    fn compute_child_layout(&mut self, node: taffy::NodeId, inputs: LayoutInput) -> LayoutOutput {
        // The initial containing block: a block, never cached (its children keep their own caches).
        if usize::from(node) == ICB_NODE {
            return taffy::compute_block_layout(self, node, inputs, None);
        }
        // An ancestor is `display: none`, so this subtree has no layout regardless of its own
        // display. taffy asks rather than assuming, because a hidden subtree still has to have its
        // cached layout zeroed.
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return taffy::compute_hidden_layout(self, node);
        }
        let measuring = inputs.run_mode == RunMode::ComputeSize;
        self.measuring += measuring as u32;
        let uncached = |tree: &mut BoxTree, node: taffy::NodeId, inputs: LayoutInput| -> LayoutOutput {
            if inputs.run_mode == RunMode::PerformLayout && tree.measuring > 0 {
                tree.mark_final_stale_above(usize::from(node) as u32);
            }
            // a layout of the box places its children again: no `vertical-align` shift is on them now
            if inputs.run_mode == RunMode::PerformLayout {
                tree.st[usize::from(node)].valign_shift = 0.0;
            }
            crate::perf_counters::add(&crate::perf_counters::BOX_LAYOUTS_COMPUTED, 1);
            tree.stats.computed += 1;
            tree.stats.measures += (inputs.run_mode == RunMode::ComputeSize) as u64;
            let display = tree.get_core_container_style(node).display;
            // A table: taffy has no table algorithm (`layout::table`).
            if display != Display::None && tree.table_role(usize::from(node) as u32) == crate::layout::style::TableRole::Table {
                return tree.layout_table(node, inputs);
            }
            let has_children = tree.child_count(node) > 0;
            // A replaced element or form control: sized by what it is (`layout::replaced`), and it
            // comes before everything else because such a box may still have a child — a
            // `<textarea>`'s value — which must be laid out INSIDE that size and not decide it.
            if let Some(natural) = tree.boxes[usize::from(node)].natural.as_deref().copied() {
                let preferred = tree.styles.preferred_ratio(tree.boxes[usize::from(node)].style);
                let out = replaced::compute_replaced_layout(
                    tree.indefinite_stretch(usize::from(node), inputs),
                    tree.get_core_container_style(node),
                    natural,
                    preferred,
                );
                if !has_children || display == Display::None {
                    return out;
                }
                // its value as a block, whatever its `display` (Chrome's inner editor; see `add_children`)
                let inner = LayoutInput { known_dimensions: out.size.map(Some), ..inputs };
                let content = taffy::compute_block_layout(tree, node, inner, None);
                return LayoutOutput { size: out.size, ..content };
            }
            // An inline formatting context is the one taffy has no algorithm for, so it is the one
            // arm that is ours. It comes first because such a box may have taffy children too —
            // its atomic inlines — and block layout would stack them instead of lining them up.
            if display != Display::None && tree.is_ifc(usize::from(node) as u32) {
                return tree.layout_ifc(node, inputs, None);
            }
            match (display, has_children) {
                (Display::None, _) => taffy::compute_hidden_layout(tree, node),
                // A block whose margins collapse in its parent's flow is laid out in that block formatting context,
                // not as the root of one: taffy lets no BFC root be collapsed through, so an empty box holding an
                // empty box kept both its margins (Chrome 150: `<div style="margin:24px 0 10px"><div style="margin:30px
                // 0 5px"></div></div>` is 30 tall in a flow root, here 60; Wikipedia's empty site notice took 48px
                // for 24). The context is a fresh one, so only for a box with no float below it (those place their
                // floats in a root sized to them, or come through `layout_in_bfc` with their parent's); a
                // `flow-root`, a flex or grid item, the root are roots as they were.
                (Display::Block, true)
                    if (inputs.vertical_margins_are_collapsible.start || inputs.vertical_margins_are_collapsible.end)
                        && !tree.floats_within.get(usize::from(node)).copied().unwrap_or(false) =>
                {
                    let mut bfc = taffy::compute::BlockFormattingContext::new();
                    let mut root = bfc.root_block_context();
                    let mut ctx = root.sub_context(0.0, [0.0, 0.0]);
                    taffy::compute_block_layout(tree, node, inputs, Some(&mut ctx))
                }
                (Display::Block | Display::FlowRoot, true) => {
                    taffy::compute_block_layout(tree, node, inputs, None)
                }
                (Display::Flex, true) => {
                    tree.note_flex_cross(usize::from(node), &inputs);
                    let inputs = tree.floored_at_edges(node, inputs);
                    let inputs = tree.flex_fit_content(node, inputs);
                    taffy::compute_flexbox_layout(tree, node, inputs)
                }
                (Display::Grid, true) => {
                    let inputs = tree.floored_at_edges(node, inputs);
                    taffy::compute_grid_layout(tree, node, inputs)
                }
                // A box with no children is a leaf, and a leaf with no text has no intrinsic
                // size. Zero is right for an empty `<div>` and wrong for one with text in it,
                // which is what P3's inline flow is for.
                (_, false) => {
                    let b = &tree.boxes[usize::from(node)];
                    let text = b.text.clone();
                    let spec = tree.styles.font(b.style).clone();
                    let style = tree.get_core_container_style(node);
                    taffy::compute_leaf_layout(inputs, style, |_, _| 0.0, |known, available| {
                        let Some(text) = text else { return Size::ZERO };
                        // The width to wrap against: whatever the parent has already decided, else
                        // whatever space it has offered. `MaxContent` means "how wide would you be
                        // on one line", which is the question shrink-to-fit asks.
                        let limit = known.width.or(match available.width {
                            AvailableSpace::Definite(w) => Some(w),
                            AvailableSpace::MaxContent => None,
                            AvailableSpace::MinContent => Some(0.0),
                        });
                        inline::measure_block(&*tree.fonts, &spec, &text, limit)
                    })
                }
            }
        };
        let out = taffy::compute_cached_layout(self, node, inputs, |tree, node, inputs| {
            let measuring = inputs.run_mode == RunMode::ComputeSize;
            let own = measuring && inputs.parent_size.width.is_none();
            let content = measuring && inputs.known_dimensions.width.is_none() && !tree.width_from_style(usize::from(node), &inputs);
            tree.sizing.push((usize::from(node) as u32, own, content));
            let out = uncached(tree, node, inputs);
            tree.sizing.pop();
            // Inside the computation and not after it: a cached answer has its content placed already.
            tree.align_cell_content(node, inputs);
            out
        });
        self.measuring -= measuring as u32;
        out
    }
}

/// Every length of a computed layout as a `LayoutUnit` (see `style::layout_unit`).
fn snap_layout(l: &mut Layout) {
    let u = crate::layout::style::layout_unit;
    l.location = Point { x: u(l.location.x), y: u(l.location.y) };
    l.size = Size { width: u(l.size.width), height: u(l.size.height) };
    let rect = |r: taffy::geometry::Rect<f32>| taffy::geometry::Rect { left: u(r.left), right: u(r.right), top: u(r.top), bottom: u(r.bottom) };
    l.border = rect(l.border);
    l.padding = rect(l.padding);
    l.margin = rect(l.margin);
}

impl BoxTree {
    /// Unlock every skipped `content-visibility: auto` box now relevant to the user:
    /// its border box, grown by 150% of the viewport's width and height on each side, meets the
    /// viewport. Answers whether any was unlocked. The page is at its initial scroll position.
    fn unlock_relevant(&mut self, viewport: Size<f32>) -> bool {
        let (mx, my) = (viewport.width * 1.5, viewport.height * 1.5);
        let mut any = false;
        for i in 0..self.cv_auto.len() {
            let (index, relevant) = self.cv_auto[i];
            if self.boxes[index as usize].style == relevant {
                continue;
            }
            let (mut x, mut y) = (0.0f32, 0.0f32);
            let mut cur = Some(index);
            while let Some(b) = cur {
                let l = &self.st[b as usize].final_layout;
                x += l.location.x;
                y += l.location.y;
                cur = self.boxes[b as usize].parent;
            }
            let size = self.st[index as usize].final_layout.size;
            let near = x - mx < viewport.width && x + size.width + mx > 0.0 && y - my < viewport.height && y + size.height + my > 0.0;
            if near {
                self.boxes[index as usize].style = relevant;
                any = true;
            }
        }
        any
    }

    /// The lines the box `index` keeps when it is a clamping legacy box's line content;
    /// 0 when it does not clamp.
    pub(crate) fn line_clamp_of(&self, index: u32) -> u32 {
        self.styles.line_clamp(self.boxes[index as usize].style)
    }

    /// The elements whose `content-visibility: auto` box is still skipped: `innerText`
    /// leaves their contents out, as Chrome 150's does for a box off the viewport.
    pub fn skipped_auto_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.cv_auto
            .iter()
            .filter(|(index, relevant)| self.boxes[*index as usize].style != *relevant)
            .map(|(index, _)| self.boxes[*index as usize].node)
    }

    /// A flex container sized INTO the space on offer -- no width decided, a definite one available: a
    /// `fit-content` keyword, a shrink-to-fit absolutely positioned or floated box -- is as wide as CSS's
    /// fit-content says, `min(max-content, max(min-content, available))` (CSS Sizing 3 §5). taffy's flexbox
    /// takes the sum of the items' flex bases there (their max-content widths), so a `width: fit-content`
    /// flex box of a 305px run of text in a 100px box measured 305 where Chrome 150's is the text's
    /// min-content, 98. A flex base size is measured under `max-content` and never
    /// comes here.
    fn flex_fit_content(&mut self, node: taffy::NodeId, inputs: LayoutInput) -> LayoutInput {
        if inputs.known_dimensions.width.is_some() {
            return inputs;
        }
        let AvailableSpace::Definite(available) = inputs.available_space.width else { return inputs };
        let probe = |space: AvailableSpace| LayoutInput {
            run_mode: RunMode::ComputeSize,
            axis: RequestedAxis::Horizontal,
            known_dimensions: Size { width: None, height: inputs.known_dimensions.height },
            available_space: Size { width: space, height: inputs.available_space.height },
            ..inputs
        };
        let max = LayoutPartialTree::compute_child_layout(self, node, probe(AvailableSpace::MaxContent)).size.width;
        let width = if max <= available {
            max
        } else {
            let min = LayoutPartialTree::compute_child_layout(self, node, probe(AvailableSpace::MinContent)).size.width;
            min.max(available).min(max)
        };
        LayoutInput { known_dimensions: Size { width: Some(width), ..inputs.known_dimensions }, ..inputs }
    }

    /// `inputs` with a known width no narrower than the box's own padding and border. A block container's
    /// layout floors it there (taffy's `maybe_max(padding_border_size)`); the flex and grid algorithms
    /// take what they are given, and a block parent gives a stretched box its container less its
    /// margins -- negative in a container narrower than those. Chrome 150: a
    /// `display:flex` box with `margin: 0 3px; padding: 0 2px; border: 1px` in a 0-wide box is 6 wide;
    /// this laid it out -6 wide.
    fn floored_at_edges(&self, node: taffy::NodeId, inputs: LayoutInput) -> LayoutInput {
        let Some(width) = inputs.known_dimensions.width else { return inputs };
        let style = self.style_of_box(usize::from(node) as u32);
        let calc = crate::style::math::resolve_ptr;
        let padding = taffy::ResolveOrZero::resolve_or_zero(style.padding, inputs.parent_size.width, calc);
        let border = taffy::ResolveOrZero::resolve_or_zero(style.border, inputs.parent_size.width, calc);
        let edges: f32 = padding.left + padding.right + border.left + border.right;
        if width >= edges {
            return inputs;
        }
        LayoutInput { known_dimensions: Size { width: Some(edges), ..inputs.known_dimensions }, ..inputs }
    }

    /// Record, as the flex container `node` starts laying out under `inputs`, whether its cross size
    /// is definite -- taffy's own test (`has_definite_cross_size`: a known cross size, or one its style
    /// resolves) less a wrapping container, whose lines are sized from their items.
    fn note_flex_cross(&mut self, node: usize, inputs: &LayoutInput) {
        let style = self.style_of_box(node as u32);
        let row = matches!(style.flex_direction, taffy::style::FlexDirection::Row | taffy::style::FlexDirection::RowReverse);
        let (known, definite, styled, basis) = if row {
            (inputs.known_dimensions.height, inputs.known_dimensions_are_definite.height, style.size.height, inputs.parent_size.height)
        } else {
            (inputs.known_dimensions.width, inputs.known_dimensions_are_definite.width, style.size.width, inputs.parent_size.width)
        };
        let styled = taffy::MaybeResolve::maybe_resolve(styled, basis, crate::style::math::resolve_ptr).is_some();
        let wraps = style.flex_wrap != taffy::style::FlexWrap::NoWrap;
        let definite = !wraps && ((known.is_some() && definite) || styled);
        // An item's measurement reads this flag (`indefinite_stretch`) and its cache key does not: when
        // it moves, the items forget what they measured under the other value. Only a REPLACED item reads it --
        // `indefinite_stretch` is called from the replaced arm of `compute_child_layout` alone, and reads only its
        // direct parent's flag -- so only those forget. Any other item's answers are a function of its own input
        // (every container below it sets its own flag from its own input at the start of its own run), and clearing
        // them made nested React-style flex rows measure every item again at each measure/layout alternation: 68
        // items a relayout on a costco-like carousel page, each with its subtree.
        if self.st[node].flex_cross_definite != definite {
            let (start, end) = self.boxes[node].children;
            let only_replaced = knob(Knob::FlexFlip);
            for i in start..end {
                let child = self.child_list[i as usize];
                if only_replaced && self.boxes[child as usize].natural.is_none() {
                    continue;
                }
                self.st[child as usize].cache.clear();
                self.stats.flex_item_clears += 1;
                crate::perf_counters::add(&crate::perf_counters::FLEX_FLIP_ITEM_CLEARS, 1);
            }
        }
        self.st[node].flex_cross_definite = definite;
    }

    /// A stretched flex item's cross size is definite only when its container's is (CSS Flexbox
    /// §9.8); otherwise its flex base size and min-content size are measured at fit-content in that
    /// axis. taffy 0.14 hands such an item the container's available cross space as a KNOWN size --
    /// under a max-content constraint that is the item's own `max-height`, made definite -- and marks
    /// it indefinite only for itself (`child_cross_size_is_definite`). A replaced box transfers that
    /// size through its ratio, so a 120x40 logo with `max-height: 70px` in an auto-height flex row
    /// measured 210 wide where Chrome 150 lays it out at 120x40 (measured in
    /// `c8/repro/imgflex`: with a 200px-high container Chrome gives 210x70, which this keeps).
    ///
    /// So when taffy MEASURES the main size of a stretched, auto-sized replaced item whose container's
    /// cross size is not definite -- the flex base size and the min-content size, both a
    /// `ComputeSize` call for the main axis -- the cross size it was handed is dropped. The pass that
    /// sizes the cross axis, and the final layout that stretches the item to its line (a
    /// `PerformLayout` call, where taffy marks the stretched size definite itself), are untouched:
    /// Chrome stretches a canvas beside a 200px sibling to 300x200.
    fn indefinite_stretch(&self, node: usize, mut inputs: LayoutInput) -> LayoutInput {
        if inputs.sizing_mode != SizingMode::ContentSize || inputs.run_mode != RunMode::ComputeSize {
            return inputs;
        }
        let Some(parent) = self.boxes[node].parent else { return inputs };
        let container = self.style_of_box(parent);
        if container.display != Display::Flex || self.st[parent as usize].flex_cross_definite {
            return inputs;
        }
        let row = matches!(container.flex_direction, taffy::style::FlexDirection::Row | taffy::style::FlexDirection::RowReverse);
        let main = if row { RequestedAxis::Horizontal } else { RequestedAxis::Vertical };
        if inputs.axis != main {
            return inputs;
        }
        let style = self.style_of_box(node as u32);
        let stretch = matches!(
            style.align_self.or(container.align_items),
            None | Some(taffy::style::AlignItems::STRETCH)
        );
        let (size, start, end) = if row {
            (style.size.height, style.margin.top, style.margin.bottom)
        } else {
            (style.size.width, style.margin.left, style.margin.right)
        };
        if stretch && size.is_auto() && !start.is_auto() && !end.is_auto() {
            if row {
                inputs.known_dimensions.height = None;
            } else {
                inputs.known_dimensions.width = None;
            }
        }
        inputs
    }
}

impl CacheTree for BoxTree {
    fn cache_get(&mut self, node: taffy::NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        if usize::from(node) == ICB_NODE {
            return None;
        }
        let index = usize::from(node);
        if self.st[index].final_stale && input.run_mode == RunMode::PerformLayout || self.measured_apart(index) {
            return None;
        }
        self.st[index].cache.get(input)
    }

    fn cache_store(&mut self, node: taffy::NodeId, input: &LayoutInput, output: LayoutOutput) {
        if usize::from(node) == ICB_NODE {
            return;
        }
        let index = usize::from(node);
        // Laid out again, unless something below was left laid out at a measuring width AFTER this
        // run laid it out (a measure that follows the final layout): a stale child keeps its parent
        // stale, or the next laying-out call would answer from here and never go back down.
        if input.run_mode == RunMode::PerformLayout && self.st[index].final_stale {
            let (start, end) = self.boxes[index].children;
            let child_stale = (start..end).any(|i| self.st[self.child_list[i as usize] as usize].final_stale);
            self.st[index].final_stale = child_stale;
        }
        if !self.measured_apart(index) && self.st[index].cache.store(input, output) {
            self.stats.margin_refused += 1;
            crate::perf_counters::add(&crate::perf_counters::MEASURES_REFUSED_MARGIN, 1);
        }
    }

    fn cache_clear(&mut self, node: taffy::NodeId) {
        if usize::from(node) == ICB_NODE {
            return;
        }
        let b = &mut self.st[usize::from(node)];
        b.cache.clear();
        b.final_stale = false;
    }
}

impl LayoutBlockContainer for BoxTree {
    type BlockContainerStyle<'a> = &'a Style;
    type BlockItemStyle<'a> = &'a Style;

    fn get_block_container_style(&self, node: taffy::NodeId) -> &Style {
        self.get_core_container_style(node)
    }

    fn get_block_child_style(&self, child: taffy::NodeId) -> &Style {
        // `<html>` is a child of the initial containing block only (see `Icb::root_style`)
        if usize::from(child) == 0 {
            return &self.icb.root_style;
        }
        self.get_core_container_style(child)
    }

    /// A child in its parent's block formatting context. Where floats are in play -- placed already, or to be placed
    /// inside this child -- it is laid out in that context and uncached: a cached answer neither knows where the floats
    /// are nor places its own again, and the boxes after it would flow under nothing. Anywhere else it is the cached
    /// layout it always was.
    fn compute_block_child_layout(
        &mut self,
        node: taffy::NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut taffy::compute::BlockContext<'_>>,
    ) -> LayoutOutput {
        let index = usize::from(node);
        match block_ctx {
            Some(ctx)
                if inputs.run_mode != RunMode::PerformHiddenLayout
                    && index != ICB_NODE
                    && (ctx.has_floats() || self.floats_within.get(index).copied().unwrap_or(false)) =>
            {
                self.layout_in_bfc(node, inputs, ctx)
            }
            _ => self.compute_child_layout(node, inputs),
        }
    }
}

impl BoxTree {
    /// Which boxes have a float below them ([`BoxTree::floats_within`]), from the styles as they are now.
    fn note_floats(&mut self) {
        let n = self.boxes.len();
        let mut within = vec![false; n];
        // a box is made after its parent, so one walk from the last box up carries each mark to the top
        for i in (0..n).rev() {
            let floated = self.styles.get(self.boxes[i].style).float != Float::None;
            if let Some(p) = self.boxes[i].parent {
                if floated || within[i] {
                    within[p as usize] = true;
                }
            }
        }
        self.floats_within = within;
    }

    /// [`BoxTree::compute_child_layout`] for a block or an inline formatting context in the block formatting context
    /// `ctx`, uncached. Every other box is not affected by the floats around it and is laid out as it always is.
    fn layout_in_bfc(&mut self, node: taffy::NodeId, inputs: LayoutInput, ctx: &mut taffy::compute::BlockContext<'_>) -> LayoutOutput {
        let index = usize::from(node);
        let display = self.get_core_container_style(node).display;
        let replaced = self.boxes[index].natural.is_some();
        let table = self.table_role(index as u32) == crate::layout::style::TableRole::Table;
        let ifc = display != Display::None && self.is_ifc(index as u32);
        if replaced || table || !(ifc || (display == Display::Block && self.child_count(node) > 0)) {
            return self.compute_child_layout(node, inputs);
        }
        let measuring = inputs.run_mode == RunMode::ComputeSize;
        self.measuring += measuring as u32;
        if inputs.run_mode == RunMode::PerformLayout && self.measuring > 0 {
            self.mark_final_stale_above(index as u32);
        }
        if inputs.run_mode == RunMode::PerformLayout {
            self.st[index].valign_shift = 0.0;
        }
        crate::perf_counters::add(&crate::perf_counters::BOX_LAYOUTS_COMPUTED, 1);
        self.stats.computed += 1;
        self.stats.measures += measuring as u64;
        let out = if ifc { self.layout_ifc(node, inputs, Some(&*ctx)) } else { taffy::compute_block_layout(self, node, inputs, Some(ctx)) };
        self.measuring -= measuring as u32;
        out
    }
}

impl LayoutFlexboxContainer for BoxTree {
    type FlexboxContainerStyle<'a> = &'a Style;
    type FlexboxItemStyle<'a> = &'a Style;

    fn get_flexbox_container_style(&self, node: taffy::NodeId) -> &Style {
        self.get_core_container_style(node)
    }

    fn get_flexbox_child_style(&self, child: taffy::NodeId) -> &Style {
        self.get_core_container_style(child)
    }
}

impl LayoutGridContainer for BoxTree {
    type GridContainerStyle<'a> = &'a Style;
    type GridItemStyle<'a> = &'a Style;

    fn get_grid_container_style(&self, node: taffy::NodeId) -> &Style {
        self.get_core_container_style(node)
    }

    fn get_grid_child_style(&self, child: taffy::NodeId) -> &Style {
        self.get_core_container_style(child)
    }
}

impl RoundTree for BoxTree {
    fn get_unrounded_layout(&self, node: taffy::NodeId) -> Layout {
        if usize::from(node) == ICB_NODE {
            return self.icb.layout;
        }
        self.st[usize::from(node)].unrounded
    }

    fn set_final_layout(&mut self, node: taffy::NodeId, layout: &Layout) {
        if usize::from(node) == ICB_NODE {
            return;
        }
        self.touch(usize::from(node));
        self.st[usize::from(node)].final_layout = *layout;
    }
}

impl PrintTree for BoxTree {
    fn get_debug_label(&self, node: taffy::NodeId) -> &'static str {
        match self.get_core_container_style(node).display {
            Display::None => "NONE",
            Display::Block => "BLOCK",
            Display::FlowRoot => "FLOW-ROOT",
            Display::Flex => "FLEX",
            Display::Grid => "GRID",
        }
    }

    fn get_final_layout(&self, node: taffy::NodeId) -> Layout {
        if usize::from(node) == ICB_NODE {
            return self.icb.layout;
        }
        self.st[usize::from(node)].final_layout
    }
}

/// Does `style` size its box off the containing block's HEIGHT -- a percentage, or a `calc()` that may hold
/// one, in `height`, `min-height` or `max-height`? [`BoxTree::carry_from`] carries no cache for such a box.
fn block_percent(style: &Style) -> bool {
    use taffy::style::CompactLength;
    let pct = |raw: CompactLength| raw.tag() == CompactLength::PERCENT_TAG || raw.is_calc();
    pct(style.size.height.into_raw()) || pct(style.min_size.height.into_raw()) || pct(style.max_size.height.into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::compute::{compute, ComputeContext, InlineStyle, ParentFormatting};
    use crate::style::matching::Matcher;
    use crate::style::media::MediaContext;
    use crate::style::parse::{parse_inline_style, parse_stylesheet};
    use crate::style::rules::{Origin, Span, Stylesheet};
    use crate::style::ua;

    /// A measurement in the box's cache input: `width` known, the rest as a flex item's measuring call has it.
    fn measure_input(width: f32, parent_height: Option<f32>) -> LayoutInput {
        LayoutInput {
            run_mode: RunMode::ComputeSize,
            sizing_mode: SizingMode::ContentSize,
            axis: RequestedAxis::Both,
            known_dimensions: Size { width: Some(width), height: None },
            known_dimensions_are_definite: Size { width: true, height: true },
            parent_size: Size { width: Some(800.0), height: parent_height },
            available_space: Size { width: AvailableSpace::Definite(width), height: AvailableSpace::MaxContent },
            vertical_margins_are_collapsible: Line::FALSE,
        }
    }

    // taffy keeps nine measurements a box; the tenth evicts one. Asked again at an evicted input, the box answers from
    // what it kept -- for that exact input and no other, and never once its cache has been cleared.
    #[test]
    fn a_box_answers_a_measurement_taffy_evicted_for_its_exact_input_only() {
        let mut c = BoxCache::default();
        for w in 0..12 {
            c.store(&measure_input(100.0 + w as f32, None), LayoutOutput::from_outer_size(Size { width: 100.0 + w as f32, height: 7.0 + w as f32 }));
        }
        let first = measure_input(100.0, None);
        assert!(c.taffy.get(&first).is_none(), "taffy's nine slots no longer hold the first of twelve");
        assert_eq!(c.get(&first).map(|o| o.size), Some(Size { width: 100.0, height: 7.0 }));
        assert!(c.get(&measure_input(100.0, Some(300.0))).is_none(), "another parent height is another input");
        assert!(c.get(&measure_input(100.5, None)).is_none());
        c.clear();
        assert!(c.get(&measure_input(111.0, None)).is_none(), "a cleared box keeps nothing");
    }

    // A size that carries margin-collapse state cannot be rebuilt from the size alone -- taffy's own slots refuse it --
    // so it is kept whole: handed back field for field for its exact input, and for no other.
    #[test]
    fn a_measurement_with_collapsing_margins_is_kept_whole_for_its_exact_input() {
        let mut c = BoxCache::default();
        let mut out = LayoutOutput::from_outer_size(Size { width: 50.0, height: 20.0 });
        out.margins_can_collapse_through = true;
        out.top_margin = taffy::CollapsibleMarginSet::from_margin(6.0);
        out.bottom_margin = taffy::CollapsibleMarginSet::from_margin(-2.0);
        assert!(c.store(&measure_input(50.0, None), out), "a refused measurement, as taffy counts it");
        assert!(c.spilled.is_empty(), "the size-only spill cannot hold it");
        assert!(c.taffy.get(&measure_input(50.0, None)).is_none(), "nor taffy's slots");
        let got = c.get(&measure_input(50.0, None)).expect("kept whole");
        assert_eq!(got.size, out.size);
        assert_eq!(got.top_margin, out.top_margin);
        assert_eq!(got.bottom_margin, out.bottom_margin);
        assert!(got.margins_can_collapse_through);
        assert!(c.get(&measure_input(50.0, Some(300.0))).is_none(), "another parent height is another input");
        c.clear();
        assert!(c.get(&measure_input(50.0, None)).is_none(), "a cleared box keeps nothing");
        assert!(c.spilled.is_empty());
    }
    /// Style and lay out `html` with `css` on top of the user-agent sheet, in a 1024x768 viewport.
    struct Page {
        dom: Dom,
        tree: BoxTree,
    }

    impl Page {
        fn new(css: &str, html: &str) -> Page {
            Page::sized(css, html, Size { width: 1024.0, height: 768.0 })
        }

        fn sized(css: &str, html: &str, viewport: Size<f32>) -> Page {
            Page::themed(css, html, viewport, spec::ThemeProfile::default())
        }

        fn themed(css: &str, html: &str, viewport: Size<f32>, theme: spec::ThemeProfile) -> Page {
            let sheets: Vec<Stylesheet> =
                vec![ua::stylesheet(), parse_stylesheet(Origin::Author, css)];
            let dom = Dom::parse(html);
            let matcher = Matcher::build(&sheets);
            let media = MediaContext {
                viewport_width: viewport.width,
                viewport_height: viewport.height,
                ..MediaContext::default()
            };
            let ctx = ComputeContext {
                viewport_width: viewport.width,
                viewport_height: viewport.height,
                root_font_size: 16.0,
                ..ComputeContext::default()
            };
            let mut arena = StyleArena::default();

            // Style every element in document order, so a parent is always computed before its
            // children have to inherit from it.
            let mut keys: Vec<Option<StyleKey>> = vec![None; dom.len()];
            let mut formatting: Vec<ParentFormatting> = vec![ParentFormatting::Flow; dom.len()];
            let mut matched = Vec::new();
            let mut stack: Vec<NodeId> = vec![dom.root()];
            while let Some(node) = stack.pop() {
                for child in dom.children(node).into_iter().rev() {
                    stack.push(child);
                }
                if !matches!(dom.node(node).kind, NodeKind::Element { .. }) {
                    continue;
                }
                let parent = dom.node(node).parent.and_then(|p| keys[p as usize]);
                matcher.match_element(&sheets, &dom, node, &media, &mut matched);
                // the presentational hints lead the block, as the page's cascade has them (`InlineStyle::hints`)
                let hints = crate::style::hints::for_element(&dom, node);
                let hint_count = if hints.is_empty() { 0 } else { parse_inline_style(&hints).declarations().len() as u32 };
                let attr = format!("{hints}{}", dom.style_for_cascade(node).unwrap_or(""));
                let inline_sheet = (!attr.trim().is_empty()).then(|| parse_inline_style(&attr));
                let inline = inline_sheet.as_ref().map(|s| InlineStyle {
                    sheet: s,
                    decls: Span::of(0..s.declarations().len()),
                    hints: hint_count,
                });
                let mine =
                    dom.node(node).parent.map_or(ParentFormatting::Flow, |p| formatting[p as usize]);
                let key = compute(&sheets, &matched, inline, parent, mine, &ctx, &mut arena);
                keys[node as usize] = Some(key);
                formatting[node as usize] = ParentFormatting::of_children(&arena, key, mine);
            }

            let mut tree = BoxTree::build_with_theme(&dom, &arena, &|n| keys[n as usize], theme);
            tree.compute(viewport);
            Page { dom, tree }
        }

        fn find(&self, id: &str) -> NodeId {
            (0..self.dom.len() as NodeId)
                .find(|&n| self.dom.get_attribute(n, "id") == Some(id))
                .unwrap_or_else(|| panic!("no #{id}"))
        }

        /// (x, y, width, height) of an element's border box, relative to its parent.
        fn rect(&self, id: &str) -> (f32, f32, f32, f32) {
            let b = self.tree.box_of(self.find(id)).unwrap_or_else(|| panic!("#{id} has no box"));
            let l = self.tree.layout(b);
            (l.location.x, l.location.y, l.size.width, l.size.height)
        }

        fn has_box(&self, id: &str) -> bool {
            self.tree.box_of(self.find(id)).is_some()
        }
    }



    // The mission, in one assertion. `document.body.getBoundingClientRect().width === 0` is a
    // one-line detect, and this is the number that stops being zero.
    #[test]
    fn the_body_fills_the_viewport_less_its_margins() {
        let page = Page::new("", "<html><body id='b'></body></html>");
        let (x, y, width, _) = page.rect("b");
        assert_eq!(width, 1024.0 - 16.0, "the UA sheet's 8px on each side");
        assert_eq!((x, y), (8.0, 8.0));
    }

    #[test]
    fn a_block_fills_its_container_and_stacks_below_its_sibling() {
        let page = Page::new(
            "#a { height: 30px } #b { height: 20px }",
            "<html><body><div id='a'></div><div id='b'></div></body></html>",
        );
        assert_eq!(page.rect("a"), (0.0, 0.0, 1008.0, 30.0));
        assert_eq!(page.rect("b"), (0.0, 30.0, 1008.0, 20.0), "stacked, not overlapping");
    }

    #[test]
    fn width_padding_and_border_add_up_the_way_the_box_model_says() {
        let page = Page::new(
            "#a { width: 100px; height: 50px; padding: 10px; border: 2px solid red; margin: 5px }",
            "<html><body><div id='a'></div></body></html>",
        );
        // content-box is the initial box-sizing, so the border box is content + padding + border.
        let (x, y, width, height) = page.rect("a");
        assert_eq!((width, height), (100.0 + 20.0 + 4.0, 50.0 + 20.0 + 4.0));
        assert_eq!(x, 5.0, "the horizontal margin is applied");
        // And the vertical one is *not*, because it collapsed with `<body>`'s. That is correct CSS
        // and it is what taffy's block layout does: adjacent vertical margins collapse to the
        // larger, so the 5px sits inside body's 8px rather than adding to it.
        assert_eq!(y, 0.0, "the top margin collapsed through body's");
    }

    // The rule the case above ran into, on its own: adjacent vertical margins collapse to the
    // larger of the two, and horizontal ones never do.
    #[test]
    fn adjacent_vertical_margins_collapse_to_the_larger() {
        let page = Page::new(
            "#a { height: 10px; margin-bottom: 30px } #b { height: 10px; margin-top: 20px }",
            "<html><body><div id='a'></div><div id='b'></div></body></html>",
        );
        assert_eq!(page.rect("a").1, 0.0);
        assert_eq!(page.rect("b").1, 40.0, "10 + max(30, 20), not 10 + 30 + 20");
    }

    #[test]
    fn border_box_sizing_puts_the_padding_inside_the_width() {
        let page = Page::new(
            "#a { box-sizing: border-box; width: 100px; height: 50px; padding: 10px }",
            "<html><body><div id='a'></div></body></html>",
        );
        assert_eq!(page.rect("a"), (0.0, 0.0, 100.0, 50.0));
    }

    #[test]
    fn a_percentage_width_is_of_the_containing_block() {
        let page = Page::new(
            "#outer { width: 400px; height: 100px } #inner { width: 50%; height: 10px }",
            "<html><body><div id='outer'><div id='inner'></div></div></body></html>",
        );
        assert_eq!(page.rect("inner").2, 200.0);
    }

    #[test]
    fn auto_margins_centre_a_fixed_width_block() {
        let page = Page::new(
            "#a { width: 200px; height: 10px; margin-left: auto; margin-right: auto }",
            "<html><body><div id='a'></div></body></html>",
        );
        let (x, _, width, _) = page.rect("a");
        assert_eq!(width, 200.0);
        assert_eq!(x, (1008.0 - 200.0) / 2.0);
    }

    // `display: none` removes the element *and its subtree* from the box tree, which is why the
    // check happens before the children are visited rather than after.
    #[test]
    fn display_none_removes_a_subtree_and_not_just_an_element() {
        let page = Page::new(
            "#gone { display: none }",
            "<html><body><div id='gone'><div id='child'></div></div><div id='after'></div></body></html>",
        );
        assert!(!page.has_box("gone"));
        assert!(!page.has_box("child"), "the subtree goes with it");
        assert!(page.has_box("after"));
        assert_eq!(page.rect("after").1, 0.0, "and it takes up no space");
    }

    // The elements that are in the DOM and nowhere on the page.
    #[test]
    fn head_and_its_contents_generate_no_boxes() {
        let page = Page::new(
            "",
            "<html><head><title id='t'>x</title><style id='s'></style></head><body id='b'></body></html>",
        );
        assert!(!page.has_box("t"));
        assert!(!page.has_box("s"));
        assert!(page.has_box("b"));
        assert_eq!(page.rect("b").1, 8.0, "body starts at the top, not below an invisible head");
    }

    #[test]
    fn flex_children_share_the_row() {
        let page = Page::new(
            "#row { display: flex; width: 300px; height: 50px } #a, #b { flex: 1 }",
            "<html><body><div id='row'><div id='a'></div><div id='b'></div></div></body></html>",
        );
        assert_eq!(page.rect("a"), (0.0, 0.0, 150.0, 50.0));
        assert_eq!(page.rect("b"), (150.0, 0.0, 150.0, 50.0));
    }

    #[test]
    fn a_grid_places_its_children_in_the_tracks_it_was_given() {
        let page = Page::new(
            "#g { display: grid; grid-template-columns: 100px 200px; width: 300px; height: 40px }",
            "<html><body><div id='g'><div id='a'></div><div id='b'></div></div></body></html>",
        );
        assert_eq!(page.rect("a").2, 100.0);
        assert_eq!(page.rect("b"), (100.0, 0.0, 200.0, 40.0));
    }

    // `fr` is what makes a grid a grid: the free space, shared.
    #[test]
    fn fractional_tracks_share_what_is_left() {
        let page = Page::new(
            "#g { display: grid; grid-template-columns: 100px 1fr 2fr; width: 400px; height: 10px }",
            "<html><body><div id='g'><div id='a'></div><div id='b'></div><div id='c'></div></div></body></html>",
        );
        assert_eq!(page.rect("a").2, 100.0);
        assert_eq!(page.rect("b").2, 100.0, "one third of the 300px left");
        assert_eq!(page.rect("c").2, 200.0);
    }

    // `minmax()` and `repeat()` are one component and n components respectively -- a naive split on
    // whitespace gets both wrong, in opposite directions.
    #[test]
    fn minmax_is_one_track_and_repeat_is_several() {
        let page = Page::new(
            "#g { display: grid; grid-template-columns: repeat(3, 1fr); width: 300px; height: 10px }",
            "<html><body><div id='g'><div id='a'></div><div id='b'></div><div id='c'></div></div></body></html>",
        );
        assert_eq!(page.rect("a").2, 100.0);
        assert_eq!(page.rect("c").0, 200.0, "three tracks, not one");

        let page = Page::new(
            "#g { display: grid; grid-template-columns: minmax(50px, 1fr) 100px; width: 300px; height: 10px }",
            "<html><body><div id='g'><div id='a'></div><div id='b'></div></div></body></html>",
        );
        assert_eq!(page.rect("a").2, 200.0, "the flexible track takes what is left");
        assert_eq!(page.rect("b").2, 100.0);
    }

    // Line names size no track, so skipping them must not shift the ones around them.
    #[test]
    fn line_names_are_skipped_without_disturbing_the_tracks() {
        let page = Page::new(
            "#g { display: grid; grid-template-columns: [start] 100px [mid] 200px [end]; \
                  width: 300px; height: 10px }",
            "<html><body><div id='g'><div id='a'></div><div id='b'></div></div></body></html>",
        );
        assert_eq!(page.rect("a").2, 100.0);
        assert_eq!(page.rect("b").2, 200.0);
    }

    #[test]
    fn an_item_can_place_itself_on_a_line_or_span_tracks() {
        let page = Page::new(
            "#g { display: grid; grid-template-columns: repeat(4, 100px); width: 400px; height: 10px }\
             #a { grid-column: 3 / 5 } #b { grid-column: 1 / span 2 }",
            "<html><body><div id='g'><div id='a'></div><div id='b'></div></div></body></html>",
        );
        // Chrome's own answers. `#b` lands in a *second* row: auto-placement walks a cursor
        // forward, and columns 1-3 are behind where `#a` left it -- so the ten pixels of height are
        // split between two rows rather than shared by one.
        assert_eq!(page.rect("a"), (200.0, 0.0, 200.0, 5.0), "tracks 3 and 4, row 1");
        assert_eq!(page.rect("b"), (0.0, 5.0, 200.0, 5.0), "tracks 1 and 2, row 2");
    }

    #[test]
    fn the_viewport_size_is_what_percentages_and_the_root_resolve_against() {
        let page = Page::sized(
            "#a { width: 50%; height: 10px }",
            "<html><body id='b'><div id='a'></div></body></html>",
            Size { width: 400.0, height: 300.0 },
        );
        assert_eq!(page.rect("b").2, 384.0);
        assert_eq!(page.rect("a").2, 192.0);
    }

    // The claim D8 is about: taffy holds no per-node style, and the styles it reads are interned.
    #[test]
    fn a_page_of_identical_elements_needs_one_layout_style() {
        // `<!doctype html>`: the claim is about interning, and a quirks-mode document's `<html>` and
        // `<body>` take a viewport-filling variant each.
        let mut items = String::from("<!doctype html><html><body>");
        for i in 0..200 {
            items.push_str(&format!("<div class='item' id='i{i}'></div>"));
        }
        items.push_str("</body></html>");
        let page = Page::new(".item { height: 10px }", &items);
        assert_eq!(page.tree.len(), 202, "html, body, and 200 items");
        assert!(
            page.tree.distinct_styles() <= 4,
            "got {} layout styles for 203 boxes",
            page.tree.distinct_styles()
        );
        assert_eq!(page.rect("i199").1, 1990.0, "and they stack");
    }

    #[test]
    fn an_empty_document_lays_out_without_complaining() {
        let page = Page::new("", "");
        assert!(page.tree.len() <= 2);
    }

    // CSS 2.1 §10.1: the containing block of an absolutely positioned box is the padding box of its
    // nearest POSITIONED ancestor -- not of its parent. `<body>` is static and carries the UA
    // sheet's 8px margin, so a box that took its parent for its containing block would land at
    // y = 8 instead of at the viewport's own top edge.
    #[test]
    fn an_absolute_box_skips_its_static_ancestors_to_find_a_containing_block() {
        let page = Page::new(
            "#a { position: absolute; top: 0; left: 0; width: 10px; height: 10px }",
            "<html><body><div style='height:40px'></div><div id='a'></div></body></html>",
        );
        assert_eq!(page.tree.bounding_rect(page.tree.box_of(page.find("a")).unwrap()).y, 0.0);
        assert_eq!(page.tree.bounding_rect(page.tree.box_of(page.find("a")).unwrap()).x, 0.0);
    }

    // ...and with a positioned ancestor in between it measures from THAT box's padding edge, which
    // is inside its border and outside its padding.
    #[test]
    fn an_absolute_box_measures_from_its_positioned_ancestors_padding_edge() {
        let page = Page::new(
            "#p { position: relative; margin: 30px; border: 5px solid red; padding: 7px; \
                  height: 200px }
             #a { position: absolute; top: 0; left: 0; width: 10px; height: 10px }",
            "<html><body><div id='p'><div style='height:20px'></div><div id='a'></div></div>\
             </body></html>",
        );
        let p = page.tree.padding_box(page.tree.box_of(page.find("p")).unwrap());
        let a = page.tree.bounding_rect(page.tree.box_of(page.find("a")).unwrap());
        assert_eq!((a.x, a.y), (p.x, p.y), "the parent's padding edge, not its content edge");
        // The nesting is two deep, so this also proves the search is a walk and not one hop up.
        // Horizontally that is `<body>`'s 8px, `#p`'s 30px margin and its 5px border; vertically
        // `#p`'s top margin collapsed with `<body>`'s into one 30px, so the 8px is not there twice.
        assert_eq!((a.x, a.y), (8.0 + 30.0 + 5.0, 30.0 + 5.0));
    }

    /// An absolutely positioned box written in a line of inline content takes that line's top when nothing in flow
    /// comes before it there, and the place below the line when something does (Chrome 152: apple.com's region check
    /// mark, the first thing in its label, at the label's line; korvus put it a line lower).
    #[test]
    fn an_absolute_box_written_at_a_lines_start_is_at_its_top() {
        let html = "<body style='margin:0;font:17px sans-serif'>\
                    <div style='position:relative;padding:3px;border:1px solid'><span><span id=a style='position:absolute;display:block;left:14px'>X</span>abc</span></div>\
                    <div style='position:relative;padding:3px;border:1px solid'><span>abc <span id=b style='position:absolute;display:block;left:14px'>X</span>def</span></div></body>";
        let page = crate::Page::default();
        let dom = korvus_dom::Dom::parse(html);
        let media = crate::style::media::MediaContext::default();
        let laid = page.layout(&dom, &media);
        let top_of = |id: &str| {
            let n = (0..dom.len() as u32).find(|&n| dom.get_attribute(n, "id") == Some(id)).unwrap();
            laid.tree.border_box(laid.tree.box_of(n).unwrap()).y
        };
        // the first container's content starts at 1 + 3; its line is as tall as the second container's
        assert_eq!(top_of("a"), 4.0);
        let second_top = laid.tree.border_box(laid.tree.box_of((0..dom.len() as u32).filter(|&n| dom.tag_name(n) == Some("div")).nth(1).unwrap()).unwrap()).y;
        assert!(top_of("b") > second_top + 4.0 + 10.0, "after its line: {} in a box at {second_top}", top_of("b"));
    }

    // An empty box whose only children are empty boxes collapses its margins through, theirs with it (CSS 2 §8.3.1):
    // Chrome 150 lays `<div style="margin:24px 0 10px"><div style="margin:30px 0 5px"></div></div>` in a flow root
    // out 30 tall, and Wikipedia's empty `#siteNotice` (24px margins round an empty `#centralNotice`) 24.
    #[test]
    fn nested_empty_boxes_collapse_their_margins_through() {
        let page = crate::Page::default();
        let html = "<body style='margin:0'><div id=d style='display:flow-root'><div style='margin:24px 0 10px'><div style='margin:30px 0 5px'></div></div></div>\
                    <div id=w style='display:grid'><div><div style='margin:24px 0'><div></div></div></div></div></body>";
        let dom = korvus_dom::Dom::parse(html);
        let media = crate::style::media::MediaContext::default();
        let laid = page.layout(&dom, &media);
        let height = |id: &str| {
            let n = (0..dom.len() as u32).find(|&n| dom.get_attribute(n, "id") == Some(id)).unwrap();
            laid.tree.border_box(laid.tree.box_of(n).unwrap()).height
        };
        assert_eq!(height("d"), 30.0);
        assert_eq!(height("w"), 24.0);
    }

    // A relatively positioned sibling's shift is not in the flow, so not in the next box's static position either
    // (Chrome 150: the absolute box sits three 24px lines down, at 72, under the unshifted `top: 10%` box).
    #[test]
    fn a_relative_siblings_shift_is_not_in_the_next_static_position() {
        let page = Page::new(
            "",
            "<html><body style='margin:0'><div style='height:200px;position:relative'><div style='height:30px'></div>\
             <div style='position:relative;top:10%;height:30px'></div><div id='a' style='position:absolute;height:5px'></div>\
             </div></body></html>",
        );
        assert_eq!(page.rect("a").1, 60.0, "under the relative box's unshifted place (30 + 30), not its drawn one (+20)");
    }

    // taffy has no `static`, so a static box arrives as `Position::Relative` and taffy would offset
    // it by its insets. CSS says a static box ignores them.
    #[test]
    fn a_static_box_is_not_moved_by_its_insets() {
        let page = Page::new(
            "#a { top: 40px; left: 40px; height: 5px }",
            "<html><body><div style='height:10px'></div><div id='a'></div></body></html>",
        );
        assert_eq!(page.rect("a").0, 0.0, "left is ignored on a static box");
        assert_eq!(page.rect("a").1, 10.0, "and so is top: it stays under its sibling");
    }

    // CSS resolves a percentage padding against the containing block's INLINE size on ALL FOUR
    // sides: `padding-top: 10%` is 10% of the container's width. taffy resolves the block-axis pair
    // against the block size, which for an auto-height container is nothing at all — so this used
    // to come out as a horizontal pair and two zeroes.
    #[test]
    fn a_percentage_padding_resolves_against_the_inline_size_on_every_side() {
        let page = Page::new("#a { padding: 10% }", "<html><body><div id='a'></div></body></html>");
        let b = page.tree.box_of(page.find("a")).unwrap();
        let padding = page.tree.layout(b).padding;
        // `<body>`'s content box is 1024 less the UA sheet's 8px margins: 1008. A tenth of that is
        // 100.8, and 100.8 is what a browser reports: Chrome 150 answered `paddingTop` of
        // **100.797px** on the same shape (its LayoutUnit is 1/64px, so 100.8 quantises to
        // 100.796875) and a `getBoundingClientRect().height` of **201.59375**, with `offsetHeight`
        // 202. This used to read 101 and 202, because the layout was rounded before anyone saw it.
        // The padding is now that LayoutUnit itself, 100.796875 -- the value Chrome
        // reported above -- rather than the f32 100.8.
        assert_eq!(
            (padding.top, padding.right, padding.bottom, padding.left),
            (100.796875, 100.796875, 100.796875, 100.796875),
            "all four against the width, not two against the width and two against the height"
        );
        // And the box is as tall as its own padding, because it has no content. The INTEGER member
        // still rounds, which is the only place a browser rounds.
        assert!((page.rect("a").3 - 201.6).abs() < 0.01, "got {}", page.rect("a").3);
        let b = page.tree.box_of(page.find("a")).unwrap();
        assert_eq!(page.tree.offset_size(b).1, 202, "Chrome's offsetHeight");
    }

    // The axis rule is not only about what gets reported: the padding is part of the box, so a
    // content-box height has to have it added on.
    #[test]
    fn a_percentage_padding_is_added_to_a_content_box_height() {
        let page = Page::new(
            "#a { height: 100px; padding: 10% }",
            "<html><body><div id='a'></div></body></html>",
        );
        // Chrome: `getBoundingClientRect().height` 301.59375, `offsetHeight` 302.
        assert!((page.rect("a").3 - 301.6).abs() < 0.01, "got {}", page.rect("a").3);
        let b = page.tree.box_of(page.find("a")).unwrap();
        assert_eq!(page.tree.offset_size(b).1, 302, "100px of content plus 100.8px above and below");
    }

    // The basis is the *containing block*, so the same style in a narrower container gives a
    // smaller padding — which is what makes this a rule rather than a constant.
    // An absolutely positioned box at its static position is put there from the places of the boxes above it, whatever
    // place it held: moved from its own, it kept a float residue of that place, so a relayout that left it where the
    // last pass had put it placed it a few ulps -- or, across a snapping edge, 1/64px -- from a layout from nothing.
    #[test]
    fn a_static_position_does_not_depend_on_where_the_box_was() {
        let mut page = Page::new(
            ".fr { line-height: 1.37; font-size: 14.3px } .sr { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden }",
            "<body><div class=fr>alpha<br>beta</div><div class=fr style='position: relative; padding: 3.3px 16.1px'><div>block</div>\
             <p style='margin: 2.7px 0'>para</p><span class=sr id=s>hidden</span></div></body>",
        );
        let b = page.tree.box_of(page.find("s")).unwrap() as usize;
        let at = page.tree.st[b].unrounded.location;
        assert!(at.y.fract() != 0.0, "a place with a fraction: {at:?}");
        for k in 1..200 {
            page.tree.st[b].unrounded.location = Point { x: at.x + k as f32 * 0.37, y: at.y - k as f32 * 1.13 };
            page.tree.place_at_static_positions();
            let now = page.tree.st[b].unrounded.location;
            assert_eq!((now.x.to_bits(), now.y.to_bits()), (at.x.to_bits(), at.y.to_bits()), "from {k}: {now:?} for {at:?}");
        }
    }

    #[test]
    fn the_percentage_padding_basis_is_the_container_and_not_the_viewport() {
        let page = Page::new(
            "#outer { width: 200px } .p { padding: 10% }",
            "<html><body><div class='p' id='wide'></div>\
             <div id='outer'><div class='p' id='narrow'></div></div></body></html>",
        );
        assert!((page.rect("wide").3 - 201.6).abs() < 0.01, "10% of <body>'s 1008px, twice");
        assert_eq!(page.rect("narrow").3, 40.0, "10% of #outer's 200px, twice");
        // Two boxes, one interned style, two different used paddings: the resolution is per box.
        let narrow = page.tree.layout(page.tree.box_of(page.find("narrow")).unwrap()).padding;
        assert_eq!((narrow.top, narrow.left), (20.0, 20.0));
    }

    // A percentage padding on a box whose own container has one: the basis has to be the container's
    // *resolved* content width, which is only known after the container itself has settled.
    #[test]
    fn a_percentage_padding_nests() {
        let page = Page::new(
            "#outer { width: 500px; padding: 10%; box-sizing: border-box }
             #inner { padding: 10% }",
            "<html><body><div id='outer'><div id='inner'></div></div></body></html>",
        );
        // #outer is 500px wide border-box with 100.8px of padding on each side (10% of <body>'s
        // 1008), so its content box is 500 - 201.6 = 298.4 wide.
        let inner = page.tree.layout(page.tree.box_of(page.find("inner")).unwrap()).padding;
        assert_eq!(inner.top, inner.left, "still the same on both axes one level down");
        assert!(
            (inner.left - 29.84).abs() < 1.0,
            "10% of #outer's 298.4px content box, got {}",
            inner.left
        );
    }

    // An absolutely positioned box resolves its percentages against its containing block's PADDING
    // box, which after the re-parenting above is simply its box-tree parent.
    #[test]
    fn an_absolute_boxs_percentage_padding_uses_its_containing_block() {
        let page = Page::new(
            "#p { position: relative; width: 400px; padding: 50px; height: 300px }
             #a { position: absolute; padding: 10% }",
            "<html><body><div id='p'><div id='a'></div></div></body></html>",
        );
        // #p's padding box is its 500px border box (400 content + 100 padding); a tenth is 50.
        let a = page.tree.layout(page.tree.box_of(page.find("a")).unwrap()).padding;
        assert_eq!((a.top, a.right, a.bottom, a.left), (50.0, 50.0, 50.0, 50.0));
    }

    // The same insets on a relatively positioned box DO move it — the two must not be conflated in
    // the other direction either.
    #[test]
    fn a_relative_box_is_moved_by_its_insets() {
        let page = Page::new(
            "#a { position: relative; top: 40px; left: 40px; height: 5px }",
            "<html><body><div style='height:10px'></div><div id='a'></div></body></html>",
        );
        assert_eq!(page.rect("a").0, 40.0);
        assert_eq!(page.rect("a").1, 50.0, "10px of sibling plus the 40px shift");
    }

    // ---------------------------------------------------------------------------------------
    // P9d: replaced elements and form controls. Every number with "Chrome" beside it was read off
    // Chrome 150.0.7871.46 over CDP (`getBoundingClientRect()`), on a fixture holding exactly the
    // markup in the test. A number that is NOT Chrome's says so, and says why.
    // ---------------------------------------------------------------------------------------

    fn size_of(page: &Page, id: &str) -> (f32, f32) {
        let (_, _, w, h) = page.rect(id);
        (w, h)
    }

    // The brief's table, the rows the layout alone decides. Before P9d: 0x17 each (a 0-wide strut).
    #[test]
    fn a_replaced_element_with_nothing_said_is_the_default_object_size() {
        let page = Page::new(
            "",
            "<html><body><canvas id='canvas'></canvas><video id='video'></video>\
             <svg id='svg'></svg><object id='obj'></object></body></html>",
        );
        assert_eq!(size_of(&page, "canvas"), (300.0, 150.0), "Chrome 300x150");
        assert_eq!(size_of(&page, "video"), (300.0, 150.0), "Chrome 300x150");
        assert_eq!(size_of(&page, "svg"), (300.0, 150.0), "Chrome 300x150");
        assert_eq!(size_of(&page, "obj"), (300.0, 150.0), "Chrome 300x150");
    }

    #[test]
    fn an_img_is_its_attributes_and_nothing_without_them() {
        let page = Page::new(
            "",
            "<html><body><img id='sized' width=40 height=30 src='px.gif'><img id='none'>\
             <img id='w' width=50></body></html>",
        );
        assert_eq!(size_of(&page, "sized"), (40.0, 30.0), "Chrome 40x30");
        assert_eq!(size_of(&page, "none"), (0.0, 0.0), "Chrome 0x0");
        assert_eq!(size_of(&page, "w"), (50.0, 0.0), "Chrome 50x0");
    }

    #[test]
    fn an_iframe_is_300_by_150_inside_its_ua_border() {
        let page = Page::new(
            "",
            "<html><body><iframe id='f'></iframe><iframe id='g' width=100 height=50></iframe>\
             </body></html>",
        );
        assert_eq!(size_of(&page, "f"), (304.0, 154.0), "Chrome 304x154");
        assert_eq!(size_of(&page, "g"), (104.0, 54.0), "Chrome 104x54");
    }

    // `<audio controls>` has no natural size; the UA sheet's 300x54 applies because it is replaced.
    #[test]
    fn audio_with_controls_takes_the_ua_sheets_size() {
        let page = Page::new(
            "",
            "<html><body><audio id='a' controls></audio><audio id='b'></audio></body></html>",
        );
        assert_eq!(size_of(&page, "a"), (300.0, 54.0), "Chrome 300x54");
        assert!(!page.has_box("b"), "Chrome 0x0 at the origin: no box");
    }

    #[test]
    fn a_canvas_is_its_bitmap_and_css_scales_it_through_the_ratio() {
        let page = Page::new(
            "",
            "<html><body><canvas id='attr' width=50 height=20></canvas>\
             <canvas id='w' style='width:100px'></canvas>\
             <canvas id='h' style='height:75px'></canvas>\
             <canvas id='maxw' style='max-width:100px'></canvas>\
             <canvas id='ar' style='aspect-ratio:1/1'></canvas>\
             <canvas id='minw' width=100 height=50 style='min-width:200px'></canvas>\
             <canvas id='bb' width=100 height=50 style='box-sizing:border-box;padding:10px;width:100px'></canvas>\
             </body></html>",
        );
        assert_eq!(size_of(&page, "attr"), (50.0, 20.0), "Chrome 50x20");
        assert_eq!(size_of(&page, "w"), (100.0, 50.0), "Chrome 100x50");
        assert_eq!(size_of(&page, "h"), (150.0, 75.0), "Chrome 150x75");
        assert_eq!(size_of(&page, "maxw"), (100.0, 50.0), "Chrome 100x50");
        assert_eq!(size_of(&page, "ar"), (300.0, 300.0), "Chrome 300x300");
        assert_eq!(size_of(&page, "minw"), (200.0, 100.0), "Chrome 200x100");
        assert_eq!(size_of(&page, "bb"), (100.0, 60.0), "Chrome 100x60");
    }

    #[test]
    fn a_video_without_metadata_takes_its_attributes_and_the_default_for_the_rest() {
        let page = Page::new(
            "",
            "<html><body><video id='both' width=100 height=60></video><video id='w' width=100></video>\
             <video id='poster' poster='nope.png'></video></body></html>",
        );
        assert_eq!(size_of(&page, "both"), (100.0, 60.0), "Chrome 100x60");
        assert_eq!(size_of(&page, "w"), (100.0, 150.0), "Chrome 100x150");
        assert_eq!(size_of(&page, "poster"), (300.0, 150.0), "Chrome 300x150");
    }

    #[test]
    fn an_svg_is_its_width_and_height_and_a_viewbox_is_a_ratio() {
        let page = Page::new(
            "",
            "<html><body><svg id='attr' width=30 height=30></svg><svg id='w' width=100></svg>\
             <svg id='px' width='30px' height='20px'></svg>\
             <div style='width:600px'><svg id='vb' viewBox='0 0 10 5'></svg></div>\
             <svg id='vbw' viewBox='0 0 10 5' width=100></svg>\
             <svg id='css' style='width:24px;height:24px'></svg></body></html>",
        );
        assert_eq!(size_of(&page, "attr"), (30.0, 30.0), "Chrome 30x30");
        assert_eq!(size_of(&page, "w"), (100.0, 150.0), "Chrome 100x150");
        assert_eq!(size_of(&page, "px"), (30.0, 20.0), "Chrome 30x20");
        assert_eq!(size_of(&page, "vb"), (600.0, 300.0), "Chrome 600x300: a ratio alone stretches");
        assert_eq!(size_of(&page, "vbw"), (100.0, 50.0), "Chrome 100x50");
        assert_eq!(size_of(&page, "css"), (24.0, 24.0), "Chrome 24x24");
    }

    // `img { max-width: 100%; height: auto }` -- the reset every framework ships. The ratio from the
    // attributes sizes the height before any image has loaded.
    #[test]
    fn an_img_keeps_its_attribute_ratio_under_max_width_and_percent_width() {
        let page = Page::new(
            "",
            "<html><body><div style='width:600px'>\
             <img id='mw' width=1200 height=800 style='max-width:100%;height:auto'></div>\
             <div style='width:600px'><img id='full' width=40 height=30 style='width:100%;height:auto'></div>\
             <img id='pad' width=40 height=30 style='display:block;padding:5px;border:1px solid'>\
             </body></html>",
        );
        assert_eq!(size_of(&page, "mw"), (600.0, 400.0), "Chrome 600x400");
        assert_eq!(size_of(&page, "full"), (600.0, 450.0), "Chrome 600x450");
        assert_eq!(size_of(&page, "pad"), (52.0, 42.0), "Chrome 52x42");
    }

    // A replaced box does not stretch to its block container's width; in a flex row it does stretch
    // on the cross axis when its height is auto.
    #[test]
    fn a_block_level_replaced_box_keeps_its_own_width_and_a_flex_item_stretches() {
        let page = Page::new(
            "",
            "<html><body><canvas id='block' style='display:block'></canvas>\
             <div style='display:flex'><canvas id='flex'></canvas><div style='height:200px;width:10px'></div></div>\
             <div style='display:flex;align-items:flex-start'><canvas id='start'></canvas></div>\
             </body></html>",
        );
        assert_eq!(size_of(&page, "block"), (300.0, 150.0), "Chrome 300x150, not 1008 wide");
        assert_eq!(size_of(&page, "flex"), (300.0, 200.0), "Chrome 300x200");
        assert_eq!(size_of(&page, "start"), (300.0, 150.0), "Chrome 300x150");
    }

    // A canvas's fallback content is not rendered, so it neither sizes the canvas nor has a box.
    #[test]
    fn a_replaced_elements_children_get_no_box() {
        let page = Page::new(
            "",
            "<html><body><canvas id='c'><div id='fallback' style='width:900px;height:900px'>x</div>\
             </canvas></body></html>",
        );
        assert_eq!(size_of(&page, "c"), (300.0, 150.0), "Chrome 300x150");
        assert!(!page.has_box("fallback"));
    }

    /// A theme for the tests below. DELIBERATELY NOT any browser's: the thickness a persona wears is
    /// its bundle's, and the live test (`korvus-runtime/tests/scrollbars_live.rs`) asserts Chrome's
    /// own numbers under all four captured bundles. Here the question is only whether the arithmetic
    /// follows the theme, so the comments quote Chrome's number under its own (Linux) theme and the
    /// assertions are written in terms of `THEME`.
    const THEME: spec::ThemeProfile = spec::ThemeProfile {
        scrollbar_thickness: 11.0,
        scrollbar_thin_thickness: 7.0,
        menu_list_inline_padding: 23.0,
        menu_list_block_padding: 3.0,
        font_map: 0,
        min_logical_font_size: 0.0,
        submit_label: "",
        reset_label: "",
    };

    fn themed(css: &str, html: &str) -> Page {
        Page::themed(css, html, Size { width: 1024.0, height: 768.0 }, THEME)
    }

    fn client_of(page: &Page, id: &str) -> (i32, i32) {
        page.tree.client_size(page.tree.box_of(page.find(id)).unwrap())
    }

    /// Blink's text-field widths from the font table's OS/2 average and maximum character widths:
    /// `avg' = max(avg, round(avg))`, then `ceil(avg' * chars + (max - avg'))` for a field and
    /// `ceil(avg' * chars)` for a textarea. See `layout::replaced::char_widths`.
    fn arial_widths(px: f32) -> (f32, f32) {
        let m = &crate::font_table::ARIAL;
        let size = crate::font::used_size(px);
        let avg = m.avg_char_width.unwrap() * size;
        (avg.max(avg.round()), (m.max_char_width.unwrap() * size).round())
    }

    // `<input>` with Chrome's UA font, padding and border supplied as author CSS. Every width is
    // Chrome 150's, because the font table now carries the two numbers Blink sizes a field in --
    // before P11b this was the `0`-advance path and 28px short at 13.333px.
    #[test]
    fn an_input_is_twenty_characters_wide_and_one_line_tall() {
        let page = Page::new(
            "input { font: 13.3333px Arial; padding: 1px 2px; border: 2px inset }
             .u { font: 16px Arial; padding: 0; border: 0 }",
            "<html><body><input id='ua'><input id='s5' class='u' size=5><input id='s0' class='u' size=0>\
             <input id='pw' class='u' type=password><input id='btn' class='u' type=button value='go'>\
             </body></html>",
        );
        assert_eq!(size_of(&page, "ua"), (185.0, 21.0), "Chrome 185x21");
        assert_eq!(size_of(&page, "s5"), (68.0, 18.0), "Chrome 68x18");
        assert_eq!(size_of(&page, "s0").0, 207.0, "size=0 is the default 20; Chrome 207");
        assert_eq!(size_of(&page, "pw"), (207.0, 18.0), "Chrome 207x18");
        let (bw, bh) = size_of(&page, "btn");
        assert!((bw - 17.797).abs() < 0.01, "Chrome 17.797: the label's width, got {bw}");
        assert_eq!(bh, 18.0, "Chrome 18");
        // The formula, at the sizes Chrome was asked at (probe: `<input size=1|2|3|10>`, no padding).
        for (px, sizes) in [(13.3333, [25.0, 33.0, 41.0, 97.0]), (16.0, [30.0, 40.0, 49.0, 114.0]), (100.0, [185.0, 243.0, 301.0, 707.0])] {
            let (avg, max) = arial_widths(px);
            for (n, chrome) in [1.0, 2.0, 3.0, 10.0].into_iter().zip(sizes) {
                assert_eq!((avg * n + (max - avg)).ceil(), chrome, "Arial {px}px size={n}");
            }
        }
    }

    // `<textarea>`: rows x line height, `cols` average characters, and -- whatever its value -- the
    // theme's scrollbar on the width, because a textarea's block axis is `auto`. Chrome 150 (Linux
    // theme): 201x36, 62x18, 108x90.
    #[test]
    fn a_textarea_is_cols_wide_and_rows_tall_whatever_its_value() {
        let page = themed(
            ".u { font: 16px Arial; padding: 0; border: 0 }",
            "<html><body><textarea id='d' class='u'></textarea>\
             <textarea id='v' class='u' cols=5 rows=1>hello world this wraps around</textarea>\
             <textarea id='r' class='u' cols=10 rows=5></textarea>\
             <textarea id='h' class='u' style='overflow:hidden'></textarea>\
             <textarea id='s' class='u' style='overflow:scroll'></textarea></body></html>",
        );
        let (avg, _) = arial_widths(16.0);
        let bar = THEME.scrollbar_thickness;
        assert_eq!(size_of(&page, "d"), ((avg * 20.0).ceil() + bar, 36.0), "Chrome 201x36");
        assert_eq!(size_of(&page, "v"), ((avg * 5.0).ceil() + bar, 18.0), "Chrome 62x18");
        assert_eq!(size_of(&page, "r"), ((avg * 10.0).ceil() + bar, 90.0), "Chrome 108x90");
        // `overflow: hidden` adds nothing; `scroll` adds the bar to both axes (Chrome, 13.333px
        // Courier New: auto 181x36, hidden 166x36, scroll 181x51).
        assert_eq!(size_of(&page, "h"), ((avg * 20.0).ceil(), 36.0));
        assert_eq!(size_of(&page, "s"), ((avg * 20.0).ceil() + bar, 36.0 + bar));
        // A shown bar is taken from INSIDE the box: Chrome's `overflow: scroll` textarea above is
        // client 164x34 in a 181x51 box.
        assert_eq!(client_of(&page, "s"), ((avg * 20.0).ceil() as i32, 36));
    }

    // A drop-down is its widest label (optgroup options indented four spaces), one font Height tall,
    // plus the theme's internal paddings. Chrome 150 (Linux theme): 72x19 and 81x19; an empty one at
    // `font-size: 0` is the paddings alone (20x2), and `appearance: none` drops them.
    #[test]
    fn a_select_is_its_widest_option_label() {
        let page = themed(
            ".u { font: 16px Arial; padding: 0; border: 0 }",
            "<html><body><select id='g' class='u'><option>a</option>\
             <optgroup label='g'><option>hello</option></optgroup></select>\
             <select id='l' class='u'><option label='WWWW'>a</option></select>\
             <select id='n' class='u' style='appearance:none'><option>a</option></select></body></html>",
        );
        let arial = &crate::font_table::ARIAL;
        let label = |s: &str| (arial.width(s) * 16.0).ceil();
        let (pw, ph) = (THEME.menu_list_inline_padding, THEME.menu_list_block_padding);
        assert_eq!(size_of(&page, "g"), (label("    hello") + pw, 17.0 + ph), "Chrome 72x19");
        assert_eq!(size_of(&page, "l"), (label("WWWW") + pw, 17.0 + ph), "Chrome 81x19");
        assert_eq!(size_of(&page, "n"), (label("a"), 17.0), "no appearance, no theme paddings");
    }

    // ── scrollbars ──────────────────────────────────────────────────────────────────
    //
    // Chrome 150 on Linux, 780px window; its numbers are under its own 15px theme and quoted in the
    // comments. The assertions follow `THEME`, whose thickness is not Chrome's on purpose.

    // An `overflow: scroll` box always gives the bar's thickness up, from the content box, on both
    // axes. Chrome: 200x100 -> client 185x85; with 10px padding and a 3px border, content-box
    // 226x126 / client 205x105 and border-box 200x100 / client 179x79.
    #[test]
    fn an_overflow_scroll_box_gives_its_client_box_to_both_scrollbars() {
        let page = themed(
            ".b { width: 200px; height: 100px; overflow: scroll }",
            "<html><body><div id='s' class='b'></div>\
             <div id='p' class='b' style='padding:10px;border:3px solid'></div>\
             <div id='bb' class='b' style='padding:10px;border:3px solid;box-sizing:border-box'></div>\
             </body></html>",
        );
        let t = THEME.scrollbar_thickness as i32;
        assert_eq!(size_of(&page, "s"), (200.0, 100.0));
        assert_eq!(client_of(&page, "s"), (200 - t, 100 - t), "Chrome 185x85");
        assert_eq!(size_of(&page, "p"), (226.0, 126.0), "Chrome 226x126");
        assert_eq!(client_of(&page, "p"), (220 - t, 120 - t), "Chrome 205x105");
        assert_eq!(size_of(&page, "bb"), (200.0, 100.0));
        assert_eq!(client_of(&page, "bb"), (194 - t, 94 - t), "Chrome 179x79");
    }

    // `auto` shows a bar only on the axis whose content does not fit, and showing one can make the
    // other axis overflow. Chrome: empty 200x100; 300 tall -> 185x100 and the child is 185 wide;
    // 300 wide -> 200x85; a 190x95 child over a 300x1 one -> 185x85, both bars.
    #[test]
    fn an_overflow_auto_box_shows_a_scrollbar_only_where_its_content_overflows() {
        let page = themed(
            ".b { width: 200px; height: 100px; overflow: auto }",
            "<html><body><div id='e' class='b'></div>\
             <div id='tall' class='b'><div id='c' style='height:300px'></div></div>\
             <div id='wide' class='b'><div style='width:300px;height:50px'></div></div>\
             <div id='both' class='b'><div style='width:190px;height:95px'></div><div style='width:300px;height:1px'></div></div>\
             <div id='fits' class='b'><div style='width:200px;height:100px'></div></div>\
             <div id='text' class='b' style='font:16px Arial'>one two three four five six seven eight \
             nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen \
             twenty twenty-one twenty-two twenty-three twenty-four twenty-five twenty-six</div>\
             </body></html>",
        );
        let t = THEME.scrollbar_thickness as i32;
        assert_eq!(client_of(&page, "e"), (200, 100), "Chrome 200x100");
        assert_eq!(client_of(&page, "tall"), (200 - t, 100), "Chrome 185x100");
        assert_eq!(size_of(&page, "c").0, (200 - t) as f32, "the content reflows into it: Chrome 185");
        assert_eq!(client_of(&page, "wide"), (200, 100 - t), "Chrome 200x85");
        assert_eq!(client_of(&page, "both"), (200 - t, 100 - t), "Chrome 185x85");
        assert_eq!(client_of(&page, "fits"), (200, 100), "content that exactly fits shows nothing");
        assert_eq!(client_of(&page, "text"), (200 - t, 100), "lines of text overflow like a box does");
    }

    // `scrollbar-width` picks the theme's thickness; `scrollbar-gutter: stable` reserves the vertical
    // one on a scroll container that does not overflow. Chrome: thin 190x90, none 200x100, hidden +
    // stable 185x100, visible + stable 200x100.
    #[test]
    fn scrollbar_width_and_gutter_follow_the_theme() {
        let page = themed(
            ".b { width: 200px; height: 100px }",
            "<html><body><div id='thin' class='b' style='overflow:scroll;scrollbar-width:thin'></div>\
             <div id='none' class='b' style='overflow:scroll;scrollbar-width:none'></div>\
             <div id='gut' class='b' style='overflow:hidden;scrollbar-gutter:stable'></div>\
             <div id='vis' class='b' style='scrollbar-gutter:stable'></div></body></html>",
        );
        let (t, thin) = (THEME.scrollbar_thickness as i32, THEME.scrollbar_thin_thickness as i32);
        assert_eq!(client_of(&page, "thin"), (200 - thin, 100 - thin), "Chrome 190x90");
        assert_eq!(client_of(&page, "none"), (200, 100), "Chrome 200x100");
        assert_eq!(client_of(&page, "gut"), (200 - t, 100), "Chrome 185x100");
        assert_eq!(client_of(&page, "vis"), (200, 100), "not a scroll container: Chrome 200x100");
    }

    // A shrink-to-fit box counts its scrollbars in its own width. Chrome: an absolutely positioned
    // `overflow: auto` box over a 50x300 child is 65 wide (client 50); an `inline-block` with
    // `overflow: scroll` over a 50x30 child is 65x45.
    #[test]
    fn a_shrink_to_fit_scroll_container_is_as_wide_as_its_content_and_its_scrollbar() {
        let page = themed(
            "",
            "<html><body><div id='a' style='position:absolute;height:100px;overflow:auto'>\
             <div style='width:50px;height:300px'></div></div>\
             <div id='i' style='display:inline-block;overflow:scroll'><div style='width:50px;height:30px'></div></div>\
             </body></html>",
        );
        let t = THEME.scrollbar_thickness;
        assert_eq!(size_of(&page, "a"), (50.0 + t, 100.0), "Chrome 65x100");
        assert_eq!(client_of(&page, "a"), (50, 100), "Chrome 50x100");
        assert_eq!(size_of(&page, "i"), (50.0 + t, 30.0 + t), "Chrome 65x45");
    }

    // The VIEWPORT's scrollbar narrows the layout viewport, not the window. Chrome, 780px window over
    // a 5000px page: innerWidth 780, documentElement.clientWidth 765, body 765 wide, a
    // `position: fixed; right: 0` 10px box at x 755. `html { overflow: hidden }` shows none, and a
    // page that fits shows none.
    #[test]
    fn the_viewport_scrollbar_narrows_the_layout_viewport() {
        let viewport = Size { width: 780.0, height: 600.0 };
        let tall = "<html><body id='b' style='margin:0'><div style='height:5000px'></div>\
                    <div id='f' style='position:fixed;right:0;top:0;width:10px;height:10px'></div></body></html>";
        let page = Page::themed("", tall, viewport, THEME);
        let t = THEME.scrollbar_thickness;
        let root = page.tree.root_box().unwrap();
        assert_eq!(page.tree.client_size(root), ((780.0 - t) as i32, 600), "Chrome 765x600");
        assert_eq!(size_of(&page, "b").0, 780.0 - t, "Chrome 765");
        assert_eq!(page.rect("f").0, 780.0 - t - 10.0, "Chrome 755");

        let hidden = Page::themed("html { overflow: hidden }", tall, viewport, THEME);
        assert_eq!(hidden.tree.client_size(hidden.tree.root_box().unwrap()), (780, 600));

        let short = Page::themed("", "<html><body><div style='height:50px'></div></body></html>", viewport, THEME);
        assert_eq!(short.tree.client_size(short.tree.root_box().unwrap()), (780, 600));

        // The body's `overflow` is the viewport's when the root's is `visible`: `body { overflow-y:
        // scroll }` shows the bar on a page that fits, and the body itself reserves nothing.
        let body = Page::themed("body { overflow-y: scroll; margin: 0 }", "<html><body id='b'></body></html>", viewport, THEME);
        assert_eq!(body.tree.client_size(body.tree.root_box().unwrap()), ((780.0 - t) as i32, 600));
        assert_eq!(client_of(&body, "b").0, (780.0 - t) as i32);
    }

    // No theme, no scrollbar: the default a context with no persona lays out with.
    #[test]
    fn with_no_theme_nothing_reserves_a_scrollbar() {
        let page = Page::sized(
            ".b { width: 200px; height: 100px; overflow: scroll }",
            "<html><body><div id='s' class='b'></div><div style='height:5000px'></div></body></html>",
            Size { width: 780.0, height: 600.0 },
        );
        assert_eq!(client_of(&page, "s"), (200, 100));
        assert_eq!(page.tree.client_size(page.tree.root_box().unwrap()), (780, 600));
    }

    #[test]
    fn a_block_level_button_shrinks_to_fit_and_a_flex_column_still_stretches_it() {
        let page = Page::new(
            ".u { font: 16px Arial; padding: 0; border: 0 }",
            "<html><body><div style='display:flex;flex-direction:column;width:300px'>\
             <button id='col' class='u'>go</button></div>\
             <button id='blk' class='u' style='display:block'>go</button></body></html>",
        );
        assert_eq!(size_of(&page, "col"), (300.0, 18.0), "Chrome 300x18");
        let (w, h) = size_of(&page, "blk");
        assert!((w - 17.797).abs() < 0.01, "Chrome 17.797, got {w}");
        assert_eq!(h, 18.0, "Chrome 18");
    }

    // An absolutely positioned box of text shrinks to fit -- the same fit-content question the
    // block-level button asks, answered by the same `Fit::Available`. Chrome: 76.484 and 53.375.
    #[test]
    fn an_absolutely_positioned_box_of_text_shrinks_to_fit() {
        let page = Page::new(
            "body { font: 16px Arial }",
            "<html><body><div style='position:relative;width:600px;height:100px'>\
             <div id='a' style='position:absolute;top:0;left:0'>hello world</div>\
             <div id='c' style='position:absolute;top:60px;left:0;text-align:center'>hi there</div>\
             <div id='lr' style='position:absolute;top:40px;left:0;right:0'>hello</div>\
             </div></body></html>",
        );
        assert!((size_of(&page, "a").0 - 76.484).abs() < 0.01, "{:?}", size_of(&page, "a"));
        assert!((size_of(&page, "c").0 - 53.375).abs() < 0.02, "{:?}", size_of(&page, "c"));
        assert_eq!(size_of(&page, "lr").0, 600.0, "left and right decide the width");
    }

    // CSS Overflow 3 section 3.3. Chrome 150 on this page: html.scrollHeight 5008,
    // body.scrollHeight 5000, and the document scrolls; before P9d korvus made <body> the scroller
    // and <html> reported the viewport.
    #[test]
    fn body_overflow_is_propagated_to_the_viewport() {
        let page = Page::new(
            "html, body { height: 100% } body { overflow-x: hidden }",
            "<html><body id='b'><div style='height:5000px'></div></body></html>",
        );
        let root = page.tree.root_box().unwrap();
        let body = page.tree.box_of(page.find("b")).unwrap();
        assert_eq!(page.tree.scroll_size(root).1, 5008, "Chrome 5008");
        assert_eq!(page.tree.scroll_size(body).1, 5000, "Chrome 5000");
        assert!(!page.tree.is_scroll_container(body), "the body is not the scroller");
        assert_eq!(page.tree.scroll_extent(root).1, 5008 - 768);
    }

    // Not propagated when the root's own overflow is not `visible`: the body keeps it and scrolls.
    // Chrome 150 on this page: html.scrollHeight = its viewport, body.scrollHeight 5000.
    #[test]
    fn body_overflow_stays_on_the_body_when_the_root_has_its_own() {
        let page = Page::new(
            "html, body { height: 100% } html { overflow-y: auto } body { overflow-x: hidden }",
            "<html><body id='b'><div style='height:5000px'></div></body></html>",
        );
        let body = page.tree.box_of(page.find("b")).unwrap();
        assert!(page.tree.is_scroll_container(body));
        assert_eq!(page.tree.scroll_size(body).1, 5000, "Chrome 5000");
    }
}
