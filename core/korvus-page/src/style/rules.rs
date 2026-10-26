//! What a parsed stylesheet *is*, and the index that keeps matching it from being quadratic.
//!
//! Today `lib.rs:1339` stores a sheet as `{cssText, type}` — a text blob — and `replaceSync` splits
//! it on `}`. Nothing can be asked of that: not which rules match an element, not which declaration
//! wins, not what `color` computes to. This is the shape that can answer.
//!
//! # Ranges, not strings
//!
//! A sheet owns its text once and everything else points into it. A rule's declarations are a span
//! of the sheet's declaration vector; a declaration's value is a span of the sheet's text. A real
//! page has tens of thousands of declarations, and one `String` each would be tens of thousands of
//! allocations to build a structure that is then only read.
//!
//! Keeping the value as *text* rather than a parsed value is design D7: layout reads about fifty
//! properties and wants them typed and compact, while `getComputedStyle` must answer for all of
//! them and mostly just hands the specified value back. Parsing `cursor: pointer` into a typed enum
//! would cost a match arm and a memory slot to produce a string again on the way out.

use crate::style::at_rules::DescriptorBlock;
use crate::style::media::{MediaContext, MediaQueryList};
use crate::style::property::PropertyId;
use korvus_dom::{AncestorBloom, Selector, SelectorBucket};
use std::collections::HashMap;

/// A half-open byte range of a stylesheet's text. `Copy` and eight bytes, so declarations stay
/// small enough to live in a flat vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const EMPTY: Span = Span { start: 0, end: 0 };

    pub fn of(range: std::ops::Range<usize>) -> Span {
        Span { start: range.start as u32, end: range.end as u32 }
    }

    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }
}

/// Where a declaration came from. This is the *first* thing the cascade compares — ahead of
/// specificity, which is why an author's `p { color: red }` loses to a user's `!important` rule no
/// matter how specific it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// The browser's own stylesheet — what makes a `<div>` a block and an `<h1>` big.
    UserAgent,
    /// The user's stylesheet (accessibility overrides, extensions).
    User,
    /// What HTML attributes map to (`hidden` is `display: none`): below every author rule and above
    /// the user's (Blink's `kAuthorPresentationalHint`).
    PresentationalHint,
    /// The page's own stylesheets and inline `style` attributes.
    Author,
}

/// How strongly a declaration is asserted, as one number the cascade can sort on.
///
/// CSS Cascade 4 §6.1 orders the origins, and `!important` *reverses* that order rather than
/// adding to it: the point of a user's `!important` is to beat the page, so it has to outrank
/// something that already outranked it. Flattening both facts into a single rung is what lets the
/// whole cascade be one lexicographic comparison instead of a nest of conditionals.
///
/// Higher wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Precedence(u8);

impl Precedence {
    pub fn of(origin: Origin, important: bool) -> Precedence {
        Precedence(match (origin, important) {
            (Origin::UserAgent, false) => 0,
            (Origin::User, false) => 1,
            (Origin::PresentationalHint, _) => 2,
            (Origin::Author, false) => 3,
            // CSS animations sit HERE, between the two — see `animation()`.
            // and back down the other way
            (Origin::Author, true) => 5,
            (Origin::User, true) => 6,
            (Origin::UserAgent, true) => 7,
        })
    }

    /// A CSS animation's value: above every normal author declaration, below every `!important`
    /// one.
    ///
    /// Not read off the spec. Measured on `/usr/bin/google-chrome` 150.0.7871.46, with an element
    /// carrying an `animation` whose keyframes set `opacity` and an inline `style` attribute that
    /// also sets it:
    ///
    ///   inline `opacity: 0.1`            -> computed `0.799004`   the animation wins
    ///   inline `opacity: 0.1 !important` -> computed `0.1`        the author wins
    ///
    /// An inline declaration is the strongest thing in the author origin, so beating it places the
    /// animation above the whole of author-normal, and losing to its `!important` places it below
    /// author-important. That is exactly one rung, and it is this one.
    pub fn animation() -> Precedence {
        Precedence(4)
    }

    /// A CSS transition's value: above everything, `!important` included.
    ///
    /// The one rung in the cascade that outranks a UA `!important` declaration. A transition is not
    /// asserting a value, it is smoothing the journey to one that has already won, so it has to be
    /// able to override the winner while it runs.
    pub fn transition() -> Precedence {
        Precedence(8)
    }
}

/// One `property: value` pair. The value stays as written; see the module note on D7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declaration {
    pub property: PropertyId,
    pub value: Span,
    pub important: bool,
    /// A component its shorthand omitted and Blink fills with `CSSInitialValue` (`outline`, `list-style`,
    /// `background`, `text-emphasis`, `-webkit-text-stroke`): the cascade reads `value`, the CSSOM
    /// writes `initial` and leaves it out of the shorthand (`style::serialize`).
    pub implicit: bool,
}

/// A declaration kept as text: a custom property, or one whose value contains `var()`.
///
/// Two different reasons, one representation, because both are declarations the parser deliberately
/// does not understand yet.
///
/// A **custom property** has no [`PropertyId`] — its name is open-ended, so it cannot be a dense
/// index — and nothing about it is typed: its value is whatever tokens the author wrote, preserved
/// exactly, which is the whole point of them.
///
/// A declaration **containing `var()`** cannot be understood at parse time either. `margin: var(--g)`
/// might be one value or four, and `color: var(--x)` might not be a colour at all, so it is neither
/// expanded nor validated until the cascade knows the element's custom properties. CSS calls the
/// failure mode that creates "invalid at computed-value time".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawDeclaration {
    /// The property name as written — with the leading `--` for a custom property, and not folded
    /// to lowercase, since custom property names are case-sensitive.
    pub name: Span,
    pub value: Span,
    pub important: bool,
    /// How many of its block's declarations (longhands) were parsed before it: where it stands among them, which
    /// Blink's declaration block keeps and its `cssText` and `item()` read (Chrome 150 writes
    /// `.r { --x: 1px; color: red }` back as `--x: 1px; color: red;`, where this left the custom property out).
    pub after: u32,
}

/// Which kind of rule this is — the discriminant `cssRules[i]` needs to know which interface to
/// hand the page, and the cascade needs to know what the rule's children mean.
///
/// A style rule keeps its declarations in the sheet's pools. An at-rule with a descriptor body keeps
/// its own block instead; see [`crate::style::at_rules`] for why. A GROUPING rule (`@media`,
/// `@supports`, `@layer`, an `@import` whose sheet has arrived, a nesting style rule) carries its
/// contents as [`Rule::children`] -- the CSSOM's tree, which is what `cssRules` walks. The cascade
/// never walks the tree: [`Stylesheet::rebuild`] flattens it into [`CascadeRule`]s.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleKind {
    Style,
    /// `@position-try <dashed-ident> { … }`. Its name is not settable.
    PositionTry { name: String, block: DescriptorBlock },
    /// `@counter-style <counter-style-name> { … }`. Its name IS settable.
    CounterStyle { name: String, block: DescriptorBlock },
    /// `@media <query list> { rules }` -- `media` indexes the sheet's media pool, `text` is the list
    /// as written (whitespace-collapsed), which is what `conditionText` reads back.
    Media { media: u32, text: String },
    /// `@supports <condition> { rules }`, decided once at parse: nothing about the engine changes
    /// while a page runs, so a supports condition is a constant.
    Supports { condition: String, matches: bool },
    /// `@layer <name>? { rules }`. `name` is `None` for an anonymous layer, which is a DIFFERENT layer
    /// every time it is written -- `anon` is what makes it unique.
    LayerBlock { name: Option<String>, anon: u64 },
    /// `@layer a, b.c;` -- declares order and nothing else.
    LayerStatement { names: Vec<String> },
    /// `@import url(x) [layer|layer(n)] [supports(c)] [media];`. Its children are the imported
    /// sheet's rules once the bytes have arrived ([`Stylesheet::attach_import`]).
    Import(Box<ImportRule>),
    /// `@keyframes <name> { keyframe blocks }`; the children are [`RuleKind::Keyframe`]s.
    Keyframes { name: String },
    /// One keyframe block: the 0..1 offsets its selector named, and the declarations in `decls`.
    Keyframe { offsets: Vec<f32> },
    /// `@font-face { descriptors }`.
    FontFace { block: DescriptorBlock },
    /// The declarations that follow a nested rule inside a style rule (CSS Nesting 1 §2.3): they
    /// apply with the parent's selectors, at their own position in the cascade order.
    NestedDeclarations,
    /// `@container [name] <query> { rules }`. The query is parsed again by
    /// [`Stylesheet::rebuild`] into [`crate::style::container::ContainerCondition`]; `query` is the text
    /// after the name, whitespace-collapsed, which is what `containerQuery` reads back.
    Container { name: Option<String>, query: String },
    /// `@scope [(<start>)]? [to (<end>)]? { rules }`, the selector lists as written.
    Scope { start: Option<String>, end: Option<String> },
    /// `@property <custom-property-name> { descriptors }`.
    Property { name: String, block: DescriptorBlock },
    /// `@page <page-selector-list>? { descriptors }` -- CSSOM only: this engine does not
    /// paginate.
    Page { selector: String, block: DescriptorBlock },
    /// `@starting-style { rules }`: kept for the CSSOM; its rules style an element's
    /// before-change state for a transition, which no cascade here reads.
    StartingStyle,
}

