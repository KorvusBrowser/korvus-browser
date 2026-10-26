//! The cascade: many declarations in, one value per property out.
//!
//! # One comparison, not a decision tree
//!
//! CSS Cascade 4 orders declarations by origin, then by `!important`, then by specificity, then by
//! document order. Written as nested conditionals that is easy to get subtly wrong. Written as a
//! single key it cannot be: every declaration gets a
//! `(precedence, specificity, sheet, rule, position)` tuple, higher wins, and the whole cascade is
//! one `>` per declaration. [`Precedence`] already folds origin and `!important` into one rung,
//! because `!important` reverses the origin order rather than adding to it.
//!
//! No sorting: the winner per property is a running maximum, found through a stack array with one
//! slot per property. A page's element has a handful of matched rules and the array is [`COUNT`]
//! entries — sorting the declarations first would cost an allocation per element to reach the same
//! answer. The slot holds an INDEX into the short list of properties that have a winner, not the
//! winner itself; see [`Winners`] for why that distinction is most of the cost of this file.
//!
//! # Inheritance is the second pass, and it is where the sharing happens
//!
//! Once the winners are known, an inherited property with no winner takes the parent's value. If
//! *nothing* inherited was set on this element, its inherited half is the parent's — by identity,
//! with no hashing and no copy. That is the common case (text wrappers, elements no rule mentions)
//! and it is what makes the arena small.

use crate::style::computed::{InheritedBasis, PropertySet, StyleArena, StyleKey};
use crate::style::logical::{is_logical, to_physical, Orientation};
use crate::style::variables::substitute;
use crate::style::matching::MatchedRule;
use crate::style::property::{is_floated, PropertyId, COUNT};
use crate::style::resolve::{self, px_of};
use crate::style::rules::{Origin, Precedence, RawDeclaration, Span, Stylesheet};
use std::borrow::Cow;
use crate::value::{Color, Rgba};

/// How strongly one declaration is asserted, as the cascade compares it. Higher wins.
///
/// The fields are in comparison order, which is what makes the derived `Ord` the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CascadeKey {
    precedence: Precedence,
    /// The shadow-tree context (CSS Cascade 5 §6.1): among declarations of one origin
    /// and importance, a normal one from the OUTER tree wins and an `!important` one from the INNER
    /// tree does -- so a document's `x-card { padding-left: 22px }` beats the element's own
    /// `:host { padding-left: 21px }` even though `:host` is the more specific, and the order flips
    /// under `!important`. `u32::MAX - context` for a normal declaration, `context` for an important
    /// one; see `MatchedRule::context`. It comes before the style attribute, which is the element's
    /// own tree's.
    context: u32,
    /// An element's own `style` attribute. CSS Cascade 5 §6.1 compares element-attached styles
    /// BEFORE layers: an inline declaration beats every layered and unlayered rule of its rung.
    attached: bool,
    /// The cascade layer: the document-wide rank for a normal declaration, and its
    /// reverse for an `!important` one -- an earlier layer's `!important` beats a later layer's, and
    /// both beat unlayered `!important`. Measured on Chrome 150:
    ///
    /// ```text
    /// @layer a { .x { color: red } }  .x { color: blue }                         blue
    /// @layer a { .x { color: red !important } }  .x { color: blue !important }   red
    /// @layer a, b; @layer b { .x { color: red } } @layer a { #x { color: blue } } red
    /// ```
    layer: u32,
    /// `@scope` proximity: a rule whose scoping root is fewer generations up wins,
    /// before specificity is looked at. `0` for a rule in no scope. See `MatchedRule::proximity`.
    proximity: u32,
    specificity: u32,
    /// Document order: which sheet, which rule in it, and where the declaration stands in that rule's
    /// block ([`typed_at`], [`raw_at`]).
    order: (u32, u32, (u32, u32)),
}

/// The element's own `style="…"` attribute, as a sheet with no rules plus the span of its
/// declarations.
pub struct InlineStyle<'a> {
    pub sheet: &'a Stylesheet,
    pub decls: Span,
    /// How many of the block's leading declarations are the element's PRESENTATIONAL HINTS
    /// (`style::hints`), which cascade below every author rule; the rest are its `style` attribute.
    pub hints: u32,
}

/// What the cascade needs to know about the document, as opposed to about the element.
#[derive(Debug, Clone, Copy)]
pub struct ComputeContext {
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// The root element's computed `font-size`, in pixels — what `rem` means anywhere in the
    /// document. Supplied rather than looked up because the root is computed like any other
    /// element, and by the time anything else resolves it is already known.
    pub root_font_size: f32,
    /// The element is in a quirks-mode document: see [`quirky_value`].
    pub quirks: bool,
    /// The query container's content-box size, for `cq*` lengths. `None`: the viewport.
    pub container: Option<(f32, f32)>,
    /// The minimum LOGICAL font size of the worn browser's UI locale (`prefs::kWebKitMinimumLogicalFontSize`:
    /// 10 for Korean, 6 otherwise) -- see [`resolve::FontSizeState::computed`].
    pub min_logical_font_size: f32,
}

/// Chrome's `IDS_MINIMUM_LOGICAL_FONT_SIZE` where a locale does not set its own.
pub const DEFAULT_MIN_LOGICAL_FONT_SIZE: f32 = 6.0;

impl ComputeContext {
    /// The worn device's minimum logical font size: `theme.min_logical_font_size`, Chrome's default for none.
    pub fn min_logical_of(theme: &spec::ThemeProfile) -> f32 {
        if theme.min_logical_font_size > 0.0 {
            theme.min_logical_font_size
        } else {
            DEFAULT_MIN_LOGICAL_FONT_SIZE
        }
    }
}

impl Default for ComputeContext {
    fn default() -> Self {
        ComputeContext {
            viewport_width: 1024.0,
            viewport_height: 768.0,
            root_font_size: 16.0,
            quirks: false,
            container: None,
            min_logical_font_size: DEFAULT_MIN_LOGICAL_FONT_SIZE,
        }
    }
}

/// What `rem` means under the root element whose inherited half is `set`: its SPECIFIED font size
/// (Blink's `root_style->SpecifiedFontSize()`), else its computed one.
pub fn rem_basis(set: &crate::style::computed::PropertySet) -> Option<f32> {
    set.custom(resolve::FontSizeState::KEY)
        .and_then(resolve::FontSizeState::decode)
        .map(|s| s.specified)
        .or_else(|| set.get(PropertyId::FontSize).and_then(px_of))
}

/// The two value quirks of a quirks-mode document (the Quirks Mode Standard §3.2, §3.3): a bare
/// number is a length in px on the properties the unitless-length quirk names, and a hex digit
/// string without `#` is a colour on the ones the hashless-hex-colour quirk names. Chrome 150, no
/// doctype: `width: 100; height: 20` is 100x20 and `color: ff0000` is `rgb(255, 0, 0)`; with a
/// doctype the same declarations are dropped (764x0, `rgb(0, 0, 0)`).
pub fn quirky_value(property: PropertyId, specified: &str) -> Option<String> {
    use PropertyId::*;
    let t = specified.trim();
    let unitless = matches!(
        property,
        BackgroundPositionX | BackgroundPositionY | BorderBottomWidth | BorderLeftWidth | BorderRightWidth | BorderTopWidth
            | Bottom | FontSize | Height | Left | LetterSpacing | MarginBottom | MarginLeft | MarginRight | MarginTop
            | MaxHeight | MaxWidth | MinHeight | MinWidth | PaddingBottom | PaddingLeft | PaddingRight | PaddingTop
            | Right | TextIndent | Top | VerticalAlign | Width | WordSpacing
    );
    if unitless && !t.is_empty() && t.parse::<f32>().is_ok() && t != "0" {
        return Some(format!("{t}px"));
    }
    let hashless = matches!(
        property,
        BackgroundColor | BorderBottomColor | BorderLeftColor | BorderRightColor | BorderTopColor | Color
    );
    if hashless && matches!(t.len(), 3 | 6) && t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(format!("#{t}"));
    }
    None
}

/// Which formatting context an element's **parent** puts it in — the half of blockification that
/// the element's own declarations cannot see.
///
/// [`blockify`] answers two questions at once and only one of them is local. `position: absolute`
/// and `float` are the element's own properties, so the cascade reads them off the set it is
/// building. Being a **flex or grid item** is not: it is decided entirely by the parent's computed
/// `display`, and a `<span>` with no declarations at all computes `block` under a `display: flex`
/// parent and `inline` under a `display: block` one. That fact has to be carried down the tree.
///
/// # Why it is threaded and not read off `parent`
///
/// One level is not enough, because **`display: contents` forwards it**. Measured on Chrome 150:
/// in `<div style=display:flex><div style=display:contents><span>` the `<span>` computes `block` —
/// the `contents` box generates no box of its own, so its children are the flex container's items —
/// and two nested `contents` boxes forward it just the same. The `contents` element's own computed
/// `display` is still `contents`, so asking only the parent's style would answer "not a flex
/// container" and get the grandchild wrong. `display: none` is the other direction and STOPS it:
/// under `<div style=display:flex><div style=display:none><span>` the `<span>` computes `inline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParentFormatting {
    /// Blocks and inlines — everything that is not a flex or grid container. The root is this too:
    /// the document element has no parent to be an item of.
    #[default]
    Flow,
    /// A flex or grid container, so this element is an ITEM and is blockified.
    FlexOrGrid,
}

impl ParentFormatting {
    /// What the CHILDREN of an element see, given the element's computed style and what it saw.
    ///
    /// `own` is this element's own [`ParentFormatting`], and it is read for exactly one display
    /// value — `contents`, which forwards. See the type's documentation for the measurements.
    pub fn of_children(arena: &StyleArena, key: StyleKey, own: ParentFormatting) -> ParentFormatting {
        match arena.value(key, PropertyId::Display) {
            "flex" | "inline-flex" | "grid" | "inline-grid" => ParentFormatting::FlexOrGrid,
            // A legacy box does NOT blockify its children (Chromium 152's `ComputedStyle::BlockifiesChildren`
            // dropped `IsDisplayWebkitBox`; Chromium 150 blockified them as a flexbox's): a `<span>` in it computes
            // `inline` and is no flex item (`min-width: auto` reads `0px`). It is still laid out as a flexbox,
            // its runs of inline content wrapped in anonymous items (`layout/tree.rs`, `BoxTree::add_children`).
            "-webkit-box" | "-webkit-inline-box" => ParentFormatting::Flow,
            // No box of its own, so its children belong to whatever context it was in.
            "contents" => own,
            _ => ParentFormatting::Flow,
        }
    }
}

