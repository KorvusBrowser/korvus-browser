//! The computed style, as layout needs it: typed, compact, and one per *distinct* style.
//!
//! # Why a second representation
//!
//! The cascade's output is strings, which is right for `getComputedStyle` (design D7) and useless
//! for layout — a block layout pass reads `width`, `margin`, and `display` on every node several
//! times over, and re-parsing `"10px"` each time would dominate the pass.
//!
//! # Why taffy's own `Style` and not one of ours
//!
//! Design D8 says taffy must store nothing per node, because `TaffyTree` keeps a `Style` for every
//! node and that is the computed style stored twice. That is about the *tree*, not the struct: a
//! `taffy::Style` derived per **interned `StyleKey`** is one struct per distinct style, not one per
//! node. A page with 5,000 elements and 80 distinct styles pays for 80 — strictly better than the
//! per-node copy D8 was written to avoid, and it comes with taffy's own trait implementations
//! rather than a second set of ours to keep in step with its releases.

use crate::layout::inline::FontSpec;
use crate::style::computed::{StyleArena, StyleKey};
use crate::style::property::{is_floated, PropertyId};
use crate::style::resolve::px_of;
use crate::value::LengthPercentage as CssLengthPercentage;
use cssparser::{Parser, ParserInput};
use std::collections::HashMap;
use cssparser::Token;

/// taffy's own track vector is private to it, and with the `std` feature it is a plain `Vec`.
/// Naming it here keeps the signatures below readable without reaching into taffy's internals.
type GridTrackVec<T> = Vec<T>;

use taffy::geometry::{Line, Point, Rect, Size};
use taffy::style::{
    AlignContent, AlignItems, BoxSizing, Clear, Dimension, Display, FlexDirection, FlexWrap, Float,
    GridPlacement, GridTemplateComponent, LengthPercentage, LengthPercentageAuto,
    MaxTrackSizingFunction, MinTrackSizingFunction, Overflow, Position, Style, TextAlign,
    TrackSizingFunction,
};

/// The CSS `position` keyword, kept because taffy's `Position` collapses `static` into `relative`.
///
/// `sticky` is here as its own value even though layout treats it as `relative`: the CSSOM resolves
/// a sticky box's insets to used lengths, so it belongs with the positioned ones.
/// What a box is in a table (CSS 2 §17.2), from its `display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableRole {
    #[default]
    None,
    /// `table` or `inline-table`.
    Table,
    /// `table-header-group` (0), `table-row-group` (1), `table-footer-group` (2).
    RowGroup(u8),
    Row,
    Cell,
    Caption,
    Column,
    ColumnGroup,
}

/// A style's table role and the properties the table algorithm reads (`layout::table`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TableInfo {
    pub role: TableRole,
    /// `border-spacing`, horizontal and vertical, in px.
    pub spacing: (f32, f32),
    /// `table-layout: fixed`.
    pub fixed: bool,
    /// `caption-side: bottom`.
    pub caption_bottom: bool,
    /// `border-collapse: collapse`.
    pub collapse: bool,
}

impl TableInfo {
    fn of(arena: &StyleArena, key: StyleKey) -> TableInfo {
        let role = match arena.value(key, PropertyId::Display).trim() {
            "table" | "inline-table" => TableRole::Table,
            "table-header-group" => TableRole::RowGroup(0),
            "table-row-group" => TableRole::RowGroup(1),
            "table-footer-group" => TableRole::RowGroup(2),
            "table-row" => TableRole::Row,
            "table-cell" => TableRole::Cell,
            "table-caption" => TableRole::Caption,
            "table-column" => TableRole::Column,
            "table-column-group" => TableRole::ColumnGroup,
            _ => TableRole::None,
        };
        if role == TableRole::None {
            return TableInfo::default();
        }
        let px = |t: &str| t.trim().trim_end_matches("px").parse::<f32>().unwrap_or(0.0);
        let spacing = {
            let v = arena.value(key, PropertyId::BorderSpacing);
            let mut parts = v.split_whitespace();
            let h = parts.next().map(px).unwrap_or(0.0);
            (h, parts.next().map(px).unwrap_or(h))
        };
        let collapse = arena.value(key, PropertyId::BorderCollapse).trim() == "collapse";
        TableInfo {
            role,
            // the collapsing border model has no spacing between cells (CSS 2 §17.6.2): reCAPTCHA's tile grid,
            // `border-collapse: collapse`, put its 126px tiles 2px further apart than Chrome 150 does
            spacing: if collapse { (0.0, 0.0) } else { spacing },
            fixed: arena.value(key, PropertyId::TableLayout).trim() == "fixed",
            caption_bottom: arena.value(key, PropertyId::CaptionSide).trim() == "bottom",
            collapse,
        }
    }
}

/// A computed `vertical-align` (CSS 2 §10.8.1), as the line layout applies it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VAlign {
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    /// A length, px, raising the box.
    Length(f32),
    /// A percentage of the element's own `line-height`, as a fraction, raising the box.
    Percent(f32),
}