/// What an `@import` rule says, and whether its sheet has arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRule {
    /// The URL as written -- `href` reads this back unresolved.
    pub href: String,
    /// The media query list, if one was written: index into the media pool, and its text.
    pub media: Option<(u32, String)>,
    /// `supports(<condition>)` as written, without the function.
    pub supports: Option<String>,
    /// `layer` (Some("")) or `layer(<name>)` (Some(name)); `None` when the import is unlayered.
    pub layer: Option<String>,
    /// Makes an anonymous `layer` unique, as for [`RuleKind::LayerBlock`].
    pub anon: u64,
    /// The resolved URL the sheet was fetched from, once it has been (`styleSheet.href`).
    pub url: Option<String>,
    /// The imported sheet's rules are this rule's children -- `styleSheet` is non-null.
    pub loaded: bool,
    /// Index into the sheet's [`Stylesheet::bases`]: what a relative `url()` inside the imported
    /// sheet resolves against.
    pub base: u32,
}

impl RuleKind {
    /// The at-rule's name, or `None` for a rule that has no settable/readable `name`.
    pub fn name(&self) -> Option<&str> {
        match self {
            RuleKind::PositionTry { name, .. } | RuleKind::CounterStyle { name, .. } | RuleKind::Keyframes { name } => {
                Some(name)
            }
            RuleKind::LayerBlock { name, .. } => Some(name.as_deref().unwrap_or("")),
            _ => None,
        }
    }

    pub fn block(&self) -> Option<&DescriptorBlock> {
        match self {
            RuleKind::PositionTry { block, .. }
            | RuleKind::CounterStyle { block, .. }
            | RuleKind::FontFace { block }
            | RuleKind::Property { block, .. }
            | RuleKind::Page { block, .. } => Some(block),
            _ => None,
        }
    }

    pub fn block_mut(&mut self) -> Option<&mut DescriptorBlock> {
        match self {
            RuleKind::PositionTry { block, .. }
            | RuleKind::CounterStyle { block, .. }
            | RuleKind::FontFace { block }
            | RuleKind::Property { block, .. }
            | RuleKind::Page { block, .. } => Some(block),
            _ => None,
        }
    }

    /// Does this rule hold a child rule list (`cssRules`)?
    pub fn is_grouping(&self) -> bool {
        matches!(
            self,
            RuleKind::Style
                | RuleKind::Media { .. }
                | RuleKind::Supports { .. }
                | RuleKind::LayerBlock { .. }
                | RuleKind::Keyframes { .. }
                | RuleKind::Container { .. }
                | RuleKind::Scope { .. }
                | RuleKind::StartingStyle
        )
    }
}

/// One rule as the CSSOM sees it: its kind, the selectors it was written with, its declarations,
/// and the rules nested inside it.
///
/// A rule is a *block*, not a selector, because that is what the CSSOM says it is —
/// `h1, .lede { color: red }` is one entry in `cssRules` with a `selectorText` of `"h1, .lede"`.
///
/// A NESTED style rule keeps its selectors as written, `&` and all (`& .c`, or `.c` read as `& .c`):
/// that is what `selectorText` answers. What the cascade matches is the resolved form in
/// [`CascadeRule::selectors`].
///
/// There is no `order` field: a rule's order is its position in its parent's list, and keeping a
/// copy of that would be one more thing to renumber every time `insertRule` puts one in the middle.
#[derive(Debug, Clone)]
pub struct Rule {
    pub kind: RuleKind,
    pub selectors: Vec<Selector>,
    /// Into the sheet's `declarations` vector.
    pub decls: Span,
    /// Into the sheet's `raw_declarations` vector.
    pub custom: Span,
    /// The rules nested inside this one, in source order.
    pub children: Vec<Rule>,
}

impl Rule {
    pub fn new(kind: RuleKind) -> Rule {
        Rule { kind, selectors: Vec::new(), decls: Span::EMPTY, custom: Span::EMPTY, children: Vec::new() }
    }

    /// `selectorText`: the selector list as the CSSOM serializes it.
    pub fn selector_text(&self) -> String {
        self.selectors.iter().map(Selector::to_css_string).collect::<Vec<_>>().join(", ")
    }
}

/// Anything that addresses a span of the sheet's declaration pools.
pub trait Declares {
    fn decl_span(&self) -> Span;
    fn custom_span(&self) -> Span;
}

impl Declares for Rule {
    fn decl_span(&self) -> Span {
        self.decls
    }
    fn custom_span(&self) -> Span {
        self.custom
    }
}

/// One rule as the CASCADE sees it: a flat entry with resolved selectors and every condition it sits
/// under, in cascade order. Built from the tree by [`Stylesheet::rebuild`]; never edited directly.
#[derive(Debug, Clone)]
pub struct CascadeRule {
    /// The selectors to match, with every `&` replaced by the enclosing rule's.
    pub selectors: Vec<Selector>,
    pub decls: Span,
    pub custom: Span,
    /// Index into the sheet's media CHAINS: every `@media` (and `@import` media list) this rule is
    /// inside, all of which must match. `0` is the empty chain.
    pub media: u32,
    /// Index into [`Stylesheet::layers`]: `0` is unlayered.
    pub layer: u32,
    /// Index into [`Stylesheet::bases`] -- `0` means the sheet's own base.
    pub base: u32,
    /// Index into the sheet's container CHAINS: every `@container` this rule is inside, all of which
    /// must hold for the element. `0` is the empty chain.
    pub container: u32,
    /// `0`, or one more than an index into the sheet's scopes: the innermost `@scope` this rule is
    /// inside.
    pub scope: u32,
}

/// One `@scope`'s selectors, parsed. `start` is `None` for a prelude-less `@scope`,
/// whose root is the owner node's parent element.
#[derive(Debug, Clone)]
pub struct ScopeInfo {
    pub start: Option<Vec<Selector>>,
    pub end: Vec<Selector>,
}

impl Declares for CascadeRule {
    fn decl_span(&self) -> Span {
        self.decls
    }
    fn custom_span(&self) -> Span {
        self.custom
    }
}

/// One segment of a layer name. A named layer is the same layer wherever it is written; an anonymous
/// one is only ever itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LayerSeg {
    Named(String),
    Anon(u64),
}

/// A layer's full path from the outermost layer: `@layer a { @layer b {} }` is `[a, b]`.
pub type LayerPath = Vec<LayerSeg>;