/// How Blink lays out `display: -webkit-box` / `-webkit-inline-box`.
///
/// Two ways, both read off reference/chromium and measured on Chrome 150 (`tools/realweb/seed/webkitbox`):
/// with `-webkit-box-orient: vertical` and a `-webkit-line-clamp`, Blink's style adjuster makes the box
/// a `flow-root` (`-webkit-inline-box`: `inline-block`) whose lines are clamped to that number
/// (`style_adjuster.cc`, `ComputedStyle::LineClamp`) -- the line-clamp idiom, 40 px for two 20 px lines
/// where this engine showed every line; otherwise it is a flexbox (`layout_object.cc`: a
/// `LayoutFlexibleBox`) driven by the `-webkit-box-*` properties, whose block-level children and runs of inline
/// content are its items (Chrome 152 no longer blockifies them: see [`ParentFormatting::of_children`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyBox {
    /// Not a legacy box.
    No,
    /// A flexbox: `-webkit-box-orient`/`-direction` are its direction, `-pack` and `-align` its
    /// alignment, each item's `-webkit-box-flex` its grow and shrink.
    Flex,
    /// A block whose lines stop after this many.
    Clamp(u32),
}

/// See [`LegacyBox`]. The computed `display` keeps the specified `-webkit-box` -- layout needs to know
/// -- and `getComputedStyle` reports the clamping one as `flow-root` (`Page::resolve_at_read`).
pub fn legacy_box(arena: &StyleArena, key: StyleKey) -> LegacyBox {
    let display = arena.value(key, PropertyId::Display).trim();
    if display != "-webkit-box" && display != "-webkit-inline-box" {
        return LegacyBox::No;
    }
    let vertical = matches!(arena.value(key, PropertyId::WebkitBoxOrient).trim(), "vertical" | "block-axis");
    let clamp = arena.value(key, PropertyId::WebkitLineClamp).trim().parse::<u32>().ok().filter(|n| *n > 0);
    match (vertical, clamp) {
        (true, Some(n)) => LegacyBox::Clamp(n),
        _ => LegacyBox::Flex,
    }
}

/// A style attribute beats every selector in its origin, however specific.
///
/// Not a very large number standing in for "quite specific" — the spec puts inline styles in their
/// own cascade step above all author rules, and `u32::MAX` is that step. A selector cannot reach it:
/// specificity packs three counts into the same word, so its maximum is short of this by
/// construction.
const INLINE_SPECIFICITY: u32 = u32::MAX;

/// One value a declarative animation is putting on this element.
///
/// A CSS animation and a CSS transition are cascade ORIGINS, not a post-processing step over the
/// cascade's answer: an animation beats every normal author declaration and loses to an
/// `!important` one, and a transition beats even a UA `!important` one. Both were measured on
/// Chrome rather than read off the spec — see [`Precedence::animation`]. Feeding them in here, as
/// declarations with their own rung, is what makes `getComputedStyle` report the moving value and
/// `!important` still win against an animation.
#[derive(Debug, Clone)]
pub struct AnimatedValue {
    pub property: PropertyId,
    /// The SPECIFIED value at this instant (`"0.4"`, `"24px"`), which then computes like any other.
    pub value: String,
    /// A transition's rung rather than an animation's.
    pub transition: bool,
}

/// Compute the style of one element and return the key it interned to.
///
/// `parent` is the already-computed style of the element's parent, or `None` for the root.
/// `formatting` is what the parent lays this element out as — see [`ParentFormatting`].
pub fn compute<'a>(
    sheets: &'a [Stylesheet],
    matched: &[MatchedRule],
    inline: Option<InlineStyle<'a>>,
    parent: Option<StyleKey>,
    formatting: ParentFormatting,
    ctx: &ComputeContext,
    arena: &mut StyleArena,
) -> StyleKey {
    compute_animated(sheets, matched, inline, parent, formatting, ctx, arena, &[])
}