impl VAlign {
    /// The computed value's text (`baseline`, `-2.8px`, `50%`, ...).
    pub fn parse(v: &str) -> VAlign {
        let v = v.trim().to_ascii_lowercase();
        match v.as_str() {
            "sub" => VAlign::Sub,
            "super" => VAlign::Super,
            "text-top" => VAlign::TextTop,
            "text-bottom" => VAlign::TextBottom,
            "middle" => VAlign::Middle,
            "top" => VAlign::Top,
            "bottom" => VAlign::Bottom,
            _ => {
                if let Some(p) = v.strip_suffix('%').and_then(|n| n.trim().parse::<f32>().ok()) {
                    VAlign::Percent(p / 100.0)
                } else if let Some(n) = v.strip_suffix("px").and_then(|n| n.trim().parse::<f32>().ok()) {
                    VAlign::Length(n)
                } else {
                    VAlign::Baseline
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssPosition {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

impl CssPosition {
    fn of(keyword: &str) -> CssPosition {
        match keyword.trim() {
            "relative" => CssPosition::Relative,
            "absolute" => CssPosition::Absolute,
            "fixed" => CssPosition::Fixed,
            "sticky" => CssPosition::Sticky,
            _ => CssPosition::Static,
        }
    }

    /// Does this box establish a containing block for absolutely positioned descendants, and does
    /// the CSSOM report its own insets as used lengths? Both questions have the same answer.
    pub fn is_positioned(self) -> bool {
        self != CssPosition::Static
    }
}

/// A stable hash of `v`: the same in every tree and every run of one build.
fn origin_of(v: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// One `taffy::Style` per distinct computed style, and the font that goes with it.
///
/// The font is interned alongside rather than separately because it is derived from the same
/// computed style: two elements with the same style have the same font by construction, so a second
/// map would key on the same thing twice.
#[derive(Debug, Default)]
pub struct LayoutStyles {
    styles: Vec<Style>,
    /// What each style IS, as a number: a hash of the whole row this table keeps for it --
    /// the taffy style, the font, hit testing, position, level, alignment and the rest -- so two trees
    /// whose boxes have equal origins lay out (and hit-test) the same, whatever ids the two tables gave
    /// them, and a colour change leaves an origin as it was. An anonymous or [`Variant`] style hashes
    /// its base's origin with the step, the row being a function of the two.
    origins: Vec<u64>,
    /// The row of each computed style already seen, with its origin, kept by the page between trees of one theme
    /// ([`KeyRows`]).
    key_rows: KeyRows,
    fonts: Vec<FontSpec>,
    /// `visibility` and `pointer-events`, which layout does not read but hit testing does. Kept
    /// here rather than looked up per test because a hit test walks every box.
    hittable: Vec<bool>,
    /// The CSS `position` keyword. taffy has only two positions — `Relative` and `Absolute` — and
    /// `static` and `relative` both map onto `Relative` because taffy lays them out identically.
    /// The CSSOM does NOT: the resolved value of `top` is a used length on a relatively positioned
    /// box and the computed value on a static one, so the distinction taffy discards has to be kept
    /// somewhere, and this is the shelf `hittable` already sits on.
    positions: Vec<CssPosition>,
    /// Which formatting context this style's box belongs to. taffy's `Display` cannot say, because
    /// taffy has no inline algorithm — see [`BoxLevel`].
    levels: Vec<BoxLevel>,
    /// `text-align` and `text-indent`, which only the inline formatting context reads. taffy's own
    /// `TextAlign` is the legacy `-webkit-center` block behaviour and means nothing here.
    aligns: Vec<TextAlignKind>,
    indents: Vec<f32>,
    ids: HashMap<StyleKey, u32>,
    /// Base style → the anonymous style derived from it. See [`LayoutStyles::intern_anonymous`].
    anonymous: HashMap<u32, u32>,
    /// (base style, adjustment) → the adjusted style. See [`LayoutStyles::intern_variant`].
    variants: HashMap<(u32, Variant), u32>,
    /// Which axes compute `overflow: auto`, per style. See [`AutoScroll`].
    autos: Vec<AutoScroll>,
    /// Whether `appearance` leaves the platform theme's parts on a control: every keyword but
    /// `none`. Only form-control sizing reads it.
    themed: Vec<bool>,
    /// Does the style transform its box (`transform` not `none`)? What lets a rect query skip every
    /// ancestor that does not (`LaidOut::transforms`).
    transformed: Vec<bool>,
    /// Each style's `vertical-align`, parsed (see [`VAlign`]).
    valigns: Vec<VAlign>,
    /// What transforms the box, per style that does (`transform`, `translate`, `rotate`, `scale`): layout's scrollable
    /// overflow takes a transformed box where it shows (`BoxTree::overflow_of`).
    transform_sources: HashMap<u32, crate::effects::TransformSource>,
    /// Each style's part in a table, and the table properties that lay one out (see [`TableInfo`]).
    tables: Vec<TableInfo>,
    /// The worn device's platform theme: what `scrollbar-width` turns into, in px. Set once, before
    /// the first style is interned, by [`crate::layout::tree::BoxTree::build_with_theme`].
    theme: spec::ThemeProfile,
    /// The HORIZONTAL scrollbar's thickness per style, where it differs from `scrollbar_width`
    /// (`::-webkit-scrollbar { height }`).
    hbars: HashMap<u32, f32>,
    /// Per style with an `aspect-ratio` other than `auto`: the ratio as written and its `auto`, which a
    /// replaced box sizes by (`replaced::compute_replaced_layout`); taffy's `aspect_ratio` keeps only
    /// the quotient.
    ratios: HashMap<u32, crate::layout::replaced::PreferredRatio>,
    /// Per style: a legacy-box FLEX container (`style::compute::LegacyBox::Flex`), whose
    /// children take [`Variant::WebkitBoxItem`].
    legacy_flex: Vec<bool>,
    /// Per style: its own `-webkit-box-flex` -- the grow AND shrink it has as a legacy box's item.
    box_flex: Vec<f32>,
    /// Per style: the number of lines a clamping legacy box keeps (0: no clamp). An anonymous box
    /// inherits it from the box it wraps, so the clamp reaches the box's own line of text.
    line_clamps: Vec<u32>,
    /// Per style: `content-visibility`, and the block size a skipped box keeps -- its
    /// `contain-intrinsic-height` / `-size`, else 0.
    content_vis: Vec<ContentVisibility>,
    intrinsic_height: Vec<f32>,
}

/// `content-visibility` (CSS Contain 2), as layout reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentVisibility {
    #[default]
    Visible,
    /// Skipped while not relevant to the user -- off the viewport by more than Blink's margin.
    Auto,
    /// Always skipped.
    Hidden,
}

/// The axes whose `overflow` is `auto` — a scroll container that shows a scrollbar only once its
/// content overflows.
///
/// taffy has no `auto`: its `Scroll` always reserves `scrollbar_width` and its `Hidden` never does.
/// So an `auto` axis is laid out as `Hidden` — the same scroll container, reserving nothing — and
/// kept here, and [`crate::layout::tree::BoxTree::compute`] turns it into `Scroll` once a pass shows
/// the content does not fit, which is what Blink does between its own layout passes. Measured on
/// Chrome 150: a `200x100` `overflow: auto` box is `clientWidth` 200 with nothing in it and 185 with
/// 300px of content, and with 300px of WIDTH only it is 200 × 85 — each axis decides for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutoScroll {
    pub x: bool,
    pub y: bool,
}

/// A box-level adjustment to an interned style: a fact about the ELEMENT (or where it sits) that
/// its computed style cannot carry, and that taffy has to see in the `Style` it lays out.
///
/// Interned like everything else, so 400 `<img>`s with one computed style cost one variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Variant {
    /// A replaced element or a form control sized by what it is (`layout::replaced`). taffy's block
    /// layout must not stretch it to its container's width (`item_is_replaced`), and on a line it is
    /// an ATOMIC inline whatever `display: inline` says: an `<img>` is one unbreakable box, not a run
    /// of fragments. An `auto` width is `auto` rather than `fit-content`, because a replaced box's
    /// auto width IS its natural one and `layout::replaced` resolves it.
    Replaced,
    /// A replaced GRID item whose alignment is `normal` in an axis: `normal` behaves as
    /// `start` for a box with a natural size (CSS Box Alignment 3 §6.1), so it keeps that size where taffy
    /// stretches every item it cannot see a definite size on -- fontshare.com's `<svg width=183 height=30>`
    /// logo was 456x75 in its grid header. `justify`/`align`: the axes whose item AND container
    /// alignment are both `normal` (an explicit `stretch` still stretches, as Chrome 150 does).
    ReplacedGridItem { justify: bool, align: bool },
    /// A block-level `<button>` shrinks to fit rather than filling its container: Chrome 150 gives
    /// `<button style="display:block">go</button>` a 30.83px box in a 1889px body.
    ShrinkToFit,
    /// The used `overflow` of `<body>` once it has been propagated to the viewport (CSS Overflow 3
    /// §3.3): `visible`, whatever the computed value is. See `BoxTree::add`.
    OverflowVisible,
    /// An `overflow: auto` axis whose content did not fit: it now shows a scrollbar, so taffy lays
    /// it out as `Scroll` and reserves the thickness. `ScrollbarX` is the HORIZONTAL scrollbar
    /// (`overflow-x`, which takes block size); `ScrollbarY` the vertical one, which takes inline size.
    ScrollbarX,
    ScrollbarY,
    /// The root box. Its `overflow` belongs to the VIEWPORT (CSS Overflow 3 §3.3), so any scrollbar
    /// it implies is the viewport's and narrows the initial containing block instead; the `<html>`
    /// box itself reserves nothing.
    ViewportRoot,
    /// An `inside` list marker drawn as a symbol (`disc`, `square`, a `<summary>`'s disclosure triangle):
    /// an empty atomic inline as wide as Blink's marker box with its margins, in 1/64 px (see
    /// `BoxTree::inside_marker_width`).
    InsideMarker { width: i32 },
    /// A checkbox or radio button the theme draws: Blink's `AdjustCheckboxStyle`/`AdjustRadioStyle` reset
    /// its padding and border (an author's are not honoured), so it is its theme part's size.
    ThemePart,
    /// `<textarea>`: Blink's style adjuster turns a `visible` overflow into `auto` on it — Chrome 150
    /// computes `overflow-x`/`-y` `auto` on a bare `<textarea>` — which the cascade here does not do,
    /// so layout does it for the axes that are still `visible`.
    TextareaOverflow,
    /// `::-webkit-scrollbar`'s thickness per axis, in 1/64 px: the vertical bar's width
    /// and the horizontal bar's height. taffy reserves one `scrollbar_width` for both axes, so the
    /// vertical one is what it reserves; the horizontal one is what [`LayoutStyles::horizontal_bar`]
    /// reports, which is what the client box reads.
    CustomScrollbar { vertical: u32, horizontal: u32 },
    /// A quirks-mode document's quirky UA top margin, dropped.
    NoMarginTop,
    /// An item of a legacy `-webkit-box` flexbox: its `-webkit-box-flex` is its grow and shrink;
    /// in a HORIZONTAL box (`row`) an `auto` width is shrink-to-fit against the box.
    WebkitBoxItem { row: bool },
    /// A box `content-visibility` skips the contents of.
    Skipped,
    /// A quirks-mode `<html>`/`<body>` filling the viewport: a used minimum height, in 1/64 px.
    QuirkMinHeight(u32),
    /// A used height in 1/64 px: a percentage height resolved, as a quirks-mode document does,
    /// against the filled `<body>` its `auto`-height containing block would otherwise leave it
    /// without.
    QuirkHeight(u32),
    /// A closed `<details>`' `::details-content` slot: `content-visibility: hidden`
    /// contains its size (0 tall) and its layout (an independent formatting context, so a `<p>`'s
    /// margins inside it do not collapse out through it).
    DetailsSlot,
    /// The host of a broken `<img>`'s FALLBACK content (A/B C8): a block container rather than a
    /// replaced box -- Blink's `HTMLImageElement::CreateLayoutObject` makes it a `LayoutBlockFlow`, so
    /// an `inline` one is an atomic inline that shrinks to fit its content, the broken-image icon and
    /// the alt text. `HTMLImageFallbackHelper::AdjustHostStyle` resets an INLINE host's `width`,
    /// `height` and `aspect-ratio` (Chrome 150: `<img src=bad alt=x width=30 height=40>` is 23.97 x 24,
    /// the icon and its text); any other keeps them, the `width`/`height` attributes' presentational
    /// hints among them (`style::hints`).
    ImageFallback,
    /// The broken-image icon in a fallback host (`HTMLImageFallbackHelper::CreateAltTextShadowTree`'s
    /// `<img width=16 height=16>`, which shows `IDR_BROKENIMAGE`): a 16x16 replaced box with no margin.
    /// Blink floats it to the start side; here, beside alt text it is an atomic inline at the start of the
    /// text's line (where the float would sit), and alone it is a block -- a host holding only a float has no
    /// line box, and is the icon's height.
    BrokenImageIcon { inline: bool },
}

/// Where a line box's content sits in the space it was given.
///
/// `justify` is deliberately absent: v1 does not stretch a line, so a `justify` block would have to
/// be reported as `start` anyway, and naming the value here would suggest otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlignKind {
    Start,
    Center,
    End,
}

impl TextAlignKind {
    /// The fraction of a line's leftover space that goes **before** its content.
    pub fn leading_fraction(self) -> f32 {
        match self {
            TextAlignKind::Start => 0.0,
            TextAlignKind::Center => 0.5,
            TextAlignKind::End => 1.0,
        }
    }

    /// `horizontal-tb` and `ltr` only, which is the one writing mode this engine lays out — so
    /// `start` is `left` and `end` is `right`. The day a second direction exists this is where it
    /// is expressed, rather than at each of the places that read the answer.
    fn of(keyword: &str) -> TextAlignKind {
        match keyword.trim() {
            "center" | "-webkit-center" => TextAlignKind::Center,
            "right" | "end" | "-webkit-right" => TextAlignKind::End,
            _ => TextAlignKind::Start,
        }
    }
}

/// Everything [`LayoutStyles::intern`] derives for one computed style, and the row's origin. A row is a function
/// of the computed style and the theme alone, so the page keeps the rows a tree worked out for the next tree of the
/// same theme ([`KeyRows`]): every layout built a new table, and each derived every distinct style again -- a dozen
/// computed values read and parsed per style, `LayoutStyles::intern` 2.8% of lowes.com's main thread (frame-pointer
/// profile, `korvus fetch`, 8 s), where hashing the row for its origin was already paid once.
#[derive(Debug, Clone)]
pub struct DerivedRow {
    style: Style,
    origin: u64,
    font: FontSpec,
    hittable: bool,
    position: CssPosition,
    level: BoxLevel,
    align: TextAlignKind,
    indent: f32,
    auto: AutoScroll,
    themed: bool,
    transformed: bool,
    source: Option<crate::effects::TransformSource>,
    valign: VAlign,
    table: TableInfo,
    ratio: Option<crate::layout::replaced::PreferredRatio>,
    legacy_flex: bool,
    box_flex: f32,
    line_clamp: u32,
    content_vis: ContentVisibility,
    intrinsic_height: f32,
}

// SAFETY: the one field that is not plain data is the taffy `Style`, whose `calc()` values are raw pointers
// (`style::math::intern`, a node of this thread's interner). A row whose derivation made one is never kept
// ([`MADE_CALC`]), so a kept row's style holds no pointer, and moving it to another thread moves numbers.
unsafe impl Send for DerivedRow {}

/// The rows of the computed styles a page's trees have derived, by computed style ([`DerivedRow`]).
pub type KeyRows = korvus_dom::FxHashMap<StyleKey, DerivedRow>;

impl LayoutStyles {
    /// The layout style for `key`, deriving it the first time it is asked for -- or copying the row an earlier tree
    /// of this theme derived for it.
    pub fn intern(&mut self, arena: &StyleArena, key: StyleKey) -> u32 {
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.styles.len() as u32;
        if let Some(row) = self.key_rows.get(&key) {
            let row = row.clone();
            self.push_row(id, row);
            self.ids.insert(key, id);
            return id;
        }
        MADE_CALC.with(|c| c.set(false));
        let mut style = derive(arena, key);
        let mut auto = AutoScroll {
            x: arena.value(key, PropertyId::OverflowX).trim() == "auto",
            y: arena.value(key, PropertyId::OverflowY).trim() == "auto",
        };
        // `scrollbar-width` picks WHICH of the theme's thicknesses; `none` hides the scrollbar and
        // keeps the scrolling, which is no thickness at all by definition rather than by capture.
        style.scrollbar_width = match arena.value(key, PropertyId::ScrollbarWidth).trim() {
            "none" => 0.0,
            "thin" => self.theme.scrollbar_thin_thickness,
            _ => self.theme.scrollbar_thickness,
        };
        // `scrollbar-gutter: stable` reserves the block-axis scrollbar's space on a scroll container
        // whether or not it overflows — Chrome 150: `overflow: hidden; scrollbar-gutter: stable` on a
        // 200px box is `clientWidth` 185. taffy's `Scroll` is exactly "always reserve". `visible` and
        // `clip` are not scroll containers and get no gutter.
        if arena.value(key, PropertyId::ScrollbarGutter).trim().starts_with("stable")
            && matches!(style.overflow.y, Overflow::Hidden | Overflow::Scroll)
        {
            style.overflow.y = Overflow::Scroll;
            auto.y = false;
        }
        self.styles.push(style);
        self.autos.push(auto);
        self.themed.push(!arena.value(key, PropertyId::Appearance).trim().eq_ignore_ascii_case("none"));
        self.transformed.push(!arena.value(key, PropertyId::Transform).trim().eq_ignore_ascii_case("none"));
        let source = crate::effects::TransformSource::read(|p| Some(arena.value(key, p).to_string()));
        if !source.is_none() {
            self.transform_sources.insert(id, source);
        }
        self.valigns.push(VAlign::parse(arena.value(key, PropertyId::VerticalAlign)));
        let table = TableInfo::of(arena, key);
        if table.role == TableRole::Table {
            // a table is sized by its columns, not stretched to its container (taffy's `is_table`)
            if let Some(last) = self.styles.last_mut() {
                last.item_is_table = true;
            }
        }
        if table.role == TableRole::Cell {
            // A cell's height is its rows' (`layout::table`), which its `min-height` and `max-height` do not touch:
            // Chrome 152.0.7977.82 lays a `<td style="min-height:60px">` around a 10px block out 10 tall, and one with
            // `height:60px; max-height:30px` 60 (2026-10-23).
            if let Some(last) = self.styles.last_mut() {
                last.min_size.height = taffy::style::LengthPercentageAuto::auto();
                last.max_size.height = taffy::style::LengthPercentageAuto::auto();
            }
        }
        self.tables.push(table);
        self.fonts.push(derive_font(arena, key));
        // `visibility: hidden` and `pointer-events: none` leave the box in the layout -- it still
        // takes up space -- and take it out of hit testing.
        self.hittable.push(
            arena.value(key, PropertyId::Visibility).trim() == "visible"
                && arena.value(key, PropertyId::PointerEvents).trim() != "none",
        );
        self.positions.push(CssPosition::of(arena.value(key, PropertyId::Position)));
        // A float is block-level: the cascade has blockified its `display`, and taffy's block layout places it
        // (`Style::float`, see `derive`). It used to be an atomic inline on its line, which kept it on the line's
        // baseline: Naver's 420px right column sat 103px down beside the 830px left one, and `float: right`
        // stayed on the left.
        self.levels.push(level(arena.value(key, PropertyId::Display)));
        self.aligns.push(TextAlignKind::of(arena.value(key, PropertyId::TextAlign)));
        self.indents.push(px_of(arena.value(key, PropertyId::TextIndent)).unwrap_or(0.0));
        let legacy = crate::style::compute::legacy_box(arena, key);
        self.legacy_flex.push(legacy == crate::style::compute::LegacyBox::Flex);
        self.box_flex.push(number(arena.value(key, PropertyId::WebkitBoxFlex)).unwrap_or(0.0));
        self.line_clamps.push(match legacy {
            crate::style::compute::LegacyBox::Clamp(n) => n,
            _ => 0,
        });
        self.content_vis.push(match arena.value(key, PropertyId::ContentVisibility).trim() {
            "auto" => ContentVisibility::Auto,
            "hidden" => ContentVisibility::Hidden,
            _ => ContentVisibility::Visible,
        });
        self.intrinsic_height.push(intrinsic_height(
            arena.value(key, PropertyId::ContainIntrinsicHeight),
            arena.value(key, PropertyId::ContainIntrinsicSize),
        ));
        if let Some(r) = crate::layout::replaced::PreferredRatio::parse(arena.value(key, PropertyId::AspectRatio)) {
            self.ratios.insert(id, r);
        }
        self.ids.insert(key, id);
        let origin = self.row_origin(id);
        self.origins.push(origin);
        // a row with a `calc()` pointer is derived again by every tree (see the `Send` above)
        if !MADE_CALC.with(|c| c.get()) {
            let row = self.row_of(id, origin);
            self.key_rows.insert(key, row);
        }
        id
    }

    /// Row `id` as [`LayoutStyles::intern`] derived it, for the page to keep.
    fn row_of(&self, id: u32, origin: u64) -> DerivedRow {
        let i = id as usize;
        DerivedRow {
            style: self.styles[i].clone(),
            origin,
            font: self.fonts[i].clone(),
            hittable: self.hittable[i],
            position: self.positions[i],
            level: self.levels[i],
            align: self.aligns[i],
            indent: self.indents[i],
            auto: self.autos[i],
            themed: self.themed[i],
            transformed: self.transformed[i],
            source: self.transform_sources.get(&id).cloned(),
            valign: self.valigns[i],
            table: self.tables[i],
            ratio: self.ratios.get(&id).copied(),
            legacy_flex: self.legacy_flex[i],
            box_flex: self.box_flex[i],
            line_clamp: self.line_clamps[i],
            content_vis: self.content_vis[i],
            intrinsic_height: self.intrinsic_height[i],
        }
    }

    /// A kept row as style `id` (the next one).
    fn push_row(&mut self, id: u32, row: DerivedRow) {
        self.styles.push(row.style);
        self.origins.push(row.origin);
        self.fonts.push(row.font);
        self.hittable.push(row.hittable);
        self.positions.push(row.position);
        self.levels.push(row.level);
        self.aligns.push(row.align);
        self.indents.push(row.indent);
        self.autos.push(row.auto);
        self.themed.push(row.themed);
        self.transformed.push(row.transformed);
        if let Some(source) = row.source {
            self.transform_sources.insert(id, source);
        }
        self.valigns.push(row.valign);
        self.tables.push(row.table);
        if let Some(r) = row.ratio {
            self.ratios.insert(id, r);
        }
        self.legacy_flex.push(row.legacy_flex);
        self.box_flex.push(row.box_flex);
        self.line_clamps.push(row.line_clamp);
        self.content_vis.push(row.content_vis);
        self.intrinsic_height.push(row.intrinsic_height);
    }

    /// A hash of everything this table keeps for style `id` (see [`LayoutStyles::origin`]).
    fn row_origin(&self, id: u32) -> u64 {
        let i = id as usize;
        let row = format!(
            "{:?}{:?}",
            (&self.styles[i], &self.fonts[i], self.hittable[i], self.positions[i], self.levels[i], self.aligns[i], self.indents[i].to_bits()),
            (
                self.autos[i],
                self.themed[i],
                self.transformed[i],
                self.valigns[i],
                self.tables[i],
                self.legacy_flex[i],
                self.box_flex[i].to_bits(),
                self.line_clamps[i],
                self.content_vis[i],
                self.intrinsic_height[i].to_bits(),
                self.hbars.get(&id).map(|v| v.to_bits()),
                self.ratios.get(&id),
            )
        );
        origin_of(&row)
    }

    /// The computed styles' rows this table worked out, for the next tree (see `key_rows`).
    pub fn take_key_rows(&mut self) -> KeyRows {
        std::mem::take(&mut self.key_rows)
    }

    /// Start from the rows an earlier table worked out under the same theme.
    pub fn set_key_rows(&mut self, rows: KeyRows) {
        self.key_rows = rows;
    }

    /// The style of an **anonymous** box wrapping a run of `base`'s inline content.
    ///
    /// CSS 2.1 §9.2.1.1: an anonymous box inherits the inheritable properties of the box it was
    /// generated inside and takes the *initial* value for everything else. That second half is the
    /// load-bearing one — an anonymous box that kept its parent's `padding`, `border`, `width` and
    /// `margin` would apply every one of them a second time, and a `<body>` with the UA sheet's 8px
    /// margin would indent its own text by 8 more pixels.
    ///
    /// So: the font, `text-align` and `text-indent` (inherited) survive; the box properties do not.
    /// Keyed by the base style, so a page of 200 identically styled paragraphs interns one.
    pub fn intern_anonymous(&mut self, base: u32) -> u32 {
        if let Some(&id) = self.anonymous.get(&base) {
            return id;
        }
        let id = self.styles.len() as u32;
        let parent = &self.styles[base as usize];
        self.styles.push(Style {
            display: Display::Block,
            // The one thing it keeps from the box half: whether its parent's overflow makes it a
            // scroll container, so an anonymous box inside `overflow: hidden` does not report a
            // scrolling area its parent has already clipped.
            overflow: parent.overflow,
            ..Style::DEFAULT
        });
        self.origins.push(origin_of(&("anonymous", self.origins[base as usize])));
        // Anonymous boxes never show a scrollbar of their own: the one their parent shows is the
        // parent's.
        self.autos.push(AutoScroll::default());
        self.themed.push(false);
        self.transformed.push(false);
        self.valigns.push(VAlign::Baseline);
        self.tables.push(TableInfo::default());
        self.fonts.push(self.fonts[base as usize].clone());
        self.hittable.push(self.hittable[base as usize]);
        self.positions.push(CssPosition::Static);
        self.levels.push(BoxLevel::Block);
        self.aligns.push(self.aligns[base as usize]);
        self.indents.push(self.indents[base as usize]);
        self.legacy_flex.push(false);
        self.box_flex.push(0.0);
        self.line_clamps.push(self.line_clamps[base as usize]);
        self.content_vis.push(ContentVisibility::Visible);
        self.intrinsic_height.push(0.0);
        self.anonymous.insert(base, id);
        id
    }

    /// `base` with one [`Variant`] applied. Everything but the adjusted fields — font, hit testing,
    /// position, alignment — is the base style's.
    pub fn intern_variant(&mut self, base: u32, variant: Variant) -> u32 {
        if let Some(&id) = self.variants.get(&(base, variant)) {
            return id;
        }
        let mut style = self.styles[base as usize].clone();
        let mut level = self.levels[base as usize];
        let mut auto = self.autos[base as usize];
        let mut ratio = self.ratios.get(&base).copied();
        match variant {
            Variant::Replaced => {
                style.item_is_replaced = true;
                if style.size.width == Dimension::fit_content() {
                    style.size.width = Dimension::auto();
                }
                if level == BoxLevel::Inline {
                    level = BoxLevel::AtomicInline;
                }
                // `auto && <ratio>` is the replaced sizing's to settle against the element's own ratio
                // (`compute_replaced_layout` reads it whole); taffy, which would carry the quotient across
                // a flex or grid item's axes first, never sees it -- as it never saw a natural ratio.
                if ratio.is_some_and(|r| r.auto) {
                    style.aspect_ratio = None;
                }
            }
            Variant::ReplacedGridItem { justify, align } => {
                if justify {
                    style.justify_self = Some(AlignItems::START);
                }
                if align {
                    style.align_self = Some(AlignItems::START);
                }
            }
            // An item of a legacy flexbox: Blink's `ResolvedFlexGrow`/`-Shrink` are its
            // `-webkit-box-flex`, 0 by default -- so items neither grow nor shrink unless asked; the
            // computed `flex-grow` stays what it was.
            Variant::WebkitBoxItem { row } => {
                let flex = self.box_flex[base as usize];
                style.flex_grow = flex;
                style.flex_shrink = flex;
                // WebKit's deprecated flexbox sizes a horizontal box's children SHRINK-TO-FIT against
                // the box: Chrome 150 (c10/repro/wbox), in a 100px `-webkit-box`, lays a 232px-wide run
                // of text -- bare, in a <span>, in a <div>, with or without `-webkit-box-flex` -- 100
                // wide on three lines, and two such spans 100 wide EACH, overflowing together (no
                // flex shrink between them). korvus kept them at their max-content width on one line.
                // In flex terms: the max-content basis it has, clamped to the box (`max-width: 100%`),
                // never under its min-content size (`min-width: auto`). Only the LAYOUT
                // style moves; the item's computed `max-width` is still its own.
                if row && style.max_size.width == LengthPercentageAuto::auto() {
                    style.max_size.width = LengthPercentageAuto::percent(1.0);
                }
            }
            // A box whose contents `content-visibility` skips: SIZE containment -- its
            // contents do not size it, so an `auto` height is its `contain-intrinsic-height`, else 0
            // (Chrome 150: an off-screen `auto` box is 0 tall, 100 under `contain-intrinsic-size: auto
            // 100px`; a `hidden` one 0) -- and paint containment, a clip that is not a scroll
            // container. The contents are still laid out inside it, as a closed `<details>`' are.
            Variant::Skipped => {
                if style.size.height == Dimension::auto() {
                    style.size.height = Dimension::length(self.intrinsic_height[base as usize]);
                }
                style.overflow = Point { x: Overflow::Clip, y: Overflow::Clip };
            }
            Variant::ShrinkToFit => {
                if style.size.width == Dimension::auto() {
                    style.size.width = Dimension::fit_content();
                }
            }
            Variant::OverflowVisible => {
                style.overflow = Point { x: Overflow::Visible, y: Overflow::Visible };
                auto = AutoScroll::default();
            }
            Variant::ScrollbarX => {
                style.overflow.x = Overflow::Scroll;
                auto.x = false;
            }
            Variant::ScrollbarY => {
                style.overflow.y = Overflow::Scroll;
                auto.y = false;
            }
            Variant::ViewportRoot => {
                style.scrollbar_width = 0.0;
                auto = AutoScroll::default();
            }
            Variant::InsideMarker { width } => {
                let zero = LengthPercentage::length(0.0);
                style.display = Display::Block;
                style.position = taffy::style::Position::Relative;
                style.inset = taffy::geometry::Rect { left: LengthPercentageAuto::auto(), right: LengthPercentageAuto::auto(), top: LengthPercentageAuto::auto(), bottom: LengthPercentageAuto::auto() };
                style.size = Size { width: Dimension::length(width as f32 / 64.0), height: Dimension::length(0.0) };
                style.min_size = Size { width: LengthPercentageAuto::auto(), height: LengthPercentageAuto::auto() };
                style.max_size = Size { width: LengthPercentageAuto::auto(), height: LengthPercentageAuto::auto() };
                style.padding = taffy::geometry::Rect { left: zero, right: zero, top: zero, bottom: zero };
                style.border = style.padding;
                style.margin = taffy::geometry::Rect { left: LengthPercentageAuto::length(0.0), right: LengthPercentageAuto::length(0.0), top: LengthPercentageAuto::length(0.0), bottom: LengthPercentageAuto::length(0.0) };
                style.item_is_replaced = true;
                level = BoxLevel::AtomicInline;
            }
            Variant::ThemePart => {
                style.padding = taffy::geometry::Rect { left: LengthPercentage::length(0.0), right: LengthPercentage::length(0.0), top: LengthPercentage::length(0.0), bottom: LengthPercentage::length(0.0) };
                style.border = style.padding;
            }
            Variant::TextareaOverflow => {
                if style.overflow.x == Overflow::Visible {
                    style.overflow.x = Overflow::Hidden;
                    auto.x = true;
                }
                if style.overflow.y == Overflow::Visible {
                    style.overflow.y = Overflow::Hidden;
                    auto.y = true;
                }
            }
            Variant::CustomScrollbar { vertical, .. } => {
                style.scrollbar_width = vertical as f32 / 64.0;
            }
            Variant::NoMarginTop => {
                style.margin.top = LengthPercentageAuto::length(0.0);
            }
            Variant::DetailsSlot => {
                style.size.height = Dimension::length(0.0);
                style.overflow = Point { x: Overflow::Hidden, y: Overflow::Hidden };
                style.scrollbar_width = 0.0;
                auto = AutoScroll::default();
            }
            Variant::ImageFallback => {
                if level == BoxLevel::Inline {
                    style.size = Size { width: Dimension::fit_content(), height: Dimension::auto() };
                    style.aspect_ratio = None;
                    ratio = None;
                    level = BoxLevel::AtomicInline;
                }
            }
            Variant::BrokenImageIcon { inline } => {
                style.size = Size { width: Dimension::length(16.0), height: Dimension::length(16.0) };
                style.item_is_replaced = true;
                level = if inline { BoxLevel::AtomicInline } else { BoxLevel::Block };
            }
            Variant::QuirkHeight(h) => {
                style.size.height = Dimension::length(h as f32 / 64.0);
            }
            Variant::QuirkMinHeight(h) => {
                let h = h as f32 / 64.0;
                let current = style.min_size.height.into_raw();
                if current.tag() != taffy::style::CompactLength::LENGTH_TAG || current.value() < h {
                    style.min_size.height = LengthPercentageAuto::length(h);
                }
            }
        }
        let id = self.styles.len() as u32;
        let hbar = match variant {
            Variant::CustomScrollbar { horizontal, .. } => Some(horizontal as f32 / 64.0),
            _ => self.hbars.get(&base).copied(),
        };
        if let Some(h) = hbar {
            self.hbars.insert(id, h);
        }
        // the base's `aspect-ratio`, unless the adjustment took it away (an inline image fallback host)
        if let Some(r) = ratio {
            self.ratios.insert(id, r);
        }
        self.styles.push(style);
        self.origins.push(origin_of(&("variant", self.origins[base as usize], variant)));
        self.autos.push(auto);
        self.themed.push(self.themed[base as usize]);
        self.transformed.push(self.transformed[base as usize]);
        if let Some(source) = self.transform_sources.get(&base).cloned() {
            self.transform_sources.insert(id, source);
        }
        self.valigns.push(self.valigns[base as usize]);
        self.tables.push(self.tables[base as usize]);
        self.fonts.push(self.fonts[base as usize].clone());
        self.hittable.push(self.hittable[base as usize]);
        self.positions.push(self.positions[base as usize]);
        self.levels.push(level);
        self.aligns.push(self.aligns[base as usize]);
        self.indents.push(self.indents[base as usize]);
        self.legacy_flex.push(self.legacy_flex[base as usize]);
        self.box_flex.push(self.box_flex[base as usize]);
        self.line_clamps.push(self.line_clamps[base as usize]);
        self.content_vis.push(self.content_vis[base as usize]);
        self.intrinsic_height.push(self.intrinsic_height[base as usize]);
        self.variants.insert((base, variant), id);
        id
    }

    /// What style `id` was made from (see the field).
    pub fn origin(&self, id: u32) -> u64 {
        self.origins[id as usize]
    }

    /// The horizontal scrollbar's thickness under this style: `scrollbar_width` unless a
    /// `::-webkit-scrollbar` gave that axis its own.
    pub fn horizontal_bar(&self, id: u32) -> f32 {
        self.hbars.get(&id).copied().unwrap_or(self.styles[id as usize].scrollbar_width)
    }

    /// The `aspect-ratio` a replaced box with this style sizes by: as written, `auto` kept.
    pub fn preferred_ratio(&self, id: u32) -> Option<crate::layout::replaced::PreferredRatio> {
        self.ratios.get(&id).copied()
    }

    /// Which formatting context a box with this style belongs to.
    pub fn level(&self, id: u32) -> BoxLevel {
        self.levels[id as usize]
    }

    /// Where this block's lines sit in the space they were given.
    pub fn align(&self, id: u32) -> TextAlignKind {
        self.aligns[id as usize]
    }

    /// `text-indent`: how far the FIRST line of this block starts in. A percentage resolves against
    /// the containing block and is not resolved here, so it reads as zero — the same honest
    /// `px_of` failure the rest of this file takes for a value it cannot turn into a length.
    pub fn indent(&self, id: u32) -> f32 {
        self.indents[id as usize]
    }

    /// Whether `id` is a legacy-box flex container.
    pub fn legacy_flex(&self, id: u32) -> bool {
        self.legacy_flex[id as usize]
    }

    /// The [`Variant::WebkitBoxItem`] an item of legacy box `container` takes: horizontal when the box
    /// lays its items out in a row (`-webkit-box-orient: horizontal`, the default).
    pub fn webkit_box_item(&self, container: u32) -> Variant {
        let row = matches!(self.styles[container as usize].flex_direction, FlexDirection::Row | FlexDirection::RowReverse);
        Variant::WebkitBoxItem { row }
    }

    /// `content-visibility` of style `id`.
    pub fn content_visibility(&self, id: u32) -> ContentVisibility {
        self.content_vis[id as usize]
    }

    /// The lines a clamping legacy box keeps, for the box of style `id`; 0 when it does not clamp.
    pub fn line_clamp(&self, id: u32) -> u32 {
        self.line_clamps[id as usize]
    }

    pub fn get(&self, id: u32) -> &Style {
        &self.styles[id as usize]
    }

    /// Does a control with this style draw the platform theme's parts (`appearance` is not `none`)?
    pub fn themed(&self, id: u32) -> bool {
        self.themed[id as usize]
    }

    /// Style `id`'s part in a table.
    pub fn table(&self, id: u32) -> TableInfo {
        self.tables[id as usize]
    }

    /// Style `id`'s `vertical-align`.
    pub fn valign(&self, id: u32) -> VAlign {
        self.valigns[id as usize]
    }

    /// Does style `id` transform its box?
    pub fn transformed(&self, id: u32) -> bool {
        self.transformed[id as usize]
    }

    /// What transforms a box of this style, when anything does.
    pub(crate) fn transform_source(&self, id: u32) -> Option<&crate::effects::TransformSource> {
        self.transform_sources.get(&id)
    }

    /// Does any style transform its box?
    pub(crate) fn any_transform(&self) -> bool {
        !self.transform_sources.is_empty()
    }

    /// Which axes of this style are `overflow: auto` and not yet showing a scrollbar.
    pub fn auto_scroll(&self, id: u32) -> AutoScroll {
        self.autos[id as usize]
    }

    /// The platform theme styles are interned against. Must be set before the first `intern`: a
    /// style interned under one theme keeps that theme's thickness.
    pub fn set_theme(&mut self, theme: spec::ThemeProfile) {
        debug_assert!(self.styles.is_empty(), "the theme is set before any style is interned");
        self.theme = theme;
    }

    /// See [`LayoutStyles::set_theme`].
    pub fn theme(&self) -> spec::ThemeProfile {
        self.theme
    }

    /// The font a run of text under this style is measured in.
    pub fn font(&self, id: u32) -> &FontSpec {
        &self.fonts[id as usize]
    }

    /// Whether a hit test can land on a box with this style.
    pub fn hittable(&self, id: u32) -> bool {
        self.hittable[id as usize]
    }

    /// The CSS `position` keyword, which taffy's own `Position` cannot express.
    pub fn position(&self, id: u32) -> CssPosition {
        self.positions[id as usize]
    }

    /// Does any of this style's four paddings need a containing block before it is a length?
    ///
    /// Asked per *style*, not per box, so the answer is computed once for every element that
    /// styles the same — and the overwhelmingly common answer is `false`, which is what lets
    /// [`BoxTree::compute`] skip the resolution pass entirely on a page that has none.
    pub fn has_percentage_padding(&self, id: u32) -> bool {
        let p = self.styles[id as usize].padding;
        is_percentage(p.left)
            || is_percentage(p.right)
            || is_percentage(p.top)
            || is_percentage(p.bottom)
    }

    /// How many distinct layout styles exist. The claim this file rests on is that it stays far
    /// below the node count.
    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }
}

/// Is this a percentage, as opposed to a length or a `calc()`?
fn is_percentage(value: LengthPercentage) -> bool {
    value.into_raw().tag() == taffy::style::CompactLength::PERCENT_TAG
}

/// `style` with its percentage paddings turned into lengths against `basis`.
///
/// **All four** of them, which is the whole point. CSS resolves a percentage padding against the
/// containing block's *inline* size on every side — `padding-top: 10%` is 10% of the container's
/// WIDTH, not of its height — and taffy resolves the block-axis pair per-axis, against a block size
/// that is usually `auto` and therefore zero. So `padding: 10%` came out as a horizontal pair and
/// nothing vertical, which is wrong twice over: the reported `padding-top` was `0px` and the box's
/// own height was short by the padding it should have had.
///
/// Handing taffy a style whose paddings are already lengths makes its own resolution a no-op, so
/// the fix lands on everything downstream of it at once — the box-sizing adjustment that sets the
/// child's known height, the padding rect the CSSOM reads back, and the content box `width` and
/// `height` resolve from.
///
/// Each a `LayoutUnit`, truncated as Blink's `MinimumValueForLength` truncates the float product: `padding: 10%` of
/// a 147.109375px container is 14.703125 in Chrome 152.0.7977.82, and a box with it above and below a 16px line is
/// 61.40625 tall, where this made it 14.7109375 and 61.421875 (2026-10-23).
pub fn with_resolved_padding(style: &Style, basis: f32) -> Style {
    let resolve = |value: LengthPercentage| {
        let raw = value.into_raw();
        if raw.tag() == taffy::style::CompactLength::PERCENT_TAG {
            LengthPercentage::length(layout_unit(raw.value() * basis))
        } else if raw.is_calc() {
            // A math function over a percentage, against the same inline basis.
            LengthPercentage::length(layout_unit(crate::style::math::resolve_ptr(raw.calc_value(), basis)))
        } else {
            value
        }
    };
    let mut style = style.clone();
    style.padding = Rect {
        left: resolve(style.padding.left),
        right: resolve(style.padding.right),
        top: resolve(style.padding.top),
        bottom: resolve(style.padding.bottom),
    };
    style
}

/// The font a run of text under this style is measured in.
///
/// `line-height` is the interesting one: a *number* is a factor of the font size and a *length* is
/// the height itself, and `normal` is neither -- it is the font's own ascent plus descent, which
/// only the metrics know. So `normal` stays `None` here and is resolved where the metrics are.
pub fn derive_font(arena: &StyleArena, key: StyleKey) -> FontSpec {
    let v = |p: PropertyId| arena.value(key, p);
    let size = px_of(v(PropertyId::FontSize)).unwrap_or(16.0);
    let raw = v(PropertyId::LineHeight).trim();
    // A specified line height is laid out in whole layout units, the fraction truncated (Blink's `LayoutUnit(float)`):
    // Chrome 150 stacks `font: 28px/1.4` lines 39.1875 apart, not 39.2. Float noise is not a fraction (1.15 x 20).
    let units = |v: f32| {
        let u = v * 64.0;
        (if (u - u.round()).abs() < 1e-3 { u.round() } else { u.trunc() }) / 64.0
    };
    let line_height = if raw.eq_ignore_ascii_case("normal") {
        None
    } else if let Ok(factor) = raw.parse::<f32>() {
        Some(units(factor * size))
    } else {
        px_of(raw).map(units)
    };
    let weight = match v(PropertyId::FontWeight).trim() {
        w if w.eq_ignore_ascii_case("bold") || w.eq_ignore_ascii_case("bolder") => 700,
        w => w.parse::<f32>().map_or(400, |n| n.round().clamp(1.0, 1000.0) as u16),
    };
    FontSpec {
        families: v(PropertyId::FontFamily).into(),
        size,
        line_height,
        // `pre`, `pre-wrap`, and `break-spaces` keep whitespace; `collapse` and `preserve-breaks`
        // do not. (`preserve-breaks` keeps newlines only, which v1 collapses along with the rest.)
        preserve: matches!(v(PropertyId::WhiteSpaceCollapse).trim(), "preserve" | "break-spaces"),
        wrap: !v(PropertyId::TextWrapMode).trim().eq_ignore_ascii_case("nowrap"),
        balance: v(PropertyId::TextWrapStyle).trim().eq_ignore_ascii_case("balance"),
        // The face a family's styles pick: bold from 600, which is where font matching
        // takes the bold face of a family that has 400 and 700; italic for `italic` and `oblique`.
        bold: weight >= 600,
        weight,
        italic: {
            let st = v(PropertyId::FontStyle);
            let st = st.trim().to_ascii_lowercase();
            st.starts_with("italic") || st.starts_with("oblique")
        },
        kerning: !v(PropertyId::FontKerning).trim().eq_ignore_ascii_case("none"),
        letter_spacing: px_of(v(PropertyId::LetterSpacing)).unwrap_or(0.0),
        word_spacing: px_of(v(PropertyId::WordSpacing)).unwrap_or(0.0),
        transform: crate::layout::inline::TextTransform::of(v(PropertyId::TextTransform)),
    }
}

/// Turn one computed style into the form layout reads.
pub fn derive(arena: &StyleArena, key: StyleKey) -> Style {
    let v = |p: PropertyId| arena.value(key, p);
    let (column_tracks, column_names) = track_list(v(PropertyId::GridTemplateColumns));
    let (row_tracks, row_names) = track_list(v(PropertyId::GridTemplateRows));
    let rtl = v(PropertyId::Direction) == "rtl";
    let mut style = Style {
        display: display(v(PropertyId::Display)),
        // A float takes its box out of the flow, to the edge of its containing block, and the line boxes after it
        // flow around it -- taffy's block layout places it (`float_layout`), and `layout_ifc` shortens the lines
        // beside it. A floated box was an atomic inline on its line: a 420px right column beside an 830px left one
        // sat on their shared line's baseline, 103px down (Naver), and `float: right` stayed on the left.
        // `position: absolute` and `fixed` win over `float` (CSS 2.1 §9.7).
        float: match (v(PropertyId::Float).trim(), v(PropertyId::Position)) {
            (_, "absolute" | "fixed") => Float::None,
            ("left", _) => Float::Left,
            ("right", _) => Float::Right,
            ("inline-start", _) => if rtl { Float::Right } else { Float::Left },
            ("inline-end", _) => if rtl { Float::Left } else { Float::Right },
            _ => Float::None,
        },
        clear: match v(PropertyId::Clear).trim() {
            "left" => Clear::Left,
            "right" => Clear::Right,
            "both" => Clear::Both,
            "inline-start" => if rtl { Clear::Right } else { Clear::Left },
            "inline-end" => if rtl { Clear::Left } else { Clear::Right },
            _ => Clear::None,
        },
        box_sizing: match v(PropertyId::BoxSizing) {
            "border-box" => BoxSizing::BorderBox,
            _ => BoxSizing::ContentBox,
        },
        position: match v(PropertyId::Position) {
            "absolute" | "fixed" => Position::Absolute,
            // `sticky` lays out in flow and is offset at paint time, so as far as layout is
            // concerned it is relative -- which is also what it is specified to be.
            _ => Position::Relative,
        },
        overflow: Point {
            x: overflow(v(PropertyId::OverflowX)),
            y: overflow(v(PropertyId::OverflowY)),
        },
        // taffy has no `static`, so a statically positioned box arrives as `Position::Relative` and
        // taffy duly *offsets* it by its insets. CSS 2.1 §9.4.3 says a static box ignores them
        // entirely -- `position: static; top: 40px` moves nothing in any browser -- so the
        // distinction taffy cannot express in its `Position` is expressed here instead, by handing
        // it insets that are already `auto`.
        inset: match CssPosition::of(v(PropertyId::Position)) {
            CssPosition::Static => {
                let auto = LengthPercentageAuto::auto();
                Rect { left: auto, right: auto, top: auto, bottom: auto }
            }
            _ => Rect {
                left: lpa(v(PropertyId::Left)),
                right: lpa(v(PropertyId::Right)),
                top: lpa(v(PropertyId::Top)),
                bottom: lpa(v(PropertyId::Bottom)),
            },
        },
        // An atomic inline -- `inline-block` and its flex/grid twins -- shrinks to fit rather than
        // filling its container, which is the one thing that makes it *inline*. taffy spells that
        // `fit_content`. Without it a `<span style="display:inline-block">` would report the width
        // of its parent, and a page cross-checking that against `measureText` would see it.
        //
        // **A FLOAT shrinks to fit too, and it is asked about `float` rather than `display`** (P7b).
        // CSS 2.1 Section 10.3.5, and measured: Chrome 150 gives `float:left` on a `<span>abc</span>`
        // a 27px box in a 600px parent, the same box it gives the `inline-block`. The two questions
        // look like one and are not -- the cascade BLOCKIFIES a floated box, so by the time this
        // reads `display` the `inline-block` spelling is gone and only `block` is left. Keying
        // shrink-to-fit off `display` alone would therefore make every float fill its container, and
        // did: before this arm `float:left` on a span measured 600px wide against Chrome's 27.
        //
        // An ABSOLUTELY positioned box needs no arm here. It reaches taffy as `Position::Absolute`,
        // whose own layout already shrink-to-fits an auto size, and a second `fit_content` would
        // fight the insets in `abs_lr` (`left:0;right:0`), where the width IS determined.
        size: Size {
            width: match (v(PropertyId::Width), v(PropertyId::Display), v(PropertyId::Float)) {
                ("auto", "inline-block" | "inline-flex" | "inline-grid" | "-webkit-inline-box", _) => Dimension::fit_content(),
                ("auto", _, float) if is_floated(float) => Dimension::fit_content(),
                (width, _, _) => size_keyword(width).unwrap_or_else(|| dim(width)),
            },
            height: dim(v(PropertyId::Height)),
        },
        // `min-width: auto` is the initial value and means "no minimum" outside a flex item, which
        // is what taffy's `auto` does here.
        min_size: Size {
            width: lpa(v(PropertyId::MinWidth)),
            height: lpa(v(PropertyId::MinHeight)),
        },
        max_size: Size {
            width: lpa(v(PropertyId::MaxWidth)),
            height: lpa(v(PropertyId::MaxHeight)),
        },
        aspect_ratio: ratio(v(PropertyId::AspectRatio)),
        margin: Rect {
            left: lpa(v(PropertyId::MarginLeft)),
            right: lpa(v(PropertyId::MarginRight)),
            top: lpa(v(PropertyId::MarginTop)),
            bottom: lpa(v(PropertyId::MarginBottom)),
        },
        padding: Rect {
            left: lp(v(PropertyId::PaddingLeft)),
            right: lp(v(PropertyId::PaddingRight)),
            top: lp(v(PropertyId::PaddingTop)),
            bottom: lp(v(PropertyId::PaddingBottom)),
        },
        // A border with no style has no width, whatever the width property says. The property
        // table's initial already encodes that for an unstyled element; this is the same rule for
        // an element that set one and not the other.
        border: Rect {
            left: border_width(v(PropertyId::BorderLeftWidth), v(PropertyId::BorderLeftStyle)),
            right: border_width(v(PropertyId::BorderRightWidth), v(PropertyId::BorderRightStyle)),
            top: border_width(v(PropertyId::BorderTopWidth), v(PropertyId::BorderTopStyle)),
            bottom: border_width(v(PropertyId::BorderBottomWidth), v(PropertyId::BorderBottomStyle)),
        },
        align_items: align_items(v(PropertyId::AlignItems)),
        align_self: align_items(v(PropertyId::AlignSelf)),
        justify_items: align_items(v(PropertyId::JustifyItems)),
        justify_self: align_items(v(PropertyId::JustifySelf)),
        align_content: align_content(v(PropertyId::AlignContent)),
        justify_content: align_content(v(PropertyId::JustifyContent)),
        gap: Size {
            width: gap(v(PropertyId::ColumnGap)),
            height: gap(v(PropertyId::RowGap)),
        },
        text_align: match v(PropertyId::TextAlign) {
            // taffy's `TextAlign` exists only for the legacy `-webkit-center` block-alignment
            // behaviour, not for text alignment proper; nothing v1 computes maps onto it.
            _ => TextAlign::Auto,
        },
        flex_direction: match v(PropertyId::FlexDirection) {
            "row-reverse" => FlexDirection::RowReverse,
            "column" => FlexDirection::Column,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => FlexDirection::Row,
        },
        flex_wrap: match v(PropertyId::FlexWrap) {
            "wrap" => FlexWrap::Wrap,
            "wrap-reverse" => FlexWrap::WrapReverse,
            _ => FlexWrap::NoWrap,
        },
        flex_grow: number(v(PropertyId::FlexGrow)).unwrap_or(0.0),
        flex_shrink: number(v(PropertyId::FlexShrink)).unwrap_or(1.0),
        flex_basis: dim(v(PropertyId::FlexBasis)),
        grid_template_columns: column_tracks,
        grid_template_rows: row_tracks,
        // The NAMES a placement refers to -- line names from the track lists, areas from
        // `grid-template-areas` (taffy derives `<area>-start`/`-end` from them). Without them every
        // named placement pointed past the explicit grid, into a new auto track at the far end.
        grid_template_column_names: column_names,
        grid_template_row_names: row_names,
        grid_template_areas: template_areas(v(PropertyId::GridTemplateAreas)),
        grid_auto_columns: auto_tracks(v(PropertyId::GridAutoColumns)),
        grid_auto_rows: auto_tracks(v(PropertyId::GridAutoRows)),
        // Computed and never laid out -- every auto item stacked in rows. fontshare.com's
        // filter bar (`inline-grid; grid-auto-flow: column`, Tailwind's `grid-flow-col`) stood as a column.
        grid_auto_flow: grid_auto_flow(v(PropertyId::GridAutoFlow)),
        grid_column: Line {
            start: placement(v(PropertyId::GridColumnStart)),
            end: placement(v(PropertyId::GridColumnEnd)),
        },
        grid_row: Line {
            start: placement(v(PropertyId::GridRowStart)),
            end: placement(v(PropertyId::GridRowEnd)),
        },
        ..Default::default()
    };
    // A legacy box (`style::compute::legacy_box`): a flexbox whose direction and
    // alignment are the `-webkit-box-*` properties -- Blink's mapping, measured on Chrome 150
    // (`-webkit-box-pack: center` centres, `vertical` stacks) -- or, clamping lines, a block.
    match crate::style::compute::legacy_box(arena, key) {
        crate::style::compute::LegacyBox::Flex => {
            let vertical = matches!(v(PropertyId::WebkitBoxOrient).trim(), "vertical" | "block-axis");
            let reverse = v(PropertyId::WebkitBoxDirection).trim() == "reverse";
            style.flex_direction = match (vertical, reverse) {
                (false, false) => FlexDirection::Row,
                (false, true) => FlexDirection::RowReverse,
                (true, false) => FlexDirection::Column,
                (true, true) => FlexDirection::ColumnReverse,
            };
            // The legacy `start`/`end` are the flex lines' own start and end, reversed with them.
            style.align_items = align_items(match v(PropertyId::WebkitBoxAlign).trim() {
                "start" => "flex-start",
                "end" => "flex-end",
                "center" => "center",
                "baseline" => "baseline",
                _ => "stretch",
            });
            style.justify_content = align_content(match v(PropertyId::WebkitBoxPack).trim() {
                "end" => "flex-end",
                "center" => "center",
                "justify" => "space-between",
                _ => "flex-start",
            });
        }
        crate::style::compute::LegacyBox::Clamp(_) => style.display = Display::Block,
        crate::style::compute::LegacyBox::No => {}
    }
    style
}

/// `display`, reduced to the three layout modes taffy implements.
///
/// Everything block-level that is not flex or grid lays out as a block, `<table>` included: v1 has
/// no table algorithm, and a table laid out as a block is wrong in its internals but right about
/// where the table itself sits. Zero would be wrong about both.
///
/// **An `inline` box never reaches taffy at all**, so what this answers for it is irrelevant: it is
/// laid out by [`crate::layout::ifc`], which taffy has no algorithm for. The value here is still
/// `Block` because a taffy `Style` has to carry something and `Block` is what the box would be if
/// its `display` were blockified — which is exactly what happens to it as a flex or grid item.
pub fn display(value: &str) -> Display {
    match value {
        "none" => Display::None,
        "flex" | "inline-flex" | "-webkit-box" | "-webkit-inline-box" => Display::Flex,
        "grid" | "inline-grid" => Display::Grid,
        // a block that is a block formatting context of its own: its floats stay inside it, and it does not
        // flow under the floats beside it
        "flow-root" => Display::FlowRoot,
        _ => Display::Block,
    }
}

/// Where a box sits in its parent's formatting context: on a line, or between lines.
///
/// taffy has no inline algorithm and no `Display::Inline`, so this is the distinction taffy cannot
/// express — the same shelf [`CssPosition`] sits on, and for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxLevel {
    /// A block-level box: it is a taffy child of its container and takes a whole line of its own.
    Block,
    /// A non-atomic inline box (`display: inline`). It generates no single box: its geometry is a
    /// run of **line-box fragments**, one per line it is split across, and taffy never sees it.
    Inline,
    /// An atomic inline (`inline-block`, `inline-flex`, `inline-grid`): laid out by taffy as one
    /// box, then placed **on a line** as an indivisible unit.
    AtomicInline,
}

impl BoxLevel {
    /// Does this box sit on a line rather than between lines?
    pub fn is_inline_level(self) -> bool {
        self != BoxLevel::Block
    }
}

/// Which formatting context a `display` value puts a box in.
///
/// `inline-table` is `AtomicInline` even though v1 lays a table out as a block: an atomic inline is
/// "a box on a line", and being wrong about a table's internals is the pre-existing approximation
/// [`display`] already documents, not a new one.
pub fn level(value: &str) -> BoxLevel {
    match value {
        "inline-block" | "inline-flex" | "inline-grid" | "inline-table" | "-webkit-inline-box" => BoxLevel::AtomicInline,
        "inline" | "ruby" | "ruby-text" | "ruby-base" | "inline list-item" => BoxLevel::Inline,
        _ => BoxLevel::Block,
    }
}

/// Does this `display` value generate a box at all?
///
/// `inline` is on this list no longer. It generates a box whose geometry is a run of line-box
/// fragments rather than one rectangle — see [`crate::layout::ifc`] — which is why the shape
/// `client_rects` has always had (design D11, a *list*) is the one it needed.
pub fn generates_box(value: &str) -> bool {
    !matches!(value, "none" | "contents")
}

fn overflow(value: &str) -> Overflow {
    match value {
        "hidden" => Overflow::Hidden,
        "clip" => Overflow::Clip,
        "scroll" => Overflow::Scroll,
        // `Hidden`, not `Scroll`: an `auto` axis is a scroll container that reserves nothing until
        // its content overflows. See [`AutoScroll`].
        "auto" => Overflow::Hidden,
        _ => Overflow::Visible,
    }
}

/// A math function in a computed value: by then its lengths are pixels (`style::resolve`), so any
/// context does; one with a percentage is taffy's to finish, through `BoxTree::resolve_calc_value`.
enum Math {
    Px(f32),
    Calc(*const ()),
}

fn math(value: &str) -> Option<Math> {
    if !crate::style::math::is_math(value) {
        return None;
    }
    let n = crate::style::math::reduce(value, &crate::value::ResolveContext::new(16.0, 16.0, 0.0, 0.0))?;
    Some(match n.px() {
        Some(px) => Math::Px(px),
        None => {
            MADE_CALC.with(|c| c.set(true));
            Math::Calc(crate::style::math::intern(n))
        }
    })
}

thread_local! {
    /// Set when [`math`] hands taffy a `calc()` pointer: a row that holds one is not kept between trees
    /// ([`DerivedRow`]).
    static MADE_CALC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A CSS pixel length as Blink lays it out: a `LayoutUnit`, a multiple of 1/64 px
/// TRUNCATED toward zero -- `FixedPoint(float)` in `platform/geometry/layout_unit.h`. Measured on Chrome
/// 150 (`tools/realweb/seed/layoutunit`): `width: 10.123456789px` lays out 10.109375 wide (647/64),
/// `0.000123px` lays out as 0, and `getBoundingClientRect` never answers a length that is not k/64.
/// This engine handed taffy the f32 and reported `10.123499870300293`.
pub(crate) fn layout_unit(px: f32) -> f32 {
    // Saturated as `ClampTo<int>` saturates the raw value: Chrome 150 lays `width: calc(1px *
    // 123456789)` out `3.35544e+07px` wide, `LayoutUnit::Max()`; NaN is 0.
    let raw = (px * 64.0).trunc();
    if raw.is_nan() {
        return 0.0;
    }
    raw.clamp(i32::MIN as f32, i32::MAX as f32) / 64.0
}

/// A `<length-percentage>` that may also be `auto`.
fn lpa(value: &str) -> LengthPercentageAuto {
    match math(value) {
        Some(Math::Px(px)) => return LengthPercentageAuto::length(layout_unit(px)),
        Some(Math::Calc(p)) => return LengthPercentageAuto::calc(p),
        None => {}
    }
    match parse(value) {
        Some(CssLengthPercentage::Length(l)) => LengthPercentageAuto::length(layout_unit(l.value)),
        Some(CssLengthPercentage::Percentage(p)) => LengthPercentageAuto::percent(p / 100.0),
        None => LengthPercentageAuto::auto(),
    }
}

/// A `<length-percentage>` with no `auto`; anything unrecognised is zero, which is what the
/// properties taking this (padding, border) do with a value they cannot use.
fn lp(value: &str) -> LengthPercentage {
    match math(value) {
        Some(Math::Px(px)) => return LengthPercentage::length(layout_unit(px)),
        Some(Math::Calc(p)) => return LengthPercentage::calc(p),
        None => {}
    }
    match parse(value) {
        Some(CssLengthPercentage::Length(l)) => LengthPercentage::length(layout_unit(l.value)),
        Some(CssLengthPercentage::Percentage(p)) => LengthPercentage::percent(p / 100.0),
        None => LengthPercentage::length(0.0),
    }
}

/// A sizing keyword of `width` as taffy's own (CSS Sizing 3 §3.2), with the `-webkit-` spellings Blink
/// keeps: `-webkit-min-content`, `-webkit-max-content`, `-webkit-fit-content` and `-webkit-fill-available`
/// (`stretch`). Each used to reach the layout as `auto` -- a block `width: max-content` filled its
/// container, a `width: fit-content` one in a 0-wide box was 0 wide (Chrome 150, 2026-10-09:
/// its content's widths). `fit-content(<length>)` is not one: Blink takes it only in a grid track list.
fn size_keyword(value: &str) -> Option<Dimension> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "min-content" | "-webkit-min-content" => Dimension::min_content(),
        "max-content" | "-webkit-max-content" => Dimension::max_content(),
        "fit-content" | "-webkit-fit-content" => Dimension::fit_content(),
        "stretch" | "-webkit-fill-available" => Dimension::stretch(),
        _ => return None,
    })
}