static NEXT_ANON_LAYER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A fresh identity for an anonymous layer.
pub fn anon_layer_id() -> u64 {
    NEXT_ANON_LAYER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One block inside a `@keyframes` rule: the offsets its selector named, and its declarations.
///
/// The offsets are a LIST because one block may carry several — `0%, 50% { … }` is one block at two
/// points on the timeline, and CSS Animations says each is a separate keyframe with the same
/// declarations. `from` is 0 and `to` is 1; a percentage is divided by 100, so the offsets are the
/// 0..1 progress the animation model works in rather than the text the author wrote.
#[derive(Debug, Clone)]
pub struct KeyframeBlock {
    pub offsets: Vec<f32>,
    /// The block's longhand declarations, addressing the sheet's declaration pool.
    pub decls: Span,
}

/// One `@keyframes` rule, as the animation model reads it.
///
/// NOT a [`CascadeRule`], and deliberately: a cascade rule carries selectors the index matches
/// elements against, and a keyframes rule must never match anything.
#[derive(Debug, Clone)]
pub struct KeyframesRule {
    /// The name as the author wrote it. `<custom-ident>` is case-SENSITIVE: `@keyframes Slide` is
    /// not reached by `animation-name: slide`, and Chrome's computed `animation-name` keeps the
    /// case it was given (measured).
    pub name: String,
    pub blocks: Vec<KeyframeBlock>,
}

/// One `@font-face` rule, as a font loader reads it: its descriptors, the media chain it sits under
/// and the base its `src` URLs resolve against.
#[derive(Debug, Clone)]
pub struct FontFaceEntry {
    pub block: DescriptorBlock,
    pub media: u32,
    pub base: u32,
}

/// A parsed stylesheet: the text, the declarations pointing into it, and the rules pointing into
/// those.
#[derive(Debug, Clone)]
pub struct Stylesheet {
    pub origin: Origin,
    /// `true` while `<link>`ed CSS is in flight or a sheet has been disabled — it stays in the list
    /// (the CSSOM can still see it) but contributes nothing.
    pub disabled: bool,
    /// CSSOM's **origin-clean flag**: may script READ this sheet's rules?
    ///
    /// `true` for everything a page wrote itself and for every same-origin sheet. `false` for a
    /// cross-origin `<link>` fetched without CORS — a sheet the page is STYLED by but may not read.
    /// It gates `cssRules` / `rules` / `insertRule` / `deleteRule` / `addRule` / `removeRule`, which
    /// throw `SecurityError`, and NOTHING in matching or the cascade. Measured on stock Chrome
    /// 150.0.7871.46 over two loopback origins: a cross-origin sheet with no `crossorigin` attribute
    /// throws `SecurityError: … Cannot access rules` on `cssRules` while its `width: 123px` still
    /// reaches `getComputedStyle`; with `crossorigin` and an `Access-Control-Allow-Origin` that
    /// permits the document, the same sheet reads back normally.
    pub origin_clean: bool,
    /// The URL this sheet's own relative `url()`s and `@import`s resolve against -- a `<link>`'s
    /// resolved href. `None` for a sheet written in the document, which resolves against the
    /// document's base URL (known to the caller, not here).
    pub base_url: Option<String>,
    /// The sheet's OWN media query list -- CSSOM's "media" of the sheet, which for a `<style>` or
    /// `<link>` is initialised from the element's `media` attribute and for a constructed sheet from
    /// `CSSStyleSheetInit.media`. `None` is the empty list, which matches everything; a
    /// list that does not match switches the WHOLE sheet off exactly as an enclosing `@media` would,
    /// while the sheet stays in `document.styleSheets` with its rules readable.
    ///
    /// Measured on stock Chrome 150.0.7871.46: `<style media=print>` and `<link rel=stylesheet
    /// media=print>` do not apply on screen (the link is still fetched), `media=""` and
    /// `media="SCREEN"` apply, an unparseable `media="garbage(("` does not.
    pub(crate) own_media: Option<(String, MediaQueryList)>,
    text: String,
    declarations: Vec<Declaration>,
    raw_declarations: Vec<RawDeclaration>,
    rules: Vec<Rule>,
    media: Vec<MediaQueryList>,
    /// The base URL of each `@import`-ed sheet, by [`ImportRule::base`]. Index 0 is a placeholder
    /// for the sheet's own [`Stylesheet::base_url`].
    bases: Vec<Option<String>>,
    // ── derived from `rules`, on the first read after a change (see `rebuild`) ──
    derived: std::sync::OnceLock<Derived>,
}

impl Stylesheet {
    pub fn new(origin: Origin, text: String) -> Stylesheet {
        Stylesheet::from_parts(origin, text, Vec::new())
    }

    /// The parser's entry point: it accumulates declarations while it walks the text, then hands
    /// both over at once. Spans are byte offsets, so moving the text in here keeps them valid.
    pub fn from_parts(origin: Origin, text: String, declarations: Vec<Declaration>) -> Stylesheet {
        Stylesheet {
            origin,
            disabled: false,
            origin_clean: true,
            base_url: None,
            own_media: None,
            text,
            declarations,
            raw_declarations: Vec::new(),
            rules: Vec::new(),
            media: Vec::new(),
            bases: vec![None],
            derived: std::sync::OnceLock::new(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The top-level rules, as `cssRules` lists them.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Replace the rule tree and derive everything the cascade reads from it.
    pub fn set_rules(&mut self, rules: Vec<Rule>) {
        self.rules = rules;
        self.rebuild();
    }

    /// The rules the cascade matches, flattened, in cascade order.
    pub fn cascade_rules(&self) -> &[CascadeRule] {
        &self.derived().cascade
    }

    /// Every layer this sheet declares, by the index [`CascadeRule::layer`] uses, in the order the
    /// sheet declares them. Index 0 is the unlayered "layer".
    pub fn layers(&self) -> &[LayerPath] {
        &self.derived().layers
    }

    /// The sheet's `@keyframes` rules that apply, in source order, the last of a name winning.
    pub fn keyframes(&self) -> &[KeyframesRule] {
        &self.derived().keyframes
    }

    /// The sheet's `@font-face` rules that apply, in source order.
    pub fn font_faces(&self) -> &[FontFaceEntry] {
        &self.derived().font_faces
    }

    /// The `@property` registrations this sheet makes, in source order.
    pub fn registrations(&self) -> &[(String, crate::style::registered::Registration)] {
        &self.derived().properties
    }

    /// Does any rule of this sheet sit under an `@container`?
    pub fn has_container_rules(&self) -> bool {
        self.derived().container_chains.len() > 1
    }

    /// Every `@container` condition of the sheet.
    pub fn container_conditions(&self) -> &[crate::style::container::ContainerCondition] {
        &self.derived().containers
    }

    /// The `@container` conditions a rule sits under, outermost first.
    pub fn container_chain(&self, rule: &CascadeRule) -> impl Iterator<Item = &crate::style::container::ContainerCondition> {
        let d = self.derived();
        d.container_chains
            .get(rule.container as usize)
            .into_iter()
            .flat_map(|c| c.iter())
            .map(|&i| &d.containers[i as usize])
    }

    /// The `@scope` a rule sits in, if any.
    pub fn scope_of(&self, rule: &CascadeRule) -> Option<&ScopeInfo> {
        rule.scope.checked_sub(1).and_then(|i| self.derived().scopes.get(i as usize))
    }

    /// What a relative URL in `rule` resolves against: the imported sheet's URL, or this sheet's
    /// own [`Stylesheet::base_url`].
    pub fn base_of(&self, base: u32) -> Option<&str> {
        match self.bases.get(base as usize) {
            Some(Some(url)) if base != 0 => Some(url),
            _ => self.base_url.as_deref(),
        }
    }

    /// Record a base URL and return its index.
    pub fn push_base(&mut self, url: String) -> u32 {
        self.bases.push(Some(url));
        (self.bases.len() - 1) as u32
    }

    /// Every declaration in the sheet, in source order. Rules address slices of this; an inline
    /// `style="…"` attribute is a sheet with no rules, where this *is* the content.
    pub fn declarations(&self) -> &[Declaration] {
        &self.declarations
    }

    /// The declarations of one rule, in source order (the cascade needs the order to break ties).
    pub fn declarations_of(&self, rule: &impl Declares) -> &[Declaration] {
        let s = rule.decl_span();
        &self.declarations[s.start as usize..s.end as usize]
    }

    /// A declaration's value, exactly as the sheet wrote it.
    pub fn value_of(&self, decl: &Declaration) -> &str {
        self.slice(decl.value)
    }

    /// The text a span addresses.
    pub fn slice(&self, span: Span) -> &str {
        &self.text[span.start as usize..span.end as usize]
    }

    /// Every text-kept declaration in the sheet, in source order.
    pub fn raw_declarations(&self) -> &[RawDeclaration] {
        &self.raw_declarations
    }

    /// The text-kept declarations of one rule.
    pub fn raw_of(&self, rule: &impl Declares) -> &[RawDeclaration] {
        let s = rule.custom_span();
        &self.raw_declarations[s.start as usize..s.end as usize]
    }

    pub fn raw_count(&self) -> usize {
        self.raw_declarations.len()
    }

    pub fn push_raw(&mut self, decl: RawDeclaration) {
        self.raw_declarations.push(decl);
    }

    /// Append a block of declarations, returning the span that addresses them.
    pub fn push_declarations(&mut self, decls: impl IntoIterator<Item = Declaration>) -> Span {
        let start = self.declarations.len();
        self.declarations.extend(decls);
        Span::of(start..self.declarations.len())
    }

    /// Append text-kept declarations, returning the span that addresses them.
    pub fn push_raws(&mut self, decls: impl IntoIterator<Item = RawDeclaration>) -> Span {
        let start = self.raw_declarations.len();
        self.raw_declarations.extend(decls);
        Span::of(start..self.raw_declarations.len())
    }

    /// Remove and return every declaration from `start` on -- how the parser collects what one
    /// declaration expanded to without leaving it in the pool out of order.
    pub(crate) fn take_declarations_from(&mut self, start: usize) -> Vec<Declaration> {
        self.declarations.split_off(start)
    }

    /// [`Stylesheet::take_declarations_from`], for the text-kept pool.
    pub(crate) fn take_raw_from(&mut self, start: usize) -> Vec<RawDeclaration> {
        self.raw_declarations.split_off(start)
    }

    /// How many declarations are stored — the parser brackets a block with this before and after.
    pub fn declaration_count(&self) -> usize {
        self.declarations.len()
    }

    /// Every longhand reaches the pool through here -- a sheet, an inline `style`, a shorthand's
    /// expansion, the CSSOM -- so a value Chrome keeps in a CANONICAL form is put in that form here,
    /// once: `font-family` is stored as Chrome serializes the parsed list (see
    /// `font_family`), and a list that does not parse declares nothing.
    pub fn push_declaration(&mut self, mut decl: Declaration) {
        if decl.property == PropertyId::FontFamily {
            let text = &self.text[decl.value.start as usize..decl.value.end as usize];
            match crate::style::font_family::canonical(text) {
                Some(c) if c == text => {}
                Some(c) => decl.value = self.intern(&c),
                None => return,
            }
        }
        self.declarations.push(decl);
    }

    /// Add a value that is not in the sheet's own text and return a span addressing it.
    ///
    /// Shorthand expansion needs this: `border: solid` sets four widths to `medium` and four colors
    /// to `currentcolor`, and neither string appears anywhere in the sheet. The text field is a
    /// *value pool*, not a copy of the source — nothing reads the original CSS back out (the CSSOM
    /// serializes `cssText` from the parsed rules, it does not echo the input), so growing it is
    /// free of consequences.
    ///
    /// Not deduplicated: the synthesized values are a handful of short strings and only appear for
    /// shorthands with omitted components. A page with 500 `border:` declarations pays a few
    /// kilobytes, which is not worth a hash map that lives on past parsing.
    pub fn intern(&mut self, value: &str) -> Span {
        let start = self.text.len();
        self.text.push_str(value);
        Span::of(start..self.text.len())
    }

    /// Record an `@media` condition and return the index rules should point at.
    pub fn push_media(&mut self, list: MediaQueryList) -> u32 {
        self.media.push(list);
        (self.media.len() - 1) as u32
    }

    /// The top-level rule list, or the child list of the rule at `path`, for the CSSOM's edits.
    /// After any edit the caller must [`Stylesheet::rebuild`].
    pub(crate) fn list_mut(&mut self, path: &[u32]) -> Option<&mut Vec<Rule>> {
        let mut list = &mut self.rules;
        for &i in path {
            list = &mut list.get_mut(i as usize)?.children;
        }
        Some(list)
    }

    /// The child list at `path` (the top level for an empty path).
    pub fn list_at(&self, path: &[u32]) -> Option<&[Rule]> {
        let mut list: &[Rule] = &self.rules;
        for &i in path {
            list = &list.get(i as usize)?.children;
        }
        Some(list)
    }

    /// The rule at `path`.
    pub fn rule_at(&self, path: &[u32]) -> Option<&Rule> {
        let (last, parent) = path.split_last()?;
        self.list_at(parent)?.get(*last as usize)
    }

    pub(crate) fn rule_at_mut(&mut self, path: &[u32]) -> Option<&mut Rule> {
        let (last, parent) = path.split_last()?;
        self.list_mut(parent)?.get_mut(*last as usize)
    }

    /// Move another sheet's pools into this one and hand back its rule tree, every span and media
    /// index shifted onto this sheet's pools.
    ///
    /// This is what makes `insertRule` and `@import` work at all. The parsed rules address a text
    /// pool that is about to be dropped, so "absorbing" them is not an optimization — reading their
    /// spans afterwards would be reading someone else's bytes.
    pub(crate) fn absorb(&mut self, other: Stylesheet) -> Vec<Rule> {
        let text_at = self.text.len() as u32;
        let decl_at = self.declarations.len() as u32;
        let custom_at = self.raw_declarations.len() as u32;
        let media_at = self.media.len() as u32;
        let base_at = self.bases.len() as u32 - 1;
        let Stylesheet { text, declarations, raw_declarations, rules, media, bases, .. } = other;

        self.text.push_str(&text);
        self.declarations.extend(declarations.into_iter().map(|mut d| {
            d.value.start += text_at;
            d.value.end += text_at;
            d
        }));
        self.raw_declarations.extend(raw_declarations.into_iter().map(|mut d| {
            d.name.start += text_at;
            d.name.end += text_at;
            d.value.start += text_at;
            d.value.end += text_at;
            d
        }));
        self.media.extend(media.into_iter().map(|m| m.shifted(media_at)));
        self.bases.extend(bases.into_iter().skip(1));

        fn shift(rule: &mut Rule, decl_at: u32, custom_at: u32, media_at: u32, base_at: u32) {
            if !rule.decls.is_empty() {
                rule.decls.start += decl_at;
                rule.decls.end += decl_at;
            } else {
                rule.decls = Span::EMPTY;
            }
            if !rule.custom.is_empty() {
                rule.custom.start += custom_at;
                rule.custom.end += custom_at;
            } else {
                rule.custom = Span::EMPTY;
            }
            match &mut rule.kind {
                RuleKind::Media { media, .. } => *media += media_at,
                RuleKind::Import(import) => {
                    if let Some((m, _)) = &mut import.media {
                        *m += media_at;
                    }
                    if import.base != 0 {
                        import.base += base_at;
                    }
                }
                _ => {}
            }
            for child in &mut rule.children {
                shift(child, decl_at, custom_at, media_at, base_at);
            }
        }
        let mut rules = rules;
        for rule in &mut rules {
            shift(rule, decl_at, custom_at, media_at, base_at);
        }
        rules
    }

    /// An imported sheet has arrived: its rules become the children of the `@import` at `path`,
    /// resolved against `url`.
    pub fn attach_import(&mut self, path: &[u32], url: &str, child: Stylesheet) -> bool {
        if !matches!(self.rule_at(path).map(|r| &r.kind), Some(RuleKind::Import(_))) {
            return false;
        }
        let base = self.push_base(url.to_string());
        let children = self.absorb(child);
        let Some(rule) = self.rule_at_mut(path) else { return false };
        if let RuleKind::Import(import) = &mut rule.kind {
            import.url = Some(url.to_string());
            import.loaded = true;
            import.base = base;
        }
        rule.children = children;
        self.rebuild();
        true
    }

    /// Is this rule live? A rule under a `@media` that does not currently match contributes
    /// nothing, but it stays in the sheet — the CSSOM can still see it, and a viewport change makes
    /// it live again without re-parsing.
    pub fn rule_applies(&self, rule: &CascadeRule, ctx: &MediaContext) -> bool {
        self.sheet_applies(ctx) && self.chain_matches(rule.media, ctx)
    }

    /// Does the sheet as a whole contribute under `ctx`: not disabled, and its own media list
    /// ([`Stylesheet::media_text`]) matches.
    pub fn sheet_applies(&self, ctx: &MediaContext) -> bool {
        !self.disabled && self.own_media.as_ref().is_none_or(|(_, list)| list.matches(ctx, &[]))
    }

    /// The sheet's own media text as it was set (`""` for none).
    pub fn media_text(&self) -> &str {
        self.own_media.as_ref().map_or("", |(text, _)| text.as_str())
    }

    /// Set the sheet's own media list from `text`. Whitespace-only text is the empty list, which
    /// matches everything -- Chrome applies `<style media="">`.
    pub fn set_media_text(&mut self, text: &str) {
        self.own_media = (!text.trim().is_empty()).then(|| (text.to_string(), MediaQueryList::parse(text, None)));
    }

    /// How many media chains the rules sit under, chain `0` (none) included: a rule's
    /// [`CascadeRule::media`] is below this.
    pub fn chain_count(&self) -> u32 {
        self.derived().chains.len() as u32
    }

    /// Does every media list in chain `chain` match?
    pub fn chain_matches(&self, chain: u32, ctx: &MediaContext) -> bool {
        match self.derived().chains.get(chain as usize) {
            Some(lists) => lists.iter().all(|&m| self.media[m as usize].matches(ctx, &self.media)),
            None => true,
        }
    }

    /// The cascade's view of the tree -- flat rules with resolved selectors, the media chains and
    /// layers they sit under, and the keyframes and font faces that apply -- is stale after this.
    ///
    /// Called after the parse and after every structural CSSOM edit. Deriving is O(rules), so it
    /// waits for the first read ([`Stylesheet::derived`]): a page that calls `insertRule` in a loop
    /// pays it once, at the next style pass, which is also when the caller's index (`PageState`'s
    /// matcher, dropped on the same edits) is rebuilt.
    pub fn rebuild(&mut self) {
        // Marks the derived view stale; the next read derives it once (every CSSOM mutation
        // -- each `insertRule` of a CSS-in-JS library -- used to re-walk the WHOLE sheet at once, so N
        // inserts cost O(N^2): 15 % of salesforce.com's load CPU was that walk's allocations,
        // measured 2026-09-29).
        self.derived = std::sync::OnceLock::new();
    }

    /// The derived view, walked from `rules` on the first read after a change.
    fn derived(&self) -> &Derived {
        self.derived.get_or_init(|| {
            let mut out = Derived {
                cascade: Vec::new(),
                chains: vec![Box::new([])],
                layers: vec![Vec::new()],
                keyframes: Vec::new(),
                font_faces: Vec::new(),
                containers: Vec::new(),
                container_chains: vec![Box::new([])],
                scopes: Vec::new(),
                properties: Vec::new(),
            };
            let ctx = Walk { media: Vec::new(), layer: Vec::new(), parent: None, base: 0, containers: Vec::new(), scope: 0, scope_top: false };
            walk(&self.rules, &ctx, &mut out);
            out
        })
    }
}

#[derive(Debug, Clone)]
struct Derived {
    cascade: Vec<CascadeRule>,
    chains: Vec<Box<[u32]>>,
    layers: Vec<LayerPath>,
    keyframes: Vec<KeyframesRule>,
    font_faces: Vec<FontFaceEntry>,
    containers: Vec<crate::style::container::ContainerCondition>,
    container_chains: Vec<Box<[u32]>>,
    scopes: Vec<ScopeInfo>,
    properties: Vec<(String, crate::style::registered::Registration)>,
}

impl Derived {
    fn container_chain(&mut self, conds: &[u32]) -> u32 {
        if let Some(i) = self.container_chains.iter().position(|c| c.as_ref() == conds) {
            return i as u32;
        }
        self.container_chains.push(conds.into());
        (self.container_chains.len() - 1) as u32
    }

    fn chain(&mut self, media: &[u32]) -> u32 {
        if let Some(i) = self.chains.iter().position(|c| c.as_ref() == media) {
            return i as u32;
        }
        self.chains.push(media.into());
        (self.chains.len() - 1) as u32
    }

    /// Declare a layer (and every prefix of it) and return its index.
    fn layer(&mut self, path: &[LayerSeg]) -> u32 {
        for n in 1..path.len() {
            self.layer(&path[..n]);
        }
        if let Some(i) = self.layers.iter().position(|l| l.as_slice() == path) {
            return i as u32;
        }
        self.layers.push(path.to_vec());
        (self.layers.len() - 1) as u32
    }
}

#[derive(Clone)]
struct Walk {
    media: Vec<u32>,
    layer: LayerPath,
    /// The enclosing style rule's RESOLVED selectors, inside one.
    parent: Option<Vec<Selector>>,
    base: u32,
    /// Indices into `Derived::containers` of every enclosing `@container`.
    containers: Vec<u32>,
    /// `0`, or one more than the innermost enclosing `@scope`'s index.
    scope: u32,
    /// A style rule here is a DIRECT child of `@scope` (not nested in another style rule), so its
    /// selectors are relative to the scoping root.
    scope_top: bool,
}

/// The selector a style rule directly inside `@scope` matches with (CSS Cascade 6 §3.5.2): one that
/// names `:scope` is kept, `&` means `:where(:scope)`, and any other is prefixed with `:where(:scope)`
/// and a descendant combinator, so it reaches only the root's descendants and adds no specificity.
/// Chrome 150: `@scope (.sc) { p { color } :scope { background-color } }` styles the `<p>` inside
/// `.sc` and `.sc` itself, and not a `<p>` outside it.
fn scoped_selector(sel: &Selector) -> Selector {
    let text = sel.to_css_string();
    if sel.has_parent_selector() {
        return match Selector::parse_list(":where(:scope)") {
            Ok(implicit) => sel.replace_parent(&implicit),
            Err(_) => sel.clone(),
        };
    }
    if text.contains(":scope") {
        return sel.clone();
    }
    Selector::parse_list(&format!(":where(:scope) {text}"))
        .ok()
        .and_then(|mut v| v.pop())
        .unwrap_or_else(|| sel.clone())
}

/// Split a dotted layer name into named segments.
fn named_segments(name: &str) -> impl Iterator<Item = LayerSeg> + '_ {
    name.split('.').map(|s| LayerSeg::Named(s.trim().to_string()))
}

fn walk(rules: &[Rule], ctx: &Walk, out: &mut Derived) {
    for rule in rules {
        match &rule.kind {
            RuleKind::Style | RuleKind::NestedDeclarations => {
                let selectors = if rule.kind == RuleKind::NestedDeclarations {
                    match &ctx.parent {
                        Some(p) => p.clone(),
                        None => continue,
                    }
                } else {
                    match &ctx.parent {
                        Some(parents) => rule.selectors.iter().map(|s| s.replace_parent(parents)).collect(),
                        None => rule.selectors.clone(),
                    }
                };
                let selectors: Vec<Selector> = if ctx.scope_top && ctx.parent.is_none() {
                    selectors.iter().map(scoped_selector).collect()
                } else {
                    selectors
                };
                let media = out.chain(&ctx.media);
                let layer = out.layer(&ctx.layer);
                let container = out.container_chain(&ctx.containers);
                out.cascade.push(CascadeRule {
                    selectors: selectors.clone(),
                    decls: rule.decls,
                    custom: rule.custom,
                    media,
                    layer,
                    base: ctx.base,
                    container,
                    scope: ctx.scope,
                });
                if !rule.children.is_empty() {
                    let inner = Walk { parent: Some(selectors), scope_top: false, ..ctx.clone() };
                    walk(&rule.children, &inner, out);
                }
            }
            RuleKind::Container { name, query } => {
                let text = match name {
                    Some(n) => format!("{n} {query}"),
                    None => query.clone(),
                };
                let Some(cond) = crate::style::container::ContainerCondition::parse(&text) else { continue };
                out.containers.push(cond);
                let mut inner = ctx.clone();
                inner.containers.push((out.containers.len() - 1) as u32);
                walk(&rule.children, &inner, out);
            }
            RuleKind::Scope { start, end } => {
                let parse = |t: &Option<String>| t.as_deref().map(Selector::parse_list);
                let start = match parse(start) {
                    Some(Ok(s)) => Some(s),
                    Some(Err(_)) => continue,
                    None => None,
                };
                let end = match parse(end) {
                    Some(Ok(e)) => e,
                    Some(Err(_)) => continue,
                    None => Vec::new(),
                };
                out.scopes.push(ScopeInfo { start, end });
                let inner = Walk { scope: out.scopes.len() as u32, scope_top: true, parent: None, ..ctx.clone() };
                walk(&rule.children, &inner, out);
            }
            RuleKind::Property { name, block } => {
                if let Some(reg) = crate::style::registered::Registration::from_block(block) {
                    out.properties.retain(|(n, _)| n != name);
                    out.properties.push((name.clone(), reg));
                }
            }
            RuleKind::Page { .. } | RuleKind::StartingStyle => {}
            RuleKind::Media { media, .. } => {
                let mut inner = ctx.clone();
                inner.media.push(*media);
                walk(&rule.children, &inner, out);
            }
            RuleKind::Supports { matches, .. } => {
                if *matches {
                    walk(&rule.children, ctx, out);
                }
            }
            RuleKind::LayerBlock { name, anon } => {
                let mut inner = ctx.clone();
                match name {
                    Some(n) => inner.layer.extend(named_segments(n)),
                    None => inner.layer.push(LayerSeg::Anon(*anon)),
                }
                out.layer(&inner.layer);
                walk(&rule.children, &inner, out);
            }
            RuleKind::LayerStatement { names } => {
                for n in names {
                    let mut path = ctx.layer.clone();
                    path.extend(named_segments(n));
                    out.layer(&path);
                }
            }
            RuleKind::Import(import) => {
                let mut inner = ctx.clone();
                match import.layer.as_deref() {
                    Some("") => inner.layer.push(LayerSeg::Anon(import.anon)),
                    Some(n) => inner.layer.extend(named_segments(n)),
                    None => {}
                }
                if import.layer.is_some() {
                    out.layer(&inner.layer);
                }
                if let Some((m, _)) = &import.media {
                    inner.media.push(*m);
                }
                inner.base = import.base;
                inner.scope_top = false;
                walk(&rule.children, &inner, out);
            }
            RuleKind::Keyframes { name } => {
                let blocks = rule
                    .children
                    .iter()
                    .filter_map(|k| match &k.kind {
                        RuleKind::Keyframe { offsets } => Some(KeyframeBlock { offsets: offsets.clone(), decls: k.decls }),
                        _ => None,
                    })
                    .collect();
                // A later rule with the same name REPLACES an earlier one rather than joining it --
                // what Chrome does with two `@keyframes spin` in one sheet.
                out.keyframes.retain(|k| &k.name != name);
                out.keyframes.push(KeyframesRule { name: name.clone(), blocks });
            }
            RuleKind::FontFace { block } => {
                let media = out.chain(&ctx.media);
                out.font_faces.push(FontFaceEntry { block: block.clone(), media, base: ctx.base });
            }
            RuleKind::PositionTry { .. } | RuleKind::CounterStyle { .. } | RuleKind::Keyframe { .. } => {}
        }
    }
}

/// Which selector, of which rule, in which sheet.
///
/// Plain indices rather than references, so the index can outlive any particular borrow of the
/// sheets and be rebuilt without touching them. `selector` is what lets a rule stay one block while
/// the cascade still gets per-selector specificity: `h1, .lede` puts two entries in the index,
/// under two different keys, pointing at the same rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleRef {
    pub sheet: u32,
    pub rule: u32,
    pub selector: u32,
}

/// The keys an element offers up when asking which rules might apply to it.
#[derive(Debug, Clone, Copy)]
pub struct ElementKeys<'a> {
    pub local_name: &'a str,
    pub id: Option<&'a str>,
    pub classes: &'a [&'a str],
    /// Its attributes, whose names key the attribute buckets.
    pub attrs: &'a [(String, String)],
    /// The element is in a QUIRKS document, where class and id selectors fold ASCII case:
    /// the buckets, keyed as the selectors were written, are then searched without case.
    pub quirks: bool,
}