/// [`compute`], plus the values declarative animations are putting on the element.
#[allow(clippy::too_many_arguments)]
pub fn compute_animated<'a>(
    sheets: &'a [Stylesheet],
    matched: &[MatchedRule],
    inline: Option<InlineStyle<'a>>,
    parent: Option<StyleKey>,
    formatting: ParentFormatting,
    ctx: &ComputeContext,
    arena: &mut StyleArena,
    animated: &[AnimatedValue],
) -> StyleKey {
    // Pass one: the winning declaration per property. The text-kept declarations -- custom
    // properties, and anything containing `var()` -- run the same comparison against a small list
    // rather than the fixed array, because their names are open-ended.
    let mut winner = Winners::new();
    let mut raw_winner: Vec<(&'a str, CascadeKey, &'a str, Option<&'a str>)> = Vec::new();
    let mut reverts = false;
    each_declaration(sheets, matched, inline.as_ref(), ctx, |key, property, value| {
        reverts |= Revert::of(&value).is_some();
        winner.consider(key, property, value);
    });
    for m in matched {
        let sheet = &sheets[m.sheet as usize];
        let rule = &sheet.cascade_rules()[m.rule as usize];
        // The base a `url()` reaching this declaration through `var()` resolves against is THIS
        // declaration's sheet, not the sheet that declared the custom property (see `style::urls`).
        let base = sheet.base_of(rule.base);
        for (i, decl) in sheet.raw_of(rule).iter().enumerate() {
            consider_raw(&mut raw_winner, rule_key(sheet, m, raw_at(decl, i), decl.important), sheet.slice(decl.name), sheet.slice(decl.value), base);
        }
    }
    if let Some(inline) = &inline {
        // A `style` attribute has no URL of its own; its values resolve against the document.
        for (i, decl) in inline.sheet.raw_declarations().iter().enumerate() {
            consider_raw(&mut raw_winner, inline_key(raw_at(decl, i), decl.important), inline.sheet.slice(decl.name), inline.sheet.slice(decl.value), None);
        }
    }

    // Quirks mode: a unitless length or a hashless colour becomes the value it spells.
    if ctx.quirks {
        let fixes: Vec<(PropertyId, CascadeKey, String)> = winner
            .list
            .iter()
            .filter_map(|(p, k, v)| quirky_value(*p, v).map(|q| (*p, *k, q)))
            .collect();
        for (p, k, q) in fixes {
            if let Some(i) = winner.slot[p.index()].checked_sub(1) {
                winner.list[i as usize] = (p, k, Cow::Owned(q));
            }
        }
    }

    // The declarative animations, on their own rungs. They go in after the author declarations
    // because `consider` is a running maximum and the rung is what decides — the order of these
    // calls does not matter, only the key does.
    for a in animated {
        winner.consider(
            CascadeKey {
                precedence: if a.transition { Precedence::transition() } else { Precedence::animation() },
                context: 0,
                attached: false,
                layer: 0,
                proximity: 0,
                specificity: 0,
                order: (0, 0, (0, 0)),
            },
            a.property,
            Cow::Owned(a.value.clone()),
        );
    }

    // The two halves of the computed style, filled by the passes below.
    let mut own = PropertySet::default();
    let mut inherited = PropertySet::default();
    let mut inherited_changed = false;

    // Between the passes: resolve the text-kept declarations.
    //
    // Custom properties first, because the rest are written in terms of them. Each one's own value
    // may reference others, so they are substituted against the element's *whole* map (this
    // element's, over the parent's) rather than in the order they were declared.
    //
    // ONLY THIS ELEMENT'S OWN custom properties are resolved here. The parent's are already
    // computed -- substituted, validated against their registrations -- so re-resolving them is the
    // identity, and they are read in place from the parent's inherited half (a sorted set: a binary
    // search per name). This used to copy every inherited custom property into two `String` maps and
    // re-substitute each one, on every element with a `var()` of its own; on github.com, whose
    // `:root` declares hundreds of them, that copying was 29 % of a whole page load's CPU
    // (measured 2026-09-29). An earlier change had already skipped it for elements with no text-kept
    // declaration at all.
    // `@property` registrations. Shared, so this is a refcount and not a copy.
    let registry = arena.registry.clone();
    let parent_customs: Option<&PropertySet> = parent.map(|p| arena.inherited(p.inherited));
    let inherited_value = |name: &str| parent_customs.and_then(|set| set.custom(name)).map(str::to_string);
    // This element's own custom declarations, raw: the winning value per name.
    let mut declared: Vec<(&str, &str)> = Vec::new();
    for (name, _, value, _) in &raw_winner {
        if !name.starts_with("--") {
            continue;
        }
        match declared.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = value,
            None => declared.push((name, value)),
        }
    }
    // Resolve each custom property against the raw map, and *drop* the ones that cannot be
    // resolved. A custom property in a cycle computes to the guaranteed-invalid value, which is
    // also its initial value -- so a `var()` naming it behaves as though it were never set, and a
    // fallback applies. Treating a cycle as an outright failure instead would swallow the fallback.
    //
    // A REGISTERED property that is not declared here takes its initial value (or, if it inherits,
    // the parent's), and one whose value does not fit its syntax is invalid at computed-value time and
    // does the same -- Chrome 150: `@property --x { syntax: "<length>"; initial-value: 7px }` with
    // `--x: red` reads `7px`, and `width: var(--x)` is 7px.
    let registered_initial = |name: &str| registry.get(name).and_then(|r| r.initial.clone());
    // Chrome 150, measured: a custom property in a `var()` CYCLE -- fallbacks count as references --
    // is invalid at computed-value time, and so is one whose `var()` names an undefined property with no
    // fallback. Either computes to the guaranteed-invalid value HERE, not the parent's; and a `var(--x, fb)`
    // naming a cycle member takes `fb`. The raw map this replaced handed a cycling sibling's raw text to
    // `substitute`, which ended in "" instead of the fallback.
    let index_of = |name: &str| declared.iter().position(|(d, _)| *d == name);
    let edges: Vec<Vec<usize>> = declared
        .iter()
        .map(|(_, v)| crate::style::variables::var_refs(v).iter().filter_map(|r| index_of(r)).collect())
        .collect();
    let cyclic = crate::style::variables::cyclic_nodes(&edges);
    let probe = crate::style::registered::TypedContext {
        font_size: 16.0,
        root_font_size: ctx.root_font_size,
        viewport: (ctx.viewport_width, ctx.viewport_height),
    };
    // A REGISTERED property invalid at computed-value time takes its inherited value when it inherits, else
    // its initial value.
    let invalid_registered = |name: &str, reg: &crate::style::registered::Registration| -> Option<String> {
        let fallback = if reg.inherits { inherited_value(name) } else { None };
        fallback.or_else(|| reg.initial.clone())
    };
    let mut resolved: Vec<Option<String>> = vec![None; declared.len()];
    for i in crate::style::variables::dependency_order(&edges, &cyclic) {
        let (name, value) = declared[i];
        let substituted = {
            let lookup = |r: &str| match index_of(r) {
                Some(j) => resolved[j].clone(),
                None => inherited_value(r).or_else(|| registered_initial(r)),
            };
            // Not trimmed: what a substitution leaves at the value's edges stays (Chrome 150: `--e:; --a: var(--e) x`
            // computes ` x`, and `--v: env(nope, 3px)` ` 3px`).
            substitute(value, &lookup)
        };
        resolved[i] = match (substituted, registry.get(name)) {
            (Some(v), Some(reg)) if reg.compute(&v, &probe).is_none() => invalid_registered(name, reg),
            (Some(v), _) => Some(v),
            (None, Some(reg)) => invalid_registered(name, reg),
            (None, None) => None,
        };
    }
    for (i, (name, _)) in declared.iter().enumerate() {
        if cyclic[i] {
            if let Some(reg) = registry.get(name) {
                resolved[i] = invalid_registered(name, reg);
            }
        }
    }
    // A name declared here is its resolution here -- or, invalid, the guaranteed-invalid value (an
    // unregistered name) / its registration's value, NOT the parent's; a name not declared here is the
    // parent's computed value.
    let lookup = |name: &str| {
        match index_of(name) {
            Some(j) => resolved[j].clone(),
            None => inherited_value(name),
        }
        .or_else(|| registered_initial(name))
    };
    // Registered, non-inherited custom properties this element declares: they live on the OWN half,
    // so a child does not inherit them, and are typed once the element's font size is known.
    let mut own_custom: Vec<(String, String)> = Vec::new();

    for (name, key, value, base) in &raw_winner {
        if name.starts_with("--") {
            // Only a custom property that resolved goes into the style; one that did not is
            // guaranteed-invalid, which is indistinguishable from unset.
            if let Some(v) = lookup(name) {
                match registry.get(name) {
                    Some(reg) if !reg.inherits => own_custom.push(((*name).to_string(), v)),
                    _ => {
                        inherited.set_custom(name, &v);
                        inherited_changed = true;
                    }
                }
            } else if registry.get(name).is_none() {
                // Guaranteed-invalid here: the entry keeps the parent's value from merging in.
                inherited.set_custom(name, crate::style::computed::GUARANTEED_INVALID);
                inherited_changed = true;
            }
            continue;
        }
        // A standard property that was deferred: substitute, then re-parse it as a declaration,
        // which is where a shorthand finally gets expanded and a value finally gets validated.
        let Some(substituted) = substitute(value, &lookup) else {
            // Invalid at computed-value time. The declaration still *won* -- it is not skipped so
            // an earlier one can take over -- and what it computes to is `unset`.
            if let Some(property) = PropertyId::from_name(name) {
                winner.consider(*key, property, Cow::Borrowed("unset"));
            }
            continue;
        };
        // A `url()` that arrives through `var()` -- from the custom property's value or from the
        // reference's own fallback -- is resolved HERE, against the sheet holding THIS declaration.
        // Not at parse time, because a custom property's value is kept as raw tokens (Chrome 150:
        // `getComputedStyle(el).getPropertyValue('--u')` is `url(varimg.png)`, still relative), and
        // not against the DECLARING sheet, because Chrome resolves against the user: a `--shared`
        // declared in `/sub/v.css` and referenced from `/other/cross.css` computes
        // `/other/sharedimg.png`, and one declared in a `<style>` and referenced from `/sub/v.css`
        // computes `/sub/docshared.png` (measured on stock Chrome 150.0.7871.46).
        let substituted = match base {
            Some(b) if substituted.contains("url(") => {
                crate::style::urls::absolutize(&substituted, b).unwrap_or(substituted)
            }
            _ => substituted,
        };
        for (property, computed) in reparse(name, &substituted) {
            // A `revert`/`revert-layer` that arrives through `var()` is not this declaration
            // rolling back: CSS-wide keywords in a custom property apply to the CUSTOM property,
            // which then holds the guaranteed-invalid value, so the `var()` is invalid at
            // computed-value time. Chrome 150: `--d: revert; display: var(--d)` on a <div> -> inline.
            let computed = if Revert::of(&computed).is_some() { "unset".to_string() } else { computed };
            winner.consider(*key, property, Cow::Owned(computed));
        }
    }

    // `revert` / `revert-layer`: a winner that rolls back is replaced by the cascade's
    // answer WITHOUT the origin (or layer) it came from. See [`resolve_reverts`].

    // Logical properties land in their physical slots, each with its own cascade key, so a
    // `margin-inline-start` declared after a `margin-left` beats it and one declared before loses
    // -- exactly the comparison two physical declarations would have made. See `style::logical`.
    if winner.logical || reverts {
        let orientation = {
            let parent_half = parent.map(|p| arena.inherited(p.inherited));
            let pick = |property: PropertyId| -> String {
                let from_parent = || {
                    parent_half
                        .and_then(|h| h.get(property))
                        .unwrap_or_else(|| property.initial())
                        .to_string()
                };
                match winner.get(property) {
                    Some((_, v)) => match Cssate::of(v) {
                        Cssate::Value => v.trim().to_ascii_lowercase(),
                        Cssate::Initial => property.initial().to_string(),
                        _ => from_parent(),
                    },
                    None => from_parent(),
                }
            };
            Orientation::of(&pick(PropertyId::WritingMode), &pick(PropertyId::Direction))
        };
        if winner.logical {
            winner.map_logical(orientation);
        }
        // `revert` / `revert-layer`: a winner that rolls back is replaced by the
        // cascade's answer WITHOUT the origin (or layer) it came from. After the logical mapping,
        // because the UA sheet writes `<p>`'s margins as `margin-block-*`: `margin: revert` on a `<p>`
        // is 16px in Chrome 150 only through them. See [`resolve_reverts`].
        if reverts {
            resolve_reverts(&mut winner, sheets, matched, inline.as_ref(), ctx, orientation);
        }
    }

    // The parent's custom properties reach this element too, and they arrive already resolved --
    // through the merge at the end, which copies every custom property of the parent's half this
    // element did not set, exactly as it copies the standard ones.
    //
    // THERE WAS A COPY HERE AS WELL, and its only effect the merge did not also have
    // was to set `inherited_changed` whenever the parent carried any custom property at all. That
    // turned "nothing inherited changed, so share the parent's half by identity" -- the shortcut
    // this file is shaped around -- into a clone, a merge and a hash of the parent's whole half on
    // every element under a `:root` that declares one `--var`. The half it produced was the
    // parent's own content, so interning handed back the parent's id anyway: the answer is the
    // same, and now so is the path to it.

    // Pass two, part one: the three properties everything else is written in terms of.
    //
    // `font-size` first, because `em` means it and it is itself resolved against the *parent's*.
    // Then `color`, because `currentColor` means it. Only then can the rest resolve.
    //
    // The parent's three are parsed out of its inherited half ONCE per distinct half, not once per
    // child -- see [`parent_basis`].
    let basis = parent.map(|p| parent_basis(arena, p.inherited));
    let parent_weight = basis.and_then(|b| b.weight).unwrap_or(400.0);
    let viewport = (ctx.viewport_width, ctx.viewport_height);

    // `font-size` as Blink's `FontBuilder` builds it: the parent's state, this
    // element's own value against it, a keyword looked up in its family's row, the family change to or
    // from a bare `monospace`, and the minimum logical font size -- see [`resolve::FontSizeState`].
    let parent_state = basis.and_then(|b| b.size_state).unwrap_or(resolve::FontSizeState::ROOT);
    let monospace = match winner.get(PropertyId::FontFamily) {
        Some((_, family)) => match Cssate::of(family) {
            Cssate::Value => resolve::is_bare_monospace(family),
            Cssate::Initial => false,
            _ => parent_state.monospace,
        },
        None => parent_state.monospace,
    };
    let mut size_state = match winner.get(PropertyId::FontSize) {
        Some((_, specified)) => match Cssate::of(specified) {
            Cssate::Value => resolve::font_size_state(specified, &parent_state, ctx.root_font_size, viewport),
            // the initial value is `medium`
            Cssate::Initial => resolve::FontSizeState { specified: 0.0, keyword: 4, absolute: false, monospace: parent_state.monospace },
            // `inherit` and `unset` are the same thing for an inherited property.
            _ => parent_state,
        },
        None => parent_state,
    };
    // `FontBuilder::UpdateSpecifiedSize`: a keyword's size, in this element's family
    if size_state.specified == 0.0 && size_state.keyword > 0 {
        size_state.specified = resolve::keyword_font_size(size_state.keyword, monospace, ctx.quirks);
    }
    // `FontBuilder::CheckForGenericFamilyChange`
    if !size_state.absolute && monospace != parent_state.monospace {
        size_state.specified = if size_state.keyword > 0 {
            resolve::keyword_font_size(size_state.keyword, monospace, ctx.quirks)
        } else {
            let fixed = resolve::DEFAULT_FIXED_FONT_SIZE / resolve::DEFAULT_FONT_SIZE;
            if parent_state.monospace {
                size_state.specified / fixed
            } else {
                size_state.specified * fixed
            }
        };
    }
    size_state.monospace = monospace;
    let font_size_px = size_state.computed(ctx.min_logical_font_size);
    // what `em` means for every other property: the SPECIFIED size
    let em_px = size_state.specified;
    // The state rides in the inherited half beside the computed size, wherever it moves from the parent's
    // (a half with none is `FontSizeState::ROOT`'s); a size that moved without a `font-size` of this
    // element's own (a family change) is written there too.
    if size_state != parent_state {
        inherited.set_custom(resolve::FontSizeState::KEY, &size_state.encode());
        inherited_changed = true;
        let parent_px = basis.and_then(|b| b.font_size);
        if winner.get(PropertyId::FontSize).is_none() && parent_px != Some(font_size_px) {
            inherited.set(PropertyId::FontSize, &crate::value::serialize_px(font_size_px));
        }
    }

    let parent_color = basis.and_then(|b| b.color).unwrap_or(Rgba::BLACK);
    let own_color = match winner.get(PropertyId::Color) {
        Some((_, specified)) => match Cssate::of(specified) {
            // `color: currentColor` is `color: inherit` -- there is no other colour to point at.
            // A `color-mix()` / relative colour naming `currentcolor` takes the parent's (`color: currentcolor` is
            // `inherit`).
            Cssate::Value => match crate::value::resolve_color(specified, parent_color) {
                Some(rgba) => rgba,
                None => match Color::parse_str(specified) {
                    Ok(Color::Rgba(rgba)) => rgba,
                    _ => parent_color,
                },
            },
            Cssate::Initial => Rgba::BLACK,
            _ => parent_color,
        },
        None => parent_color,
    };

    // a vertical writing mode, this element's or the one it inherits (`text-emphasis-style`'s default shape)
    let vertical = {
        let inherited_mode = || parent.map(|p| arena.inherited(p.inherited)).and_then(|h| h.get(PropertyId::WritingMode).map(str::to_string));
        let mode = match winner.get(PropertyId::WritingMode) {
            Some((_, v)) => match Cssate::of(v) {
                Cssate::Value => v.trim().to_ascii_lowercase(),
                Cssate::Initial => String::new(),
                _ => inherited_mode().unwrap_or_default(),
            },
            None => inherited_mode().unwrap_or_default(),
        };
        mode.starts_with("vertical") || mode.starts_with("sideways") || matches!(mode.as_str(), "tb" | "tb-rl")
    };
    let rctx = resolve::Context {
        font_size: em_px,
        computed_font_size: font_size_px,
        root_font_size: ctx.root_font_size,
        viewport_width: ctx.viewport_width,
        viewport_height: ctx.viewport_height,
        color: own_color,
        parent_font_weight: parent_weight,
        container: ctx.container,
        vertical,
    };

    // The registered custom properties' TYPED computed values, now that `em` means something:
    // Chrome 150, `@property --x { syntax: "<length>" … }` with `--x: 2em; font-size: 10px` reads
    // `20px`.
    if !registry.is_empty() {
        let typed = crate::style::registered::TypedContext {
            font_size: em_px,
            root_font_size: ctx.root_font_size,
            viewport: (ctx.viewport_width, ctx.viewport_height),
        };
        for (name, value) in &own_custom {
            let v = registry.get(name).and_then(|r| r.compute(value, &typed)).unwrap_or_else(|| value.clone());
            own.set_custom(name, &v);
        }
        let declared: Vec<(String, String)> = raw_winner
            .iter()
            .filter(|(n, ..)| n.starts_with("--") && registry.get(n).is_some_and(|r| r.inherits))
            .filter_map(|(n, ..)| inherited.custom(n).map(|v| ((*n).to_string(), v.to_string())))
            .collect();
        for (name, value) in declared {
            if let Some(v) = registry.get(&name).and_then(|r| r.compute(&value, &typed)) {
                inherited.set_custom(&name, &v);
            }
        }
    }

    // Pass two, part two: every property, resolved.

    //
    // In `ALL` order, as it always was, but over the properties that HAVE a winner rather than all
    // [`COUNT`] of them: the loop body `continue`d on every other one. See [`Winners::in_order`].
    for (property, specified) in winner.in_order() {
        let specified: &str = specified;
        // A value that comes from the parent is *already* computed; re-resolving it would resolve
        // `em` a second time against a different font.
        let from_parent = || inherited_from(arena, parent, property).map(Resolved::Computed);
        let resolved: Option<Resolved<'_>> = match Cssate::of(specified) {
            Cssate::Value => Some(Resolved::Specified(specified)),
            Cssate::Initial => None,
            Cssate::Inherit => from_parent(),
            // `unset` is `inherit` for an inherited property and `initial` for the rest, which is
            // also exactly what happens when a property has no winner at all.
            Cssate::Unset if property.is_inherited() => from_parent(),
            Cssate::Unset => None,
        };
        let value = resolved.map(|r| match r {
            // `th`'s UA `text-align: -internal-center` (Blink's `TextAlign::ApplyValue`): the parent's alignment when
            // that is not the initial `start`, else `center` -- Chrome 150 reads `center` for a <th>, and the
            // table's `right` for one in a `text-align: right` table.
            Resolved::Specified(text) if property == PropertyId::TextAlign && text.trim().eq_ignore_ascii_case("-internal-center") => {
                match inherited_from(arena, parent, property) {
                    Some(p) if p != "start" => p,
                    _ => "center".to_string(),
                }
            }
            Resolved::Specified(text) => resolve::computed_value(property, text, &rctx),
            Resolved::Computed(text) => text,
        });
        match (property.is_inherited(), value) {
            (true, Some(v)) => {
                inherited.set(property, &v);
                inherited_changed = true;
            }
            // An inherited property explicitly reset to its initial value still changes the
            // inherited half: the subtree below must stop seeing the parent's value.
            (true, None) => {
                inherited.set(property, property.initial());
                inherited_changed = true;
            }
            (false, Some(v)) => own.set(property, &v),
            (false, None) => {}
        }
    }

    // Pass three: blockify an out-of-flow box, or a flex/grid item. See [`blockify`].
    blockify(&mut own, formatting);

    // Anything inherited that this element did not set comes from the parent. Done only when
    // something changed: otherwise the parent's half is reused by identity, and that shortcut is
    // the reason the arena stays small.
    let inherited_id = match (parent, inherited_changed) {
        (Some(p), false) => p.inherited,
        (None, false) => 0,
        (parent, true) => {
            if let Some(p) = parent {
                inherited.inherit_from(arena.inherited(p.inherited));
            }
            arena.intern_inherited(inherited)
        }
    };
    StyleKey { inherited: inherited_id, own: arena.intern_own(own) }
}