fn dim(value: &str) -> Dimension {
    match math(value) {
        Some(Math::Px(px)) => return Dimension::length(layout_unit(px)),
        Some(Math::Calc(p)) => return Dimension::calc(p),
        None => {}
    }
    match parse(value) {
        Some(CssLengthPercentage::Length(l)) => Dimension::length(layout_unit(l.value)),
        Some(CssLengthPercentage::Percentage(p)) => Dimension::percent(p / 100.0),
        None => Dimension::auto(),
    }
}

/// The block size `contain-intrinsic-height` / `contain-intrinsic-size` give a skipped box:
/// the last length -- `auto 100px` and `200px 100px` both mean 100 -- and 0 for `none`. The
/// `auto` keyword's REMEMBERED size needs a box that was once rendered, which a first layout has not.
fn intrinsic_height(height: &str, size: &str) -> f32 {
    let last_px = |v: &str| v.split_ascii_whitespace().rev().find_map(|t| px_of(t).filter(|_| t != "auto"));
    last_px(height).or_else(|| last_px(size)).map(layout_unit).unwrap_or(0.0)
}

/// `normal` is the initial gap and means zero.
fn gap(value: &str) -> LengthPercentage {
    if value.trim().eq_ignore_ascii_case("normal") {
        return LengthPercentage::length(0.0);
    }
    lp(value)
}