/// One entry of a [`RuleIndex`] bucket: the rule, and the selector's ancestor hashes
/// ([`Selector::ancestor_hashes`]) beside it, so the ancestor filter rejects it without the selector
/// being looked at (Blink's RuleData). www.wolfram.com has 4,819 rules like
/// `.heirs-width-1-2__2 > *`, every one a candidate for every element: each used to cost a media check
/// and a `MatchingContext` before the filter said no.
#[derive(Debug, Clone, Copy)]
struct Indexed {
    at: RuleRef,
    /// The index's number for the rule's sheet and media chain, which the caller answers its media
    /// questions by (the matcher's `MediaAnswers`). `0` for an index that numbers none.
    chain: u32,
    hashes: [u32; 3],
    folded: [u32; 3],
}

/// Emit each entry of `bucket` that `bloom` cannot rule out, with its media chain number.
#[inline(always)]
fn emit(bucket: &[Indexed], bloom: Option<&AncestorBloom>, f: &mut impl FnMut(RuleRef, u32)) {
    match bloom {
        None => bucket.iter().for_each(|e| f(e.at, e.chain)),
        Some(b) if b.folded() => bucket.iter().filter(|e| b.may_match(&e.folded)).for_each(|e| f(e.at, e.chain)),
        Some(b) => bucket.iter().filter(|e| b.may_match(&e.hashes)).for_each(|e| f(e.at, e.chain)),
    }
}