/// **Blockification**: an out-of-flow box, or a flex/grid ITEM, computes its block-level `display`.
///
/// A box is out of flow when `position` is `absolute` or `fixed`, or when `float` is not `none`.
/// `position: relative` and `position: sticky` are IN flow and change nothing -- that is measured
/// below, not assumed, because both are routinely described as "positioned".
///
/// # The second trigger: the PARENT's formatting context
///
/// A child of a `display: flex | inline-flex | grid | inline-grid` box is a flex or grid **item**,
/// and Chrome blockifies it through the **same table**. Measured on Chrome 150 over the 29 `display`
/// keywords below crossed with all four container displays -- 116 cells, and every one of them is
/// the answer [`block_level_equivalent`] already gave for the out-of-flow trigger. There is one map,
/// not two, which is why this is a second condition on the same function rather than a second
/// function.
///
/// Three things about it are measured rather than reasoned, and all three would have been got wrong:
///
/// * **`float` SURVIVES on a flex item.** `<div style=display:flex><span style=float:left>` computes
///   `float: left` in Chrome, where `position: absolute` erases it to `none`. So being an item
///   blockifies and nothing else; the erasure below stays keyed to `out_of_flow` alone.
/// * **`display: contents` FORWARDS the context, and `display: none` STOPS it** -- see
///   [`ParentFormatting`], which is why the trigger is threaded down the tree instead of read off
///   the parent's computed `display` one level up.
/// * **Only flex and grid containers do it.** A `table`, `table-cell`, `list-item`, `flow-root`,
///   `inline-block` or `ruby` parent leaves its children's `display` alone. (`ruby` has a rule of
///   its own in the opposite direction -- it *inlinifies* a `block` child to `inline-block` -- which
///   is not blockification and is not implemented here.)
///
/// An absolutely positioned child of a flex container is **not** a flex item, which would matter if
/// the two maps differed. They do not: measured cell by cell, `position: absolute` inside a flex
/// container gives the same computed `display` as `position: absolute` anywhere else.
///
/// # Why this is in the cascade and not in `innerText`
///
/// It is a computed-value rule, so `getComputedStyle(el).display` is one of the things it has to
/// answer correctly. The consumer that made it visible is the HTML rendered-text collection steps
/// (`korvus-webapi`'s `INLINE_DISPLAYS`), which branch on computed `display` to decide where a line
/// break goes: with `inline` reported for a `position: absolute` `<span>`, two text runs either side
/// of it FUSE into one token -- the right number of characters and the wrong words. Fixing that at
/// the consumer would have left `getComputedStyle` lying and the next consumer broken, so the
/// correction is here, where the wrong value is produced.
///
/// # The table is RECORDED, not read off the spec
///
/// Measured on stock Google Chrome 150.0.7871.46 on 2026-09-14, over CDP on a loopback fixture, for
/// every `display` keyword below crossed with `position: static|relative|absolute|fixed|sticky` and
/// `float: none|left|right`. Four rows of it are NOT what CSS Display §2.7's prose would suggest and
/// would have been got wrong from the spec:
///
/// * the **internal table displays** (`table-cell`, `table-row`, the three row groups, the two
///   column kinds, `table-caption`) all blockify to `block`. §2.7 discusses inline-level boxes; these
///   are not inline-level, and they blockify anyway.
/// * `display: table` and `display: list-item` are already block-level and are **unchanged** --
///   `inline-table` becomes `table`, but `table` does not become `block`.
/// * `ruby` blockifies to the two-keyword `block ruby`, which is the only place this engine produces
///   a multi-keyword `display`, and it is what Chrome serializes.
/// * **`float` computes to `none` on an absolutely positioned box** -- `position` wins outright, and
///   the losing `float` is not merely ignored at layout time, it is gone from the computed style.
///   `float: left; position: absolute` reports `float: none` in Chrome, so it reports it here.
///
/// `display: none` and `display: contents` are untouched by any of it.
///
/// # The input is already normalised
///
/// CSS Display Level 3's two-keyword syntax is resolved before this runs (see
/// `resolve`'s `canonical_display`), so the table below is written against the canonical spellings
/// and `"inline flex"` cannot reach it -- that value computes to `inline-flex` in pass two. The two
/// multi-keyword values that ARE canonical, `inline list-item` and `inline flow-root list-item`,
/// have their own arms.
fn blockify(own: &mut PropertySet, formatting: ParentFormatting) {
    let out_of_flow = matches!(
        own.get(PropertyId::Position).unwrap_or_else(|| PropertyId::Position.initial()),
        "absolute" | "fixed"
    );
    let floated =
        is_floated(own.get(PropertyId::Float).unwrap_or_else(|| PropertyId::Float.initial()));
    // A flex or grid ITEM is blockified by the same table, and the trigger is the PARENT's
    // formatting context rather than anything on this element. See [`ParentFormatting`].
    let item = formatting == ParentFormatting::FlexOrGrid;
    if !out_of_flow && !floated && !item {
        return;
    }
    if let Some(blockified) =
        block_level_equivalent(own.get(PropertyId::Display).unwrap_or_else(|| PropertyId::Display.initial()))
    {
        own.set(PropertyId::Display, blockified);
    }
    // Only when it was actually set to something: writing `none` over an unset `float` would put an
    // entry in the interned set that is identical to the initial value, and two styles that differ
    // only in that would stop sharing an arena slot.
    if out_of_flow && floated {
        own.set(PropertyId::Float, "none");
    }
}

/// The block-level equivalent of one computed `display`, or `None` when it already is one.
///
/// The table [`blockify`] documents, as code. `None` covers three separate cases that all mean "do
/// nothing": already block-level (`block`, `flex`, `table`, `list-item`, …), box-suppressing
/// (`none`), and box-replacing (`contents`).
fn block_level_equivalent(display: &str) -> Option<&'static str> {
    Some(match display {
        "inline" | "inline-block" => "block",
        "inline-flex" => "flex",
        "inline-grid" => "grid",
        "inline-table" => "table",
        // The two spellings that stay multi-keyword because no shorter one exists. `inline
        // flow-root list-item` loses only its outside, exactly as `inline list-item` does.
        "inline list-item" => "list-item",
        "inline flow-root list-item" => "flow-root list-item",
        // The internal table displays. Measured; §2.7's prose does not cover them.
        "table-cell" | "table-row" | "table-row-group" | "table-header-group" | "table-footer-group"
        | "table-column" | "table-column-group" | "table-caption" => "block",
        "ruby" => "block ruby",
        // `ruby-text` is a real computed value; `ruby-base` is one Chrome does not accept at all
        // (it drops the declaration and reports `inline`) and that this engine still carries
        // through, because it validates no keyword value anywhere. Its blockified answer is right
        // either way, so the arm stays until validation arrives.
        "ruby-text" | "ruby-base" => "block",
        _ => return None,
    })
}

/// The same comparison as [`consider`], over a list instead of an array. These declarations are
/// keyed by name rather than by [`PropertyId`], because a custom property's name is open-ended --
/// and there are few enough of them on any one element that a linear scan beats a map's allocation.
fn consider_raw<'a>(
    winner: &mut Vec<(&'a str, CascadeKey, &'a str, Option<&'a str>)>,
    key: CascadeKey,
    name: &'a str,
    value: &'a str,
    base: Option<&'a str>,
) {
    match winner.iter_mut().find(|(n, ..)| *n == name) {
        Some(slot) if key > slot.1 => *slot = (name, key, value, base),
        Some(_) => {}
        None => winner.push((name, key, value, base)),
    }
}