/// A border's width is zero when its style is `none` or `hidden`, whatever the width says. Chrome
/// reports the same thing from `getComputedStyle`, and a layout that ignored it would give every
/// `border-width: 2px; border-style: none` element four extra pixels.
fn border_width(width: &str, style: &str) -> LengthPercentage {
    if matches!(style.trim(), "none" | "hidden") {
        return LengthPercentage::length(0.0);
    }
    match width.trim() {
        // The keyword widths, as Chrome resolves them.
        "thin" => LengthPercentage::length(1.0),
        "medium" => LengthPercentage::length(3.0),
        "thick" => LengthPercentage::length(5.0),
        other => lp(other),
    }
}

/// The computed value is already in pixels by the time layout sees it, so this only has to read a
/// number back out — but it goes through the same parser so a percentage is still a percentage.
fn parse(value: &str) -> Option<CssLengthPercentage> {
    let text = value.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("auto") || text.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(px) = px_of(text) {
        return Some(CssLengthPercentage::Length(crate::value::Length::px(px)));
    }
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let parsed = CssLengthPercentage::parse(&mut p).ok()?;
    p.expect_exhausted().ok()?;
    Some(parsed)
}

fn number(value: &str) -> Option<f32> {
    value.trim().parse::<f32>().ok()
}