/// Rules bucketed by the key of their rightmost compound.
///
/// Without this, styling a page is `nodes × rules` selector tests — 15 million for a 5,000-node
/// page with 3,000 rules, each one a tree walk. With it, an element only ever sees the rules that
/// could plausibly match it: the ones keyed on its tag, its id, one of its classes, and the
/// universal residue. Every other rule is skipped without being looked at.
///
/// This is not an optimization to add later. It is the difference between styling a page and
/// hanging on one.
#[derive(Debug, Default)]
pub struct RuleIndex {
    by_id: HashMap<String, Vec<Indexed>>,
    by_class: HashMap<String, Vec<Indexed>>,
    /// By the lowercase name of an attribute the rightmost compound requires ([`SelectorBucket::Attribute`]).
    by_attribute: HashMap<String, Vec<Indexed>>,
    by_local_name: HashMap<String, Vec<Indexed>>,
    /// `*`, `:hover`, `.a > *` — nothing about the element narrows them down.
    universal: Vec<Indexed>,
    /// The id and class buckets again under their ASCII-lowercase keys, for a QUIRKS document, which
    /// folds class and id case. Made at the first quirks lookup; an insert drops it.
    folded: std::sync::OnceLock<FoldedKeys>,
}

/// [`RuleIndex`]'s id and class buckets merged under their ASCII-lowercase keys: `.Nav` and `.nav`
/// share one.
#[derive(Debug, Default)]
struct FoldedKeys {
    by_id: HashMap<String, Vec<Indexed>>,
    by_class: HashMap<String, Vec<Indexed>>,
}