/// Where a property's value came from, which decides whether it still needs resolving.
enum Resolved<'a> {
    /// As the sheet wrote it — `2em`, `red`, `bold`.
    Specified(&'a str),
    /// Already computed, taken from the parent. Resolving it again would resolve `em` a second
    /// time, against a font that is no longer the one it was written against.
    Computed(String),
}

/// What a child's cascade reads off its parent's inherited half before anything else: the font
/// size `em` means, the weight `bolder` means, and the colour `currentColor` means. Each is `None`
/// when the half does not carry it, or carries something that does not parse, and the caller's
/// default applies -- exactly what the three `and_then` chains this replaced produced.
///
/// MEMOISED PER INHERITED HALF. Every child of every element re-parsed these three --
/// `Color::parse_str` on the parent's `color` included -- which on the P9c fixture was ~200 ns of a
/// ~1,450 ns `compute_animated`, re-deriving what a few hundred distinct halves already answered. An
/// interned half is never rewritten (its id names the same values for the arena's lifetime), so the
/// answer banked against the id is by construction the one re-parsing would give.
fn parent_basis(arena: &mut StyleArena, inherited: u32) -> InheritedBasis {
    if let Some(basis) = arena.basis(inherited) {
        return basis;
    }
    let set = arena.inherited(inherited);
    let basis = InheritedBasis {
        font_size: set.get(PropertyId::FontSize).and_then(px_of),
        size_state: set.custom(resolve::FontSizeState::KEY).and_then(resolve::FontSizeState::decode),
        weight: set.get(PropertyId::FontWeight).and_then(|w| w.trim().parse::<f32>().ok()),
        color: set.get(PropertyId::Color).and_then(|c| match Color::parse_str(c) {
            Ok(Color::Rgba(rgba)) => Some(rgba),
            _ => None,
        }),
    };
    arena.set_basis(inherited, basis);
    basis
}

/// The winning declaration per property, as a slot per property that INDEXES a short list.
///
/// # Why not the array of winners it replaces
///
/// This was `[Option<(CascadeKey, Cow<str>)>; COUNT]` -- 56 bytes a slot, ~28 KB for the element,
/// written in full to create it and read in full by the `ALL` loop to find the two or three slots
/// that were set. Measured on the P9c fixture (12,004 elements, ~2 winners each) in ns per element:
/// creating the array ~210, the `ALL` loop ~570, of a `compute_animated` of ~1,450. Neither is the
/// cascade; both are walking memory nobody wrote to.
///
/// A slot is now a `u16` (1 KB), and the loop walks the list, sorted into `ALL` order first -- the
/// order the body ran in before, so every `PropertySet::set` and every flag happens in the same
/// sequence. `consider` is still one comparison per declaration, with no sorting and nothing
/// allocated beyond the list itself.
struct Winners<'a> {
    /// `0` is "no winner"; otherwise `i + 1` for `list[i]`.
    slot: [u16; COUNT],
    list: Vec<(PropertyId, CascadeKey, Cow<'a, str>)>,
    /// Some winner is a logical property, which [`Winners::map_logical`] has to move onto its
    /// physical slot before the values are resolved. A flag, so an element with none pays nothing.
    logical: bool,
}

impl<'a> Winners<'a> {
    fn new() -> Self {
        Winners { slot: [0; COUNT], list: Vec::new(), logical: false }
    }

    /// Move every logical winner onto the physical property it is under `o`, competing there with
    /// its own key. The logical entries are then emptied: nothing is stored under a logical name.
    fn map_logical(&mut self, o: Orientation) {
        let logical: Vec<(PropertyId, CascadeKey, Cow<'a, str>)> = self
            .list
            .iter()
            .filter(|(p, _, _)| is_logical(*p))
            .cloned()
            .collect();
        for (property, key, value) in logical {
            // Emptied rather than removed: removing would renumber every slot after it.
            self.slot[property.index()] = 0;
            if let Some(physical) = to_physical(property, o) {
                self.consider(key, physical, value);
            }
        }
        self.list.retain(|(p, _, _)| !is_logical(*p));
        for s in self.slot.iter_mut() {
            *s = 0;
        }
        for (i, (p, _, _)) in self.list.iter().enumerate() {
            self.slot[p.index()] = i as u16 + 1;
        }
        self.logical = false;
    }

    /// The winning key and specified value for `property`, if any declaration named it.
    fn get(&self, property: PropertyId) -> Option<(&CascadeKey, &Cow<'a, str>)> {
        match self.slot[property.index()] {
            0 => None,
            i => {
                let (_, key, value) = &self.list[i as usize - 1];
                Some((key, value))
            }
        }
    }

    /// Keep `value` if it outranks whatever is in `property`'s slot. This *is* the cascade: one
    /// comparison, no sorting.
    fn consider(&mut self, key: CascadeKey, property: PropertyId, value: Cow<'a, str>) {
        match self.slot[property.index()] {
            0 => {
                self.logical |= is_logical(property);
                self.list.push((property, key, value));
                // `COUNT` is a few hundred, so the list can never outgrow a `u16`.
                self.slot[property.index()] = self.list.len() as u16;
            }
            i => {
                let entry = &mut self.list[i as usize - 1];
                if key > entry.1 {
                    entry.1 = key;
                    entry.2 = value;
                }
            }
        }
    }

    /// Every property with a winner and its specified value, in [`ALL`](crate::style::property::ALL) order -- which is property
    /// index order, `PropertyId::index` being the position in `ALL`.
    fn in_order(&self) -> Vec<(PropertyId, &str)> {
        let mut out: Vec<(PropertyId, &str)> = self.list.iter().map(|(p, _, v)| (*p, v.as_ref())).collect();
        out.sort_unstable_by_key(|(p, _)| p.index());
        out
    }
}

/// Re-parse a substituted declaration into the longhands it turns out to be.
///
/// This is the moment a `var()`-bearing shorthand finally expands and a `var()`-bearing value
/// finally gets validated: `margin: var(--gap)` does not know whether it is one value or four until
/// `--gap` is known. Going back through the parser rather than reimplementing that here is what
/// keeps the two paths from drifting.
fn reparse(name: &str, value: &str) -> Vec<(PropertyId, String)> {
    let sheet = crate::style::parse::parse_inline_style(&format!("{name}: {value}"));
    sheet
        .declarations()
        .iter()
        .map(|d| (d.property, sheet.value_of(d).to_string()))
        .collect()
}

/// What the parent computes for `property`, or `None` at the root (where `inherit` means initial).
///
/// Copied out rather than borrowed: the caller goes on to intern into the same arena, and an
/// outstanding borrow of it would make that impossible. `inherit` and `unset` are written rarely
/// enough that a copy per occurrence is not worth restructuring around.
fn inherited_from(arena: &StyleArena, parent: Option<StyleKey>, property: PropertyId) -> Option<String> {
    let parent = parent?;
    if property.is_inherited() {
        return arena.inherited(parent.inherited).get(property).map(str::to_string);
    }
    // `inherit` on a NON-inherited property takes the parent's computed value, which lives in
    // its OWN half: `html { box-sizing: border-box } * { box-sizing: inherit }` -- CSS-Tricks' pattern,
    // fontshare.com's stylesheet -- left every element below the root at the initial `content-box`, and
    // `overflow`, `position`, `display`, `background-color` and `margin: inherit` the same (stock Chrome
    // 150: the parent's value). A parent that set nothing has the initial value, which `None` gives.
    arena.own(parent.own).get(property).map(str::to_string)
}

/// Where typed declaration `i` stands in its block: after the `i` typed declarations before it, and after every
/// text-kept one written before it.
fn typed_at(i: usize) -> (u32, u32) {
    (i as u32 + 1, 0)
}

/// Where text-kept declaration `j` of a block stands: after the `decl.after` typed declarations written before it,
/// before the next, and after the text-kept ones before it. Both lists' own indices compared as one said a block's
/// LATER `width: auto` lost to its earlier `width: var(--w)` whenever there were fewer typed declarations before the
/// `auto` than text-kept ones before the `var()` -- apple.com's `.ac-ls-continue { width: var(--sk-button-width); …;
/// --sk-button-width: 100%; …; width: auto }` made its locale banner's 58 px button 100 % wide.
fn raw_at(decl: &RawDeclaration, j: usize) -> (u32, u32) {
    (decl.after, j as u32 + 1)
}

/// The cascade key of the declaration at `at` ([`typed_at`], [`raw_at`]) of matched rule `m`.
fn rule_key(sheet: &Stylesheet, m: &MatchedRule, at: (u32, u32), important: bool) -> CascadeKey {
    CascadeKey {
        precedence: Precedence::of(sheet.origin, important),
        context: if important { m.context } else { u32::MAX - m.context },
        attached: false,
        layer: if important { u32::MAX - m.layer } else { m.layer },
        proximity: m.proximity,
        specificity: m.specificity,
        order: (m.position, m.rule, at),
    }
}

/// The cascade key of the element's presentational hint `i` (`style::hints`): its own origin, below the
/// author's and above the user agent's, never important.
fn hint_key(i: usize) -> CascadeKey {
    CascadeKey {
        precedence: Precedence::of(Origin::PresentationalHint, false),
        context: u32::MAX,
        attached: false,
        layer: 0,
        proximity: 0,
        specificity: 0,
        order: (u32::MAX, u32::MAX, typed_at(i)),
    }
}

/// The cascade key of the declaration at `at` ([`typed_at`], [`raw_at`]) of the element's own `style` attribute.
fn inline_key(at: (u32, u32), important: bool) -> CascadeKey {
    CascadeKey {
        precedence: Precedence::of(Origin::Author, important),
        context: if important { 0 } else { u32::MAX },
        attached: true,
        layer: 0,
        proximity: 0,
        specificity: INLINE_SPECIFICITY,
        order: (u32::MAX, u32::MAX, at),
    }
}

/// Every typed declaration that applies to the element -- the matched rules', then the `style`
/// attribute's -- with its cascade key and its value as the cascade compares it (a quirks-only value
/// dropped outside quirks mode, a `url()` absolutized against its own sheet). The ONE walk both the
/// cascade's first pass and [`resolve_reverts`] take, so the two cannot disagree about a key.
fn each_declaration<'a>(
    sheets: &'a [Stylesheet],
    matched: &[MatchedRule],
    inline: Option<&InlineStyle<'a>>,
    ctx: &ComputeContext,
    mut visit: impl FnMut(CascadeKey, PropertyId, Cow<'a, str>),
) {
    for m in matched {
        let sheet = &sheets[m.sheet as usize];
        let rule = &sheet.cascade_rules()[m.rule as usize];
        // A `url()` from a sheet with a URL of its own resolves against THAT sheet (see `style::urls`).
        let base = sheet.base_of(rule.base);
        for (i, decl) in sheet.declarations_of(rule).iter().enumerate() {
            let text = sheet.value_of(decl);
            // A unitless length or a hashless colour is valid only in quirks mode; in any
            // other document the declaration does not exist, and an earlier one stands. Chrome 150
            // with `<!doctype html>`: `color: ff0000` computes `rgb(0, 0, 0)`.
            if !ctx.quirks && quirky_value(decl.property, text).is_some() {
                continue;
            }
            let value = match base {
                Some(b) if text.contains("url(") => {
                    crate::style::urls::absolutize(text, b).map_or(Cow::Borrowed(text), Cow::Owned)
                }
                _ => Cow::Borrowed(text),
            };
            visit(rule_key(sheet, m, typed_at(i), decl.important), decl.property, value);
        }
    }
    if let Some(inline) = inline {
        let block = &inline.sheet.declarations()[inline.decls.start as usize..inline.decls.end as usize];
        for (i, decl) in block.iter().enumerate() {
            let text = inline.sheet.value_of(decl);
            if !ctx.quirks && quirky_value(decl.property, text).is_some() {
                continue;
            }
            let key = if (i as u32) < inline.hints { hint_key(i) } else { inline_key(typed_at(i), decl.important) };
            visit(key, decl.property, Cow::Borrowed(text));
        }
    }
}

/// The two CSS-wide keywords that roll the cascade back rather than naming a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Revert {
    /// `revert`: as though the declaring ORIGIN had said nothing.
    Origin,
    /// `revert-layer`: as though the declaring cascade LAYER had said nothing.
    Layer,
}