/// `aspect-ratio` as taffy takes it: the quotient, for `auto && <ratio>` too (a non-replaced box has no
/// ratio of its own to defer to); a degenerate ratio is `auto`. The replaced-element sizing reads the
/// whole value ([`LayoutStyles::preferred_ratio`]).
fn ratio(value: &str) -> Option<f32> {
    crate::layout::replaced::PreferredRatio::parse(value).map(|p| p.width / p.height)
}

/// `start` and `flex-start` are different keywords, and taffy keeps them apart -- the first is
/// relative to the writing direction, the second to the flex direction, and they differ under
/// `flex-direction: row-reverse`.
/// `grid-template-columns` / `grid-template-rows`: the tracks, and the names of the lines between them.
///
/// Handles the forms real stylesheets use: a list of track sizes, `repeat(n, …)`, `minmax(a, b)`,
/// `<flex>`, and the intrinsic keywords. Line *names* (`[main-start] 1fr`) come back as taffy's
/// positional sets: they were skipped, and an item placed by one landed past the grid.
fn track_list(value: &str) -> (GridTrackVec<GridTemplateComponent<String>>, GridTrackVec<GridTrackVec<String>>) {
    let text = value.trim();
    let mut out = GridTrackVec::new();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return (out, GridTrackVec::new());
    }
    // `names[i]` names line i + 1: the set before track i, and one more after the last track (taffy's
    // positional sets, one per line, since every track here is a `Single`).
    let mut names: Vec<Vec<String>> = vec![Vec::new()];
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    while let Ok(component) = next_component(&mut parser) {
        let Some(component) = component else { break };
        match component.as_str() {
            t if t.starts_with('[') => names.last_mut().expect("one set").extend(line_names(t)),
            t if t.to_ascii_lowercase().starts_with("repeat(") => {
                if let Some((repeat, repeat_names)) = parse_repeat(t) {
                    let mut sets = repeat_names.into_iter();
                    if let Some(first) = sets.next() {
                        names.last_mut().expect("one set").extend(first);
                    }
                    for (track, set) in repeat.into_iter().zip(sets) {
                        out.push(track);
                        names.push(set);
                    }
                }
            }
            t => {
                if let Some(track) = track(t) {
                    out.push(GridTemplateComponent::Single(track));
                    names.push(Vec::new());
                }
            }
        }
    }
    let named = names.iter().any(|n| !n.is_empty());
    (out, if named { names } else { GridTrackVec::new() })
}