impl FoldedKeys {
    fn of(index: &RuleIndex) -> FoldedKeys {
        let fold = |buckets: &HashMap<String, Vec<Indexed>>| {
            let mut out: HashMap<String, Vec<Indexed>> = HashMap::with_capacity(buckets.len());
            for (key, rules) in buckets {
                out.entry(key.to_ascii_lowercase()).or_default().extend_from_slice(rules);
            }
            out
        };
        FoldedKeys { by_id: fold(&index.by_id), by_class: fold(&index.by_class) }
    }
}

/// `key` in ASCII lowercase, borrowed when it has nothing to fold.
fn folded(key: &str) -> std::borrow::Cow<'_, str> {
    if key.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(key.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(key)
    }
}

impl RuleIndex {
    pub fn insert(&mut self, selector: &Selector, rule: RuleRef, chain: u32) {
        self.insert_under(selector.bucket(), selector, rule, chain);
    }

    /// [`RuleIndex::insert`], a selector keyed on a class going under the one of its rightmost compound's classes
    /// that `shared` counts the fewest selectors keying on -- not the first. airbnb.com's atomic sheet writes
    /// `.atm_….dir.dir-ltr`, and under the first class the `dir` bucket held 7,189 rules, every one of them a
    /// candidate for every element (every element there carries `dir`): about 160 µs of matching per element styled,
    /// the page taking 30 s to render what Chrome renders in 2 (2026-10-22). Any of the classes is a correct key, as
    /// the element must carry all of them; the rarest is the one that hands the fewest elements the rule.
    pub fn insert_rarest(&mut self, selector: &Selector, rule: RuleRef, chain: u32, shared: &HashMap<String, u32>) {
        let bucket = match selector.bucket() {
            SelectorBucket::Class(first) => SelectorBucket::Class(
                selector.key_classes().into_iter().min_by_key(|c| shared.get(c).copied().unwrap_or(0)).unwrap_or(first),
            ),
            other => other,
        };
        self.insert_under(bucket, selector, rule, chain);
    }