impl Revert {
    fn of(text: &str) -> Option<Revert> {
        // On the cascade's hot path (every declaration of every matched rule): one byte decides for
        // nearly all of them.
        let t = text.trim_start();
        if !t.as_bytes().first().is_some_and(|b| b.eq_ignore_ascii_case(&b'r')) {
            return None;
        }
        let t = t.trim_end();
        if t.eq_ignore_ascii_case("revert") {
            Some(Revert::Origin)
        } else if t.eq_ignore_ascii_case("revert-layer") {
            Some(Revert::Layer)
        } else {
            None
        }
    }
}

/// The origin a precedence rung belongs to, for `revert`: 0 user agent, 1 user, 2 author. A
/// presentational hint is author-level, and CSS Cascade 5 §7.3.3 counts the animation origin (and so
/// the transition rung above it) as author for this purpose.
fn origin_rank(p: Precedence) -> u8 {
    if p == Precedence::of(Origin::UserAgent, false) || p == Precedence::of(Origin::UserAgent, true) {
        0
    } else if p == Precedence::of(Origin::User, false) || p == Precedence::of(Origin::User, true) {
        1
    } else {
        2
    }
}

/// Replace every winner that is `revert` / `revert-layer` with the cascade's answer without it
/// (CSS Cascade 5 §7.3.3-4), repeating while the replacement rolls back too:
///
/// * `revert` keeps only declarations of an EARLIER origin -- for an author declaration, the user
///   agent's (no user sheets here), so `display: revert` on a `<p>` is `block` and on a `<script>`
///   is `none`;
/// * `revert-layer` keeps the declarations of the same rung in an earlier layer (the `style`
///   attribute sits above every layer, so from there it reaches the author rules); with none, it
///   rolls back like `revert`.
///
/// Nothing left means `unset`. Measured on stock Chrome 150.0.7871.46 (the L19 E6 reset,
/// `:where(:not(html,...)){all:unset;display:revert}` inside `@layer`, which turned every `<script>`
/// and `<style>` on toss.im and css-tricks.com into visible text here).
///
/// A logical declaration is a candidate for the physical property it lands on under `orientation`.
/// What is NOT a candidate, rarer than one page in the corpus: a declaration kept as text (a `var()`
/// value).
fn resolve_reverts<'a>(
    winner: &mut Winners<'a>,
    sheets: &'a [Stylesheet],
    matched: &[MatchedRule],
    inline: Option<&InlineStyle<'a>>,
    ctx: &ComputeContext,
    orientation: Orientation,
) {
    for i in 0..winner.list.len() {
        let property = winner.list[i].0;
        for _ in 0..16 {
            let (key, revert) = {
                let (_, key, value) = &winner.list[i];
                match Revert::of(value) {
                    Some(r) => (*key, r),
                    None => break,
                }
            };
            let layer_below = |k: &CascadeKey| k.precedence == key.precedence && (k.attached, k.layer) < (key.attached, key.layer);
            let origin_below = |k: &CascadeKey| origin_rank(k.precedence) < origin_rank(key.precedence);
            let mut best: Option<(CascadeKey, Cow<'a, str>)> = None;
            let pick = |accept: &dyn Fn(&CascadeKey) -> bool, best: &mut Option<(CascadeKey, Cow<'a, str>)>| {
                each_declaration(sheets, matched, inline, ctx, |k, p, v| {
                    let lands = p == property || (is_logical(p) && to_physical(p, orientation) == Some(property));
                    if lands && accept(&k) && best.as_ref().is_none_or(|(b, _)| k > *b) {
                        *best = Some((k, v));
                    }
                });
            };
            if revert == Revert::Layer {
                pick(&layer_below, &mut best);
            }
            if best.is_none() {
                pick(&origin_below, &mut best);
            }
            match best {
                Some((k, v)) => winner.list[i] = (property, k, v),
                None => {
                    winner.list[i].2 = Cow::Borrowed("unset");
                    break;
                }
            }
        }
    }
}

/// The CSS-wide keywords, which mean the same thing for every property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cssate {
    Value,
    Initial,
    Inherit,
    Unset,
}

