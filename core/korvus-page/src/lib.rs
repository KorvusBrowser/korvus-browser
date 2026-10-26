//! `page` — the page pipeline: style → layout → hit test.
//!
//! # Why this exists
//!
//! Korvus can already look like Chrome in four of the five ways a detector checks: the native
//! surface, the TLS/JA4 fingerprint, neuromotor input, and canvas/WebGL pixels. The fifth is
//! geometry, and today it is not *approximately* wrong — it is uniformly zero:
//!
//! ```text
//! document.body.getBoundingClientRect().width === 0
//! ```
//!
//! That is a one-line detect on every page, and no number of new WebAPI interfaces closes it: the
//! members that *depend* on layout (`offsetWidth`, `getComputedStyle` resolved values,
//! `IntersectionObserver`) can only be further lies until real style and layout exist. This crate
//! is that real style and layout.
//!
//! # Shape
//!
//! One plugin, not three, because it is one pipeline — a stylesheet becomes matched rules becomes a
//! computed style becomes a box becomes a rect, and splitting that across crates would force every
//! intermediate type `pub` for no one's benefit. The rasterizer stays a *separate* plugin because it
//! is a **device**, not a stage: which pixels a page produces is a swappable backend (Skia locally,
//! or ops relayed to a real phone), and absorbing it would hardcode that choice.
//!
//! # Degrading
//!
//! Loading Korvus without this plugin must keep working. Consumers ask the `Context` and fall back:
//!
//! ```ignore
//! match ctx.get::<LayoutHandle>() {
//!     Some(h) => h.border_box(node),
//!     None => Rect::ZERO, // page plugin not loaded — today's behavior, unchanged
//! }
//! ```

pub mod animation;
pub mod effects; // `transform` and `opacity`: how a box shows beyond where layout put it (D77).
mod flat;
pub mod font;
pub mod font_table;
mod incremental;
pub mod layout;
pub mod paint;
pub mod perf_counters;
pub mod style;
pub mod value;
pub mod css_color;
mod verify;

use crate::layout::geometry::{Rect, ScrollSnapshot};
use crate::layout::resolved;
use crate::layout::tree::BoxTree;
use crate::style::compute::{compute_animated, ComputeContext, InlineStyle, ParentFormatting};
use crate::style::computed::{StyleArena, StyleKey};
use crate::style::declarative::{self, AnimatedValue, Declarative, DeclarativeUpdate};
use crate::style::container::{ContainerCondition, ContainerReads, QueryContainer};
use crate::style::matching::{Adoptions, MatchEnv, MatchedRule, Matcher, SheetScope};
use crate::style::media::MediaContext;
use crate::style::parse::{parse_inline_style, parse_stylesheet};
use crate::style::property::PropertyId;
use crate::style::resolve::px_of;
use crate::style::rules::{ElementKeys, Origin, RuleIndex, RuleRef, Span, Stylesheet};
use crate::style::ua;
use korvus_dom::{AncestorBloom, Dom, NodeId, NodeKind};
use spec::kernel::{Context, Plugin, PluginConfig};
use std::sync::{Arc, Mutex};

/// The pipeline's state for one page: the stylesheets, the rule index over them, and the arena of
/// distinct computed styles.
///
/// It is `Send + Sync` because it lives on the kernel [`Context`] as a service. The interior
/// mutability is one `Mutex` around the whole thing rather than one per field: every style query
/// touches all of it, and in practice one V8 isolate owns one thread, so the lock is never
/// contended.
#[derive(Debug, Default)]
pub struct Page {
    state: Mutex<PageState>,
}

/// The inputs [`Page::layout_fingerprint_of`] hashes -- DOM revision and length, sheet
/// revision, viewport, container generation, animation revision, natural-image generation and the four
/// theme metrics. All `Copy`, so the fingerprint cache compares them without allocating.
type FpInputs = (u64, u64, u64, u32, u32, u64, u64, u64, u32, u32, u32, u32);

struct PageState {
    /// Index 0 is the user-agent sheet; the rest are the document's, in cascade order. One vector
    /// rather than two because the matcher indexes them together and cascade order is exactly this
    /// order — the UA sheet is simply the one that loses to everything.
    sheets: Vec<Stylesheet>,
    /// Which element each sheet came from, parallel to `sheets`. `None` for the UA sheet and for
    /// sheets script constructed itself.
    owners: Vec<Option<NodeId>>,
    /// Which tree each sheet styles, parallel to `sheets`: derived from `owners` by
    /// [`sheet_scope`] at every document sync, [`SheetScope::Adopted`] for a constructed sheet, and
    /// [`SheetScope::Everywhere`] for any other sheet with no owner.
    scopes: Vec<SheetScope>,
    /// The constructed sheets each tree adopts, set by [`Page::set_adopted_sheets`].
    adoptions: Adoptions,
    /// The owning element's text as it was when the sheet was last parsed from it.
    ///
    /// Kept separately because a `Stylesheet`'s own text is a *value pool* that grows as rules are
    /// inserted, so it stops matching the source the moment script edits the sheet. Comparing
    /// against it would make the next sync mistake an edited sheet for a changed `<style>` and
    /// reparse the element's text over the top, silently undoing the edit.
    sources: Vec<String>,
    /// The owning element's `media` attribute as it was when the sheet's media list was last taken
    /// from it, parallel to `sheets`.
    ///
    /// CSSOM's media list is the SHEET's: the attribute initialises it and every later change of the
    /// attribute re-initialises it, but `sheet.media.mediaText = x` changes the sheet and leaves the
    /// attribute alone. Measured on stock Chrome 150.0.7871.46: after `style.removeAttribute("media")`
    /// and then `style.sheet.media.mediaText = "print"`, `getAttribute("media")` is `null` and the
    /// sheet no longer applies; a later `setAttribute("media", "all")` applies it again. Comparing the
    /// attribute against this snapshot is what tells those two writers apart.
    media_attrs: Vec<Option<String>>,
    /// Rebuilt whenever a sheet changes. `None` means "rebuild before the next query".
    matcher: Option<Matcher>,
    /// What `CSS.registerProperty` registered, in call order. They take precedence over any `@property`
    /// of the same name (CSS Properties and Values 1 §3.1) and a name registers once per document.
    script_registrations: Vec<(String, crate::style::registered::Registration)>,
    /// Bumped by every change to the stylesheets — a sheet added, a rule inserted or deleted, a
    /// `<style>` element's text reparsed. The DOM's own revision cannot see any of these: a page
    /// that calls `insertRule` and then measures has not touched the DOM at all.
    sheet_revision: u64,
    /// What each of the last [`SHEET_LOG`] bumps of `sheet_revision` changed: `Some(i)` the
    /// rules, the media or the enabled state of sheet `i` (one it added included), `None` anything else
    /// -- adoption, a sheet's tree, the quirks or viewer rows. A sheet change the log names is styled
    /// from the changed sheets alone ([`incremental::SheetChange`]).
    sheet_log: std::collections::VecDeque<(u64, Option<u32>)>,
    /// How many passes styled a sheet change from the changed sheets alone.
    sheet_passes: u64,
    /// The last [`Page::sheet_change`] answer, by root and the revisions it is between.
    sheet_change: Option<((NodeId, u64, u64), Option<std::sync::Arc<incremental::SheetChange>>)>,
    /// The DOM revision the `<style>` walk last ran at. `None` = never.
    ///
    /// `sync_document_sheets` used to walk every node on EVERY style query, which made one
    /// `getComputedStyle` O(nodes) and a loop over the document O(nodes^2). The field's own comment
    /// said a measurement would have to demand the fix; one did. `innerText` asks for the computed
    /// display of every element it descends through, so on en.wikipedia.org (2,417 elements) it
    /// cost 26 SECONDS, against 0.05 s for `textContent` over the same tree. That is not a
    /// micro-optimisation: `innerText` is what a scraper reads, and 26 s per page makes a real-site
    /// sweep impossible to run at all.
    ///
    /// The arena already carries the signal (`Dom::revision`, bumped on entry to every mutating
    /// method, deliberately over-bumping so a stale answer is impossible). Comparing it turns the
    /// walk into one integer compare when the document has not changed since the last sync, which
    /// is the overwhelmingly common case for a measure-heavy workload.
    synced_at_dom_revision: Option<u64>,
    /// Every `<style>` and `<link>` element in the arena (of any `rel`: an attribute write can make a link a
    /// stylesheet), in arena order, as the last WHOLE walk found them, and the DOM's
    /// [`Dom::structure_revision`] and [`Dom::instance`] then. While those are unchanged only attributes have been
    /// written since, which cannot add, remove, reorder or retext such an element: the walk visits these alone.
    sheet_nodes: Vec<NodeId>,
    synced_at_structure: Option<(u64, u64)>,
    /// How many times the `<style>` walk visited the whole arena: a count for tests.
    sheet_walks: u64,
    arena: StyleArena,
    /// What `animation-*` and `transition-*` declare, and what the running ones are putting on the
    /// cascade. See `style/declarative.rs`.
    declarative: Declarative,
    /// How many nodes the last rescanning `declarative_update` styled. See
    /// [`Page::declarative_styled`].
    ///
    /// It was the styles themselves, `(node, style)` per styled node, which a skipped pass walked
    /// to list the animations -- and earlier one slot per NODE, cloned and scanned per
    /// `getComputedStyle` (5.95 us per call at 2,010 elements, 32.35 us at 32,010). The list is now
    /// made where the styles are ([`PageState::last_animations`]) and the rescan keeps its own record
    /// ([`DeclarativeScan`]), so only the count is left.
    last_styled: usize,
    /// What the last animation rescan scanned, for the next one to start from.
    declarative_scan: DeclarativeScan,
    /// Bumped whenever the values the animations contribute actually change.
    ///
    /// It is in [`Page::layout_fingerprint`] and NOT in the cascade fingerprint, and the difference
    /// is the whole point: a moving animation must invalidate the caller's layout cache (an animated
    /// `width` has to move the box), but must NOT make `declarative_update` re-style the document
    /// every tick -- nothing the cascade reads has changed, and re-scanning would turn one animation
    /// into a full style pass per frame.
    animation_revision: u64,
    /// Moves whenever the animated values change at all -- a paint-only one too, which `animation_revision` leaves
    /// alone: what `transform` and `opacity` are read again on (D77, [`Page::effects_fingerprint`]).
    animated_generation: u64,
    /// The [`StyleKey`] of each element [`Page::computed_style`] has been asked for, and the
    /// [`Page::layout_fingerprint`] they were computed at.
    ///
    /// WHY THIS EXISTS, given that `computed_style` is deliberately always-fresh (design D9). It is
    /// not an invalidation map: it is discarded WHOLE the moment the fingerprint moves, so it can
    /// only ever answer for a document that has not changed since the answer was taken: the DOM
    /// revision, DOM length, sheet revision, viewport and animation revision. The caller's
    /// `LayoutCache` is keyed on [`Page::geometry_fingerprint`], the same inputs with the DOM counted
    /// only where a document can see it: it moves whenever this one does for any element in
    /// a document, and a change it ignores -- to a node no document holds -- changes no such element.
    ///
    /// It exists because `getComputedStyle` stopped materialising all 475 properties per call and
    /// now resolves one per read: reading five properties off one declaration (which is what
    /// `innerText` does per element) would otherwise cascade the element's ancestor chain five
    /// times, and reading all of them, 475 times.
    style_memo: (u64, std::collections::HashMap<NodeId, MemoEntry>),
    /// The last [`Page::layout_fingerprint`] and the exact inputs it was computed from. A
    /// `getComputedStyle` loop recomputes the fingerprint (a hash of ~10 values) on every property read;
    /// when the inputs have not moved -- the common case within a burst of reads -- the cached number is
    /// returned instead. Keyed on every input the hash reads, so it is never stale.
    fp_cache: std::cell::Cell<Option<(FpInputs, u64)>>,
    /// How many ancestor chains `computed_style` has actually walked. See [`Page::cascades_run`].
    cascades_run: u64,
    /// The animation specs the last RESCANNING `declarative_update` found, so a skipped update hands
    /// back the list instead of re-deriving it. See
    /// [`Page::declarative_spec_walks`].
    last_animations: std::sync::Arc<Vec<declarative::CssAnimationSpec>>,
    /// How many `(node, style)` pairs have had their `animation-*` longhands read back into specs.
    declarative_spec_walks: u64,
    /// Cascade results keyed on everything the cascade reads. See [`CascadeMemo`].
    cascade_memo: CascadeMemo,
    /// The rules that declare an animation or a transition, indexed, and the `sheet_revision` the
    /// index was built at. See `declarative_update`'s first pass.
    /// With it: whether any of those rules has a selector that names the `style` attribute.
    declarative_index: Option<(u64, RuleIndex, Reads)>,
    /// The same rules for an element the page style pass has matched ([`DeclaringRules`]), at a `sheet_revision`.
    declaring_rules: Option<(u64, DeclaringRules)>,
    /// See [`DeclarativeMatches`].
    declarative_matches: DeclarativeMatches,
    /// See [`Page::declarative_selector_tests`].
    declarative_selector_tests: u64,
    /// The matched rules of the last pass that styled from nothing. See [`MatchCache`].
    match_cache: MatchCache,
    /// The rules that can read the `style` attribute, and the `sheet_revision` they were found at.
    /// See [`StyleReading`].
    style_reading: Option<(u64, StyleReading)>,
    /// Some enabled sheet has a rule under `@container`, or a `cq*` length: the cascade
    /// needs container sizes. Derived with the matcher.
    has_container_rules: bool,
    /// What the enabled sheets' `@container` conditions read of a container, at a sheet revision
    /// ([`container_moved`]).
    container_reads: Option<(u64, ContainerReads)>,
    /// Some enabled sheet's text names a `cq*` unit.
    uses_cq_units: bool,
    /// Each query container's content-box size from the last layout that styled with container
    /// queries in play. See [`Page::layout`].
    container_sizes: std::collections::HashMap<NodeId, (f32, f32)>,
    /// Bumped whenever `container_sizes` changes, and part of the cascade fingerprint: an answer taken
    /// under the old sizes is stale.
    container_generation: u64,
    /// The [`Page::container_basis_of`] the sizes were taken at. A cascade read at any other basis
    /// lays the document out first.
    container_basis: Option<u64>,
    /// Style-and-layout passes [`Page::layout`] has run for container queries beyond the first --
    /// bounded by [`CONTAINER_PASSES`], and a count for tests.
    container_passes: u64,
    /// How many boxes a layout carried over from the last one instead of laying them out.
    boxes_carried: u64,
    /// How many box layouts the layouts computed (cache misses), summed over every `compute` -- a count for tests.
    box_layouts_computed: u64,
    /// Box trees built, and layouts that patched the last tree in place instead: counts for tests.
    trees_built: u64,
    trees_patched: u64,
    /// Pseudo-element styles by `(element, pseudo name)`, under the `style_memo` fingerprint.
    pseudo_memo: (u64, std::collections::HashMap<(NodeId, String), StyleKey>),
    /// Documents laid out, the page's and every frame's -- a count for tests.
    layouts_run: u64,
    /// The decoded natural size of every `<img>` whose image is available, and which are broken (A/B
    /// C8), by element, and a generation that moves whenever one arrives or goes. The caller owns the truth --
    /// the image loader is `korvus-webapi`'s -- and hands it over before each layout with
    /// [`Page::set_natural_images`]; the generation is part of the layout fingerprint, so a loaded
    /// image lays the page out again.
    natural_images: std::collections::HashMap<NodeId, ImageState>,
    natural_images_gen: u64,
    /// The document's web fonts as layout measures them, and their generation ([`Page::set_web_fonts`]); like the
    /// images', the generation is in the layout fingerprint ([`PageState::resources_gen`]), so an arrived font lays the
    /// page out again.
    web_fonts: Option<std::sync::Arc<font::WebFonts>>,
    web_fonts_gen: u64,
    /// The caller's number for what the table was last made from ([`Page::web_fonts_key`]).
    web_fonts_key: u64,
    /// The VIEWER document's own user-agent sheet and the rules it was made from: Chrome's
    /// JSON viewer styles its `<pre>` from UA rules no page can see. One row, created once, switched
    /// off (`disabled`) for every document that is not that viewer's.
    viewer_sheet: Option<(usize, String)>,
    /// Each laid-out document's styles between passes, by root: see [`incremental`]. A few
    /// roots at most -- the page's and its frames' -- and the oldest is dropped past [`DOC_STYLES`].
    doc_styles: std::collections::HashMap<NodeId, incremental::DocStyle>,
    /// The layout the last container-settling style read made (see [`Page::settle_containers`]).
    settled: Option<KeptLayout>,
    /// The page geometry fingerprint `settled` was laid out at, for a geometry read to take it
    /// ([`Page::take_settled_layout`]) instead of laying the same document out again.
    settled_at: Option<(u64, MediaContext)>,
    /// The layout-style rows worked out so far, by computed style, and the theme they hold under:
    /// see `layout::style::DerivedRow`.
    layout_origins: (spec::ThemeProfile, crate::layout::style::KeyRows),
    /// Style passes that styled only what changed, and elements matched by style passes: see
    /// [`Page::layouts_reusing_matches`], [`Page::elements_matched`].
    incremental_passes: u64,
    /// Animation rescans that started from what changed. See [`Page::animation_rescans_from_what_changed`].
    rescans_from_what_changed: u64,
    /// Every computed style from its ancestor chain, none from the page style pass: for the tests that
    /// measure what the chain walk costs.
    chains_only: bool,
    elements_matched: u64,
    /// What every rule reads, at a sheet revision: see [`incremental::Features`].
    features: Option<(u64, incremental::Features)>,
}

/// How many documents keep their styles between passes.
const DOC_STYLES: usize = 4;

/// How many sheet changes [`PageState::sheet_log`] remembers. A document whose styles are further
/// behind than that is styled from nothing.
const SHEET_LOG: usize = 64;

impl PageState {
    /// The generation of what layout reads besides the DOM and the sheets -- the images' and the web fonts' -- as
    /// one number: either moving lays the page out again.
    fn resources_gen(&self) -> u64 {
        self.natural_images_gen ^ self.web_fonts_gen.rotate_left(32)
    }

    /// The sheets changed: `Some(i)` for sheet `i`'s rules, media or enabled state, `None` for anything
    /// else (see `sheet_log`). The rule index goes, and every style made before is stale.
    fn sheets_changed(&mut self, sheet: Option<usize>) {
        self.matcher = None;
        self.sheet_revision += 1;
        if self.sheet_log.len() == SHEET_LOG {
            self.sheet_log.pop_front();
        }
        self.sheet_log.push_back((self.sheet_revision, sheet.map(|i| i as u32)));
    }
}

/// A layout the page keeps for itself between calls.
///
/// `LaidOut` is not `Send`: taffy keeps a `calc()` value as a pointer, and this engine's point into the
/// making thread's interned store (`style::math::intern`). This one is used only on the thread that
/// made it -- [`KeptLayout::take`] answers `None` anywhere else -- and dropping it elsewhere follows none
/// of those pointers, so the page state may still move between threads.
struct KeptLayout {
    thread: std::thread::ThreadId,
    laid: LaidOut,
}

// SAFETY: see the type's documentation: the pointers in `laid` are read only on `thread`.
unsafe impl Send for KeptLayout {}

impl KeptLayout {
    fn new(laid: LaidOut) -> KeptLayout {
        KeptLayout { thread: std::thread::current().id(), laid }
    }

    /// The layout, on the thread that made it.
    fn take(self) -> Option<LaidOut> {
        (self.thread == std::thread::current().id()).then_some(self.laid)
    }
}

/// May a style-and-layout pass reuse what an earlier one left? See [`Page::layout_pass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reuse {
    Allowed,
    Nothing,
}

/// What [`Page::layout_document_counted`] lays a document out from: nothing; the caller's last layout, lent; the
/// caller's last layout, handed over; or the tree an earlier container pass of the same layout made. An owned tree
/// gives its states to the tree carried from it ([`BoxTree::carry_from_moving`]) and is then replaced by that tree,
/// so a gutted tree is never reachable.
enum Prev<'a> {
    Nothing,
    Borrowed(&'a BoxTree),
    Caller(BoxTree, ScrollSnapshot),
    Pass(BoxTree),
}

impl Prev<'_> {
    fn tree(&self) -> Option<&BoxTree> {
        match self {
            Prev::Nothing => None,
            Prev::Borrowed(t) => Some(t),
            Prev::Caller(t, _) | Prev::Pass(t) => Some(t),
        }
    }
}

/// What a layout made: the lent layout stands ([`Page::relayout_document`]'s `None`), the handed-over one stands,
/// or a new one.
enum Laid {
    KeptBorrowed,
    Kept(LaidOut),
    New(LaidOut),
}

/// What [`Page::relayout_document_owned`] made of the layout it was handed.
pub enum Relaid {
    /// The document lays out exactly as it did: the layout handed in, untouched.
    Kept(LaidOut),
    /// A new layout -- carried, where it could be, from the one handed in, which is gone.
    New(LaidOut),
}

impl Relaid {
    /// The layout, whichever it is.
    pub fn into_inner(self) -> LaidOut {
        match self {
            Relaid::Kept(l) | Relaid::New(l) => l,
        }
    }

    pub fn is_new(&self) -> bool {
        matches!(self, Relaid::New(_))
    }
}

/// What one style-and-layout pass produced.
struct PassOut {
    tree: BoxTree,
    /// Each query container's content-box size.
    sizes: std::collections::HashMap<NodeId, (f32, f32)>,
    /// Every element's style, by node; `None` for a node the pass did not style.
    keys: Vec<Option<StyleKey>>,
    generated: std::collections::HashMap<NodeId, Generated>,
    /// The previous layout was kept: `tree` was built and not laid out.
    kept: bool,
}

/// How many extra style-and-layout passes one [`Page::layout`] runs to settle `@container` queries.
/// One pass answers queries against the sizes the previous layout produced; a second answers them
/// against the sizes that styling produced. A query whose answer moves its own container (a
/// container that shrinks when it matches `(min-width)`) oscillates, and this is what bounds it.
const CONTAINER_PASSES: usize = 3;

/// The internal sheets after the user-agent sheet at 0: Blink's quirks-mode sheet at
/// 1, enabled only for a quirks document, and at 2 the presentational hints HTML attributes map to.
/// Document sheets start at 3.
const QUIRKS_SHEET: usize = 1;

/// Does an `@container` condition hold for `node`? The query container is the nearest ancestor whose
/// `container-type` makes the axes the query asks about queryable and whose `container-name` has the
/// name the rule gives, if any (CSS Containment 3 §6.1). No such ancestor: the rule does not apply.
#[allow(clippy::too_many_arguments)]
fn container_holds(
    arena: &StyleArena,
    sizes: &std::collections::HashMap<NodeId, (f32, f32)>,
    style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
    dom: &Dom,
    node: NodeId,
    cond: &ContainerCondition,
    media: &MediaContext,
    root_font_size: f32,
) -> bool {
    let needs = cond.needs();
    let mut cur = dom.node(node).parent;
    while let Some(a) = cur {
        cur = dom.node(a).parent;
        if !matches!(dom.node(a).kind, NodeKind::Element { .. }) {
            continue;
        }
        let Some(key) = style_of(a) else { continue };
        if let Some(name) = &cond.name {
            if !arena.value(key, PropertyId::ContainerName).split_ascii_whitespace().any(|n| n == name) {
                continue;
            }
        }
        let axes = match arena.value(key, PropertyId::ContainerType).trim() {
            "size" => (true, true),
            "inline-size" => (true, false),
            _ => (false, false),
        };
        if (needs.0 && !axes.0) || (needs.1 && !axes.1) {
            continue;
        }
        let font_size = px_of(arena.value(key, PropertyId::FontSize)).unwrap_or(16.0);
        let custom = |n: &str| arena.custom_value(key, n).map(str::to_string);
        let container = QueryContainer {
            size: sizes.get(&a).copied(),
            axes,
            font_size,
            root_font_size,
            viewport: (media.viewport_width, media.viewport_height),
            custom: &custom,
        };
        return cond.evaluate(&container);
    }
    false
}

/// What `cqw`/`cqh` mean for `node`: the content box of the nearest ancestor whose
/// `container-type` makes that axis queryable, each axis on its own, the viewport where there is none.
/// `None` when no length in the document is a `cq*` unit.
fn cq_basis(
    arena: &StyleArena,
    sizes: &std::collections::HashMap<NodeId, (f32, f32)>,
    style_of: &dyn Fn(NodeId) -> Option<StyleKey>,
    dom: &Dom,
    node: NodeId,
    media: &MediaContext,
) -> (f32, f32) {
    let (mut w, mut h) = (None, None);
    let mut cur = dom.node(node).parent;
    while let Some(a) = cur {
        cur = dom.node(a).parent;
        if w.is_some() && h.is_some() {
            break;
        }
        let Some(key) = style_of(a) else { continue };
        let ty = arena.value(key, PropertyId::ContainerType).trim();
        let Some(size) = sizes.get(&a) else { continue };
        if w.is_none() && matches!(ty, "size" | "inline-size") {
            w = Some(size.0);
        }
        if h.is_none() && ty == "size" {
            h = Some(size.1);
        }
    }
    (w.unwrap_or(media.viewport_width), h.unwrap_or(media.viewport_height))
}

/// Whether an element's style moving from `then` to `now` can change what is below it through it as
/// a container: its type for a `cq*` length; with `@container` rules (`queries`), what those conditions
/// read of it ([`ContainerReads`]) -- its name, if one names its container; its font size, if one asks a
/// size and it is a size container (`em`); the custom properties a `style()` query asks. The other inputs of
/// [`container_holds`] and [`cq_basis`] -- sizes, `rem`, the viewport, the registrations -- are in the
/// style basis, whose change restyles everything.
///
/// Any custom property, its name, and its font size were taken for a container's however no condition read them:
/// with `@container` rules in the sheets, an element whose (inherited) custom properties moved had everything
/// below it matched and cascaded again. www.airbnb.com writes custom properties on its buttons each frame, and a
/// page that has loaded spent most of its main thread re-matching thousands of elements below them.
fn container_moved(arena: &StyleArena, then: StyleKey, now: StyleKey, queries: Option<&ContainerReads>) -> bool {
    if then == now {
        return false;
    }
    let differs = |p: PropertyId| arena.value(then, p) != arena.value(now, p);
    if differs(PropertyId::ContainerType) {
        return true;
    }
    let Some(reads) = queries else { return false };
    let sizing = |k: StyleKey| arena.value(k, PropertyId::ContainerType).trim() != "normal";
    (reads.named && differs(PropertyId::ContainerName))
        || (reads.size && (sizing(then) || sizing(now)) && differs(PropertyId::FontSize))
        || reads.custom.iter().any(|n| arena.custom_value(then, n) != arena.custom_value(now, n))
}

/// Which changes a geometry fingerprint is keyed on.
#[derive(Clone, Copy)]
enum Keyed {
    /// Every document of the arena.
    Arena,
    /// The page's document.
    Page,
    /// One other document: a frame's.
    Document(NodeId),
}

/// Which tree the sheet `owner` owns styles: the tree `owner` is in, when that is a
/// document or a shadow tree.
///
/// A sheet an element owns (`<style>`, `<link rel=stylesheet>`) styles the tree that element is in and
/// no other. The walk that creates sheets visits the WHOLE arena, which holds every nested browsing
/// context's document beside the page's, so a frame's `<style>` must not restyle the page nor the
/// page's the frame -- Chrome 150.0.7871.46, measured: a page with `body{margin:0}`
/// holding `<iframe srcdoc="<div id=in>">` reports `#in` at `[8, 8, 50, 50]` inside the frame. And a
/// shadow tree is a tree of its own: Chrome 150 (`tools/realweb/seed/ss/scope.html`) styles nothing in
/// the document from a shadow root's `<style>`, and nothing in a shadow tree from the document's.
///
/// A sheet whose owner is in NO DOCUMENT -- a `<style>` script created and never inserted, or one
/// inside a `<template>`'s content -- keeps `Everywhere` here and contributes nothing
/// ([`owner_contributes`]).
fn sheet_scope(dom: &Dom, owner: NodeId) -> SheetScope {
    let root = dom.root_of(owner);
    if dom.node_type(root) == 9 || dom.is_shadow_root(root) {
        SheetScope::Tree(root)
    } else {
        SheetScope::Everywhere
    }
}

/// The parent element of the node that owns sheet `sheet` -- the root of a prelude-less `@scope`.
fn owner_parent_of(owners: &[Option<NodeId>], dom: &Dom, sheet: u32) -> Option<NodeId> {
    let owner = (*owners.get(sheet as usize)?)?;
    let parent = dom.node(owner).parent?;
    matches!(dom.node(parent).kind, NodeKind::Element { .. }).then_some(parent)
}

/// What one element's pseudo-elements add to its box, handed to the box tree.
pub use crate::layout::tree::{Generated, ImageState, PseudoBox};

/// [`Dom::unrevised_writes`] when a cache entry was filled: the `style` and the `data-*` writes, which
/// move no [`Dom::match_revision`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Unrevised {
    style: u64,
    data: u64,
}

impl Unrevised {
    fn of(dom: &Dom) -> Unrevised {
        let (style, data) = dom.unrevised_writes();
        Unrevised { style, data }
    }
}

/// Which of the two kinds of unrevised write a set of rules can read -- or, from [`Reads::since`], which
/// of them were written since a cache entry was filled and are read.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Reads {
    style: bool,
    data: bool,
}

impl Reads {
    fn of(selector: &korvus_dom::Selector) -> Reads {
        Reads { style: selector.mentions_style_attribute(), data: selector.mentions_data_attribute() }
    }

    fn or(self, other: Reads) -> Reads {
        Reads { style: self.style || other.style, data: self.data || other.data }
    }

    /// The kinds these rules read that were written between `then` and `now`: a cached answer is
    /// exact while this is empty.
    fn since(self, then: Unrevised, now: Unrevised) -> Reads {
        Reads { style: self.style && then.style != now.style, data: self.data && then.data != now.data }
    }

    fn any(self) -> bool {
        self.style || self.data
    }
}

/// The rules with a selector that names the `style` attribute (`[style*=…]`, at any depth) or a `data-*`
/// one, which are the only rules a write to such an attribute can make match or stop
/// matching. A cache of matched rules kept across such writes drops, from each cached list,
/// the rules that read a kind written since it was filled, and matches them again: a `style`
/// write re-matches only the `style` rules, a `data-*` write only the `data-*` ones, and no write
/// nothing. Matching every one of them again for any write cost www.mapbox.com -- `[data-*]` rules and a
/// `style` rewrite per frame -- 2-3x its load after C7.P6.
struct StyleReading {
    /// `(sheet, rule)` of every rule that names `style`, and of every one that names a `data-*` attribute.
    style_rules: std::collections::HashSet<(u32, u32)>,
    data_rules: std::collections::HashSet<(u32, u32)>,
    /// All the selectors of each set, in the trees their sheets style, as [`Matcher::build_scoped`]
    /// indexes every selector.
    style_matcher: Matcher,
    data_matcher: Matcher,
    /// Whether each set has a rule. `reads.style` is all the geometry's discount for inert `style`
    /// rewrites asks ([`Page::geometry_fingerprint`]): read as "any such rule", one `[data-*]` rule
    /// turned that discount off, and www.mapbox.com's `transform` writes then laid it out 2509 times to
    /// its load, not 266 (C7.P6).
    reads: Reads,
}

impl StyleReading {
    fn of(sheets: &[Stylesheet], scopes: &[SheetScope], adoptions: &Adoptions) -> StyleReading {
        let (mut style_rules, mut data_rules) = (std::collections::HashSet::new(), std::collections::HashSet::new());
        for (s, sheet) in sheets.iter().enumerate() {
            for (r, rule) in sheet.cascade_rules().iter().enumerate() {
                let reads = rule.selectors.iter().fold(Reads::default(), |acc, sel| acc.or(Reads::of(sel)));
                if reads.style {
                    style_rules.insert((s as u32, r as u32));
                }
                if reads.data {
                    data_rules.insert((s as u32, r as u32));
                }
            }
        }
        // Every selector of such a rule: the cached lists drop the whole rule, so it is matched again whole.
        let style_matcher = Matcher::build_scoped(sheets, scopes, adoptions, |s, r| style_rules.contains(&(s, r)));
        let data_matcher = Matcher::build_scoped(sheets, scopes, adoptions, |s, r| data_rules.contains(&(s, r)));
        let reads = Reads { style: !style_rules.is_empty(), data: !data_rules.is_empty() };
        StyleReading { style_rules, data_rules, style_matcher, data_matcher, reads }
    }

    /// The kinds a list cached at `then` has to match again now.
    fn stale(&self, then: Unrevised, now: Unrevised) -> Reads {
        self.reads.since(then, now)
    }

    /// Whether a cached list drops `m` when `stale` is to be matched again.
    fn drops(&self, stale: Reads, m: &MatchedRule) -> bool {
        (stale.style && self.style_rules.contains(&(m.sheet, m.rule))) || (stale.data && self.data_rules.contains(&(m.sheet, m.rule)))
    }

    /// Append the rules of the `stale` kinds that match `node` to `out`, with `Matcher::match_element`'s
    /// exact test (`rule_applies`, then the selector) and its rule for a rule matched through two
    /// selectors -- or through both sets -- (once, at the higher specificity).
    #[allow(clippy::too_many_arguments)]
    fn rematch(&self, stale: Reads, sheets: &[Stylesheet], dom: &Dom, node: NodeId, media: &MediaContext, env: &MatchEnv<'_>, out: &mut Vec<MatchedRule>) {
        if stale.style {
            self.style_matcher.collect(sheets, dom, node, media, None, env, out);
        }
        if stale.data {
            self.data_matcher.collect(sheets, dom, node, media, None, env, out);
        }
    }
}


/// Every element's matched rules from the last [`Page::layout`] that styled from nothing, kept for the next
/// one when nothing selector matching reads has changed in between. Now only the
/// documents that pass styles from nothing use it -- the ones with `@container` rules or `cq*` units.
///
/// # Why
///
/// A page that writes `el.style.height` and then reads a box back does it in a loop, and each read
/// lays the document out again. The cascade is memoised ([`CascadeMemo`]), which left MATCHING every
/// element against every candidate rule as the whole cost: www.who.int, sampled while the parity
/// driver waited 30 s for `Runtime.evaluate`, spent 16 of 20 samples in `match_element` under
/// `Page::layout`, re-matching 6,213 nodes for each of ~280 layouts -- and counting its mutations
/// showed exactly one `style` attribute write between consecutive layouts.
///
/// # Why this cannot go stale
///
/// It is reused only when all of these are what they were when it was filled: the DOM's
/// [`Dom::match_revision`] (every mutation except a `style` attribute write) and length, the
/// stylesheets (`sheet_revision`), and the whole [`MediaContext`] `rule_applies` evaluates. The one
/// write it looks through can change a match only through a rule with a selector that names
/// `style`, so those rules ([`StyleReading`]) are never taken from the cache: each reusing layout
/// matches them again. (www.who.int has one, which is how the first version of this -- off whenever
/// such a rule existed -- measured as no change at all there.)
#[derive(Default)]
struct MatchCache {
    generation: Option<(u64, usize, u64, MediaContext, u64, NodeId)>,
    /// The unrevised writes when it was filled: see [`StyleReading`].
    filled: Unrevised,
    /// Per node: `(start, len)` into `rules`.
    spans: Vec<(u32, u32)>,
    rules: Vec<MatchedRule>,
    /// Layouts that reused it. See [`Page::layouts_reusing_matches`].
    reused: u64,
}

/// What the incremental style pass needs to style an element's pseudo-elements: the page's matching
/// state and the element's place.
struct PseudoStyling<'a> {
    matcher: &'a Matcher,
    sheets: &'a [Stylesheet],
    container_sizes: &'a std::collections::HashMap<NodeId, (f32, f32)>,
    owners: &'a [Option<NodeId>],
    dom: &'a Dom,
    media: &'a MediaContext,
}

impl PseudoStyling<'_> {
    /// `node`'s `::-webkit-scrollbar`, `::before` and `::after` styles, as the generated-content walk
    /// of [`Page::style_from_nothing`] makes them: matched against exactly its ancestors (`ancestors`,
    /// or a filter built here), cascaded from its style `key` and the formatting it gives `children`,
    /// under `ctx`. `None` when it generates no box or has none of them.
    #[allow(clippy::too_many_arguments)]
    fn keys(
        &self,
        arena: &mut StyleArena,
        cascade_memo: &mut CascadeMemo,
        keys: &[Option<StyleKey>],
        node: NodeId,
        key: StyleKey,
        children: ParentFormatting,
        ctx: &ComputeContext,
        ancestors: Option<&AncestorBloom>,
        matched: &mut Vec<MatchedRule>,
    ) -> Option<Box<incremental::PseudoKeys>> {
        if !generates_box_for(arena, key) {
            return None;
        }
        let dom = self.dom;
        let mut sheets = 0u64;
        let mut pseudo = |name: &str| -> Option<StyleKey> {
            if !self.matcher.has_pseudo(name) {
                return None;
            }
            {
                let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                let arena_ref = &*arena;
                let containers = |at: NodeId, cond: &ContainerCondition| {
                    container_holds(arena_ref, self.container_sizes, &style_of, dom, at, cond, self.media, ctx.root_font_size)
                };
                let owner_parent = |sheet: u32| owner_parent_of(self.owners, dom, sheet);
                let env = MatchEnv { containers: Some(&containers), owner_parent: Some(&owner_parent), ancestors, tree: None };
                self.matcher.match_pseudo(self.sheets, dom, node, self.media, name, &env, matched);
            }
            if matched.is_empty() {
                return None;
            }
            for rule in matched.iter() {
                sheets |= incremental::PseudoKeys::sheet_bit(rule.sheet);
            }
            let facts = ElementFacts { textarea: false, quirks: dom.document_mode_of(node) == korvus_dom::DocumentMode::Quirks, contents_is_none: false, svg_block: false };
            let k = cascade_memoised(cascade_memo, self.sheets, matched, "", "", Some(key), children, ctx, arena, &[], facts);
            Some(pseudo_content(arena, k, name, dom, node))
        };
        let (scrollbar, before, after) = (pseudo("-webkit-scrollbar"), pseudo("before"), pseudo("after"));
        let keys = incremental::PseudoKeys { scrollbar, before, after, sheets };
        (keys.scrollbar.is_some() || keys.before.is_some() || keys.after.is_some()).then(|| Box::new(keys))
    }
}

/// Is a style `display: none`?
fn display_none(arena: &StyleArena, key: StyleKey) -> bool {
    arena.value(key, PropertyId::Display) == "none"
}

/// Which elements are not rendered (`declarative::not_rendered`): `display: none` on the element or on an
/// ancestor in the FLAT tree, or outside the flat tree -- a light child of a shadow host that no slot takes, and
/// everything below it -- as `checkVisibility()` reads it. Chrome 150 (`fixtures/shadow-animations`): an unslotted
/// child declaring `animation: kf 80ms` has no animation in `getAnimations()` and sends no event. The styles are
/// `keys` (the page style pass's, or a rescan's own); the flat tree is walked as the style pass walks it
/// (`flat::FlatTree`), and each answer is kept, so a rescan climbs each ancestor once.
struct Hidden<'a> {
    arena: &'a StyleArena,
    dom: &'a Dom,
    keys: &'a [Option<StyleKey>],
    flat: crate::flat::FlatTree,
    memo: korvus_dom::FxHashMap<NodeId, bool>,
}

impl<'a> Hidden<'a> {
    fn new(arena: &'a StyleArena, dom: &'a Dom, keys: &'a [Option<StyleKey>]) -> Self {
        Hidden { arena, dom, keys, flat: crate::flat::FlatTree::default(), memo: Default::default() }
    }

    fn of(&mut self, node: NodeId) -> bool {
        let mut path = Vec::new();
        let mut cur = Some(node);
        let mut answer = false;
        while let Some(n) = cur {
            if let Some(&known) = self.memo.get(&n) {
                answer = known;
                break;
            }
            path.push(n);
            if self.keys.get(n as usize).copied().flatten().is_some_and(|k| display_none(self.arena, k)) {
                answer = true;
                break;
            }
            let up = self.flat.parent(self.dom, n);
            // A light child no slot takes renders nowhere, nor does anything below it.
            if up.is_none() && self.dom.node(n).parent.is_some_and(|p| self.dom.shadow_root_of(p).is_some()) {
                answer = true;
                break;
            }
            cur = up;
        }
        for n in path {
            self.memo.insert(n, answer);
        }
        answer
    }
}

/// The last element a pre-order walk of the flat tree from `node` visits below it: the element whose
/// container basis [`Page::style_from_nothing`] leaves in its context for the pseudo-elements.
fn last_element_below(dom: &Dom, flat: &mut crate::flat::FlatTree, node: NodeId) -> Option<NodeId> {
    let mut children = flat.children(dom, node);
    while let Some(c) = children.pop() {
        if let Some(e) = last_element_below(dom, flat, c) {
            return Some(e);
        }
        if matches!(dom.node(c).kind, NodeKind::Element { .. }) {
            return Some(c);
        }
    }
    None
}

/// What [`Page::declarative_update`]'s rescans matched, kept for the next rescan while nothing
/// selector matching reads has changed -- the same rule [`MatchCache`] keeps for layout.
///
/// A rescan runs whenever the cascade fingerprint moves, and a `style` attribute write moves it:
/// www.mapbox.com's replay made 17,748 `getComputedStyle` calls, 1,126 of them rescanned, and 1,073 of
/// those had nothing but `style` writes behind them -- each re-matching every animation candidate and
/// its ancestors from scratch. A `style` write changes a match only through a rule that names `style`,
/// so those rules are matched again from [`StyleReading`] and the rest are taken from here.
#[derive(Default)]
struct DeclarativeMatches {
    generation: Option<(u64, usize, u64, MediaContext, u64)>,
    /// Pass one's answer per element: does a rule that declares an animation or a transition match
    /// it? With the unrevised writes it was answered at: kept while no declaring rule reads a kind
    /// written since.
    declares: korvus_dom::FxHashMap<NodeId, (Unrevised, bool)>,
    /// Pass two's matched rules per element, with the unrevised writes they are exact for.
    matched: korvus_dom::FxHashMap<NodeId, (Unrevised, Vec<MatchedRule>)>,
    /// Pass two's last cascade per element. See [`Cascaded`].
    cascaded: korvus_dom::FxHashMap<NodeId, Cascaded>,
}

/// One element's last cascade in a rescan's pass two: what [`cascade_memoised`] was given that can
/// move inside one [`DeclarativeMatches`] generation, and its answer. The sheets and the media are the
/// generation's, and the matched list is named by the mark [`declarative_match`] returns: a list
/// changes only when it is matched again, and that moves its mark. So the same inputs here are the
/// same cascade, and the answer is taken without asking the memo. Asking it hashes and compares the
/// whole matched list: 3.5 % of www.mapbox.com's samples were that comparison, on rescans where
/// nearly every element came back unchanged.
struct Cascaded {
    matched: Unrevised,
    parent: Option<StyleKey>,
    mine: ParentFormatting,
    facts: ElementFacts,
    root_font_size: u32,
    container: Option<(u32, u32)>,
    /// The element's `style` attribute, named by [`Dom::style_writes_of`]: comparing the text was the
    /// largest memcmp left on www.mapbox.com, whose inline styles are long.
    style_writes: u32,
    key: StyleKey,
}

/// Pass two's matching of one element through [`DeclarativeMatches`]: see there.
#[allow(clippy::too_many_arguments)]
fn declarative_match(
    cache: &mut DeclarativeMatches,
    matcher: &Matcher,
    style_reading: &StyleReading,
    sheets: &[Stylesheet],
    dom: &Dom,
    node: NodeId,
    media: &MediaContext,
    now: Unrevised,
    env: &MatchEnv<'_>,
    out: &mut Vec<MatchedRule>,
) -> Unrevised {
    if let Some((then, cached)) = cache.matched.get_mut(&node) {
        out.clear();
        let stale = style_reading.stale(*then, now);
        if !stale.any() {
            out.extend_from_slice(cached);
            return *then;
        }
        out.extend(cached.iter().filter(|m| !style_reading.drops(stale, m)));
        style_reading.rematch(stale, sheets, dom, node, media, env, out);
        // Exact as of `now`, so the next rescan reuses it whole until another write it reads.
        *then = now;
        cached.clear();
        cached.extend_from_slice(out);
        return now;
    }
    matcher.match_element_in(sheets, dom, node, media, env, out);
    cache.matched.insert(node, (now, out.clone()));
    now
}

/// Pass one's question of one element (see [`Page::rescan_everything`]): could it declare an animation
/// or a transition? Its inline `style` names one, or it is styled from a shadow tree, or a rule that
/// declares one matches it.
struct Declaring<'a> {
    index: &'a RuleIndex,
    /// What the declaring rules' selectors read.
    reads: Reads,
    sheets: &'a [Stylesheet],
    scopes: &'a [SheetScope],
    adoptions: &'a Adoptions,
    media: &'a MediaContext,
    now: Unrevised,
}

impl Declaring<'_> {
    /// The answer for `node`, kept in `dm` while the declaring rules read nothing written since.
    /// `tests` counts the candidate selectors considered; `classes` and `rules` are scratch.
    fn declares<'d>(
        &self,
        dm: &mut DeclarativeMatches,
        dom: &'d Dom,
        node: NodeId,
        tests: &mut u64,
        classes: &mut Vec<&'d str>,
        rules: &mut Vec<RuleRef>,
    ) -> bool {
        if !matches!(dom.node(node).kind, NodeKind::Element { .. }) {
            return false;
        }
        let attr = dom.style_for_cascade(node).unwrap_or("");
        if attr.contains("animation") || attr.contains("transition") {
            return true;
        }
        // A shadow host and a slotted element are also styled from a shadow tree (`:host`,
        // `::slotted`), whose rules this index does not match from here: they are candidates
        // whatever it says, and pass two styles them exactly.
        if dom.shadow_root_of(node).is_some() || dom.assigned_slot(node).is_some() {
            return true;
        }
        if let Some(&(then, d)) = dm.declares.get(&node) {
            if !self.reads.since(then, self.now).any() {
                return d;
            }
        }
        let d = self.any_matches(self.index, dom, node, tests, classes, rules);
        dm.declares.insert(node, (self.now, d));
        d
    }

    /// The answer for `node`, which the page style pass has matched to `matched`: inline and shadow as
    /// [`Declaring::declares`]; one of `matched` declares; or a rule that matching can leave out matches
    /// ([`DeclaringRules`]). The same answer, without matching every declaring rule again: an element restyled
    /// below a moved value was matched twice -- by the style pass, and here -- and this was most of
    /// www.airbnb.com's main thread once it had loaded (a rescan per frame).
    #[allow(clippy::too_many_arguments)]
    fn declares_matched<'d>(
        &self,
        dom: &'d Dom,
        node: NodeId,
        matched: &[MatchedRule],
        declaring: &DeclaringRules,
        tests: &mut u64,
        classes: &mut Vec<&'d str>,
        rules: &mut Vec<RuleRef>,
    ) -> bool {
        if !matches!(dom.node(node).kind, NodeKind::Element { .. }) {
            return false;
        }
        let attr = dom.style_for_cascade(node).unwrap_or("");
        if attr.contains("animation") || attr.contains("transition") {
            return true;
        }
        if dom.shadow_root_of(node).is_some() || dom.assigned_slot(node).is_some() {
            return true;
        }
        matched.iter().any(|m| declaring.declares(m)) || self.any_matches(&declaring.residual, dom, node, tests, classes, rules)
    }

    /// Does a rule of `index` match `node`, in a tree its sheet styles and under the media?
    fn any_matches<'d>(
        &self,
        index: &RuleIndex,
        dom: &'d Dom,
        node: NodeId,
        tests: &mut u64,
        classes: &mut Vec<&'d str>,
        rules: &mut Vec<RuleRef>,
    ) -> bool {
        let NodeKind::Element { name, attrs, .. } = &dom.node(node).kind else { return false };
        classes.clear();
        classes.extend(dom.get_attribute(node, "class").unwrap_or("").split_ascii_whitespace());
        let keys = ElementKeys {
            local_name: name.as_str(),
            id: dom.get_attribute(node, "id").filter(|v| !v.is_empty()),
            classes,
            attrs,
            quirks: dom.document_mode_of(node) == korvus_dom::DocumentMode::Quirks,
        };
        rules.clear();
        index.for_each_candidate(keys, None, |r, _| rules.push(r));
        !rules.is_empty() && {
            let bloom = AncestorBloom::for_node(dom, node);
            *tests += rules.len() as u64;
            let tree = dom.root_of(node);
            let adopted = self.adoptions.get(&tree);
            rules.iter().any(|r| {
                // Only a rule of a sheet that styles this element's tree.
                let styles_tree = match self.scopes[r.sheet as usize] {
                    SheetScope::Everywhere => true,
                    SheetScope::Tree(root) => root == tree,
                    SheetScope::Adopted => adopted.is_some_and(|l| l.contains(&r.sheet)),
                };
                if !styles_tree {
                    return false;
                }
                let sheet = &self.sheets[r.sheet as usize];
                let rule = &sheet.cascade_rules()[r.rule as usize];
                sheet.rule_applies(rule, self.media) && rule.selectors[r.selector as usize].matches_with(dom, node, Some(&bloom))
            })
        }
    }
}

/// What the last animation rescan scanned, per node, for the next one to start from. See
/// [`Page::rescan_what_changed`].
#[derive(Default)]
struct DeclarativeScan {
    /// The rest is the last rescan's: false before the first one, after one that started over without
    /// keeping a record, and after a navigation.
    valid: bool,
    /// The [`Dom::instance`] it is about.
    instance: u64,
    /// An element that could declare an animation or a transition (see [`Declaring`]).
    candidate: Vec<bool>,
    /// Scanned: the document element, each candidate, and their ancestors.
    scanned: Vec<bool>,
    /// Scanned with a style cascaded through its ancestors rather than the page style pass's: an
    /// animated element and everything under it.
    chained: Vec<bool>,
    /// The scanned elements' animation specs, by node.
    specs: std::collections::BTreeMap<NodeId, Vec<declarative::CssAnimationSpec>>,
    /// A scanned element whose own `display` was `none` at its last scan. When that moves it hides or shows
    /// everything below it, whose styles do not move, so all of it is scanned again.
    own_none: Vec<bool>,
    /// Each chained element's last before-animation cascade, and what it was cascaded from. See
    /// [`Page::chain_bases`].
    chain: korvus_dom::FxHashMap<NodeId, (ChainInputs, StyleKey)>,
    /// How many are scanned, the chained ones in node order, and the animated elements (sorted) and
    /// the journal position they were worked out at: while no candidate comes or goes, no scanned
    /// element is unlinked and the animated elements are the same, the sets are the same.
    count: usize,
    chained_list: Vec<NodeId>,
    animated: Vec<NodeId>,
    seq: u64,
}

/// What a chained element's before-animation style is cascaded from, beyond the document-wide inputs a
/// pass that starts over moves (the sheets, the media, `rem`): its matched rules, named by the page
/// style pass's epoch and span; its parent's before-animation style and formatting; its facts; its
/// `style` attribute, named by its write count; and the size its `cq*` lengths resolve against.
#[derive(Clone, Copy, PartialEq)]
struct ChainInputs {
    matched: (u64, (u32, u32)),
    parent: Option<StyleKey>,
    mine: ParentFormatting,
    facts: ElementFacts,
    style_writes: u32,
    /// The `cq*` basis it was cascaded under.
    container: Option<(u32, u32)>,
}

impl DeclarativeScan {
    /// [`DeclarativeScan::scanned_from_candidates`] with what each element's own style differs by:
    /// the animated elements. Records what the sets were worked out at.
    fn rework(&mut self, dom: &Dom, root: NodeId, root_element: Option<NodeId>, animated: Vec<NodeId>) {
        let mut own = vec![false; dom.len()];
        for &x in &animated {
            if let Some(b) = own.get_mut(x as usize) {
                *b = true;
            }
        }
        self.count = self.scanned_from_candidates(dom, root, root_element, |x| own[x as usize]);
        self.chained_list = (0..self.chained.len()).filter(|&x| self.chained[x]).map(|x| x as NodeId).collect();
        self.animated = animated;
        self.seq = dom.journal().seq();
    }

    /// Whether the sets [`DeclarativeScan::rework`] made can have moved since: a candidate came or
    /// went (`candidacy_moved`), a scanned element was unlinked (it, or a subtree holding it, moved or
    /// left), or the animated elements are others.
    fn moved(&self, dom: &Dom, candidacy_moved: bool, animated: &[NodeId]) -> bool {
        use korvus_dom::journal::Change;
        let scanned = |x: NodeId| self.scanned.get(x as usize).copied().unwrap_or(false);
        candidacy_moved
            || animated != self.animated.as_slice()
            || dom.journal().since(self.seq).is_none_or(|entries| {
                entries.iter().any(|c| match c {
                    Change::Removed { node, .. } => scanned(*node),
                    Change::ChildrenReplaced { old, .. } => dom.journal().nodes(*old).iter().any(|&o| scanned(o)),
                    _ => false,
                })
            })
    }

    /// Whether the document's SHADOW STRUCTURE can have moved since the sets were worked out: a
    /// shadow root attached (an `Other` entry), a child linked into or out of a host, a shadow root or a slot (slot
    /// assignment), a `slot` attribute, a slot's `name`. A shadow host and a slotted element are candidates
    /// whatever their style says (`Declaring::declares`), and the flat tree the chains follow is the assignment's,
    /// so none of that reaches the rescan through the style pass's restyled elements: the rescan starts over.
    fn shadow_moved(&self, dom: &Dom) -> bool {
        use korvus_dom::journal::Change;
        let reassigns = |parent: NodeId| dom.shadow_root_of(parent).is_some() || dom.is_shadow_root(parent) || dom.is_html_slot_element(parent);
        dom.journal().since(self.seq).is_none_or(|entries| {
            entries.iter().any(|c| match c {
                Change::Removed { parent, .. } | Change::Inserted { parent, .. } | Change::ChildrenReplaced { parent, .. } => reassigns(*parent),
                Change::Attribute { node, name, .. } => {
                    let name = dom.journal().name(*name);
                    name == "slot" || (name == "name" && dom.is_html_slot_element(*node))
                }
                Change::Other { .. } => true,
                _ => false,
            })
        })
    }

    /// Work out `scanned` and `chained` from `candidate`, dropping a candidate that is no longer in
    /// the document (`root`). `own_chain` says whether an element's own style differs from the page
    /// style pass's. Returns how many are scanned.
    fn scanned_from_candidates(
        &mut self,
        dom: &Dom,
        root: NodeId,
        root_element: Option<NodeId>,
        own_chain: impl Fn(NodeId) -> bool,
    ) -> usize {
        const UNKNOWN: u8 = 0;
        const OUTSIDE: u8 = 1;
        const IN: u8 = 2;
        const IN_CHAINED: u8 = 3;
        let n = dom.len();
        self.candidate.resize(n, false);
        let mut state = vec![UNKNOWN; n];
        let mut path: Vec<NodeId> = Vec::new();
        let mut flat = crate::flat::FlatTree::default();
        let mut walk = |state: &mut Vec<u8>, from: NodeId| -> bool {
            path.clear();
            let mut cur = Some(from);
            // What is known above the path: (in the document, chained there).
            let mut above = (false, false);
            while let Some(x) = cur {
                match state[x as usize] {
                    UNKNOWN => {}
                    OUTSIDE => break,
                    s => {
                        above = (true, s == IN_CHAINED);
                        break;
                    }
                }
                path.push(x);
                if x == root {
                    above = (true, false);
                    break;
                }
                // up the FLAT tree, as the page style pass and the whole rescan's chains go: a shadow
                // tree's top element to its host, a slotted element to its slot (so the slot is scanned too); a light
                // child no slot takes reaches no root, and is not rendered (`Hidden`)
                cur = flat.parent(dom, x);
            }
            let (inside, mut chained) = above;
            for &x in path.iter().rev() {
                if !inside {
                    state[x as usize] = OUTSIDE;
                    continue;
                }
                chained = chained || (matches!(dom.node(x).kind, NodeKind::Element { .. }) && own_chain(x));
                state[x as usize] = if chained { IN_CHAINED } else { IN };
            }
            inside
        };
        if let Some(e) = root_element {
            walk(&mut state, e);
        }
        for c in 0..n {
            if self.candidate[c] && !walk(&mut state, c as NodeId) {
                self.candidate[c] = false;
            }
        }
        self.scanned.clear();
        self.chained.clear();
        let mut count = 0;
        for (x, s) in state.iter().enumerate() {
            let scanned = *s >= IN && matches!(dom.node(x as NodeId).kind, NodeKind::Element { .. });
            count += scanned as usize;
            self.scanned.push(scanned);
            self.chained.push(scanned && *s == IN_CHAINED);
        }
        count
    }
}

/// A whole-document style pass's answers, keyed on EVERYTHING [`compute_animated`] reads, so an
/// element whose inputs did not change is not cascaded again.
///
/// # Why
///
/// `Page::layout` styles every element of the document, and it runs again after every DOM
/// mutation that a geometry or computed-style read follows. A page that alternates a write and a
/// read over a list — the most common shape of layout code there is — therefore cascaded the
/// whole document once per element. Measured by sampling the engine thread (`gdb`, a stack every
/// 2 s) while the parity driver's `Page.navigate` waited 30 s unanswered on replayed recordings:
///
/// ```text
///   page            samples   in compute_animated under Page::layout   caller
///   asana.com            30        28 (93 %)                            getBoundingClientRect/offset*
///   www.jetbrains.com    22        17 (77 %)                            getBoundingClientRect
///   discord.com          15        14 (93 %)                            geometry + getComputedStyle
/// ```
///
/// Chrome recalculates only the elements a mutation dirtied. This does not track dirtiness; it
/// makes a CLEAN element cheap instead: the cascade is a pure function of its inputs, so an element
/// whose matched rules, inline `style`, parent style, parent formatting context and `rem` base are
/// what they were last time interns to the same [`StyleKey`] it did then, and the lookup replaces
/// the cascade. It also collapses siblings that share all of those within ONE pass — a list of a
/// thousand identical items is cascaded once.
///
/// # Why this cannot go stale
///
/// * The KEY is the whole input. Matching is still run per element every pass, so a selector that
///   stops matching changes `matched` and misses. The parent's [`StyleKey`] is interned, so it is
///   equal exactly when the parent's computed style is.
/// * What the key leaves out is document-wide, and the map is dropped when it moves: the
///   stylesheets (`sheet_revision`, bumped by every sheet edit) and the viewport. The arena is
///   append-only, so a key the map returns still names the style it named when it was stored.
/// * An element with animation overrides is never memoised: the overrides are an input, and they
///   change every frame.
#[derive(Default)]
struct CascadeMemo {
    /// `(sheet_revision, viewport width bits, viewport height bits)` the map was filled under.
    generation: Option<(u64, u32, u32)>,
    map: korvus_dom::FxHashMap<CascadeInput, StyleKey>,
    /// Cascades actually run by the whole-document passes (a memo miss). See
    /// [`Page::document_cascades_run`].
    runs: u64,
}

#[derive(PartialEq, Eq, Hash)]
struct CascadeInput {
    matched: Box<[(u32, u32, u32, u32)]>,
    inline: Box<str>,
    parent: Option<StyleKey>,
    flex_or_grid_item: bool,
    root_font_size: u32,
    min_logical_font_size: u32,
    /// The element facts Blink's style adjuster reads. See [`ElementFacts`].
    facts: ElementFacts,
    /// The `cq*` basis, as bits.
    container: Option<(u32, u32)>,
}

/// What the cascade's answer depends on about the ELEMENT beyond its matched rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ElementFacts {
    /// A `<textarea>`: Chrome 150 computes `overflow: auto` on a bare one, where the cascade says
    /// `visible` (Blink's style adjuster turns a textarea's `visible` into `auto`).
    pub textarea: bool,
    /// The element is in a quirks-mode document: unitless lengths and hashless colours are accepted
    /// on the properties the quirks list names.
    pub quirks: bool,
    /// `display: contents` computes to `none` on this element: a replaced element, a form
    /// control, `<br>`/`<wbr>`, an outermost `<svg>` -- elements with no children of their own to lay
    /// out in its place. See [`CONTENTS_IS_NONE`].
    pub contents_is_none: bool,
    /// An SVG `<text>` or `<foreignObject>`: an inline-level `display` computes `block` (Blink's style adjuster:
    /// "SVG text layout code expects us to be a block-level style element"). Chrome 150: a `<text>` reads
    /// `display: block`.
    pub svg_block: bool,
}

/// The elements whose `display: contents` computes to `none` (CSS Display 3 Appendix B, "unusual
/// elements"). Measured on Chrome 150 (2026-09-30, 28 elements each holding a 9px inline-block):
/// exactly these answer `getComputedStyle(el).display` `none` and lay nothing out, where `<button>`,
/// `<fieldset>`, `<legend>`, `<details>`, `<summary>`, the table parts, `<slot>` and even `<template>` keep
/// `contents` and lay their children out in their place. The outermost `<svg>` is on the list too.
const CONTENTS_IS_NONE: &[&str] = &[
    "img", "video", "audio", "canvas", "iframe", "embed", "object", "input", "select", "textarea", "meter",
    "progress", "br", "wbr",
];

impl ElementFacts {
    fn of(dom: &Dom, node: NodeId) -> ElementFacts {
        let html_tag = |tags: &[&str]| dom.is_html_ns(node) && dom.tag_name(node).is_some_and(|t| tags.iter().any(|x| t.eq_ignore_ascii_case(x)));
        const SVG: &str = "http://www.w3.org/2000/svg";
        let outer_svg = dom.namespace_uri(node) == Some(SVG)
            && dom.tag_name(node).is_some_and(|t| t == "svg")
            && dom.parent(node).is_none_or(|p| dom.namespace_uri(p) != Some(SVG));
        ElementFacts {
            textarea: html_tag(&["textarea"]),
            quirks: dom.document_mode_of(node) == korvus_dom::DocumentMode::Quirks,
            contents_is_none: html_tag(CONTENTS_IS_NONE) || outer_svg,
            svg_block: dom.namespace_uri(node) == Some(SVG) && dom.tag_name(node).is_some_and(|t| t == "text" || t == "foreignObject"),
        }
    }
}

/// A pseudo-element's `content`, computed against its originating element. See
/// `style::generated`.
fn pseudo_content(arena: &mut StyleArena, key: StyleKey, pseudo: &str, dom: &Dom, node: NodeId) -> StyleKey {
    let specified = arena.value(key, PropertyId::Content).to_string();
    let computed = crate::style::generated::computed_content(pseudo, &specified, dom, node, None);
    if computed == specified {
        return key;
    }
    let mut own = arena.own(key.own).clone();
    own.set(PropertyId::Content, &computed);
    StyleKey { inherited: key.inherited, own: arena.intern_own(own) }
}

/// Whether a style generates a box (and so takes part in counters and generated content).
fn generates_box_for(arena: &StyleArena, key: StyleKey) -> bool {
    arena.value(key, PropertyId::Display).trim() != "none"
}

/// `counter-reset`, `counter-set` and `counter-increment` of one style, applied to the counters in
/// scope in document order (CSS Lists 3 §4). `depth` is where a reset's scope starts: it and its
/// following siblings' subtrees see the new counter.
fn apply_counters(arena: &StyleArena, key: StyleKey, depth: usize, counters: &mut Vec<(String, i32, usize)>) {
    fn pairs(text: &str, default: i32) -> Vec<(String, i32)> {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("none") {
            return Vec::new();
        }
        let mut out: Vec<(String, i32)> = Vec::new();
        for token in t.split_ascii_whitespace() {
            match (token.parse::<i32>(), out.last_mut()) {
                (Ok(n), Some(last)) => last.1 = n,
                (Ok(_), None) => {}
                (Err(_), _) => out.push((token.to_string(), default)),
            }
        }
        out
    }
    for (name, value) in pairs(arena.value(key, PropertyId::CounterReset), 0) {
        match counters.iter_mut().rev().find(|(n, _, _)| *n == name) {
            Some(c) if c.2 == depth => c.1 = value,
            _ => counters.push((name, value, depth)),
        }
    }
    for (name, value) in pairs(arena.value(key, PropertyId::CounterSet), 0) {
        match counters.iter_mut().rev().find(|(n, _, _)| *n == name) {
            Some(c) => c.1 = value,
            None => counters.push((name, value, depth)),
        }
    }
    for (name, value) in pairs(arena.value(key, PropertyId::CounterIncrement), 1) {
        match counters.iter_mut().rev().find(|(n, _, _)| *n == name) {
            Some(c) => c.1 += value,
            None => counters.push((name, value, depth)),
        }
    }
}

/// The box a `::before`/`::after` style generates, if its `content` generates one.
fn pseudo_box(arena: &StyleArena, key: StyleKey, counters: &[(String, i32, usize)], quote_depth: &mut i32) -> Option<PseudoBox> {
    if !generates_box_for(arena, key) {
        return None;
    }
    let items = crate::style::generated::content_items(arena.value(key, PropertyId::Content))?;
    let values = |name: &str| counters.iter().filter(|(n, _, _)| n == name).map(|(_, v, _)| *v).collect::<Vec<_>>();
    let mut scope = crate::style::generated::ContentScope { counter: &values, quote_depth };
    let text = crate::style::generated::render_text(&items, &mut scope);
    Some(PseudoBox { key, text })
}

/// Blink's style adjustments that are about the element rather than its declarations.
fn adjust_for_element(arena: &mut StyleArena, key: StyleKey, facts: ElementFacts) -> StyleKey {
    let contents_to_none = facts.contents_is_none && arena.value(key, PropertyId::Display) == "contents";
    let to_block = facts.svg_block && {
        let d = arena.value(key, PropertyId::Display);
        d.starts_with("inline") || d == "math" || d == "ruby" || d == "-webkit-inline-box"
    };
    if !facts.textarea && !contents_to_none && !to_block {
        return key;
    }
    let mut own = arena.own(key.own).clone();
    let mut changed = false;
    if contents_to_none {
        own.set(PropertyId::Display, "none");
        changed = true;
    }
    if to_block {
        own.set(PropertyId::Display, "block");
        changed = true;
    }
    if facts.textarea {
        for p in [PropertyId::OverflowX, PropertyId::OverflowY] {
            if own.get(p).unwrap_or_else(|| p.initial()) == "visible" {
                own.set(p, "auto");
                changed = true;
            }
        }
    }
    if !changed {
        return key;
    }
    StyleKey { inherited: key.inherited, own: arena.intern_own(own) }
}

/// Every selector of every rule that declares an `animation-*` or `transition-*` longhand, bucketed
/// the way [`Matcher::build`] buckets all of them. The declaring test is the one
/// `declarative_update` always applied to a matched rule: a parsed declarative longhand, or a raw
/// (text-kept) declaration whose name starts with one of the two prefixes.
/// Does `rule` declare an animation or a transition?
fn rule_declares(sheet: &Stylesheet, rule: &crate::style::rules::CascadeRule) -> bool {
    sheet.declarations_of(rule).iter().any(|d| declarative::is_declarative(d.property))
        || sheet.raw_of(rule).iter().any(|d| {
            let n = sheet.slice(d.name);
            n.starts_with("animation") || n.starts_with("transition")
        })
}

/// The rules that declare an animation or a transition, for an element the page style pass has MATCHED
/// ([`Declaring::declares_matched`]): one of its matched rules is one -- `by_sheet` -- or one of the rest matches it,
/// the declaring rules that matching can leave out where [`Declaring::declares`]'s would not (`residual`, indexed):
/// a rule under `@container` (its condition is that matching's, not this question's) or `@scope`, a selector with a
/// pseudo-element (matched for the pseudo-element apart), and a sheet that styles a tree other than the page's
/// document (a shadow tree's, an adopted one).
#[derive(Default)]
struct DeclaringRules {
    by_sheet: Vec<Vec<bool>>,
    residual: RuleIndex,
}

impl DeclaringRules {
    fn of(sheets: &[Stylesheet], scopes: &[SheetScope], page: NodeId) -> DeclaringRules {
        let mut out = DeclaringRules::default();
        for (s, sheet) in sheets.iter().enumerate() {
            let tree_of_page = matches!(scopes.get(s), Some(SheetScope::Everywhere) | Some(SheetScope::Tree(_)))
                && !matches!(scopes.get(s), Some(SheetScope::Tree(t)) if *t != page);
            let rules = sheet.cascade_rules();
            let mut flags = vec![false; rules.len()];
            for (r, rule) in rules.iter().enumerate() {
                if !rule_declares(sheet, rule) {
                    continue;
                }
                flags[r] = true;
                for (n, selector) in rule.selectors.iter().enumerate() {
                    if !tree_of_page || rule.container != 0 || rule.scope != 0 || selector.pseudo_element().is_some() {
                        out.residual.insert(selector, RuleRef { sheet: s as u32, rule: r as u32, selector: n as u32 }, 0);
                    }
                }
            }
            out.by_sheet.push(flags);
        }
        out
    }

    fn declares(&self, m: &MatchedRule) -> bool {
        self.by_sheet.get(m.sheet as usize).and_then(|r| r.get(m.rule as usize)).copied().unwrap_or(false)
    }
}

fn declarative_rule_index(sheets: &[Stylesheet]) -> (RuleIndex, Reads) {
    let mut index = RuleIndex::default();
    let mut reads = Reads::default();
    for (s, sheet) in sheets.iter().enumerate() {
        for (r, rule) in sheet.cascade_rules().iter().enumerate() {
            if !rule_declares(sheet, rule) {
                continue;
            }
            for (n, selector) in rule.selectors.iter().enumerate() {
                reads = reads.or(Reads::of(selector));
                index.insert(selector, RuleRef { sheet: s as u32, rule: r as u32, selector: n as u32 }, 0);
            }
        }
    }
    (index, reads)
}

/// Above this many entries the memo is dropped and refilled: it is a cache, and a page that keeps
/// minting distinct inline styles must not grow it without bound.
const CASCADE_MEMO_CAP: usize = 1 << 18;

impl CascadeMemo {
    /// Drop the map if a document-wide input moved since it was filled.
    fn begin(&mut self, sheet_revision: u64, media: &MediaContext) {
        let generation =
            Some((sheet_revision, media.viewport_width.to_bits(), media.viewport_height.to_bits()));
        if self.generation != generation || self.map.len() > CASCADE_MEMO_CAP {
            self.map.clear();
            self.generation = generation;
        }
    }
}

/// [`compute_animated`] for one element of a whole-document pass, answered from the
/// [`CascadeMemo`] when every input is one it has seen. `attr` is the element's `style` attribute.
#[allow(clippy::too_many_arguments)]
fn cascade_memoised(
    memo: &mut CascadeMemo,
    sheets: &[Stylesheet],
    matched: &[MatchedRule],
    attr: &str,
    hints: &str,
    parent: Option<StyleKey>,
    mine: ParentFormatting,
    ctx: &ComputeContext,
    arena: &mut StyleArena,
    animated: &[AnimatedValue],
    facts: ElementFacts,
) -> StyleKey {
    let run = |arena: &mut StyleArena| {
        // The presentational hints lead the block, the `style` attribute follows (`InlineStyle::hints`).
        let hint_count = if hints.is_empty() { 0 } else { parse_inline_style(hints).declarations().len() as u32 };
        let text: std::borrow::Cow<str> = if hints.is_empty() { attr.into() } else { format!("{hints}{attr}").into() };
        let inline_sheet = (!text.trim().is_empty()).then(|| parse_inline_style(&text));
        let inline = inline_sheet
            .as_ref()
            .map(|sheet| InlineStyle { sheet, decls: Span::of(0..sheet.declarations().len()), hints: hint_count });
        let ctx = ComputeContext { quirks: facts.quirks, ..*ctx };
        let key = compute_animated(sheets, matched, inline, parent, mine, &ctx, arena, animated);
        adjust_for_element(arena, key, facts)
    };
    if !animated.is_empty() {
        memo.runs += 1;
        return run(arena);
    }
    let input = CascadeInput {
        matched: matched.iter().map(|m| (m.sheet, m.rule, m.specificity, m.proximity)).collect(),
        inline: if hints.is_empty() { attr.into() } else { format!("{hints}\u{1}{attr}").into() },
        parent,
        flex_or_grid_item: mine == ParentFormatting::FlexOrGrid,
        root_font_size: ctx.root_font_size.to_bits(),
        min_logical_font_size: ctx.min_logical_font_size.to_bits(),
        facts,
        container: ctx.container.map(|(w, h)| (w.to_bits(), h.to_bits())),
    };
    if let Some(&key) = memo.map.get(&input) {
        return key;
    }
    memo.runs += 1;
    let key = run(arena);
    memo.map.insert(input, key);
    key
}

/// What [`Page::computed_style`] banked for one element, and everything an element BELOW it needs
/// in order to resume the cascade from here instead of from the document root.
///
/// P8a banked every ancestor of every element it was asked about, but only ever read the entry back
/// for the element in the question — so a descendant asked about next still re-cascaded the whole
/// chain, and a document walk stayed O(elements x depth). Measured on a 4,000-element fixture
/// nested 200 deep, reading one property off every element cost 1,736.9 ms against the same 4,000
/// elements flat at 73.4 ms — 23.7x for depth alone, where Chrome 150 is 0.89x.
///
/// Resuming needs three things and the [`StyleKey`] is only the first of them, which is why this is
/// a struct and not the bare key it replaces:
///
/// * `key` — the parent style the next element down inherits from.
/// * `children` — what this element's CHILDREN are laid out as
///   ([`ParentFormatting::of_children`]). It cannot be recomputed from `key` alone: `display:
///   contents` FORWARDS whatever its own parent established, so the value depends on the whole
///   chain above, not on this element's computed style.
/// * `root_font_size` — the `rem` base, which the document element fixes for everything below it.
///   Kept per entry rather than once per generation because a chain that never reaches the document
///   element (a detached subtree) makes its own top element the base, and two such chains in one
///   generation would otherwise overwrite each other's.
///
/// **This adds no staleness surface.** Every entry is discarded with the map the instant
/// [`Page::layout_fingerprint`] moves, so within one generation nothing the cascade reads has
/// changed and a banked ancestor is by construction what re-cascading it would produce. The trust
/// boundary is exactly the one P8a already relied on to answer `computed_style` from this map at
/// all; this only stops the walk re-deriving values it has already banked.
#[derive(Debug, Clone, Copy)]
struct MemoEntry {
    key: StyleKey,
    children: ParentFormatting,
    root_font_size: f32,
    /// Whether this element's PARENT made it a flex or grid item -- the context it was cascaded in,
    /// where `children` is the one it gives. Blink keeps the same fact on the style
    /// (`IsFlexOrGridOrCustomItem`), and `getComputedStyle` reads it for `min-width: auto`
    /// ([`Page::resolve_at_read`]).
    item: bool,
}

impl std::fmt::Debug for PageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageState")
            .field("sheets", &self.sheets.len())
            .field("owners", &self.owners.len())
            .field("matcher", &self.matcher.is_some())
            .field("sheet_revision", &self.sheet_revision)
            .field("synced_at_dom_revision", &self.synced_at_dom_revision)
            .field("arena", &self.arena)
            .field("animated", &!self.declarative.is_empty())
            .finish()
    }
}

impl Default for PageState {
    fn default() -> Self {
        PageState {
            sheets: vec![ua::stylesheet(), ua::quirks_stylesheet(), ua::hints_stylesheet()],
            owners: vec![None, None, None],
            scopes: vec![SheetScope::Everywhere; 3],
            adoptions: Adoptions::default(),
            sources: vec![String::new(), String::new(), String::new()],
            media_attrs: vec![None, None, None],
            matcher: None,
            script_registrations: Vec::new(),
            sheet_revision: 0,
            sheet_log: std::collections::VecDeque::new(),
            sheet_change: None,
            sheet_passes: 0,
            synced_at_dom_revision: None,
            sheet_nodes: Vec::new(),
            synced_at_structure: None,
            sheet_walks: 0,
            arena: StyleArena::default(),
            declarative: Declarative::default(),
            last_styled: 0,
            declarative_scan: DeclarativeScan::default(),
            animation_revision: 0,
            animated_generation: 0,
            // `u64::MAX` is "no fingerprint yet": the first query computes a real one, which will
            // not match, and the empty map is discarded for an empty map.
            style_memo: (u64::MAX, std::collections::HashMap::new()),
            fp_cache: std::cell::Cell::new(None),
            cascades_run: 0,
            last_animations: Default::default(),
            declarative_spec_walks: 0,
            cascade_memo: CascadeMemo::default(),
            declarative_index: None,
            declaring_rules: None,
            declarative_matches: DeclarativeMatches::default(),
            declarative_selector_tests: 0,
            style_reading: None,
            has_container_rules: false,
            container_reads: None,
            uses_cq_units: false,
            container_sizes: std::collections::HashMap::new(),
            container_generation: 0,
            container_basis: None,
            container_passes: 0,
            boxes_carried: 0,
            box_layouts_computed: 0,
            trees_built: 0,
            trees_patched: 0,
            pseudo_memo: (u64::MAX, std::collections::HashMap::new()),
            layouts_run: 0,
            natural_images: std::collections::HashMap::new(),
            natural_images_gen: 0,
            web_fonts: None,
            web_fonts_gen: 0,
            web_fonts_key: 0,
            viewer_sheet: None,
            match_cache: MatchCache::default(),
            doc_styles: std::collections::HashMap::new(),
            settled: None,
            settled_at: None,
            layout_origins: Default::default(),
            incremental_passes: 0,
            rescans_from_what_changed: 0,
            chains_only: false,
            elements_matched: 0,
            features: None,
        }
    }
}

/// A handle the CSSOM refers to a stylesheet by. It is the sheet's index, and index 0 is the
/// user-agent sheet, which script can never reach.
pub type SheetId = u32;

/// Is this element a `<link>` that carries a stylesheet?
///
/// `rel` is a space-separated token list matched ASCII-case-insensitively, which is what makes
/// `rel="alternate stylesheet"`, `rel="preload stylesheet"` and `rel=" STYLESHEET "` one — measured
/// on stock Chrome 150.0.7871.46, where each has a `.sheet` and appears in `document.styleSheets`. A
/// `<link>` with no `rel`, or one whose `rel` is `icon` / `preload` / `prefetch` / `manifest`, has no
/// sheet at all there, and neither has a stylesheet link with a `disabled` attribute
/// (`<link rel=stylesheet disabled>` is fetched and its `.sheet` is null).
pub fn is_stylesheet_link(dom: &Dom, node: NodeId) -> bool {
    let NodeKind::Element { name, .. } = &dom.node(node).kind else { return false };
    if !name.eq_ignore_ascii_case("link") || dom.get_attribute(node, "disabled").is_some() {
        return false;
    }
    link_rel_has(dom, node, "stylesheet")
}

/// Does `node`'s `rel` token list carry `token` (ASCII case-insensitively)?
pub fn link_rel_has(dom: &Dom, node: NodeId, token: &str) -> bool {
    dom.get_attribute(node, "rel").is_some_and(|rel| rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(token)))
}

/// Does the sheet `owner` owns contribute to the cascade? A `<style>` or stylesheet
/// `<link>` does while it is in a document (a shadow tree of a connected host counts) and a link that
/// is not an `alternate` one; anything else keeps its sheet row -- handles are never renumbered -- but
/// contributes nothing. Measured on stock Chrome 150.0.7871.46: a removed `<style>` stops styling at
/// once; `link.rel = "preload"` on a stylesheet link removes its sheet; `rel="alternate stylesheet"`
/// (with or without a `title`) does not apply; `disabled` on the link does not apply.
fn owner_contributes(dom: &Dom, owner: NodeId) -> bool {
    let root = dom.shadow_including_root(owner);
    // A `<template>`'s content is inert: Chrome 150 gives a `<style>` there no sheet and styles
    // nothing with it -- parsed from the page's markup, set by `innerHTML`, or a copy imported from it --
    // until it is connected (`tools/realweb/seed/templatestyle`). It used to style the WHOLE DOCUMENT here
    // (a template's `p { color }` recolored every `<p>`), kept because WPT's `template-for-style.html` passed
    // only through that leak; declarative partial updates (`<template for>`, S33a) now put the content into
    // the document the way Chrome does.
    if !matches!(dom.node(root).kind, NodeKind::Document) {
        return false;
    }
    match &dom.node(owner).kind {
        NodeKind::Element { name, .. } if name.eq_ignore_ascii_case("style") => true,
        NodeKind::Element { .. } => is_stylesheet_link(dom, owner) && !link_rel_has(dom, owner, "alternate"),
        _ => false,
    }
}

impl Page {
    /// Parse a stylesheet and keep it, returning the handle the CSSOM refers to it by.
    ///
    /// Handles are never reused and sheets are never removed. A page that builds thousands of
    /// throwaway `CSSStyleSheet`s grows this vector, which is a bounded leak for the life of the
    /// page and the alternative to a stale handle silently addressing somebody else's sheet — the
    /// CSSOM hands these out to script, and script can hold one as long as it likes.
    pub fn add_sheet(&self, origin: Origin, owner: Option<NodeId>, text: &str) -> SheetId {
        let mut state = self.state.lock().unwrap();
        state.sheets.push(parse_stylesheet(origin, text));
        state.owners.push(owner);
        // An owned sheet's tree is taken at the next document sync, which this makes walk.
        state.scopes.push(SheetScope::Everywhere);
        if owner.is_some() {
            state.synced_at_dom_revision = None;
            state.synced_at_structure = None;
        }
        state.sources.push(text.to_string());
        state.media_attrs.push(None);
        let added = state.sheets.len() - 1;
        state.sheets_changed(Some(added));
        (state.sheets.len() - 1) as SheetId
    }

    /// [`Page::add_sheet`] for a sheet script constructed (`new CSSStyleSheet()`): it styles nothing
    /// until a tree adopts it ([`Page::set_adopted_sheets`]).
    pub fn add_constructed_sheet(&self, text: &str) -> SheetId {
        let id = self.add_sheet(Origin::Author, None, text);
        let mut state = self.state.lock().unwrap();
        state.scopes[id as usize] = SheetScope::Adopted;
        state.sheets_changed(None);
        id
    }

    /// The tree rooted at `tree` (a document or a shadow root) now adopts `sheets`, in this order --
    /// its `adoptedStyleSheets` was set. A handle that is not a constructed sheet styles nothing
    /// through this.
    /// `CSS.registerProperty`: register `name` for the document. `false` when script already
    /// registered it (Blink's InvalidModificationError); the cascade re-types `name` from now on.
    pub fn register_property(&self, name: &str, reg: crate::style::registered::Registration) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.script_registrations.iter().any(|(n, _)| n == name) {
            return false;
        }
        state.script_registrations.push((name.to_string(), reg));
        state.sheets_changed(None);
        true
    }

    /// Whether `CSS.registerProperty` registered `name` in this document.
    pub fn is_script_registered(&self, name: &str) -> bool {
        self.state.lock().unwrap().script_registrations.iter().any(|(n, _)| n == name)
    }

    pub fn set_adopted_sheets(&self, tree: NodeId, sheets: &[SheetId]) {
        let mut state = self.state.lock().unwrap();
        let now = state.adoptions.get(&tree).map_or(&[][..], |v| &v[..]);
        if now == sheets {
            return;
        }
        if sheets.is_empty() {
            state.adoptions.remove(&tree);
        } else {
            state.adoptions.insert(tree, sheets.to_vec());
        }
        state.sheets_changed(None);
    }

    /// The current document's viewer UA rules: `Some(rules)` for a document a browser
    /// VIEWER built -- Chrome's JSON viewer -- and `None` for every other document, which switches the
    /// row off again. A navigation calls this once per document.
    pub fn set_viewer_rules(&self, rules: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        match (state.viewer_sheet.clone(), rules) {
            (Some((i, text)), Some(want)) if text == want => {
                if !state.sheets[i].disabled {
                    return;
                }
                state.sheets[i].disabled = false;
            }
            (Some((i, _)), Some(want)) => {
                state.sheets[i] = parse_stylesheet(Origin::UserAgent, want);
                state.sources[i] = want.to_string();
                state.viewer_sheet = Some((i, want.to_string()));
            }
            (Some((i, _)), None) => {
                if state.sheets[i].disabled {
                    return;
                }
                state.sheets[i].disabled = true;
            }
            (None, Some(want)) => {
                state.sheets.push(parse_stylesheet(Origin::UserAgent, want));
                state.owners.push(None);
                state.scopes.push(SheetScope::Everywhere);
                state.sources.push(want.to_string());
                state.media_attrs.push(None);
                let i = state.sheets.len() - 1;
                state.viewer_sheet = Some((i, want.to_string()));
            }
            (None, None) => return,
        }
        state.sheets_changed(None);
    }

    /// Walk the document's elements in tree order with their computed styles: `visit(node, value)`
    /// reads one property of that element's style, and answers whether to descend into its children
    /// (a `display: none` element's subtree renders nothing). The loads a stylesheet induces are
    /// decided from exactly this (`korvus-webapi`'s `core/css/sheet_loads.rs`).
    pub fn rendered_styles(&self, dom: &Dom, media: &MediaContext, mut visit: impl FnMut(NodeId, &dyn Fn(PropertyId) -> String) -> bool) {
        let mut stack = vec![dom.root()];
        while let Some(node) = stack.pop() {
            let descend = match &dom.node(node).kind {
                NodeKind::Element { .. } => match self.computed_style(dom, node, media) {
                    Some(key) => {
                        let value = |p: PropertyId| self.state.lock().unwrap().arena.value(key, p).to_string();
                        visit(node, &value)
                    }
                    None => false,
                },
                _ => true,
            };
            if descend {
                for child in dom.children(node).into_iter().rev() {
                    stack.push(child);
                }
            }
        }
    }

    /// Read one sheet. `None` when the handle is not one this page issued.
    pub fn with_sheet<R>(&self, id: SheetId, f: impl FnOnce(&Stylesheet) -> R) -> Option<R> {
        let state = self.state.lock().unwrap();
        Some(f(state.sheets.get(id as usize)?))
    }

    /// Edit one sheet. The rule index is dropped, because it addresses rules that may have moved.
    pub fn with_sheet_mut<R>(&self, id: SheetId, f: impl FnOnce(&mut Stylesheet) -> R) -> Option<R> {
        let mut state = self.state.lock().unwrap();
        let result = f(state.sheets.get_mut(id as usize)?);
        state.sheets_changed(Some(id as usize));
        Some(result)
    }

    /// How many sheets there are, the user-agent one included.
    pub fn sheet_count(&self) -> usize {
        self.state.lock().unwrap().sheets.len()
    }

    /// Make sure every `<style>` element and every `<link rel=stylesheet>` in the document has a
    /// sheet, in document order, and that a `<style>`'s text is current.
    ///
    /// This is what makes styling work for a page that never touches the CSSOM at all — which is
    /// most pages. Sheets used to appear only when script read `styleEl.sheet`, so a document with
    /// stylesheets and no script had no styles.
    ///
    /// # Why a `<link>` gets an EMPTY sheet here rather than one the walk can fill
    ///
    /// A `<style>`'s text is in the arena, so the walk can read it. A `<link>`'s is on the network,
    /// and arrives at some later turn of the event loop ([`Page::set_linked_sheet`]). The walk still
    /// has to create the entry, because a sheet's INDEX in `sheets` is its position in the cascade
    /// (`compute::Rank::order`) and handles are never renumbered — so a linked sheet appended
    /// whenever its bytes happened to land would sort after `<style>`s that follow it in the
    /// markup, and a tie between two equally specific rules would go the wrong way. Creating the
    /// row in the document-order walk puts it where the markup put it, and the bytes fill it in.
    ///
    /// It walks the whole document each time. That is O(nodes) per style query, which is honest
    /// rather than fast: there is no mutation counter on the arena to key a cache off, and a cache
    /// that missed a `<style>` somebody appended would be worse than a walk. D9 says invalidation
    /// waits for a measurement to demand it, and this is the place that measurement would look.
    pub fn sync_document_sheets(&self, dom: &Dom) {
        let mut state = self.state.lock().unwrap();
        // Blink's quirks sheet applies to a quirks-mode document and to nothing else.
        let quirks = dom.document_mode(dom.root()) == korvus_dom::DocumentMode::Quirks;
        if state.sheets[QUIRKS_SHEET].disabled == quirks {
            state.sheets[QUIRKS_SHEET].disabled = !quirks;
            state.sheets_changed(None);
        }
        // Nothing in the tree has moved since the last walk, so the walk cannot find anything new.
        // `Dom::revision` bumps on entry to every mutating method and over-bumps by design, so this
        // can be early but never late -- a missed sync would be a stale style, and that is the one
        // failure this must not have.
        if state.synced_at_dom_revision == Some(dom.revision()) {
            return;
        }
        state.synced_at_dom_revision = Some(dom.revision());
        // ONLY ATTRIBUTES MOVED since the last whole walk (the structure revision is the revision less attribute
        // writes; making a node, inserting, removing or retexting one all move it): the `<style>` and `<link>`
        // elements are the ones that walk found, in the same order, and every `<style>`'s text is the one it
        // compared -- so only their attributes (`rel`, `media`, `disabled`) are read again. A style, class or `data-*`
        // write followed by a geometry read walked the whole arena and copied every `<style>`'s text each time.
        let structure = (dom.structure_revision(), dom.instance());
        let whole = state.synced_at_structure != Some(structure);
        let nodes = if whole {
            state.sheet_walks += 1;
            state.synced_at_structure = Some(structure);
            let sheet_element = |n: NodeId| matches!(&dom.node(n).kind, NodeKind::Element { name, .. } if name.eq_ignore_ascii_case("style") || name.eq_ignore_ascii_case("link"));
            (0..dom.len() as NodeId).filter(|&n| sheet_element(n)).collect()
        } else {
            let nodes = std::mem::take(&mut state.sheet_nodes);
            if cfg!(test) || verify::enabled() {
                Self::check_sheet_nodes(&state, dom, &nodes);
            }
            nodes
        };
        for &node in &nodes {
            let NodeKind::Element { name, .. } = &dom.node(node).kind else { continue };
            let style = name.eq_ignore_ascii_case("style");
            if !style && !is_stylesheet_link(dom, node) {
                continue;
            }
            let at = state.owners.iter().position(|o| *o == Some(node));
            // A `<link>`'s rules come from the network, never from the element. Its text content is
            // empty (it is a void element) and reading it as a source would make the "has the text
            // moved?" test below reparse the loaded sheet back to nothing on the next walk. A `<style>`'s is read
            // only where it can have moved: on a whole walk, or for one no walk registered yet.
            let text = if style && (whole || at.is_none()) { dom.text_content(node) } else { String::new() };
            let media = dom.get_attribute(node, "media").map(str::to_string);
            match at {
                Some(i) => {
                    if state.media_attrs[i] != media {
                        state.sheets[i].set_media_text(media.as_deref().unwrap_or(""));
                        state.media_attrs[i] = media;
                        state.sheets_changed(Some(i));
                    }
                    // Reparse only when the element's text actually moved: a page that reads
                    // `getComputedStyle` in a loop must not reparse its stylesheets each time, and
                    // a sheet script has edited must not be overwritten from its element.
                    if style && whole && state.sources[i] != text {
                        state.sheets[i].replace_sync(&text);
                        state.sources[i] = text;
                        state.sheets_changed(Some(i));
                    }
                }
                None => {
                    let mut sheet = parse_stylesheet(Origin::Author, &text);
                    sheet.set_media_text(media.as_deref().unwrap_or(""));
                    state.sheets.push(sheet);
                    state.owners.push(Some(node));
                    state.scopes.push(sheet_scope(dom, node));
                    state.sources.push(text);
                    state.media_attrs.push(media);
                    let added = state.sheets.len() - 1;
                    state.sheets_changed(Some(added));
                }
            }
        }
        state.sheet_nodes = nodes;
        // Whether each owned sheet contributes, re-asked on every tree change: an owner removed from
        // the document, a link whose `rel` stopped being a stylesheet, a `disabled` link. The sheet
        // rows stay where they are.
        for i in 0..state.owners.len() {
            let Some(owner) = state.owners[i] else { continue };
            let off = !owner_contributes(dom, owner);
            if state.sheets[i].disabled != off {
                state.sheets[i].disabled = off;
                state.sheets_changed(Some(i));
            }
            // And which tree it styles: a `<style>` moved into a shadow root styles that tree now.
            let scope = sheet_scope(dom, owner);
            if state.scopes[i] != scope {
                state.scopes[i] = scope;
                state.sheets_changed(None);
            }
        }
    }

    /// What a walk of only the `<style>` and `<link>` elements rests on, checked against the whole arena (tests, and
    /// `KORVUS_VERIFY_INCREMENTAL`): the same elements in the same order, and every registered `<style>`'s text the
    /// source its sheet was parsed from.
    fn check_sheet_nodes(state: &PageState, dom: &Dom, nodes: &[NodeId]) {
        let whole: Vec<NodeId> = (0..dom.len() as NodeId)
            .filter(|&n| matches!(&dom.node(n).kind, NodeKind::Element { name, .. } if name.eq_ignore_ascii_case("style") || name.eq_ignore_ascii_case("link")))
            .collect();
        let mut wrong = (whole != nodes).then(|| format!("sheet owners {nodes:?}, the whole arena's {whole:?}"));
        for &n in &whole {
            let style = matches!(&dom.node(n).kind, NodeKind::Element { name, .. } if name.eq_ignore_ascii_case("style"));
            if let (true, Some(i)) = (style, state.owners.iter().position(|o| *o == Some(n))) {
                if wrong.is_none() && state.sources[i] != dom.text_content(n) {
                    wrong = Some(format!("{}'s text moved under an unchanged structure revision", verify::describe(dom, n)));
                }
            }
        }
        if let Some(what) = wrong {
            if cfg!(test) {
                panic!("the <style> walk of only the sheet owners: {what}");
            }
            verify::mismatch(&format!("the <style> walk of only the sheet owners: {what}"));
        }
    }

    /// The handle for a `<style>` or `<link rel=stylesheet>` element's sheet, creating it if this is
    /// the first time anyone has asked. Script reaching `styleEl.sheet` / `linkEl.sheet` must get the
    /// sheet the cascade is already using, not a second empty one.
    pub fn sheet_for_owner(&self, dom: &Dom, owner: NodeId) -> SheetId {
        self.sync_document_sheets(dom);
        let mut state = self.state.lock().unwrap();
        if let Some(i) = state.owners.iter().position(|o| *o == Some(owner)) {
            return i as SheetId;
        }
        let text = dom.text_content(owner);
        let media = dom.get_attribute(owner, "media").map(str::to_string);
        let mut sheet = parse_stylesheet(Origin::Author, &text);
        sheet.set_media_text(media.as_deref().unwrap_or(""));
        state.sheets.push(sheet);
        state.owners.push(Some(owner));
        state.scopes.push(sheet_scope(dom, owner));
        state.sources.push(text);
        state.media_attrs.push(media);
        let added = state.sheets.len() - 1;
        state.sheets_changed(Some(added));
        (state.sheets.len() - 1) as SheetId
    }

    /// A `<link rel=stylesheet>`'s BYTES have arrived: parse them into the sheet the cascade already
    /// holds for that element, at the position the document-order walk gave it.
    ///
    /// `origin_clean` is CSSOM's own flag and it is set here because this is the only place that
    /// knows the response: false for a cross-origin sheet fetched without CORS, which a page may
    /// still be STYLED by but may not READ (`cssRules` throws `SecurityError`). It is the sheet's,
    /// not the rules', so it rides on [`Stylesheet`] rather than on the CSSOM wrapper — the wrapper
    /// is minted lazily by whoever reads `.sheet` first, which is usually long after this.
    pub fn set_linked_sheet(&self, dom: &Dom, owner: NodeId, text: &str, origin_clean: bool) -> SheetId {
        let id = self.sheet_for_owner(dom, owner);
        let mut state = self.state.lock().unwrap();
        let i = id as usize;
        if let Some(sheet) = state.sheets.get_mut(i) {
            sheet.replace_sync(text);
            sheet.origin_clean = origin_clean;
            state.sources[i] = text.to_string();
            state.sheets_changed(Some(i));
        }
        id
    }

    /// The computed style of one element.
    ///
    /// Computed down the ancestor chain rather than by a document-wide pass, because that is the
    /// shape of the question: `getComputedStyle(el)` needs `el` and what it inherits from, and
    /// nothing else. It is also always fresh, which is what lets v1 keep no invalidation map at all
    /// (design D9).
    ///
    /// The answer is MEMOISED per element under [`Page::layout_fingerprint`] (`style_memo`), which
    /// does not weaken that sentence: the whole map is thrown away the instant the fingerprint
    /// moves, so a hit is only ever an answer for a document that has not changed since it was
    /// taken. See the field's own comment for why `getComputedStyle` needs it.
    ///
    /// # The walk STOPS at the first ancestor the map already holds
    ///
    /// The chain is climbed rather than descended for exactly that reason: a document walk asks
    /// about a parent before its child, so by the time the child is asked the whole chain above it
    /// is already banked and re-deriving it is pure repetition. P8a banked the ancestors but never
    /// read them back mid-walk, which left the cost O(elements x depth) — and depth is not a
    /// theoretical axis, it is what makes the three worst corpus pages the deeply-nested
    /// documentation sites. Measured here on 4,000 elements, one property read off each:
    ///
    /// ```text
    ///                                     korvus before   korvus after   chrome 150
    ///   flat (4000 spans, depth 1)             73.4 ms        67.5 ms        1.8 ms
    ///   deep (3800 spans under 200 divs)     1736.9 ms        75.6 ms        1.6 ms
    ///                        deep/flat          23.66x          1.12x         0.89x
    /// ```
    ///
    /// Chrome is flat in depth, and so is this now. What is left is a per-element constant that has
    /// nothing to do with the cascade — see the flat column, which the resume barely moves.
    pub fn computed_style(&self, dom: &Dom, node: NodeId, media: &MediaContext) -> Option<StyleKey> {
        perf_counters::add(&perf_counters::COMPUTED_STYLE_CALLS, 1);
        self.sync_document_sheets(dom);
        // BEFORE the lock, and AFTER the sheet sync: `layout_fingerprint` takes the lock itself, and
        // the sync is what makes `sheet_revision` current.
        let fingerprint = self.layout_fingerprint(dom, media);
        let mut state = self.state.lock().unwrap();
        if !matches!(dom.node(node).kind, NodeKind::Element { .. }) {
            return None;
        }
        if state.style_memo.0 != fingerprint {
            state.style_memo = (fingerprint, std::collections::HashMap::new());
        } else if let Some(entry) = state.style_memo.1.get(&node) {
            return Some(entry.key);
        }
        Self::ensure_matcher(&mut state);
        // A container query is answered by a laid-out ancestor. When the sizes were not taken at this
        // document, lay it out first -- once: the layout records the basis, and the basis does not
        // include the sizes it produces.
        //
        // Only for a node of the PAGE document. A node of a FRAME document arrives here with
        // its FRAME's media (its viewport, its `@media`), and settling the PAGE (`dom.root()`) with it
        // would lay the page out at the frame's viewport -- then the next page read lays it back, so a
        // page interleaving `getComputedStyle` on itself and on a frame restyled the whole page on every
        // call (oecd.org: 614 full page restyles during load). The frame's own containers are settled by
        // the caller's `refresh_frame` before this; the page's are irrelevant to a frame node.
        let page_node = dom.is_connected(node);
        if page_node && state.has_container_rules && state.container_basis != Some(Self::container_basis_of(&state, dom, media)) {
            drop(state);
            self.settle_containers(dom, media);
            return self.computed_style(dom, node, media);
        }
        let kept = Self::kept_style(&mut state, dom, node, media);
        if let Some(entry) = kept.filter(|_| !verify::enabled()) {
            state.style_memo.1.insert(node, entry);
            return Some(entry.key);
        }

        // Climb to the nearest ancestor whose style this generation already knows, and start there.
        // `seed` is `None` only when the climb ran out of ancestors, which is the document element
        // -- or the top of a detached subtree, where the cascade has nothing above it either.
        // Non-elements are simply not collected; the old loop collected them and skipped them.
        let mut chain = vec![node];
        let mut seed: Option<MemoEntry> = None;
        // The climb follows the FLAT tree, as the page style pass does: a shadow tree's top level inherits from the
        // host (CSS Scoping §3.3), and a slotted light child from its SLOT, not from the host -- Chrome
        // 150, a span slotted into `<slot>` styled `color: blue` under a black host reads blue (korvus read the
        // host's black). A light child no slot renders keeps its DOM parent.
        let mut flat = crate::flat::FlatTree::default();
        let mut style_parent = |n: NodeId| flat.parent(dom, n).or_else(|| dom.node(n).parent.or_else(|| dom.shadow_host_of(n)));
        let mut cur = style_parent(node);
        while let Some(n) = cur {
            if matches!(dom.node(n).kind, NodeKind::Element { .. }) {
                if let Some(&entry) = state.style_memo.1.get(&n) {
                    seed = Some(entry);
                    break;
                }
                chain.push(n);
            }
            cur = style_parent(n);
        }
        chain.reverse();

        let state = &mut *state;
        // THE SAME CASCADE MEMO THE WHOLE-DOCUMENT PASSES USE, under the same
        // generation rule. `computed_style` cascaded every element of every chain it walked, so a
        // document walk over 4,000 alike `<div class=cN><p>…<span>` items -- the P9c fixture --
        // computed 12,004 styles to intern a few dozen. The memo's key is the cascade's whole input
        // and its generation is the stylesheets and the viewport (see [`CascadeMemo`]); nothing
        // here adds to either, so an answer from it is the answer `compute_animated` would give.
        state.cascade_memo.begin(state.sheet_revision, media);
        let matcher = state.matcher.as_ref().expect("just built");
        let mut ctx = ComputeContext {
            viewport_width: media.viewport_width,
            viewport_height: media.viewport_height,
            root_font_size: seed.map_or(16.0, |e| e.root_font_size),
            quirks: false,
            container: None,
            min_logical_font_size: ComputeContext::min_logical_of(&media.theme),
        };
        let mut matched = Vec::new();
        let mut key = seed.map(|e| e.key);
        let mut formatting = seed.map_or(ParentFormatting::Flow, |e| e.children);
        for n in chain {
            {
                let memo = &state.style_memo.1;
                let style_of = |a: NodeId| memo.get(&a).map(|e| e.key);
                let (arena, sizes, owners) = (&state.arena, &state.container_sizes, &state.owners);
                let root_font_size = ctx.root_font_size;
                let containers = |at: NodeId, cond: &ContainerCondition| {
                    container_holds(arena, sizes, &style_of, dom, at, cond, media, root_font_size)
                };
                let owner_parent = |sheet: u32| owner_parent_of(owners, dom, sheet);
                let env = MatchEnv { containers: Some(&containers), owner_parent: Some(&owner_parent), ancestors: None, tree: None };
                matcher.match_element_in(&state.sheets, dom, n, media, &env, &mut matched);
            }
            if state.uses_cq_units {
                let memo = &state.style_memo.1;
                let style_of = |a: NodeId| memo.get(&a).map(|e| e.key);
                ctx.container = Some(cq_basis(&state.arena, &state.container_sizes, &style_of, dom, n, media));
            }
            let attr = dom.style_for_cascade(n).unwrap_or("");
            let animated = state.declarative.animated_for(n).to_vec();
            let item = formatting == ParentFormatting::FlexOrGrid;
            let computed = cascade_memoised(
                &mut state.cascade_memo, &state.sheets, &matched, attr, &crate::style::hints::for_element(dom, n), key, formatting, &ctx,
                &mut state.arena, &animated, ElementFacts::of(dom, n),
            );
            // What THIS element makes of its own children, for the next turn of the loop. The chain
            // is root-first, so the value is always the one the next element's parent produced.
            formatting = ParentFormatting::of_children(&state.arena, computed, formatting);
            if key.is_none() {
                // The first element is the document element, and `rem` means *its* font size --
                // for itself as well as for everything below it. `key` is already `Some` when the
                // walk resumed from a banked ancestor, so this runs only for a chain that really
                // started at a root, and `ctx.root_font_size` carries the seed's otherwise.
                if let Some(px) = crate::style::compute::rem_basis(state.arena.inherited(computed.inherited)) {
                    ctx.root_font_size = px;
                }
            }
            // EVERY element of the chain, not just the last: the walk has already paid for each
            // ancestor's key at its own position in the cascade, and a document walk asks about the
            // ancestors again for every descendant. Recording them, AND resuming from them, is what
            // turns `innerText` over a tree from O(nodes x depth) cascades into O(nodes).
            state.style_memo.1.insert(
                n,
                MemoEntry { key: computed, children: formatting, root_font_size: ctx.root_font_size, item },
            );
            state.cascades_run += 1;
            key = Some(computed);
        }
        if let Some(entry) = kept {
            verify::started();
            let at = state.style_memo.1.get(&node).copied();
            if at.map(|e| (e.key, e.children, e.root_font_size.to_bits(), e.item))
                != Some((entry.key, entry.children, entry.root_font_size.to_bits(), entry.item))
            {
                verify::mismatch(&format!(
                    "computed style of {} in {}: the page style pass's {:?}, the chain's {:?}",
                    verify::describe(dom, node),
                    verify::document_title(dom),
                    (entry.key, entry.children, entry.root_font_size, entry.item),
                    at.map(|e| (e.key, e.children, e.root_font_size, e.item)),
                ));
            }
        }
        key
    }

    /// [`Page::computed_style`]'s answer from the page style pass, brought up to date
    /// first, when it is the same function: an element of the page's document with no shadow host
    /// above it (the chain climbs the DOM, the style pass the flat tree). The matching, the cascade, the
    /// parent, the formatting it inherits, `rem`, the animated values and -- the container sizes having
    /// been settled by the caller -- the `cq*` basis and the container answers are then the chain's.
    /// `None` where it is not, or where bringing the styles up to date would style the whole document.
    ///
    /// The chain walk cascaded the element's ancestors again after every write that moved the
    /// layout fingerprint: on www.mapbox.com, whose script reads computed styles between writes, 11 %
    /// of the load's cycles. The pass restyles what the write reached.
    fn kept_style(state: &mut PageState, dom: &Dom, node: NodeId, media: &MediaContext) -> Option<MemoEntry> {
        let root = dom.root();
        if state.chains_only || dom.root_of(node) != root {
            return None;
        }
        // Nor under a node that is not an element, linked between elements (a fragment inserted as it
        // is: www.worldbank.org, knockoutjs.com's cufon): the chain climbs over it to the element above
        // and inherits from that, the style pass inherits from it -- nothing. Nor under a shadow host:
        // its light children are styled in the flat tree, slotted under a slot or not at all, and the
        // chain climbs the DOM (github.com, whose components have shadow roots, read every element's
        // style through chains before this).
        let shadow = dom.has_shadow_roots();
        let mut up = dom.node(node).parent;
        while let Some(p) = up.filter(|&p| p != root) {
            if !matches!(dom.node(p).kind, NodeKind::Element { .. }) || (shadow && dom.shadow_root_of(p).is_some()) {
                return None;
            }
            up = dom.node(p).parent;
        }
        // And only while bringing the kept styles up to date costs what changed: not before the first
        // pass, not after a media change or a sheet change the pass cannot plan (a pass from nothing,
        // see `Page::sheet_change`), not under `@scope` (every pass is one). The chain answers those,
        // as before.
        let basis = Self::style_basis(state, dom, root, media);
        if !state.doc_styles.get(&root).is_some_and(|d| d.basis == Some(basis)) && Self::sheet_change(state, dom, root, &basis).is_none() {
            return None;
        }
        Self::ensure_features(state);
        if state.features.as_ref().is_some_and(|(_, f)| f.scope) {
            return None;
        }
        state.cascade_memo.begin(state.sheet_revision, media);
        Self::style_incremental(state, dom, root, media);
        let doc = &state.doc_styles[&root];
        let key = doc.keys.get(node as usize).copied().flatten()?;
        let item = dom.node(node).parent.is_some_and(|p| doc.formatting[p as usize] == ParentFormatting::FlexOrGrid);
        perf_counters::add(&perf_counters::COMPUTED_STYLES_KEPT, 1);
        Some(MemoEntry { key, children: doc.formatting[node as usize], root_font_size: doc.root_font_size, item })
    }

    /// Lay the page out to settle its container-query sizes for a style read, keeping that layout for
    /// the next such read: a tree built as the kept one was is not laid out again. Measured
    /// 2026-10-02: www.hubspot.com's `cq*` units made 250 of its 420 layouts this way.
    fn settle_containers(&self, dom: &Dom, media: &MediaContext) {
        let previous = self.state.lock().unwrap().settled.take().and_then(KeptLayout::take);
        let out = match previous {
            Some(p) => self.relayout_document_owned(dom, dom.root(), media, p).into_inner(),
            None => self.layout(dom, media),
        };
        // Taken after the layout: the sizes it settled are part of the fingerprint.
        let at = self.page_geometry_fingerprint(dom, media);
        let mut state = self.state.lock().unwrap();
        state.settled = Some(KeptLayout::new(out));
        state.settled_at = Some((at, *media));
    }

    /// The layout the last container settle made, when it was made at `fingerprint` -- the page's geometry
    /// as a geometry read finds it now -- moved out to that read. A style read on a page with `@container`
    /// rules settles the containers with a whole layout, and the geometry read that asked for the style
    /// (`offsetWidth` flushing a CSS animation's values, a frame's scroll and resize checks) then laid
    /// the same document out again: www.wayfair.com's search page, 47 running opacity animations under
    /// container rules, ran ~20 whole layouts a second and 273 of 600 were the second of such a pair.
    ///
    /// Only for the same media as well: a settle is the page's, at the page's media, and a read at another
    /// (a frame's viewport) never takes it -- its fingerprint hashes the viewport too, and this says so outright.
    pub fn take_settled_layout(&self, dom: &Dom, media: &MediaContext, fingerprint: u64) -> Option<LaidOut> {
        let _ = dom;
        let mut state = self.state.lock().unwrap();
        if state.settled_at != Some((fingerprint, *media)) {
            return None;
        }
        state.settled_at = None;
        state.settled.take().and_then(KeptLayout::take)
    }

    /// The computed style of `node`'s `pseudo` pseudo-element (`"before"`, `"marker"`,
    /// `"-webkit-scrollbar"`), as `getComputedStyle(el, "::before")` reads it.
    ///
    /// The pseudo-element inherits from its originating element, is a flex or grid item when the
    /// element is a container (Chrome 150: `::before` of a `display: flex` box computes `display:
    /// block`), and matches only the rules that end in `::pseudo`. Its `content` is computed here --
    /// `attr()` substituted, `normal` on `::before`/`::after` read as `none` -- because that step needs
    /// the element, which the cascade does not see.
    pub fn computed_pseudo_style(&self, dom: &Dom, node: NodeId, pseudo: &str, media: &MediaContext) -> Option<StyleKey> {
        let element = self.computed_style(dom, node, media)?;
        let mut state = self.state.lock().unwrap();
        let fingerprint = state.style_memo.0;
        if state.pseudo_memo.0 != fingerprint {
            state.pseudo_memo = (fingerprint, std::collections::HashMap::new());
        } else if let Some(&k) = state.pseudo_memo.1.get(&(node, pseudo.to_string())) {
            return Some(k);
        }
        Self::ensure_matcher(&mut state);
        // The page style pass, current for this element, styled its `::before`/`::after` with it -- the same match
        // and cascade as below (`PseudoStyling::keys`) -- and kept the key: no matching again. The stylesheet-load
        // scan reads both of every rendered element's after each change, and matched each again: 2.3% of lowes.com's
        // main thread.
        let kept = Self::pseudo_kept(&mut state, dom, node, pseudo, media);
        if let (Some(k), false) = (kept, verify::enabled()) {
            state.pseudo_memo.1.insert((node, pseudo.to_string()), k);
            return Some(k);
        }
        let entry = state.style_memo.1.get(&node).copied().unwrap_or(MemoEntry {
            key: element,
            children: ParentFormatting::Flow,
            root_font_size: 16.0,
            item: false,
        });
        let state = &mut *state;
        // The page style pass matched this element's pseudo-elements with it: where it found
        // no rule for this one, there is none to find again. The stylesheet-load scan reads both
        // pseudo-elements of every rendered element, and matching each again was 20 % of
        // www.jlcpcb.com's load -- for elements no such rule styles.
        let unmatched = Self::pseudo_unmatched(state, dom, node, pseudo, media);
        state.cascade_memo.begin(state.sheet_revision, media);
        let matcher = state.matcher.as_ref().expect("just built");
        let mut matched = Vec::new();
        if !unmatched || verify::enabled() {
            let memo = &state.style_memo.1;
            let style_of = |a: NodeId| memo.get(&a).map(|e| e.key);
            let (arena, sizes, owners) = (&state.arena, &state.container_sizes, &state.owners);
            let root_font_size = entry.root_font_size;
            let containers = |at: NodeId, cond: &ContainerCondition| {
                container_holds(arena, sizes, &style_of, dom, at, cond, media, root_font_size)
            };
            let owner_parent = |sheet: u32| owner_parent_of(owners, dom, sheet);
            // A pseudo-element's query container search starts at its originating element.
            let env = MatchEnv { containers: Some(&containers), owner_parent: Some(&owner_parent), ancestors: None, tree: None };
            matcher.match_pseudo(&state.sheets, dom, node, media, pseudo, &env, &mut matched);
        }
        if unmatched && !matched.is_empty() {
            verify::mismatch(&format!(
                "::{pseudo} of {} in {}: the page style pass matched no rule, matching finds {}",
                verify::describe(dom, node),
                verify::document_title(dom),
                matched.len(),
            ));
        }
        let ctx = ComputeContext {
            viewport_width: media.viewport_width,
            viewport_height: media.viewport_height,
            root_font_size: entry.root_font_size,
            min_logical_font_size: ComputeContext::min_logical_of(&media.theme),
            ..ComputeContext::default()
        };
        let facts = ElementFacts { textarea: false, quirks: dom.document_mode_of(node) == korvus_dom::DocumentMode::Quirks, contents_is_none: false, svg_block: false };
        let key = cascade_memoised(
            &mut state.cascade_memo, &state.sheets, &matched, "", "", Some(entry.key), entry.children, &ctx,
            &mut state.arena, &[], facts,
        );
        let key = pseudo_content(&mut state.arena, key, pseudo, dom, node);
        if kept.is_some_and(|k| k != key) {
            verify::mismatch(&format!(
                "::{pseudo} of {} in {}: the page style pass kept another style than matching gives",
                verify::describe(dom, node),
                verify::document_title(dom),
            ));
        }
        state.pseudo_memo.1.insert((node, pseudo.to_string()), key);
        Some(key)
    }

    /// The `::before` / `::after` style the page style pass, current for `node` ([`Page::kept_style`]), cascaded for it
    /// when it styled it; `None` when it has none to give (no rule matched, another pseudo-element, a stale pass).
    fn pseudo_kept(state: &mut PageState, dom: &Dom, node: NodeId, pseudo: &str, media: &MediaContext) -> Option<StyleKey> {
        if !matches!(pseudo, "before" | "after") {
            return None;
        }
        let entry = Self::kept_style(state, dom, node, media)?;
        if !generates_box_for(&state.arena, entry.key) {
            return None;
        }
        let keys = state.doc_styles.get(&dom.root())?.pseudo.get(node as usize)?.as_ref()?;
        if pseudo == "before" { keys.before } else { keys.after }
    }

    /// Did the page style pass, current for `node` ([`Page::kept_style`]), match no rule for its
    /// `::before` / `::after` when it styled it? It matches an element's pseudo-elements whenever it
    /// styles an element that generates a box, so an absent key there is "no rule", not "not asked".
    fn pseudo_unmatched(state: &mut PageState, dom: &Dom, node: NodeId, pseudo: &str, media: &MediaContext) -> bool {
        if !matches!(pseudo, "before" | "after") {
            return false;
        }
        let Some(entry) = Self::kept_style(state, dom, node, media) else { return false };
        if !generates_box_for(&state.arena, entry.key) {
            return false;
        }
        let Some(doc) = state.doc_styles.get(&dom.root()) else { return false };
        match doc.pseudo.get(node as usize) {
            Some(Some(keys)) => if pseudo == "before" { keys.before.is_none() } else { keys.after.is_none() },
            Some(None) => true,
            None => false,
        }
    }

    /// One property of a pseudo-element's computed style. `None` when the element has no style.
    pub fn computed_pseudo_value(&self, dom: &Dom, node: NodeId, pseudo: &str, media: &MediaContext, property: PropertyId, laid_out: Option<&LaidOut>) -> Option<String> {
        let key = self.computed_pseudo_style(dom, node, pseudo, media)?;
        let computed = {
            let state = self.state.lock().unwrap();
            // A `::before`/`::after` is an item of its own element when that element is a flex or grid
            // container (Chrome 150: `auto` on a `display: flex` box's `::before`).
            let item = state.style_memo.1.get(&node).is_some_and(|e| e.children == ParentFormatting::FlexOrGrid);
            Self::resolve_at_read(&state.arena, key, item, property, state.arena.value(key, property)).to_string()
        };
        if !resolved::resolves_from_layout(property) {
            return Some(computed);
        }
        let owned;
        let laid_out = match laid_out {
            Some(l) => l,
            None => {
                owned = self.layout(dom, media);
                &owned
            }
        };
        Some(laid_out.resolved_pseudo_value(node, pseudo, property, &computed).unwrap_or(computed))
    }

    /// The custom properties a pseudo-element's style carries, in enumeration order.
    pub fn computed_pseudo_custom_names(&self, dom: &Dom, node: NodeId, pseudo: &str, media: &MediaContext) -> Vec<String> {
        let Some(key) = self.computed_pseudo_style(dom, node, pseudo, media) else { return Vec::new() };
        let state = self.state.lock().unwrap();
        state.arena.custom_values(key).map(|(n, _)| n.to_string()).collect()
    }

    /// A pseudo-element's computed custom property.
    pub fn computed_pseudo_custom(&self, dom: &Dom, node: NodeId, pseudo: &str, media: &MediaContext, name: &str) -> Option<String> {
        let key = self.computed_pseudo_style(dom, node, pseudo, media)?;
        let state = self.state.lock().unwrap();
        state.arena.custom_value(key, name).map(str::to_string)
    }

    /// One property's COMPUTED value, as the cascade holds it -- what `computedStyleMap()` reports
    /// (Blink reads it with `CSSValuePhase::kComputedValue`), where [`Page::computed_value`] answers the
    /// resolved one: `width: 50%` stays `50%` and `min-width` stays `auto`, with no layout asked
    /// (`tools/realweb/seed/stylemap`).
    pub fn computed_phase_value(&self, dom: &Dom, node: NodeId, media: &MediaContext, property: PropertyId) -> Option<String> {
        let key = self.computed_style(dom, node, media)?;
        Some(self.state.lock().unwrap().arena.value(key, property).to_string())
    }

    /// One RESOLVED property, as `getComputedStyle(el).foo` returns it.
    ///
    /// Named "computed" because that is what the member is called, and the CSSOM's own name for what
    /// it hands back is the *resolved* value — the computed value for most properties and the
    /// **used** value for the two dozen that only layout knows (`layout::resolved`). A page that
    /// reads `width` back as `"50%"` has caught an engine that stopped at the cascade.
    ///
    /// `laid_out` is the caller's already-computed layout, or `None` to compute one here — the same
    /// parameter, for the same reason, that [`Page::computed_values`] takes. It was added in
    /// P8a, when `getComputedStyle` stopped materialising all 475 properties and started coming
    /// through HERE for each property a page actually reads: without it every read of a
    /// resolved-value property (`width`, `height`, the offsets) rebuilt the whole box tree, which is
    /// the ~11 ms-per-call trap `computed_values`' own comment records.
    pub fn computed_value(
        &self,
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        property: PropertyId,
        laid_out: Option<&LaidOut>,
    ) -> Option<String> {
        if !resolved::resolves_from_layout(property) {
            if let Some(value) = self.memoised_value(dom, node, media, property) {
                return Some(value);
            }
        }
        let key = self.computed_style(dom, node, media)?;
        let computed = {
            let state = self.state.lock().unwrap();
            let item = state.style_memo.1.get(&node).is_some_and(|e| e.item);
            Self::resolve_at_read(&state.arena, key, item, property, state.arena.value(key, property)).to_string()
        };
        if !resolved::resolves_from_layout(property) {
            return Some(computed);
        }
        // Only now, and only for the properties that need it: laying the document out to answer
        // `color` would make every style read a layout pass.
        let owned;
        let laid_out = match laid_out {
            Some(l) => l,
            None => {
                owned = self.layout(dom, media);
                &owned
            }
        };
        Some(laid_out.resolved_value(node, property, &computed).unwrap_or(computed))
    }

    /// [`Page::computed_value`] as the cascade computed it, BEFORE layout resolves it: a `transform-origin` as its
    /// percentages and keywords -- what Blink's layout reads (`FloatValueForLength` against the box) -- not the px
    /// `getComputedStyle` answers, which it writes to six significant digits.
    pub fn computed_value_unresolved(&self, dom: &Dom, node: NodeId, media: &MediaContext, property: PropertyId) -> Option<String> {
        let key = self.computed_style(dom, node, media)?;
        let state = self.state.lock().unwrap();
        let item = state.style_memo.1.get(&node).is_some_and(|e| e.item);
        Some(Self::resolve_at_read(&state.arena, key, item, property, state.arena.value(key, property)).to_string())
    }

    /// A property of an element whose style is already banked, under ONE lock -- or
    /// `None`, and the caller takes the full path.
    ///
    /// The full path is [`Page::computed_style`] and then a lock for the value: `sync_document_sheets`,
    /// `layout_fingerprint` (which hashed the cascade fingerprint under one lock and took a second
    /// for the animation revision), the memo lookup, and the value -- four locks per property a page
    /// reads, when the memo already had the answer. This asks the same three questions in the same
    /// order and answers only when all three say the full path would have gone straight to the memo:
    ///
    /// * the `<style>` walk would have returned at once (`synced_at_dom_revision` is current);
    /// * the memo is at the fingerprint `computed_style` would compute
    ///   ([`Page::layout_fingerprint_of`], the one definition); and
    /// * the memo has the element -- which also answers the "is it an element" question, because
    ///   only elements are ever banked.
    ///
    /// Any miss is `None`, so there is no case in which this and the full path disagree.
    fn memoised_value(&self, dom: &Dom, node: NodeId, media: &MediaContext, property: PropertyId) -> Option<String> {
        let state = self.state.lock().unwrap();
        if state.synced_at_dom_revision != Some(dom.revision()) {
            return None;
        }
        if state.style_memo.0 != Self::layout_fingerprint_of(&state, dom, media) {
            return None;
        }
        let entry = state.style_memo.1.get(&node)?;
        let value = state.arena.value(entry.key, property);
        Some(Self::resolve_at_read(&state.arena, entry.key, entry.item, property, value).to_string())
    }

    /// `min-width`, `min-height` and their logical forms as `getComputedStyle` reports them:
    /// their computed `auto` reads `auto` on a flex or grid item and `0px` on any other box --
    /// Blink's `ComputedStyleUtils::MinWidthOrMinHeightAuto`, whose test is the style's
    /// `IsFlexOrGridOrCustomItem` (set by the style adjuster for an in-flow, non-`contents` child of a
    /// box that blockifies its children) and not `IsEnsuredInDisplayNone`. Measured on Chrome 150
    /// (`tools/realweb/seed/minauto`): a flex, grid and `display: contents`-forwarded item `auto`; a
    /// block, an absolutely positioned child of a flex container and a `display: none` one `0px`; a
    /// non-item that INHERITS `auto` from an item `0px` -- which is why the computed value stays `auto`
    /// and only this answer changes.
    ///
    /// And `display`: a `-webkit-box` that clamps lines is a `flow-root` -- an
    /// `inline-block` for `-webkit-inline-box` -- in Chrome 150's `getComputedStyle`, while the cascade
    /// keeps the specified value for layout (`style::compute::legacy_box`).
    fn resolve_at_read<'v>(arena: &StyleArena, key: StyleKey, item: bool, property: PropertyId, value: &'v str) -> std::borrow::Cow<'v, str> {
        use std::borrow::Cow;
        use PropertyId::*;
        if property == Display {
            return Cow::Borrowed(match crate::style::compute::legacy_box(arena, key) {
                crate::style::compute::LegacyBox::Clamp(_) if value.trim() == "-webkit-inline-box" => "inline-block",
                crate::style::compute::LegacyBox::Clamp(_) => "flow-root",
                _ => value,
            });
        }
        // A unitless `line-height` reads back in pixels: the number times the computed font size
        // (Blink's `ValueForLineHeight`). Chrome 150: `line-height: 0.1` at 16px reads `1.6px`.
        if property == LineHeight {
            if let Ok(n) = value.trim().parse::<f32>() {
                let size = arena.value(key, FontSize).trim().trim_end_matches("px").parse::<f32>().unwrap_or(16.0);
                return Cow::Owned(crate::value::serialize_px(n * size));
            }
            return Cow::Borrowed(value);
        }
        if value != "auto" || !matches!(property, MinWidth | MinHeight | MinInlineSize | MinBlockSize) {
            return Cow::Borrowed(value);
        }
        let in_flow = !matches!(arena.value(key, Position), "absolute" | "fixed");
        let boxed = !matches!(arena.value(key, Display), "none" | "contents");
        Cow::Borrowed(if item && in_flow && boxed { value } else { "0px" })
    }

    /// The custom properties (`--foo`) this element's style carries, in the order `getComputedStyle`
    /// enumerates them — after the standard table, by name.
    ///
    /// It is the only part of `getComputedStyle(el).length`, of `item(i)` and of the enumeration
    /// that depends on the element at all: the first `style::property::ENUMERATED.len()` names are a
    /// static table, so those three members can answer without materialising 475 values.
    pub fn computed_custom_names(&self, dom: &Dom, node: NodeId, media: &MediaContext) -> Vec<String> {
        let Some(key) = self.computed_style(dom, node, media) else { return Vec::new() };
        let state = self.state.lock().unwrap();
        state.arena.custom_values(key).map(|(n, _)| n.to_string()).collect()
    }

    /// One computed custom property (`--foo`), as `getPropertyValue("--foo")` returns it.
    pub fn computed_custom(
        &self,
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        name: &str,
    ) -> Option<String> {
        let key = self.computed_style(dom, node, media)?;
        let state = self.state.lock().unwrap();
        state.arena.custom_value(key, name).map(str::to_string)
    }

    /// Every computed property, in the order `getComputedStyle` enumerates them: the standard ones
    /// in table order, then the custom ones by name, which is where Chrome puts them too.
    /// `laid_out` is the caller's already-computed layout, or `None` to compute one here.
    ///
    /// WHY THE CALLER GETS TO SUPPLY IT. `layout()` rebuilds the whole box tree every call --
    /// deliberately, per design D9: v1 keeps no invalidation map and a stale rect is worse than a
    /// recomputed one -- and the runtime's geometry ops pay for that once and then cache, keyed on
    /// `layout_fingerprint`. This function could not reach that cache, so it built its own layout
    /// per call, and `getComputedStyle` measured at ~11 ms EACH on en.wikipedia.org, linear in the
    /// number of calls and the same for every property.
    ///
    /// What made it visible was `innerText`, which asks for the computed `display` of every element
    /// it descends through: 26 SECONDS on a 2,417-element page against 0.05 s for `textContent`
    /// over the same tree. `innerText` is what a scraper reads, and a real-site sweep is not
    /// runnable at that price.
    ///
    /// The layout cannot be cached on `Page` itself: `LaidOut` holds taffy's `CompactLength`, which
    /// is a `*const ()` and therefore not `Send`, while `Page` lives behind the `Arc` the kernel
    /// `Context` shares across threads. That is why the cache belongs to the single-threaded
    /// runtime (`OpState`) and why this parameter exists instead of a field.
    pub fn computed_values(
        &self,
        dom: &Dom,
        node: NodeId,
        media: &MediaContext,
        laid_out: Option<&LaidOut>,
    ) -> Vec<(String, String)> {
        let Some(key) = self.computed_style(dom, node, media) else { return Vec::new() };
        let mut values: Vec<(PropertyId, String)> = {
            let state = self.state.lock().unwrap();
            let item = state.style_memo.1.get(&node).is_some_and(|e| e.item);
            state.arena.enumerated_values(key).map(|(p, v)| (p, Self::resolve_at_read(&state.arena, key, item, p, v).to_string())).collect()
        };
        // One layout pass for the whole enumeration rather than one per property, and -- since
        // `with_layout` memoises on the same fingerprint the geometry ops use -- one for the whole
        // DOCUMENT rather than one per `getComputedStyle`. That second part is the expensive one:
        // this line used to call `layout()` directly, which rebuilds the box tree unconditionally,
        // so reading `.display` off 2,417 elements rebuilt Wikipedia's layout 2,417 times at ~11 ms
        // a call. The lock is released above first; `with_layout` takes it.
        let owned;
        let laid_out = match laid_out {
            Some(l) => l,
            None => {
                owned = self.layout(dom, media);
                &owned
            }
        };
        for (property, value) in &mut values {
            if resolved::resolves_from_layout(*property) {
                if let Some(used) = laid_out.resolved_value(node, *property, value) {
                    *value = used;
                }
            }
        }
        let state = self.state.lock().unwrap();
        values
            .into_iter()
            .map(|(p, v)| (p.name().to_string(), v))
            .chain(state.arena.custom_values(key).map(|(n, v)| (n.to_string(), v.to_string())))
            .collect()
    }

    /// How many distinct computed styles have been interned. The arena's whole claim is that this
    /// stays far below the element count, so it is worth being able to look.
    pub fn distinct_styles(&self) -> (usize, usize) {
        self.state.lock().unwrap().arena.len()
    }

    /// How many elements [`Page::computed_style`] has actually cascaded, as opposed to answered from
    /// `style_memo`.
    ///
    /// It exists for the same reason [`Page::distinct_styles`] does: the claim this lane makes is a
    /// claim about a COUNT, and a count is the only honest way to assert it. `getComputedStyle` used
    /// to resolve all 475 properties whether the caller read one or none; the claim afterwards is
    /// that reading N properties off one element costs ONE cascade for any N, which is this number
    /// staying put while N grows. A test that asserted a millisecond figure instead would be
    /// asserting that the machine was idle.
    pub fn cascades_run(&self) -> u64 {
        self.state.lock().unwrap().cascades_run
    }

    /// How many nodes the last [`Page::declarative_update`] left styled — the exact length of the
    /// walk that every SKIPPED update then repeats, and therefore every `getComputedStyle` call.
    ///
    /// It is here for the same reason [`Page::cascades_run`] is: the claim is about a count. The
    /// claim is that this number follows what the document DECLARES — the document element, plus
    /// the elements an `animation-*` / `transition-*` declaration could reach, plus their ancestors
    /// — and not how many elements the document has. Before P8b the skipped path cloned and scanned
    /// one slot per NODE instead, which is what made `getComputedStyle` O(document) per call.
    pub fn declarative_styled(&self) -> usize {
        self.state.lock().unwrap().last_styled
    }

    /// How many candidate selectors [`Page::declarative_update`]'s first pass has had to consider:
    /// the selectors of the rules that DECLARE an animation or a transition and share
    /// a bucket with the element, never the document's other rules. Counted before the
    /// short-circuit, so it is an upper bound on the selector tests actually run.
    pub fn declarative_selector_tests(&self) -> u64 {
        self.state.lock().unwrap().declarative_selector_tests
    }

    /// How many `(node, style)` pairs [`Page::declarative_update`] has read `animation-*` specs
    /// out of. The claim is that only a RESCAN pays this — a skipped update, which is
    /// what every `getComputedStyle` over an unchanged document runs, reads none.
    pub fn declarative_spec_walks(&self) -> u64 {
        self.state.lock().unwrap().declarative_spec_walks
    }

    /// How many element cascades the whole-document passes ([`Page::layout`] and a rescanning
    /// [`Page::declarative_update`]) have actually run, as opposed to answered from the
    /// [`CascadeMemo`] -- and the chains [`Page::computed_style`]
    /// walks, which go through the same memo. The claim is a count: laying out an unchanged document again,
    /// or one where a single element changed, cascades what changed and not the document.
    pub fn document_cascades_run(&self) -> u64 {
        self.state.lock().unwrap().cascade_memo.runs
    }

    /// How many style passes of [`Page::layout`] styled only what changed rather than the whole
    /// document (see [`incremental`]). A count, for the same reason as the others here.
    pub fn layouts_reusing_matches(&self) -> u64 {
        self.state.lock().unwrap().incremental_passes
    }

    /// How many animation rescans of [`Page::declarative_update`] scanned only what changed rather than
    /// the whole document (see [`Page::rescan_what_changed`]).
    pub fn animation_rescans_from_what_changed(&self) -> u64 {
        self.state.lock().unwrap().rescans_from_what_changed
    }

    /// How many elements [`Page::layout`]'s style passes have matched: a full pass matches
    /// every element, one that styles what changed only the elements a write can reach.
    pub fn elements_matched(&self) -> u64 {
        self.state.lock().unwrap().elements_matched
    }

    /// Style and lay out the whole document, and answer geometry questions about the result.
    ///
    /// Every element is styled here, not just an ancestor chain, because layout is a whole-document
    /// question: a box's position depends on its *siblings*, so there is no shortcut of the kind
    /// `getComputedStyle` gets. The tree is rebuilt per call for the same reason
    /// [`Page::computed_style`] recomputes: v1 keeps no invalidation map (design D9), and a stale
    /// rect is worse than a recomputed one.
    pub fn layout(&self, dom: &Dom, media: &MediaContext) -> LaidOut {
        self.layout_document(dom, dom.root(), media)
    }

    /// The natural sizes of the images that are available and the images that are broken, keyed by
    /// `<img>` element, and their generation (A/B C8). Taken only when the generation
    /// moved, so the per-query call costs a comparison. Call it before [`Page::layout_fingerprint`],
    /// whose number includes the generation.
    pub fn set_natural_images(&self, images: &std::collections::HashMap<NodeId, ImageState>, generation: u64) {
        let mut state = self.state.lock().unwrap();
        if state.natural_images_gen != generation {
            state.natural_images = images.clone();
            state.natural_images_gen = generation;
        }
    }

    /// The document's web fonts -- the `@font-face` faces whose fonts have arrived, which layout measures text in
    /// ([`font::WithWebFonts`]) -- made from what the caller's `key` names (its fonts and sheets). The layout's
    /// generation moves only when the table holds something else ([`font::WebFonts::signature`]); `None` = none.
    pub fn set_web_fonts(&self, fonts: Option<std::sync::Arc<font::WebFonts>>, key: u64) {
        let mut state = self.state.lock().unwrap();
        state.web_fonts_key = key;
        let fonts = fonts.filter(|f| !f.is_empty());
        if fonts.as_ref().map(|f| f.signature()) != state.web_fonts.as_ref().map(|f| f.signature()) {
            state.web_fonts = fonts;
            state.web_fonts_gen += 1;
        }
    }

    /// The `key` [`Page::set_web_fonts`] last took (0 before it ever did): a caller whose inputs still hash to it owes
    /// no new table.
    pub fn web_fonts_key(&self) -> u64 {
        self.state.lock().unwrap().web_fonts_key
    }

    /// [`Page::layout`] for the document whose arena root is `root`, into `media`'s viewport.
    ///
    /// One arena holds the page's document and every nested browsing context's. A
    /// frame's document is laid out exactly as the page's is -- the same cascade, the same box tree --
    /// from its own root and into the frame's content box, which the caller passes as the viewport.
    /// Measured on Chrome 150.0.7871.46: an element inside `<iframe srcdoc>` reports `[10, 10, 50,
    /// 50]` in the frame's own coordinates, where this engine used to report four zeros because
    /// nothing below a frame's document root was ever laid out.
    ///
    /// Only the rules of the stylesheets IN THAT DOCUMENT apply, with the user-agent and constructed
    /// sheets, which have no owner: see [`sheet_scope`].
    pub fn layout_document(&self, dom: &Dom, root: NodeId, media: &MediaContext) -> LaidOut {
        let out = match self.layout_document_counted(dom, root, media, Prev::Nothing) {
            Laid::New(out) | Laid::Kept(out) => out,
            // nothing to keep: a new layout
            Laid::KeptBorrowed => unreachable!("a layout from nothing keeps nothing"),
        };
        perf_counters::add(&perf_counters::LAYOUT_DOCUMENTS, 1);
        perf_counters::flush();
        out
    }

    /// [`Page::layout_document`] for a caller that holds the document's last layout, `previous`: `None`
    /// when `previous` IS the layout of the document as it is now -- its box tree would be built
    /// exactly as it was, into the same viewport, so laying it out again would repeat it.
    /// The styles are brought up to date either way.
    ///
    /// Measured 2026-10-02 (diag counters): www.hubspot.com builds the same tree for 399 of its 424
    /// layouts, atlassian 24 of 47, github 9 of 14 -- reads that moved the document's revision but
    /// nothing a box is made from.
    pub fn relayout_document(&self, dom: &Dom, root: NodeId, media: &MediaContext, previous: &LaidOut) -> Option<LaidOut> {
        let out = match self.layout_document_counted(dom, root, media, Prev::Borrowed(&previous.tree)) {
            Laid::KeptBorrowed => None,
            Laid::New(out) | Laid::Kept(out) => Some(out),
        };
        perf_counters::add(&perf_counters::LAYOUT_DOCUMENTS, 1);
        if out.is_none() {
            perf_counters::add(&perf_counters::LAYOUTS_KEPT, 1);
        }
        perf_counters::flush();
        out
    }

    /// [`Page::relayout_document`] for a caller that hands its last layout over and stores whatever comes back
    /// ([`Relaid::into_inner`]): what is carried is MOVED out of `previous` rather than copied, and the copy's drop is
    /// gone too. `Kept` hands `previous` back untouched; `New` is a new layout, and `previous` is gone.
    pub fn relayout_document_owned(&self, dom: &Dom, root: NodeId, media: &MediaContext, previous: LaidOut) -> Relaid {
        let LaidOut { tree, scroll } = previous;
        let out = match self.layout_document_counted(dom, root, media, Prev::Caller(tree, scroll)) {
            Laid::Kept(out) => {
                perf_counters::add(&perf_counters::LAYOUTS_KEPT, 1);
                Relaid::Kept(out)
            }
            Laid::New(out) => Relaid::New(out),
            Laid::KeptBorrowed => unreachable!("nothing was lent"),
        };
        perf_counters::add(&perf_counters::LAYOUT_DOCUMENTS, 1);
        perf_counters::flush();
        out
    }

    fn layout_document_counted(&self, dom: &Dom, root: NodeId, media: &MediaContext, mut previous: Prev<'_>) -> Laid {
        self.sync_document_sheets(dom);
        self.state.lock().unwrap().layouts_run += 1;
        // CONTAINER QUERIES. A rule under `@container` is answered by an ancestor's
        // laid-out size, and applying it can move that size -- so, as Blink interleaves style and
        // layout for a container's subtree, style with the sizes the last layout produced, lay out,
        // and go again while the sizes moved. Bounded by `CONTAINER_PASSES`: a query that flips its
        // own container's size keeps the last settled answer rather than looping.
        let mut passes = 0usize;
        loop {
            // One pass over a tree that `&Dom` keeps still: `:nth-*` and `:has()` share their caches. Each pass
            // compares against, and carries from, the LATEST complete tree: the caller's, or an earlier pass's.
            let pass = korvus_dom::with_pass_caches(|| self.layout_pass(dom, root, media, Reuse::Allowed, &mut previous));
            if verify::enabled() {
                let full = korvus_dom::with_pass_caches(|| self.layout_pass(dom, root, media, Reuse::Nothing, &mut Prev::Nothing));
                // A kept layout is checked as itself: the previous tree, against one laid out now.
                let laid = match (pass.kept, previous.tree()) {
                    (true, Some(p)) => p,
                    _ => &pass.tree,
                };
                Self::verify_pass(dom, &pass, laid, &full, self.state.lock().unwrap().layouts_run);
            }
            let PassOut { tree, sizes, kept, .. } = pass;
            if !kept {
                // what this pass carried from is replaced (and dropped) here, gutted or not
                previous = Prev::Pass(tree);
            }
            let mut state = self.state.lock().unwrap();
            // The container-query sizes are the PAGE's (their basis is the page's fingerprint), so a
            // frame's layout neither takes a second round of them nor overwrites them.
            if !state.has_container_rules || root != dom.root() {
                drop(state);
                return self.laid(dom, root, media, previous);
            }
            let basis = Self::container_basis_of(&state, dom, media);
            if sizes == state.container_sizes || passes >= CONTAINER_PASSES {
                state.container_basis = Some(basis);
                drop(state);
                return self.laid(dom, root, media, previous);
            }
            state.container_sizes = sizes;
            state.container_generation = state.container_generation.wrapping_add(1);
            state.container_basis = Some(basis);
            state.container_passes += 1;
            passes += 1;
        }
    }

    /// What a layout made of what it laid the document out from, once no pass is owed: a lent tree stands, the
    /// caller's own stands (nothing was moved from it: a pass that carries from a tree replaces it), a pass's is new.
    fn laid(&self, dom: &Dom, root: NodeId, media: &MediaContext, previous: Prev<'_>) -> Laid {
        let gutted = previous.tree().is_some_and(BoxTree::is_gutted);
        debug_assert!(!gutted, "a layout whose states were moved out");
        if gutted {
            // Unreachable by construction; if it ever is, lay the document out from nothing rather than answer zeros.
            perf_counters::add(&perf_counters::LAYOUT_GUTTED_FALLBACK, 1);
            return self.layout_document_counted(dom, root, media, Prev::Nothing);
        }
        match previous {
            Prev::Nothing => unreachable!("a pass that is kept had a tree to keep"),
            Prev::Borrowed(_) => Laid::KeptBorrowed,
            Prev::Caller(tree, scroll) => Laid::Kept(LaidOut { tree, scroll }),
            Prev::Pass(tree) => Laid::New(LaidOut { tree, scroll: ScrollSnapshot::default() }),
        }
    }

    /// Compare a pass that reused work with one that reused none ([`verify`]): every
    /// element's style, its generated content, the container sizes, and every box.
    fn verify_pass(dom: &Dom, pass: &PassOut, laid: &BoxTree, full: &PassOut, layout: u64) {
        perf_counters::add(&perf_counters::VERIFY_PASSES, 1);
        verify::started();
        if let Some(d) = Self::pass_difference(dom, pass, laid, full) {
            verify::mismatch(&format!("layout {layout} of {}: {d}", verify::document_title(dom)));
        }
    }

    /// The first difference between two passes over one document, or `None`.
    fn pass_difference(dom: &Dom, pass: &PassOut, laid: &BoxTree, full: &PassOut) -> Option<String> {
        let at = |n: usize| verify::describe(dom, n as NodeId);
        // A node past the end of either is unstyled in it.
        let key = |p: &PassOut, n: usize| p.keys.get(n).copied().flatten();
        if let Some(n) = (0..pass.keys.len().max(full.keys.len())).find(|&n| key(pass, n) != key(full, n)) {
            return Some(format!(
                "style of {}: {:?}, full pass {:?}; {}",
                at(n),
                key(pass, n),
                key(full, n),
                verify::history(dom, n as NodeId)
            ));
        }
        let generated = |g: &std::collections::HashMap<NodeId, Generated>| {
            let mut v: Vec<_> = g.iter().map(|(n, g)| (*n, format!("{g:?}"))).collect();
            v.sort();
            v
        };
        let (mine, theirs) = (generated(&pass.generated), generated(&full.generated));
        if mine != theirs {
            return Some(mine.iter().zip(theirs.iter()).find(|(a, b)| a != b).map_or_else(
                || format!("{} elements with generated content, full pass {}", mine.len(), theirs.len()),
                |(a, b)| format!("generated content of {}: {}, full pass {} {}", at(a.0 as usize), a.1, at(b.0 as usize), b.1),
            ));
        }
        if pass.sizes != full.sizes {
            return Some(format!("container sizes {:?}, full pass {:?}", pass.sizes, full.sizes));
        }
        laid.first_difference(&full.tree).map(|d| d.describe(|n| verify::describe(dom, n)))
    }

    /// One style-and-layout pass: every element styled (with its generated content), the box tree
    /// built and laid out, and the content-box size of each query container.
    ///
    /// [`Reuse::Nothing`] is the pass [`verify`] checks the others against: it neither reads nor
    /// writes anything a previous pass left -- the matched rules -- and so is a function of the
    /// document, the sheets and the viewport alone. The cascade memo is the exception, because it is
    /// one: its key is the cascade's whole input.
    fn layout_pass(&self, dom: &Dom, root: NodeId, media: &MediaContext, reuse_mode: Reuse, source: &mut Prev<'_>) -> PassOut {
        let previous = source.tree();
        perf_counters::add(&perf_counters::LAYOUT_PASSES, 1);
        let mut state = self.state.lock().unwrap();
        Self::ensure_matcher(&mut state);
        let state = &mut *state;
        state.cascade_memo.begin(state.sheet_revision, media);
        let owned;
        let generated;
        let keys: &[Option<StyleKey>] = match reuse_mode {
            Reuse::Allowed => {
                generated = Self::style_incremental(state, dom, root, media).unwrap_or_else(|| Self::generated_content(state, dom, root));
                &state.doc_styles.get(&root).expect("just styled").keys
            }
            Reuse::Nothing => {
                let (k, g) = Self::style_from_nothing(state, dom, root, media, false);
                (owned, generated) = (k, g);
                &owned
            }
        };
        // The style rows of earlier trees under this theme (`layout::style::DerivedRow`) -- none for the reference
        // pass `verify` compares with, which derives every row again.
        let origins = match &mut state.layout_origins {
            (theme, map) if *theme == media.theme && reuse_mode == Reuse::Allowed => std::mem::take(map),
            _ => Default::default(),
        };
        let viewport = taffy::geometry::Size { width: media.viewport_width, height: media.viewport_height };
        // The fingerprint of what the tree would be BUILT from. If it equals the previous
        // tree's and the viewport is unchanged, `build_with_origins` would produce a `same_build`-equal
        // tree that lays out exactly as the previous did -- so the build AND the layout are skipped and
        // the previous tree is kept. Reuse::Nothing (the verify reference) never reuses.
        let build_fp = (reuse_mode == Reuse::Allowed).then(|| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            state.doc_styles.get(&root).map_or(0, |d| d.keys_version).hash(&mut h);
            dom.match_revision().hash(&mut h);
            state.resources_gen().hash(&mut h);
            root.hash(&mut h);
            dom.instance().hash(&mut h);
            let t = &media.theme;
            t.font_map.hash(&mut h);
            for v in [t.scrollbar_thickness, t.scrollbar_thin_thickness, t.menu_list_inline_padding, t.menu_list_block_padding] {
                v.to_bits().hash(&mut h);
            }
            h.finish()
        });
        let reuse = matches!((build_fp, previous), (Some(fp), Some(p))
            if p.build_fp() == Some(fp) && p.requested_viewport() == Some(viewport) && *p.generated_map() == generated);
        // Diag only: how much of the tree a layout could carry over. Set in the build branch.
        let mut measured: Option<(u64, korvus_dom::FxHashMap<NodeId, u64>)> = None;
        // A change that moved only styles patches the previous tree in place (`BoxTree::patch_styles`).
        let mut origins = origins;
        let patched = match (reuse, reuse_mode, build_fp) {
            (false, Reuse::Allowed, Some(fp)) => {
                let image = (state.resources_gen(), &state.natural_images);
                Self::patch_pass(dom, root, media, keys, &generated, fp, viewport, source, &state.arena, image, state.web_fonts.as_ref(), &mut origins)
            }
            _ => None,
        };
        let previous = source.tree();
        let (tree, kept) = if reuse {
            // Keep the previous layout outright: no build, no compute. Put back the origins we took so the
            // next real build still has them.
            state.layout_origins = (media.theme, origins);
            perf_counters::add(&perf_counters::LAYOUTS_KEPT, 1);
            (BoxTree::default(), true)
        } else if let Some((tree, kept)) = patched {
            state.layout_origins = (media.theme, origins);
            state.trees_patched += 1;
            if kept {
                perf_counters::add(&perf_counters::LAYOUTS_KEPT, 1);
            } else {
                state.boxes_carried += tree.len() as u64;
                state.box_layouts_computed += tree.stats().computed;
            }
            (tree, kept)
        } else {
            let arena = &state.arena;
            let mut tree = BoxTree::build_with_origins(dom, root, arena, &|n| keys.get(n as usize).copied().flatten(), media.theme, &generated, &state.natural_images, state.web_fonts.as_ref(), origins);
            tree.set_viewport_bars_off(media.scrollbars_off);
            let origins = tree.take_key_rows();
            state.trees_built += (reuse_mode == Reuse::Allowed) as u64;
            if let Some(fp) = build_fp {
                tree.set_build_fp(fp);
                if crate::layout::tree::knob(crate::layout::tree::Knob::Patch) {
                    let basis = crate::layout::tree::PatchBasis {
                        structure: dom.structure_revision(),
                        seq: dom.journal().seq(),
                        instance: dom.instance(),
                        root,
                        images: state.resources_gen(),
                    };
                    tree.set_patch_basis(basis, keys.to_vec());
                }
            }
            perf_counters::add(&perf_counters::BOXES_BUILT, tree.len() as u64);
            measured = (cfg!(feature = "perf-counters") && reuse_mode == Reuse::Allowed).then(|| {
                let hashes = tree.subtree_hashes();
                let by_node: korvus_dom::FxHashMap<NodeId, u64> =
                    (0..tree.len() as u32).filter_map(|i| tree.element_of_box(i).map(|n| (n, hashes[i as usize]))).collect();
                (hashes.first().copied().unwrap_or(0), by_node)
            });
            // A tree built exactly as the previous layout's was, into the same viewport, lays
            // out exactly as it did -- `compute` reads nothing else -- so that layout is kept.
            let kept = reuse_mode == Reuse::Allowed && previous.is_some_and(|p| tree.same_build(p, viewport));
            if !kept {
                // The reference lays every pass out from empty caches.
                match reuse_mode {
                    Reuse::Allowed => {
                        // What was built as before is laid out as before: moved out of a tree this layout replaces,
                        // copied out of one it was lent.
                        if previous.is_some_and(|p| p.requested_viewport() == Some(viewport)) {
                            let carried = match source {
                                Prev::Nothing => 0,
                                Prev::Borrowed(p) => tree.carry_from(p),
                                Prev::Caller(p, _) | Prev::Pass(p) => tree.carry_from_moving(p),
                            };
                            perf_counters::add(&perf_counters::BOXES_CARRIED, carried as u64);
                            state.boxes_carried += carried as u64;
                        }
                        tree.set_viewport_bars_off(media.scrollbars_off);
                        tree.compute(viewport);
                        state.box_layouts_computed += tree.stats().computed;
                    }
                    Reuse::Nothing => {
                        tree.set_viewport_bars_off(media.scrollbars_off);
                        tree.compute_from_nothing(viewport)
                    }
                }
            }
            state.layout_origins = (media.theme, origins);
            (tree, kept)
        };
        let laid = match (kept, source.tree()) {
            (true, Some(p)) => p,
            _ => &tree,
        };
        let arena = &state.arena;
        let mut sizes = std::collections::HashMap::new();
        // Only a page with `@container` rules reads the sizes (`layout_document_counted`, and verify's comparison, which
        // sees the same flag): without them this was a string read and trim per element per layout for nothing.
        let containers = if state.has_container_rules { keys } else { &[] };
        for (n, key) in containers.iter().enumerate() {
            let Some(key) = key else { continue };
            let ty = arena.value(*key, PropertyId::ContainerType).trim();
            if matches!(ty, "size" | "inline-size") {
                if let Some((w, h)) = laid.content_size(n as NodeId) {
                    // Only the axes the type makes queryable: an `inline-size` container whose height
                    // moves has not changed any answer, and must not cost another pass.
                    sizes.insert(n as NodeId, (w, if ty == "size" { h } else { 0.0 }));
                }
            }
        }
        let keys = if reuse_mode == Reuse::Nothing || verify::enabled() || cfg!(test) { keys.to_vec() } else { Vec::new() };
        if let (Some((root_hash, by_node)), Some(doc)) = (measured, state.doc_styles.get_mut(&root)) {
            if let Some((last_root, last)) = &doc.last_hashes {
                if *last_root == root_hash {
                    perf_counters::add(&perf_counters::LAYOUT_TREES_IDENTICAL, 1);
                }
                perf_counters::add(&perf_counters::LAYOUT_ELEMENT_BOXES, by_node.len() as u64);
                let clean = by_node.iter().filter(|(n, h)| last.get(n) == Some(h)).count();
                perf_counters::add(&perf_counters::LAYOUT_ELEMENT_BOXES_CLEAN, clean as u64);
            }
            doc.last_hashes = Some((root_hash, by_node));
        }
        PassOut { tree, sizes, keys, generated, kept }
    }

    /// Patch the previous tree, when the layout owns it, in place ([`BoxTree::patch_styles`]): `Some((tree, kept))`
    /// as the build would have it -- the patched tree laid out again, or `kept` (no box's layout row moved: the patched
    /// tree is the layout, and stays where it is); `None` to build as before. The guard is everything a build reads
    /// besides the elements' styles: the DOM's structure (no node made, moved or retexted since -- its structure
    /// revision) and the attributes the builder reads (the journal since the build holds only writes of `class`,
    /// `id`, `style`, `data-*`, `aria-*`, `role` and `tabindex`), the root, the arena, the theme, the viewport, the
    /// generated content and the images; and no quirks mode or shadow tree, whose rules the patch does not follow.
    #[allow(clippy::too_many_arguments)]
    fn patch_pass(
        dom: &Dom,
        root: NodeId,
        media: &MediaContext,
        keys: &[Option<StyleKey>],
        generated: &std::collections::HashMap<NodeId, Generated>,
        fp: u64,
        viewport: taffy::geometry::Size<f32>,
        source: &mut Prev<'_>,
        arena: &StyleArena,
        (images_gen, images): (u64, &std::collections::HashMap<NodeId, ImageState>),
        web_fonts: Option<&std::sync::Arc<font::WebFonts>>,
        origins: &mut crate::layout::style::KeyRows,
    ) -> Option<(BoxTree, bool)> {
        use crate::layout::tree::{knob, Knob, PatchBasis};
        if !knob(Knob::Patch) {
            return None;
        }
        let tree = match source {
            Prev::Caller(t, _) | Prev::Pass(t) => t,
            _ => return None,
        };
        let basis = tree.patch_basis()?;
        let journal = dom.journal();
        let now = PatchBasis { structure: dom.structure_revision(), seq: basis.seq, instance: dom.instance(), root, images: images_gen };
        if now != basis
            || tree.is_gutted()
            || tree.requested_viewport() != Some(viewport)
            || tree.theme() != media.theme
            || *tree.generated_map() != *generated
            || dom.document_mode(root) == korvus_dom::DocumentMode::Quirks
            || dom.has_shadow_roots()
        {
            return None;
        }
        let styles_only = |name: &str| {
            matches!(name, "class" | "id" | "style" | "role" | "tabindex") || name.starts_with("data-") || name.starts_with("aria-")
        };
        let entries = journal.since(basis.seq)?;
        if !entries.iter().all(|c| matches!(c, korvus_dom::journal::Change::Attribute { name, .. } if styles_only(journal.name(*name)))) {
            return None;
        }
        // The page's style rows, so a style the tree has not seen is interned without deriving its row again.
        tree.set_key_rows(std::mem::take(origins));
        let patched = tree.patch_styles(dom, arena, keys);
        *origins = tree.take_key_rows();
        let patched = patched?;
        tree.set_build_fp(fp);
        tree.set_patch_basis(PatchBasis { seq: journal.seq(), ..now }, keys.to_vec());
        // The patched tree is the tree a build makes, box for box: checked against one in tests and under
        // `KORVUS_VERIFY_INCREMENTAL`.
        if cfg!(test) || verify::enabled() {
            let full = BoxTree::build_with_origins(dom, root, arena, &|n| keys.get(n as usize).copied().flatten(), media.theme, generated, images, web_fonts, Default::default());
            if !full.same_build(tree, viewport) {
                verify::mismatch(&format!("a box tree patched in place is not the tree a build makes: {}", verify::document_title(dom)));
                if cfg!(test) {
                    panic!("a box tree patched in place is not the tree a build makes");
                }
            }
        }
        if !patched.moved && patched.simple {
            return Some((BoxTree::default(), true));
        }
        let (Prev::Caller(mut tree, _) | Prev::Pass(mut tree)) = std::mem::replace(source, Prev::Nothing) else { unreachable!("matched above") };
        tree.prepare_recompute();
        tree.set_viewport_bars_off(media.scrollbars_off);
        tree.compute(viewport);
        Some((tree, false))
    }

    /// What a document's kept styles were computed for (`DocStyle::basis`): the sheet revision, the
    /// media, the container-size generation, the root, and the arena.
    fn style_basis(state: &PageState, dom: &Dom, root: NodeId, media: &MediaContext) -> (u64, MediaContext, u64, NodeId, u64) {
        (state.sheet_revision, *media, state.container_generation, root, dom.instance())
    }

    /// What the sheets as a whole are now ([`incremental::SheetFacts`]). The rule index is built.
    fn sheet_facts(state: &PageState) -> incremental::SheetFacts {
        incremental::SheetFacts {
            layer_ranks: state.matcher.as_ref().expect("built by the caller").layer_ranks().to_vec(),
            registry: state.arena.registry.clone(),
            uses_cq_units: state.uses_cq_units,
            has_container_rules: state.has_container_rules,
        }
    }

    /// The sheet change since `root`'s kept styles were made, when the pass bringing them up to `basis`
    /// can style what the changed sheets' rules reach and nothing else: only the sheet
    /// revision moved, every change since is one the log names to a sheet that styles every tree, and
    /// the sheets as a whole -- layer order, `@property`, container queries -- are as they were. `None`
    /// where the pass starts over, as every pass after a sheet change did before.
    ///
    /// A stylesheet that arrives restyled every element before: www.jlcpcb.com's scan for the loads its
    /// sheets induce read each element's style, and both pseudo-elements', through ancestor chains
    /// after each of its sheets arrived (473 K chain answers, 315 K pseudo-element matchings). Its
    /// sheets are a component's each, a few classes: the 18 changes now restyle 599 elements, and the
    /// load fell from 36.7 G instructions to 32.4 G (2026-10-03, median of 5).
    fn sheet_change(
        state: &mut PageState,
        dom: &Dom,
        root: NodeId,
        basis: &(u64, MediaContext, u64, NodeId, u64),
    ) -> Option<std::sync::Arc<incremental::SheetChange>> {
        let was = state.doc_styles.get(&root)?.basis?;
        if was.0 >= basis.0 || (was.1, was.2, was.3, was.4) != (basis.1, basis.2, basis.3, basis.4) {
            return None;
        }
        let key = (root, was.0, basis.0);
        if let Some((k, change)) = &state.sheet_change {
            if *k == key {
                return change.clone();
            }
        }
        let change = Self::plan_sheet_change(state, dom, root, was.0);
        state.sheet_change = Some((key, change.clone()));
        change
    }

    /// The sheets changed since revision `from`, when every change since is one the log names to a
    /// sheet (`PageState::sheet_log`); `None` when one is not, or the log does not reach back that far.
    fn sheets_changed_since(log: &std::collections::VecDeque<(u64, Option<u32>)>, from: u64) -> Option<Vec<u32>> {
        // The log runs without a gap from the revision after `from`.
        if log.front().is_none_or(|&(r, _)| r > from + 1) {
            return None;
        }
        let mut changed: Vec<u32> = Vec::new();
        for &(r, sheet) in log {
            if r <= from {
                continue;
            }
            let i = sheet?;
            if !changed.contains(&i) {
                changed.push(i);
            }
        }
        Some(changed)
    }

    /// What every sheet's selectors read ([`incremental::Features`]), for the current sheet revision.
    /// After a change the log names to sheets, the changed sheets' features are added to the last ones
    /// rather than every sheet's selectors walked again (on github.com, whose sheets are
    /// large, the walk after each sheet change was 5 % of the load): what a removed rule leaves behind
    /// only makes a change mark more.
    fn ensure_features(state: &mut PageState) {
        let PageState { features, sheets, sheet_log, sheet_revision, .. } = state;
        match features {
            Some((r, _)) if *r == *sheet_revision => return,
            Some((r, f)) => {
                if let Some(changed) = Self::sheets_changed_since(sheet_log, *r) {
                    for i in changed {
                        f.add_sheet(&sheets[i as usize]);
                    }
                    *r = *sheet_revision;
                    return;
                }
            }
            None => {}
        }
        *features = Some((*sheet_revision, incremental::Features::of(sheets)));
    }

    fn plan_sheet_change(state: &mut PageState, dom: &Dom, root: NodeId, from: u64) -> Option<std::sync::Arc<incremental::SheetChange>> {
        let tree = dom.root_of(root);
        let mut changed = Self::sheets_changed_since(&state.sheet_log, from)?;
        // A sheet of this document's tree, or of every tree; one of another document's styles nothing
        // here, and one of a shadow tree or adopted reaches past the walk's own reach.
        for &i in &changed {
            match state.scopes.get(i as usize) {
                Some(SheetScope::Everywhere) => {}
                Some(SheetScope::Tree(t)) if *t == tree => {}
                Some(SheetScope::Tree(t)) if !dom.is_shadow_root(*t) => {}
                _ => return None,
            }
        }
        changed.retain(|&i| !matches!(state.scopes.get(i as usize), Some(SheetScope::Tree(t)) if *t != tree));
        Self::ensure_matcher(state);
        let now = Self::sheet_facts(state);
        let kept = state.doc_styles.get(&root)?.sheets.as_ref()?;
        if (&kept.registry, kept.uses_cq_units, kept.has_container_rules) != (&now.registry, now.uses_cq_units, now.has_container_rules) {
            return None;
        }
        // A kept match carries its layer's rank, so every sheet that did not change ranks its layers
        // as it did (a layer one of them declares moving anything else's rank moves its too).
        if kept.layer_ranks.iter().enumerate().any(|(i, r)| !changed.contains(&(i as u32)) && now.layer_ranks.get(i) != Some(r)) {
            return None;
        }
        incremental::SheetChange::of(&state.sheets, changed).map(std::sync::Arc::new)
    }

    /// Style the document the walk from `root` reaches, styling again only what changed since the last
    /// pass over it (see [`incremental`]). The styles stay in `state.doc_styles[root]`. Returns
    /// the generated content when the pass made it (one that styled from nothing for container queries);
    /// otherwise [`Page::generated_content`] makes it from the kept pseudo-element styles, for a caller
    /// that lays out.
    ///
    /// The walk is [`Page::style_from_nothing`]'s, with three differences, each checked against it by
    /// [`verify`]: an element is matched only when a mark says its rules may have changed (and
    /// cascaded when that, or its parent's style, or its animated values changed); the walk goes into a
    /// child only while something there can have changed; and an element's pseudo-element styles are
    /// computed with it and kept, so the generated-content walk only lays out counters and quotes.
    fn style_incremental(
        state: &mut PageState,
        dom: &Dom,
        root: NodeId,
        media: &MediaContext,
    ) -> Option<std::collections::HashMap<NodeId, Generated>> {
        use incremental::{Fallback, BELOW, RECASCADE, RESTYLE, SUBTREE};
        if !state.doc_styles.contains_key(&root) && state.doc_styles.len() >= DOC_STYLES {
            let other = *state.doc_styles.keys().find(|&&r| r != dom.root()).unwrap_or(&root);
            state.doc_styles.remove(&other);
        }
        let basis = Self::style_basis(state, dom, root, media);
        // Nothing written since the last pass, under the same basis, and no animated value moved: the
        // styles are what a pass would make (the computed-style reads ask this between writes).
        if state.doc_styles.get(&root).is_some_and(|d| {
            d.basis == Some(basis) && d.seq == dom.journal().seq() && d.animated.is_empty() && d.keys.len() == dom.len()
        }) {
            // A pass that styled only what changed -- nothing.
            perf_counters::add(&perf_counters::STYLE_PASSES_INCREMENTAL, 1);
            state.incremental_passes += 1;
            return None;
        }
        // A sheet change this pass can style from the changed sheets alone, and, for styles
        // made under a new basis, what the sheets as a whole are now.
        let moved = state.doc_styles.get(&root).is_none_or(|d| d.basis != Some(basis));
        let sheet_change = if moved { Self::sheet_change(state, dom, root, &basis) } else { None };
        let facts = moved.then(|| Self::sheet_facts(state));
        // What the sheets' selectors read is worked out only for a pass that plans: a page
        // whose sheets change before every layout styles from nothing each time and never needs it.
        if !moved || sheet_change.is_some() {
            Self::ensure_features(state);
        }
        let sheet_revision = state.sheet_revision;
        // Styles that follow laid-out sizes (after I3): a `cq*` length resolves against the
        // element's nearest size containers, and an `@container` rule asks its containers' sizes and
        // styles. The sizes are in the basis (a pass after they move starts over); what is left is a
        // container's own style moving, below.
        let uses_cq_units = state.uses_cq_units;
        let container_queries = state.has_container_rules && state.sheets.iter().any(|s| !s.disabled && s.has_container_rules());
        // What the conditions read of a container, made once per sheet revision.
        if container_queries && state.container_reads.as_ref().is_none_or(|(rev, _)| *rev != state.sheet_revision) {
            let reads = ContainerReads::of(state.sheets.iter().filter(|s| !s.disabled).flat_map(|s| s.container_conditions()));
            state.container_reads = Some((state.sheet_revision, reads));
        }
        let container_reads = if container_queries { state.container_reads.as_ref().map(|(_, r)| r.clone()) } else { None };
        let PageState {
            matcher, sheets, cascade_memo, arena, declarative, container_sizes, owners, features, doc_styles, incremental_passes, elements_matched, sheet_passes, ..
        } = state;
        let matcher = matcher.as_ref().expect("built by the caller");
        let doc = doc_styles.entry(root).or_default();
        doc.grow(dom);
        doc.tracks_restyled = root == dom.root();
        let animated = std::mem::take(&mut doc.animated);
        let fallback = if doc.basis != Some(basis) && sheet_change.is_none() {
            Some(Fallback::Basis)
        } else {
            if let Some(change) = &sheet_change {
                perf_counters::add(&perf_counters::SHEET_CHANGES_PLANNED, 1);
                *sheet_passes += 1;
                incremental::plan_sheets(doc, dom, root, change);
                doc.basis = Some(basis);
                doc.sheets = facts.clone();
            }
            let features = &features.as_ref().expect("built above").1;
            if features.scope {
                Some(Fallback::Scope)
            } else {
                incremental::plan(doc, dom, root, features, &animated).err()
            }
        };
        let form_asked = features.as_ref().filter(|(r, _)| *r == sheet_revision).map(|(_, f)| f.form_asked());
        // The FLAT tree, as in `style_from_nothing`.
        let mut flat = crate::flat::FlatTree::default();
        // The document element: `rem` is its font size.
        let root_element = {
            let mut stack = vec![root];
            let mut found = None;
            while let Some(n) = stack.pop() {
                if matches!(dom.node(n).kind, NodeKind::Element { .. }) {
                    found = Some(n);
                    break;
                }
                for c in flat.children(dom, n).into_iter().rev() {
                    stack.push(c);
                }
            }
            found
        };
        let mut full = fallback.is_some();
        if let Some(why) = fallback {
            incremental::count_fallback(why);
        }
        let walk_tree = dom.root_of(root);
        let mut matched = Vec::new();
        let mut pseudo_matched = Vec::new();
        let styling = PseudoStyling { matcher, sheets: &*sheets, container_sizes: &*container_sizes, owners: &*owners, dom, media };
        // Set by an element whose change reaches what is below it through its containers; read and
        // cleared where its children are pushed.
        let mut below_containers = false;
        loop {
            if full {
                doc.reset(dom);
                doc.basis = Some(basis);
                if facts.is_some() {
                    doc.sheets = facts.clone();
                }
            } else {
                // Each marked node's flat ancestors lead the walk to it.
                let marked: Vec<NodeId> = doc.marked.clone();
                for n in marked {
                    let mut cur = flat.parent(dom, n);
                    while let Some(p) = cur {
                        if doc.marks[p as usize] & BELOW != 0 {
                            break;
                        }
                        doc.mark_below(p);
                        if p == root {
                            break;
                        }
                        cur = flat.parent(dom, p);
                    }
                }
                doc.mark_below(root);
            }
            doc.seq = dom.journal().seq();
            let mut ctx = ComputeContext {
                viewport_width: media.viewport_width,
                viewport_height: media.viewport_height,
                root_font_size: if full { 16.0 } else { doc.root_font_size },
                min_logical_font_size: ComputeContext::min_logical_of(&media.theme),
                ..ComputeContext::default()
            };
            let mut ancestors = AncestorBloom::for_document_of(dom, root);
            // (node, leaving, its parent's style changed, everything below is to be styled)
            let mut stack: Vec<(NodeId, bool, bool, bool)> = vec![(root, false, false, full)];
            let mut root_font_moved = false;
            let flags = std::mem::take(&mut doc.flags);
            let (_, flags) = korvus_dom::invalidation::record_selector_flags(flags, || {
                while let Some((node, leaving, parent_changed, subtree)) = stack.pop() {
                    if leaving {
                        ancestors.pop(dom, node);
                        continue;
                    }
                    let mark = doc.marks.get(node as usize).copied().unwrap_or(0);
                    if subtree && !full && mark & (SUBTREE | RESTYLE) == 0 {
                        perf_counters::add(&perf_counters::ELEMENTS_RESTYLED_BY_SUBTREE, 1);
                    }
                    let subtree = subtree || mark & SUBTREE != 0;
                    let element = matches!(dom.node(node).kind, NodeKind::Element { .. });
                    let mut changed = false;
                    if element {
                        let rematch = subtree || mark & RESTYLE != 0 || doc.keys[node as usize].is_none();
                        if rematch || parent_changed || mark & RECASCADE != 0 {
                            let flat_parent = flat.parent(dom, node);
                            let parent = flat_parent.and_then(|p| doc.keys[p as usize]);
                            {
                                let keys = &doc.keys;
                                let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                                let (arena_ref, sizes) = (&*arena, &*container_sizes);
                                let root_font_size = ctx.root_font_size;
                                let containers = |at: NodeId, cond: &ContainerCondition| {
                                    container_holds(arena_ref, sizes, &style_of, dom, at, cond, media, root_font_size)
                                };
                                let owner_parent = |sheet: u32| owner_parent_of(owners, dom, sheet);
                                let env = MatchEnv { containers: Some(&containers), owner_parent: Some(&owner_parent), ancestors: Some(&ancestors), tree: Some(walk_tree) };
                                if rematch {
                                    perf_counters::add(&perf_counters::ELEMENTS_MATCHED, 1);
                                    *elements_matched += 1;
                                    matcher.match_element_in(sheets, dom, node, media, &env, &mut matched);
                                    doc.spans[node as usize] = (doc.rules.len() as u32, matched.len() as u32);
                                    doc.rules.extend_from_slice(&matched);
                                } else {
                                    let (start, len) = doc.spans[node as usize];
                                    matched.clear();
                                    matched.extend_from_slice(&doc.rules[start as usize..(start + len) as usize]);
                                }
                            }
                            let attr = dom.style_for_cascade(node).unwrap_or("");
                            let mine = flat_parent.map_or(ParentFormatting::Flow, |p| doc.formatting[p as usize]);
                            if uses_cq_units {
                                // As `style_from_nothing` does: against the nearest size containers.
                                let keys = &doc.keys;
                                let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                                ctx.container = Some(cq_basis(arena, container_sizes, &style_of, dom, node, media));
                            }
                            perf_counters::add(&perf_counters::ELEMENTS_STYLED, 1);
                            let key = cascade_memoised(
                                cascade_memo, sheets, &matched, attr, &crate::style::hints::for_element(dom, node), parent, mine, &ctx, arena,
                                declarative.animated_for(node), ElementFacts::of(dom, node),
                            );
                            let children = ParentFormatting::of_children(arena, key, mine);
                            if doc.tracks_restyled {
                                doc.restyled.push(node);
                            }
                            let was = doc.keys[node as usize];
                            changed = was != Some(key) || doc.formatting[node as usize] != children;
                            // A container's own style moving: what is below asks it again.
                            if (container_queries || uses_cq_units) && was.is_some_and(|w| container_moved(arena, w, key, container_reads.as_ref())) {
                                perf_counters::add(&perf_counters::MARKS_BELOW_CONTAINERS, 1);
                                below_containers = true;
                            }
                            if changed {
                                // This element's style or child formatting moved, so the box
                                // tree is no longer what the last build produced.
                                doc.keys_version = doc.keys_version.wrapping_add(1);
                            }
                            doc.keys[node as usize] = Some(key);
                            doc.formatting[node as usize] = children;
                            if Some(node) == root_element {
                                // As `style_from_nothing` does: 16 unless the element says otherwise.
                                let px = crate::style::compute::rem_basis(arena.inherited(key.inherited)).unwrap_or(16.0);
                                if full {
                                    ctx.root_font_size = px;
                                    doc.root_font_size = px;
                                } else if px != ctx.root_font_size {
                                    root_font_moved = true;
                                    return;
                                }
                            }
                            // The pseudo-elements, with the element: their matching sees exactly its
                            // ancestors, as the generated-content walk's does. Under `cq*` units, in the
                            // container basis they were last styled in (see after the walk), and not in a
                            // pass from nothing, which styles them all after the walk.
                            if !uses_cq_units {
                                let next = styling.keys(arena, cascade_memo, &doc.keys, node, key, children, &ctx, Some(&ancestors), &mut pseudo_matched);
                                if doc.pseudo[node as usize] != next {
                                    // A ::before/::after/scrollbar pseudo that moved changes the
                                    // boxes the build generates, even if the element's own key did not.
                                    doc.keys_version = doc.keys_version.wrapping_add(1);
                                }
                                doc.pseudo[node as usize] = next;
                            } else if !full {
                                let at = ComputeContext { container: doc.pseudo_basis, ..ctx };
                                let next = styling.keys(arena, cascade_memo, &doc.keys, node, key, children, &at, Some(&ancestors), &mut pseudo_matched);
                                if doc.pseudo[node as usize] != next {
                                    doc.keys_version = doc.keys_version.wrapping_add(1);
                                }
                                doc.pseudo[node as usize] = next;
                            }
                            // Its answers, when what is asked is known; else unknown, which the next plan
                            // takes for a change (a few controls styled again, once).
                            if rematch && form_asked != Some(0) && dom.has_form_state(node) {
                                doc.track_form(node, form_asked.map_or(incremental::FORM_UNKNOWN, |asked| dom.form_state_bits(node, asked)));
                            }
                        }
                    }
                    let below = std::mem::take(&mut below_containers);
                    if !(subtree || changed || mark & BELOW != 0) {
                        continue;
                    }
                    if element {
                        stack.push((node, true, false, false));
                    }
                    for child in flat.children(dom, node).into_iter().rev() {
                        stack.push((child, false, changed, subtree || below));
                    }
                    if element {
                        ancestors.push(dom, node);
                    }
                }
            });
            doc.flags = flags;
            doc.clear_marks();
            if root_font_moved {
                incremental::count_fallback(Fallback::RootFont);
                full = true;
                continue;
            }
            if uses_cq_units {
                // `style_from_nothing` styles the pseudo-elements after its element walk, in the
                // context that walk leaves: the LAST element's container basis, whichever element's
                // pseudo-element it is. The walk above styled those of the elements it styled in the
                // basis of the last pass; where that basis moved (and after a pass from nothing) every
                // element's are styled again here, in tree order, with its ancestors' filter.
                let last = last_element_below(dom, &mut flat, root)
                    .or_else(|| matches!(dom.node(root).kind, NodeKind::Element { .. }).then_some(root));
                let basis = last.map(|l| {
                    let keys = &doc.keys;
                    let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                    cq_basis(arena, container_sizes, &style_of, dom, l, media)
                });
                if full || doc.pseudo_basis != basis {
                    perf_counters::add(&perf_counters::PSEUDO_RESTYLES_ALL, 1);
                    doc.pseudo_basis = basis;
                    let at = ComputeContext { container: basis, ..ctx };
                    let mut filter = AncestorBloom::for_document_of(dom, root);
                    let mut walk: Vec<(NodeId, bool)> = vec![(root, false)];
                    while let Some((node, leaving)) = walk.pop() {
                        if leaving {
                            filter.pop(dom, node);
                            continue;
                        }
                        let element = matches!(dom.node(node).kind, NodeKind::Element { .. });
                        if element {
                            if let Some(key) = doc.keys[node as usize] {
                                let children = doc.formatting[node as usize];
                                doc.pseudo[node as usize] =
                                    styling.keys(arena, cascade_memo, &doc.keys, node, key, children, &at, Some(&filter), &mut pseudo_matched);
                            }
                            walk.push((node, true));
                        }
                        for child in flat.children(dom, node).into_iter().rev() {
                            walk.push((child, false));
                        }
                        if element {
                            filter.push(dom, node);
                        }
                    }
                }
            }
            break;
        }
        if full {
            perf_counters::add(&perf_counters::STYLE_PASSES_FULL, 1);
        } else {
            perf_counters::add(&perf_counters::STYLE_PASSES_INCREMENTAL, 1);
            *incremental_passes += 1;
            doc.compact_rules();
        }
        // Passes with no animation rescan between them each add what they cascaded: kept to the
        // elements, not the passes.
        if doc.restyled.len() > 2 * doc.keys.len() {
            doc.restyled.sort_unstable();
            doc.restyled.dedup();
        }
        None
    }

    /// The generated content of the document `root` leads to, as `style_from_nothing` makes it, from the
    /// pseudo-element styles [`Page::style_incremental`] kept: counters and quotes laid out in tree order.
    fn generated_content(state: &PageState, dom: &Dom, root: NodeId) -> std::collections::HashMap<NodeId, Generated> {
        let mut out: std::collections::HashMap<NodeId, Generated> = std::collections::HashMap::new();
        let matcher = state.matcher.as_ref().expect("built by the caller");
        if !(matcher.has_pseudo("before") || matcher.has_pseudo("after") || matcher.has_pseudo("-webkit-scrollbar")) {
            return out;
        }
        let (arena, doc) = (&state.arena, &state.doc_styles[&root]);
        let mut flat = crate::flat::FlatTree::default();
        let mut counters: Vec<(String, i32, usize)> = Vec::new();
        let mut quote_depth = 0i32;
        // (node, depth, exiting)
        let mut walk: Vec<(NodeId, usize, bool)> = vec![(root, 0, false)];
        while let Some((node, depth, exiting)) = walk.pop() {
            let Some(key) = doc.keys.get(node as usize).copied().flatten() else {
                if !exiting {
                    walk.push((node, depth, true));
                    for child in flat.children(dom, node).into_iter().rev() {
                        walk.push((child, depth + 1, false));
                    }
                }
                continue;
            };
            let pseudo = doc.pseudo[node as usize].as_deref();
            if exiting {
                counters.retain(|(_, _, d)| *d <= depth + 1);
                if let Some(k) = pseudo.and_then(|p| p.after) {
                    if let Some(b) = pseudo_box(arena, k, &counters, &mut quote_depth) {
                        out.entry(node).or_default().after = Some(b);
                    }
                }
                continue;
            }
            counters.retain(|(_, _, d)| *d <= depth);
            apply_counters(arena, key, depth, &mut counters);
            if !generates_box_for(arena, key) {
                continue;
            }
            if let Some(k) = pseudo.and_then(|p| p.scrollbar) {
                let thickness = |p: PropertyId| px_of(arena.value(k, p));
                let none = arena.value(k, PropertyId::Display).trim() == "none";
                out.entry(node).or_default().scrollbar =
                    Some(if none { (Some(0.0), Some(0.0)) } else { (thickness(PropertyId::Width), thickness(PropertyId::Height)) });
            }
            if let Some(k) = pseudo.and_then(|p| p.before) {
                apply_counters(arena, k, depth + 1, &mut counters);
                if let Some(b) = pseudo_box(arena, k, &counters, &mut quote_depth) {
                    out.entry(node).or_default().before = Some(b);
                }
            }
            walk.push((node, depth, true));
            for child in flat.children(dom, node).into_iter().rev() {
                walk.push((child, depth + 1, false));
            }
        }
        out
    }

    /// Style every element the walk from `root` reaches, from nothing (the reference, and what
    /// every pass did): each element matched and cascaded, its generated content made.
    /// Returns each node's style and the generated content.
    fn style_from_nothing(
        state: &mut PageState,
        dom: &Dom,
        root: NodeId,
        media: &MediaContext,
        reuse_matches: bool,
    ) -> (Vec<Option<StyleKey>>, std::collections::HashMap<NodeId, Generated>) {
        // The matched rules of the previous such pass, if nothing matching reads has moved since. See
        // `MatchCache`. The verifying pass reuses nothing: it matches into a cache of its own.
        if state.style_reading.as_ref().map(|(rev, _)| *rev) != Some(state.sheet_revision) {
            state.style_reading = Some((state.sheet_revision, StyleReading::of(&state.sheets, &state.scopes, &state.adoptions)));
        }
        // The root is part of the generation: one document's matched rules are not another's.
        let generation = (dom.match_revision(), dom.len(), state.sheet_revision, *media, state.container_generation, root);
        let reuse = reuse_matches && state.match_cache.generation == Some(generation);
        let now = Unrevised::of(dom);
        let mut scratch = MatchCache::default();
        if !reuse_matches {
            scratch.spans.resize(dom.len(), (0, 0));
        } else if !reuse {
            state.match_cache.generation = Some(generation);
            state.match_cache.filled = now;
            state.match_cache.rules.clear();
            state.match_cache.spans.clear();
            state.match_cache.spans.resize(dom.len(), (0, 0));
        } else {
            state.match_cache.reused += 1;
        }
        let PageState {
            matcher, sheets, match_cache, cascade_memo, arena, declarative, style_reading, container_sizes, owners, uses_cq_units, ..
        } = state;
        let match_cache = if reuse_matches { match_cache } else { &mut scratch };
        let matcher = matcher.as_ref().expect("just built");
        let style_reading = &style_reading.as_ref().expect("just built").1;
        let stale = if reuse { style_reading.stale(match_cache.filled, now) } else { Reads::default() };
        let mut ctx = ComputeContext {
            viewport_width: media.viewport_width,
            viewport_height: media.viewport_height,
            root_font_size: 16.0,
            min_logical_font_size: ComputeContext::min_logical_of(&media.theme),
            ..ComputeContext::default()
        };

        // Document order, so a parent is always computed before a child has to inherit from it.
        let mut keys: Vec<Option<StyleKey>> = vec![None; dom.len()];
        // ...and the same order is what lets the flex/grid-item context be carried down: an
        // element's entry is written before any of its children read it. See `ParentFormatting`.
        let mut formatting: Vec<ParentFormatting> = vec![ParentFormatting::Flow; dom.len()];
        let mut matched = Vec::new();
        // `(node, leaving)`: an element is pushed onto `ancestors` on the way down and popped on the
        // way back up, so the filter holds exactly the ancestors of the element being matched. Built
        // per element instead, it cost a walk to the root with every ancestor's id and classes hashed
        // again -- 9 % of www.mapbox.com's replay.
        let mut ancestors = AncestorBloom::for_document_of(dom, root);
        let walk_tree = dom.root_of(root);
        // The FLAT tree: a host's shadow tree is styled in place of its light children, and a
        // slotted light child under its slot, inheriting from it.
        let mut flat = crate::flat::FlatTree::default();
        let mut stack: Vec<(NodeId, bool)> = vec![(root, false)];
        let mut is_root = true;
        while let Some((node, leaving)) = stack.pop() {
            if leaving {
                ancestors.pop(dom, node);
                continue;
            }
            let element = matches!(dom.node(node).kind, NodeKind::Element { .. });
            if element {
                stack.push((node, true));
            }
            for child in flat.children(dom, node).into_iter().rev() {
                stack.push((child, false));
            }
            if !element {
                continue;
            }
            let flat_parent = flat.parent(dom, node);
            let parent = flat_parent.and_then(|p| keys[p as usize]);
            let rules: &[MatchedRule] = {
                let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                let (arena_ref, sizes) = (&*arena, &*container_sizes);
                let root_font_size = ctx.root_font_size;
                let containers = |at: NodeId, cond: &ContainerCondition| {
                    container_holds(arena_ref, sizes, &style_of, dom, at, cond, media, root_font_size)
                };
                let owner_parent = |sheet: u32| owner_parent_of(owners, dom, sheet);
                let env = MatchEnv { containers: Some(&containers), owner_parent: Some(&owner_parent), ancestors: Some(&ancestors), tree: Some(walk_tree) };
                if reuse {
                    let (start, len) = match_cache.spans[node as usize];
                    let cached = &match_cache.rules[start as usize..(start + len) as usize];
                    if !stale.any() {
                        cached
                    } else {
                        // The rules a write since the fill CAN change are matched again; the rest are kept.
                        matched.clear();
                        matched.extend(cached.iter().filter(|m| !style_reading.drops(stale, m)));
                        style_reading.rematch(stale, sheets, dom, node, media, &env, &mut matched);
                        &matched
                    }
                } else {
                    perf_counters::add(&perf_counters::ELEMENTS_MATCHED, 1);
                    matcher.match_element_in(sheets, dom, node, media, &env, &mut matched);
                    match_cache.spans[node as usize] = (match_cache.rules.len() as u32, matched.len() as u32);
                    match_cache.rules.extend_from_slice(&matched);
                    &matched
                }
            };
            if *uses_cq_units {
                let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                ctx.container = Some(cq_basis(arena, container_sizes, &style_of, dom, node, media));
            }
            let attr = dom.style_for_cascade(node).unwrap_or("");
            let mine = flat_parent.map_or(ParentFormatting::Flow, |p| formatting[p as usize]);
            // Whole-document, so memoised: see `CascadeMemo` for the measurement and the argument.
            perf_counters::add(&perf_counters::ELEMENTS_STYLED, 1);
            let key = cascade_memoised(
                cascade_memo,
                sheets,
                rules,
                attr,
                &crate::style::hints::for_element(dom, node),
                parent,
                mine,
                &ctx,
                arena,
                declarative.animated_for(node),
                ElementFacts::of(dom, node),
            );
            formatting[node as usize] = ParentFormatting::of_children(arena, key, mine);
            if is_root {
                // `rem` is the document element's font size, for itself as well as everything below.
                if let Some(px) = crate::style::compute::rem_basis(arena.inherited(key.inherited)) {
                    ctx.root_font_size = px;
                }
                is_root = false;
            }
            keys[node as usize] = Some(key);
            ancestors.push(dom, node);
        }

        // GENERATED CONTENT: `::before`/`::after` boxes and `::-webkit-scrollbar`
        // thicknesses. A document with no rule naming either pays one hash lookup each.
        let generated = if matcher.has_pseudo("before") || matcher.has_pseudo("after") || matcher.has_pseudo("-webkit-scrollbar") {
            let mut out: std::collections::HashMap<NodeId, Generated> = std::collections::HashMap::new();
            let mut counters: Vec<(String, i32, usize)> = Vec::new();
            let mut quote_depth = 0i32;
            // (node, depth, exiting)
            let mut walk: Vec<(NodeId, usize, bool)> = vec![(root, 0, false)];
            // The ancestors of the node being visited, kept as the walk enters and leaves them:
            // with none, `collect` built one per element per pseudo-element name from the whole
            // ancestor chain -- 12 % of mapbox.com's load CPU (2026-09-29). A node is in it while its
            // children are walked: pushed after its own `::before` / scrollbar, popped before its
            // `::after`, so each pseudo-element sees exactly its element's ancestors.
            let mut ancestors = AncestorBloom::for_node(dom, root);
            while let Some((node, depth, exiting)) = walk.pop() {
                let Some(key) = keys.get(node as usize).copied().flatten() else {
                    if exiting {
                        ancestors.pop(dom, node);
                    } else {
                        ancestors.push(dom, node);
                        walk.push((node, depth, true));
                        for child in flat.children(dom, node).into_iter().rev() {
                            walk.push((child, depth + 1, false));
                        }
                    }
                    continue;
                };
                if exiting {
                    ancestors.pop(dom, node);
                }
                let pseudo = |name: &str, arena: &mut StyleArena, cascade_memo: &mut CascadeMemo, matched: &mut Vec<MatchedRule>| -> Option<StyleKey> {
                    if !matcher.has_pseudo(name) {
                        return None;
                    }
                    {
                        let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                        let (arena_ref, sizes) = (&*arena, &*container_sizes);
                        let containers = |at: NodeId, cond: &ContainerCondition| {
                            container_holds(arena_ref, sizes, &style_of, dom, at, cond, media, ctx.root_font_size)
                        };
                        let owner_parent = |sheet: u32| owner_parent_of(owners, dom, sheet);
                        let env = MatchEnv { containers: Some(&containers), owner_parent: Some(&owner_parent), ancestors: Some(&ancestors), tree: None };
                        matcher.match_pseudo(sheets, dom, node, media, name, &env, matched);
                    }
                    if matched.is_empty() {
                        return None;
                    }
                    let facts = ElementFacts { textarea: false, quirks: dom.document_mode_of(node) == korvus_dom::DocumentMode::Quirks, contents_is_none: false, svg_block: false };
                    let k = cascade_memoised(cascade_memo, sheets, matched, "", "", Some(key), formatting[node as usize], &ctx, arena, &[], facts);
                    Some(pseudo_content(arena, k, name, dom, node))
                };
                if exiting {
                    counters.retain(|(_, _, d)| *d <= depth + 1);
                    if let Some(k) = pseudo("after", arena, cascade_memo, &mut matched) {
                        if let Some(b) = pseudo_box(arena, k, &counters, &mut quote_depth) {
                            out.entry(node).or_default().after = Some(b);
                        }
                    }
                    continue;
                }
                // Leaving a scope: a counter a sibling subtree deeper than this element created ends.
                counters.retain(|(_, _, d)| *d <= depth);
                apply_counters(arena, key, depth, &mut counters);
                if !generates_box_for(arena, key) {
                    continue;
                }
                if let Some(k) = pseudo("-webkit-scrollbar", arena, cascade_memo, &mut matched) {
                    let thickness = |p: PropertyId| px_of(arena.value(k, p));
                    let none = arena.value(k, PropertyId::Display).trim() == "none";
                    out.entry(node).or_default().scrollbar =
                        Some(if none { (Some(0.0), Some(0.0)) } else { (thickness(PropertyId::Width), thickness(PropertyId::Height)) });
                }
                if let Some(k) = pseudo("before", arena, cascade_memo, &mut matched) {
                    apply_counters(arena, k, depth + 1, &mut counters);
                    if let Some(b) = pseudo_box(arena, k, &counters, &mut quote_depth) {
                        out.entry(node).or_default().before = Some(b);
                    }
                }
                ancestors.push(dom, node);
                walk.push((node, depth, true));
                for child in flat.children(dom, node).into_iter().rev() {
                    walk.push((child, depth + 1, false));
                }
            }
            out
        } else {
            std::collections::HashMap::new()
        };

        (keys, generated)
    }

    // ── declarative animations ──────────────────────────────────────────────────────────────────

    /// The keyframes of `name`, as `(offset, declarations)` sorted by offset.
    ///
    /// The LAST sheet to declare the name wins, and within a sheet the last rule does
    /// (`Stylesheet::push_keyframes`), because `@keyframes` addresses one set of keyframes by name
    /// and a second declaration of it replaces the first. `None` means no rule of that name exists,
    /// which is why `animation: nosuchkeyframes 4s` creates no animation at all — measured in
    /// Chrome, where such an element's `getAnimations()` is empty while its computed
    /// `animation-name` still reads `nosuchkeyframes`.
    pub fn keyframes(&self, name: &str) -> Option<Vec<(f32, Vec<(PropertyId, String)>)>> {
        self.keyframes_in(name, None)
    }

    /// [`Page::keyframes`] as the element in tree `tree` (a document or a shadow root) sees them:
    /// `@keyframes` names are TREE-SCOPED (CSS Scoping 1 §3.5), so only the sheets that style that tree
    /// answer. Chrome 150: a shadow root's own `@keyframes` animate its elements, and a shadow-tree
    /// element naming a keyframes rule only the document declares does not animate. `None`: every sheet.
    pub fn keyframes_in(&self, name: &str, tree: Option<NodeId>) -> Option<Vec<(f32, Vec<(PropertyId, String)>)>> {
        let state = self.state.lock().unwrap();
        let styles_tree = |i: usize| match (tree, &state.scopes[i]) {
            (None, _) | (_, SheetScope::Everywhere) => true,
            (Some(t), SheetScope::Tree(root)) => *root == t,
            (Some(t), SheetScope::Adopted) => state.adoptions.get(&t).is_some_and(|l| l.contains(&(i as u32))),
        };
        let rule = state.sheets.iter().enumerate().rev().filter(|(i, _)| styles_tree(*i)).find_map(|(_, sheet)| {
            sheet.keyframes().iter().rev().find(|k| k.name == name).map(|k| (sheet, k))
        })?;
        let (sheet, rule) = rule;
        let mut out: Vec<(f32, Vec<(PropertyId, String)>)> = Vec::new();
        for block in &rule.blocks {
            let decls: Vec<(PropertyId, String)> = sheet.declarations()
                [block.decls.start as usize..block.decls.end as usize]
                .iter()
                .map(|d| (d.property, sheet.value_of(d).to_string()))
                .collect();
            for offset in &block.offsets {
                out.push((*offset, decls.clone()));
            }
        }
        out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        Some(out)
    }

    /// Whether anything in the document could declare a CSS animation or a transition: a rule of its
    /// sheets that declares one of the thirteen `animation-*` / `transition-*` longhands, or an
    /// element whose `style` attribute mentions one -- the two ways [`Page::declarative_update`]'s
    /// rescan finds a candidate (`rescan_everything`), asked without its walk. `false` means a rescan
    /// would find no candidate: no animation to make and no before-change style to keep. Conservative:
    /// a detached element's `style` counts too.
    pub fn may_declare_animations(&self, dom: &Dom) -> bool {
        self.sync_document_sheets(dom);
        {
            let mut state = self.state.lock().unwrap();
            if state.declarative_index.as_ref().map(|(rev, _, _)| *rev) != Some(state.sheet_revision) {
                let (index, reads) = declarative_rule_index(&state.sheets);
                state.declarative_index = Some((state.sheet_revision, index, reads));
            }
            if state.declarative_index.as_ref().is_some_and(|(_, index, _)| !index.is_empty()) {
                return true;
            }
        }
        (0..dom.len() as NodeId).any(|id| {
            dom.get_attribute(id, "style").is_some_and(|s| s.contains("animation") || s.contains("transition"))
        })
    }

    /// Re-read every element's `animation-*` and `transition-*` longhands and report what the
    /// document now declares: which CSS animations should exist, and which transitions have just
    /// become due.
    ///
    /// This is the "update animations" step of the rendering loop, and it is deliberately LAZY
    /// rather than driven by a frame clock: this engine has no vsync, so the honest trigger is
    /// "before anything observes the document". It is gated on [`Page::layout_fingerprint`], which
    /// covers every input the cascade has — a DOM mutation, a sheet edit, a viewport change — so
    /// the steady state of an animation playing over an untouched document is one integer compare.
    ///
    /// It computes styles WITHOUT the animation overrides on purpose. A transition starts when the
    /// CASCADED value changes; comparing animated values instead would make a running animation
    /// look like a cascade change and start a transition on every frame.
    pub fn declarative_update(&self, dom: &Dom, media: &MediaContext) -> DeclarativeUpdate {
        perf_counters::add(&perf_counters::DECLARATIVE_UPDATES, 1);
        let out = korvus_dom::with_pass_caches(|| self.declarative_update_in_pass(dom, media));
        if out.rescanned {
            perf_counters::flush();
        }
        out
    }

    /// [`Page::declarative_update`], inside one pass of shared selector caches.
    fn declarative_update_in_pass(&self, dom: &Dom, media: &MediaContext) -> DeclarativeUpdate {
        let fingerprint = self.cascade_fingerprint(dom, media);
        self.sync_document_sheets(dom);
        let mut state = self.state.lock().unwrap();
        if !declarative::needs_rescan(&state.declarative, fingerprint) {
            // Nothing that feeds the cascade has moved, so no transition can have become due and
            // no animation can have been declared or withdrawn. The caller still needs the list.
            //
            // It walks nothing. The list is a function of the last rescan's styles
            // and the append-only arena, neither of which a skipped update can change, so it is
            // derived once, by the rescan, and handed back here. Re-deriving it per
            // call parsed eight comma lists per styled node on every `getComputedStyle`: on a page
            // whose rules put a `transition` on most elements that was every node, and sampling the
            // engine thread while the parity driver's `Runtime.evaluate` waited 30 s unanswered found
            // it in 15 of 19 samples on tc39.es/ecma262 and 13 of 17 on supabase.com.
            return DeclarativeUpdate { animations: state.last_animations.clone(), ..Default::default() };
        }
        // A container query is answered by a laid-out ancestor, here as in `computed_style`: when the sizes
        // were not taken at this document, lay it out first. A rescan before the sizes would
        // cascade an element without the `@container` rules that hold for it, and the one after them would
        // take the difference for a change and start transitions Chrome never starts: Chrome's first style
        // of an element is made with its containers laid out.
        // The matcher first: it is what says whether the sheets have container rules at all, and a sheet added
        // since the last rescan has not been looked at yet.
        Self::ensure_matcher(&mut state);
        if state.has_container_rules && state.container_basis != Some(Self::container_basis_of(&state, dom, media)) {
            drop(state);
            self.settle_containers(dom, media);
            return self.declarative_update_in_pass(dom, media);
        }
        let state = &mut *state;
        state.cascade_memo.begin(state.sheet_revision, media);
        // A page whose styles follow laid-out sizes -- `@container` rules, `cq*` units -- rescanned as before
        // From nothing each time: the rescan answered no container rule and resolved `cq*`
        // against the viewport, so its styles were not the page style pass's. Both now answer as that pass
        // does (`base_chains`, `chain_bases`), and the pass styles such a page from what changed
        // (after I3). www.airbnb.com started over 52 times in 52 rescans, www.glassdoor.com 242 in 244.
        // KORVUS_VERIFY_INCREMENTAL: the whole-document rescan, from the same state, is what the
        // incremental one must say. It runs first, on a copy of the animation state and with
        // matches of its own, and what it records is put back.
        let reference = verify::enabled().then(|| {
            let kept = (
                std::mem::take(&mut state.declarative_scan),
                std::mem::take(&mut state.declarative_matches),
                state.last_animations.clone(),
                state.last_styled,
                state.declarative_spec_walks,
                state.declarative_selector_tests,
            );
            let mut theirs = state.declarative.clone();
            std::mem::swap(&mut state.declarative, &mut theirs);
            let (full, ..) = Self::rescan_everything(state, dom, media, fingerprint);
            std::mem::swap(&mut state.declarative, &mut theirs);
            (
                state.declarative_scan,
                state.declarative_matches,
                state.last_animations,
                state.last_styled,
                state.declarative_spec_walks,
                state.declarative_selector_tests,
            ) = kept;
            (full, theirs)
        });
        let out = Self::rescan_what_changed(state, dom, media, fingerprint);
        if let Some((full, theirs)) = reference {
            verify::started();
            if let Some(d) = declarative::difference(&out, &state.declarative, &full, &theirs) {
                verify::mismatch(&format!("animation rescan of {}: {d}", verify::document_title(dom)));
            }
        }
        out
    }

    /// The animation rescan from what changed.
    ///
    /// [`Page::rescan_everything`] scans every candidate and every ancestor of one with its
    /// before-animation style. A scanned element whose style is the one it was last scanned with, and
    /// which stays scanned, answers as it did then -- no transition, the same animations -- so it is
    /// enough to scan:
    ///
    /// * the elements the page's style pass cascaded since the last rescan, and the ones it forgot
    ///   (`DocStyle::restyled`): each is asked again whether it is a candidate;
    /// * the elements that join the scanned set or leave it;
    /// * the elements whose before-animation style is not the page style pass's -- an animated element
    ///   and everything under it -- cascaded through their ancestors as the whole rescan does them,
    ///   this rescan and the one after.
    ///
    /// Every other scanned element's before-animation style IS the page style pass's: the same rules
    /// matched (the matching is the same function), the same cascade, the same parent (no shadow tree
    /// in the document, so the flat tree is the DOM) and no animated values. That pass, brought up to
    /// date first, supplies them.
    ///
    /// The scanned and chained sets are worked out again only where they can have moved: a candidate
    /// came or went, a scanned element was unlinked (it, or a subtree holding it, moved or left), or
    /// other elements are animated. Otherwise the work is what was restyled and what is chained.
    ///
    /// It starts over, as [`Page::rescan_everything`], on the first rescan and after a navigation; in
    /// a document with a shadow root (an element outside the flat tree has no page style); under an
    /// `@scope` rule (a prelude-less one is rooted differently there); and while the document element
    /// is animated (`rem`).
    fn rescan_what_changed(state: &mut PageState, dom: &Dom, media: &MediaContext, fingerprint: u64) -> DeclarativeUpdate {
        let root = dom.root();
        let root_element = {
            let mut cur = dom.first_child(root);
            while let Some(c) = cur {
                if matches!(dom.node(c).kind, NodeKind::Element { .. }) {
                    break;
                }
                cur = dom.node(c).next_sibling;
            }
            cur
        };
        Self::ensure_features(state);
        let scope = state.features.as_ref().is_some_and(|(_, f)| f.scope);
        // A document with shadow roots rescans from what changed too: candidacy is asked of the
        // elements of every tree in the document, the scanned and chained sets and the chained cascades follow the
        // FLAT tree the page style pass styles, and what the flat tree does not render is not rendered. It started
        // over here before: www.airbnb.com's one closed shadow root (a `<browser-font-size>` probe) made 54 of its 57
        // rescans whole ones.
        if scope {
            perf_counters::add(&perf_counters::DECLARATIVE_STARTED_OVER_SCOPE, 1);
            state.declarative_scan.valid = false;
            if let Some(doc) = state.doc_styles.get_mut(&root) {
                doc.restyled.clear();
            }
            return Self::rescan_everything(state, dom, media, fingerprint).0;
        }
        Self::style_incremental(state, dom, root, media);
        let first = !state.declarative_scan.valid
            || state.declarative_scan.instance != dom.instance()
            || (dom.has_shadow_roots() && state.declarative_scan.shadow_moved(dom));
        let root_animated = root_element.is_some_and(|e| !state.declarative.animated_for(e).is_empty());
        let doc = state.doc_styles.get_mut(&root).expect("just styled");
        let mut restyled = std::mem::take(&mut doc.restyled);
        if first || root_animated {
            let counter = if root_animated { &perf_counters::DECLARATIVE_STARTED_OVER_ROOT } else { &perf_counters::DECLARATIVE_STARTED_OVER_FIRST };
            perf_counters::add(counter, 1);
            let (out, candidates, _) = Self::rescan_everything(state, dom, media, fingerprint);
            let scan = &mut state.declarative_scan;
            scan.instance = dom.instance();
            scan.candidate.clear();
            scan.candidate.resize(dom.len(), false);
            for c in candidates {
                scan.candidate[c as usize] = true;
            }
            let keys = &state.doc_styles[&root].keys;
            let mut animated: Vec<NodeId> = state.declarative.animated_nodes().collect();
            animated.sort_unstable();
            scan.rework(dom, root, root_element, animated);
            // See the same test below.
            let arena = &state.arena;
            let mut hidden = Hidden::new(arena, dom, keys);
            scan.valid = scan.scanned.iter().zip(keys).enumerate().all(|(x, (s, k))| !s || k.is_some() || hidden.of(x as NodeId));
            let arena = &state.arena;
            scan.own_none = scan.scanned.iter().zip(keys).map(|(s, k)| *s && k.is_some_and(|k| display_none(arena, k))).collect();
            scan.specs.clear();
            scan.chain.clear();
            for spec in out.animations.iter() {
                scan.specs.entry(spec.node).or_default().push(spec.clone());
            }
            return out;
        }
        perf_counters::add(&perf_counters::DECLARATIVE_RESCANS_INCREMENTAL, 1);
        perf_counters::add(&perf_counters::DECLARATIVE_RESCANS, 1);
        state.rescans_from_what_changed += 1;
        let n = dom.len();
        let mut dm = Self::declarative_matches(state, dom, media);
        let now = Unrevised::of(dom);

        // Who is a candidate now, asked again of each element the style pass cascaded or forgot.
        restyled.sort_unstable();
        restyled.dedup();
        restyled.retain(|&x| (x as usize) < n);
        if state.declaring_rules.as_ref().map(|(rev, _)| *rev) != Some(state.sheet_revision) {
            state.declaring_rules = Some((state.sheet_revision, DeclaringRules::of(&state.sheets, &state.scopes, root)));
        }
        let scan = &mut state.declarative_scan;
        scan.candidate.resize(n, false);
        let mut candidacy_moved = false;
        {
            let (_, index, reads) = state.declarative_index.as_ref().expect("just built");
            let declaring = Declaring { index, reads: *reads, sheets: &state.sheets, scopes: &state.scopes, adoptions: &state.adoptions, media, now };
            let (_, declaring_rules) = state.declaring_rules.as_ref().expect("just built");
            let doc = &state.doc_styles[&root];
            let (mut classes, mut rules) = (Vec::new(), Vec::new());
            for &x in &restyled {
                perf_counters::add(&perf_counters::DECLARATIVE_ELEMENTS_VISITED, 1);
                // An element the page style pass styled answers from the rules it matched; another as before.
                let matched = doc.keys.get(x as usize).copied().flatten().and_then(|_| {
                    let (start, len) = *doc.spans.get(x as usize)?;
                    doc.rules.get(start as usize..(start + len) as usize)
                });
                let is = dom.is_connected(x)
                    && match matched {
                        Some(m) => declaring.declares_matched(dom, x, m, declaring_rules, &mut state.declarative_selector_tests, &mut classes, &mut rules),
                        None => declaring.declares(&mut dm, dom, x, &mut state.declarative_selector_tests, &mut classes, &mut rules),
                    };
                candidacy_moved |= scan.candidate[x as usize] != is;
                scan.candidate[x as usize] = is;
            }
        }
        let mut animated: Vec<NodeId> = state.declarative.animated_nodes().collect();
        animated.sort_unstable();

        // What to scan: what was restyled, what joined or left, and what goes through its ancestors.
        // `was_scanned`: the scanned set before, where it moved.
        let (touched, was_scanned): (Vec<NodeId>, Option<Vec<bool>>) = if scan.moved(dom, candidacy_moved, &animated) {
            perf_counters::add(&perf_counters::DECLARATIVE_SETS_REWORKED, 1);
            let (was_scanned, was_chained) = (std::mem::take(&mut scan.scanned), std::mem::take(&mut scan.chained));
            scan.rework(dom, root, root_element, animated);
            // Every scanned element has a page style -- or is not rendered (`Hidden`: outside the flat tree, or
            // under `display: none`), which declares nothing: if one did not, there would be nothing to start from.
            let keys = &state.doc_styles[&root].keys;
            let mut hidden = Hidden::new(&state.arena, dom, keys);
            if (0..n).any(|x| scan.scanned[x] && keys[x].is_none() && !hidden.of(x as NodeId)) {
                perf_counters::add(&perf_counters::DECLARATIVE_STARTED_OVER_FIRST, 1);
                state.declarative_scan.valid = false;
                state.declarative_matches = dm;
                return Self::rescan_what_changed(state, dom, media, fingerprint);
            }
            let mut is_restyled = vec![false; n];
            for &x in &restyled {
                is_restyled[x as usize] = true;
            }
            let was = |v: &Vec<bool>, x: usize| v.get(x).copied().unwrap_or(false);
            let touched = (0..n)
                .filter(|&x| {
                    let (now_in, then_in) = (scan.scanned[x], was(&was_scanned, x));
                    now_in != then_in || (now_in && is_restyled[x]) || scan.chained[x] || was(&was_chained, x)
                })
                .map(|x| x as NodeId)
                .collect();
            (touched, Some(was_scanned))
        } else {
            // The same sets: what was restyled in them, and the chained.
            scan.scanned.resize(n, false);
            scan.chained.resize(n, false);
            scan.seq = dom.journal().seq();
            let mut touched: Vec<NodeId> = restyled.iter().copied().filter(|&x| scan.scanned[x as usize]).collect();
            touched.extend_from_slice(&scan.chained_list);
            touched.sort_unstable();
            touched.dedup();
            (touched, None)
        };
        // An element whose own `display: none` came or went hides or shows what is below it, whose styles did not
        // move: everything scanned below it is scanned again (`declarative::not_rendered`).
        let touched = {
            let keys = &state.doc_styles[&root].keys;
            scan.own_none.resize(n, false);
            let mut below: Vec<NodeId> = Vec::new();
            for &x in &touched {
                let i = x as usize;
                let none = scan.scanned[i] && keys[i].is_some_and(|k| display_none(&state.arena, k));
                if none == scan.own_none[i] {
                    continue;
                }
                scan.own_none[i] = none;
                let mut stack: Vec<NodeId> = Vec::new();
                let mut child = dom.node(x).first_child;
                while let Some(c) = child {
                    stack.push(c);
                    child = dom.node(c).next_sibling;
                }
                if let Some(sr) = dom.shadow_root_of(x) {
                    stack.push(sr);
                }
                while let Some(c) = stack.pop() {
                    if scan.scanned.get(c as usize).copied().unwrap_or(false) {
                        below.push(c);
                    }
                    let mut child = dom.node(c).first_child;
                    while let Some(g) = child {
                        stack.push(g);
                        child = dom.node(g).next_sibling;
                    }
                    // and a host's shadow tree, which is below it in the flat tree
                    if let Some(sr) = dom.shadow_root_of(c) {
                        stack.push(sr);
                    }
                }
            }
            let mut touched = touched;
            if !below.is_empty() {
                touched.extend(below);
                touched.sort_unstable();
                touched.dedup();
            }
            touched
        };
        let count = scan.count;
        let chained: Vec<NodeId> = touched.iter().copied().filter(|&x| scan.chained[x as usize]).collect();
        perf_counters::add(&perf_counters::DECLARATIVE_TOUCHED, touched.len() as u64);
        perf_counters::add(&perf_counters::DECLARATIVE_CHAINED, chained.len() as u64);
        let through_ancestors = Self::chain_bases(state, dom, media, root, &chained);
        let scan = &mut state.declarative_scan;
        let keys = &state.doc_styles[&root].keys;
        let entries: Vec<(NodeId, Option<StyleKey>, bool)> = touched
            .iter()
            .map(|&x| {
                let i = x as usize;
                let key = if !scan.scanned[i] {
                    None
                } else if scan.chained[i] {
                    through_ancestors.get(&x).map(|&(k, _)| k)
                } else {
                    keys[i]
                };
                (x, key, was_scanned.as_ref().map_or(scan.scanned[i], |w| w.get(i).copied().unwrap_or(false)))
            })
            .collect();
        let shown = state.declarative.animated_snapshot();
        let hidden = std::cell::RefCell::new(Hidden::new(&state.arena, dom, keys));
        let hidden = |x: NodeId| hidden.borrow_mut().of(x);
        let (out, read) = declarative::scan_touched(&mut state.declarative, dom, &state.arena, &entries, &mut scan.specs, hidden, |node, property, cascaded| {
            shown
                .get(&node)
                .and_then(|vs| vs.iter().find(|a| a.property == property))
                .map(|a| a.value.clone())
                .unwrap_or_else(|| cascaded.to_string())
        });
        declarative::note_fingerprint(&mut state.declarative, fingerprint);
        state.declarative_matches = dm;
        state.last_styled = count;
        state.declarative_spec_walks += read as u64;
        state.last_animations = out.animations.clone();
        out
    }

    /// The rescans' matching state for this document: the declaring rules' index and the
    /// `style`-reading rules brought up to the sheets, and [`DeclarativeMatches`] kept while its
    /// generation holds. Put back in `state` by the rescan.
    fn declarative_matches(state: &mut PageState, dom: &Dom, media: &MediaContext) -> DeclarativeMatches {
        if state.declarative_index.as_ref().map(|(rev, _, _)| *rev) != Some(state.sheet_revision) {
            let (index, reads) = declarative_rule_index(&state.sheets);
            state.declarative_index = Some((state.sheet_revision, index, reads));
        }
        if state.style_reading.as_ref().map(|(rev, _)| *rev) != Some(state.sheet_revision) {
            state.style_reading = Some((state.sheet_revision, StyleReading::of(&state.sheets, &state.scopes, &state.adoptions)));
        }
        let generation = (dom.match_revision(), dom.len(), state.sheet_revision, *media, state.container_generation);
        let dm = std::mem::take(&mut state.declarative_matches);
        if dm.generation == Some(generation) {
            return dm;
        }
        DeclarativeMatches { generation: Some(generation), ..Default::default() }
    }

    /// The whole-document animation rescan (once the only one): every element that could
    /// declare an animation or a transition found by matching, the before-animation styles of those and
    /// their ancestors cascaded, and all of them scanned.
    ///
    /// Returns the update, the candidates, and the style each scanned node was scanned with.
    fn rescan_everything(
        state: &mut PageState,
        dom: &Dom,
        media: &MediaContext,
        fingerprint: u64,
    ) -> (DeclarativeUpdate, Vec<NodeId>, Vec<Option<StyleKey>>) {
        // PASS ONE: which elements could declare an animation or a transition at all.
        //
        // This pass MATCHES but does not COMPUTE, and the difference is the whole cost of this
        // feature. Matching an element is a few index lookups; computing its style resolves 475
        // properties and interns the result. Styling the whole document here -- which is what this
        // function did first -- cost a second full cascade on every observation, and since a live
        // page mutates its DOM between calls, the fingerprint gate did not catch it. MEASURED on
        // `https://www.chosun.com/` (21,824 elements and, as it happens, NO animations at all): a
        // 150-element `getComputedStyle` loop went from 194 ms to 3,788 ms. A page that declares no
        // animation must not pay for the machinery that runs them.
        //
        // An element is a candidate when a rule that declares one of the thirteen `animation-*` /
        // `transition-*` longhands matches it, or when its inline `style` mentions one -- a cheap
        // substring test, which is sound because a declaration cannot reach the element without its
        // property name appearing in the text.
        //
        // AND IT MATCHES ONLY THE RULES THAT COULD MAKE AN ELEMENT A CANDIDATE. The
        // question is "does a DECLARING rule match", and matching every rule of the document to
        // answer it cost a full match of every element on every rescan -- which a page that writes
        // and then reads a computed style pays per write. Sampling the engine thread while the parity
        // driver waited 30 s on www.who.int found `match_element` under this pass in 19 of 30 samples
        // once the cascade itself was memoised. The index below holds only the rules that declare an
        // `animation-*` / `transition-*` longhand, bucketed exactly as `Matcher` buckets every rule,
        // and each candidate rule is tested exactly as `Matcher::match_element` tests it
        // (`rule_applies`, then any selector) -- so the answer is the one full matching gave.
        let mut dm = Self::declarative_matches(state, dom, media);
        let now = Unrevised::of(dom);
        let (_, index, reads) = state.declarative_index.as_ref().expect("just built");
        let declaring = Declaring { index, reads: *reads, sheets: &state.sheets, scopes: &state.scopes, adoptions: &state.adoptions, media, now };
        let mut candidates: Vec<NodeId> = Vec::new();
        let (mut classes, mut rule_candidates) = (Vec::new(), Vec::new());
        let mut stack: Vec<NodeId> = vec![dom.root()];
        while let Some(node) = stack.pop() {
            // Last child first, through the sibling links: `Dom::children` makes a vector per node.
            let mut child = dom.node(node).last_child;
            while let Some(c) = child {
                stack.push(c);
                child = dom.node(c).prev_sibling;
            }
            // and a host's shadow tree, whose elements animate too: Chrome 150 runs a `@keyframes` of
            // an interactive challenge widget's shadow root on its spinner (`getAnimations()` 1; this walk never
            // entered the tree, so it was 0)
            if let Some(shadow) = dom.shadow_root_of(node) {
                stack.push(shadow);
            }
            if !matches!(dom.node(node).kind, NodeKind::Element { .. }) {
                continue;
            }
            perf_counters::add(&perf_counters::DECLARATIVE_ELEMENTS_VISITED, 1);
            if declaring.declares(&mut dm, dom, node, &mut state.declarative_selector_tests, &mut classes, &mut rule_candidates) {
                candidates.push(node);
            }
        }

        let keys = Self::base_chains(state, dom, media, &mut dm, now, &candidates);
        // The override map is read by the `displayed` closure while `scan` holds `&mut` on the
        // rest of the state, so it is snapshotted first rather than borrowed through.
        let shown = state.declarative.animated_snapshot();
        perf_counters::add(&perf_counters::DECLARATIVE_RESCANS, 1);
        let hidden = std::cell::RefCell::new(Hidden::new(&state.arena, dom, &keys));
        let hidden = |x: NodeId| hidden.borrow_mut().of(x);
        let out = declarative::scan(&mut state.declarative, dom, &state.arena, &keys, hidden, |node, property, cascaded| {
            shown
                .get(&node)
                .and_then(|vs| vs.iter().find(|a| a.property == property))
                .map(|a| a.value.clone())
                .unwrap_or_else(|| cascaded.to_string())
        });
        declarative::note_fingerprint(&mut state.declarative, fingerprint);
        state.declarative_matches = dm;
        state.last_styled = keys.iter().filter(|k| k.is_some()).count();
        state.declarative_spec_walks += state.last_styled as u64;
        // The list every skipped update until the next rescan hands back. See the skipped path.
        state.last_animations = out.animations.clone();
        (out, candidates, keys)
    }


    /// The before-animation styles of the chained elements `chained`, by node, with what each
    /// makes of its children: each cascaded with no animated values from its parent's before-animation
    /// style -- the page style pass's, for a parent that is not chained -- and the rules the page style
    /// pass matched for it (the same matching, on the path that calls this). An element whose inputs
    /// are the ones it was last cascaded from (`DeclarativeScan::chain`) is not cascaded again: an
    /// animation that moves leaves them as they were.
    fn chain_bases(
        state: &mut PageState,
        dom: &Dom,
        media: &MediaContext,
        root: NodeId,
        chained: &[NodeId],
    ) -> korvus_dom::FxHashMap<NodeId, (StyleKey, ParentFormatting)> {
        let PageState { doc_styles, declarative_scan: scan, cascade_memo, sheets, arena, container_sizes, uses_cq_units, .. } = state;
        let doc = &doc_styles[&root];
        let mut ctx = ComputeContext {
            viewport_width: media.viewport_width,
            viewport_height: media.viewport_height,
            root_font_size: doc.root_font_size,
            min_logical_font_size: ComputeContext::min_logical_of(&media.theme),
            ..ComputeContext::default()
        };
        let mut done: korvus_dom::FxHashMap<NodeId, (StyleKey, ParentFormatting)> = Default::default();
        let mut path: Vec<NodeId> = Vec::new();
        // The parents are the FLAT tree's, as the page style pass cascades: a slotted element's is
        // its slot, a shadow tree's top element's its host.
        let mut flat = crate::flat::FlatTree::default();
        for &x in chained {
            path.clear();
            let mut cur = Some(x);
            while let Some(c) = cur {
                if !scan.chained[c as usize] || done.contains_key(&c) {
                    break;
                }
                path.push(c);
                cur = flat.parent(dom, c);
            }
            for &c in path.iter().rev() {
                let (parent, mine) = match flat.parent(dom, c) {
                    Some(p) => done.get(&p).map_or((doc.keys[p as usize], doc.formatting[p as usize]), |&(k, f)| (Some(k), f)),
                    None => (None, ParentFormatting::Flow),
                };
                let (start, len) = doc.spans[c as usize];
                if *uses_cq_units {
                    // As the page style pass does: see `base_chains`.
                    let style_of = |a: NodeId| doc.keys.get(a as usize).copied().flatten();
                    ctx.container = Some(cq_basis(arena, container_sizes, &style_of, dom, c, media));
                }
                let inputs = ChainInputs {
                    matched: (doc.epoch, (start, len)),
                    parent,
                    mine,
                    facts: ElementFacts::of(dom, c),
                    style_writes: dom.style_writes_of(c),
                    container: ctx.container.map(|(w, h)| (w.to_bits(), h.to_bits())),
                };
                let key = match scan.chain.get(&c) {
                    Some(&(was, key)) if was == inputs => key,
                    _ => {
                        perf_counters::add(&perf_counters::DECLARATIVE_CASCADES, 1);
                        let matched = &doc.rules[start as usize..(start + len) as usize];
                        let attr = dom.style_for_cascade(c).unwrap_or("");
                        let key = cascade_memoised(cascade_memo, sheets, matched, attr, &crate::style::hints::for_element(dom, c), parent, mine, &ctx, arena, &[], inputs.facts);
                        scan.chain.insert(c, (inputs, key));
                        key
                    }
                };
                done.insert(c, (key, ParentFormatting::of_children(arena, key, mine)));
            }
        }
        scan.chain.retain(|c, _| done.contains_key(c));
        done
    }

    /// PASS TWO of a rescan: the before-animation styles of `candidates` and of every ancestor of
    /// theirs, and of the document element (its font size is `rem`), each matched (through `dm`) and
    /// cascaded with no animated values from its DOM parent's. By node.
    fn base_chains(
        state: &mut PageState,
        dom: &Dom,
        media: &MediaContext,
        dm: &mut DeclarativeMatches,
        now: Unrevised,
        candidates: &[NodeId],
    ) -> Vec<Option<StyleKey>> {
        let matcher = state.matcher.as_ref().expect("built by the caller");
        let style_reading = &state.style_reading.as_ref().expect("built by the caller").1;
        let (container_rules, cq_units) = (state.has_container_rules, state.uses_cq_units);
        let mut ctx = ComputeContext {
            viewport_width: media.viewport_width,
            viewport_height: media.viewport_height,
            root_font_size: 16.0,
            quirks: false,
            container: None,
            min_logical_font_size: ComputeContext::min_logical_of(&media.theme),
        };
        let mut keys: Vec<Option<StyleKey>> = vec![None; dom.len()];
        // Memoised beside `keys` and written wherever `keys` is, so a chain that stops at an
        // already-computed ancestor still finds that ancestor's answer. See `ParentFormatting`.
        let mut formatting: Vec<ParentFormatting> = vec![ParentFormatting::Flow; dom.len()];
        let mut matched = Vec::new();
        // PASS TWO: the candidates' real styles, ancestor chain and all, memoised so a shared
        // ancestor is computed once however many candidates sit under it.
        //
        // The root's font size has to be known before `rem` means anything, so it is resolved first
        // whether or not the root is a candidate.
        let mut chain: Vec<NodeId> = Vec::new();
        {
            let root = dom.root();
            let mut first_element = None;
            let mut stack = vec![root];
            while let Some(n) = stack.pop() {
                if matches!(dom.node(n).kind, NodeKind::Element { .. }) {
                    first_element = Some(n);
                    break;
                }
                for child in dom.children(n).into_iter().rev() {
                    stack.push(child);
                }
            }
            if let Some(n) = first_element {
                declarative_match(dm, matcher, style_reading, &state.sheets, dom, n, media, now, &MatchEnv::default(), &mut matched);
                if cq_units {
                    // As the page style pass does: no ancestor, so the viewport.
                    let style_of = |_: NodeId| None;
                    ctx.container = Some(cq_basis(&state.arena, &state.container_sizes, &style_of, dom, n, media));
                }
                let attr = dom.style_for_cascade(n).unwrap_or("");
                let key = cascade_memoised(
                    &mut state.cascade_memo, &state.sheets, &matched, attr, &crate::style::hints::for_element(dom, n), None,
                    ParentFormatting::Flow, &ctx, &mut state.arena, &[], ElementFacts::of(dom, n),
                );
                if let Some(px) = crate::style::compute::rem_basis(state.arena.inherited(key.inherited)) {
                    ctx.root_font_size = px;
                }
                keys[n as usize] = Some(key);
                formatting[n as usize] =
                    ParentFormatting::of_children(&state.arena, key, ParentFormatting::Flow);
            }
        }
        // A shadow root's children inherit from its host.
        // The FLAT tree's parent, as the page style pass cascades: a shadow tree's top element
        // inherits from its host, a slotted element from its SLOT (it was the host's light parent here), and a light
        // child no slot takes from nothing (it is not rendered: `Hidden`). Chrome 150 (`scratchpad/cqanim/probe6`):
        // a slotted child transitions `color` when its slot's color changes.
        let flat = std::cell::RefCell::new(crate::flat::FlatTree::default());
        let style_parent = |n: NodeId| flat.borrow_mut().parent(dom, n);
        for &candidate in candidates {
            chain.clear();
            let mut cur = Some(candidate);
            while let Some(n) = cur {
                if keys[n as usize].is_some() {
                    break;
                }
                if matches!(dom.node(n).kind, NodeKind::Element { .. }) {
                    chain.push(n);
                }
                cur = style_parent(n);
            }
            for node in chain.iter().rev().copied() {
                let parent = style_parent(node).and_then(|p| keys[p as usize]);
                // A rule inside an `@container` is answered as the page style pass answers it: from the
                // laid-out container sizes and the before-animation styles of the ancestors, cascaded above. With
                // no answer (`MatchEnv::default()`), no container rule held here, so a transition or an animation
                // one declares never ran, and the rescan's styles were not the page pass's, which is why a
                // container-query page could not rescan from what changed.
                let mark = {
                    let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                    let (arena, sizes, root_font_size) = (&state.arena, &state.container_sizes, ctx.root_font_size);
                    let containers =
                        |at: NodeId, cond: &ContainerCondition| container_holds(arena, sizes, &style_of, dom, at, cond, media, root_font_size);
                    let env = MatchEnv { containers: if container_rules { Some(&containers) } else { None }, ..MatchEnv::default() };
                    declarative_match(dm, matcher, style_reading, &state.sheets, dom, node, media, now, &env, &mut matched)
                };
                let attr = dom.style_for_cascade(node).unwrap_or("");
                let mine =
                    style_parent(node).map_or(ParentFormatting::Flow, |p| formatting[p as usize]);
                let facts = ElementFacts::of(dom, node);
                if cq_units {
                    // A `cq*` length resolves against the element's nearest size containers, as in the page style
                    // pass: their laid-out sizes and their types, from the styles cascaded above.
                    let style_of = |a: NodeId| keys.get(a as usize).copied().flatten();
                    ctx.container = Some(cq_basis(&state.arena, &state.container_sizes, &style_of, dom, node, media));
                }
                let root_font_size = ctx.root_font_size.to_bits();
                let container = ctx.container.map(|(w, h)| (w.to_bits(), h.to_bits()));
                let style_writes = dom.style_writes_of(node);
                let same = |c: &Cascaded| {
                    c.matched == mark
                        && c.parent == parent
                        && c.mine == mine
                        && c.facts == facts
                        && c.root_font_size == root_font_size
                        && c.container == container
                        && c.style_writes == style_writes
                };
                let key = match dm.cascaded.get(&node).filter(|c| same(c)) {
                    Some(c) => c.key,
                    None => {
                        perf_counters::add(&perf_counters::DECLARATIVE_CASCADES, 1);
                        let key = cascade_memoised(
                            &mut state.cascade_memo, &state.sheets, &matched, attr, &crate::style::hints::for_element(dom, node), parent, mine, &ctx,
                            &mut state.arena, &[], facts,
                        );
                        dm.cascaded.insert(
                            node,
                            Cascaded { matched: mark, parent, mine, facts, root_font_size, container, style_writes, key },
                        );
                        key
                    }
                };
                keys[node as usize] = Some(key);
                formatting[node as usize] = ParentFormatting::of_children(&state.arena, key, mine);
            }
        }
        keys
    }
    /// The properties a script animation holds at the next scan for transitions: one played since the last
    /// tick, whose values [`Page::set_animated`] has not put on the cascade yet, still keeps a transition from starting
    /// on them (`style::declarative`'s `held_by_animation`). The caller replaces the set every tick it has one.
    pub fn set_transition_blockers(&self, blockers: std::collections::HashSet<(NodeId, PropertyId)>) {
        self.state.lock().unwrap().declarative.set_blockers(blockers);
    }

    /// Replace what the declarative animations are putting on the cascade.
    ///
    /// Whole-map, not incremental: the caller recomputes every live animation's values each tick,
    /// and a partial update would leave a finished animation's last value on the element for ever.
    pub fn set_animated(&self, values: std::collections::HashMap<NodeId, Vec<AnimatedValue>>) {
        let mut state = self.state.lock().unwrap();
        if state.declarative.animated_differs(&values) {
            state.animated_generation = state.animated_generation.wrapping_add(1);
            // Only a LAYOUT-affecting change bumps the revision the layout cache is keyed on. An
            // opacity or transform animation moves no box, and bumping for one would rebuild the
            // whole tree on every observation while it ran.
            if state.declarative.layout_affecting_change(&values) {
                state.animation_revision = state.animation_revision.wrapping_add(1);
            }
            // Each kept document cascades them again at its next pass: the page's and a
            // frame's passes each read the list, so each has its own.
            let moved = state.declarative.moved(&values);
            for doc in state.doc_styles.values_mut() {
                doc.animated.extend_from_slice(&moved);
            }
            state.declarative.set_animated(values);
            // AND THE CASCADE MEMO, WHICH `animation_revision` DOES NOT COVER. The line above is
            // deliberately selective -- a paint-only animation must not rebuild the box tree -- but
            // an animated `opacity` IS what `getComputedStyle` reports, so a memo keyed on
            // `layout_fingerprint` alone would keep answering the pre-animation value. Two
            // korvus-page tests caught exactly that (`an_animated_value_is_what_the_computed_style_
            // reports`, `an_animation_loses_to_important_and_a_transition_beats_it`), which is why
            // this is the invariant `style_memo` names: the map is valid only while every cascade
            // INPUT is unchanged, and the overrides are an input the fingerprint cannot see.
            state.style_memo = (u64::MAX, std::collections::HashMap::new());
        }
    }

    /// What every box's `transform`, `opacity` and clip depend on, in one number (D77; `effects::BoxEffects` is read
    /// again when it moves): the page's geometry fingerprint `geometry` ([`Page::page_geometry_fingerprint`]) -- every
    /// write a box or a cascade input can see, sheets, layout-moving animations -- and what that one leaves out on
    /// purpose: the page's `style` rewrites that changed only the transform family and `opacity`
    /// ([`Dom::inert_style_writes`]) and every change of the animated values, paint-only ones included.
    ///
    /// It was [`Page::layout_fingerprint`], which counts EVERY DOM write: a `data-*` attribute nothing reads, then a
    /// geometry read, re-read the effect properties of every box (`box_effects`, each box's style looked up again) --
    /// 0.5-1.4 ms a write-and-read pair on a 1,500-element page where Chrome 152 takes 0.5 us (2026-10-23).
    pub fn effects_fingerprint(&self, dom: &Dom, geometry: u64) -> u64 {
        let generation = self.state.lock().unwrap().animated_generation;
        geometry ^ dom.inert_style_writes().wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ generation.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    /// Remember where a running transition is heading, so a cascade that moves somewhere else can
    /// cancel it instead of fighting it.
    pub fn note_transition_target(&self, node: NodeId, property: PropertyId, to: String) {
        self.state.lock().unwrap().declarative.note_transition_target(node, property, to);
    }

    pub fn forget_transition(&self, node: NodeId, property: PropertyId) {
        self.state.lock().unwrap().declarative.forget_transition(node, property);
    }

    /// Forget every before-change style and every override. A navigation reuses node ids for
    /// different elements, so a stale snapshot would invent transitions out of the new document's
    /// very first style.
    pub fn clear_animations(&self) {
        let mut state = self.state.lock().unwrap();
        let moved: Vec<NodeId> = state.declarative.animated_nodes().collect();
        for doc in state.doc_styles.values_mut() {
            doc.animated.extend_from_slice(&moved);
        }
        state.declarative.clear();
        state.last_styled = 0;
        state.declarative_scan.valid = false;
        state.last_animations = Default::default();
        // The overrides are gone, so every memoised key that carried one is wrong. See
        // `set_animated` for why the fingerprint cannot be trusted to notice.
        state.style_memo = (u64::MAX, std::collections::HashMap::new());
    }

    /// Everything layout depends on, in one number — the key a caller caches a [`LaidOut`] under.
    ///
    /// The cache lives with the caller and not here, because a laid-out document is not `Sync`:
    /// taffy's `Style` carries a raw pointer for `calc()` values, so it cannot cross a thread and
    /// cannot sit in a service on the kernel `Context`. That is the right place for it anyway —
    /// one V8 isolate owns one thread and one document, which is exactly the scope a layout cache
    /// is valid for.
    ///
    /// This is two integers, not a walk of the document. It used to hash every element's tag,
    /// `class`, `style`, and `id`, which is O(nodes) — and since a page calls
    /// `getBoundingClientRect` once per element, that made the *check* quadratic even though the
    /// layout it protects was cached. Measured on Wikipedia's 6,268 elements: 15 seconds. The DOM's
    /// own revision counter answers the same question in a compare (design D9: a global dirty flag
    /// is enough), and it is *more* correct than the hash was — the hash saw only three attributes,
    /// so a `hidden` attribute or a text change slipped past it.
    pub fn layout_fingerprint(&self, dom: &Dom, media: &MediaContext) -> u64 {
        Self::layout_fingerprint_of(&self.state.lock().unwrap(), dom, media)
    }

    /// What a document's GEOMETRY depends on: [`Page::layout_fingerprint`] with the DOM
    /// counted by [`Dom::render_revision`], which a node made or changed outside every document does
    /// not move, nor does an attribute write that leaves the value as it was; less the `style` rewrites
    /// that changed only what no box depends on; and told apart from another document by
    /// [`Dom::instance`] rather than by the arena's length (which a node being made moves). The geometry queries' layout cache is keyed on this.
    /// The per-element style memo stays on `layout_fingerprint`: `getComputedStyle` answers for a
    /// detached element too, and its answer has to follow that element's own changes.
    pub fn geometry_fingerprint(&self, dom: &Dom, media: &MediaContext) -> u64 {
        self.geometry_fingerprint_of(dom, media, Keyed::Arena)
    }

    /// [`Page::geometry_fingerprint`] for the PAGE's own layout: a change to another
    /// document of the arena -- a frame's, which a frame's own layout is keyed on -- leaves it where it was
    /// ([`Dom::page_render_revision`]).
    pub fn page_geometry_fingerprint(&self, dom: &Dom, media: &MediaContext) -> u64 {
        self.geometry_fingerprint_of(dom, media, Keyed::Page)
    }

    /// [`Page::geometry_fingerprint`] for the layout of the document rooted at `root`: the page's
    /// is [`Page::page_geometry_fingerprint`]; a frame document's moves with its own document's changes
    /// ([`Dom::document_render_revision`]), its viewport and the sheets -- not with the page's DOM nor with
    /// another frame's, none of which lays it out differently.
    pub fn document_geometry_fingerprint(&self, dom: &Dom, media: &MediaContext, root: NodeId) -> u64 {
        if root == dom.root() {
            return self.page_geometry_fingerprint(dom, media);
        }
        self.geometry_fingerprint_of(dom, media, Keyed::Document(root))
    }

    fn geometry_fingerprint_of(&self, dom: &Dom, media: &MediaContext, keyed: Keyed) -> u64 {
        use std::hash::{Hash, Hasher};
        // The sheets first, as `computed_style` does: the layout syncs them itself, and a number taken
        // before that sync differs from every number taken after it, which cost each document a second
        // layout after its first.
        self.sync_document_sheets(dom);
        let mut state = self.state.lock().unwrap();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // A `style` rewrite that changed only the transform family and `opacity` moves no box
        // ([`Dom::inert_style_writes`]) -- unless a selector reads the attribute itself. That is a
        // question about THIS sheet set, answered from its `style_reading` (made here when the layout
        // pass has not made it yet): answered from whether that set happened to exist, it flipped after
        // each sheet change and cost a second layout for every one. The mode is hashed too: the two
        // counts are different numbers, and one taken in either mode must never equal one taken in the
        // other by chance.
        if state.style_reading.as_ref().map(|(rev, _)| *rev) != Some(state.sheet_revision) {
            let reading = StyleReading::of(&state.sheets, &state.scopes, &state.adoptions);
            state.style_reading = Some((state.sheet_revision, reading));
        }
        let style_is_read = state.style_reading.as_ref().is_some_and(|(_, r)| r.reads.style);
        style_is_read.hash(&mut h);
        match keyed {
            // The inert-write discount is the page's: `inert_style_writes` counts only the page's writes.
            Keyed::Document(root) => dom.document_render_revision(root).hash(&mut h),
            Keyed::Page | Keyed::Arena => {
                let revision = if matches!(keyed, Keyed::Page) { dom.page_render_revision() } else { dom.render_revision() };
                if style_is_read {
                    revision.hash(&mut h);
                } else {
                    (revision - dom.inert_style_writes()).hash(&mut h);
                }
            }
        }
        dom.instance().hash(&mut h);
        state.sheet_revision.hash(&mut h);
        media.viewport_width.to_bits().hash(&mut h);
        media.viewport_height.to_bits().hash(&mut h);
        state.container_generation.hash(&mut h);
        state.animation_revision.hash(&mut h);
        state.resources_gen().hash(&mut h);
        let t = &media.theme;
        for v in [t.scrollbar_thickness, t.scrollbar_thin_thickness, t.menu_list_inline_padding, t.menu_list_block_padding] {
            v.to_bits().hash(&mut h);
        }
        h.finish()
    }

    /// [`Page::layout_fingerprint`] over a state the caller has already locked -- the one
    /// definition, so a path that holds the lock cannot compute a different number.
    fn layout_fingerprint_of(state: &PageState, dom: &Dom, media: &MediaContext) -> u64 {
        use std::hash::{Hash, Hasher};
        let t = &media.theme;
        // Every input the hash below reads. Compared instead of re-hashed on a repeat read.
        let inputs: FpInputs = (
            dom.revision(),
            dom.len() as u64,
            state.sheet_revision,
            media.viewport_width.to_bits(),
            media.viewport_height.to_bits(),
            state.container_generation,
            state.animation_revision,
            state.resources_gen(),
            t.scrollbar_thickness.to_bits(),
            t.scrollbar_thin_thickness.to_bits(),
            t.menu_list_inline_padding.to_bits(),
            t.menu_list_block_padding.to_bits(),
        );
        if let Some((cached, fp)) = state.fp_cache.get() {
            if cached == inputs {
                return fp;
            }
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Everything the CASCADE depends on, plus the one thing only layout does: the values the
        // running animations are contributing. An animated `width` has to move the box, and the
        // caller's layout cache is keyed on this number.
        Self::cascade_fingerprint_of(state, dom, media).hash(&mut h);
        state.animation_revision.hash(&mut h);
        // An image or a web font that arrived moves its box.
        state.resources_gen().hash(&mut h);
        // The platform theme moves boxes too (a classic scrollbar narrows every scroll container)
        // and the cascade never reads it, so it is here and not in the cascade's number.
        for v in [t.scrollbar_thickness, t.scrollbar_thin_thickness, t.menu_list_inline_padding, t.menu_list_block_padding] {
            v.to_bits().hash(&mut h);
        }
        let fp = h.finish();
        state.fp_cache.set(Some((inputs, fp)));
        fp
    }

    /// Everything the CASCADE depends on, in one number. `layout_fingerprint` is this plus the
    /// animation revision; `declarative_update`'s rescan gate is this one, because a running
    /// animation changes no cascade input and must not cost a style pass per frame.
    fn cascade_fingerprint(&self, dom: &Dom, media: &MediaContext) -> u64 {
        Self::cascade_fingerprint_of(&self.state.lock().unwrap(), dom, media)
    }

    fn cascade_fingerprint_of(state: &PageState, dom: &Dom, media: &MediaContext) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // The revision identifies mutations *within* one document; `len` separates two documents
        // that have each been mutated the same number of times. A navigation also drops the
        // caller's cache outright, which is the belt to this brace.
        dom.revision().hash(&mut h);
        dom.len().hash(&mut h);
        state.sheet_revision.hash(&mut h);
        media.viewport_width.to_bits().hash(&mut h);
        media.viewport_height.to_bits().hash(&mut h);
        // The container-query sizes are a cascade input.
        state.container_generation.hash(&mut h);
        h.finish()
    }

    /// Everything a LAYOUT depends on except the container-query sizes it itself produces: when this
    /// has not moved since the sizes were taken, they are current.
    fn container_basis_of(state: &PageState, dom: &Dom, media: &MediaContext) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // What the page's layout is keyed on (`geometry_fingerprint_of`, `Keyed::Page`): `Dom::revision` moves on
        // every mutating call, a no-op one and one in a node outside every document included, and each move
        // settled the containers again with a whole layout -- half of www.wayfair.com's (1294 revisions to 666
        // that rendered).
        (dom.page_render_revision() - dom.inert_style_writes()).hash(&mut h);
        dom.instance().hash(&mut h);
        state.sheet_revision.hash(&mut h);
        state.animation_revision.hash(&mut h);
        media.viewport_width.to_bits().hash(&mut h);
        media.viewport_height.to_bits().hash(&mut h);
        let t = &media.theme;
        for v in [t.scrollbar_thickness, t.scrollbar_thin_thickness, t.menu_list_inline_padding, t.menu_list_block_padding] {
            v.to_bits().hash(&mut h);
        }
        h.finish()
    }

    /// Build the rule index if a sheet change dropped it, and derive what goes with it: the
    /// `@property` registry the arena types custom properties against, and whether any rule is under
    /// `@container`.
    fn ensure_matcher(state: &mut PageState) {
        if state.matcher.is_some() {
            return;
        }
        state.matcher = Some(Matcher::build_scoped(&state.sheets, &state.scopes, &state.adoptions, |_, _| true));
        let mut registry = crate::style::registered::PropertyRegistry::default();
        for sheet in state.sheets.iter().filter(|s| !s.disabled) {
            for (name, reg) in sheet.registrations() {
                registry.insert(name.clone(), reg.clone());
            }
        }
        for (name, reg) in &state.script_registrations {
            registry.insert(name.clone(), reg.clone());
        }
        if *state.arena.registry != registry {
            state.arena.registry = std::sync::Arc::new(registry);
        }
        state.uses_cq_units = state.sheets.iter().any(|s| {
            !s.disabled && ["cqw", "cqh", "cqi", "cqb", "cqmin", "cqmax"].iter().any(|u| s.text().contains(u))
        });
        state.has_container_rules =
            state.uses_cq_units || state.sheets.iter().any(|s| !s.disabled && s.has_container_rules());
    }

    /// Does any enabled rule style the `name` pseudo-element? A document with none pays nothing for
    /// asking about it.
    pub fn styles_pseudo(&self, name: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        Self::ensure_matcher(&mut state);
        state.matcher.as_ref().is_some_and(|m| m.has_pseudo(name))
    }

    /// How many documents [`Page::layout_document`] has laid out, the page's and every frame's.
    /// A count, for the test that a page with no frames lays out nothing extra.
    pub fn layouts_run(&self) -> u64 {
        self.state.lock().unwrap().layouts_run
    }

    /// How many extra style-and-layout passes `@container` queries have cost.
    /// Would a style read on the page settle its container-query sizes first -- has the page `@container` rules, and
    /// has what their sizes rest on moved since a layout recorded them? A caller that keeps a layout of the page's own
    /// lays it out there instead (with what did not change carried over), and the settle then finds
    /// the sizes current.
    pub fn containers_stale(&self, dom: &Dom, media: &MediaContext) -> bool {
        let mut state = self.state.lock().unwrap();
        Self::ensure_matcher(&mut state);
        state.has_container_rules && state.container_basis != Some(Self::container_basis_of(&state, dom, media))
    }

    /// The counters a geometry fingerprint ([`Page::page_geometry_fingerprint`]) reads from this page: the sheet
    /// revision, the container-size generation and the animation revision, under one lock. With them, the DOM's
    /// revision, instance and document mode, the media and the natural-image generation all as they were when a
    /// fingerprint was taken -- and the sheets synced then -- that fingerprint is what taking it again gives: a caller
    /// may keep it rather than take it per read. The fourth is the animated values' generation, which the
    /// [`Page::effects_fingerprint`] reads (D77): a paint-only sample -- a running `transform` -- moves it alone.
    pub fn geometry_counters(&self) -> (u64, u64, u64, u64) {
        let state = self.state.lock().unwrap();
        (state.sheet_revision, state.container_generation, state.animation_revision, state.animated_generation)
    }

    /// Is `KORVUS_VERIFY_INCREMENTAL` on? A caller that keeps something derived from this page checks it then.
    pub fn verifying() -> bool {
        verify::enabled()
    }

    /// Report what a caller's check under `KORVUS_VERIFY_INCREMENTAL` found to differ.
    pub fn verify_mismatch(what: &str) {
        verify::started();
        verify::mismatch(what);
    }

    /// How many times the `<style>` walk visited the whole arena.
    #[cfg(test)]
    fn sheet_walks(&self) -> u64 {
        self.state.lock().unwrap().sheet_walks
    }

    /// How many boxes layouts carried over from the layout before instead of laying them out.
    pub fn boxes_carried(&self) -> u64 {
        self.state.lock().unwrap().boxes_carried
    }

    /// How many box layouts the reusing layouts computed, summed: what carrying and the caches leave to do.
    pub fn box_layouts_computed(&self) -> u64 {
        self.state.lock().unwrap().box_layouts_computed
    }

    /// `(box trees built, layouts that patched the last tree in place)`, by the reusing layouts.
    pub fn trees_built_and_patched(&self) -> (u64, u64) {
        let state = self.state.lock().unwrap();
        (state.trees_built, state.trees_patched)
    }

    pub fn container_passes(&self) -> u64 {
        self.state.lock().unwrap().container_passes
    }
}

/// A laid-out document. Geometry is asked of this rather than of [`Page`], so a caller that needs
/// several measurements pays for one layout — which is what a page measuring a list of elements in
/// a loop does.
pub struct LaidOut {
    tree: BoxTree,
    /// What has been scrolled, as the caller last said. Every CLIENT-coordinate answer
    /// below reads it; the document-coordinate ones do not. See [`ScrollSnapshot`].
    scroll: ScrollSnapshot,
}

impl LaidOut {
    /// Tell the layout what has been scrolled since it was laid out. A scroll moves no box, so it
    /// costs no layout: the caller keeps one `LaidOut` per DOM revision and updates this before
    /// asking a client-coordinate question.
    pub fn set_scroll(&mut self, scroll: ScrollSnapshot) {
        self.scroll = scroll;
    }

    /// The fingerprint of the style the kept `transform`/`opacity` were read from (D77), `None` before any.
    pub fn effects_key(&self) -> Option<u64> {
        self.tree.effects_key()
    }

    /// Keep what each box's style does beyond layout (`Page::box_effects`), read from the style whose fingerprint
    /// is `key`: the paint walk, hit testing and the client rects all read it.
    pub fn set_effects(&mut self, key: u64, effects: effects::BoxEffects) {
        self.tree.set_effects(key, effects);
    }

    /// The border box (the union of the fragments, for an inline box) in DOCUMENT coordinates -- where
    /// layout put it, before anything was scrolled. An element that generates no box has an all-zero
    /// rect. `getBoundingClientRect` is [`LaidOut::viewport_bounding_rect`].
    pub fn bounding_rect(&self, node: NodeId) -> Rect {
        self.tree.box_of(node).map_or(Rect::ZERO, |b| self.tree.bounding_rect(b))
    }

    /// `getBoundingClientRect`: the box in CLIENT coordinates, after every scroll above it. An element
    /// that generates no box gives four zeros, as a browser does for `display: none`.
    pub fn viewport_bounding_rect(&self, node: NodeId) -> Rect {
        self.tree.box_of(node).map_or(Rect::ZERO, |b| self.tree.viewport_bounding_rect(b, &self.scroll))
    }

    /// `getClientRects`: one rectangle per fragment, in CLIENT coordinates. Empty for no box.
    pub fn viewport_client_rects(&self, node: NodeId) -> Vec<Rect> {
        self.tree.box_of(node).map(|b| self.tree.viewport_client_rects(b, &self.scroll)).unwrap_or_default()
    }

    /// The padding box less its scrollbars, in CLIENT coordinates -- what a scroll container shows its
    /// content through. `None` for an element with no box.
    pub fn viewport_padding_box(&self, node: NodeId) -> Option<Rect> {
        self.tree.box_of(node).map(|b| self.tree.viewport_padding_box(b, &self.scroll))
    }

    /// The padding box less its scrollbars, in DOCUMENT coordinates. `None` for no box.
    pub fn padding_box(&self, node: NodeId) -> Option<Rect> {
        self.tree.box_of(node).map(|b| self.tree.padding_box(b))
    }

    /// The content box -- the padding box less its padding -- in DOCUMENT coordinates. `None` for no
    /// box. What a `ResizeObserver` reports as `contentRect` (offset by the padding) and what a frame's
    /// document is laid out into.
    pub fn content_box(&self, node: NodeId) -> Option<Rect> {
        self.tree.box_of(node).map(|b| self.tree.content_box(b))
    }

    /// The used value of a resolved-value property, or `None` when this element generated no box.
    ///
    /// `None` is the browser's answer too: `getComputedStyle` on a `display: none` element reports
    /// `width: 50%`, because there is no used width for a box that was never laid out.
    pub fn resolved_value(&self, node: NodeId, property: PropertyId, computed: &str) -> Option<String> {
        let index = self.tree.box_of(node)?;
        self.tree.resolved_value(index, property, computed)
    }

    /// [`LaidOut::resolved_value`] for a `::before`/`::after` box. `None` for any other
    /// pseudo-element, and for one that generated no box -- whose computed value then stands, as
    /// Chrome's does: `getComputedStyle(el, "::after").width` is `auto` with no `::after` rule.
    pub fn resolved_pseudo_value(&self, node: NodeId, pseudo: &str, property: PropertyId, computed: &str) -> Option<String> {
        let after = match pseudo {
            "before" => false,
            "after" => true,
            _ => return None,
        };
        let index = self.tree.pseudo_box(node, after)?;
        self.tree.resolved_value(index, property, computed)
    }

    /// The `::before`/`::after` boxes this layout generated, for `innerText`'s
    /// white-space model. See [`crate::layout::tree::BoxTree::generated_boxes`].
    pub fn generated_boxes(&self) -> impl Iterator<Item = (NodeId, bool, &str, crate::layout::style::BoxLevel, bool, bool)> + '_ {
        self.tree.generated_boxes()
    }

    /// The elements whose `content-visibility: auto` contents this layout skipped -- off the viewport
    /// by more than Blink's margin. See [`crate::layout::tree::BoxTree::skipped_auto_nodes`].
    pub fn skipped_auto_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.tree.skipped_auto_nodes()
    }

    /// One rectangle per fragment, in DOCUMENT coordinates. Empty for an element with no box.
    /// `getClientRects` is [`LaidOut::viewport_client_rects`].
    pub fn client_rects(&self, node: NodeId) -> Vec<Rect> {
        self.tree.box_of(node).map(|b| self.tree.client_rects(b)).unwrap_or_default()
    }

    /// `offsetWidth` / `offsetHeight`.
    pub fn offset_size(&self, node: NodeId) -> (i32, i32) {
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.offset_size(b))
    }

    /// `offsetLeft` / `offsetTop`.
    pub fn offset_position(&self, node: NodeId) -> (i32, i32) {
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.offset_position(b))
    }

    /// Does the element's box carry a transform (`transform` not `none`) that applies to it? `false`
    /// with no box, and for a non-replaced inline box, which a transform does not apply to (CSS
    /// Transforms 1 §2) -- an inline `<svg>` or `<img>` is an atomic inline and does transform.
    pub fn transforms(&self, node: NodeId) -> bool {
        self.tree.box_of(node).is_some_and(|b| self.tree.box_transformed(b) && self.tree.box_level(b) != crate::layout::style::BoxLevel::Inline)
    }

    /// Is the element a non-atomic inline box (`display: inline`) -- geometry as line-box fragments placed in its
    /// container's coordinates, with no box of its own? `false` with no box.
    pub fn is_inline_box(&self, node: NodeId) -> bool {
        self.tree.box_of(node).is_some_and(|b| self.tree.box_level(b) == crate::layout::style::BoxLevel::Inline)
    }

    /// The used `position` of the element's box, or `None` when it generates none.
    pub fn position_of(&self, node: NodeId) -> Option<crate::layout::style::CssPosition> {
        self.tree.box_of(node).map(|b| self.tree.css_position(b))
    }

    /// `offsetLeft` / `offsetTop` against the element `offsetParent` answers (`None`: null).
    pub fn offset_position_from(&self, node: NodeId, offset_parent: Option<NodeId>) -> (i32, i32) {
        let parent = offset_parent.and_then(|p| self.tree.box_of(p));
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.offset_position_from(b, parent))
    }

    /// `clientWidth` / `clientHeight`.
    pub fn client_size(&self, node: NodeId) -> (i32, i32) {
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.client_size(b))
    }

    /// `clientTop` / `clientLeft` — the top and left border widths.
    pub fn border_widths(&self, node: NodeId) -> (i32, i32) {
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.border_widths(b))
    }

    /// `scrollWidth` / `scrollHeight`.
    ///
    /// `(0, 0)` for an element that generated no box, which is what Chrome answers for a detached
    /// one: `document.createElement("div").scrollHeight` is 0 there and here.
    pub fn scroll_size(&self, node: NodeId) -> (i32, i32) {
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.scroll_size(b))
    }

    /// How far this element can be scrolled — `scrollWidth - clientWidth`, `scrollHeight -
    /// clientHeight`, floored at zero, and zero for a box that does not scroll at all.
    pub fn scroll_extent(&self, node: NodeId) -> (i32, i32) {
        self.tree.box_of(node).map_or((0, 0), |b| self.tree.scroll_extent(b))
    }

    /// How far the DOCUMENT can be scrolled — what `window.scrollTo(0, 2e6)` lands on.
    ///
    /// The root box's own extent, because the viewport's scrolling box *is* the root element's: in a
    /// no-quirks document Chrome reports the same number through `documentElement.scrollTop` and
    /// through `window.scrollY`, and moving either moves the other (measured, Chrome 150, 2026-09-13
    /// — `d.scrollTop = 2e6` left `scrollY` at 3307 and `scrollTo(0, 2e6)` left `d.scrollTop` at
    /// 3307 on the same page). `(0, 0)` for a document with no boxes at all, which is a document
    /// with nothing to scroll.
    pub fn document_scroll_extent(&self) -> (i32, i32) {
        self.tree.root_box().map_or((0, 0), |b| self.tree.scroll_extent(b))
    }

    /// `document.caretPositionFromPoint` at a CLIENT point: a node and an offset into it, and the
    /// caret's rectangle. `None` outside the viewport. See [`crate::layout::caret`].
    pub fn caret_position(&self, dom: &Dom, x: f32, y: f32) -> Option<crate::layout::caret::Caret> {
        self.tree.caret_position(dom, x, y, &self.scroll)
    }

    /// A `Range`'s rects for text node `node` from UTF-16 offset `start` to `end`, in CLIENT coordinates: one per line
    /// ([`BoxTree::text_range_rects`]).
    pub fn text_range_rects(&self, dom: &Dom, node: NodeId, start: u32, end: u32) -> Vec<Rect> {
        self.tree.text_range_rects(dom, node, start, end, &self.scroll)
    }

    /// `document.elementFromPoint` at a CLIENT point: the topmost element there, or the document
    /// element for a point in the viewport that no box covers. `None` outside the viewport.
    pub fn element_from_point(&self, x: f32, y: f32) -> Option<NodeId> {
        self.tree.elements_from_point_for_document(x, y, &self.scroll).into_iter().next()
    }

    /// `document.elementsFromPoint` — the whole stack at a point, topmost first.
    pub fn elements_from_point(&self, x: f32, y: f32) -> Vec<NodeId> {
        self.tree.elements_from_point_for_document(x, y, &self.scroll)
    }

    /// The hover chain for a viewport point — the element under it and its element ancestors,
    /// outermost first. Hand this to `Dom::set_hover_chain` and `:hover` follows the pointer.
    /// See [`BoxTree::hover_chain_at`] for why it is an ancestor walk and not the box stack.
    pub fn hover_chain_at(&self, dom: &Dom, x: f32, y: f32) -> Vec<NodeId> {
        self.tree.hover_chain_at(dom, x, y, &self.scroll)
    }

    /// How many boxes the document generated, and how many distinct layout styles they needed.
    pub fn stats(&self) -> (usize, usize) {
        (self.tree.len(), self.tree.distinct_styles())
    }

    /// What the layout that made this did ([`crate::layout::tree::LayoutStats`]).
    pub fn layout_stats(&self) -> crate::layout::tree::LayoutStats {
        self.tree.stats()
    }
}

/// The page service as it lives on the kernel [`Context`]. `Arc` so the `Context` shares it cheaply.
pub struct PageHandle(pub Arc<Page>);

/// Registers the page pipeline as the `page` service.
pub struct PagePlugin;

impl Plugin for PagePlugin {
    fn name(&self) -> &str {
        "page"
    }
    fn provides(&self) -> &[&str] {
        &["page"]
    }
    fn apply(&self, ctx: &Context, _config: &PluginConfig) -> spec::kernel::Result<()> {
        ctx.provide(PageHandle(Arc::new(Page::default())));
        // The CSS value layer is its own service because a consumer that only wants to measure
        // text or parse a colour -- the canvas -- should not have to know what a stylesheet is.
        ctx.provide(spec::CssHandle(Arc::new(font::BuiltinFonts)));
        // The document compositor is the page's service (it paints the box tree this crate
        // lays out), read at the rendering opportunity via `ctx.get::<CompositorHandle>()`. The default
        // is `NullCompositor` -- no paint, Korvus's pre-RC0 behaviour and the zero-cost default (goal
        // R5). A persona/bundle swaps in a real one later (R4); RC1 implements it
        // here in `korvus-page`, rastering via `RenderHandle`'s Skia (R3).
        ctx.provide(spec::CompositorHandle(Arc::new(spec::NullCompositor)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::kernel::Loader;

    // The container-query loop is COUNT-tested: a query settles in one extra pass, an
    // unchanged document costs none, and a query that keeps flipping its own container's size stops at
    // `CONTAINER_PASSES` rather than looping.
    #[test]
    fn the_container_query_loop_settles_once_costs_nothing_again_and_is_bounded() {
        let page = Page::default();
        let dom = Dom::parse(
            "<!doctype html><style>.c { container-type: inline-size; width: 300px } \
             @container (min-width: 200px) { .i { height: 7px } }</style>\
             <body><div class=c><div class=i id=i></div></div></body>",
        );
        let media = MediaContext::default();
        let out = page.layout(&dom, &media);
        let i = (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "id") == Some("i")).unwrap();
        assert_eq!(out.offset_size(i).1, 7, "the query answered against the laid-out container");
        assert_eq!(page.container_passes(), 1, "sizes unknown, then known: one extra pass");
        let _ = page.layout(&dom, &media);
        assert_eq!(page.container_passes(), 1, "nothing moved, so no extra pass");

        // A shrink-wrapping container whose size is decided by the child the query styles: 150 -> 50
        // -> 150 -> ... forever, if nothing bounded it.
        let page = Page::default();
        let dom = Dom::parse(
            "<!doctype html><style>.c { container-type: inline-size; display: inline-block } \
             .i { width: 150px; height: 1px } @container (min-width: 100px) { .i { width: 50px } }</style>\
             <body><div class=c><div class=i></div></div></body>",
        );
        let _ = page.layout(&dom, &media);
        assert_eq!(page.container_passes(), CONTAINER_PASSES as u64, "bounded");
    }

    // The container settle's basis is the page's render revision, not `Dom::revision`. A
    // container whose size moves through a NON-style change -- a sibling's text in a flex row -- still answers
    // its query anew at the next style read.
    #[test]
    fn a_container_resized_by_a_text_change_answers_its_query_anew() {
        let page = Page::default();
        let mut dom = Dom::parse(
            "<!doctype html><style>.row { display: flex; width: 400px } .c { flex: 1; container-type: inline-size } \
             @container (max-width: 200px) { .p { color: rgb(255, 0, 0) } }</style>\
             <body><div class=row><span id=s>x</span><div class=c><p class=p id=p>t</p></div></div></body>",
        );
        let media = MediaContext::default();
        let find = |dom: &Dom, id: &str| (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "id") == Some(id)).unwrap();
        let (p, s) = (find(&dom, "p"), find(&dom, "s"));
        let red = Some("rgb(255, 0, 0)".to_string());
        assert_ne!(page.computed_value(&dom, p, &media, PropertyId::Color, None), red, "a ~390px container: the query does not hold");
        dom.set_text_content(s, &"w".repeat(40));
        assert_eq!(page.computed_value(&dom, p, &media, PropertyId::Color, None), red, "the sibling's text took the row: the container is narrow");
    }

    // A font probe -- an off-screen absolutely positioned span whose `font-family` a script rewrites,
    // reading its size after each -- lays out the span and the boxes above it; the rest of the document is carried
    // over from the layout before. And every answer is a fresh layout's.
    #[test]
    fn a_font_probe_lays_out_the_probe_and_carries_the_rest() {
        let page = Page::default();
        let rows: String = (0..300).map(|i| format!("<div class=r><span>row {i}</span> text <b>b</b></div>")).collect();
        let mut dom = Dom::parse(&format!(
            "<!doctype html><style>.r {{ display: flex; padding: 2px }} #p {{ position: absolute; left: -9999px; font-size: 72px }}</style>\
             <body>{rows}<span id=p>mmmmmmmmmmlli</span></body>"
        ));
        let media = MediaContext::default();
        let probe = element(&dom, "p");
        let mut laid = page.layout(&dom, &media);
        for family in ["monospace", "serif", "sans-serif", "No Such Font, monospace", "cursive"] {
            dom.set_attribute(probe, "style", &format!("font-family: {family}"));
            let before = page.boxes_carried();
            laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new font is a new layout");
            let carried = page.boxes_carried() - before;
            assert!(carried > 1500, "{family}: only {carried} boxes carried");
            let fresh = Page::default().layout(&dom, &media);
            assert_eq!(laid.bounding_rect(probe), fresh.bounding_rect(probe), "{family}: the probe's box is a fresh layout's");
            assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{family}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
        }
    }

    // A layout that carries what was built as before over from the last one is the layout made from
    // nothing, under random mutations -- classes, an absolutely positioned probe's font and text, inline styles, text,
    // inserted and removed boxes, `display: none` -- each laid out from the one before.
    #[test]
    fn carrying_what_was_built_as_before_equals_laying_out_everything_under_random_mutations() {
        const SHEET: &str = "<style>
            .f { display: flex } .g { display: grid; grid-template-columns: 1fr 2fr } .w { width: 120px } .n { display: none }
            .ib { display: inline-block; width: 30px } .big { font-size: 24px } .pad { padding: 3px 5px } .ab { position: absolute; left: 7px; top: 3px }
            .probe { position: absolute; left: -9999px; font-size: 72px } .wrap { white-space: nowrap } .em { margin: 1em }
            </style>";
        let classes = ["f", "g", "w", "n", "ib", "big", "pad", "ab", "wrap", "em", ""];
        let fonts = ["monospace", "serif", "sans-serif", "No Such Font, serif", "cursive"];
        let texts = ["x", "a longer run of words", "mmmmmmmm", "", "two words"];
        for seed in 1..=12u64 {
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = move |n: usize| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n as u64) as usize
            };
            let block = "<div class='pad'><span>one</span> two <b>three</b><div class='ib'>i</div></div>\
                <div class='f'><p>flex <i>item</i></p><p class='w'>w</p></div><div class='g'><div>a</div><div>b c</div></div>";
            let mut dom = Dom::parse(&format!(
                "<!doctype html><html><head>{SHEET}</head><body>{}<span class=probe id=probe>mmmmmmmmmmlli</span></body></html>",
                block.repeat(6)
            ));
            let page = Page::default();
            let media = MediaContext::default();
            let body = dom.get_elements_by_tag_name("body")[0];
            let probe = element(&dom, "probe");
            let root = dom.root();
            let mut elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n).is_some() && dom.root_of(n) == root && dom.parent(n).is_some_and(|p| p != root)).collect();
            let mut laid = page.layout(&dom, &media);
            for step in 0..60 {
                for _ in 0..1 + next(2) {
                    let target = elements[next(elements.len())];
                    match next(10) {
                        0..=2 => dom.set_attribute(probe, "style", &format!("font-family: {}", fonts[next(fonts.len())])),
                        3 => dom.set_text_content(probe, texts[next(texts.len())]),
                        4 | 5 => dom.set_attribute(target, "class", classes[next(classes.len())]),
                        6 => dom.set_attribute(target, "style", ["width: 40px", "font-family: serif", "", "display: inline"][next(4)]),
                        7 => {
                            let el = dom.create_element(["div", "span", "b"][next(3)]);
                            let t = dom.create_text_node(texts[next(texts.len())]);
                            dom.append_child(el, t);
                            dom.append_child(if next(3) == 0 { body } else { target }, el);
                            elements.push(el);
                        }
                        8 => {
                            if target != probe && target != body {
                                if let Some(p) = dom.parent(target) {
                                    dom.remove_child(p, target);
                                    elements.retain(|&e| e != target && dom.is_connected(e));
                                }
                            }
                        }
                        _ => dom.set_text_content(target, texts[next(texts.len())]),
                    }
                }
                if let Some(out) = page.relayout_document(&dom, dom.root(), &media, &laid) {
                    laid = out;
                }
                let fresh = Page::default().layout(&dom, &media);
                if let Some(d) = laid.tree.first_difference(&fresh.tree) {
                    panic!("seed {seed} step {step}: a carried layout is not the fresh one: {}", d.what);
                }
            }
            assert!(page.boxes_carried() > 1000, "seed {seed}: only {} boxes carried in 60 layouts", page.boxes_carried());
        }
    }

    /// A 15px classic-scrollbar theme in the default 1024x768 window: what a Windows persona lays out with.
    fn classic_bars() -> MediaContext {
        MediaContext { theme: spec::ThemeProfile { scrollbar_thickness: 15.0, ..Default::default() }, ..MediaContext::default() }
    }

    /// The randomized carry check under a classic viewport scrollbar, on a page taller than the window, with what
    /// the plain one has no room for: collapsing `<p>` margins, percentage padding, `overflow: auto` boxes and
    /// carousels that show their own bars, inline-blocks inside margin `<p>`s, column flexes and `aspect-ratio`.
    /// Every relayout -- carried from the one before, which is handed over as the browser's cache hands it -- must be
    /// the fresh layout. Returns the boxes carried and the box layouts computed.
    fn carry_under_classic_bars(seeds: std::ops::RangeInclusive<u64>, steps: usize) -> (u64, u64) {
        const SHEET: &str = "<style>
            .f { display: flex } .g { display: grid; grid-template-columns: 1fr 2fr } .w { width: 120px } .n { display: none }
            .ib { display: inline-block; width: 30px } .big { font-size: 24px } .pad { padding: 3px 5px } .ab { position: absolute; left: 7px; top: 3px }
            .probe { position: absolute; left: -9999px; font-size: 72px } .wrap { white-space: nowrap } .em { margin: 1em }
            .m { margin: 6px 0 } .pp { padding: 4% 2% } .ov { overflow: auto; height: 40px } .car { display: flex; overflow-x: auto }
            .car > div { flex: 0 0 200px } .ib2 { display: inline-block; padding: 2px } .col { display: flex; flex-direction: column }
            .ar { aspect-ratio: 2 }
            </style>";
        let classes = ["f", "g", "w", "n", "ib", "big", "pad", "ab", "wrap", "em", "m", "pp", "ov", "car", "col", "ar", ""];
        let fonts = ["monospace", "serif", "sans-serif", "No Such Font, serif", "cursive"];
        let texts = ["x", "a longer run of words", "mmmmmmmm", "", "two words"];
        let media = classic_bars();
        let (mut carried, mut computed) = (0, 0);
        for seed in seeds {
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = move |n: usize| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n as u64) as usize
            };
            let block = "<div class='pad'><span>one</span> two <b>three</b><div class='ib'>i</div></div>\
                <div class='f'><p class='m'>flex <span class='ib2'>$1</span> <i>item</i></p><p class='w'>w</p></div><div class='g'><div>a</div><div>b c</div></div>\
                <div class='pp'><p class='m'>price <span class='ib2'>$2</span> each</p><div class='ar'></div></div>\
                <div class='car'><div>c1</div><div class='pp'>c2</div><div>c3</div><div>c4</div><div>c5</div><div>c6</div></div>\
                <div class='col'><div class='ov'><p>o</p><p>o</p><p>o</p></div><p class='m'>col <span class='ib2'>$3</span></p></div>";
            let mut dom = Dom::parse(&format!(
                "<!doctype html><html><head>{SHEET}</head><body>{}<span class=probe id=probe>mmmmmmmmmmlli</span></body></html>",
                block.repeat(12)
            ));
            let page = Page::default();
            let body = dom.get_elements_by_tag_name("body")[0];
            let probe = element(&dom, "probe");
            let root = dom.root();
            let mut elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n).is_some() && dom.root_of(n) == root && dom.parent(n).is_some_and(|p| p != root)).collect();
            let mut laid = page.layout(&dom, &media);
            assert!(laid.tree.viewport().width < 1024.0, "seed {seed}: the page shows its viewport scrollbar");
            for step in 0..steps {
                for _ in 0..1 + next(2) {
                    let target = elements[next(elements.len())];
                    match next(10) {
                        0..=2 => dom.set_attribute(probe, "style", &format!("font-family: {}", fonts[next(fonts.len())])),
                        3 => dom.set_text_content(probe, texts[next(texts.len())]),
                        4 | 5 => dom.set_attribute(target, "class", classes[next(classes.len())]),
                        6 => dom.set_attribute(target, "style", ["width: 40px", "font-family: serif", "", "display: inline", "padding-left: 5%"][next(5)]),
                        7 => {
                            let el = dom.create_element(["div", "span", "b"][next(3)]);
                            let t = dom.create_text_node(texts[next(texts.len())]);
                            dom.append_child(el, t);
                            dom.append_child(if next(3) == 0 { body } else { target }, el);
                            elements.push(el);
                        }
                        8 => {
                            if target != probe && target != body {
                                if let Some(p) = dom.parent(target) {
                                    dom.remove_child(p, target);
                                    elements.retain(|&e| e != target && dom.is_connected(e));
                                }
                            }
                        }
                        _ => dom.set_text_content(target, texts[next(texts.len())]),
                    }
                }
                laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
                let fresh = Page::default().layout(&dom, &media);
                if let Some(d) = laid.tree.first_difference(&fresh.tree) {
                    panic!("seed {seed} step {step}: a carried layout is not the fresh one: {}", d.describe(|n| verify::describe(&dom, n)));
                }
            }
            carried += page.boxes_carried();
            computed += page.box_layouts_computed();
        }
        (carried, computed)
    }

    // A cell's `colspan` is a fact the box tree is BUILT with, not a style: a write moves no style key, and the
    // rebuilt tree has every box, node and origin of the last one. It must still be laid out -- the cell now spans
    // two columns -- where the kept-tree check, blind to the spans, had kept the old table.
    #[test]
    fn a_colspan_write_is_laid_out() {
        let page = Page::default();
        let mut dom = Dom::parse("<!doctype html><body><table><tr><td id=a>x</td><td>y</td></tr><tr><td id=b>z</td><td>w</td></tr></table></body>");
        let media = MediaContext::default();
        let a = element(&dom, "a");
        let laid = page.layout(&dom, &media);
        let before = laid.offset_size(a);
        dom.set_attribute(a, "colspan", "2");
        let laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a cell that spans two columns is a new layout");
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
        assert_ne!(laid.offset_size(a), before, "the cell is as wide as two columns now");
    }

    // A block inside a `<span>` splits it: its halves are laid out by the anonymous boxes on either side, and the
    // span's `parent` names only the first. Its percentage padding resolves against that box's width; when it moves,
    // BOTH boxes that lay the span out must forget what they measured with the old padding. The second half's text is
    // an `<i>`'s, so nothing else there carries the span's style: from the first layout on, the second box answered
    // its second pass from the first, where the padding was still 0 (an 8px fragment for the from-nothing pass's 68).
    #[test]
    fn a_split_inline_with_percentage_padding_lays_out_as_from_nothing() {
        verify::FORCED.with(|f| f.set(true));
        let page = Page::default();
        let mut dom = Dom::parse("<!doctype html><body><div id=o style='width:300px'><div><span id=s style='padding:0 10%'>a<div>b</div><i>c</i></span></div></div></body>");
        let media = MediaContext::default();
        let (o, sp) = (element(&dom, "o"), element(&dom, "s"));
        let mut laid = page.layout(&dom, &media);
        for width in ["width:200px", "width:420px", "width:300px"] {
            dom.set_attribute(o, "style", width);
            laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new width is a new layout");
            let fresh = Page::default().layout(&dom, &media);
            assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{width}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
            assert_eq!(laid.viewport_client_rects(sp), fresh.viewport_client_rects(sp), "{width}");
        }
        verify::FORCED.with(|f| f.set(false));
    }

    /// Lay `dom` out from nothing and relayout it from `previous` with the fast paths `off`, then the same with them
    /// on: both pairs must be the same layouts. Returns (computed with them on, computed with them off) for the fresh
    /// layouts, and the same for the relayouts.
    fn switch_on_equals_off(dom: &Dom, media: &MediaContext, page: &Page, previous: &LaidOut, off: &[crate::layout::tree::Knob]) -> ((u64, u64), (u64, u64)) {
        use crate::layout::tree::with_knobs;
        let fresh_on = Page::default().layout(dom, media);
        let fresh_off = with_knobs(off, || Page::default().layout(dom, media));
        if let Some(d) = fresh_on.tree.first_difference(&fresh_off.tree) {
            panic!("{off:?}: a fresh layout with the switch on is not one with it off: {}", d.describe(|n| verify::describe(dom, n)));
        }
        // Each relayout from its own copy of the previous layout's page state, so neither sees the other's.
        let relaid_on = page.relayout_document(dom, dom.root(), media, previous);
        let relaid_off = with_knobs(off, || page.relayout_document(dom, dom.root(), media, previous));
        let (on, off_tree) = match (&relaid_on, &relaid_off) {
            (Some(a), Some(b)) => (&a.tree, &b.tree),
            (None, None) => (&previous.tree, &previous.tree),
            _ => panic!("{off:?}: one relayout kept the layout and the other did not"),
        };
        if let Some(d) = on.first_difference(off_tree) {
            panic!("{off:?}: a relayout with the switch on is not one with it off: {}", d.describe(|n| verify::describe(dom, n)));
        }
        if let Some(d) = on.first_difference(&fresh_on.tree) {
            panic!("{off:?}: a relayout is not the fresh layout: {}", d.describe(|n| verify::describe(dom, n)));
        }
        ((fresh_on.layout_stats().computed, fresh_off.layout_stats().computed), (on.stats().computed, off_tree.stats().computed))
    }

    // A flex container whose cross size flips between definite and not -- measured, then laid out -- clears only its
    // REPLACED items' measurements, the only ones that read the flag. Under a classic scrollbar, on a page taller than
    // the window, under a percentage-padding ancestor: a column of carousels of column-flex tiles holding text, a
    // percentage-padding media box, a row with an `<img>`, a stretched `<canvas>` and an inline-block, and margin
    // `<p>`s holding inline-blocks. The switch off is the code before.
    #[test]
    fn a_flex_flip_keeps_its_text_items_measured() {
        use crate::layout::tree::Knob;
        let tile = "<div class='tile col'><div class=media><i></i></div><div class=row><img width=60 height=30><canvas width=60 height=30></canvas>\
                    <span class=ib>$9.99</span></div><p>Item text that wraps over lines</p><p>price <span class=ib>$1</span> each</p></div>";
        let car = format!("<div class=car>{}</div>", tile.repeat(8));
        let mut dom = Dom::parse(&format!(
            "<!doctype html><style>.col{{display:flex;flex-direction:column}} .row{{display:flex;align-items:stretch}} \
             .car{{display:flex;overflow-x:auto;gap:8px}} .tile{{flex:0 0 180px;padding:4px}} .media{{position:relative;padding-top:56.25%}} \
             .media i{{position:absolute;inset:0}} .ib{{display:inline-block;padding:2px}} p{{margin:6px 0}} canvas{{max-height:40px}}</style>\
             <body><div style='padding-top:5%'><div class=col>{}</div></div><p id=t>x</p></body>",
            car.repeat(20)
        ));
        let media = classic_bars();
        let page = Page::default();
        let laid = page.layout(&dom, &media);
        assert!(laid.tree.viewport().width < 1024.0, "the viewport scrollbar shows");
        let off = crate::layout::tree::with_knobs(&[Knob::FlexFlip], || Page::default().layout(&dom, &media));
        assert!(off.layout_stats().flex_item_clears > 10 * laid.layout_stats().flex_item_clears, "the tiles flip, and their text items stay measured");
        let t = element(&dom, "t");
        dom.set_text_content(t, "a longer text");
        let laid2 = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a text change is a new layout");
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid2.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid2.tree.first_difference(&fresh.tree).map(|d| d.what));
        dom.set_text_content(t, "x");
        let ((fresh_on, fresh_off), _) = switch_on_equals_off(&dom, &media, &page, &laid2, &[Knob::FlexFlip]);
        assert!(fresh_on < fresh_off, "fewer computations with the switch on: {fresh_on} against {fresh_off}");
    }

    // A measurement carrying margin-collapse state -- every margin card, `<p>` and heading has one -- is kept whole
    // for its exact input, where taffy's size-only slots refuse it and the box was measured again with its subtree at
    // every ancestor measurement. Under a classic scrollbar on a page taller than the window, under a percentage-
    // padding ancestor: flex rows of column-flex margin cards holding margin `<p>`s, one with an inline-block -- a
    // column flex measures its items at one input several times. The switch off is the code before; both must lay out
    // alike, fresh and carried (20,536 computations -> 16,936 fresh, 2026-10-23).
    #[test]
    fn margin_cards_in_flex_rows_match_head() {
        use crate::layout::tree::Knob;
        let card = "<div class=card><p>title text</p><p>price <span style='display:inline-block;padding:2px'>$1</span></p></div>";
        let row = format!("<div class=row>{}</div>", card.repeat(5));
        let mut dom = Dom::parse(&format!(
            "<!doctype html><style>.row{{display:flex}} .card{{margin:6px;display:flex;flex-direction:column}} p{{margin:4px 0}}</style>\
             <body><div style='padding-top:4%'>{}</div><p id=t>x</p></body>",
            row.repeat(60)
        ));
        let media = classic_bars();
        let page = Page::default();
        let laid = page.layout(&dom, &media);
        assert!(laid.tree.viewport().width < 1024.0, "the viewport scrollbar shows");
        let t = element(&dom, "t");
        dom.set_text_content(t, "a longer text");
        let ((fresh_on, fresh_off), (relaid_on, relaid_off)) = switch_on_equals_off(&dom, &media, &page, &laid, &[Knob::MarginCache]);
        assert!(fresh_on < fresh_off, "fewer computations with the switch on, fresh: {fresh_on} against {fresh_off}");
        assert!(relaid_on <= relaid_off, "and carried: {relaid_on} against {relaid_off}");
    }

    // After a write that moves only attributes the `<style>` walk visits the `<style>` and `<link>` elements alone --
    // and still applies what an attribute changes there: a `<style>`'s `media`, a `<link>` that becomes a stylesheet,
    // a `disabled` link. Every computed style is a fresh page's.
    #[test]
    fn an_attribute_write_syncs_sheets_without_walking_the_arena() {
        let mut dom = Dom::parse(
            "<!doctype html><style id=st media=print>#d { color: rgb(255, 0, 0) }</style><link id=l rel=preload href=x.css>\
             <body><div id=d class=a>d</div></body>",
        );
        let media = MediaContext::default();
        let page = Page::default();
        let (st, l, d) = (element(&dom, "st"), element(&dom, "l"), element(&dom, "d"));
        let same_as_fresh = |page: &Page, dom: &Dom, what: &str| {
            let fresh = Page::default();
            for n in 0..dom.len() as NodeId {
                if dom.tag_name(n).is_some() {
                    assert_eq!(
                        page.computed_value(dom, n, &media, PropertyId::Color, None),
                        fresh.computed_value(dom, n, &media, PropertyId::Color, None),
                        "{what}: {}", verify::describe(dom, n)
                    );
                }
            }
        };
        let color = |page: &Page, dom: &Dom| page.computed_value(dom, d, &media, PropertyId::Color, None);
        assert_eq!(color(&page, &dom).as_deref(), Some("rgb(0, 0, 0)"), "a print sheet does not apply");
        let walks = page.sheet_walks();
        dom.set_attribute(st, "media", "all");
        assert_eq!(color(&page, &dom).as_deref(), Some("rgb(255, 0, 0)"), "the media attribute applies the sheet");
        same_as_fresh(&page, &dom, "media");
        let sheets = page.sheet_count();
        dom.set_attribute(l, "rel", "stylesheet");
        let _ = color(&page, &dom);
        assert_eq!(page.sheet_count(), sheets + 1, "the link is a stylesheet now: it has its row");
        let link_sheet = page.sheet_for_owner(&dom, l);
        assert_eq!(page.with_sheet(link_sheet, |s| s.disabled), Some(false));
        dom.set_attribute(l, "disabled", "");
        let _ = color(&page, &dom);
        assert_eq!(page.with_sheet(link_sheet, |s| s.disabled), Some(true), "a disabled link styles nothing");
        same_as_fresh(&page, &dom, "disabled");
        dom.set_attribute(d, "class", "b");
        assert_eq!(color(&page, &dom).as_deref(), Some("rgb(255, 0, 0)"));
        assert_eq!(page.sheet_walks(), walks, "no attribute write walked the arena");
        // A structural change does.
        let extra = dom.create_element("style");
        let text = dom.create_text_node("#d { color: rgb(0, 0, 255) }");
        dom.append_child(extra, text);
        let body = dom.get_elements_by_tag_name("body")[0];
        dom.append_child(body, extra);
        assert_eq!(color(&page, &dom).as_deref(), Some("rgb(0, 0, 255)"), "a new <style> is found");
        assert!(page.sheet_walks() > walks);
        same_as_fresh(&page, &dom, "inserted");
    }

    /// A 400-row flex grid of 1,600 cells, every 50th row with a percentage padding: a page taller than the window.
    fn grid_page() -> Dom {
        let mut rows = String::new();
        for i in 0..400 {
            let cls = format!("row{}{}", if i % 2 == 1 { " a" } else { "" }, if i % 50 == 0 { " pp" } else { "" });
            let cells: String = (0..4).map(|j| format!("<div class=cell id=c{}>cell {i}.{j}</div>", i * 4 + j)).collect();
            rows.push_str(&format!("<div class='{cls}'>{cells}</div>"));
        }
        Dom::parse(&format!(
            "<!doctype html><style>.row{{display:flex;gap:4px;padding:2px}} .cell{{flex:1;min-width:0;padding:1px 3px}} \
             .a .cell{{color:#333}} .pp{{padding-top:3%}}</style><body><div id=root>{rows}</div></body>"
        ))
    }

    /// Relayout `dom` from `laid` with the switches `off`, as `page` would, and check it is the fresh layout;
    /// returns it with the box layouts it computed and the boxes it carried.
    fn relaid_off(page: &Page, dom: &Dom, media: &MediaContext, laid: &LaidOut, off: &[crate::layout::tree::Knob]) -> (LaidOut, u64, u64) {
        let carried = page.boxes_carried();
        let out = crate::layout::tree::with_knobs(off, || page.relayout_document(dom, dom.root(), media, laid)).expect("a write is a new layout");
        let fresh = Page::default().layout(dom, media);
        if let Some(d) = out.tree.first_difference(&fresh.tree) {
            panic!("{off:?}: a relayout is not the fresh layout: {}", d.describe(|n| verify::describe(dom, n)));
        }
        let computed = out.layout_stats().computed;
        (out, computed, page.boxes_carried() - carried)
    }

    // Under a classic viewport scrollbar a relayout lays the page out twice -- at the window's width, then at the
    // width less the bar -- and with one state per box each pass evicted the other's answers: one cell's padding
    // write laid more boxes out than the page has. With a state per pass both are carried.
    #[test]
    fn a_relayout_under_a_viewport_scrollbar_lays_out_only_what_changed() {
        use crate::layout::tree::Knob;
        let mut dom = grid_page();
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let boxes = laid.tree.len() as u64;
        assert!(laid.tree.viewport().width < 1024.0, "the viewport scrollbar shows");
        // A first layout keeps one state (`BoxTree::compute`): the first relayout carries it as one state did, and
        // keeps a state per pass from then on.
        assert_eq!(laid.tree.states_kept(), 1, "a first layout keeps one state");
        dom.set_attribute(element(&dom, "c1"), "style", "padding-left:9px");
        let (out, first, _) = relaid_off(&page, &dom, &media, &laid, &[]);
        assert!(first > boxes, "the first relayout lays out as one state did ({first})");
        assert_eq!(out.tree.states_kept(), 2, "a relayout keeps a state per pass");
        laid = out;
        for (k, cell) in ["c7", "c900", "c1599", "c0"].into_iter().enumerate() {
            let c = element(&dom, cell);
            dom.set_attribute(c, "style", &format!("padding-left:{}px", k + 10));
            // the switch off first, from the same layout: one state, as before
            let (_, off, _) = relaid_off(&page, &dom, &media, &laid, &[Knob::TwoStates]);
            let (out, on, _) = relaid_off(&page, &dom, &media, &laid, &[]);
            assert!(on * 10 < boxes, "{cell}: {on} box layouts computed for {boxes} boxes");
            assert!(off > boxes, "{cell}: one state lays more out than the page has ({off})");
            laid = out;
        }
    }

    // A theme with no classic scrollbar has no final pass: the one state is carried as before.
    #[test]
    fn a_relayout_without_a_viewport_scrollbar_still_carries() {
        use crate::layout::tree::Knob;
        let mut dom = grid_page();
        let media = MediaContext::default();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let boxes = laid.tree.len() as u64;
        assert_eq!(laid.tree.viewport(), taffy::geometry::Size { width: 1024.0, height: 768.0 }, "no scrollbar narrows it");
        for (k, cell) in ["c7", "c900"].into_iter().enumerate() {
            let c = element(&dom, cell);
            dom.set_attribute(c, "style", &format!("padding-left:{}px", k + 10));
            for off in [&[Knob::TwoStates][..], &[]] {
                let (_, computed, carried) = relaid_off(&page, &dom, &media, &laid, off);
                assert!(computed * 10 < boxes, "{cell} {off:?}: {computed} computed for {boxes} boxes");
                assert!(carried * 10 > boxes * 9, "{cell} {off:?}: only {carried} of {boxes} boxes carried");
            }
            laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new layout");
        }
    }

    // A `<span>` split by a block is laid out by two anonymous boxes, and its `parent` names only the first. When the
    // second half changes, the box that lays it out must lay it out again in the final pass -- a box starting from the
    // full-window state with a carried box below it, through its INLINE items, refuses its laid-out answer.
    #[test]
    fn a_split_span_beside_a_carried_half_keeps_both_halves_fragments() {
        let block = "<div class='pad'><span>one</span> two <b>three</b><div class='ib'>i</div></div><p>para</p>";
        let mut dom = Dom::parse(&format!(
            "<!doctype html><style>.big {{ font-size: 24px }} .ib {{ display: inline-block; width: 30px }} .pad {{ padding: 3px }}</style>\
             <body>{}<div style='width:300px'><span id=s>a<div id=d>b</div><i id=c>c</i></span></div></body>",
            block.repeat(12)
        ));
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        assert!(laid.tree.viewport().width < 1024.0, "the viewport scrollbar shows");
        let (sp, c, d) = (element(&dom, "s"), element(&dom, "c"), element(&dom, "d"));
        for step in 0..4 {
            match step {
                0 => dom.set_text_content(c, "a longer second half"),
                1 => dom.set_attribute(c, "class", "big"),
                2 => {
                    let t = dom.create_text_node(" more");
                    dom.insert_before(sp, t, c);
                }
                _ => dom.set_attribute(d, "class", "big"),
            }
            laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new layout");
            let fresh = Page::default().layout(&dom, &media);
            assert!(laid.tree.first_difference(&fresh.tree).is_none(), "step {step}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
            let rects = laid.viewport_client_rects(sp);
            assert_eq!(rects, fresh.viewport_client_rects(sp), "step {step}");
            assert!(rects.len() >= 2, "step {step}: both halves have a fragment: {rects:?}");
        }
    }

    // A document within a few pixels of the window's height: whether the bar shows decides the second pass. Writes that
    // move it across and back, and one that moves nothing near it: each relayout is the fresh layout.
    #[test]
    fn a_document_that_fits_only_without_the_scrollbar_settles_as_from_nothing() {
        let mut dom = Dom::parse(
            "<!doctype html><style>body { margin: 0 } .h { height: 100px } p { margin: 0; height: 60px }</style>\
             <body><div class=h></div><div class=h></div><div class=h></div><div class=h></div><div class=h></div>\
             <div class=h></div><div class=h></div><p id=p>text that wraps when the page is narrower</p><span id=x>x</span></body>",
        );
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let (p, x) = (element(&dom, "p"), element(&dom, "x"));
        for style in ["height: 80px", "height: 30px", "", "height: 70px"] {
            dom.set_attribute(p, "style", style);
            for (step, write) in ["a", "b"].into_iter().enumerate() {
                if step == 1 {
                    dom.set_attribute(x, "class", write);
                }
                laid = page.relayout_document(&dom, dom.root(), &media, &laid).unwrap_or(laid);
                let fresh = Page::default().layout(&dom, &media);
                assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{style:?} {step}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
            }
        }
    }

    // A middle-aligned cell's content is placed inside the cell's own layout, so a cell answered from its cache -- the
    // second pass under a classic bar, a relayout that carries the cell -- has it where its layout put it. The table
    // moved it by the alignment after that layout, again on every answer: on this page x.offsetTop read 33 on the
    // first layout and 65, 81, 97 over relayouts, where Chrome 152.0.7977.82 reads 17 throughout (2026-10-23).
    #[test]
    fn a_middle_aligned_cell_places_its_content_once_however_often_it_is_answered() {
        let mut dom = Dom::parse(
            "<!doctype html><body><table><tr><td id=b>a<br>a2<br>a3</td><td><div id=x>x middle</div></td></tr></table>\
             <table><tr><td>a</td><td rowspan=2><div id=y>y in rowspan</div></td></tr><tr><td>b</td></tr></table>\
             <div style='height:2000px'>tall</div></body>",
        );
        let page = Page::default();
        let (b, x, y) = (element(&dom, "b"), element(&dom, "x"), element(&dom, "y"));
        // one pass, no bar: where the content belongs
        let alone = page.layout(&dom, &MediaContext::default());
        let at = |laid: &LaidOut| (laid.offset_position(x).1, laid.offset_position(y).1);
        let want = at(&alone);
        assert!(want.0 > 0 && want.1 > 0, "both are moved down by the alignment: {want:?}");
        let media = classic_bars();
        let mut laid = page.layout(&dom, &media);
        assert!(laid.tree.viewport().width < 1024.0, "the viewport scrollbar shows");
        assert_eq!(at(&laid), want, "the second pass answers the cells from their caches");
        for k in 2..6 {
            dom.set_attribute(b, "style", &format!("padding-left:{k}px"));
            laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new layout");
            let fresh = Page::default().layout(&dom, &media);
            assert!(laid.tree.first_difference(&fresh.tree).is_none(), "write {k}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
            assert_eq!(at(&laid), want, "write {k}");
        }
    }

    // A cell's `height` is the least its rows give it and its content is aligned by the content's own height, a
    // percentage height inside resolving only against a cell or table height of its own; `min-height` and `max-height`
    // do nothing to a cell. Measured with its `height`, a `<td height=60>` was as tall as its row and its content never
    // moved. Every number is Chrome 152.0.7977.82's (2026-10-23): [y of the 10px block, the cell's height].
    #[test]
    fn a_cell_aligns_its_content_by_the_contents_height_and_takes_its_height_as_a_least() {
        let cases: &[(&str, &str, (f32, f32))] = &[
            ("<td id=c style='height:60px'>", "", (25.0, 60.0)),
            ("<td id=c style='height:60px;vertical-align:bottom'>", "", (50.0, 60.0)),
            ("<td id=c style='height:60px;padding:5px'>", "", (30.0, 70.0)),
            ("<td id=c style='height:60px;box-sizing:border-box;padding:5px'>", "", (25.0, 60.0)),
            ("<td id=c style='height:60px;border:2px solid;padding:3px'>", "", (30.0, 70.0)),
            ("<td id=c style='min-height:60px'>", "", (0.0, 10.0)),
            ("<td id=c style='height:60px;max-height:30px'>", "", (25.0, 60.0)),
            ("<td id=c style='height:30px'>", "height:50px", (0.0, 50.0)),
            ("<td id=c height=60>", "margin:7px 0 3px", (27.0, 60.0)),
            ("<td id=c style='height:60px'>", "height:100%", (0.0, 60.0)),
            ("<td id=c style='height:60px'>", "height:50%", (15.0, 60.0)),
        ];
        for &(td, block, want) in cases {
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{margin:0}} table{{border-spacing:0}} td{{padding:0}} #x{{width:10px;height:10px}}</style>\
                 <body><table><tr>{td}<div id=x style='{block}'></div></td></tr></table></body>"
            ));
            let laid = Page::default().layout(&dom, &MediaContext::default());
            let got = (laid.bounding_rect(element(&dom, "x")).y, laid.bounding_rect(element(&dom, "c")).height);
            assert_eq!(got, want, "{td} {block}");
        }
        // a percentage height under a row's height or a taller neighbour is `auto`; under the table's it resolves
        for (table, row, want) in [("", "<td><div style='width:5px;height:60px'></div></td>", (30.0, 0.0)), ("", "", (30.0, 0.0)), ("height:60px", "", (15.0, 30.0))] {
            let tr = if row.is_empty() && table.is_empty() { "style='height:60px'" } else { "" };
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{margin:0}} table{{border-spacing:0}} td{{padding:0}}</style><body><table style='{table}'>\
                 <tr {tr}><td><div id=x style='width:10px;height:50%'></div></td>{row}</tr></table></body>"
            ));
            let r = Page::default().layout(&dom, &MediaContext::default()).bounding_rect(element(&dom, "x"));
            assert_eq!((r.y, r.height), want, "table '{table}', row '{tr}', beside '{row}'");
        }
    }

    // A table's own height goes to its rows, as Chrome 152.0.7977.82 shares it (2026-10-23): the rows' heights here.
    #[test]
    fn a_tables_height_is_shared_among_its_rows() {
        let b = |h: u32| format!("<td><div style='width:10px;height:{h}px'></div></td>");
        let cases: Vec<(String, Vec<f32>)> = vec![
            (format!("<tr>{}</tr><tr>{}</tr>", b(10), b(30)), vec![25.0, 75.0]),
            (format!("<tr style='height:20px'>{}</tr><tr>{}</tr>", b(10), b(10)), vec![20.0, 80.0]),
            (format!("<tr><td></td></tr><tr>{}</tr>", b(10)), vec![0.0, 100.0]),
            ("<tr><td></td></tr><tr><td></td></tr>".to_string(), vec![50.0, 50.0]),
            (format!("<tr style='height:20px'>{}</tr><tr style='height:30px'>{}</tr>", b(10), b(10)), vec![40.0, 60.0]),
            (format!("<tr>{}</tr><tr><td style='height:30px'><div style='width:10px;height:10px'></div></td></tr>", b(10)), vec![70.0, 30.0]),
            (format!("<thead><tr>{}</tr></thead><tbody><tr>{}</tr></tbody>", b(10), b(30)), vec![10.0, 90.0]),
            (format!("<thead><tr>{}</tr></thead><tfoot><tr>{}</tr></tfoot>", b(10), b(30)), vec![25.0, 75.0]),
            (format!("<tbody><tr>{}</tr><tr>{}</tr></tbody><tbody><tr>{}</tr></tbody>", b(10), b(20), b(30)), vec![16.65625, 33.34375, 50.0]),
        ];
        for (rows, want) in cases {
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{margin:0}} table{{border-spacing:0}} td{{padding:0;vertical-align:top}}</style>\
                 <body><table id=t style='height:100px'>{rows}</table></body>"
            ));
            let laid = Page::default().layout(&dom, &MediaContext::default());
            let got: Vec<f32> = dom.get_elements_by_tag_name("tr").into_iter().map(|r| laid.bounding_rect(r).height).collect();
            assert_eq!(got, want, "{rows}");
            assert_eq!(laid.bounding_rect(element(&dom, "t")).height, 100.0, "{rows}");
        }
        // spacing, border and padding come off first; a caption is outside the height
        let dom = Dom::parse(&format!(
            "<!doctype html><style>body{{margin:0}} td{{padding:0;vertical-align:top}}</style><body>\
             <table id=t style='height:100px;border-spacing:4px;border:3px solid;padding:2px'><tr>{}</tr><tr>{}</tr></table>\
             <table id=u height=100><caption style='height:20px'></caption><tr>{}</tr></table></body>",
            b(10), b(30), b(10)
        ));
        let laid = Page::default().layout(&dom, &MediaContext::default());
        let rows: Vec<f32> = dom.get_elements_by_tag_name("tr").into_iter().map(|r| laid.bounding_rect(r).height).collect();
        assert_eq!(rows, vec![19.5, 58.5, 96.0]);
        assert_eq!(laid.bounding_rect(element(&dom, "u")).height, 120.0);
    }

    // A table's `height` is held within its `min-height` and `max-height` before its rows share it, and a `min-height`
    // alone is shared too. Chrome 152.0.7977.82 (2026-10-23): [the table's height, its cell's, y of the 10px block and its
    // height]; the unclamped height went to the rows ([100, 100, 45, 10] for the first).
    #[test]
    fn a_tables_height_is_held_within_its_min_and_max_height() {
        let cases: &[(&str, &str, &str, [f32; 4])] = &[
            ("", "height:100px;max-height:50px", "", [50.0, 50.0, 20.0, 10.0]),
            ("", "min-height:150px", "", [150.0, 150.0, 70.0, 10.0]),
            ("", "height:100px;min-height:150px", "", [150.0, 150.0, 70.0, 10.0]),
            ("", "min-height:100px;max-height:50px", "", [100.0, 100.0, 45.0, 10.0]),
            ("height:300px", "height:100%;max-height:80px", "", [80.0, 80.0, 35.0, 10.0]),
            ("", "height:100px;max-height:50px;padding:5px;border:2px solid", "", [50.0, 36.0, 20.0, 10.0]),
            ("", "box-sizing:content-box;height:100px;max-height:50px;padding:5px;border:2px solid", "", [64.0, 50.0, 27.0, 10.0]),
            ("", "height:100px;max-height:50px", "<caption>c</caption>", [70.0, 50.0, 40.0, 10.0]),
            // a percentage height in the cell resolves against the clamped height, and not under a `min-height` alone
            ("", "height:100px;max-height:50px", "", [50.0, 50.0, 12.5, 25.0]),
            ("", "min-height:100px", "", [100.0, 100.0, 50.0, 0.0]),
            // a percentage limit only against a parent with a height of its own (an absolute box's measured one is not)
            ("", "height:100px;max-height:50%", "", [100.0, 100.0, 45.0, 10.0]),
            ("height:120px", "height:100px;max-height:50%", "", [60.0, 60.0, 25.0, 10.0]),
        ];
        for (k, &(parent, table, caption, want)) in cases.iter().enumerate() {
            let block = if k == 8 || k == 9 { "height:50%" } else { "height:10px" };
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{margin:0;font:16px/20px Arial}} table{{border-spacing:0}} td{{padding:0}}</style><body>\
                 <div style='position:absolute;top:0;left:0;{parent}'><table id=t style='{table}'>{caption}<tr><td id=c>\
                 <div id=x style='width:10px;{block}'></div></td></tr></table></div></body>"
            ));
            let laid = Page::default().layout(&dom, &MediaContext::default());
            let (t, c, x) = (laid.bounding_rect(element(&dom, "t")), laid.bounding_rect(element(&dom, "c")), laid.bounding_rect(element(&dom, "x")));
            assert_eq!([t.height, c.height, x.y, x.height], want, "{parent} {table} {caption}");
        }
        // two rows share the clamped height
        let dom = Dom::parse(
            "<!doctype html><style>body{margin:0} table{border-spacing:0} td{padding:0}</style><body><table style='height:200px;max-height:120px'>\
             <tr><td><div style='width:10px;height:10px'></div></td></tr><tr><td><div style='width:10px;height:10px'></div></td></tr></table></body>",
        );
        let laid = Page::default().layout(&dom, &MediaContext::default());
        let rows: Vec<f32> = dom.get_elements_by_tag_name("tr").into_iter().map(|r| laid.bounding_rect(r).height).collect();
        assert_eq!(rows, vec![60.0, 60.0]);
    }

    // A relatively positioned child's `top`/`bottom` shift is not content: the cell's content is aligned by its place in
    // the flow, then shifted. Chrome 152.0.7977.82 (2026-10-23), y of a 10px block in a cell beside a 100px one (or in a
    // 100px cell); the shift read as content put the middle ones half of it higher and the bottom one all. (A sticky
    // child, taffy's `relative` too, is the same in the browser -- 45 for `top: 20px` -- where its sticky offset applies.)
    #[test]
    fn a_cell_aligns_a_relative_childs_place_in_the_flow() {
        let cases: &[(&str, &str, &str, f32)] = &[
            ("", "", "position:relative;top:20px", 65.0),
            ("", "vertical-align:bottom", "position:relative;top:20px", 110.0),
            ("", "", "position:relative;bottom:-20px", 65.0),
            ("", "", "position:relative;top:-20px", 25.0),
            ("", "", "position:relative;top:20px;margin-bottom:10px", 60.0),
            ("", "height:100px", "position:relative;top:30px", 75.0),
            ("", "height:100px;padding:5px", "position:relative;top:20px", 70.0),
            // a percentage against the cell's height where that is definite, else `auto`
            ("", "height:100px;padding:5px;border:3px solid", "position:relative;top:10%", 63.0),
            ("height:100px", "", "position:relative;top:10%", 55.0),
            ("", "", "position:relative;top:10%", 45.0),
            ("", "", "position:relative;top:calc(10% + 5px)", 45.0),
        ];
        for &(table, td, block, want) in cases {
            let beside = if td.contains("height") || !table.is_empty() { "" } else { "<td><div style='width:10px;height:100px'></div></td>" };
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{margin:0}} table{{border-spacing:0}} td{{padding:0}}</style><body>\
                 <table style='{table}'><tr>{beside}<td style='{td}'><div id=x style='width:10px;height:10px;{block}'></div></td></tr></table></body>"
            ));
            let laid = Page::default().layout(&dom, &MediaContext::default());
            assert_eq!(laid.bounding_rect(element(&dom, "x")).y, want, "table '{table}', td '{td}', {block}");
        }
        // a second child, after a block
        let dom = Dom::parse(
            "<!doctype html><style>body{margin:0} table{border-spacing:0} td{padding:0}</style><body><table><tr>\
             <td><div style='width:10px;height:100px'></div></td><td><div style='width:10px;height:10px'></div>\
             <div id=x style='width:10px;height:10px;position:relative;top:40px'></div></td></tr></table></body>",
        );
        let laid = Page::default().layout(&dom, &MediaContext::default());
        assert_eq!(laid.bounding_rect(element(&dom, "x")).y, 90.0);
    }

    // A scroll container that fits the window and overflows once the viewport's bar narrows the page shows its own bar
    // only in a later pass: carried states for it, from either pass, must not keep the other's answer.
    #[test]
    fn a_box_that_overflows_only_at_the_narrow_width_is_revealed_as_from_nothing() {
        use crate::layout::tree::Knob;
        let mut dom = Dom::parse(
            "<!doctype html><style>body { margin: 0 } .t { height: 2000px }</style><body>\
             <div id=o style='overflow:auto'><div style='width:1015px;height:10px'></div></div><div class=t id=t>t</div></body>",
        );
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let o = element(&dom, "o");
        assert!(laid.client_size(o).1 < laid.offset_size(o).1, "the box shows a horizontal bar at the narrow width");
        let t = element(&dom, "t");
        for (k, height) in ["1990px", "2010px", "500px", "2000px"].into_iter().enumerate() {
            dom.set_attribute(t, "style", &format!("height:{height}"));
            for off in [&[Knob::TwoStates][..], &[]] {
                let _ = relaid_off(&page, &dom, &media, &laid, off);
            }
            laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new layout");
            let _ = k;
        }
    }

    // The passes' scans walk maintained lists and snap only the boxes they wrote; the switch off scans and snaps
    // every box, as before. Both lay out alike.
    #[test]
    fn carrying_with_whole_scans_equals_fresh_under_classic_scrollbars() {
        use crate::layout::tree::Knob;
        let (carried, _) = crate::layout::tree::with_knobs(&[Knob::Lists], || carry_under_classic_bars(1..=6, 40));
        assert!(carried > 6_000, "only {carried} boxes carried");
        let mut dom = grid_page();
        let media = classic_bars();
        let page = Page::default();
        let laid = page.layout(&dom, &media);
        let c = element(&dom, "c33");
        dom.set_attribute(c, "style", "padding-left: 9px");
        let _ = switch_on_equals_off(&dom, &media, &page, &laid, &[Knob::Lists]);
    }

    #[test]
    fn carrying_one_state_equals_fresh_under_classic_scrollbars() {
        let (carried, _) = crate::layout::tree::with_knobs(&[crate::layout::tree::Knob::TwoStates], || carry_under_classic_bars(1..=6, 40));
        assert!(carried > 6_000, "only {carried} boxes carried");
    }

    // A carousel that shows its own scrollbar was carried only when the bar's style had moved nothing: the carry
    // hash took the KEPT tree's styles as its last pass left them -- the bar shown -- and a fresh build has none yet.
    // By what each tree was built with, the rows pair, and their percentage paddings stand.
    #[test]
    fn an_overflowing_carousel_is_carried_with_its_scrollbar() {
        let tile = "<div class=tile><div class=media></div><p>text</p></div>";
        let row = format!("<div class=car>{}</div>", tile.repeat(8));
        let mut dom = Dom::parse(&format!(
            "<!doctype html><style>.car {{ display: flex; overflow-x: auto; gap: 8px }} .tile {{ flex: 0 0 180px; padding: 4px }} \
             .media {{ padding-top: 56.25% }} p {{ margin: 6px 0 }}</style><body>{}<p id=t>x</p></body>",
            row.repeat(10)
        ));
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let rows = dom.get_elements_by_tag_name("div").into_iter().filter(|&d| dom.get_attribute(d, "class") == Some("car")).collect::<Vec<_>>();
        assert!(rows.iter().all(|&r| laid.client_size(r).1 < laid.offset_size(r).1), "every row shows its horizontal bar");
        let t = element(&dom, "t");
        for text in ["a longer text", "x"] {
            dom.set_text_content(t, text);
            let before = page.boxes_carried();
            laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
            let carried = page.boxes_carried() - before;
            assert!(carried as usize + 20 > laid.tree.len(), "{text}: only {carried} of {} boxes carried", laid.tree.len());
            assert_eq!(laid.layout_stats().padding_moved, 0, "{text}: no percentage padding moved");
            let fresh = Page::default().layout(&dom, &media);
            assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{text}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
        }
    }

    // A container of thousands of children, one changed: its children pair in order, each with its first counterpart.
    #[test]
    fn a_wide_container_pairs_its_children_in_order() {
        let items: String = (0..3000).map(|i| format!("<div id=d{i}>{i}</div>")).collect();
        let mut dom = Dom::parse(&format!("<!doctype html><body><div id=w>{items}</div></body>"));
        let media = MediaContext::default();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let d = element(&dom, "d1500");
        dom.set_attribute(d, "style", "width: 40px");
        let before = page.boxes_carried();
        laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
        // 2,999 untouched children, each with its text box
        assert!(page.boxes_carried() - before >= 2 * 2999, "{} carried", page.boxes_carried() - before);
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
    }

    // Inline-blocks nested thirty deep, then a write elsewhere: the carry compares each box pair once. An atomic inline
    // is reached as a child and as an IFC item, and the shape check walked both: 2^30 pairs, and the relayout never
    // came back (www.nzherald.co.nz, 2026-10-23).
    #[test]
    fn nested_inline_blocks_are_compared_once_per_pair() {
        let depth = 30;
        let open: String = (0..depth).map(|i| format!("<span style='display:inline-block'>t{i} ")).collect();
        let close: String = (0..depth).map(|_| "</span>").collect();
        let mut dom = Dom::parse(&format!("<!doctype html><body><div id=a>x</div><p>{open}{close}</p></body>"));
        let media = MediaContext::default();
        let page = Page::default();
        let laid = page.layout(&dom, &media);
        // a structural change, so the relayout builds a tree and carries into it (a style write is patched in place)
        let d = dom.create_element("div");
        dom.append_child(element(&dom, "a"), d);
        let t0 = std::time::Instant::now();
        let laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "{:?}", t0.elapsed());
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
    }

    // The by-value relayout: a write that moves no box hands the layout back as it was; one that does gives a new
    // layout, the fresh one; and a container-query page, whose second pass carries from the first pass's tree, gives
    // a new layout too, never the gutted one it was handed.
    #[test]
    fn relayout_owned_returns_the_previous_untouched_when_kept_and_a_new_tree_after_a_carry() {
        let mut dom = grid_page();
        let media = classic_bars();
        let page = Page::default();
        let laid = page.layout(&dom, &media);
        let (c, cell) = (element(&dom, "c5"), element(&dom, "c9"));
        let (rect, stats) = (laid.bounding_rect(cell), laid.layout_stats().computed);
        dom.set_attribute(c, "style", "color: rgb(1, 2, 3)");
        let kept = page.relayout_document_owned(&dom, dom.root(), &media, laid);
        assert!(!kept.is_new(), "a colour moves no box");
        let laid = kept.into_inner();
        assert_eq!((laid.bounding_rect(cell), laid.layout_stats().computed), (rect, stats), "handed back untouched");
        dom.set_attribute(c, "style", "width: 30px");
        let relaid = page.relayout_document_owned(&dom, dom.root(), &media, laid);
        assert!(relaid.is_new());
        let laid = relaid.into_inner();
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));

        let page = Page::default();
        let mut dom = Dom::parse(
            "<!doctype html><style>.c { container-type: inline-size; width: 300px } .i { height: 3px } \
             @container (min-width: 200px) { .i { height: 7px } }</style>\
             <body><div class=c id=c><div class=i id=i></div></div><p id=p>p</p></body>",
        );
        let media = MediaContext::default();
        let laid = page.layout(&dom, &media);
        let (c, i, p) = (element(&dom, "c"), element(&dom, "i"), element(&dom, "p"));
        assert_eq!(laid.offset_size(i).1, 7);
        dom.set_attribute(c, "style", "width: 150px");
        let passes = page.container_passes();
        let relaid = page.relayout_document_owned(&dom, dom.root(), &media, laid);
        assert!(page.container_passes() > passes, "the query is answered again in a second pass");
        assert!(relaid.is_new());
        let laid = relaid.into_inner();
        assert!(!laid.tree.is_gutted());
        assert_eq!(laid.offset_size(i).1, 3, "the narrow container no longer matches");
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
        dom.set_attribute(p, "class", "q");
        let laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
    }

    // A style write patches the last box tree in place: no tree is built after the first layout, each patched tree is
    // the tree a build makes (asserted inside the patch in tests), and each relayout of it is the fresh layout. The
    // switch off builds and carries, as before; both lay out alike.
    #[test]
    fn a_style_write_patches_the_tree_in_place() {
        use crate::layout::tree::Knob;
        let mut dom = grid_page();
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let (built, _) = page.trees_built_and_patched();
        for k in 0..50usize {
            let c = element(&dom, &format!("c{}", (k * 37) % 1600));
            match k % 3 {
                0 => dom.set_attribute(c, "style", &format!("padding-left:{}px", k % 9 + 10)),
                1 => dom.set_attribute(c, "style", &format!("width:{}px", 20 + k)),
                _ => dom.set_attribute(c, "class", if k % 2 == 0 { "cell pp" } else { "cell x" }),
            }
            if k % 10 == 9 {
                let _ = switch_on_equals_off(&dom, &media, &page, &laid, &[Knob::Patch]);
            }
            laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
            let fresh = Page::default().layout(&dom, &media);
            if let Some(d) = laid.tree.first_difference(&fresh.tree) {
                panic!("write {k}: a patched layout is not the fresh one: {}", d.describe(|n| verify::describe(&dom, n)));
            }
        }
        let (built_after, patched) = page.trees_built_and_patched();
        // the five switch comparisons relayout a LENT layout twice each, which builds; the 50 writes build none
        assert!(built_after - built <= 10, "{} trees built", built_after - built);
        assert!(patched >= 50, "{patched} patched");
    }

    // An attribute the BUILDER reads -- an input's type and value, an image's alt, a cell's colspan, a details' open --
    // is no style write: the tree is built again, and every layout is the fresh one.
    #[test]
    fn an_attribute_the_builder_reads_rebuilds() {
        let mut dom = Dom::parse(
            "<!doctype html><body><input id=i value=abc><img id=m alt=picture><table><tr><td id=c>a</td><td>b</td></tr>\
             <tr><td>c</td><td>d</td></tr></table><details id=d><summary>s</summary><p>p</p></details><p id=p>x</p></body>",
        );
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let (i, m, c, d, p) = (element(&dom, "i"), element(&dom, "m"), element(&dom, "c"), element(&dom, "d"), element(&dom, "p"));
        let writes: [(NodeId, &str, &str); 6] = [(i, "type", "checkbox"), (i, "value", "a much longer value"), (m, "alt", "x"), (c, "colspan", "2"), (d, "open", ""), (p, "class", "q")];
        for (node, name, value) in writes {
            let (built, _) = page.trees_built_and_patched();
            dom.set_attribute(node, name, value);
            laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
            let fresh = Page::default().layout(&dom, &media);
            assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{name}: {:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
            if name != "class" {
                assert!(page.trees_built_and_patched().0 > built, "{name}: the tree is built again");
            }
        }
    }

    // A `display: contents` element has no box, but its style is what its own text runs are laid out in, and its flex
    // parent's anonymous item's: a write to it is not a write to nothing. Patched as box-less, `none` -> `contents`
    // kept its text hidden, `contents` -> `none` kept it shown, and a font write kept the old lines -- until the next
    // build. A `display: none` element that stays `none` is still nothing of the tree, and patches.
    #[test]
    fn a_display_contents_write_is_laid_out() {
        let mut dom = Dom::parse(
            "<!doctype html><style>.big{font-size:40px} .lh{line-height:60px}</style><body>\
             <div id=p style='width:300px'><div id=c style='display:none'>hello world hello world hello world</div></div>\
             <div id=q style='width:300px'><div id=d style='display:contents'>hello world hello world hello world</div></div>\
             <div id=f style='display:flex;width:300px'><div id=g style='display:contents'>flex text item here</div></div>\
             <div id=h><span id=s style='display:contents'><span style='display:contents'>nested text</span> \
             <i style='display:none'>x</i></span></div><p id=t>t</p></body>",
        );
        let media = classic_bars();
        let page = Page::default();
        let mut laid = page.layout(&dom, &media);
        let (c, d, g, s, q, t) = (element(&dom, "c"), element(&dom, "d"), element(&dom, "g"), element(&dom, "s"), element(&dom, "q"), element(&dom, "t"));
        let writes: [(NodeId, &str, &str); 10] = [
            (c, "style", "display:contents"),
            (q, "style", "width:301px"),
            (c, "style", "display:none"),
            (c, "style", "display:none; color: red"),
            (d, "class", "big"),
            (d, "class", "lh"),
            (g, "class", "big"),
            (s, "class", "big"),
            (c, "style", "display:contents"),
            (t, "class", "big"),
        ];
        for (k, (node, name, value)) in writes.into_iter().enumerate() {
            let (_, patched) = page.trees_built_and_patched();
            dom.set_attribute(node, name, value);
            laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
            let fresh = Page::default().layout(&dom, &media);
            if let Some(diff) = laid.tree.first_difference(&fresh.tree) {
                panic!("write {k} ({name}={value}): {}", diff.describe(|n| verify::describe(&dom, n)));
            }
            if k == 3 {
                assert!(page.trees_built_and_patched().1 > patched, "a `none` element staying `none` patches");
            }
        }
        assert!(laid.offset_size(element(&dom, "p")).1 > 0, "c's text shows");
    }

    // What a carry never carries is laid out from nothing after a patch too: the initial containing block's fixed and
    // absolute boxes (`pair_with` pairs from `<html>` down), and a `content-visibility: auto` box with its ancestors.
    // Carried from the patched tree's own last layout instead, a shrink-to-fit fixed box around a 25% padding, whose
    // passes do not settle (the padding grows the box it is a percentage of), went one padding pass further on each
    // relayout (196.14 wide where from nothing it is 195.95), and a `content-visibility: auto` box with a percentage
    // padding inside one kept the padded style it was resolved from while skipped (118px tall for 46).
    #[test]
    fn a_patched_relayout_lays_out_what_a_carry_does_not_carry_from_nothing() {
        let media = classic_bars();
        for pos in ["position:fixed;bottom:5px;left:5px", "position:absolute;bottom:5px;left:5px"] {
            let mut dom = Dom::parse(&format!(
                "<!doctype html><style>body{{font:14px Arial;margin:8px}}</style><body><div id=root><div id=e11 style='{pos}'>\
                 <div id=e17><div id=e19><div id=e20>beta gamma and kappa</div></div></div></div><div id=e30></div></div></body>"
            ));
            let page = Page::default();
            let mut laid = page.layout(&dom, &media);
            let (e19, e30) = (element(&dom, "e19"), element(&dom, "e30"));
            dom.set_attribute(e19, "style", "padding-left:25%");
            for k in 0..5 {
                if k > 0 {
                    dom.set_attribute(e30, "style", &format!("width:{}px", 10 + k % 2));
                }
                laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
                let fresh = Page::default().layout(&dom, &media);
                if let Some(diff) = laid.tree.first_difference(&fresh.tree) {
                    panic!("{pos}, write {k}: {}", diff.describe(|n| verify::describe(&dom, n)));
                }
            }
            assert!(page.trees_built_and_patched().1 >= 4, "the writes patch");
        }
        for pos in ["position:fixed;bottom:5px;left:5px", "position:absolute;bottom:5px;left:5px", "float:left"] {
            let mut dom = Dom::parse(&format!(
                "<!doctype html><style>body{{font:14px Arial;margin:8px}} .cv{{content-visibility:auto;contain-intrinsic-size:auto 100px}} \
                 .pp{{padding:4% 2%}} .lh{{line-height:2}}</style><body><div id=root><div id=e12 style='{pos}'><div id=e13>\
                 <p id=e14 class='lh cv'>theta delta alpha supercalifragilistic</p></div></div><div id=e41></div></div></body>"
            ));
            let page = Page::default();
            let mut laid = page.layout(&dom, &media);
            let (e14, e41) = (element(&dom, "e14"), element(&dom, "e41"));
            for (k, (node, name, value)) in [(e14, "class", "lh cv pp"), (e41, "style", "padding-left:13px"), (e41, "style", "padding-left:14px")].into_iter().enumerate() {
                dom.set_attribute(node, name, value);
                laid = page.relayout_document_owned(&dom, dom.root(), &media, laid).into_inner();
                let fresh = Page::default().layout(&dom, &media);
                if let Some(diff) = laid.tree.first_difference(&fresh.tree) {
                    panic!("{pos}, write {k}: {}", diff.describe(|n| verify::describe(&dom, n)));
                }
            }
        }
    }

    // A percentage padding inside a box sized from its content adds nothing to that size (CSS Sizing 3 §5.2.1: a cyclic
    // percentage resolves against zero for the contribution), then takes its share of the width found. Resolved from
    // the last pass's width in every measurement, it grew the box pass after pass, never settling: 195.95 wide from
    // nothing and 196.14 after relayouts, where Chrome 152.0.7977.82 is 147.11 -- the text's own width, a quarter of
    // it padding and the text on two lines -- for each of these boxes (2026-10-23).
    #[test]
    fn a_percentage_padding_adds_nothing_to_the_shrink_to_fit_width_it_is_a_percentage_of() {
        let media = classic_bars();
        for s in ["display:inline-block", "width:fit-content", "float:left", "display:inline-flex", "position:absolute"] {
            let html = |pad: &str| {
                Dom::parse(&format!(
                    "<!doctype html><style>body{{font:14px Arial;margin:8px}}</style><body><div id=root><div id=e11 style='{s}'>\
                     <div id=e17><div id=e19 style='{pad}'><div id=e20>beta gamma and kappa</div></div></div></div><div id=e30></div></div></body>"
                ))
            };
            let bare = html("");
            let text = Page::default().layout(&bare, &media).bounding_rect(element(&bare, "e11"));
            let mut dom = html("padding-left:25%");
            let page = Page::default();
            let mut laid = page.layout(&dom, &media);
            let (e11, e30) = (element(&dom, "e11"), element(&dom, "e30"));
            for k in 0..4 {
                if k > 0 {
                    dom.set_attribute(e30, "style", &format!("width:{}px", 10 + k % 2));
                    laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new layout");
                }
                let r = laid.bounding_rect(e11);
                assert_eq!(r.width, text.width, "{s}, write {k}: as wide as the text");
                assert!(r.height > text.height, "{s}, write {k}: the padding takes its share and the text wraps ({} tall)", r.height);
                let fresh = Page::default().layout(&dom, &media);
                if let Some(diff) = laid.tree.first_difference(&fresh.tree) {
                    panic!("{s}, write {k}: {}", diff.describe(|n| verify::describe(&dom, n)));
                }
            }
        }
    }

    // The same for a flex or grid ITEM with the percentage padding, while its container sizes itself from its items:
    // the item kept its resolved padding there, so `inline-flex` around it was 195.95 wide from nothing and 196.14
    // after relayouts, where Chrome 152.0.7977.82 is 147.11 and the item two lines (147 x 32) for each of these
    // containers. In a container of a definite width the item still counts its padding against that width -- taffy
    // measures its base size with no width -- as Chrome does: a 400px row's item is 247.11 wide, and so is the item of
    // a 400px row measured inside an `inline-block` (2026-10-23).
    #[test]
    fn a_flex_or_grid_items_percentage_padding_adds_nothing_to_the_width_its_container_takes_from_it() {
        let media = classic_bars();
        let html = |s: &str, pad: &str| {
            Dom::parse(&format!(
                "<!doctype html><style>body{{font:14px Arial;margin:8px}}</style><body><div id=root><div id=e11 style='{s}'>\
                 <div id=e19 style='{pad}'><div id=e20>beta gamma and kappa</div></div></div><div id=e30></div></div></body>"
            ))
        };
        let bare = html("display:inline-block", "");
        let text = Page::default().layout(&bare, &media).bounding_rect(element(&bare, "e11"));
        for s in ["display:inline-flex", "display:inline-grid", "display:flex;width:fit-content", "display:inline-flex;flex-direction:column"] {
            let mut dom = html(s, "padding-left:25%");
            let page = Page::default();
            let mut laid = page.layout(&dom, &media);
            let (e11, e19, e30) = (element(&dom, "e11"), element(&dom, "e19"), element(&dom, "e30"));
            for k in 0..4 {
                if k > 0 {
                    dom.set_attribute(e30, "style", &format!("width:{}px", 10 + k % 2));
                    laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new layout");
                }
                assert_eq!(laid.bounding_rect(e11).width, text.width, "{s}, write {k}: as wide as the text");
                let item = laid.bounding_rect(e19);
                assert!(item.height > text.height, "{s}, write {k}: the padding takes its share and the text wraps ({} tall)", item.height);
                let fresh = Page::default().layout(&dom, &media);
                if let Some(diff) = laid.tree.first_difference(&fresh.tree) {
                    panic!("{s}, write {k}: {}", diff.describe(|n| verify::describe(&dom, n)));
                }
            }
        }
        for (s, outer) in [("display:flex;width:400px", ""), ("display:flex;width:400px", "display:inline-block")] {
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{font:14px Arial;margin:8px}}</style><body><div style='{outer}'><div style='{s}'>\
                 <div id=e19 style='padding-left:25%'><div>beta gamma and kappa</div></div></div></div></body>"
            ));
            let item = Page::default().layout(&dom, &media).bounding_rect(element(&dom, "e19"));
            assert_eq!((item.width, item.height), (text.width + 100.0, text.height), "{s} in '{outer}': its padding a quarter of 400");
        }
    }

    // A layout from nothing settles its percentage paddings in one padding pass, LayoutUnit truncation and all. The first
    // pass has no resolved padding yet -- taffy's own 3% of 640 is 19.2, the resolved one 19.1875 -- and the boxes whose
    // basis is the content width of a box with such a padding saw that width move by the truncation and asked for one
    // more pass of the whole page: the text runs, which carry their element's style and never use its padding, and a
    // percentage padding inside one, whose basis now takes its parent's padding as just resolved. Chrome 152.0.7977.82:
    // .b 601.625 wide with 18.0469px of padding, a .b inside it 565.53125 with 16.9531px (2026-10-23). A third level of
    // inexact percentages still takes a pass more: the middle box's own width moves with the outer padding.
    #[test]
    fn a_layout_from_nothing_settles_truncated_percentage_paddings_in_one_pass() {
        for (sheet, body, runs) in [
            (".a{padding:5% 3%}", "<div class=a>text</div>", 2),
            (".a{padding:5% 3%} .b{padding:0 3%}", "<div class=a><div class=b id=b>text</div></div>", 2),
            (".a{padding:5% 3%} .b{padding:0 3%}", "<div class=a><div class=b><div class=b id=bb>text</div></div></div>", 3),
        ] {
            let dom = Dom::parse(&format!(
                "<!doctype html><style>body{{font:14px Arial;margin:8px}} #root{{width:640px}} {sheet}</style><body><div id=root>{}</div></body>",
                body.repeat(20)
            ));
            let laid = Page::default().layout(&dom, &MediaContext::default());
            assert_eq!(laid.layout_stats().root_runs, runs, "{body}: root runs");
            if let Some(b) = dom.get_element_by_id("b") {
                assert_eq!(laid.bounding_rect(b).width, 601.625, "{body}: Chrome's width");
            }
            if let Some(b) = dom.get_element_by_id("bb") {
                assert_eq!(laid.bounding_rect(b).width, 565.53125, "{body}: Chrome's width");
            }
        }
    }

    // An unlocked `content-visibility: auto` box is laid out with its relevant style, percentage padding and all: its
    // resolved-padding style, made while it was skipped, kept the skipped one's intrinsic 100px. Chrome 152.0.7977.82:
    // 40 tall once relevant (16 of text and 12px above and below), where this read 124 (2026-10-23).
    #[test]
    fn an_unlocked_box_lays_out_its_percentage_padding_with_its_own_style() {
        let html = |cv: &str| {
            Dom::parse(&format!(
                "<!doctype html><style>body{{font:14px Arial;margin:8px}} .cv{{content-visibility:auto;contain-intrinsic-size:auto 100px}} \
                 .pp{{padding:4% 2%}}</style><body><div style='width:300px'><p id=a class='{cv} pp'>theta delta alpha</p></div></body>"
            ))
        };
        let plain = html("");
        let want = Page::default().layout(&plain, &MediaContext::default()).offset_size(element(&plain, "a"));
        for media in [MediaContext::default(), classic_bars()] {
            let dom = html("cv");
            let laid = Page::default().layout(&dom, &media);
            assert_eq!(laid.offset_size(element(&dom, "a")), want, "unlocked, it is laid out as a box that was never skipped");
        }
    }

    #[test]
    fn carrying_equals_fresh_under_classic_scrollbars_margins_and_percentage_padding() {
        let (carried, _) = carry_under_classic_bars(1..=12, 60);
        assert!(carried > 12_000, "only {carried} boxes carried");
    }

    // A box sized off its containing block's HEIGHT is not carried. WPT
    // css-sizing/aspect-ratio-affects-container-width-when-height-changes (Chrome 150 passes): a
    // `height: 100%` chain under a container whose height moves, ending in a float that shrink-wraps an `aspect-ratio`
    // box. Carried from the layout before, the chain answered from caches keyed on what does not hold the parent's
    // height, and the float kept its old width (200 for 100).
    #[test]
    fn a_percentage_height_chain_is_laid_out_anew_when_its_container_height_moves() {
        let page = Page::default();
        let mut dom = Dom::parse(
            "<!doctype html><body><div id=c style='height:200px'><div id=m style='height:100%'>\
             <div id=f style='float:left; height:100%'><div id=a style='height:100%; aspect-ratio:1'></div></div></div></div></body>",
        );
        let media = MediaContext::default();
        let (c, f) = (element(&dom, "c"), element(&dom, "f"));
        let laid = page.layout(&dom, &media);
        assert_eq!(laid.offset_size(f), (200, 200), "the float shrink-wraps its square");
        dom.set_attribute(c, "style", "height:100px");
        let laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new height is a new layout");
        assert_eq!(laid.offset_size(f), (100, 100), "the float follows the square down");
        let fresh = Page::default().layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
    }

    // The WPT case itself: the float shrink-wraps a block `<img>` whose natural size is 1x1 (the test's
    // transparent GIF), `height: 100%` all the way down.
    #[test]
    fn a_percentage_height_image_chain_is_laid_out_anew_when_its_container_height_moves() {
        let page = Page::default();
        let mut dom = Dom::parse(
            "<!doctype html><body><div id=c style='height:200px'><div style='height:100%'>\
             <div id=f style='float:left; height:100%'><img id=i style='display:block; height:100%'></div></div></div></body>",
        );
        let media = MediaContext::default();
        let (c, f, i) = (element(&dom, "c"), element(&dom, "f"), element(&dom, "i"));
        let images: std::collections::HashMap<NodeId, ImageState> = [(i, ImageState::Available(1.0, 1.0))].into_iter().collect();
        page.set_natural_images(&images, 1);
        let laid = page.layout(&dom, &media);
        assert_eq!(laid.offset_size(f), (200, 200), "the float shrink-wraps the 200px square image");
        dom.set_attribute(c, "style", "height:100px");
        let laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a new height is a new layout");
        assert_eq!((laid.offset_size(f), laid.offset_size(i)), ((100, 100), (100, 100)), "the float follows the image down");
        let fresh = Page::default();
        fresh.set_natural_images(&images, 1);
        let fresh = fresh.layout(&dom, &media);
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
    }

    // A web font that arrives lays its text out anew: nothing measured in the fallback is carried over. Chrome 150
    // re-measures `font: 56px W, sans-serif` once W loads; this kept the fallback's width until something else moved
    // the box (apple.com's hero headline, 308 px for Chrome's 281).
    #[test]
    fn text_is_measured_anew_when_its_web_font_arrives() {
        let page = Page::default();
        let dom = Dom::parse(
            "<!doctype html><body><div style='display:flex'><span id=t style='font: 56px W, sans-serif'>iPhone Duo</span></div></body>",
        );
        let media = MediaContext::default();
        let t = element(&dom, "t");
        let laid = page.layout(&dom, &media);
        let mut table = font::WebFonts::default();
        let metrics = font::FontProvider::metrics(&font::BuiltinFonts, "Courier New");
        table.add("W", font::WebFace { weight: (1.0, 1000.0), style: "normal".into(), ranges: Vec::new(), metrics, instance: None });
        page.set_web_fonts(Some(std::sync::Arc::new(table)), 2);
        let laid = page.relayout_document(&dom, dom.root(), &media, &laid).expect("a font arrived");
        let fresh = Page::default();
        fresh.set_web_fonts(page.state.lock().unwrap().web_fonts.clone(), 2);
        let fresh = fresh.layout(&dom, &media);
        assert_eq!(laid.offset_size(t), fresh.offset_size(t), "measured in W, as a layout made with it from the start");
        assert!(laid.tree.first_difference(&fresh.tree).is_none(), "{:?}", laid.tree.first_difference(&fresh.tree).map(|d| d.what));
    }

    // Registered custom properties type their values and do not inherit when told not to.
    #[test]
    fn a_registered_custom_property_is_typed_initial_and_not_inherited() {
        let page = Page::default();
        let dom = Dom::parse(
            "<!doctype html><style>@property --x { syntax: '<length>'; inherits: false; initial-value: 7px } \
             .p { --x: 2em; font-size: 10px }</style><body><div class=p id=p><div id=c></div></div></body>",
        );
        let media = MediaContext::default();
        let find = |id: &str| (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "id") == Some(id)).unwrap();
        assert_eq!(page.computed_custom(&dom, find("p"), &media, "--x").as_deref(), Some("20px"));
        assert_eq!(page.computed_custom(&dom, find("c"), &media, "--x").as_deref(), Some("7px"));
    }

    // `::before` styles and boxes; the element keeps its own children.
    #[test]
    fn a_before_pseudo_element_has_a_style_and_a_box() {
        let page = Page::default();
        let dom = Dom::parse(
            "<!doctype html><style>#a::before { content: 'PRE'; display: block; height: 30px }</style>\
             <body><div id=a style='font: 16px/18px Arial'>x</div></body>",
        );
        let media = MediaContext::default();
        let a = (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "id") == Some("a")).unwrap();
        let key = page.computed_pseudo_style(&dom, a, "before", &media).unwrap();
        assert_eq!(page.state.lock().unwrap().arena.value(key, PropertyId::Content), "\"PRE\"");
        let out = page.layout(&dom, &media);
        assert_eq!(out.offset_size(a).1, 48, "a 30px block ::before over one 18px line");
        assert_eq!(page.computed_pseudo_value(&dom, a, "before", &media, PropertyId::Height, Some(&out)).as_deref(), Some("30px"));
    }

    #[test]
    fn plugin_registers_the_page_service() {
        let ctx = Context::root();
        let mut loader = Loader::new();
        loader.register(PagePlugin);
        loader.apply(&ctx, &PluginConfig::default()).unwrap();
        assert!(ctx.has::<PageHandle>(), "consumers find the pipeline by asking the Context");
        assert!(ctx.has::<spec::CssHandle>(), "and the value layer, which the canvas asks for");
    }

    #[test]
    fn a_sheet_can_be_parsed_kept_and_edited_through_the_handle() {
        let page = Page::default();
        let id = page.add_sheet(Origin::Author, None, "p { color: red }");
        assert_eq!(page.with_sheet(id, |s| s.rules().len()), Some(1));
        page.with_sheet_mut(id, |s| s.insert_rule("b { color: blue }", 1).unwrap());
        assert_eq!(page.with_sheet(id, |s| s.rules()[1].selector_text()), Some("b".to_string()));
        assert_eq!(page.with_sheet(99, |s| s.rules().len()), None, "an unknown handle is not a sheet");
    }

    // Handle 0 is the user-agent sheet, which is there before anybody adds anything -- and since
    // So are the quirks-mode UA sheet (1, disabled until a quirks document) and the
    // presentational hints (2).
    #[test]
    fn a_new_page_already_has_the_user_agent_sheet() {
        let page = Page::default();
        assert_eq!(page.sheet_count(), 3);
        assert!(page.with_sheet(0, |s| s.rules().len()).unwrap() > 80);
        assert_eq!(page.with_sheet(1, |s| s.disabled), Some(true), "no quirks until a quirks document");
        assert_eq!(page.add_sheet(Origin::Author, None, ""), 3, "author sheets start at 3");
    }

    #[test]
    fn sheets_are_independent_and_handles_are_not_reused() {
        let page = Page::default();
        let a = page.add_sheet(Origin::Author, None, "a { color: red }");
        let b = page.add_sheet(Origin::Author, None, "b { color: blue } c { color: teal }");
        assert_ne!(a, b);
        assert_eq!(page.with_sheet(a, |s| s.rules().len()), Some(1));
        assert_eq!(page.with_sheet(b, |s| s.rules().len()), Some(2));
        page.with_sheet_mut(a, |s| s.replace_sync(""));
        assert_eq!(page.with_sheet(a, |s| s.rules().len()), Some(0));
        assert_eq!(page.with_sheet(b, |s| s.rules().len()), Some(2), "b is untouched");
    }

    fn element(dom: &Dom, id: &str) -> NodeId {
        (0..dom.len() as NodeId)
            .find(|&n| dom.get_attribute(n, "id") == Some(id))
            .unwrap_or_else(|| panic!("no #{id}"))
    }

    fn computed(html: &str, id: &str, property: PropertyId) -> String {
        let page = Page::default();
        let dom = Dom::parse(html);
        page.computed_value(&dom, element(&dom, id), &MediaContext::default(), property, None)
            .unwrap()
    }

    // ---- resolved (used) values ----------------------------------------------------------------
    //
    // The CSSOM's `getComputedStyle` reports the RESOLVED value, which for these is the USED one --
    // the number layout arrived at, not the string the author wrote. Reporting `"50%"` for `width` is
    // not an approximation of `"512px"`; it is a different kind of answer, and it contradicts the
    // `offsetWidth` the same element already gives.
    //
    // The viewport here is `MediaContext::default()`'s 1024x768.

    /// A `box-sizing: border-box` box reports its BORDER box as `width` and `height` (Blink's `UsedBoxSize`); any
    /// other box its content box. Chrome 150: `100px`/`50px` under padding, a border and scrollbars, and a
    /// border-box float with nothing in it is `20px` tall -- its padding.
    #[test]
    fn a_border_box_reports_its_border_box() {
        let html = "<body><div id=b style='width:100px;height:50px;padding:10px;border:3px solid;box-sizing:border-box;overflow:scroll'></div>\
                    <div id=f style='width:100px;padding:10px;box-sizing:border-box;float:left'></div>\
                    <div id=c style='width:100px;height:50px;padding:10px;border:3px solid'></div></body>";
        assert_eq!(computed(html, "b", PropertyId::Width), "100px");
        assert_eq!(computed(html, "b", PropertyId::Height), "50px");
        assert_eq!(computed(html, "f", PropertyId::Width), "100px");
        assert_eq!(computed(html, "f", PropertyId::Height), "20px");
        assert_eq!(computed(html, "c", PropertyId::Width), "100px");
    }

    #[test]
    fn width_and_height_report_the_used_length_not_the_authors_percentage() {
        let html = "<body style='margin:0'><div id='t' style='width:50%; height:25%'>x</div></body>";
        assert_eq!(computed(html, "t", PropertyId::Width), "512px", "50% of a 1024px viewport");
        // `height: 25%` against an auto-height parent is indefinite, so the used height is the
        // content's -- one line box. The point is that it is a LENGTH either way, never `25%`.
        assert!(computed(html, "t", PropertyId::Height).ends_with("px"));
    }

    // `inline-size` is `width` under `horizontal-tb`, and a browser reports the same used length for
    // both. Ours used to report `auto` for every element on the page.
    #[test]
    fn the_logical_sizes_are_the_physical_ones_in_a_horizontal_writing_mode() {
        let html = "<body style='margin:0'><div id='t' style='width:50%'>x</div></body>";
        assert_eq!(
            computed(html, "t", PropertyId::InlineSize),
            computed(html, "t", PropertyId::Width)
        );
        assert_eq!(
            computed(html, "t", PropertyId::BlockSize),
            computed(html, "t", PropertyId::Height)
        );
    }

    // An element that generated no box has no used value, and the browser reports the computed one.
    // This is the spec's own rule, not a shortcut: `getComputedStyle(hidden).width` IS `"50%"`.
    #[test]
    fn an_element_with_no_box_keeps_its_computed_value() {
        let html = "<body><div id='t' style='display:none; width:50%'></div></body>";
        assert_eq!(computed(html, "t", PropertyId::Width), "50%");
        assert_eq!(computed(html, "t", PropertyId::BlockSize), "auto");
    }

    // CSSOM: an inset's resolved value is the USED value on a positioned box and the COMPUTED value
    // on a static one. So the same `top: 10%` reads back two different ways depending on `position`,
    // and that is not an inconsistency -- it is the rule.
    #[test]
    fn an_inset_is_a_used_length_only_when_the_box_is_positioned() {
        let statically = "<body style='margin:0'><div id='t' style='top:10%'>x</div></body>";
        assert_eq!(computed(statically, "t", PropertyId::Top), "10%", "not positioned, so computed");

        let relative =
            "<body style='margin:0'><div id='t' style='position:relative; left:20px'>x</div></body>";
        assert_eq!(computed(relative, "t", PropertyId::Left), "20px");
        // Over-constrained by construction: only one of each pair is independent, and `left` wins in
        // a left-to-right document. A browser answers `right` with the negation, never with `auto`.
        assert_eq!(computed(relative, "t", PropertyId::Right), "-20px");
        assert_eq!(computed(relative, "t", PropertyId::Top), "0px", "both auto means no shift");
        assert_eq!(computed(relative, "t", PropertyId::InsetInlineStart), "20px");
    }

    // The insets of an absolutely positioned box are used values even where the author wrote `auto`:
    // the box ended up somewhere, and all four edges are measured from where it ended up. That also
    // means they can never disagree with `getBoundingClientRect`, which is the cross-check a page
    // runs.
    #[test]
    fn an_absolutely_positioned_box_reports_all_four_edges_as_lengths() {
        let html = "<body style='margin:0'>\
                    <div id='t' style='position:absolute; top:0; left:0; width:100px; height:50px'></div>\
                    </body>";
        for property in [PropertyId::Top, PropertyId::Right, PropertyId::Bottom, PropertyId::Left] {
            let value = computed(html, "t", property);
            assert!(value.ends_with("px"), "{} came back as {value}", property.name());
        }
        assert_eq!(computed(html, "t", PropertyId::Left), "0px");
        assert_eq!(computed(html, "t", PropertyId::Top), "0px");
    }

    // `transform-origin: 50% 50%` is half the BORDER box, which is a number only layout has.
    #[test]
    fn an_origin_resolves_against_the_box_it_belongs_to() {
        let html = "<body style='margin:0'>\
                    <div id='t' style='width:100px; height:40px; border:5px solid'></div></body>";
        assert_eq!(computed(html, "t", PropertyId::TransformOrigin), "55px 25px");
        assert_eq!(computed(html, "t", PropertyId::PerspectiveOrigin), "55px 25px");
    }

    // THE CLAIM, AS A COUNT RATHER THAN AS A CLOCK.
    //
    // `getComputedStyle` used to materialise all 475 properties per call, so a caller that read one
    // paid for 475 and a caller that read NONE paid the same: on a synthetic 2,006-element page the
    // call alone cost 355 ms here against 0.2 ms in Chrome 150. The fix is that the declaration
    // resolves what it is asked for, and the property that says so is that the number of ancestor
    // cascades does not grow with the number of properties read.
    //
    // It is asserted as a COUNT deliberately. A wall-clock bound is the "no measurable time passed"
    // shape that has gone red twice in this tree under load with the engine behaving correctly, and
    // it would be asserting that the machine was idle rather than that the engine got cheaper.
    #[test]
    fn reading_more_properties_does_not_cost_more_cascades() {
        let page = chain_page();
        let mut dom = Dom::parse(
            "<body><div><section><article><div id='t' style='width:50%'>x</div></article></section></div></body>",
        );
        let node = element(&dom, "t");
        let media = MediaContext::default();

        // One property: the element's chain is walked once, and every ANCESTOR of it is banked too,
        // which is what makes a document walk linear rather than quadratic.
        let before = page.cascades_run();
        let first = page.computed_value(&dom, node, &media, PropertyId::Color, None).unwrap();
        let one = page.cascades_run() - before;
        assert!(one > 0, "the first read has to actually cascade");

        // ...and then all 475, off the same element. Not one more.
        let at_one = page.cascades_run();
        let mut seen = Vec::new();
        for &property in style::property::ENUMERATED {
            seen.push(page.computed_value(&dom, node, &media, property, None).unwrap());
        }
        assert_eq!(
            page.cascades_run(),
            at_one,
            "reading all {} properties cascaded again -- the cost still scales with the property count",
            style::property::ENUMERATED.len(),
        );

        // The answers are the ones the eager enumeration gives, property for property. Cheap is only
        // worth having if it is also the same.
        let all = page.computed_values(&dom, node, &media, None);
        for (i, &property) in style::property::ENUMERATED.iter().enumerate() {
            let enumerated = all.iter().find(|(n, _)| n == property.name()).map(|(_, v)| v.as_str());
            assert_eq!(Some(seen[i].as_str()), enumerated, "{} disagrees", property.name());
        }

        // AND IT IS NOT A STALE ANSWER. A mutation the fingerprint can see drops the whole map, so
        // the next read cascades afresh and reports the new value -- the thing a cache must prove.
        dom.set_attribute(node, "style", "width:50%; color: rgb(1, 2, 3)");
        let at_all = page.cascades_run();
        let after = page.computed_value(&dom, node, &media, PropertyId::Color, None).unwrap();
        assert!(page.cascades_run() > at_all, "the mutation did not invalidate the memo");
        assert_ne!(first, after, "a mutation was not visible through the memo");
        assert_eq!(after, "rgb(1, 2, 3)");
    }

    // ── The same claim for the OTHER axis — depth ──────────────────────────────────
    //
    // P8a made reading N properties off one element cost one cascade. Reading ONE property off
    // every element of a document still cost O(elements x depth), because the walk banked each
    // ancestor and then never looked at the bank again: the next element down re-derived the whole
    // chain from the root. Measured on 4,000 elements with one property read off each --
    //
    //   flat (4000 spans, depth 1)         73.4 ms      deep (3800 spans under 200 divs)  1736.9 ms
    //
    // -- 23.66x for nesting alone, where Chrome 150 on the same fixture is 0.89x. Chrome is FLAT in
    // depth; measured, not read off a spec.
    //
    // ASSERTED AS A COUNT, for P8a's reason: a millisecond bound asserts that the machine is idle.
    // The count is the property itself -- within one memo generation each element is cascaded
    // exactly ONCE, so the total is the element count and two shapes with the same element count
    // cost the same however deeply one of them is nested. What this test measures, on its own 404
    // elements, with the resume taken out and put back:
    //
    //   flat   1,608 cascades -> 404      deep  61,508 cascades -> 404      deep/flat 38.25x -> 1.00x
    //
    // The flat shape moved too (4x), which is the same defect seen from the other end: even at
    // depth 3 every span was re-deriving <html>, <body> and #host for itself.
    fn shape(depth: usize, total: usize) -> String {
        let mut html = String::from("<body><div id='host' style='color: rgb(1, 2, 3)'>");
        for _ in 0..depth {
            html.push_str("<div>");
        }
        for i in 0..total - depth {
            html.push_str(&format!("<span>{i}</span>"));
        }
        for _ in 0..depth {
            html.push_str("</div>");
        }
        html.push_str("</div></body>");
        html
    }

    /// Put `page`'s computed-style reads on the ancestor-chain path (most are the page style
    /// pass's), which these tests measure: the path an element under a shadow host, a detached one or
    /// one read before any layout still takes.
    fn reads_walk_chains(page: &Page) {
        page.state.lock().unwrap().chains_only = true;
    }

    fn chain_page() -> Page {
        let page = Page::default();
        reads_walk_chains(&page);
        page
    }

    /// Read one property off every element of the document, in document order, and answer what that
    /// cost in cascades.
    fn read_every_element(page: &Page, dom: &Dom) -> (u64, usize) {
        let media = MediaContext::default();
        let before = page.cascades_run();
        let mut elements = 0;
        for n in 0..dom.len() as NodeId {
            if !matches!(dom.node(n).kind, NodeKind::Element { .. }) {
                continue;
            }
            elements += 1;
            page.computed_value(dom, n, &media, PropertyId::Color, None).unwrap();
        }
        (page.cascades_run() - before, elements)
    }

    #[test]
    fn reading_a_deeper_document_does_not_cost_more_cascades() {
        // The SAME element count under #host, differing only in how it is nested.
        let flat = Dom::parse(&shape(0, 400));
        let deep = Dom::parse(&shape(200, 400));

        let (flat_cascades, flat_elements) = read_every_element(&chain_page(), &flat);
        let (deep_cascades, deep_elements) = read_every_element(&chain_page(), &deep);
        assert_eq!(flat_elements, deep_elements, "the two shapes must hold the same elements");

        // ONE cascade per element, and therefore the same number for both shapes. Before this
        // change the deep shape cost 201x what the flat one did on the same elements.
        assert_eq!(
            flat_cascades, flat_elements as u64,
            "the flat document cascaded {flat_cascades} times for {flat_elements} elements",
        );
        assert_eq!(
            deep_cascades, flat_cascades,
            "nesting {deep_elements} elements 200 deep cost {deep_cascades} cascades against the \
             flat shape's {flat_cascades} -- the walk is still re-deriving the ancestor chain",
        );

        // And it is a property of the DOCUMENT, not of the visiting order: whatever order the
        // elements are asked about, each is cascaded once. Reverse order is the worst case for a
        // chain that resumes from above, because the deepest element is asked first.
        let page = chain_page();
        let media = MediaContext::default();
        let before = page.cascades_run();
        for n in (0..deep.len() as NodeId).rev() {
            if matches!(deep.node(n).kind, NodeKind::Element { .. }) {
                page.computed_value(&deep, n, &media, PropertyId::Color, None).unwrap();
            }
        }
        assert_eq!(page.cascades_run() - before, deep_elements as u64, "reverse order");
    }

    // THE HALF THAT BREAKS SILENTLY. Resuming from a banked ancestor is only sound while the bank
    // cannot be stale, and each of these is a way the answer at the BOTTOM of a chain moves without
    // the bottom element itself being touched. Every expectation here is Chrome 150.0.7871.46's,
    // measured on a loopback fixture on 2026-09-14 (`--headless --dump-dom`), not read off a spec.
    #[test]
    fn a_mutation_above_an_element_is_visible_at_the_bottom_of_the_chain() {
        let mut dom = Dom::parse(
            "<body><div id='top' style='color: rgb(1, 2, 3); font-size: 20px'>\
             <div id='mid'><div><span id='leaf'>x</span></div></div></div>\
             <div id='elsewhere' style='font-size: 40px'></div></body>",
        );
        let page = Page::default();
        let media = MediaContext::default();
        let read = |dom: &Dom, node, property| {
            page.computed_value(dom, node, &media, property, None).unwrap()
        };
        let (top, mid, leaf) = (element(&dom, "top"), element(&dom, "mid"), element(&dom, "leaf"));
        let elsewhere = element(&dom, "elsewhere");

        // Bank the whole chain, which is what makes every case below a real test of invalidation.
        assert_eq!(read(&dom, leaf, PropertyId::Color), "rgb(1, 2, 3)");
        assert_eq!(read(&dom, leaf, PropertyId::FontSize), "20px");

        // 1. An inline style on an element MID-CHAIN. Chrome: rgb(9, 8, 7).
        dom.set_attribute(mid, "style", "color: rgb(9, 8, 7)");
        assert_eq!(read(&dom, leaf, PropertyId::Color), "rgb(9, 8, 7)", "a mid-chain mutation");

        // 2. A GRANDPARENT of the mutated element, two links further up. Chrome: 31px.
        dom.set_attribute(top, "style", "color: rgb(1, 2, 3); font-size: 31px");
        assert_eq!(read(&dom, leaf, PropertyId::FontSize), "31px", "a mutation higher up the chain");

        // 3. A SHEET rule matching mid-chain, with no element touched at all.
        page.add_sheet(Origin::Author, None, "#mid { letter-spacing: 11px }");
        assert_eq!(read(&dom, leaf, PropertyId::LetterSpacing), "11px", "a sheet edit above");

        // 4. REPARENTING, which is the case a chain-resume is uniquely exposed to: nothing's style
        //    changed, the element's own style is untouched, and yet what it inherits is different
        //    because its chain is. Chrome, measured: moving the subtree under a 20px ancestor takes
        //    the leaf from 31px to 20px.
        dom.append_child(elsewhere, mid);
        assert_eq!(read(&dom, leaf, PropertyId::FontSize), "40px", "the chain itself moved");
        assert_eq!(read(&dom, leaf, PropertyId::Color), "rgb(9, 8, 7)", "and #mid came with it");
    }

    // ── And the OTHER per-call cost, which was never a constant ────────────────────
    //
    // `getComputedStyle` runs the "update animations" step first, and its skipped path -- the one a
    // document whose fingerprint has not moved takes, which is every call in a read loop -- cloned
    // and scanned one slot per NODE. So the call was O(document) and reading a style off every
    // element was O(elements^2), on a document that declares no animation at all. Measured with the
    // SAME 2,000 `getComputedStyle` calls reading nothing:
    //
    //   document elements   2,010     8,010    32,010
    //   before              5.95 us  10.10 us  32.35 us
    //   after               4.55 us   5.20 us   8.25 us
    //
    // AS A COUNT: the walk is as long as what the document DECLARES, which for a document with no
    // animations is the document element and nothing else, whatever its size.
    #[test]
    fn the_animation_pass_walks_what_the_document_declares_not_how_big_it_is() {
        let media = MediaContext::default();
        let plain = |n: usize| Dom::parse(&format!("<body>{}</body>", "<span>x</span>".repeat(n)));

        let small = Page::default();
        small.declarative_update(&plain(50), &media);
        let big = Page::default();
        big.declarative_update(&plain(4000), &media);
        assert_eq!(
            (small.declarative_styled(), big.declarative_styled()),
            (1, 1),
            "a document that declares no animation styles only its document element, at any size",
        );

        // ...and it does grow with what IS declared, so the number is measuring something.
        let animated = Page::default();
        let dom = Dom::parse(
            "<head><style>.spin { animation: turn 1s linear infinite }</style></head>\
             <body><div><span class='spin'>a</span><span class='spin'>b</span></div>\
             <span>plain</span></body>",
        );
        animated.declarative_update(&dom, &media);
        assert!(
            animated.declarative_styled() > 1,
            "two animated spans and their chain left {} styled",
            animated.declarative_styled(),
        );
        assert!(
            animated.declarative_styled() < 8,
            "and only those: {} of a 7-element document",
            animated.declarative_styled(),
        );
    }

    // In a document with no shadow root, no container query and no `cq*` unit, a computed
    // style is the page style pass's, kept and brought up to date: reading every element walks no
    // ancestor chain, and answers what the chain walk answers, before a write and after one.
    #[test]
    fn computed_styles_come_from_the_page_style_pass_and_agree_with_the_chain() {
        let markup = "<head><style>.a { color: rgb(1, 2, 3) } .a > p { margin-left: 3px } \
                      div p:first-child { width: 7px } .f { display: flex } .f > * { min-width: auto }</style></head>\
                      <body style='font-size: 20px'><div class='a'><p>x</p><p class='f'><span>y</span></p></div>\
                      <div><p style='width: 2rem'>z</p></div></body>";
        let mut kept_dom = Dom::parse(markup);
        let mut chain_dom = Dom::parse(markup);
        let (kept, chain) = (Page::default(), chain_page());
        let media = MediaContext::default();
        let properties = [PropertyId::Color, PropertyId::MarginLeft, PropertyId::Width, PropertyId::Display, PropertyId::FontSize, PropertyId::MinWidth];
        let compare = |kept_dom: &Dom, chain_dom: &Dom| {
            for n in 0..kept_dom.len() as NodeId {
                if !matches!(kept_dom.node(n).kind, NodeKind::Element { .. }) {
                    continue;
                }
                for p in properties {
                    assert_eq!(kept.computed_value(kept_dom, n, &media, p, None), chain.computed_value(chain_dom, n, &media, p, None), "node {n} {}", p.name());
                }
            }
        };
        // The page has styled its document (a layout), so its reads are that pass's.
        kept.layout(&kept_dom, &media);
        compare(&kept_dom, &chain_dom);
        // (Under KORVUS_VERIFY_INCREMENTAL the chain is walked too, to compare.)
        let walked = |page: &Page| if verify::enabled() { 0 } else { page.cascades_run() };
        assert_eq!(walked(&kept), 0, "no chain walked");
        assert!(chain.cascades_run() > 0);
        // A write above an element moves what it inherits and matches.
        for dom in [&mut kept_dom, &mut chain_dom] {
            let first = dom.get_elements_by_tag_name("div")[0];
            dom.set_attribute(first, "class", "f");
            let body = dom.get_elements_by_tag_name("body")[0];
            dom.set_attribute(body, "style", "font-size: 30px; color: rgb(9, 8, 7)");
        }
        compare(&kept_dom, &chain_dom);
        assert_eq!(walked(&kept), 0, "no chain walked after the writes either");
    }

    // A `::before`/`::after` read off an element the page style pass styled skips matching
    // where the pass found no rule, and answers what matching it alone gives: before a write and after.
    #[test]
    fn pseudo_element_styles_agree_with_matching_them_alone() {
        let markup = "<head><style>.q::before { content: 'q'; color: rgb(1, 2, 3) } .a::after { content: attr(data-t); margin-left: 3px }
                      .f { display: flex } .f::before { content: 'f' } p::after { content: ''; background-image: url(x.png) }
                      .none::before { content: 'n'; display: none }</style></head>
                      <body><div class='q'>x</div><p class='a' data-t='t'>y</p><div class='f q'><span>z</span></div><p class='none'>w</p><b class='q a' data-t='u'>v</b><i>plain</i></body>";
        let mut kept_dom = Dom::parse(markup);
        let mut chain_dom = Dom::parse(markup);
        let (kept, chain) = (Page::default(), chain_page());
        let media = MediaContext::default();
        kept.layout(&kept_dom, &media);
        let properties = [PropertyId::Content, PropertyId::Color, PropertyId::MarginLeft, PropertyId::Display, PropertyId::BackgroundImage];
        let compare = |kept_dom: &Dom, chain_dom: &Dom| {
            for n in 0..kept_dom.len() as NodeId {
                if !matches!(kept_dom.node(n).kind, NodeKind::Element { .. }) {
                    continue;
                }
                for pseudo in ["before", "after"] {
                    for p in properties {
                        assert_eq!(
                            kept.computed_pseudo_value(kept_dom, n, pseudo, &media, p, None),
                            chain.computed_pseudo_value(chain_dom, n, pseudo, &media, p, None),
                            "node {n} ::{pseudo} {}", p.name()
                        );
                    }
                }
            }
        };
        compare(&kept_dom, &chain_dom);
        for dom in [&mut kept_dom, &mut chain_dom] {
            let first = dom.get_elements_by_tag_name("div")[0];
            dom.set_attribute(first, "class", "a f");
            dom.set_attribute(first, "data-t", "new");
            let plain = dom.get_elements_by_tag_name("i")[0];
            dom.set_attribute(plain, "class", "q");
        }
        kept.layout(&kept_dom, &media);
        compare(&kept_dom, &chain_dom);
    }

    // The page style pass answers only where it is the chain's function, and only while
    // bringing it up to date costs what changed. Under a shadow host it is not the chain's -- a slotted
    // element inherits from its slot there and from its host in the chain -- nor for a node outside
    // the document; before the first pass, and after a sheet change the pass cannot style from the
    // changed sheet alone (one whose rule has no subject key), the pass would style the whole
    // document.
    #[test]
    fn the_page_style_pass_answers_only_where_it_is_the_chains_function() {
        let page = Page::default();
        let media = MediaContext::default();
        let mut dom = Dom::parse("<head><style>p { color: red }</style></head><body><p id='p'>x</p></body>");
        let p = element(&dom, "p");
        let detached = dom.create_element("div");
        let kept = |dom: &Dom, node: NodeId| {
            page.sync_document_sheets(dom);
            let mut state = page.state.lock().unwrap();
            Page::ensure_matcher(&mut state);
            Page::kept_style(&mut state, dom, node, &media).is_some()
        };
        assert!(!kept(&dom, p), "no pass yet");
        page.layout(&dom, &media);
        assert!(kept(&dom, p), "an element of a document with no shadow root, after a pass");
        assert!(!kept(&dom, detached), "a detached element");
        let style = dom.get_elements_by_tag_name("style")[0];
        let text = dom.first_child(style).unwrap();
        dom.set_node_value(text, "p { color: blue }");
        assert!(kept(&dom, p), "a sheet changed since the pass, styled from that sheet alone");
        dom.set_node_value(text, "* { color: blue }");
        assert!(!kept(&dom, p), "a sheet changed since the pass, with a rule any element can match");
        page.layout(&dom, &media);
        assert!(kept(&dom, p), "and laid out again");
        // A shadow host's light child, slotted: styled in the flat tree (under the slot), read through
        // the DOM (under the host).
        let body = dom.get_elements_by_tag_name("body")[0];
        let host = dom.create_element("div");
        let light = dom.create_element("span");
        dom.append_child(host, light);
        dom.append_child(body, host);
        let shadow = dom.attach_shadow(host, korvus_dom::ShadowRootMode::Open).unwrap();
        let slot = dom.create_element("slot");
        dom.append_child(shadow, slot);
        page.layout(&dom, &media);
        assert!(page.state.lock().unwrap().doc_styles[&dom.root()].keys[light as usize].is_some(), "the slotted child is styled");
        assert!(!kept(&dom, light), "a shadow host's light child");
        assert!(kept(&dom, host), "the host itself");
        assert!(kept(&dom, p), "an element no host is above, in a document with a shadow root");
    }

    // `computed_style` walks its chains through the same `CascadeMemo` the whole-document
    // passes use, so alike elements -- the same matched rules, inline block and parent style -- are
    // computed once between them. On the P9c fixture (4,000 `<div class=cN><p>item <span>x</span>`)
    // that is most of what the first `getComputedStyle(e).color` over every element cost.
    //
    // AS A COUNT: every element is still cascaded in the sense `cascades_run` counts (a chain walk),
    // a handful are computed, and each one still answers for ITSELF -- the class that differs, the
    // inline block, the subtree under a different parent, a later mutation.
    #[test]
    fn alike_elements_compute_one_style_between_them() {
        let page = Page::default();
        let items = "<li class='a'>x</li><li class='b'>y</li>".repeat(1000);
        let mut dom = Dom::parse(&format!(
            "<head><style>.a {{ color: rgb(1, 2, 3) }} .b {{ color: rgb(4, 5, 6) }} \
             #other li {{ margin-left: 7px }}</style></head>\
             <body><ul>{items}</ul><ul id='other'><li class='a' id='far'>z</li></ul>\
             <li class='a' id='inline' style='color: rgb(9, 9, 9)'>w</li></body>"
        ));
        reads_walk_chains(&page);
        let media = MediaContext::default();
        let before = page.document_cascades_run();
        let (walked, elements) = read_every_element(&page, &dom);
        assert_eq!(walked, elements as u64, "every element's chain is still walked");
        let computed = page.document_cascades_run() - before;
        assert!(elements > 2000);
        assert!(computed < 20, "{elements} elements computed {computed} styles");

        let value = |dom: &Dom, id: &str, p| page.computed_value(dom, element(dom, id), &media, p, None).unwrap();
        let lis: Vec<NodeId> = (0..dom.len() as NodeId)
            .filter(|&n| dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("li")))
            .collect();
        assert_eq!(page.computed_value(&dom, lis[0], &media, PropertyId::Color, None).unwrap(), "rgb(1, 2, 3)");
        assert_eq!(page.computed_value(&dom, lis[1], &media, PropertyId::Color, None).unwrap(), "rgb(4, 5, 6)");
        assert_eq!(page.computed_value(&dom, lis[1998], &media, PropertyId::MarginLeft, None).unwrap(), "0px");
        assert_eq!(value(&dom, "far", PropertyId::MarginLeft), "7px");
        assert_eq!(value(&dom, "inline", PropertyId::Color), "rgb(9, 9, 9)");

        // A mutation: the element answers its new class, and a memo-less page agrees.
        dom.set_attribute(lis[0], "class", "b");
        assert_eq!(page.computed_value(&dom, lis[0], &media, PropertyId::Color, None).unwrap(), "rgb(4, 5, 6)");
        assert_eq!(page.computed_value(&dom, lis[2], &media, PropertyId::Color, None).unwrap(), "rgb(1, 2, 3)");
        let cold = Page::default();
        for n in [lis[0], lis[1], lis[2], element(&dom, "far"), element(&dom, "inline")] {
            for p in [PropertyId::Color, PropertyId::MarginLeft, PropertyId::Display] {
                assert_eq!(page.computed_value(&dom, n, &media, p, None), cold.computed_value(&dom, n, &media, p, None));
            }
        }
    }

    // A `--var` on `:root` made EVERY element below it rebuild, merge and hash an
    // inherited half -- the parent's custom properties were copied in a way that marked the half
    // changed -- and copy every custom property into Strings to resolve a map nothing read. As a
    // COUNT: 200 elements that differ only in a non-inherited property build no inherited half of
    // their own, and every one still reads the custom properties it inherits.
    #[test]
    fn inherited_custom_properties_do_not_make_every_element_rebuild_its_inherited_half() {
        let vars: String = (0..50).map(|i| format!("--v{i}: {i}px;")).collect();
        let rules: String = (0..200).map(|i| format!(".m{i} {{ margin-left: {i}px }}")).collect();
        let items: String = (0..200).map(|i| format!("<div class='m{i}'>x</div>")).collect();
        let dom = Dom::parse(&format!(
            "<head><style>:root {{ {vars} }} {rules} .own {{ --v7: 70px; width: var(--v3) }}</style></head>\
             <body>{items}<div id='own' class='own'>y</div></body>"
        ));
        let page = Page::default();
        let media = MediaContext::default();
        let (_, elements) = read_every_element(&page, &dom);
        assert!(elements > 200);
        let built = page.state.lock().unwrap().arena.inherited_interns();
        assert!(built < 12, "{elements} elements built {built} inherited halves");

        let divs: Vec<NodeId> = (0..dom.len() as NodeId)
            .filter(|&n| dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("div")))
            .collect();
        assert_eq!(page.computed_custom(&dom, divs[5], &media, "--v7").as_deref(), Some("7px"));
        assert_eq!(page.computed_value(&dom, divs[5], &media, PropertyId::MarginLeft, None).unwrap(), "5px");
        let own = element(&dom, "own");
        assert_eq!(page.computed_custom(&dom, own, &media, "--v7").as_deref(), Some("70px"));
        assert_eq!(page.computed_custom(&dom, own, &media, "--v49").as_deref(), Some("49px"));
        assert_eq!(page.computed_value(&dom, own, &media, PropertyId::Width, None).unwrap(), "3px");
    }

    // ── The whole-document passes, which a write-then-read loop repeats ─────────────
    //
    // Sampling the engine thread while the parity driver waited 30 s for `Page.navigate` found the
    // cascade under `Page::layout` in 93 % of samples on asana.com and 77 % on jetbrains.com: every
    // geometry read after a DOM write re-laid the document out, and that cascaded every element. As
    // COUNTS: a second layout of an unchanged document cascades nothing, a list of identical items
    // cascades once, and a one-element write cascades that element (and what inherits from it).
    #[test]
    fn a_layout_cascades_what_changed_not_the_document() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse(&format!(
            "<head><style>li {{ color: rgb(1, 2, 3) }} .hot {{ color: rgb(9, 9, 9) }}</style></head>\
             <body><ul id='list'>{}</ul><p id='after'>tail</p></body>",
            "<li>item</li>".repeat(1000)
        ));
        let before = page.document_cascades_run();
        page.layout(&dom, &media);
        let first = page.document_cascades_run() - before;
        assert!(
            first < 20,
            "1,000 identical <li>s under one <ul> share one cascade; the pass ran {first} for 1,004 elements"
        );

        let at = page.document_cascades_run();
        page.layout(&dom, &media);
        assert_eq!(page.document_cascades_run() - at, 0, "an unchanged document cascades nothing");

        // One element changes class: that element's inputs are new, nothing else's are.
        let li = (0..dom.len() as NodeId)
            .filter(|&n| dom.tag_name(n).is_some_and(|t| t.eq_ignore_ascii_case("li")))
            .nth(500)
            .expect("an li");
        dom.set_attribute(li, "class", "hot");
        let at = page.document_cascades_run();
        let laid = page.layout(&dom, &media);
        let after_write = page.document_cascades_run() - at;
        assert_eq!(after_write, 1, "one class write re-cascades that one element, ran {after_write}");

        // AND THE ANSWER IS THE ONE A PAGE WITH NO MEMO GIVES. The count above would be satisfied by
        // an engine that never re-cascaded at all; this is what rules that out.
        let cold = Page::default();
        let fresh = cold.layout(&dom, &media);
        for n in [li, element(&dom, "list"), element(&dom, "after")] {
            assert_eq!(laid.bounding_rect(n), fresh.bounding_rect(n), "node {n}");
        }
        assert_eq!(
            page.computed_value(&dom, li, &media, PropertyId::Color, None),
            Some("rgb(9, 9, 9)".to_string())
        );
    }

    // A `style` write between two layouts is the loop www.who.int ran ~280 times, re-matching 6,213
    // nodes each time. As counts: that layout reuses the matches; a class write, which matching
    // reads, does not; and either way the boxes are the ones a memo-less page lays out.
    // Inside one style pass the `:nth-*` caches are shared, keyed on node addresses. A move
    // between two passes changes the indices and no address, so the second pass reads the first's
    // entries unless they were emptied when it closed.
    #[test]
    fn a_move_between_layouts_is_never_read_through_the_nth_cache() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse(
            "<head><style>div { height: 10px } div:nth-child(2) { height: 40px } div:nth-last-child(1) { width: 30px }</style></head>\
             <body id='body'><div id='a'>a</div><div id='b'>b</div><div id='c'>c</div></body>",
        );
        let (body, a, b, c) = (element(&dom, "body"), element(&dom, "a"), element(&dom, "b"), element(&dom, "c"));
        let laid = page.layout(&dom, &media);
        assert_eq!((laid.bounding_rect(a).height, laid.bounding_rect(b).height), (10.0, 40.0));
        assert_eq!(laid.bounding_rect(c).width, 30.0);
        dom.insert_before(body, c, a);
        let laid = page.layout(&dom, &media);
        assert_eq!(
            (laid.bounding_rect(c).height, laid.bounding_rect(a).height, laid.bounding_rect(b).height),
            (10.0, 40.0, 10.0),
            "c a b: a is the second child now"
        );
        assert_eq!(laid.bounding_rect(b).width, 30.0, "and b the last");
    }

    #[test]
    fn a_style_write_between_layouts_keeps_the_matches_and_a_class_write_does_not() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse(
            "<head><style>.box { height: 10px } .tall { height: 40px } div + div { width: 70px }</style></head>\
             <body><div id='a' class='box'>a</div><div id='b' class='box'>b</div></body>",
        );
        let (a, b) = (element(&dom, "a"), element(&dom, "b"));
        let cold = |dom: &Dom, n: NodeId| Page::default().layout(dom, &media).bounding_rect(n);
        page.layout(&dom, &media);
        assert_eq!(page.layouts_reusing_matches(), 0);
        let all = page.elements_matched();

        // A write matches its own element again and keeps every other element's rules.
        dom.set_attribute(a, "style", "height: 25px");
        let laid = page.layout(&dom, &media);
        assert_eq!((page.layouts_reusing_matches(), page.elements_matched() - all), (1, 1), "a style write matches only its element");
        assert_eq!(laid.bounding_rect(a).height, 25.0);
        assert_eq!((laid.bounding_rect(a), laid.bounding_rect(b)), (cold(&dom, a), cold(&dom, b)));

        dom.set_attribute(b, "class", "tall");
        let laid = page.layout(&dom, &media);
        assert_eq!((page.layouts_reusing_matches(), page.elements_matched() - all), (2, 2), "so does a class no other selector reads past it");
        assert_eq!(laid.bounding_rect(b).height, 40.0);
        assert_eq!((laid.bounding_rect(a), laid.bounding_rect(b)), (cold(&dom, a), cold(&dom, b)));

        dom.remove_attribute(a, "style");
        let laid = page.layout(&dom, &media);
        assert_eq!((page.layouts_reusing_matches(), page.elements_matched() - all), (3, 3), "and removing a style attribute");
        assert_eq!((laid.bounding_rect(a), laid.bounding_rect(b)), (cold(&dom, a), cold(&dom, b)));
    }

    // ...except for a rule whose selector READS the style attribute, which a reusing layout matches
    // again: into matching, out of matching, and through a rule that has a second selector that
    // does not read it (the rule applies once, at the higher specificity, exactly as full matching
    // has it).
    #[test]
    fn a_rule_on_the_style_attribute_is_matched_again_by_a_reusing_layout() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse(
            "<head><style>div { height: 10px; width: 10px } :is(div[style*='--big']) { height: 90px } \
             #b, div[style*='--wide'] { width: 50px } div.w { width: 20px }</style></head>\
             <body><div id='a'>a</div><div id='b' class='w'>b</div></body>",
        );
        let (a, b) = (element(&dom, "a"), element(&dom, "b"));
        let cold = |dom: &Dom, n: NodeId| Page::default().layout(dom, &media).bounding_rect(n);
        let check = |page: &Page, dom: &Dom| {
            let laid = page.layout(dom, &media);
            assert_eq!((laid.bounding_rect(a), laid.bounding_rect(b)), (cold(dom, a), cold(dom, b)));
            laid
        };
        assert_eq!(check(&page, &dom).bounding_rect(a).height, 10.0);

        dom.set_attribute(a, "style", "--big: 1");
        assert_eq!(check(&page, &dom).bounding_rect(a).height, 90.0, "into matching");
        dom.set_attribute(a, "style", "--wide: 1");
        let laid = check(&page, &dom);
        assert_eq!((laid.bounding_rect(a).height, laid.bounding_rect(a).width), (10.0, 50.0), "one out, one in");
        // `#b` wins over `div.w` through the rule's id selector, with or without its style selector.
        dom.set_attribute(b, "style", "--wide: 1");
        assert_eq!(check(&page, &dom).bounding_rect(b).width, 50.0, "the higher specificity of the two");
        dom.remove_attribute(a, "style");
        assert_eq!(check(&page, &dom).bounding_rect(a).width, 10.0, "out of matching");
        assert_eq!(page.layouts_reusing_matches(), 4, "and every one of those layouts reused the rest");
    }

    // The memo's key is the cascade's whole input, so a change to any document-wide input it does
    // NOT key on must drop it: a sheet edit, and the viewport. Asserted against a memo-less page.
    #[test]
    fn the_layout_memo_does_not_survive_a_sheet_edit_or_a_viewport_change() {
        let page = Page::default();
        let dom = Dom::parse(
            "<head><style>#t { width: 100px }</style></head><body><div id='t'>x</div></body>",
        );
        let t = element(&dom, "t");
        let media = MediaContext::default();
        assert_eq!(page.layout(&dom, &media).bounding_rect(t).width, 100.0);

        page.add_sheet(Origin::Author, None, "#t { width: 30px }");
        assert_eq!(page.layout(&dom, &media).bounding_rect(t).width, 30.0, "a sheet added");

        let narrow = MediaContext { viewport_width: 300.0, ..MediaContext::default() };
        page.add_sheet(Origin::Author, None, "#t { width: 50vw }");
        page.layout(&dom, &media);
        assert_eq!(page.layout(&dom, &narrow).bounding_rect(t).width, 150.0, "the viewport moved");
        let cold = Page::default();
        cold.sync_document_sheets(&dom); // the same sheet order: the document's `<style>` first
        cold.add_sheet(Origin::Author, None, "#t { width: 30px }");
        cold.add_sheet(Origin::Author, None, "#t { width: 50vw }");
        assert_eq!(
            page.layout(&dom, &narrow).bounding_rect(t),
            cold.layout(&dom, &narrow).bounding_rect(t),
            "and a memo-less page agrees",
        );
    }

    // The skipped animation update -- every `getComputedStyle` over an unchanged document -- used to
    // re-derive the spec list from every styled node. tc39.es/ecma262 and supabase.com spent 15/19
    // and 13/17 samples there while `Runtime.evaluate` went unanswered. As a count: only a rescan
    // reads specs, and a skipped update hands back the same list.
    #[test]
    fn a_skipped_animation_update_reads_no_specs() {
        let media = MediaContext::default();
        let page = Page::default();
        let dom = Dom::parse(&format!(
            "<head><style>@keyframes turn {{ to {{ opacity: 0 }} }} \
             span {{ transition: color 1s }} .spin {{ animation: turn 1s infinite }}</style></head>\
             <body>{}<b class='spin'>a</b></body>",
            "<span>x</span>".repeat(500)
        ));
        // Twice: the first update's own `<style>` sync moves the fingerprint it was keyed on, so the
        // second rescans as well (pre-existing, and not what this test is about).
        page.declarative_update(&dom, &media);
        let rescan = page.declarative_update(&dom, &media);
        assert!(rescan.rescanned);
        let walks = page.declarative_spec_walks();
        assert!(walks > 500, "the rescan read every styled node: {walks}");
        for _ in 0..100 {
            let skipped = page.declarative_update(&dom, &media);
            assert!(!skipped.rescanned);
            assert_eq!(skipped.animations, rescan.animations, "the skipped update's list is the rescan's");
        }
        assert_eq!(page.declarative_spec_walks(), walks, "100 skipped updates read no spec");
        assert_eq!(rescan.animations.len(), 1, "one animated <b>");
    }

    // A rescan asks "does a DECLARING rule match this element", and it used to answer by matching
    // every rule of the document against every element. www.who.int spent 19 of 30 samples there
    // once the cascade was memoised. As a count: a document with a hundred ordinary rules on every
    // element and one animation rule on two elements considers two selectors, and finds both.
    #[test]
    fn the_animation_rescan_matches_only_the_rules_that_declare_one() {
        let media = MediaContext::default();
        let page = Page::default();
        let rules: String = (0..100).map(|i| format!(".c{i} {{ color: rgb({i}, 0, 0) }} ")).collect();
        let classes: String = (0..100).map(|i| format!("c{i} ")).collect();
        let mut dom = Dom::parse(&format!(
            "<head><style>{rules} @keyframes turn {{ to {{ opacity: 0 }} }} \
             .spin {{ animation: turn 1s infinite }}</style></head>\
             <body id='body'>{}<i class='{classes} spin'>a</i><i class='spin'>b</i><p id='p'>c</p></body>",
            format!("<b class='{classes}'>x</b>").repeat(1000)
        ));
        // The first rescan asks every element, and tests only the declaring rule.
        let update = page.declarative_update(&dom, &media);
        assert!(update.rescanned);
        assert_eq!(page.declarative_selector_tests(), 2, "the .spin selector, on its two elements");
        assert_eq!(update.animations.len(), 2, "and both animations are found: {:?}", update.animations);
        // A write no selector reads restyles nothing, and the rescan that follows asks
        // nothing again -- nor does a `data-*` write.
        let body = element(&dom, "body");
        for name in ["title", "data-x"] {
            dom.set_attribute(body, name, "1");
            let at = page.declarative_selector_tests();
            let update = page.declarative_update(&dom, &media);
            assert!(update.rescanned);
            assert_eq!(page.declarative_selector_tests() - at, 0, "a {name} write re-matches nothing");
            assert_eq!(update.animations.len(), 2, "and the animations are the same: {:?}", update.animations);
        }
        // A class a declaring rule reads, on an element with no element under it: it, and only it, is
        // asked again -- and answered from the rules the page style pass matched it to, no selector tested.
        dom.set_attribute(element(&dom, "p"), "class", "spin");
        let at = page.declarative_selector_tests();
        let update = page.declarative_update(&dom, &media);
        assert_eq!(page.declarative_selector_tests() - at, 0, "the <p>'s matched rules answer");
        assert_eq!(update.animations.len(), 3, "and its animation is found: {:?}", update.animations);
    }

    // A rescan keeps each element's last cascade while its inputs hold. An inline `style`
    // rewrite changes no match and no mark, so the attribute is the input that has to tell.
    #[test]
    fn a_rescan_cascades_again_an_element_whose_inline_style_changed() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse(
            "<head><style>div { transition: width 1s }</style></head><body><div id='a' style='width: 10px'>a</div><p>b</p></body>",
        );
        page.declarative_update(&dom, &media);
        let a = element(&dom, "a");
        let to = |page: &Page, dom: &Dom| page.declarative_update(dom, &media).transitions.iter().map(|t| t.to.clone()).collect::<Vec<_>>();
        dom.set_attribute(a, "style", "width: 20px");
        assert_eq!(to(&page, &dom), vec!["20px"]);
        dom.set_attribute(a, "style", "width: 30px");
        assert_eq!(to(&page, &dom), vec!["30px"]);
        dom.set_attribute(a, "data-x", "1");
        assert!(to(&page, &dom).is_empty(), "a write that changes no input starts nothing");
    }

    // A declaring rule that names a `data-*` attribute answers again after a `data-*` write, and a
    // `[style]` one after a `style` write -- from the rules the page style pass matched the element to, so
    // no declaring selector is tested again (www.mapbox.com rewrites `style` on every frame).
    #[test]
    fn the_animation_rescan_matches_again_only_rules_that_read_what_was_written() {
        let media = MediaContext::default();
        let tests_after = |css: &str, key: &str, value: &str| {
            let page = Page::default();
            let mut dom = Dom::parse(&format!(
                "<head><style>@keyframes turn {{ to {{ opacity: 0 }} }} {css}</style></head>\
                 <body id='body'><i id='i' data-on='1' style='color: red'>a</i><b>b</b></body>"
            ));
            let first = page.declarative_update(&dom, &media);
            let i = element(&dom, "i");
            dom.set_attribute(i, key, value);
            let at = page.declarative_selector_tests();
            let update = page.declarative_update(&dom, &media);
            (page.declarative_selector_tests() - at, first.animations.len(), update.animations.len())
        };
        let data_rule = "[data-on='1'] { animation: turn 1s infinite }";
        assert_eq!(tests_after(data_rule, "style", "color: blue"), (0, 1, 1), "a style write leaves a data-* rule's answer");
        assert_eq!(tests_after(data_rule, "data-on", "2"), (0, 1, 0), "a data-* write: the rule stops matching");
        let style_rule = "[style*='blue'] { animation: turn 1s infinite }";
        assert_eq!(tests_after(style_rule, "style", "color: blue"), (0, 0, 1), "a style write: a style rule starts matching");
    }

    // Measured on Chrome 150 (`tools/realweb/seed/ss/scope.html`): a shadow tree's
    // sheet styles that tree and its host (`:host`) and nothing else; the document's sheet styles
    // nothing inside the shadow tree; and the shadow tree inherits from its host.
    #[test]
    fn a_shadow_trees_sheet_styles_its_own_tree_and_host_only() {
        let page = Page::default();
        let media = MediaContext::default();
        let mut dom = Dom::parse(
            "<style>.in { border-left: 5px solid } #host { color: rgb(10, 20, 30) }</style>\
             <div id='light' part='button'>l</div><x-card id='host'></x-card>",
        );
        let host = element(&dom, "host");
        let root = dom.attach_shadow(host, korvus_dom::ShadowRootMode::Open).unwrap();
        dom.set_inner_html(root, "<style>:host { display: block } [part~=button] { border-top: 4px solid }</style><div class='in' part='button' id='in'>i</div>");
        let inner = (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "id") == Some("in")).unwrap();
        let value = |node: NodeId, p: PropertyId| page.computed_value(&dom, node, &media, p, None).unwrap();
        assert_eq!(value(element(&dom, "light"), PropertyId::BorderTopWidth), "0px", "no leak into the document");
        assert_eq!(value(inner, PropertyId::BorderTopWidth), "4px", "the tree's own rule");
        assert_eq!(value(inner, PropertyId::BorderLeftWidth), "0px", "no document rule inside");
        assert_eq!(value(inner, PropertyId::Color), "rgb(10, 20, 30)", "inherited from the host");
        assert_eq!(value(host, PropertyId::Display), "block", ":host");
    }

    // A rescan that only `style` attribute writes caused matches nothing again -- a
    // `style` write cannot change which rules match unless a selector names `style` -- and still
    // sees what the write declared.
    #[test]
    fn a_rescan_after_a_style_write_reuses_the_matches() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse(
            "<head><style>@keyframes turn { to { opacity: 0 } } .spin { animation: turn 1s infinite }</style></head>\
             <body><i class='spin'>a</i><i id='t'>b</i></body>",
        );
        page.declarative_update(&dom, &media);
        let t = element(&dom, "t");
        dom.set_attribute(t, "data-x", "1");
        page.declarative_update(&dom, &media);
        let at = page.declarative_selector_tests();
        dom.set_attribute(t, "style", "animation: turn 2s infinite");
        let update = page.declarative_update(&dom, &media);
        assert!(update.rescanned);
        assert_eq!(page.declarative_selector_tests() - at, 0, "nothing matched again");
        assert_eq!(update.animations.len(), 2, "the inline animation is found too: {:?}", update.animations);
    }

    // `computed_values` enumerates all 475 in one pass, and it has to apply the same rule the
    // single-property path does -- otherwise `cs.width` and `cs[i]` disagree, which is a two-line
    // detect.
    #[test]
    fn the_enumeration_and_the_single_property_path_give_the_same_answer() {
        let page = Page::default();
        let dom = Dom::parse("<body style='margin:0'><div id='t' style='width:50%'>x</div></body>");
        let node = element(&dom, "t");
        let media = MediaContext::default();
        let all = page.computed_values(&dom, node, &media, None);
        for property in [PropertyId::Width, PropertyId::InlineSize, PropertyId::TransformOrigin] {
            let one = page.computed_value(&dom, node, &media, property, None).unwrap();
            let enumerated = all
                .iter()
                .find(|(name, _)| name == property.name())
                .map(|(_, v)| v.clone())
                .unwrap();
            assert_eq!(one, enumerated, "{}", property.name());
        }
    }

    // The whole point of `sync_document_sheets`: a document with stylesheets and no script used to
    // have no styles at all, because a sheet only existed once script read `styleEl.sheet`.
    #[test]
    fn a_page_that_never_touches_the_cssom_is_still_styled() {
        assert_eq!(
            computed("<style>p { color: red }</style><p id='t'>x</p>", "t", PropertyId::Color),
            "rgb(255, 0, 0)"
        );
    }

    #[test]
    fn the_user_agent_sheet_answers_for_an_element_nobody_styled() {
        assert_eq!(computed("<div id='t'>x</div>", "t", PropertyId::Display), "block");
        assert_eq!(computed("<span id='t'>x</span>", "t", PropertyId::Display), "inline");
        assert_eq!(computed("<li id='t'>x</li>", "t", PropertyId::Display), "list-item");
        assert_eq!(computed("<div id='t'>x</div>", "t", PropertyId::Color), "rgb(0, 0, 0)");
    }

    #[test]
    fn an_author_sheet_beats_the_user_agent_sheet() {
        assert_eq!(
            computed("<style>div { display: flex }</style><div id='t'>x</div>", "t", PropertyId::Display),
            "flex"
        );
    }

    #[test]
    fn an_inline_style_beats_the_sheet() {
        assert_eq!(
            computed(
                "<style>p { color: red }</style><p id='t' style='color: blue'>x</p>",
                "t",
                PropertyId::Color
            ),
            "rgb(0, 0, 255)"
        );
    }

    #[test]
    fn several_style_elements_cascade_in_document_order() {
        assert_eq!(
            computed(
                "<style>p { color: red }</style><style>p { color: blue }</style><p id='t'>x</p>",
                "t",
                PropertyId::Color
            ),
            "rgb(0, 0, 255)"
        );
    }

    // `rem` is the *document element's* font size, so the root has to be computed before anything
    // that mentions it -- including the root itself.
    #[test]
    fn rem_resolves_against_the_document_element() {
        assert_eq!(
            computed(
                "<style>html { font-size: 20px } p { margin-top: 2rem }</style><p id='t'>x</p>",
                "t",
                PropertyId::MarginTop
            ),
            "40px"
        );
    }

    #[test]
    fn editing_a_sheet_through_the_cssom_changes_what_elements_compute_to() {
        let page = Page::default();
        let dom = Dom::parse("<style id='s'>p { color: red }</style><p id='t'>x</p>");
        let media = MediaContext::default();
        let node = element(&dom, "t");
        assert_eq!(
            page.computed_value(&dom, node, &media, PropertyId::Color, None).unwrap(),
            "rgb(255, 0, 0)"
        );
        let sheet = page.sheet_for_owner(&dom, element(&dom, "s"));
        page.with_sheet_mut(sheet, |s| s.insert_rule("p { color: blue }", 1).unwrap());
        assert_eq!(
            page.computed_value(&dom, node, &media, PropertyId::Color, None).unwrap(),
            "rgb(0, 0, 255)",
            "the rule index was rebuilt"
        );
        // And the next sync must not overwrite the edit from the <style> element's text, which is
        // what comparing against the sheet's own (growing) value pool used to do.
        page.sync_document_sheets(&dom);
        assert_eq!(
            page.computed_value(&dom, node, &media, PropertyId::Color, None).unwrap(),
            "rgb(0, 0, 255)"
        );
    }

    // Script reading `styleEl.sheet` must get the sheet the cascade is already using, not a second
    // empty one that quietly shadows nothing.
    #[test]
    fn asking_for_a_style_elements_sheet_finds_the_one_already_in_use() {
        let page = Page::default();
        let dom = Dom::parse("<style id='s'>p { color: red }</style><p>x</p>");
        page.sync_document_sheets(&dom);
        let before = page.sheet_count();
        let sheet = page.sheet_for_owner(&dom, element(&dom, "s"));
        assert_eq!(page.sheet_count(), before, "no second sheet appeared");
        assert_eq!(page.with_sheet(sheet, |s| s.rules().len()), Some(1));
    }

    #[test]
    fn a_media_query_that_does_not_match_contributes_nothing() {
        let html = "<style>@media (min-width: 900px) { p { color: red } }</style><p id='t'>x</p>";
        let page = Page::default();
        let dom = Dom::parse(html);
        let node = element(&dom, "t");
        let wide = MediaContext::default();
        let narrow = MediaContext { viewport_width: 400.0, ..MediaContext::default() };
        assert_eq!(
            page.computed_value(&dom, node, &wide, PropertyId::Color, None).unwrap(),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            page.computed_value(&dom, node, &narrow, PropertyId::Color, None).unwrap(),
            "rgb(0, 0, 0)",
            "same parsed sheet, different viewport"
        );
    }

    #[test]
    fn a_text_node_has_no_computed_style() {
        let page = Page::default();
        let dom = Dom::parse("<p id='t'>hello</p>");
        let text = dom.children(element(&dom, "t"))[0];
        assert!(page.computed_style(&dom, text, &MediaContext::default()).is_none());
    }

    /// `getComputedStyle` enumerates what Chrome enumerates, in Chrome's order.
    ///
    /// This used to assert `values.len() == COUNT` -- everything the table knows. That was the right
    /// invariant while the table was 127 properties of our own choosing, and it is the wrong one now
    /// that the table is Chrome's 475 plus four the cascade needs and Chrome does not enumerate
    /// (`border-spacing`, `background-position-x`/`-y`, `page`). The count and the ORDER are both things a page
    /// reads -- `getComputedStyle(el).length` was answering 127 against Chrome's 475 -- so both are
    /// asserted, against the list recorded from the Brave on this machine.
    #[test]
    fn computed_style_enumerates_what_chrome_enumerates() {
        let page = Page::default();
        let dom = Dom::parse("<p id='t'>x</p>");
        let values = page.computed_values(&dom, element(&dom, "t"), &MediaContext::default(), None);
        assert_eq!(values.len(), crate::style::property::ENUMERATED.len());
        assert_eq!(values.len(), 476, "Chrome 152's computed-property count");
        assert_eq!(values[0].0, "accent-color", "and Chrome's order, which `cs[i]` exposes");
        assert_eq!(values[values.len() - 1].0, "-webkit-writing-mode");
        assert!(values.iter().any(|(p, v)| p == "display" && v == "block"));
        // The four the cascade knows are NOT among them, exactly as in Chrome.
        for hidden in ["border-spacing", "background-position-x", "background-position-y", "page"] {
            assert!(!values.iter().any(|(p, _)| p == hidden), "{hidden} is not enumerated by Chrome");
            assert!(
                crate::style::property::PropertyId::from_name(hidden).is_some(),
                "{hidden} is still a property the cascade knows"
            );
        }
    }

    // Custom properties inherit and are kept exactly as written -- no lowercasing, no parsing.
    #[test]
    fn custom_properties_cascade_inherit_and_are_enumerated() {
        let page = Page::default();
        let dom = Dom::parse(
            "<style>#outer { --brand: #f00; --Brand: blue }</style>\
             <div id='outer'><p id='t' style='--local: 1px'>x</p></div>",
        );
        let media = MediaContext::default();
        let node = element(&dom, "t");
        assert_eq!(page.computed_custom(&dom, node, &media, "--brand"), Some("#f00".into()));
        assert_eq!(
            page.computed_custom(&dom, node, &media, "--Brand"),
            Some("blue".into()),
            "custom property names are case-sensitive"
        );
        assert_eq!(page.computed_custom(&dom, node, &media, "--local"), Some("1px".into()));
        assert_eq!(page.computed_custom(&dom, node, &media, "--absent"), None);

        let values = page.computed_values(&dom, node, &media, None);
        assert_eq!(
            values.len(),
            crate::style::property::ENUMERATED.len() + 3,
            "enumerated alongside the rest"
        );
    }

    // -------- layout --------

    fn laid_out(html: &str) -> (Dom, LaidOut) {
        let page = Page::default();
        let dom = Dom::parse(html);
        let out = page.layout(&dom, &MediaContext::default());
        (dom, out)
    }

    /// Lay out into a NAMED viewport, so an expectation can be Chrome's own number.
    ///
    /// Every `scrollHeight` expectation below was measured on `/usr/bin/google-chrome`
    /// (150.0.7871.46) over CDP at a viewport of 1920×993, on the same markup. Laying out into
    /// anything else would make the numbers un-checkable against the browser that produced them.
    fn laid_out_in(html: &str, width: f32, height: f32) -> (Dom, LaidOut) {
        let page = Page::default();
        let dom = Dom::parse(html);
        let media = MediaContext { viewport_width: width, viewport_height: height, ..MediaContext::default() };
        let out = page.layout(&dom, &media);
        (dom, out)
    }

    /// Chrome's viewport in every measurement quoted in these tests.
    const CHROME_VIEWPORT: (f32, f32) = (1920.0, 993.0);

    // The mission, through the API a page actually calls.
    #[test]
    fn the_body_has_a_non_zero_bounding_rect() {
        let (dom, out) = laid_out("<!doctype html><html><body id='b'><p id='p'>hi</p></body></html>");
        let body = out.bounding_rect(element(&dom, "b"));
        assert_eq!(body.width, 1008.0, "1024 less the UA sheet's 8px margins");
        // The `<p>`'s UA `margin-block-start: 1em` collapses through the body's 8px.
        // Chrome 150, this markup: body rect (8, 16) with the doctype, (8, 8) without -- the
        // no-doctype answer is the quirks-mode `__qem` margin, and this engine has no quirks mode.
        assert_eq!((body.x, body.y), (8.0, 16.0));
    }

    // Rects are in *viewport* coordinates, not parent-relative ones -- which is the difference
    // between a rect a page can act on and one it cannot.
    #[test]
    fn a_nested_elements_rect_is_absolute_and_not_relative_to_its_parent() {
        let (dom, out) = laid_out(
            "<html><body><div style='margin-left: 100px; margin-top: 50px'>\
               <div id='inner' style='height: 10px'></div></div></body></html>",
        );
        let inner = out.bounding_rect(element(&dom, "inner"));
        assert_eq!(inner.x, 108.0, "8 from body, 100 from the wrapper -- horizontal margins add");
        // And vertical ones collapse: the wrapper's 50px top margin absorbs body's 8px rather than
        // adding to it, so this is 50 and not 58.
        assert_eq!(inner.y, 50.0);
    }

    #[test]
    fn the_offset_and_client_metrics_agree_with_the_box_model() {
        let (dom, out) = laid_out(
            "<html><body><div id='a' style='width: 100px; height: 40px; padding: 10px; \
               border: 2px solid red'></div></body></html>",
        );
        let a = element(&dom, "a");
        assert_eq!(out.offset_size(a), (124, 64), "border box");
        assert_eq!(out.client_size(a), (120, 60), "padding box: the border box less its borders");
        assert_eq!(out.border_widths(a), (2, 2), "clientTop/clientLeft ARE the border widths");
        assert_eq!(out.bounding_rect(a).width, 124.0);
    }

    // ── the scrolling box ────────────────────────────────────────────────────────
    //
    // Every number in this block is Chrome 150.0.7871.46's, measured over CDP on the same markup at
    // a 1920×993 viewport on 2026-09-13, not read off a spec. `scrollWidth`/`scrollHeight` used to
    // be a literal zero here (`cb_zero_uint`, "the scrolling box's size, which has no layout behind
    // it") and there was a layout behind it all along.

    /// The root's floor is the VIEWPORT, not its own box — the fact that makes
    /// `scrollHeight - innerHeight` a real extent on a short page instead of a negative number.
    #[test]
    fn a_page_shorter_than_the_viewport_has_the_viewport_as_its_scroll_height() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            // With `<!doctype html>`: without one Chrome 150 is in quirks mode and answers
            // `documentElement.offsetHeight` = the viewport (780x513 in a 780x600 window), not 50.
            "<!doctype html><html><head><style>html,body{margin:0;padding:0}</style></head>\
             <body><div id='box' style='height:50px'></div></body></html>",
            w,
            h,
        );
        let root = element(&dom, "box");
        let html_el = dom.parent(dom.parent(root).unwrap()).unwrap();
        // Chrome: de_scrollHeight 993, de_clientHeight 993, de_offsetHeight 50.
        assert_eq!(out.scroll_size(html_el), (1920, 993));
        assert_eq!(out.client_size(html_el), (1920, 993), "the root's clientHeight is the viewport");
        assert_eq!(out.offset_size(html_el), (1920, 50), "...and its offsetHeight is still its box");
        assert_eq!(out.document_scroll_extent(), (0, 0), "nothing to scroll");
        // Chrome: body.scrollHeight is 50 on this page -- a different number from the root's, which
        // is why the parity probe records both.
        let body = dom.parent(root).unwrap();
        assert_eq!(out.scroll_size(body), (1920, 50));
    }

    /// A page taller than the viewport: the extent is the content, and it is what `scrollTo` lands
    /// on. Chrome: `scrollTo(0, 2e6)` left `scrollY` at 4007 = 5000 - 993.
    #[test]
    fn a_page_taller_than_the_viewport_can_be_scrolled_by_exactly_the_difference() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            "<html><head><style>html,body{margin:0;padding:0}</style></head>\
             <body><div id='box' style='height:5000px'></div></body></html>",
            w,
            h,
        );
        let html_el = dom.parent(dom.parent(element(&dom, "box")).unwrap()).unwrap();
        assert_eq!(out.scroll_size(html_el).1, 5000);
        assert_eq!(out.client_size(html_el).1, 993);
        assert_eq!(out.document_scroll_extent(), (0, 4007), "Chrome's scrollY after scrollTo(0, 2e6)");
    }

    /// A scroll container's own END padding is part of its scrollable overflow region, and both
    /// axes carry it. Chrome, on a 200×300 content box with 7px padding and a 3px border holding a
    /// 1234×2345 child: `scrollHeight` 1248, `scrollWidth` 2359.
    #[test]
    fn a_scroll_containers_own_end_padding_is_inside_its_scrolling_area() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            "<html><head><style>html,body{margin:0;padding:0}</style></head><body>\
             <div id='sc' style='height:200px;width:300px;overflow:auto;border:3px solid black;padding:7px'>\
             <div style='height:1234px;width:2345px'></div></div></body></html>",
            w,
            h,
        );
        let sc = element(&dom, "sc");
        assert_eq!(out.scroll_size(sc), (2359, 1248), "child + BOTH paddings, on both axes");
        assert_eq!(out.offset_size(sc), (320, 220), "content box + padding + border");
        // With NO theme -- no persona -- nothing reserves a scrollbar, so the client box is the full
        // 314x214 padding box and the extent is the scrolling area less that.
        assert_eq!(out.client_size(sc), (314, 214));
        assert_eq!(out.scroll_extent(sc), (2045, 1034));

        // Under a theme with classic scrollbars both bars show, because the content
        // overflows both axes, and each takes its thickness from the client box and gives it to the
        // extent. Chrome under its Linux theme: client 299x199, extent 2060/1049. The thickness here
        // is deliberately not Chrome's -- the arithmetic is the claim, the number is the bundle's.
        let theme = spec::ThemeProfile { scrollbar_thickness: 11.0, ..Default::default() };
        let media = MediaContext { viewport_width: w, viewport_height: h, theme, ..MediaContext::default() };
        let out = Page::default().layout(&dom, &media);
        assert_eq!(out.scroll_size(sc), (2359, 1248), "no scrollbar touches the scrolling area");
        assert_eq!(out.client_size(sc), (314 - 11, 214 - 11));
        assert_eq!(out.scroll_extent(sc), (2045 + 11, 1034 + 11));
    }

    /// Overflow is reported by every box; only a scroll container can be scrolled through it.
    /// Chrome, on three 100px-tall divs each holding a 900px child: `scrollHeight` is 900 for all
    /// three, and `el.scrollTop = 2e6` leaves `visible` at 0, `clip` at 0 and `hidden` at 800.
    #[test]
    fn overflow_is_reported_by_every_box_but_only_a_scroll_container_scrolls() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            "<html><head><style>html,body{margin:0;padding:0}</style></head><body>\
             <div id='vis'  style='height:100px;overflow:visible'><div style='height:900px'></div></div>\
             <div id='clip' style='height:100px;overflow:clip'><div style='height:900px'></div></div>\
             <div id='hid'  style='height:100px;overflow:hidden'><div style='height:900px'></div></div>\
             </body></html>",
            w,
            h,
        );
        for id in ["vis", "clip", "hid"] {
            assert_eq!(out.scroll_size(element(&dom, id)).1, 900, "#{id} reports its overflow");
        }
        assert_eq!(out.scroll_extent(element(&dom, "vis")), (0, 0));
        assert_eq!(out.scroll_extent(element(&dom, "clip")), (0, 0));
        assert_eq!(out.scroll_extent(element(&dom, "hid")), (0, 800));
    }

    /// Overflow BEFORE the scroll origin is unreachable and is not in the number. Chrome leaves
    /// `documentElement.scrollWidth` at the viewport width for a box at `left: -500px`.
    #[test]
    fn overflow_to_the_left_of_the_scroll_origin_is_not_scrollable_width() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            "<html><head><style>html,body{margin:0;padding:0}</style></head><body>\
             <div id='b' style='position:absolute;left:-500px;top:0;height:100px;width:100px'></div>\
             </body></html>",
            w,
            h,
        );
        let html_el = dom.parent(dom.parent(element(&dom, "b")).unwrap()).unwrap();
        assert_eq!(out.scroll_size(html_el), (1920, 993));
        assert_eq!(out.document_scroll_extent(), (0, 0));
    }

    /// An absolutely positioned box below the fold DOES extend the document, even though the root's
    /// own border box is empty. Chrome: `documentElement.scrollHeight` 3100 with `offsetHeight` 0.
    #[test]
    fn an_absolutely_positioned_box_below_the_fold_extends_the_document() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            "<html><head><style>html,body{margin:0;padding:0}</style></head><body>\
             <div id='b' style='position:absolute;top:3000px;height:100px;width:50px'></div>\
             </body></html>",
            w,
            h,
        );
        let html_el = dom.parent(dom.parent(element(&dom, "b")).unwrap()).unwrap();
        assert_eq!(out.scroll_size(html_el).1, 3100);
        assert_eq!(out.document_scroll_extent().1, 2107, "Chrome's scrollY after scrollTo(0, 2e6)");
    }

    /// The root's own end padding is in the region, which is the one place the root differs from a
    /// box taffy already treats as a scroll container. Chrome: 5000 + 40 + 40.
    #[test]
    fn the_root_is_a_scroll_container_even_with_overflow_visible() {
        let (w, h) = CHROME_VIEWPORT;
        let (dom, out) = laid_out_in(
            "<html><head><style>html{padding:40px}body{margin:0;padding:0}</style></head>\
             <body><div id='b' style='height:5000px'></div></body></html>",
            w,
            h,
        );
        let html_el = dom.parent(dom.parent(element(&dom, "b")).unwrap()).unwrap();
        assert_eq!(out.scroll_size(html_el).1, 5080);
        assert_eq!(out.document_scroll_extent().1, 4087, "Chrome's scrollY after scrollTo(0, 2e6)");
        // `html { overflow: hidden }` does NOT stop a programmatic scroll -- Chrome still lands at
        // 4007 on a 5000px page. The root is the viewport's scrolling box whatever it says.
        let (dom, out) = laid_out_in(
            "<html><head><style>html{overflow:hidden}html,body{margin:0;padding:0}</style></head>\
             <body><div id='b' style='height:5000px'></div></body></html>",
            w,
            h,
        );
        let _ = element(&dom, "b");
        assert_eq!(out.document_scroll_extent().1, 4007);
    }

    /// `scrollHeight` is a `long` over a fractional layout, and Chrome rounds it HALF UP: 2000.4 →
    /// 2000, 2000.5 → **2001**, 2000.6 → 2001, 1999.1 → 1999. Measured rather than assumed, because
    /// truncation is just as plausible a reading of "long".
    #[test]
    fn a_fractional_scrolling_box_rounds_half_up() {
        let (w, h) = CHROME_VIEWPORT;
        for (css, expected) in [("1999.1", 1999), ("2000.4", 2000), ("2000.5", 2001), ("2000.6", 2001)] {
            let html = format!(
                "<html><head><style>html,body{{margin:0;padding:0}}</style></head>\
                 <body><div id='b' style='height:{css}px'></div></body></html>"
            );
            let (dom, out) = laid_out_in(&html, w, h);
            let html_el = dom.parent(dom.parent(element(&dom, "b")).unwrap()).unwrap();
            assert_eq!(out.scroll_size(html_el).1, expected, "height:{css}px");
        }
    }

    /// A text field's value is its scrolling area, even though the box tree never saw it: it lives
    /// in an attribute, so the child walk finds nothing inside the control. Chrome, on a
    /// 57-character value in a 100×50 field: `scrollWidth` 387 against a `clientWidth` of 104, and
    /// `scrollLeft = 10` sticks. A submit button's label overflows the same way and the button does
    /// NOT scroll (`scrollLeft` stays 0); a `number`, `date` or `checkbox` field reports neither.
    #[test]
    fn a_text_fields_value_is_a_scrolling_area_and_a_buttons_label_is_not() {
        let (w, h) = CHROME_VIEWPORT;
        let long = "qwertyuiopasddfghjklzxcvbnmqwertyuiopasddfghjklzxcvbnmqwer";
        let html = format!(
            "<!doctype html><html><head><style>html,body{{margin:0;padding:0}}input{{width:100px;height:50px}}\
             </style></head><body>\
             <input id='t' type='text' value='{long}'>\
             <input id='p' type='password' value='{long}'>\
             <input id='b' type='submit' value='{long}'>\
             <input id='n' type='number' value='{long}'>\
             <input id='c' type='checkbox' value='{long}'>\
             <input id='bare' value='{long}'>\
             <input id='empty' type='text'>\
             </body></html>"
        );
        let (dom, out) = laid_out_in(&html, w, h);
        let width_of = |id: &str| out.scroll_size(element(&dom, id)).0;
        let client = out.client_size(element(&dom, "t")).0;
        // Chrome 150: 104 -- the 100px content box plus the UA sheet's `padding-inline: 2px`, which
        // the sheet did not carry until its logical properties were kept.
        assert_eq!(client, 104, "the field itself is 100px wide, plus its UA padding");

        // The text field's scrolling area is its value, so it is far wider than the field.
        assert!(width_of("t") > 300, "a 57-character value is wider than 300px: {}", width_of("t"));
        assert_eq!(width_of("bare"), width_of("t"), "a missing `type` is `text`");
        // ...and the field can be scrolled through it, though its computed `overflow` is `clip`.
        assert!(out.scroll_extent(element(&dom, "t")).0 >= 10);

        // A password field is measured as BULLETS -- Chrome's 275 against text's 387 for the same
        // string is the width of one U+2022 per character, and measuring the real characters would
        // put the shape of the password into a number any script can read off `scrollWidth`.
        assert!(width_of("p") > 100, "a password field still has a scrolling area");
        assert_ne!(width_of("p"), width_of("t"), "and it is not the value's own width");

        // A button's label overflows and is reported, but the button does not scroll.
        assert!(width_of("b") > 300, "the label overflows: {}", width_of("b"));
        assert_eq!(out.scroll_extent(element(&dom, "b")), (0, 0), "a button is not a scroller");

        // Types whose value is not rendered as a run of text report the box and nothing more --
        // its PADDING box, which Chrome 150 measures on this markup as 104 for `number` and an empty
        // text field (UA `padding-inline: 2px`) and 100 for a checkbox (UA `padding: initial`).
        assert_eq!(width_of("n"), 104, "number");
        assert_eq!(width_of("c"), 100, "checkbox");
        assert_eq!(width_of("empty"), 104, "an empty value adds nothing");
    }

    /// An element that generated no box has no scrolling box either — zero, not the viewport.
    /// Chrome: `document.createElement("div").scrollHeight` is 0, and so is a `display: none` one's.
    #[test]
    fn an_element_with_no_box_has_no_scrolling_box() {
        let (dom, out) = laid_out("<html><body><div id='gone' style='display:none'></div></body></html>");
        assert_eq!(out.scroll_size(element(&dom, "gone")), (0, 0));
        assert_eq!(out.scroll_extent(element(&dom, "gone")), (0, 0));
    }

    // An element with no box has no geometry, and a browser answers with zeros rather than with
    // nothing at all.
    #[test]
    fn an_element_with_no_box_reads_as_zero_rather_than_as_missing() {
        let (dom, out) = laid_out(
            "<html><body><div id='gone' style='display: none'></div>\
               <span id='inline'>x</span></body></html>",
        );
        assert_eq!(out.bounding_rect(element(&dom, "gone")), Rect::ZERO);
        assert!(out.client_rects(element(&dom, "gone")).is_empty());
        assert_eq!(out.offset_size(element(&dom, "gone")), (0, 0));
        // An INLINE box is the other half of the sentence, and it is no longer the same half: its
        // geometry is a run of line boxes, and it has one. Chrome reports a `<span>` holding one
        // character at the body's content origin, one content area tall, and answers `clientWidth`
        // and `scrollWidth` of 0 for it — an inline box has no padding box and no scrolling area.
        let inline = element(&dom, "inline");
        let rect = out.bounding_rect(inline);
        assert_eq!((rect.x, rect.y), (8.0, 8.0), "at <body>'s content origin");
        assert!(rect.width > 0.0 && rect.height > 0.0, "a real box: {rect:?}");
        assert_eq!(out.client_rects(inline).len(), 1, "one line, one rect");
        assert_eq!(out.client_size(inline), (0, 0));
        assert_eq!(out.scroll_size(inline), (0, 0));
    }

    #[test]
    fn a_style_element_in_the_document_moves_the_boxes() {
        let (dom, out) = laid_out(
            "<html><head><style>#a { width: 200px; height: 30px }</style></head>\
             <body><div id='a'></div><div id='b' style='height: 5px'></div></body></html>",
        );
        assert_eq!(out.bounding_rect(element(&dom, "a")).width, 200.0);
        assert_eq!(out.bounding_rect(element(&dom, "b")).y, 38.0, "below a, plus body's margin");
    }

    /// Lay `html` out, apply `mutate`, and check the pass that styles only what changed against one that
    /// styles everything; the pass must also have been one that reused the rest.
    fn incremental_equals_full(html: &str, mutate: impl FnOnce(&mut Dom)) {
        let mut dom = Dom::parse(html);
        let page = Page::default();
        let media = MediaContext::default();
        page.layout(&dom, &media);
        mutate(&mut dom);
        page.sync_document_sheets(&dom);
        let root = dom.root();
        let mine = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Allowed, &mut Prev::Nothing));
        let full = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Nothing, &mut Prev::Nothing));
        assert_eq!(page.layouts_reusing_matches(), 1, "styled only what changed");
        if let Some(d) = Page::pass_difference(&dom, &mine, &mine.tree, &full) {
            panic!("{d}");
        }
    }

    /// A `:has()` that reads the hover chain, anchored OUTSIDE the chain (a sibling before
    /// it), so only the `:has()` invalidation can reach it.
    #[test]
    fn a_hover_change_re_asks_a_has_that_reads_hover() {
        let html = "<!doctype html><style>p:has(+ ul li:hover) { margin-left: 3px }</style><p id='p'>p</p><ul id='u'><li id='a'>a</li></ul>";
        let hover = |dom: &mut Dom| {
            let a = dom.get_element_by_id("a").unwrap();
            let chain: Vec<NodeId> = std::iter::successors(Some(a), |&n| dom.parent(n)).filter(|&n| dom.tag_name(n).is_some()).collect();
            dom.set_hover_chain(chain.into_iter().rev().collect());
        };
        let mut probe = Dom::parse(html);
        let p = probe.get_element_by_id("p").unwrap();
        let value = |dom: &Dom| Page::default().computed_value(dom, p, &MediaContext::default(), PropertyId::MarginLeft, None);
        let before = value(&probe);
        hover(&mut probe);
        assert_ne!(before, value(&probe), "the rule applies on hover");
        incremental_equals_full(html, hover);
    }

    /// An element moved out of the document into a detached tree has no style any more,
    /// though it still has a parent.
    #[test]
    fn a_move_into_a_detached_tree_forgets_the_style() {
        incremental_equals_full("<ul><li id='a' class='x'>a</li></ul>", |dom| {
            let a = dom.get_element_by_id("a").unwrap();
            let holder = dom.create_element("div");
            dom.append_child(holder, a);
        });
    }

    /// An animated value that moves while another document of the arena is laid out still
    /// cascades again at the page's next pass: each document keeps its own list.
    #[test]
    fn an_animated_value_moved_between_the_pages_passes_reaches_the_page_through_a_frames_pass() {
        let mut dom = Dom::parse("<div id='a'>a</div>");
        let a = element(&dom, "a");
        let frame = dom.create_html_document();
        let html = dom.create_element("html");
        dom.append_child(frame, html);
        let page = Page::default();
        let media = MediaContext::default();
        let root = dom.root();
        page.layout(&dom, &media);
        page.layout_document(&dom, frame, &media);
        let mut values = std::collections::HashMap::new();
        values.insert(a, vec![AnimatedValue { property: PropertyId::Width, value: "33px".into(), transition: false }]);
        page.set_animated(values);
        page.layout_document(&dom, frame, &media);
        let mine = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Allowed, &mut Prev::Nothing));
        let full = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Nothing, &mut Prev::Nothing));
        if let Some(d) = Page::pass_difference(&dom, &mine, &mine.tree, &full) {
            panic!("{d}");
        }
    }

    /// Lay `html` out, then for each step apply it and check the page's pass against one that reuses
    /// nothing. Returns how many passes styled a sheet change from the changed sheets alone.
    fn incremental_equals_full_over(html: &str, steps: &[&dyn Fn(&mut Dom, &Page)]) -> u64 {
        let mut dom = Dom::parse(html);
        let page = Page::default();
        let media = MediaContext::default();
        page.layout(&dom, &media);
        let root = dom.root();
        for (i, step) in steps.iter().enumerate() {
            step(&mut dom, &page);
            page.sync_document_sheets(&dom);
            let mine = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Allowed, &mut Prev::Nothing));
            let full = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Nothing, &mut Prev::Nothing));
            if let Some(d) = Page::pass_difference(&dom, &mine, &mine.tree, &full) {
                panic!("step {i}: {d}");
            }
        }
        let planned = page.state.lock().unwrap().sheet_passes;
        planned
    }

    /// A sheet that starts asking a form state, styled from that sheet alone: checking a box
    /// inserted before it (and after the pass from nothing, which tracks every control) restyles the
    /// label. The control's answers were kept under the user-agent sheet's `:disabled` alone.
    #[test]
    fn a_sheet_that_starts_asking_a_form_state_tracks_the_controls() {
        let html = "<!doctype html><style id='s'>.x { color: red }</style><div id='d'></div>";
        let planned = incremental_equals_full_over(
            html,
            &[
                &|dom, _| {
                    let d = dom.get_element_by_id("d").unwrap();
                    let _ = dom.set_inner_html(d, "<input id='c' type='checkbox'><label class='x'>l</label>");
                },
                &|dom, _| {
                    let s = dom.get_element_by_id("s").unwrap();
                    dom.set_text_content(s, ".x { color: red } input:checked + .x { width: 21px }");
                },
                &|dom, _| {
                    let c = dom.get_element_by_id("c").unwrap();
                    dom.set_checkedness(c, true);
                },
            ],
        );
        assert_eq!(planned, 1, "the sheet change was styled from the sheet alone");
    }

    /// An earlier sheet that declares the layer order the other way round changes which of a
    /// later sheet's rules wins, for elements that match nothing of the changed sheet: the pass starts
    /// over, as a kept match carries its layer's rank.
    #[test]
    fn a_sheet_that_reorders_the_layers_restyles_what_the_other_sheets_layers_style() {
        let html = "<!doctype html><style id='s'>.y { color: red }</style>\
                    <style>@layer a { .w { margin-left: 5px } } @layer b { .w { margin-left: 6px } }</style><div class='w'>w</div>";
        let planned = incremental_equals_full_over(
            html,
            &[&|dom, _| {
                let s = dom.get_element_by_id("s").unwrap();
                dom.set_text_content(s, "@layer b, a; .y { color: red }");
            }],
        );
        assert_eq!(planned, 0, "a pass from nothing");
    }

    /// A non-element node with element children, linked into the document (as www.worldbank.org
    /// does), brings elements whose parent style is gone: they inherit from nothing now.
    #[test]
    fn elements_under_an_inserted_non_element_are_styled_again() {
        incremental_equals_full("<style>.p { color: red }</style><div class='p' id='p'><span id='s'>s</span></div><div id='q'></div>", |dom| {
            let (s, q) = (dom.get_element_by_id("s").unwrap(), dom.get_element_by_id("q").unwrap());
            let holder = dom.create_fragment();
            dom.append_child(holder, s);
            dom.append_child(q, holder);
        });
    }

    /// A form state only a `:has()` reads.
    #[test]
    fn a_checkbox_re_asks_a_has_that_reads_checked_and_nothing_else_does() {
        incremental_equals_full("<!doctype html><style>form:has(:checked) { margin-left: 4px }</style><form id='f'><input id='c' type='checkbox'></form>", |dom| {
            let c = dom.get_element_by_id("c").unwrap();
            dom.set_checkedness(c, true);
        });
    }

    /// A sibling-looking `:has()` whose answer reaches below its anchor.
    #[test]
    fn a_later_sibling_changes_what_an_earlier_ones_has_reaches_below_it() {
        let html = "<style>li:has(~ .c) .w { width: 5px }</style>\
                    <ul><li><span class='w' id='s'>s</span></li><li id='b'>b</li></ul>";
        incremental_equals_full(html, |dom| {
            let b = dom.get_element_by_id("b").unwrap();
            dom.set_attribute(b, "class", "c");
        });
    }

    /// Incremental style == full style, over random mutations of a document whose sheet reads
    /// classes, ids and attributes from every position, positional pseudo-classes, `:empty`, `:hover`,
    /// form state, counters and `attr()`. After each batch of writes the page's own pass (which styles
    /// only what the journal says changed) is compared with one that reuses nothing.
    #[test]
    fn styling_what_changed_equals_styling_everything_under_random_mutations() {
        const SHEET: &str = "<style>
            .a { color: red } .a .b { margin-left: 3px } .a > .c { width: 10px } .b + .c { height: 7px }
            .c ~ .d { padding-top: 2px } #x .b { height: 11px } [data-k] { padding-right: 5px }
            [data-v='1'] ~ span { margin-top: 3px } li:first-child { margin-top: 1px }
            li:last-child { margin-bottom: 2px } li:nth-child(2n) { padding-left: 4px }
            li:nth-last-child(1) { border-left: 1px solid } li:only-child { font-size: 20px }
            li:nth-last-child(2) { padding-bottom: 6px } span:nth-last-of-type(2) { padding-top: 1px }
            div:empty { height: 9px } p:empty + span { width: 13px } :hover > .e { color: blue }
            .h:hover { width: 50px } input:checked + label { width: 17px } input:disabled { height: 3px }
            fieldset:disabled input { width: 4px } ol { counter-reset: n } .r { counter-increment: n }
            .q::before { content: 'q' counter(n) } .t::after { content: attr(data-t) }
            .flex { display: flex } .none { display: none } .inh { font-size: 15px } .inh * { line-height: 1.5 }
            :not(.a) > .z { margin: 1px } :is(.a, .y) .w { margin: 2px } .em { width: 2em }
            .a:has(.b) { padding-left: 1px } ul:has(li.h:hover) { margin-right: 4px }
            fieldset:has(input:checked) label { height: 8px } :has(> [data-k]) > span { width: 6px }
            </style>";
        // Odd seeds: `:has()` that reads position or looks sideways, so every insertion re-asks; even
        // seeds: none, so only an insertion that holds an argument's element does.
        const SIDEWAYS: &str = "<style>li:has(+ .c) { margin-left: 3px } div:has(> p:empty) .w { width: 9px }
            li:has(~ .c) .w { width: 5px }</style>";
        // Sheets that change: most keyed, so a pass styles from the changed sheet alone, or
        // keyed by an ancestor (`.d *`) or `:root`; one whose subject has no key at all and one whose
        // keyless subject has only a keyed sibling, one that moves the layer order and one that moves
        // `rem`, which start over.
        const DYN: [&str; 15] = [
            ":root { --g: 4px; color: rgb(9, 8, 7) } .w { margin-left: var(--g) }",
            ".d * { padding-left: 2px } .a > :first-child { border-top: 1px solid } #x :not(.b) { height: 5px }",
            ".c ~ * { padding-top: 3px }",
            ".a { color: green } .w::before { content: 'w' } li.c { width: 3px }",
            "@layer one { .w { margin-left: 5px } } @layer two { .w { margin-left: 6px } }",
            "@layer two, one; .b { margin-right: 2px }",
            ".b { margin-left: 9px } .q::before { content: none } .h:hover { height: 4px }",
            "input:checked + label { width: 21px } .inh { line-height: 2 } fieldset:has(input:checked) .w { color: red }",
            ".z, #x { padding: 3px } [data-k='1'] { margin: 4px } .em { font-size: 18px }",
            "",
            "* { padding-bottom: 1px }",
            "@layer base { .a { color: pink } }",
            ".d .w { width: 1px } .t::after { content: 'T' } .b ~ .c { height: 2px }",
            "html { font-size: 20px }",
            ".flex { display: grid } li:nth-child(odd).r { margin-left: 2px }",
        ];
        const RULES: [&str; 4] = [".y { width: 8px }", ".r::before { content: 'r' }", ".c { display: none }", "li { margin-right: 1px }"];
        let classes = ["a", "b", "c", "d", "e", "h", "q", "r", "t", "flex", "none", "inh", "z", "w", "y", "em"];
        let tags = ["div", "span", "ul", "li", "p", "ol", "input", "label", "fieldset"];
        for seed in 1..=12u64 {
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = move |n: usize| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n as u64) as usize
            };
            let section = "<div id='top' class='a'><ul><li class='e'>1</li><li class='h'>2</li><li>3</li><li class='c'>4</li></ul>\
                <div class='b'>b</div><div class='c'>c</div><div class='d'><span class='w'>w</span></div>\
                <p></p><span>s</span><fieldset><input class='z'><label>l</label></fieldset>\
                <ol><li class='r q'>1</li><li class='r q t' data-t='v'>2</li><li class='r q'>3</li></ol></div>";
            let extra = if seed % 2 == 1 { SIDEWAYS } else { "" };
            let mut dom = Dom::parse(&format!(
                "<html><head>{SHEET}{extra}<style id='dyn'>{}</style></head><body>{}</body></html>",
                DYN[0],
                section.repeat(4)
            ));
            let body = dom.get_elements_by_tag_name("body")[0];
            let head = dom.get_elements_by_tag_name("head")[0];
            let dyn_style = dom.get_element_by_id("dyn").unwrap();
            let mut added: Vec<NodeId> = Vec::new();
            let page = Page::default();
            let media = MediaContext::default();
            let root = dom.root();
            let mut elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n).is_some() && dom.root_of(n) == root).collect();
            for step in 0..120 {
                let pick = |next: &mut dyn FnMut(usize) -> usize, elements: &[NodeId]| elements[next(elements.len())];
                for _ in 0..1 + next(3) {
                    let target = pick(&mut next, &elements);
                    match next(29) {
                        28 => {
                            // Children moved among themselves, a few times in a row (`appendChild`
                            // cycling them).
                            if let Some(p) = dom.parent(target).filter(|&p| p != root) {
                                for _ in 0..1 + next(4) {
                                    let kids = dom.children(p);
                                    let kid = kids[next(kids.len())];
                                    if next(2) == 0 {
                                        dom.append_child(p, kid);
                                    } else {
                                        let first = dom.first_child(p).unwrap();
                                        if first != kid {
                                            dom.insert_before(p, kid, first);
                                        }
                                    }
                                }
                            }
                        }
                        24 => {
                            let text = DYN[next(DYN.len())];
                            dom.set_text_content(dyn_style, text);
                        }
                        25 => match next(3) {
                            0 => dom.set_attribute(dyn_style, "media", "print"),
                            1 => dom.set_attribute(dyn_style, "media", "all"),
                            _ => dom.remove_attribute(dyn_style, "media"),
                        },
                        26 => {
                            if !added.is_empty() && next(2) == 0 {
                                let gone = added.swap_remove(next(added.len()));
                                if let Some(p) = dom.parent(gone) {
                                    dom.remove_child(p, gone);
                                }
                            } else {
                                let style = dom.create_element("style");
                                let text = dom.create_text_node(DYN[next(DYN.len())]);
                                dom.append_child(style, text);
                                dom.append_child(head, style);
                                added.push(style);
                            }
                        }
                        27 => {
                            // The CSSOM edits the sheet in place: the element's text stays.
                            let id = page.sheet_for_owner(&dom, dyn_style);
                            let (rule, at, delete) = (RULES[next(RULES.len())], next(3), next(3) == 0);
                            page.with_sheet_mut(id, |sheet| {
                                if delete {
                                    let _ = sheet.delete_rule(at);
                                } else {
                                    let _ = sheet.insert_rule(rule, at.min(sheet.rules().len()));
                                }
                            });
                        }
                        0..=5 => {
                            let n = next(3);
                            let list: Vec<&str> = (0..n).map(|_| classes[next(classes.len())]).collect();
                            dom.set_attribute(target, "class", &list.join(" "));
                        }
                        6 => {
                            if next(2) == 0 { dom.set_attribute(target, "id", "x") } else { dom.remove_attribute(target, "id") }
                        }
                        7..=9 => {
                            let name = ["data-k", "data-v", "data-t", "disabled", "style"][next(5)];
                            let value = ["1", "2", "width: 7px", ""][next(4)];
                            if next(3) == 0 { dom.remove_attribute(target, name) } else { dom.set_attribute(target, name, value) }
                        }
                        10..=13 => {
                            let el = dom.create_element(tags[next(tags.len())]);
                            if next(2) == 0 {
                                dom.set_attribute(el, "class", classes[next(classes.len())]);
                            }
                            let parent = if next(4) == 0 { body } else { target };
                            match dom.first_child(parent).filter(|_| next(2) == 0) {
                                Some(first) => dom.insert_before(parent, el, first),
                                None => dom.append_child(parent, el),
                            }
                            elements.push(el);
                        }
                        14 | 15 => {
                            if target != elements[0] {
                                if let Some(p) = dom.parent(target) {
                                    dom.remove_child(p, target);
                                    match next(4) {
                                        0 => {
                                            let to = pick(&mut next, &elements);
                                            if !dom.host_including_contains(target, to) {
                                                dom.append_child(to, target);
                                            }
                                        }
                                        // Back in, inside a non-element that is linked as it is.
                                        2 => {
                                            let to = pick(&mut next, &elements);
                                            if !dom.host_including_contains(target, to) {
                                                let holder = dom.create_fragment();
                                                dom.append_child(holder, target);
                                                dom.append_child(to, holder);
                                            }
                                        }
                                        // Into a detached tree: it keeps a parent, and leaves the document.
                                        1 => {
                                            let holder = dom.create_element("div");
                                            dom.append_child(holder, target);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        16 => {
                            if next(2) == 0 { dom.set_text_content(target, "") } else { dom.set_text_content(target, "txt") }
                        }
                        17..=19 => {
                            let mut chain = vec![target];
                            while let Some(p) = dom.parent(*chain.last().unwrap()).filter(|&p| p != root) {
                                chain.push(p);
                            }
                            chain.reverse();
                            dom.set_hover_chain(if next(3) == 0 { Vec::new() } else { chain });
                        }
                        20 | 21 => {
                            if dom.tag_name(target) == Some("input") {
                                dom.set_attribute(target, "type", "checkbox");
                                dom.set_checkedness(target, next(2) == 0);
                            }
                        }
                        23 if next(2) == 0 => {
                            // Animated values: the elements they come and go on cascade again.
                            let mut values = std::collections::HashMap::new();
                            for _ in 0..next(3) {
                                let property = [PropertyId::Width, PropertyId::FontSize, PropertyId::Display][next(3)];
                                let value = ["33px", "1.5em", "none", "flex"][next(4)].to_string();
                                values.insert(pick(&mut next, &elements), vec![AnimatedValue { property, value, transition: next(2) == 0 }]);
                            }
                            if values.is_empty() && next(2) == 0 { page.clear_animations() } else { page.set_animated(values) }
                        }
                        22 => {
                            let _ = dom.set_inner_html(target, "<li class='r q'>1</li><li class='t' data-t='z'></li><p></p><span></span>");
                            elements.extend(dom.children(target).into_iter().filter(|&c| dom.tag_name(c).is_some()));
                        }
                        _ => {
                            // A text node appended or emptied: `:empty` reads it.
                            if let Some(t) = dom.first_child(target).filter(|&c| dom.tag_name(c).is_none()) {
                                dom.set_node_value(t, if next(2) == 0 { "" } else { "x" });
                            } else {
                                let t = dom.create_text_node("y");
                                dom.append_child(target, t);
                            }
                        }
                    }
                }
                elements.retain(|&e| dom.root_of(e) == root);
                if elements.is_empty() {
                    elements.push(body);
                }
                // As `layout_document` does first: the `<style>` elements are the sheets.
                page.sync_document_sheets(&dom);
                let mine = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Allowed, &mut Prev::Nothing));
                let full = korvus_dom::with_pass_caches(|| page.layout_pass(&dom, root, &media, Reuse::Nothing, &mut Prev::Nothing));
                if let Some(d) = Page::pass_difference(&dom, &mine, &mine.tree, &full) {
                    panic!("seed {seed}, step {step}: {d}");
                }
                assert!(page.sheet_count() > 3, "the document's own sheets are in");
            }
            assert!(page.layouts_reusing_matches() > 60, "seed {seed}: most passes styled only what changed ({})", page.layouts_reusing_matches());
            let planned = page.state.lock().unwrap().sheet_passes;
            assert!(planned > 5, "seed {seed}: sheet changes styled from the changed sheets alone ({planned})");
        }
    }

    /// The animation rescan that scans what changed answers as the whole-document one, over
    /// random writes, insertions, removals, moves, hover and animated values. Each rescan is checked
    /// against the whole one run from the same state (verification forced on for this thread), and
    /// every transition it starts is noted as running, so the cancelling path is taken too.
    #[test]
    fn rescanning_what_changed_equals_rescanning_everything_under_random_mutations() {
        const SHEET: &str = "<style>@keyframes k { to { opacity: 0 } }
            .t { transition: width 1s, color 2s } .a .t { width: 5px } .a > .c { width: 10px; color: red }
            .b + .c { height: 7px; transition: height 1s } .anim { animation: k 1s infinite }
            .a .anim2 { animation: k 2s 3 } .c ~ .d { transition: all 1s } [data-k] { width: 3px }
            .h:hover { width: 50px; transition: width 3s } .inh { font-size: 15px } .em { width: 2em; transition: width 1s }
            li:first-child { color: red } li:nth-child(2n) { transition: color 1s } div:empty { height: 9px }
            .a:has(.b) { padding-left: 1px; transition: padding-left 1s } .none { display: none }
            </style>";
        let classes = ["a", "b", "c", "d", "t", "h", "anim", "anim2", "inh", "em", "none"];
        let styles = ["width: 7px", "transition: color 1s; color: blue", "animation: k 1s", "color: green", ""];
        let tags = ["div", "span", "ul", "li", "p"];
        verify::FORCED.with(|f| f.set(true));
        for seed in 1..=10u64 {
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = move |n: usize| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n as u64) as usize
            };
            let section = "<div class='a'><ul><li class='t'>1</li><li class='h'>2</li><li>3</li><li class='c'>4</li></ul>\
                <div class='b'>b</div><div class='c t'>c</div><div class='d'><span class='anim2'>w</span></div>\
                <p></p><span class='em'>s</span></div>";
            let mut dom = Dom::parse(&format!("<html><head>{SHEET}</head><body>{}</body></html>", section.repeat(4)));
            let body = dom.get_elements_by_tag_name("body")[0];
            let page = Page::default();
            let media = MediaContext::default();
            let root = dom.root();
            let mut elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n).is_some() && dom.root_of(n) == root).collect();
            page.declarative_update(&dom, &media);
            for _ in 0..80 {
                let pick = |next: &mut dyn FnMut(usize) -> usize, elements: &[NodeId]| elements[next(elements.len())];
                for _ in 0..1 + next(3) {
                    let target = pick(&mut next, &elements);
                    match next(20) {
                        16 => {
                            // Children moved among themselves, a few times in a row.
                            if let Some(p) = dom.parent(target).filter(|&p| p != root) {
                                for _ in 0..1 + next(4) {
                                    let kids = dom.children(p);
                                    let kid = kids[next(kids.len())];
                                    dom.append_child(p, kid);
                                }
                            }
                        }
                        0..=4 => {
                            let list: Vec<&str> = (0..next(3)).map(|_| classes[next(classes.len())]).collect();
                            dom.set_attribute(target, "class", &list.join(" "));
                        }
                        5 | 6 => {
                            if next(3) == 0 { dom.remove_attribute(target, "style") } else { dom.set_attribute(target, "style", styles[next(styles.len())]) }
                        }
                        7 => {
                            if next(2) == 0 { dom.set_attribute(target, "data-k", "1") } else { dom.remove_attribute(target, "data-k") }
                        }
                        8 | 9 => {
                            let el = dom.create_element(tags[next(tags.len())]);
                            if next(2) == 0 {
                                dom.set_attribute(el, "class", classes[next(classes.len())]);
                            }
                            let parent = if next(4) == 0 { body } else { target };
                            dom.append_child(parent, el);
                            elements.push(el);
                        }
                        10 | 11 => {
                            if target != elements[0] {
                                if let Some(p) = dom.parent(target) {
                                    dom.remove_child(p, target);
                                    let to = pick(&mut next, &elements);
                                    match next(3) {
                                        0 if !dom.host_including_contains(target, to) => dom.append_child(to, target),
                                        // Back in, inside a non-element that is linked as it is.
                                        1 if !dom.host_including_contains(target, to) => {
                                            let holder = dom.create_fragment();
                                            dom.append_child(holder, target);
                                            dom.append_child(to, holder);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        12 => {
                            let mut chain = vec![target];
                            while let Some(p) = dom.parent(*chain.last().unwrap()).filter(|&p| p != root) {
                                chain.push(p);
                            }
                            chain.reverse();
                            dom.set_hover_chain(if next(3) == 0 { Vec::new() } else { chain });
                        }
                        13 | 14 => {
                            // Animated values, as the running animations put them: on any element,
                            // the document element included now and then.
                            let mut values = std::collections::HashMap::new();
                            for _ in 0..next(4) {
                                let property = [PropertyId::Width, PropertyId::Color, PropertyId::FontSize, PropertyId::Opacity][next(4)];
                                let value = ["33px", "rgb(1, 2, 3)", "1.5em", "0.5"][next(4)].to_string();
                                let at = if next(40) == 0 { elements[0] } else { pick(&mut next, &elements) };
                                values.insert(at, vec![AnimatedValue { property, value, transition: next(2) == 0 }]);
                            }
                            if values.is_empty() && next(2) == 0 { page.clear_animations() } else { page.set_animated(values) }
                        }
                        17 => {
                            // A shadow root on a light element: its own sheet declaring
                            // transitions, a default or a named slot, and an element of its own.
                            if dom.shadow_root_of(target).is_none() && matches!(dom.tag_name(target), Some("div" | "span" | "p")) {
                                let mode = if next(2) == 0 { korvus_dom::ShadowRootMode::Open } else { korvus_dom::ShadowRootMode::Closed };
                                if let Ok(sr) = dom.attach_shadow(target, mode) {
                                    let style = dom.create_element("style");
                                    dom.set_text_content(style, ".t { transition: width 1s, color 2s } .w { width: 5px } .red { color: red }");
                                    dom.append_child(sr, style);
                                    let slot = dom.create_element("slot");
                                    if next(2) == 0 {
                                        dom.set_attribute(slot, "name", "a");
                                    }
                                    if next(2) == 0 {
                                        dom.set_attribute(slot, "class", "red");
                                    }
                                    dom.append_child(sr, slot);
                                    let own = dom.create_element("div");
                                    dom.set_attribute(own, "class", ["t", "t w", "w"][next(3)]);
                                    dom.append_child(sr, own);
                                }
                            }
                        }
                        18 => {
                            if next(2) == 0 { dom.set_attribute(target, "slot", "a") } else { dom.remove_attribute(target, "slot") }
                        }
                        19 => {
                            // Inside a shadow tree: its own element's class, or its slot's name and class.
                            let roots: Vec<NodeId> = elements.iter().filter_map(|&e| dom.shadow_root_of(e)).collect();
                            if !roots.is_empty() {
                                let sr = roots[next(roots.len())];
                                let kids: Vec<NodeId> = dom.children(sr).into_iter().filter(|&c| dom.tag_name(c).is_some_and(|t| t != "style")).collect();
                                if !kids.is_empty() {
                                    let k = kids[next(kids.len())];
                                    match (dom.is_html_slot_element(k), next(2)) {
                                        (true, 0) => {
                                            if next(2) == 0 { dom.set_attribute(k, "name", "a") } else { dom.remove_attribute(k, "name") }
                                        }
                                        _ => dom.set_attribute(k, "class", ["t", "t w", "w", "red"][next(4)]),
                                    }
                                }
                            }
                        }
                        _ => {
                            let _ = dom.set_inner_html(target, "<li class='t'>1</li><li class='anim'></li><p class='c'></p>");
                            elements.extend(dom.children(target).into_iter().filter(|&c| dom.tag_name(c).is_some()));
                        }
                    }
                }
                elements.retain(|&e| dom.root_of(e) == root);
                if elements.is_empty() {
                    elements.push(body);
                }
                // A layout between rescans, now and then, as a page's reads make one.
                if next(3) == 0 {
                    page.layout(&dom, &media);
                }
                let update = page.declarative_update(&dom, &media);
                for t in &update.transitions {
                    page.note_transition_target(t.node, t.property, t.to.clone());
                }
                // A computed style, from the page style pass (checked against the chain).
                for _ in 0..3 {
                    let at = pick(&mut next, &elements);
                    page.computed_value(&dom, at, &media, [PropertyId::Width, PropertyId::Color, PropertyId::FontSize][next(3)], None);
                }
            }
            assert!(page.animation_rescans_from_what_changed() > 40, "seed {seed}: most rescans scanned only what changed ({})", page.animation_rescans_from_what_changed());
        }
        verify::FORCED.with(|f| f.set(false));
    }

    /// Styles that follow laid-out sizes are styled from what changed too.
    /// Random writes over a sheet with `cq*` lengths, size and `style()` container queries and
    /// pseudo-elements in `cq*` units; each layout's style pass is checked against one from nothing
    /// (verification forced on for this thread), container sizes, generated content and boxes
    /// included.
    #[test]
    fn styles_that_follow_container_sizes_are_styled_from_what_changed() {
        const SHEET: &str = "<style>
            .c { container-type: inline-size } .s { container-type: size; height: 120px } .n { container-name: box }
            .w { width: 50cqw } .h { height: 10cqh } .i { padding-left: 5cqi } .m { margin-left: 2cqmin }
            @container (min-width: 300px) { .d { color: red; width: 25cqw } }
            @container box (max-width: 200px) { .e { margin-top: 3px } }
            @container style(--t: 1) { .f { color: blue } }
            .t { --t: 1 } .q::before { content: 'q'; width: 10cqw } .a::after { content: 'a'; margin-left: 1cqi }
            .wide { width: 600px } .narrow { width: 150px } .none { display: none } .em { font-size: 30px }
            </style>";
        let classes = ["c", "s", "n", "w", "h", "i", "m", "d", "e", "f", "t", "q", "a", "wide", "narrow", "none", "em"];
        let tags = ["div", "span", "p", "section"];
        verify::FORCED.with(|f| f.set(true));
        for seed in 1..=6u64 {
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut next = move |n: usize| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n as u64) as usize
            };
            let section = "<div class='c wide'><div class='w d q'>x</div><p class='i e a'>y</p>\
                <section class='s n narrow'><div class='h m f'>z</div><span class='w q'>s</span></section>\
                <div class='t'><p class='f a'>t</p></div></div>";
            let mut dom = Dom::parse(&format!("<html><head>{SHEET}</head><body>{}</body></html>", section.repeat(3)));
            let body = dom.get_elements_by_tag_name("body")[0];
            let page = Page::default();
            let media = MediaContext::default();
            let root = dom.root();
            let mut elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n).is_some() && dom.root_of(n) == root).collect();
            page.layout(&dom, &media);
            for _ in 0..60 {
                let pick = |next: &mut dyn FnMut(usize) -> usize, elements: &[NodeId]| elements[next(elements.len())];
                for _ in 0..1 + next(3) {
                    let target = pick(&mut next, &elements);
                    match next(10) {
                        0..=4 => {
                            let list: Vec<&str> = (0..next(4)).map(|_| classes[next(classes.len())]).collect();
                            dom.set_attribute(target, "class", &list.join(" "));
                        }
                        5 => {
                            let value = ["width: 40cqw", "--t: 1", "container-type: inline-size; width: 250px", "height: 5cqh", ""][next(5)];
                            dom.set_attribute(target, "style", value);
                        }
                        6 | 7 => {
                            let el = dom.create_element(tags[next(tags.len())]);
                            dom.set_attribute(el, "class", classes[next(classes.len())]);
                            let parent = if next(4) == 0 { body } else { target };
                            dom.append_child(parent, el);
                            elements.push(el);
                        }
                        8 => {
                            if target != elements[0] {
                                if let Some(p) = dom.parent(target) {
                                    dom.remove_child(p, target);
                                    let to = pick(&mut next, &elements);
                                    if next(2) == 0 && !dom.host_including_contains(target, to) {
                                        dom.append_child(to, target);
                                    }
                                }
                            }
                        }
                        _ => {
                            let t = dom.create_text_node("txt");
                            dom.append_child(target, t);
                        }
                    }
                }
                elements.retain(|&e| dom.root_of(e) == root);
                if elements.is_empty() {
                    elements.push(body);
                }
                page.layout(&dom, &media);
                // Computed styles, from the page style pass (checked against the chain).
                for _ in 0..4 {
                    let at = pick(&mut next, &elements);
                    page.computed_value(&dom, at, &media, [PropertyId::Width, PropertyId::Color, PropertyId::PaddingLeft, PropertyId::MarginTop][next(4)], None);
                }
            }
            assert!(page.layouts_reusing_matches() > 30, "seed {seed}: most passes styled only what changed ({})", page.layouts_reusing_matches());
        }
        verify::FORCED.with(|f| f.set(false));
    }

    /// A write that changes no box -- a colour -- keeps the last layout; one that changes a
    /// box's style, text or place lays out again; and a kept layout is the one laying out gives.
    #[test]
    fn a_layout_is_kept_when_the_tree_would_be_built_as_it_was() {
        let media = MediaContext::default();
        let page = Page::default();
        let mut dom = Dom::parse("<style>.red { color: red } .wide { width: 300px }</style><div id='a'>a</div><p id='b'>b</p>");
        let (a, b) = (element(&dom, "a"), element(&dom, "b"));
        let first = page.layout(&dom, &media);
        dom.set_attribute(a, "class", "red");
        assert!(page.relayout_document(&dom, dom.root(), &media, &first).is_none(), "a colour moves no box");
        let cold = Page::default().layout(&dom, &media);
        assert!(first.tree.first_difference(&cold.tree).is_none(), "and the kept layout is the fresh one");
        dom.set_attribute(a, "class", "red wide");
        let second = page.relayout_document(&dom, dom.root(), &media, &first).expect("a width does");
        assert_eq!(second.bounding_rect(a).width, 300.0);
        dom.set_text_content(b, "longer text");
        assert!(page.relayout_document(&dom, dom.root(), &media, &second).is_some(), "so does text");
        let narrow = MediaContext { viewport_width: 500.0, ..media };
        assert!(page.relayout_document(&dom, dom.root(), &narrow, &second).is_some(), "and a viewport");
        // Hit testing reads the kept tree too: `visibility` moves no box but is not kept.
        let third = page.layout(&dom, &media);
        dom.set_attribute(b, "style", "visibility: hidden");
        assert!(page.relayout_document(&dom, dom.root(), &media, &third).is_some(), "visibility is the tree's too");
    }

    /// The comparison the verify mode rests on sees a difference in one box's geometry, and
    /// sees none between two layouts of one document.
    #[test]
    fn the_verify_comparison_finds_a_moved_box_and_nothing_in_a_repeat() {
        let html = |w: u32| format!("<html><body><div id='a' style='width: {w}px; height: 10px'></div><p>text</p></body></html>");
        let (_, one) = laid_out(&html(40));
        let (_, again) = laid_out(&html(40));
        assert!(one.tree.first_difference(&again.tree).is_none());
        let (dom, wider) = laid_out(&html(41));
        let d = one.tree.first_difference(&wider.tree).expect("a 41px box is not a 40px one");
        assert_eq!(d.node, Some(element(&dom, "a")), "{}", d.what);
    }

    #[test]
    fn getting_several_measurements_costs_one_layout() {
        let page = Page::default();
        let dom = Dom::parse("<html><body><div id='a' style='height: 10px'></div></body></html>");
        let out = page.layout(&dom, &MediaContext::default());
        let a = element(&dom, "a");
        // The same laid-out document answers repeatedly, rather than each call re-laying it out.
        assert_eq!(out.bounding_rect(a), out.bounding_rect(a));
        assert_eq!(out.stats().0, 3, "html, body, div");
    }

    // C7.P6 put the `data-*` rules among the rules a matched-rules cache must match again, and the
    // geometry read "any such rule" as "a rule reads `style`": one `[data-*]` rule then made every
    // `transform` rewrite a new layout (www.mapbox.com: 2509 layouts to load, not 266).
    #[test]
    fn an_inert_style_rewrite_keeps_the_geometry_unless_a_rule_reads_style() {
        let media = MediaContext::default();
        let fingerprints = |css: &str| {
            let page = Page::default();
            let mut dom = Dom::parse(&format!(
                "<html><head><style>{css}</style></head><body><div id='a' data-x='1' style='transform: translateX(1px)'></div></body></html>"
            ));
            let before = page.page_geometry_fingerprint(&dom, &media);
            let div = element(&dom, "a");
            dom.set_attribute(div, "style", "transform: translateX(2px)");
            (before, page.page_geometry_fingerprint(&dom, &media))
        };
        let (before, after) = fingerprints("[data-x] { color: red }");
        assert_eq!(before, after, "a `data-*` rule cannot see a `style` rewrite");
        let (before, after) = fingerprints("[style*='2px'] { height: 9px }");
        assert_ne!(before, after, "a `style` rule can");
    }

    // The cache key, not the cache: a laid-out document cannot live in this service (taffy's Style
    // is not Sync), so the fingerprint is what the caller keys its own cache on.
    #[test]
    fn the_layout_fingerprint_moves_when_layout_would() {
        let page = Page::default();
        let media = MediaContext::default();
        let dom = Dom::parse("<html><body><div id='a' class='x' style='width: 10px'></div></body></html>");
        let mut dom = dom;
        let base = page.layout_fingerprint(&dom, &media);
        assert_eq!(base, page.layout_fingerprint(&dom, &media), "and stays put when nothing does");

        // The viewport.
        let wider = MediaContext { viewport_width: 800.0, ..media };
        assert_ne!(base, page.layout_fingerprint(&dom, &wider), "the viewport");

        // Any mutation at all, which is the point of a dirty *counter*: it does not have to know
        // that `hidden` matters, or that this attribute is one of three the old hash looked at.
        let div = element(&dom, "a");
        dom.set_attribute(div, "hidden", "");
        let after_attr = page.layout_fingerprint(&dom, &media);
        assert_ne!(base, after_attr, "an attribute nobody enumerated");

        dom.set_attribute(div, "style", "width: 20px");
        let after_style = page.layout_fingerprint(&dom, &media);
        assert_ne!(after_attr, after_style, "an inline style");

        let p = dom.create_element("p");
        dom.append_child(div, p);
        assert_ne!(after_style, page.layout_fingerprint(&dom, &media), "another element");

        // Two *different* documents can share a fingerprint, and that is fine: a layout cache holds
        // one document, and a navigation drops it. `len` separates the common cases anyway.
        let other = Dom::parse("<html><body><div id='a'></div><p></p><p></p></body></html>");
        let fresh = Dom::parse("<html><body><div id='a'></div></body></html>");
        assert_ne!(
            page.layout_fingerprint(&other, &media),
            page.layout_fingerprint(&fresh, &media),
            "different sizes, at least, are told apart"
        );

        let with_sheet = Dom::parse(
            "<html><head><style>div { height: 5px }</style></head>\
             <body><div id='a'></div></body></html>",
        );
        // Sheet 0 is the user-agent one, so the document's own is 1.
        page.sync_document_sheets(&with_sheet);
        let before = page.layout_fingerprint(&with_sheet, &media);
        page.with_sheet_mut(1, |s| s.insert_rule("div { height: 9px }", 1).unwrap()).unwrap();
        assert_ne!(before, page.layout_fingerprint(&with_sheet, &media), "a sheet edit");
    }

    // The whole point of the fallback shape: a Context with no page plugin answers `None` rather
    // than failing, so an embedder that only wants `fetch` never pays for layout.
    #[test]
    fn an_unloaded_pipeline_is_absent_not_broken() {
        let ctx = Context::root();
        assert!(ctx.get::<PageHandle>().is_none());
    }

    // ---- `:hover` in the cascade ----------------------------------------------------
    //
    // The selector engine answering `:hover` is one half; the other is that the CASCADE consults it,
    // that the LAYOUT does, and that moving the pointer invalidates both. Chrome 150 was asked each
    // of these over CDP before they were written down.

    /// Lay `html` out with the pointer at `(x, y)` and report `property` for `#id`.
    fn computed_with_pointer_at(
        html: &str,
        id: &str,
        property: PropertyId,
        at: Option<(f32, f32)>,
    ) -> String {
        let page = Page::default();
        let mut dom = Dom::parse(html);
        let media = MediaContext::default();
        if let Some((x, y)) = at {
            let out = page.layout(&dom, &media);
            let chain = out.hover_chain_at(&dom, x, y);
            assert!(!chain.is_empty(), "the probe point ({x}, {y}) is on no box");
            dom.set_hover_chain(chain);
        }
        page.computed_value(&dom, element(&dom, id), &media, property, None).unwrap()
    }

    /// The Chrome fixture's shape, with the geometry in the sheet (not inline, which would outrank
    /// every `:hover` rule) and the hover effect on `background-color`, which changes no box — so
    /// what is under the pointer cannot depend on what hovering did.
    const NEST: &str = "<html><head><style>\
        #outer { width: 400px; height: 400px } #mid { width: 300px; height: 300px }\
        #inner { width: 200px; height: 200px } #sub { width: 5px; height: 10px }\
        #outer:hover { background-color: rgb(1, 1, 1) }\
        #mid:hover   { background-color: rgb(2, 2, 2) }\
        #inner:hover { background-color: rgb(3, 3, 3) }\
        #outer:hover > #sub { width: 77px }\
      </style></head><body style='margin:0'>\
        <div id='outer'><div id='mid'><div id='inner'></div></div><div id='sub'></div></div>\
      </body></html>";

    // Chrome: with the pointer inside #inner, all three rules apply at once.
    #[test]
    fn hover_styles_the_whole_chain_not_only_the_element_under_the_pointer() {
        for (id, want) in
            [("outer", "rgb(1, 1, 1)"), ("mid", "rgb(2, 2, 2)"), ("inner", "rgb(3, 3, 3)")]
        {
            assert_eq!(
                computed_with_pointer_at(NEST, id, PropertyId::BackgroundColor, Some((10.0, 10.0))),
                want,
                "#{id} is in the chain, so its own :hover rule applies"
            );
        }
        // And nothing outside the chain moves: #sub is a sibling of #mid, not an ancestor of #inner.
        assert_eq!(
            computed_with_pointer_at(NEST, "sub", PropertyId::BackgroundColor, Some((10.0, 10.0))),
            PropertyId::BackgroundColor.initial()
        );
    }

    // `#outer:hover > #sub` — the menu case. #sub is NOT under the pointer; its parent is.
    #[test]
    fn a_child_of_a_hovered_ancestor_restyles_which_is_how_a_hover_menu_opens() {
        assert_eq!(
            computed_with_pointer_at(NEST, "sub", PropertyId::Width, None),
            "5px",
            "closed with no pointer on the page"
        );
        assert_eq!(
            computed_with_pointer_at(NEST, "sub", PropertyId::Width, Some((10.0, 10.0))),
            "77px",
            "open once an ancestor is hovered"
        );
    }

    // Moving the pointer must make the NEXT getComputedStyle different. The mechanism is the DOM
    // revision, which `layout_fingerprint` hashes, so the cached layout is dropped too.
    #[test]
    fn moving_the_pointer_invalidates_style_and_layout() {
        let page = Page::default();
        let mut dom = Dom::parse(NEST);
        let media = MediaContext::default();
        let inner = element(&dom, "inner");
        let sub = element(&dom, "sub");

        let before = page.layout_fingerprint(&dom, &media);
        let unhovered = PropertyId::BackgroundColor.initial().to_string();
        assert_eq!(
            page.computed_value(&dom, inner, &media, PropertyId::BackgroundColor, None).unwrap(),
            unhovered,
            "un-hovered"
        );
        assert_eq!(page.layout(&dom, &media).bounding_rect(sub).width, 5.0, "the menu is shut");

        let chain = page.layout(&dom, &media).hover_chain_at(&dom, 10.0, 10.0);
        assert!(dom.set_hover_chain(chain), "the pointer arrived");
        assert_ne!(before, page.layout_fingerprint(&dom, &media), "style must be invalidated");
        assert_eq!(
            page.computed_value(&dom, inner, &media, PropertyId::BackgroundColor, None).unwrap(),
            "rgb(3, 3, 3)",
            "and the next read reflects the pointer"
        );
        // The layout cache is keyed on the same revision, so the BOXES move too -- which is what a
        // hover menu actually is.
        assert_eq!(
            page.layout(&dom, &media).bounding_rect(sub).width,
            77.0,
            "layout re-ran with the hovered rule"
        );

        assert!(dom.set_hover_chain(Vec::new()), "the pointer left");
        assert_eq!(
            page.computed_value(&dom, inner, &media, PropertyId::BackgroundColor, None).unwrap(),
            unhovered,
            "both directions"
        );
        assert_eq!(page.layout(&dom, &media).bounding_rect(sub).width, 5.0, "and the menu shuts");
    }

    // A pointer moving INSIDE one element changes nothing, so nothing may be invalidated -- Chrome
    // fires a bare mousemove there and restyles nothing. Over-invalidating would make every mouse
    // sample re-lay-out the document.
    #[test]
    fn a_move_that_does_not_change_the_chain_invalidates_nothing() {
        let page = Page::default();
        let mut dom = Dom::parse(NEST);
        let media = MediaContext::default();
        let out = page.layout(&dom, &media);
        dom.set_hover_chain(out.hover_chain_at(&dom, 10.0, 10.0));
        let settled = page.layout_fingerprint(&dom, &media);

        let again = out.hover_chain_at(&dom, 12.0, 14.0);
        assert!(!dom.set_hover_chain(again), "still inside #inner");
        assert_eq!(settled, page.layout_fingerprint(&dom, &media), "so no restyle");
    }

    // -- declarative animations ------------------------------------------------------------------

    const ANIM_PAGE: &str = r#"<!doctype html><html><head><style>
      @keyframes slide { from { opacity: 0 } to { opacity: 1 } }
      #a { animation: slide 4s linear; }
      #t { transition: opacity 3s linear; opacity: 0.25; }
      #t.on { opacity: 0.75; }
      #i { animation: slide 4s linear; opacity: 0.1 !important; }
    </style></head><body>
      <div id=a></div><div id=t></div><div id=i></div>
    </body></html>"#;

    #[test]
    fn an_animation_declaration_becomes_a_spec_with_its_keyframes() {
        let page = Page::default();
        let dom = Dom::parse(ANIM_PAGE);
        let update = page.declarative_update(&dom, &MediaContext::default());
        let a = element(&dom, "a");
        let specs: Vec<_> = update.animations.iter().filter(|s| s.node == a).collect();
        assert_eq!(specs.len(), 1, "one animation on #a, from the SHORTHAND");
        assert_eq!(specs[0].name, "slide");
        assert_eq!(specs[0].timing.duration, Some(4000.0));
        assert_eq!(
            page.keyframes("slide"),
            Some(vec![
                (0.0, vec![(PropertyId::Opacity, "0".to_string())]),
                (1.0, vec![(PropertyId::Opacity, "1".to_string())]),
            ])
        );
        assert_eq!(page.keyframes("nosuchkeyframes"), None);
    }

    /// The whole point of the cascade rung: an animated value is what `getComputedStyle` reports.
    #[test]
    fn an_animated_value_is_what_the_computed_style_reports() {
        let page = Page::default();
        let dom = Dom::parse(ANIM_PAGE);
        let a = element(&dom, "a");
        let media = MediaContext::default();
        assert_eq!(page.computed_value(&dom, a, &media, PropertyId::Opacity, None).as_deref(), Some("1"));
        let mut map = std::collections::HashMap::new();
        map.insert(a, vec![AnimatedValue { property: PropertyId::Opacity, value: "0.4".into(), transition: false }]);
        page.set_animated(map);
        assert_eq!(page.computed_value(&dom, a, &media, PropertyId::Opacity, None).as_deref(), Some("0.4"));
    }

    /// Measured on Chrome 150: an animation beats an inline normal declaration and loses to an
    /// inline `!important` one. `#i` carries `opacity: 0.1 !important`.
    #[test]
    fn an_animation_loses_to_important_and_a_transition_beats_it() {
        let page = Page::default();
        let dom = Dom::parse(ANIM_PAGE);
        let i = element(&dom, "i");
        let media = MediaContext::default();
        let set = |transition: bool| {
            let mut map = std::collections::HashMap::new();
            map.insert(i, vec![AnimatedValue { property: PropertyId::Opacity, value: "0.4".into(), transition }]);
            page.set_animated(map);
            page.computed_value(&dom, i, &media, PropertyId::Opacity, None).unwrap()
        };
        assert_eq!(set(false), "0.1", "author !important beats a CSS animation");
        assert_eq!(set(true), "0.4", "a transition beats even that");
    }

    #[test]
    fn a_transition_starts_only_when_the_cascaded_value_changes() {
        let page = Page::default();
        let mut dom = Dom::parse(ANIM_PAGE);
        let media = MediaContext::default();
        let t = element(&dom, "t");
        // The first pass has no before-change style, so nothing transitions into the first value --
        // which is what Chrome does for a freshly styled element (measured).
        let first = page.declarative_update(&dom, &media);
        assert!(first.transitions.is_empty(), "no before-change style, no transition");
        // A second pass with nothing changed finds nothing either.
        assert!(page.declarative_update(&dom, &media).transitions.is_empty());
        // Now move the cascaded value.
        dom.set_attribute(t, "class", "on");
        let update = page.declarative_update(&dom, &media);
        let started: Vec<_> = update
            .transitions
            .iter()
            .filter(|s| s.node == t && s.property == PropertyId::Opacity)
            .collect();
        assert_eq!(started.len(), 1, "got {:?}", update.transitions);
        assert_eq!(started[0].from, "0.25");
        assert_eq!(started[0].to, "0.75");
        assert_eq!(started[0].timing.duration, Some(3000.0));
    }

    /// `transition-duration: 0s` is the longhand's initial value, so without this the engine would
    /// start a zero-length transition on every property of every element that ever changes.
    #[test]
    fn a_zero_duration_transition_is_not_a_transition() {
        let page = Page::default();
        let mut dom = Dom::parse(
            "<html><head><style>#x { opacity: 0.2 } #x.on { opacity: 0.9 }</style></head>\
             <body><div id=x></div></body></html>",
        );
        let media = MediaContext::default();
        let x = element(&dom, "x");
        page.declarative_update(&dom, &media);
        dom.set_attribute(x, "class", "on");
        assert!(page.declarative_update(&dom, &media).transitions.is_empty());
    }
}