    fn insert_under(&mut self, bucket: SelectorBucket, selector: &Selector, rule: RuleRef, chain: u32) {
        self.folded = std::sync::OnceLock::new();
        let (hashes, folded) = selector.ancestor_hashes();
        let entry = Indexed { at: rule, chain, hashes, folded };
        match bucket {
            SelectorBucket::Id(k) => self.by_id.entry(k).or_default().push(entry),
            SelectorBucket::Class(k) => self.by_class.entry(k).or_default().push(entry),
            SelectorBucket::Attribute(k) => self.by_attribute.entry(k).or_default().push(entry),
            SelectorBucket::LocalName(k) => self.by_local_name.entry(k).or_default().push(entry),
            SelectorBucket::Universal => self.universal.push(entry),
        }
    }

    /// Every rule that could match an element with these keys.
    ///
    /// A callback rather than an iterator so nothing allocates per element — this runs once for
    /// every node in the document, and the caller already owns a scratch buffer to sort in.
    /// Candidates arrive in no particular order; the cascade sorts them itself, since matching
    /// order is not cascade order.
    ///
    /// `bloom`, the element's ancestor filter, rejects a rule whose ancestor parts no ancestor has, by
    /// the test `match_mode` would start with -- so only when it is the filter `match_mode` would ask:
    /// folded exactly when the document is in quirks mode.
    ///
    /// Always inlined: out of line (as the quirks branch left it) its candidate pushes cost
    /// www.wolfram.com 3 % more instructions.
    #[inline(always)]
    /// The rules keyed on the type selector `local_name` matches. A selector is bucketed under its LOWERCASED name
    /// (`Selector::bucket`), so an element whose name has capitals -- SVG's `foreignObject`, `clipPath`,
    /// `linearGradient` -- asks under its lowercased one; asked as written, every type selector of such a name was
    /// never tried (`svg|foreignObject { overflow: hidden }`, `clipPath { ... }`), where Chrome matches it.
    fn local_name_bucket(&self, local_name: &str) -> Option<&Vec<Indexed>> {
        if local_name.bytes().any(|b| b.is_ascii_uppercase()) {
            self.by_local_name.get(local_name.to_ascii_lowercase().as_str())
        } else {
            self.by_local_name.get(local_name)
        }
    }

    pub fn for_each_candidate(&self, keys: ElementKeys<'_>, bloom: Option<&AncestorBloom>, mut f: impl FnMut(RuleRef, u32)) {
        let bloom = bloom.filter(|b| b.folded() == keys.quirks);
        if keys.quirks {
            return self.for_each_quirks_candidate(keys, bloom, f);
        }
        if let Some(id) = keys.id {
            if let Some(v) = self.by_id.get(id) {
                emit(v, bloom, &mut f);
            }
        }
        for class in keys.classes {
            if let Some(v) = self.by_class.get(*class) {
                emit(v, bloom, &mut f);
            }
        }
        self.for_each_attribute_candidate(keys, bloom, &mut f);
        if let Some(v) = self.local_name_bucket(keys.local_name) {
            emit(v, bloom, &mut f);
        }
        emit(&self.universal, bloom, &mut f);
    }

    /// The attribute buckets of `keys.attrs`' names, in ASCII lowercase: a selector `[viewBox]` on an SVG
    /// element and `[VIEWBOX]` both name the bucket `viewbox`. An element's attribute names are distinct,
    /// so no bucket comes twice.
    #[inline(always)]
    fn for_each_attribute_candidate(&self, keys: ElementKeys<'_>, bloom: Option<&AncestorBloom>, f: &mut impl FnMut(RuleRef, u32)) {
        if self.by_attribute.is_empty() {
            return;
        }
        for (name, _) in keys.attrs {
            if let Some(v) = self.by_attribute.get(folded(name).as_ref()) {
                emit(v, bloom, f);
            }
        }
    }

    /// [`RuleIndex::for_each_candidate`] for an element of a QUIRKS document, looked up under the folded
    /// keys. A pass over every id and class bucket, which this replaced, cost atlassian.com (a
    /// quirks page with thousands of class buckets) about 20 G instructions per style pass. Each bucket
    /// still comes once, as the pass gave it, when two of the element's classes fold alike. Out of line,
    /// so the no-quirks path stays what it was.
    #[inline(never)]
    fn for_each_quirks_candidate(&self, keys: ElementKeys<'_>, bloom: Option<&AncestorBloom>, mut f: impl FnMut(RuleRef, u32)) {
        let index = self.folded.get_or_init(|| FoldedKeys::of(self));
        if let Some(v) = keys.id.and_then(|id| index.by_id.get(folded(id).as_ref())) {
            emit(v, bloom, &mut f);
        }
        for (i, class) in keys.classes.iter().enumerate() {
            if keys.classes[..i].iter().any(|c| c.eq_ignore_ascii_case(class)) {
                continue;
            }
            if let Some(v) = index.by_class.get(folded(class).as_ref()) {
                emit(v, bloom, &mut f);
            }
        }
        self.for_each_attribute_candidate(keys, bloom, &mut f);
        if let Some(v) = self.local_name_bucket(keys.local_name) {
            emit(v, bloom, &mut f);
        }
        emit(&self.universal, bloom, &mut f);
    }