/// The names in one `[a b]` group.
fn line_names(group: &str) -> Vec<String> {
    group.trim_start_matches('[').trim_end_matches(']').split_whitespace().map(str::to_string).collect()
}

/// `repeat(<n>, <track-list>)` with a fixed count, expanded, and the line-name sets of the expansion
/// (`tracks + 1` of them): each repetition's last set merges with the next one's first, as CSS says.
fn parse_repeat(text: &str) -> Option<(Vec<GridTemplateComponent<String>>, Vec<Vec<String>>)> {
    let inner = text.strip_prefix("repeat(").or_else(|| text.strip_prefix("REPEAT("))?;
    let inner = inner.strip_suffix(')')?;
    let (count, rest) = inner.split_once(',')?;
    let count: usize = count.trim().parse().ok()?;
    if count == 0 || count > 1000 {
        return None; // `auto-fill`/`auto-fit` land here, and so does a hostile count.
    }
    let mut one = Vec::new();
    let mut one_names: Vec<Vec<String>> = vec![Vec::new()];
    let mut pi = ParserInput::new(rest);
    let mut parser = Parser::new(&mut pi);
    while let Ok(Some(component)) = next_component(&mut parser) {
        if component.starts_with('[') {
            one_names.last_mut().expect("one set").extend(line_names(&component));
        } else if let Some(t) = track(&component) {
            one.push(GridTemplateComponent::Single(t));
            one_names.push(Vec::new());
        }
    }
    let len = one.len();
    let mut sets: Vec<Vec<String>> = vec![Vec::new(); len * count + 1];
    for rep in 0..count {
        for (i, set) in one_names.iter().enumerate() {
            sets[rep * len + i].extend(set.iter().cloned());
        }
    }
    let tracks = one.iter().cycle().take(len * count).cloned().collect();
    Some((tracks, sets))
}