impl Cssate {
    fn of(text: &str) -> Cssate {
        let t = text.trim();
        if t.eq_ignore_ascii_case("initial") {
            Cssate::Initial
        } else if t.eq_ignore_ascii_case("inherit") {
            Cssate::Inherit
        // A `revert`/`revert-layer` still standing here rolled back past every origin that set the
        // property (`resolve_reverts`), which is `unset` by definition.
        } else if t.eq_ignore_ascii_case("unset") || Revert::of(t).is_some() {
            Cssate::Unset
        } else {
            Cssate::Value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::matching::Matcher;
    use crate::style::media::MediaContext;
    use crate::style::parse::{parse_inline_style, parse_stylesheet};
    use korvus_dom::Dom;

    /// Style `#target` in `html` against the given sheets and read one property back.
    struct Fixture {
        sheets: Vec<Stylesheet>,
        dom: Dom,
        arena: StyleArena,
    }

    impl Fixture {
        fn new(sheets: Vec<(Origin, &str)>, html: &str) -> Fixture {
            Fixture {
                sheets: sheets.into_iter().map(|(o, css)| parse_stylesheet(o, css)).collect(),
                dom: Dom::parse(html),
                arena: StyleArena::default(),
            }
        }

        fn node(&self, id: &str) -> u32 {
            (0..self.dom.len() as u32)
                .find(|&n| self.dom.get_attribute(n, "id") == Some(id))
                .unwrap_or_else(|| panic!("no #{id}"))
        }

        /// Compute down the ancestor chain so inheritance has something to inherit from.
        fn style_of(&mut self, id: &str) -> StyleKey {
            let node = self.node(id);
            let mut chain = vec![node];
            let mut cur = self.dom.node(node).parent;
            while let Some(n) = cur {
                chain.push(n);
                cur = self.dom.node(n).parent;
            }
            chain.reverse();

            let matcher = Matcher::build(&self.sheets);
            let media = MediaContext::default();
            let mut matched = Vec::new();
            let mut parent = None;
            let mut formatting = ParentFormatting::Flow;
            for n in chain {
                if !matches!(self.dom.node(n).kind, korvus_dom::NodeKind::Element { .. }) {
                    continue;
                }
                matcher.match_element(&self.sheets, &self.dom, n, &media, &mut matched);
                let attr = self.dom.style_for_cascade(n).unwrap_or("").to_string();
                let inline_sheet = parse_inline_style(&attr);
                let inline = (!attr.is_empty()).then(|| InlineStyle {
                    sheet: &inline_sheet,
                    decls: Span::of(0..inline_sheet.declarations().len()),
                    hints: 0,
                });
                let key = compute(
                    &self.sheets,
                    &matched,
                    inline,
                    parent,
                    formatting,
                    &ComputeContext::default(),
                    &mut self.arena,
                );
                formatting = ParentFormatting::of_children(&self.arena, key, formatting);
                parent = Some(key);
            }
            parent.expect("at least one element")
        }

        fn value(&mut self, id: &str, property: PropertyId) -> String {
            let key = self.style_of(id);
            self.arena.value(key, property).to_string()
        }
    }

    fn author(css: &str, html: &str) -> Fixture {
        Fixture::new(vec![(Origin::Author, css)], html)
    }

    // ── cascade layers and nesting ──────────────────────────────────────────────────
    //
    // Rows measured on `/usr/bin/google-chrome` 150.0.7871.46 (the fixture's `.l`, `.n .c` rows).

    #[test]
    fn an_unlayered_rule_beats_a_layered_one_and_important_reverses_it() {
        let mut f = author("@layer base { .l { color: rgb(1, 2, 3) } } .l { }", "<div class=l id=l></div>");
        assert_eq!(f.value("l", PropertyId::Color), "rgb(1, 2, 3)", "an empty unlayered rule declares nothing");
        let mut f = author("@layer a { .x { color: red } } .x { color: blue }", "<div class=x id=x></div>");
        assert_eq!(f.value("x", PropertyId::Color), "rgb(0, 0, 255)");
        let mut f = author("@layer a { .x { color: red !important } } .x { color: blue !important }", "<div class=x id=x></div>");
        assert_eq!(f.value("x", PropertyId::Color), "rgb(255, 0, 0)");
        let mut f = author("@layer a, b; @layer b { .x { color: red } } @layer a { #x { color: blue } }", "<div class=x id=x></div>");
        assert_eq!(f.value("x", PropertyId::Color), "rgb(255, 0, 0)", "the layer order beats specificity");
    }

    #[test]
    fn an_inline_style_beats_a_layered_important_only_when_it_is_important_too() {
        let mut f = author("@layer a { .x { color: red !important } }", "<div class=x id=x style='color: blue'></div>");
        assert_eq!(f.value("x", PropertyId::Color), "rgb(255, 0, 0)");
        let mut f = author("@layer a { .x { color: red } }", "<div class=x id=x style='color: blue'></div>");
        assert_eq!(f.value("x", PropertyId::Color), "rgb(0, 0, 255)");
    }

    #[test]
    fn a_nested_rule_and_a_supports_block_reach_the_cascade() {
        let mut f = author(".n { & .c { color: rgb(7, 8, 9) } } @supports (display:grid) { .s { color: rgb(4, 5, 6) } }",
            "<div class=n><div class=c id=c></div></div><div class=s id=s></div>");
        assert_eq!(f.value("c", PropertyId::Color), "rgb(7, 8, 9)");
        assert_eq!(f.value("s", PropertyId::Color), "rgb(4, 5, 6)");
    }

    // ── logical properties ─────────────────────────────────────────────────────────
    //
    // Every expectation here was read off `/usr/bin/google-chrome` 150.0.7871.46 over CDP with the
    // same declarations (`fx/t5d.html` in the unit's scratch; the rows are quoted beside each).

    #[test]
    fn a_logical_declaration_is_its_physical_property_and_the_later_one_wins() {
        // `margin-left: 1px; margin-inline-start: 2px` -> marginLeft 2px; the reverse order -> 1px.
        let mut f = author(".a { margin-left: 1px; margin-inline-start: 2px } .b { margin-inline-start: 2px; margin-left: 1px }",
            "<div class=a id=a></div><div class=b id=b></div>");
        assert_eq!(f.value("a", PropertyId::MarginLeft), "2px");
        assert_eq!(f.value("b", PropertyId::MarginLeft), "1px");
        // ...and reading the logical name back maps it again.
        assert_eq!(f.value("a", PropertyId::MarginInlineStart), "2px");
        assert_eq!(f.value("b", PropertyId::MarginInlineStart), "1px");
    }

    #[test]
    fn a_more_specific_physical_declaration_still_beats_a_logical_one() {
        let mut f = author("#a { margin-top: 9px } .a { margin-block-start: 3px }", "<div class=a id=a></div>");
        assert_eq!(f.value("a", PropertyId::MarginTop), "9px");
    }

    #[test]
    fn the_writing_mode_and_direction_choose_the_physical_side() {
        // Chrome: writing-mode: vertical-rl -> margin-block-start is marginRight, margin-inline-start
        // is marginTop, padding-inline-end is paddingBottom, inline-size is height.
        let mut f = author("", "<div id=w style='writing-mode: vertical-rl; margin-block-start: 3px; margin-inline-start: 4px; padding-inline-end: 5px; inline-size: 50px'></div>");
        assert_eq!(f.value("w", PropertyId::MarginRight), "3px");
        assert_eq!(f.value("w", PropertyId::MarginTop), "4px");
        assert_eq!(f.value("w", PropertyId::PaddingBottom), "5px");
        assert_eq!(f.value("w", PropertyId::Height), "50px");
        assert_eq!(f.value("w", PropertyId::MarginBlockStart), "3px");
        // Chrome: dir=rtl -> margin-inline-start 7px is marginRight, border-start-start-radius is top-right.
        let mut f = author("[dir=rtl] { direction: rtl }", "<div id=r dir=rtl style='margin-inline-start: 7px; border-start-start-radius: 2px'></div>");
        assert_eq!(f.value("r", PropertyId::MarginRight), "7px");
        assert_eq!(f.value("r", PropertyId::MarginLeft), "0px");
        assert_eq!(f.value("r", PropertyId::BorderTopRightRadius), "2px");
    }

    #[test]
    fn the_direction_is_inherited_into_the_mapping() {
        let mut f = author(".p { direction: rtl } .c { padding-inline-start: 6px }", "<div class=p><span class=c id=c></span></div>");
        assert_eq!(f.value("c", PropertyId::PaddingRight), "6px");
    }

    #[test]
    fn the_logical_box_shorthands_expand_and_map() {
        // Chrome: `margin-inline: 3px 4px; padding-block: 5px; border-block: 2px solid red;
        // inset-inline: 1px 2px` -> 3px 4px 5px 5px 2px rgb(255, 0, 0) 1px 2px, border-left 0px.
        let mut f = author(".o { margin-inline: 3px 4px; padding-block: 5px; border-block: 2px solid red; inset-inline: 1px 2px; position: relative }", "<div class=o id=o></div>");
        assert_eq!(f.value("o", PropertyId::MarginLeft), "3px");
        assert_eq!(f.value("o", PropertyId::MarginRight), "4px");
        assert_eq!(f.value("o", PropertyId::PaddingTop), "5px");
        assert_eq!(f.value("o", PropertyId::PaddingBottom), "5px");
        assert_eq!(f.value("o", PropertyId::BorderTopWidth), "2px");
        assert_eq!(f.value("o", PropertyId::BorderBottomColor), "rgb(255, 0, 0)");
        assert_eq!(f.value("o", PropertyId::Left), "1px");
        assert_eq!(f.value("o", PropertyId::Right), "2px");
        assert_eq!(f.value("o", PropertyId::BorderLeftWidth), "0px");
    }

    #[test]
    fn a_property_nobody_set_reads_as_its_initial_value() {
        let mut f = author("", "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 0)");
        assert_eq!(f.value("target", PropertyId::Width), "auto");
    }

    #[test]
    fn a_more_specific_selector_wins_however_late_the_other_one_is() {
        let mut f = author(
            "#target { color: red } p.lede { color: blue } p { color: teal }",
            "<p id='target' class='lede'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
    }

    #[test]
    fn equal_specificity_is_broken_by_document_order_and_later_wins() {
        let mut f = author("p { color: red } p { color: blue }", "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 255)");
    }

    #[test]
    fn important_beats_specificity_within_an_origin() {
        let mut f = author(
            "#target { color: red } p { color: blue !important }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 255)");
    }

    // The origin ladder, and the fact that `!important` turns it upside down.
    #[test]
    fn the_author_beats_the_ua_but_not_a_user_important_rule() {
        let sheets = vec![
            (Origin::UserAgent, "p { color: black }"),
            (Origin::User, "p { color: green }"),
            (Origin::Author, "p { color: red }"),
        ];
        let mut f = Fixture::new(sheets, "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");

        let sheets = vec![
            (Origin::User, "p { color: green !important }"),
            (Origin::Author, "p { color: red !important }"),
        ];
        let mut f = Fixture::new(sheets, "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 128, 0)");
    }

    // A style attribute is its own cascade step above all author rules, not a very specific
    // selector -- an `!important` id rule still beats it.
    #[test]
    fn an_inline_style_beats_every_selector_but_not_an_important_one() {
        let mut f = author("#target { color: red }", "<p id='target' style='color: blue'>x</p>");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 255)");

        let mut f = author(
            "#target { color: red !important }",
            "<p id='target' style='color: blue'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
    }

    #[test]
    fn an_inherited_property_reaches_a_child_that_never_set_it() {
        let mut f = author(
            "#outer { color: red; width: 500px }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
        // The correction to D6, from the other end: sharing the inherited half must not drag the
        // parent's own values along.
        assert_eq!(f.value("target", PropertyId::Width), "auto");
    }

    #[test]
    fn inheritance_passes_through_elements_that_set_nothing() {
        let mut f = author(
            "#outer { color: red }",
            "<div id='outer'><span><span><em id='target'>x</em></span></span></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
    }

    #[test]
    fn the_css_wide_keywords_mean_what_they_say() {
        let mut f = author(
            "#outer { color: red } #target { color: inherit }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");

        let mut f = author(
            "#outer { color: red } #target { color: initial }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 0)");

        // `unset` is inherit for an inherited property...
        let mut f = author(
            "#outer { color: red } #target { color: unset }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
        // ...and initial for one that is not.
        let mut f = author("#target { width: 5px; width: unset }", "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::Width), "auto");
    }

    // An inherited property reset to initial must stop the parent's value reaching the subtree,
    // which means the inherited half really does change -- the reuse shortcut must not fire.
    #[test]
    fn resetting_an_inherited_property_stops_it_at_that_element() {
        let mut f = author(
            "#outer { color: red } #mid { color: initial }",
            "<div id='outer'><div id='mid'><p id='target'>x</p></div></div>",
        );
        assert_eq!(f.value("mid", PropertyId::Color), "rgb(0, 0, 0)");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 0)");
    }

    // Two elements with the same style are one entry in the arena. This is the claim the whole
    // design rests on, so it is measured rather than assumed.
    #[test]
    fn a_list_of_identical_items_interns_to_one_style() {
        let mut items = String::new();
        for i in 0..200 {
            items.push_str(&format!("<li id='i{i}' class='item'>x</li>"));
        }
        let mut f = author(".item { color: red; margin: 4px }", &format!("<ul>{items}</ul>"));
        for i in 0..200 {
            f.style_of(&format!("i{i}"));
        }
        let (inherited, own) = f.arena.len();
        assert_eq!(own, 2, "the empty set plus the one the items share");
        assert_eq!(inherited, 2, "the empty set plus the one colour");
    }

    // Elements a stylesheet never mentions take the parent's inherited half by identity: no hash,
    // no allocation, nothing added to the arena.
    #[test]
    fn unstyled_wrappers_add_nothing_to_the_arena() {
        let mut f = author(
            "#outer { color: red }",
            "<div id='outer'><span><span><span><em id='target'>x</em></span></span></span></div>",
        );
        f.style_of("target");
        let (inherited, own) = f.arena.len();
        assert_eq!(inherited, 2, "the empty set plus outer's colour, shared all the way down");
        assert_eq!(own, 1, "nothing set a non-inherited property, so only the empty set");
    }

    // The three properties everything else is written in terms of, and the order they need.
    #[test]
    fn em_resolves_against_this_elements_font_size_which_resolves_against_its_parents() {
        let mut f = author(
            "#outer { font-size: 20px } #target { margin-top: 2em }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::MarginTop), "40px");

        // And a font-size in em is against the *parent's*, so the margin below it follows the new one.
        let mut f = author(
            "#outer { font-size: 20px } #target { font-size: 1.5em; margin-top: 1em }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::FontSize), "30px");
        assert_eq!(f.value("target", PropertyId::MarginTop), "30px");
    }

    #[test]
    fn current_color_follows_the_colour_the_same_element_computed() {
        let mut f = author(
            "#target { color: red; border-top-color: currentColor }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::BorderTopColor), "rgb(255, 0, 0)");
    }

    // `currentcolor` computes to itself and is read against the READING element's `color` -- initial, inherited or
    // `inherit`ed (Chrome 152, `cc.html`, 2026-10-23: under `color: red` an unset `border-top-color` reads red, and a
    // `color: blue` child of a `currentcolor` parent reads blue).
    #[test]
    fn current_color_is_read_against_the_reading_elements_colour() {
        let mut f = author(
            "#p { color: red; border-top-color: currentcolor; -webkit-text-fill-color: currentcolor; fill: currentcolor } \
             #c { color: blue; border-top-color: inherit; animation-name: currentcolor } #q { color: red; caret-color: auto } \
             #e { color: blue }",
            "<div id='p'><p id='c'>x</p><p id='d'>y</p></div><div id='q'><span id='e'>z</span></div>",
        );
        assert_eq!(f.value("p", PropertyId::BorderTopColor), "rgb(255, 0, 0)");
        assert_eq!(f.value("c", PropertyId::BorderTopColor), "rgb(0, 0, 255)");
        assert_eq!(f.value("c", PropertyId::WebkitTextFillColor), "rgb(0, 0, 255)");
        assert_eq!(f.value("c", PropertyId::Fill), "rgb(0, 0, 255)");
        assert_eq!(f.value("c", PropertyId::OutlineColor), "rgb(0, 0, 255)", "initial");
        assert_eq!(f.value("d", PropertyId::TextDecorationColor), "rgb(255, 0, 0)", "initial, red inherited");
        assert_eq!(f.value("e", PropertyId::CaretColor), "rgb(0, 0, 255)", "`auto` inherited");
        assert_eq!(f.value("e", PropertyId::FloodColor), "rgb(0, 0, 0)", "not every colour starts as currentcolor");
        assert_eq!(f.value("c", PropertyId::AnimationName), "currentcolor", "a name, not a colour");
        // what a transition compares: the keyword on both sides, though the colours read differ
        let (p, c) = (f.style_of("p"), f.style_of("c"));
        assert_ne!(f.arena.value(p, PropertyId::BorderTopColor), f.arena.value(c, PropertyId::BorderTopColor));
        assert_eq!(f.arena.computed_text(p, PropertyId::BorderTopColor), f.arena.computed_text(c, PropertyId::BorderTopColor));
        assert_eq!(f.arena.computed_text(c, PropertyId::OutlineColor), "currentcolor");
    }

    // Chrome 152 (`bw.html`, 2026-10-23): a border side with no style reads `0px` whatever its width, a style alone
    // gives it `medium`, and `inherit` takes the parent's width as it was computed, not as it reads.
    #[test]
    fn a_border_side_without_a_style_has_no_width() {
        let mut f = author(
            "#a { border-top-style: solid } #b { border-top-width: 10px } #c { border-top-width: 10px; border-top-style: hidden } \
             #k { border-inline-start-width: 7px } #h { border-top-width: inherit; border-top-style: solid }",
            "<div id='a'></div><div id='b'><p id='h'>x</p></div><div id='c'></div><div id='k'></div>",
        );
        assert_eq!(f.value("a", PropertyId::BorderTopWidth), "3px");
        assert_eq!(f.value("a", PropertyId::BorderLeftWidth), "0px");
        assert_eq!(f.value("b", PropertyId::BorderTopWidth), "0px");
        assert_eq!(f.value("c", PropertyId::BorderTopWidth), "0px");
        assert_eq!(f.value("k", PropertyId::BorderLeftWidth), "0px");
        assert_eq!(f.value("k", PropertyId::BorderInlineStartWidth), "0px");
        assert_eq!(f.value("h", PropertyId::BorderTopWidth), "10px", "inherits the width the parent computed");
        // ...and that is the USED width alone: the computed one keeps the width, as `computedStyleMap()` reads it and
        // a transition runs from it (Chrome 152, `bwt.html` / `vtr2.html`: `10px` and `3px` for `b` and `a`'s left)
        let (a, b, k) = (f.style_of("a"), f.style_of("b"), f.style_of("k"));
        assert_eq!(f.arena.computed_value(b, PropertyId::BorderTopWidth), "10px");
        assert_eq!(f.arena.computed_text(a, PropertyId::BorderLeftWidth), "3px");
        assert_eq!(f.arena.computed_value(k, PropertyId::BorderInlineStartWidth), "7px");
        assert_eq!(crate::style::computed::as_computed(|| f.arena.value(b, PropertyId::BorderTopWidth).to_string()), "10px");
        assert_eq!(f.arena.value(b, PropertyId::BorderTopWidth), "0px", "as_computed ends with its call");
    }

    // A shadow's `currentcolor` -- written, or no colour at all -- computes to the keyword and inherits as itself: a
    // `color: blue` child reads its own colour (Chrome 152, `c1rev.html`, 2026-10-23), and a `color` change is no
    // change of the shadow to a transition.
    #[test]
    fn a_shadow_takes_the_reading_elements_colour() {
        let mut f = author(
            "#p { color: red; text-shadow: 1px 1px; box-shadow: 2px 2px currentcolor } #c { color: blue; box-shadow: inherit }",
            "<div id='p'><p id='c'>x</p></div>",
        );
        let (p, c) = (f.style_of("p"), f.style_of("c"));
        assert_eq!(f.arena.computed_text(c, PropertyId::TextShadow), "currentcolor 1px 1px 0px");
        assert_eq!(f.arena.computed_text(p, PropertyId::BoxShadow), f.arena.computed_text(c, PropertyId::BoxShadow));
        assert_eq!(f.arena.transition_value(c, PropertyId::TextShadow), "rgb(0, 0, 255) 1px 1px 0px");
        assert_eq!(f.arena.transition_value(p, PropertyId::BoxShadow), "rgb(255, 0, 0) 2px 2px 0px 0px");
    }

    // An inherited value arrives already computed. Resolving it a second time would resolve its
    // `em` against a font that is no longer the one it was written against.
    #[test]
    fn an_inherited_length_is_not_resolved_twice() {
        let mut f = author(
            "#outer { font-size: 20px; letter-spacing: 1em } #target { font-size: 40px }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("outer", PropertyId::LetterSpacing), "20px");
        assert_eq!(
            f.value("target", PropertyId::LetterSpacing),
            "20px",
            "inherited as 20px, not re-resolved to 40px"
        );
    }

    #[test]
    fn rem_is_the_root_font_size_wherever_it_is_written() {
        let mut f = author(
            "#outer { font-size: 40px } #target { margin-top: 2rem }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        // The fixture computes with the default context, whose root font size is 16px.
        assert_eq!(f.value("target", PropertyId::MarginTop), "32px");
    }

    #[test]
    fn bolder_steps_from_whatever_the_parent_computed() {
        let mut f = author(
            "#outer { font-weight: bold } #target { font-weight: bolder }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("outer", PropertyId::FontWeight), "700");
        assert_eq!(f.value("target", PropertyId::FontWeight), "900");
    }

    // -------- var() --------

    // The measurement that put this in v1: 28.8% of real declarations look like this.
    #[test]
    fn a_var_reference_resolves_to_the_custom_property_it_names() {
        let mut f = author(
            "#target { --brand: red; color: var(--brand) }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
    }

    #[test]
    fn a_custom_property_set_on_an_ancestor_reaches_the_reference() {
        let mut f = author(
            "#outer { --brand: blue } #target { color: var(--brand) }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 255)");
    }

    // This resolves an element's OWN custom properties only and reads the parent's in place. A
    // name declared here that does not resolve (a cycle) is guaranteed-invalid HERE -- a `var()` of
    // it takes its fallback -- and never the parent's value; a name not declared here is the
    // parent's. Chrome 150 (measured 2026-09-29): `color` is green on #c, red on #d.
    #[test]
    fn an_own_custom_property_in_a_cycle_does_not_fall_back_to_the_parents() {
        let mut f = author(
            "#p { --a: red } #c { --a: var(--b); --b: var(--a); color: var(--a, green) } #d { color: var(--a, green) }",
            "<div id='p'><p id='c'>x</p><p id='d'>y</p></div>",
        );
        assert_eq!(f.value("c", PropertyId::Color), "rgb(0, 128, 0)");
        assert_eq!(f.value("d", PropertyId::Color), "rgb(255, 0, 0)");
    }

    // The reason a `var()` declaration cannot be expanded at parse time: its arity is not known
    // until the value is.
    #[test]
    fn a_shorthand_written_with_var_expands_once_the_value_is_known() {
        let mut f = author("#target { --gap: 4px; margin: var(--gap) }", "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::MarginTop), "4px");
        assert_eq!(f.value("target", PropertyId::MarginLeft), "4px");

        // The same declaration, a different value, a different number of longhands.
        let mut f = author(
            "#target { --gap: 1px 2px; margin: var(--gap) }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::MarginTop), "1px");
        assert_eq!(f.value("target", PropertyId::MarginRight), "2px");
    }

    #[test]
    fn a_reference_can_be_part_of_a_larger_value() {
        let mut f = author(
            "#target { --w: 2px; border-top: var(--w) solid red }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::BorderTopWidth), "2px");
        assert_eq!(f.value("target", PropertyId::BorderTopStyle), "solid");
        assert_eq!(f.value("target", PropertyId::BorderTopColor), "rgb(255, 0, 0)");
    }

    // One block, one order: the declaration written later wins, kept as text or parsed. The two lists' own
    // indices compared as one let the `var()` win here (apple.com's locale banner button, `raw_at`).
    #[test]
    fn a_later_declaration_beats_an_earlier_var_in_its_block() {
        let mut f = author(
            "#a { --x: 1; --y: 2; width: var(--w); --w: 100px; width: auto } #b { width: 10px; --x: 1; width: var(--w); --w: 20px }",
            "<p id='a'>x</p><p id='b'>y</p><p id='c' style='--x: 1; --y: 2; width: var(--w); --w: 5px; width: 7px'>z</p>",
        );
        assert_eq!(f.value("a", PropertyId::Width), "auto");
        assert_eq!(f.value("b", PropertyId::Width), "20px");
        assert_eq!(f.value("c", PropertyId::Width), "7px");
    }

    #[test]
    fn a_fallback_covers_a_property_nobody_set() {
        let mut f = author("#target { color: var(--missing, green) }", "<p id='target'>x</p>");
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 128, 0)");
    }

    // Invalid at computed-value time is not the same as dropped: the declaration still *won* the
    // cascade, so the earlier rule does not take over -- what it computes to is `unset`.
    #[test]
    fn an_unresolvable_reference_computes_to_unset_rather_than_letting_an_earlier_rule_win() {
        let mut f = author(
            "#outer { color: blue } p { color: red } #target { color: var(--missing) }",
            "<div id='outer'><p id='target'>x</p></div>",
        );
        assert_eq!(
            f.value("target", PropertyId::Color),
            "rgb(0, 0, 255)",
            "unset on an inherited property is inherit -- blue from #outer, not red from p"
        );

        // And on a property that does not inherit, `unset` is `initial`.
        let mut f = author(
            "p { width: 5px } #target { width: var(--missing) }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Width), "auto");
    }

    #[test]
    fn a_custom_property_may_be_written_in_terms_of_another() {
        let mut f = author(
            "#target { --a: var(--b); --b: teal; color: var(--a) }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 128, 128)");
    }

    // A custom property in a cycle computes to the guaranteed-invalid value, which is also its
    // *initial* value -- so a `var()` naming it behaves as though it were never set, and the
    // fallback applies. Treating the cycle as an outright failure would swallow the fallback.
    #[test]
    fn a_cycle_between_custom_properties_leaves_them_unset_so_a_fallback_still_applies() {
        let mut f = author(
            "#target { --a: var(--b); --b: var(--a); color: var(--a, purple) }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(128, 0, 128)");

        // With no fallback there is nothing to fall back to, so the declaration is invalid at
        // computed-value time and the property is `unset`.
        let mut f = author(
            "#target { --a: var(--b); --b: var(--a); color: var(--a) }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(0, 0, 0)");
    }

    #[test]
    fn a_reference_in_an_inline_style_resolves_against_the_cascade() {
        let mut f = author(
            "#outer { --brand: red }",
            "<div id='outer'><p id='target' style='color: var(--brand)'>x</p></div>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)");
    }

    // A later declaration of the custom property wins, and the reference sees the winner -- not the
    // value that was in scope where the reference was written.
    #[test]
    fn a_reference_sees_the_cascaded_value_not_the_declaration_order() {
        let mut f = author(
            "#target { color: var(--c); --c: red } p { --c: blue }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::Color), "rgb(255, 0, 0)", "#target beats p");
    }

    #[test]
    fn a_shorthand_cascades_as_the_longhands_it_became() {
        let mut f = author(
            "p { margin: 4px } #target { margin-top: 9px }",
            "<p id='target'>x</p>",
        );
        assert_eq!(f.value("target", PropertyId::MarginTop), "9px");
        assert_eq!(f.value("target", PropertyId::MarginLeft), "4px");
    }
}