    /// How many rules are indexed, for tests and for reporting.
    pub fn len(&self) -> usize {
        self.by_id.values().map(Vec::len).sum::<usize>()
            + self.by_class.values().map(Vec::len).sum::<usize>()
            + self.by_attribute.values().map(Vec::len).sum::<usize>()
            + self.by_local_name.values().map(Vec::len).sum::<usize>()
            + self.universal.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(text: &str) -> Vec<Selector> {
        Selector::parse_list(text).unwrap()
    }

    fn push(sheet: &mut Stylesheet, text: &str, decls: Span) {
        let mut rules = sheet.rules().to_vec();
        rules.push(Rule { selectors: sel(text), decls, ..Rule::new(RuleKind::Style) });
        sheet.set_rules(rules);
    }

    // CSS Cascade 4 §6.1. The reversal is the part worth a test: `!important` does not add weight
    // on top of the origin order, it turns that order upside down, so a user's important rule beats
    // the page and the UA's important rule beats everyone.
    #[test]
    fn important_reverses_the_origin_order_rather_than_adding_to_it() {
        use Origin::*;
        let ladder = [
            Precedence::of(UserAgent, false),
            Precedence::of(User, false),
            Precedence::of(Author, false),
            Precedence::of(Author, true),
            Precedence::of(User, true),
            Precedence::of(UserAgent, true),
        ];
        for pair in ladder.windows(2) {
            assert!(pair[0] < pair[1], "{:?} should lose to {:?}", pair[0], pair[1]);
        }
        // The two ends: normally the page wins, but a user's important rule takes it back.
        assert!(Precedence::of(Author, false) > Precedence::of(User, false));
        assert!(Precedence::of(User, true) > Precedence::of(Author, true));
    }

    /// The two rungs the declarative animations sit on, and they are not adjacent to each other.
    /// Both were measured on Chrome 150 rather than read off the spec — see `Precedence::animation`.
    #[test]
    fn an_animation_beats_author_normal_and_a_transition_beats_everything() {
        use Origin::*;
        let ladder = [
            Precedence::of(UserAgent, false),
            Precedence::of(User, false),
            Precedence::of(Author, false),
            Precedence::animation(),
            Precedence::of(Author, true),
            Precedence::of(User, true),
            Precedence::of(UserAgent, true),
            Precedence::transition(),
        ];
        for pair in ladder.windows(2) {
            assert!(pair[0] < pair[1], "{:?} should lose to {:?}", pair[0], pair[1]);
        }
    }

    // A selector list is one rule -- that is what `cssRules` says -- but its selectors still
    // specify separately, which is what the index entries are for.
    #[test]
    fn a_selector_list_is_one_rule_whose_selectors_specify_separately() {
        let mut sheet = Stylesheet::new(Origin::Author, "h1, .lede { color: red }".into());
        let decls = sheet.push_declarations([Declaration {
            property: PropertyId::Color,
            value: Span::of(19..22),
            important: false,
            implicit: false,
        }]);
        push(&mut sheet, "h1, .lede", decls);

        assert_eq!(sheet.rules().len(), 1);
        let rule = &sheet.rules()[0];
        assert_eq!(rule.selector_text(), "h1, .lede");
        assert_ne!(rule.selectors[0].specificity(), rule.selectors[1].specificity());
        assert_eq!(sheet.value_of(&sheet.declarations_of(rule)[0]), "red");
    }

    #[test]
    fn spans_address_the_sheets_own_text() {
        let text = "a { color: blue; margin-top: 4px }".to_string();
        let mut sheet = Stylesheet::new(Origin::Author, text);
        let decls = sheet.push_declarations([
            Declaration { property: PropertyId::Color, value: Span::of(11..15), important: false, implicit: false },
            Declaration { property: PropertyId::MarginTop, value: Span::of(29..32), important: false, implicit: false },
        ]);
        push(&mut sheet, "a", decls);
        let rule = &sheet.rules()[0];
        let got: Vec<&str> = sheet.declarations_of(rule).iter().map(|d| sheet.value_of(d)).collect();
        assert_eq!(got, vec!["blue", "4px"]);
    }

    // The point of the index: an element sees the rules that could match it and no others.
    #[test]
    fn an_element_only_ever_sees_its_own_buckets() {
        let mut index = RuleIndex::default();
        let mut add = |text: &str, n: u32| {
            index.insert(&sel(text)[0], RuleRef { sheet: 0, rule: n, selector: 0 }, 0);
        };
        add("p", 0);
        add("#main", 1);
        add(".lede", 2);
        add("span", 3);
        add("*", 4);
        add(".other", 5);

        let mut seen = Vec::new();
        index.for_each_candidate(
            ElementKeys { local_name: "p", id: Some("main"), classes: &["lede"], attrs: &[], quirks: false },
            None,
            |r, _| seen.push(r.rule),
        );
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2, 4], "the span and the unrelated class are never looked at");
        assert_eq!(index.len(), 6);
    }

    // A compound of several classes goes under the one the fewest selectors key on: airbnb.com's atomic
    // `.atm_….dir.dir-ltr` rules under `dir` (on every element) made 7,189 candidates of every element.
    #[test]
    fn a_compound_of_classes_is_indexed_under_its_rarest_class() {
        let mut index = RuleIndex::default();
        let shared: HashMap<String, u32> = [("dir", 3), ("a", 1), ("b", 1)].into_iter().map(|(k, n)| (k.to_string(), n)).collect();
        for (i, s) in [".dir.a", ".dir.b", ".dir"].iter().enumerate() {
            index.insert_rarest(&sel(s)[0], RuleRef { sheet: 0, rule: i as u32, selector: 0 }, 0, &shared);
        }
        let seen = |classes: &[&str]| {
            let mut out = Vec::new();
            index.for_each_candidate(ElementKeys { local_name: "div", id: None, classes, attrs: &[], quirks: false }, None, |r, _| {
                out.push(r.rule)
            });
            out.sort_unstable();
            out
        };
        assert_eq!(seen(&["dir"]), vec![2], "`.dir.a` and `.dir.b` wait under `a` and `b`");
        assert_eq!(seen(&["dir", "a"]), vec![0, 2]);
        assert_eq!(seen(&["b", "dir"]), vec![1, 2]);
    }

    // An element with no id and no classes is the common case, and it must not be paying for the
    // id and class tables at all.
    #[test]
    fn a_bare_element_gets_only_its_tag_and_the_universal_residue() {
        let mut index = RuleIndex::default();
        for (i, s) in ["div", "#a", ".b", "[hidden]"].iter().enumerate() {
            index.insert(&sel(s)[0], RuleRef { sheet: 0, rule: i as u32, selector: 0 }, 0);
        }
        let mut seen = Vec::new();
        index.for_each_candidate(
            ElementKeys { local_name: "div", id: None, classes: &[], attrs: &[], quirks: false },
            None,
            |r, _| seen.push(r.rule),
        );
        seen.sort_unstable();
        assert_eq!(seen, vec![0], "`[hidden]` waits in its attribute's bucket");
    }

    // An attribute-only rightmost compound is bucketed by the attribute's lowercase name, and an
    // element sees it only through an attribute of that name in any case -- `[data-aos]` is not tried
    // against the 10,000 elements of www.jlcpcb.com that have no `data-aos`.
    #[test]
    fn an_attribute_rule_is_seen_only_by_elements_with_that_attribute() {
        let mut index = RuleIndex::default();
        for (i, s) in ["[data-aos]", "[data-aos][data-aos-delay='50']", "[viewBox]", "a[href]", "[TYPE=button]", ".c[title]", "*"]
            .iter()
            .enumerate()
        {
            index.insert(&sel(s)[0], RuleRef { sheet: 0, rule: i as u32, selector: 0 }, 0);
        }
        let seen = |local_name: &str, classes: &[&str], attrs: &[(&str, &str)]| {
            let attrs: Vec<(String, String)> = attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let mut out = Vec::new();
            index.for_each_candidate(ElementKeys { local_name, id: None, classes, attrs: &attrs, quirks: false }, None, |r, _| out.push(r.rule));
            out.sort_unstable();
            out
        };
        assert_eq!(seen("div", &[], &[]), vec![6]);
        assert_eq!(seen("div", &[], &[("data-aos", "fade")]), vec![0, 1, 6]);
        assert_eq!(seen("svg", &[], &[("viewBox", "0 0 1 1")]), vec![2, 6], "an SVG name keeps its case; the bucket does not");
        assert_eq!(seen("a", &[], &[("href", "/")]), vec![3, 6], "the attribute before the tag, as Blink keys it");
        assert_eq!(seen("button", &[], &[("type", "button")]), vec![4, 6]);
        assert_eq!(seen("p", &["c"], &[("title", "t")]), vec![5, 6], "a class before an attribute");
        assert_eq!(seen("p", &[], &[("title", "t")]), vec![6]);
    }

    // A quirks document folds class and id case: every bucket whose key folds to one of the
    // element's keys, each once however many of the element's classes fold to it, and nothing else.
    #[test]
    fn a_quirks_element_sees_each_bucket_its_keys_fold_to_once() {
        let mut index = RuleIndex::default();
        for (i, s) in [".Nav", ".nav", ".x .NAV", "#Main", "#main", ".other", "p", "*"].iter().enumerate() {
            index.insert(&sel(s)[0], RuleRef { sheet: 0, rule: i as u32, selector: 0 }, 0);
        }
        fn seen(index: &RuleIndex, id: Option<&str>, classes: &[&str], quirks: bool) -> Vec<u32> {
            let mut out = Vec::new();
            index.for_each_candidate(ElementKeys { local_name: "p", id, classes, attrs: &[], quirks }, None, |r, _| out.push(r.rule));
            out.sort_unstable();
            out
        }
        assert_eq!(seen(&index, Some("MAIN"), &["nAv", "NaV", "nav"], true), vec![0, 1, 2, 3, 4, 6, 7]);
        assert_eq!(seen(&index, None, &["OTHER"], true), vec![5, 6, 7]);
        assert_eq!(seen(&index, Some("MAIN"), &["nAv"], false), vec![6, 7], "no-quirks keys keep their case");
        // An insert after a quirks lookup is seen by the next one.
        index.insert(&sel(".NaV")[0], RuleRef { sheet: 0, rule: 8, selector: 0 }, 0);
        assert_eq!(seen(&index, None, &["nav"], true), vec![0, 1, 2, 6, 7, 8]);
    }

    #[test]
    fn candidates_carry_which_selector_of_which_rule_of_which_sheet() {
        let mut index = RuleIndex::default();
        index.insert(&sel("p")[0], RuleRef { sheet: 7, rule: 3, selector: 2 }, 0);
        let mut seen = Vec::new();
        index.for_each_candidate(
            ElementKeys { local_name: "p", id: None, classes: &[], attrs: &[], quirks: false },
            None,
            |r, _| seen.push(r),
        );
        assert_eq!(seen, vec![RuleRef { sheet: 7, rule: 3, selector: 2 }]);
    }
}