/// `grid-template-areas` as taffy's areas: one quoted string per row, cells separated by
/// white space, a run of `.` a null cell. Every name must cover a filled rectangle and every row the same
/// number of columns, or the declaration is invalid (`none`). Lines are 1-based.
fn template_areas(value: &str) -> Option<taffy::style::GridTemplateAreas<String>> {
    let text = value.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return None;
    }
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    loop {
        match parser.next() {
            Ok(Token::QuotedString(s)) => {
                let s = s.to_string();
                rows.push(area_cells(&s)?);
            }
            Ok(_) => return None,
            Err(_) => break,
        }
    }
    let columns = rows.first()?.len();
    if columns == 0 || rows.iter().any(|r| r.len() != columns) {
        return None;
    }
    let mut spans: Vec<(String, usize, usize, usize, usize)> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let Some(name) = cell else { continue };
            match spans.iter_mut().find(|s| &s.0 == name) {
                Some(s) => {
                    s.1 = s.1.min(r);
                    s.2 = s.2.max(r);
                    s.3 = s.3.min(c);
                    s.4 = s.4.max(c);
                }
                None => spans.push((name.clone(), r, r, c, c)),
            }
        }
    }
    let mut areas = GridTrackVec::new();
    for (name, r0, r1, c0, c1) in spans {
        let filled = (r0..=r1).all(|r| (c0..=c1).all(|c| rows[r][c].as_deref() == Some(name.as_str())));
        let count = rows.iter().flatten().filter(|c| c.as_deref() == Some(name.as_str())).count();
        if !filled || count != (r1 - r0 + 1) * (c1 - c0 + 1) {
            return None;
        }
        areas.push(taffy::style::GridTemplateArea {
            name,
            row_start: (r0 + 1) as u16,
            row_end: (r1 + 2) as u16,
            column_start: (c0 + 1) as u16,
            column_end: (c1 + 2) as u16,
        });
    }
    Some(taffy::style::GridTemplateAreas { areas, row_count: rows.len() as u16, column_count: columns as u16 })
}

/// The cells of one `grid-template-areas` row: a name, or `None` for a null cell (a run of `.`); any other
/// character makes the row invalid.
fn area_cells(row: &str) -> Option<Vec<Option<String>>> {
    let mut out = Vec::new();
    let mut chars = row.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
        } else if ch == '.' {
            while chars.peek() == Some(&'.') {
                chars.next();
            }
            out.push(None);
        } else if ch.is_alphanumeric() || ch == '-' || ch == '_' || !ch.is_ascii() {
            let mut name = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
                    name.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Some(name));
        } else {
            return None;
        }
    }
    Some(out)
}

/// One track size: a length, a percentage, a `<flex>`, `minmax()`, or an intrinsic keyword.
fn track(text: &str) -> Option<TrackSizingFunction> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(inner) = lower.strip_prefix("minmax(").and_then(|s| s.strip_suffix(')')) {
        let (min, max) = inner.split_once(',')?;
        return Some(TrackSizingFunction { min: min_track(min)?, max: max_track(max)? });
    }
    Some(TrackSizingFunction { min: min_track(t)?, max: max_track(t)? })
}

fn min_track(text: &str) -> Option<MinTrackSizingFunction> {
    let t = text.trim();
    Some(match t.to_ascii_lowercase().as_str() {
        "auto" => MinTrackSizingFunction::auto(),
        "min-content" => MinTrackSizingFunction::min_content(),
        "max-content" => MinTrackSizingFunction::max_content(),
        // A flexible track has no minimum of its own: `1fr` is `minmax(auto, 1fr)`.
        s if s.ends_with("fr") => MinTrackSizingFunction::auto(),
        _ => match parse(t)? {
            CssLengthPercentage::Length(l) => MinTrackSizingFunction::length(l.value),
            CssLengthPercentage::Percentage(p) => MinTrackSizingFunction::percent(p / 100.0),
        },
    })
}

fn max_track(text: &str) -> Option<MaxTrackSizingFunction> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    Some(match lower.as_str() {
        "auto" => MaxTrackSizingFunction::auto(),
        "min-content" => MaxTrackSizingFunction::min_content(),
        "max-content" => MaxTrackSizingFunction::max_content(),
        _ => {
            if let Some(n) = lower.strip_suffix("fr") {
                MaxTrackSizingFunction::fr(n.trim().parse().ok()?)
            } else {
                match parse(t)? {
                    CssLengthPercentage::Length(l) => MaxTrackSizingFunction::length(l.value),
                    CssLengthPercentage::Percentage(p) => MaxTrackSizingFunction::percent(p / 100.0),
                }
            }
        }
    })
}

/// `grid-auto-flow`: `[ row | column ] || dense`.
fn grid_auto_flow(v: &str) -> taffy::style::GridAutoFlow {
    let words: Vec<String> = v.split_ascii_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    let column = words.iter().any(|w| w == "column");
    let dense = words.iter().any(|w| w == "dense");
    match (column, dense) {
        (false, false) => taffy::style::GridAutoFlow::Row,
        (true, false) => taffy::style::GridAutoFlow::Column,
        (false, true) => taffy::style::GridAutoFlow::RowDense,
        (true, true) => taffy::style::GridAutoFlow::ColumnDense,
    }
}

/// `grid-auto-rows` / `grid-auto-columns`: a plain list of track sizes, no `repeat()`.
fn auto_tracks(value: &str) -> GridTrackVec<TrackSizingFunction> {
    let text = value.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("auto") {
        return GridTrackVec::new();
    }
    let mut out = GridTrackVec::new();
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    while let Ok(Some(component)) = next_component(&mut parser) {
        if let Some(t) = track(&component) {
            out.push(t);
        }
    }
    out
}

/// `grid-row-start` and friends: a line number, a span, or a name.
fn placement(value: &str) -> GridPlacement<String> {
    let t = value.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("auto") {
        return GridPlacement::Auto;
    }
    if let Some(rest) = t.strip_prefix("span ").or_else(|| t.strip_prefix("SPAN ")) {
        return match rest.trim().parse::<u16>() {
            Ok(n) if n > 0 => GridPlacement::Span(n),
            // `span <name>` places by a named line, which v1 does not track.
            _ => GridPlacement::Auto,
        };
    }
    match t.parse::<i16>() {
        // Line 0 does not exist in CSS; a stylesheet that writes it means nothing by it.
        Ok(0) => GridPlacement::Auto,
        Ok(n) => GridPlacement::Line(n.into()),
        Err(_) => GridPlacement::NamedLine(t.to_string(), 1),
    }
}

/// One whitespace-separated component of a track list, with any block kept whole.
///
/// `minmax(100px, 1fr)` is one component and `[a] 1fr [b]` is three, which a naive split on
/// whitespace gets wrong in both directions.
fn next_component(parser: &mut Parser) -> Result<Option<String>, ()> {
    parser.skip_whitespace();
    let start = parser.position();
    let opens_block = match parser.next() {
        Err(_) => return Ok(None),
        Ok(t) => matches!(
            t,
            Token::Function(_)
                | Token::ParenthesisBlock
                | Token::SquareBracketBlock
                | Token::CurlyBracketBlock
        ),
    };
    if opens_block {
        let _ = parser.parse_nested_block(|inner| -> Result<(), cssparser::ParseError<'_, ()>> {
            while inner.next().is_ok() {}
            Ok(())
        });
    }
    Ok(Some(parser.slice(start..parser.position()).trim().to_string()))
}

fn align_items(value: &str) -> Option<AlignItems> {
    Some(match value.trim() {
        "start" => AlignItems::START,
        "end" => AlignItems::END,
        "flex-start" => AlignItems::FLEX_START,
        "flex-end" => AlignItems::FLEX_END,
        "self-start" => AlignItems::SELF_START,
        "self-end" => AlignItems::SELF_END,
        "center" => AlignItems::CENTER,
        "baseline" => AlignItems::BASELINE,
        "stretch" => AlignItems::STRETCH,
        // `normal` and `auto` are "whatever the layout mode's default is", which taffy spells None.
        _ => return None,
    })
}

fn align_content(value: &str) -> Option<AlignContent> {
    Some(match value.trim() {
        "start" => AlignContent::START,
        "end" => AlignContent::END,
        "flex-start" => AlignContent::FLEX_START,
        "flex-end" => AlignContent::FLEX_END,
        "center" => AlignContent::CENTER,
        "stretch" => AlignContent::STRETCH,
        "space-between" => AlignContent::SPACE_BETWEEN,
        "space-around" => AlignContent::SPACE_AROUND,
        "space-evenly" => AlignContent::SPACE_EVENLY,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::computed::PropertySet;

    /// A style with exactly these declarations set, as computed values.
    fn style(pairs: &[(PropertyId, &str)]) -> (StyleArena, StyleKey) {
        let mut arena = StyleArena::default();
        let mut own = PropertySet::default();
        let mut inherited = PropertySet::default();
        for (p, v) in pairs {
            if p.is_inherited() {
                inherited.set(*p, v);
            } else {
                own.set(*p, v);
            }
        }
        let key = StyleKey {
            inherited: arena.intern_inherited(inherited),
            own: arena.intern_own(own),
        };
        (arena, key)
    }

    fn taffy(pairs: &[(PropertyId, &str)]) -> Style {
        let (arena, key) = style(pairs);
        derive(&arena, key)
    }

    #[test]
    fn lengths_percentages_and_auto_all_arrive_as_themselves() {
        let s = taffy(&[
            (PropertyId::Width, "100px"),
            (PropertyId::Height, "50%"),
            (PropertyId::MarginLeft, "auto"),
            (PropertyId::PaddingTop, "8px"),
        ]);
        assert_eq!(s.size.width, Dimension::length(100.0));
        assert_eq!(s.size.height, Dimension::percent(0.5));
        assert_eq!(s.margin.left, LengthPercentageAuto::auto());
        assert_eq!(s.padding.top, LengthPercentage::length(8.0));
        assert_eq!(s.size.width, Dimension::length(100.0));
    }

    // A resolved percentage padding is a LayoutUnit, truncated: 10% of 147.109375 is 14.703125 in Chrome 152.0.7977.82.
    #[test]
    fn a_resolved_percentage_padding_is_a_layout_unit() {
        let s = taffy(&[(PropertyId::PaddingTop, "10%"), (PropertyId::PaddingLeft, "25%")]);
        let r = with_resolved_padding(&s, 147.109375);
        assert_eq!(r.padding.top, LengthPercentage::length(14.703125));
        assert_eq!(r.padding.left, LengthPercentage::length(36.765625));
        assert_eq!(r.padding.right, LengthPercentage::length(0.0));
    }

    #[test]
    fn an_unset_property_is_its_initial_value() {
        let s = taffy(&[]);
        assert_eq!(s.size.width, Dimension::auto());
        assert_eq!(s.margin.top, LengthPercentageAuto::length(0.0));
        assert_eq!(s.padding.top, LengthPercentage::length(0.0));
        assert_eq!(s.border.top, LengthPercentage::length(0.0));
        assert_eq!(s.display, Display::Block, "the initial is `inline`, which lays out as a block");
        assert_eq!(s.flex_shrink, 1.0);
        assert_eq!(s.flex_grow, 0.0);
    }

    // A border with no style has no width, whatever the width property says.
    #[test]
    fn a_border_with_no_style_has_no_width() {
        let s = taffy(&[
            (PropertyId::BorderTopWidth, "2px"),
            (PropertyId::BorderTopStyle, "none"),
            (PropertyId::BorderLeftWidth, "2px"),
            (PropertyId::BorderLeftStyle, "solid"),
        ]);
        assert_eq!(s.border.top, LengthPercentage::length(0.0));
        assert_eq!(s.border.left, LengthPercentage::length(2.0));
    }

    #[test]
    fn the_keyword_border_widths_resolve_the_way_chrome_resolves_them() {
        for (keyword, px) in [("thin", 1.0), ("medium", 3.0), ("thick", 5.0)] {
            let s = taffy(&[
                (PropertyId::BorderTopWidth, keyword),
                (PropertyId::BorderTopStyle, "solid"),
            ]);
            assert_eq!(s.border.top, LengthPercentage::length(px), "{keyword}");
        }
    }

    #[test]
    fn display_reduces_to_the_modes_v1_implements() {
        assert_eq!(display("block"), Display::Block);
        assert_eq!(display("flex"), Display::Flex);
        assert_eq!(display("inline-flex"), Display::Flex);
        assert_eq!(display("grid"), Display::Grid);
        assert_eq!(display("none"), Display::None);
        assert_eq!(display("table"), Display::Block, "no table algorithm in v1");
        assert_eq!(display("list-item"), Display::Block);
    }

    // Inline is the one display value taffy cannot express, not the one that has no box. Its
    // geometry is a run of line boxes, which `crate::layout::ifc` produces and `client_rects`
    // returns one entry of per line.
    #[test]
    fn only_none_and_contents_generate_no_box() {
        assert!(!generates_box("none"));
        assert!(!generates_box("contents"));
        assert!(generates_box("inline"));
        assert!(generates_box("block"));
        assert!(generates_box("inline-block"));
        assert!(generates_box("list-item"));
        assert!(generates_box("flex"));
    }

    // Which formatting context each value puts a box in — the distinction taffy's `Display` throws
    // away, and the one that decides whether a box sits ON a line or BETWEEN lines.
    #[test]
    fn display_says_which_formatting_context_a_box_belongs_to() {
        assert_eq!(level("inline"), BoxLevel::Inline);
        assert_eq!(level("inline-block"), BoxLevel::AtomicInline);
        assert_eq!(level("inline-flex"), BoxLevel::AtomicInline);
        assert_eq!(level("inline-grid"), BoxLevel::AtomicInline);
        assert_eq!(level("block"), BoxLevel::Block);
        assert_eq!(level("flex"), BoxLevel::Block);
        assert_eq!(level("list-item"), BoxLevel::Block);
        assert_eq!(level("table"), BoxLevel::Block);
        assert!(BoxLevel::Inline.is_inline_level());
        assert!(BoxLevel::AtomicInline.is_inline_level());
        assert!(!BoxLevel::Block.is_inline_level());
    }

    #[test]
    fn normal_is_a_zero_gap_and_not_a_parse_failure() {
        let s = taffy(&[(PropertyId::RowGap, "normal"), (PropertyId::ColumnGap, "10px")]);
        assert_eq!(s.gap.height, LengthPercentage::length(0.0));
        assert_eq!(s.gap.width, LengthPercentage::length(10.0));
    }

    #[test]
    fn the_alignment_keywords_map_and_normal_means_the_modes_default() {
        assert_eq!(align_items("center"), Some(AlignItems::CENTER));
        assert_eq!(align_items("normal"), None);
        assert_eq!(align_items("auto"), None);
        assert_eq!(align_content("space-between"), Some(AlignContent::SPACE_BETWEEN));
        assert_eq!(align_content("normal"), None);
        // `start` and `flex-start` are different keywords, and stay different.
        assert_ne!(align_items("start"), align_items("flex-start"));
    }

    #[test]
    fn aspect_ratio_takes_a_fraction_or_a_number() {
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "16 / 9")]).aspect_ratio, Some(16.0 / 9.0));
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "1.5")]).aspect_ratio, Some(1.5));
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "auto")]).aspect_ratio, None);
        // `auto && <ratio>` -- the `width`/`height` hints' form -- is its ratio to a box with none of its own,
        // and a degenerate ratio is `auto`
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "auto 4 / 2")]).aspect_ratio, Some(2.0));
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "4/2 auto")]).aspect_ratio, Some(2.0));
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "auto 40 / 0")]).aspect_ratio, None);
        assert_eq!(taffy(&[(PropertyId::AspectRatio, "0 / 30")]).aspect_ratio, None);
        let p = crate::layout::replaced::PreferredRatio::parse("auto 40.5 / 30").expect("a ratio");
        assert_eq!((p.width, p.height, p.auto), (40.5, 30.0, true));
    }

    // The claim the whole file rests on: styles are interned, so a page of identical elements
    // derives one layout style and not one per node.
    #[test]
    fn identical_styles_intern_to_one_layout_style() {
        let (arena, key) = style(&[(PropertyId::Width, "100px")]);
        let mut styles = LayoutStyles::default();
        let a = styles.intern(&arena, key);
        let b = styles.intern(&arena, key);
        assert_eq!(a, b);
        assert_eq!(styles.len(), 1);
        assert_eq!(styles.get(a).size.width, Dimension::length(100.0));
    }
}
