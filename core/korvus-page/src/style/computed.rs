//! The computed style, and the arena that keeps one copy of each distinct one.
//!
//! # Why nodes point at styles instead of owning them
//!
//! A page's elements vastly outnumber its distinct computed styles. Two hundred `<li>`s in a list
//! compute to one style; a table of a thousand cells computes to two or three. Storing a style per
//! node stores the same answer over and over, which is why Chrome refcounts `ComputedStyle` and
//! shares it between siblings. Here a node holds a [`StyleKey`] — eight bytes — and the arena holds
//! the styles.
//!
//! An arena index rather than an `Rc`: no refcount traffic, no pointer chasing, and a `Vec` is
//! contiguous, which matters because layout walks the tree several times over.
//!
//! # Why a style is two halves
//!
//! Design D6 says a node with no matched rules and no inline style can point at its *parent's*
//! style, because its inherited properties equal the parent's and its own are all initial. The
//! first half of that is true and the conclusion does not follow: the parent's style also carries
//! the parent's non-inherited values, so a child sharing it wholesale would inherit its parent's
//! `width`.
//!
//! Splitting the style into an inherited half and a non-inherited half makes the shortcut correct,
//! and it is what Blink does for the same reason. A node with nothing of its own takes the parent's
//! inherited half *by identity* — no hashing, no allocation, no copy — and the shared "everything
//! initial" own half. That is the common case in any real document: text nodes, `<span>` wrappers,
//! and every element a stylesheet never mentions.

use crate::style::property::{PropertyId, COUNT};

/// Which interned halves a node's style is made of. Eight bytes per node, and the only per-node
/// style data there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StyleKey {
    pub inherited: u32,
    pub own: u32,
}

impl StyleKey {
    /// The style of an element that nothing applies to: no inherited values, everything initial.
    /// Index 0 of both arenas, which [`StyleArena::default`] guarantees is the empty set.
    pub const INITIAL: StyleKey = StyleKey { inherited: 0, own: 0 };
}

/// A custom property that is INVALID AT COMPUTED-VALUE TIME on an element -- in a `var()` cycle, or a `var()` with
/// no fallback to an undefined name. It computes to the guaranteed-invalid value, which must NOT
/// inherit the parent's: an entry holding this marker blocks the merge, and every reader sees no value.
pub const GUARANTEED_INVALID: &str = "\u{0}guaranteed-invalid";

/// The computed value a colour property keeps for `currentcolor`, which [`StyleArena::value`] reads as the element's
/// own `color`.
pub const CURRENT_COLOR: &str = "currentcolor";

/// The border style a border side's width answers to ([`StyleArena::value`]), a logical side's as a logical one
/// (both map through the same writing mode).
fn side_style(property: PropertyId) -> Option<PropertyId> {
    use PropertyId::*;
    Some(match property {
        BorderTopWidth => BorderTopStyle,
        BorderRightWidth => BorderRightStyle,
        BorderBottomWidth => BorderBottomStyle,
        BorderLeftWidth => BorderLeftStyle,
        BorderBlockStartWidth => BorderBlockStartStyle,
        BorderBlockEndWidth => BorderBlockEndStyle,
        BorderInlineStartWidth => BorderInlineStartStyle,
        BorderInlineEndWidth => BorderInlineEndStyle,
        _ => return None,
    })
}

thread_local! {
    /// Set while [`as_computed`] runs.
    static AS_COMPUTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` with [`StyleArena::value`] answering the COMPUTED value ([`StyleArena::computed_value`]) where it would answer
/// the used one: what `computedStyleMap()` reads through the page's one-property read, which is the used value's.
/// Chrome 152 (`bwt.html`, 2026-10-23): `computedStyleMap().get('border-left-width')` under `border-left-width: 10px`
/// and no style is `CSSUnitValue` `10px`, and `3px` with nothing set, where `getComputedStyle` reads `0px` for both.
pub fn as_computed<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            AS_COMPUTED.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(AS_COMPUTED.with(|c| c.replace(true)));
    f()
}

/// `text`, a computed shadow list (`box-shadow`, `text-shadow`) or filter list (`filter`, `backdrop-filter`: its
/// `drop-shadow()`s), with each shadow's `currentcolor` read as `color` -- `None` when it holds none, or `property`
/// keeps no shadow. A shadow with no colour of its own computes to `currentcolor` and is read against the READING
/// element's `color`, as a colour property's is (`StyleArena::computed_value`): Chrome 152 (`c1rev.html`, `fix1.html`,
/// 2026-10-23) reads an inherited `text-shadow: 1px 1px` under `color: blue` as `rgb(0, 0, 255) 1px 1px 0px` though
/// the parent that declared it is `color: red`, and `box-shadow: inherit` / `filter: inherit` the same way, in
/// `getComputedStyle` and in `computedStyleMap()` alike.
pub fn current_color_in(property: PropertyId, text: &str, color: impl FnOnce() -> String) -> Option<String> {
    use PropertyId::*;
    if !matches!(property, BoxShadow | TextShadow | Filter | BackdropFilter) || !text.contains(CURRENT_COLOR) {
        return None;
    }
    let color = color();
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 16);
    let (mut i, mut copied) = (0, 0);
    while i < b.len() {
        // a `url()` and a string are passed over whole: a filter's `url(#currentcolor)` names no colour
        if b[i..].len() >= 4 && b[i..i + 4].eq_ignore_ascii_case(b"url(") {
            i += 4;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                match b[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    b'\\' => i += 1,
                    _ => {}
                }
                i += 1;
            }
            continue;
        }
        if matches!(b[i], b'"' | b'\'') {
            let q = b[i];
            i += 1;
            while i < b.len() && b[i] != q {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            continue;
        }
        let word = |at: usize| at == 0 || matches!(b[at - 1], b' ' | b',' | b'(');
        let end = i + CURRENT_COLOR.len();
        if word(i) && b[i..].starts_with(CURRENT_COLOR.as_bytes()) && (end == b.len() || matches!(b[end], b' ' | b',' | b')')) {
            out.push_str(&text[copied..i]);
            out.push_str(&color);
            i = end;
            copied = end;
            continue;
        }
        i += 1;
    }
    if copied == 0 {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// A set of property values, sorted by property.
///
/// Sorted rather than hashed because these are small (a handful of entries in the common case, a
/// few dozen at worst), because a sorted `Vec` hashes and compares as a unit — which is what makes
/// interning cheap — and because iteration in property order is what `getComputedStyle` wants
/// anyway.
///
/// Names and values are `Arc<str>`: an element that sets any inherited property builds
/// its inherited half over its parent's (`inherit_from`), and a page whose `:root` declares hundreds
/// of custom properties copied every one of them, a `Box<str>` pair each, per such element --
/// sharing them is a refcount, not an allocation.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct PropertySet {
    entries: Vec<(PropertyId, std::sync::Arc<str>)>,
    /// Custom properties, sorted by name. Kept apart because they have no [`PropertyId`] -- their
    /// names are open-ended, so they cannot be a dense index. They are always inherited, so in
    /// practice this is only ever non-empty on the inherited half.
    custom: Customs,
    /// The engine's own values kept beside them, under names that start with U+0000 (`FontSizeState::KEY`), which no
    /// page can write or read: apart from the shared block, so an element whose font size moves -- every heading --
    /// does not copy every token of the page to hold its own.
    engine: Vec<(std::sync::Arc<str>, std::sync::Arc<str>)>,
}

/// A set's custom properties, sorted by name, as one shared block with its hash. Nearly every element inherits all of
/// its parent's -- a page declares its tokens on `:root` -- so its inherited half holds the parent's block, one
/// refcount, where it copied every pair (a refcount each), hashed them all again to intern the half and dropped the
/// copy when the half was interned already: lowes.com's few hundred tokens made the cascade 11% of its main thread
/// (`inherit_from`, the pairs' drop and the set's hash its three leaves).
#[derive(Debug, Default, Clone)]
struct Customs(Option<std::sync::Arc<CustomBlock>>);

/// The pairs and, once something asked, their hash: a set that writes its custom properties one by one (`:root`'s few
/// hundred) writes them into its own block in place ([`PropertySet::set_custom`]) and is hashed once, when interned.
#[derive(Debug)]
struct CustomBlock {
    items: Vec<(std::sync::Arc<str>, std::sync::Arc<str>)>,
    hash: std::sync::OnceLock<u64>,
}

impl Clone for CustomBlock {
    fn clone(&self) -> Self {
        CustomBlock { items: self.items.clone(), hash: std::sync::OnceLock::new() }
    }
}

impl CustomBlock {
    fn hash(&self) -> u64 {
        *self.hash.get_or_init(|| {
            use std::hash::BuildHasher;
            korvus_dom::FxHashMap::<u8, u8>::default().hasher().hash_one(&self.items)
        })
    }
}

impl Customs {
    fn of(items: Vec<(std::sync::Arc<str>, std::sync::Arc<str>)>) -> Self {
        if items.is_empty() {
            return Customs(None);
        }
        Customs(Some(std::sync::Arc::new(CustomBlock { items, hash: std::sync::OnceLock::new() })))
    }

    fn items(&self) -> &[(std::sync::Arc<str>, std::sync::Arc<str>)] {
        self.0.as_ref().map_or(&[], |b| &b.items)
    }

    /// This set's own pairs to write: its block when no other set shares it, else a copy of it first.
    fn items_mut(&mut self) -> &mut Vec<(std::sync::Arc<str>, std::sync::Arc<str>)> {
        let block = self.0.get_or_insert_with(|| std::sync::Arc::new(CustomBlock { items: Vec::new(), hash: std::sync::OnceLock::new() }));
        let b = std::sync::Arc::make_mut(block);
        b.hash = std::sync::OnceLock::new();
        &mut b.items
    }
}

impl PartialEq for Customs {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                std::sync::Arc::ptr_eq(a, b) || (a.items.len() == b.items.len() && a.hash() == b.hash() && a.items == b.items)
            }
            _ => false,
        }
    }
}

impl Eq for Customs {}

impl std::hash::Hash for Customs {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.as_ref().map_or(0, |b| b.hash()));
    }
}

impl PropertySet {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.custom.items().is_empty() && self.engine.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len() + self.custom.items().len() + self.engine.len()
    }

    /// The value set for `property`, or `None` if this set does not mention it.
    pub fn get(&self, property: PropertyId) -> Option<&str> {
        self.entries
            .binary_search_by_key(&property, |(p, _)| *p)
            .ok()
            .map(|i| self.entries[i].1.as_ref())
    }

    /// Set a value, keeping the entries sorted.
    pub fn set(&mut self, property: PropertyId, value: &str) {
        match self.entries.binary_search_by_key(&property, |(p, _)| *p) {
            Ok(i) => self.entries[i].1 = value.into(),
            Err(i) => self.entries.insert(i, (property, value.into())),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (PropertyId, &str)> {
        self.entries.iter().map(|(p, v)| (*p, v.as_ref()))
    }

    /// A custom property's value. Names are case-*sensitive*: `--Foo` and `--foo` are two
    /// properties, unlike every other name in CSS.
    pub fn custom(&self, name: &str) -> Option<&str> {
        let items = if name.starts_with('\u{0}') { &self.engine[..] } else { self.custom.items() };
        items
            .binary_search_by(|(n, _)| n.as_ref().cmp(name))
            .ok()
            .map(|i| items[i].1.as_ref())
            .filter(|v| *v != GUARANTEED_INVALID)
    }

    pub fn set_custom(&mut self, name: &str, value: &str) {
        let items = if name.starts_with('\u{0}') { &mut self.engine } else { self.custom.items_mut() };
        match items.binary_search_by(|(n, _)| n.as_ref().cmp(name)) {
            Ok(i) => items[i].1 = value.into(),
            Err(i) => items.insert(i, (name.into(), value.into())),
        }
    }

    /// The same custom properties as `other`, invalid ones included (whether a container's
    /// `style()` answers can have moved).
    pub fn same_custom(&self, other: &PropertySet) -> bool {
        self.custom == other.custom
    }

    pub fn custom_iter(&self) -> impl Iterator<Item = (&str, &str)> {
        // a name starting with U+0000 is the engine's own (`FontSizeState::KEY`): no page can write or read one
        self.custom
            .items()
            .iter()
            .map(|(n, v)| (n.as_ref(), v.as_ref()))
            .filter(|(n, v)| *v != GUARANTEED_INVALID && !n.starts_with('\u{0}'))
    }

    /// Complete this set with every property and custom property of `parent` it does not set itself:
    /// the two sorted halves merged in one pass, this set's values winning, `parent`'s shared rather
    /// than copied. The same set as looking each of `parent`'s entries up here and inserting the
    /// missing ones (the loop this replaces), without its per-entry insert.
    pub fn inherit_from(&mut self, parent: &PropertySet) {
        self.entries = merge_sorted(&self.entries, &parent.entries, |a, b| a.0.cmp(&b.0));
        // the parent's block itself when this set has none of its own -- the common case -- and the merge otherwise
        self.custom = match (self.custom.items().is_empty(), parent.custom.items().is_empty()) {
            (_, true) => self.custom.clone(),
            (true, false) => parent.custom.clone(),
            (false, false) => Customs::of(merge_sorted(self.custom.items(), parent.custom.items(), |a, b| a.0.cmp(&b.0))),
        };
        if !parent.engine.is_empty() {
            self.engine = merge_sorted(&self.engine, &parent.engine, |a, b| a.0.cmp(&b.0));
        }
    }
}

/// `own` and `parent`, each sorted by `cmp`, merged into one sorted run; on a tie `own`'s entry wins.
fn merge_sorted<T: Clone>(own: &[T], parent: &[T], cmp: impl Fn(&T, &T) -> std::cmp::Ordering) -> Vec<T> {
    let mut out = Vec::with_capacity(own.len() + parent.len());
    let (mut i, mut j) = (0, 0);
    while i < own.len() && j < parent.len() {
        match cmp(&own[i], &parent[j]) {
            std::cmp::Ordering::Less => {
                out.push(own[i].clone());
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(parent[j].clone());
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(own[i].clone());
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&own[i..]);
    out.extend_from_slice(&parent[j..]);
    out
}

/// One copy of each distinct half, and a map back from a half to its index.
///
/// Index 0 of each is the empty set, so [`StyleKey::INITIAL`] is always valid and an unstyled
/// document interns nothing at all.
#[derive(Debug)]
pub struct StyleArena {
    inherited: Vec<PropertySet>,
    own: Vec<PropertySet>,
    inherited_ids: korvus_dom::FxHashMap<PropertySet, u32>,
    own_ids: korvus_dom::FxHashMap<PropertySet, u32>,
    /// What the cascade parsed out of each inherited half for its children, by the half's id --
    /// filled on first use by `compute::parent_basis`. Safe to keep for the arena's
    /// lifetime for the same reason the ids are: a half is interned once and never rewritten.
    basis: Vec<Option<InheritedBasis>>,
    /// How many inherited halves have been offered to [`StyleArena::intern_inherited`], new or not
    /// -- each one a whole half built, merged and hashed. A count for tests.
    inherited_interns: u64,
    /// The document's `@property` registrations, which the cascade types custom
    /// properties against and which answer for a registered property no half carries. Set by the page
    /// whenever its sheets change; an `Arc` so the cascade can hold it while it interns.
    pub registry: std::sync::Arc<crate::style::registered::PropertyRegistry>,
}

/// The three values a child's cascade parses out of its parent's inherited half. See
/// `compute::parent_basis`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InheritedBasis {
    pub font_size: Option<f32>,
    /// The parent's [`crate::style::resolve::FontSizeState`].
    pub size_state: Option<crate::style::resolve::FontSizeState>,
    pub weight: Option<f32>,
    pub color: Option<crate::value::Rgba>,
}

impl Default for StyleArena {
    fn default() -> Self {
        let mut arena = StyleArena {
            inherited: Vec::new(),
            own: Vec::new(),
            inherited_ids: Default::default(),
            own_ids: Default::default(),
            basis: Vec::new(),
            inherited_interns: 0,
            registry: Default::default(),
        };
        assert_eq!(arena.intern_inherited(PropertySet::default()), 0);
        assert_eq!(arena.intern_own(PropertySet::default()), 0);
        arena
    }
}

impl StyleArena {
    pub fn intern_inherited(&mut self, set: PropertySet) -> u32 {
        self.inherited_interns += 1;
        intern(&mut self.inherited, &mut self.inherited_ids, set)
    }

    /// See the field. The two empty halves the arena starts with are counted.
    pub fn inherited_interns(&self) -> u64 {
        self.inherited_interns
    }

    pub fn intern_own(&mut self, set: PropertySet) -> u32 {
        intern(&mut self.own, &mut self.own_ids, set)
    }

    pub fn inherited(&self, id: u32) -> &PropertySet {
        &self.inherited[id as usize]
    }

    /// The banked [`InheritedBasis`] of inherited half `id`, if the cascade has parsed it yet.
    pub fn basis(&self, id: u32) -> Option<InheritedBasis> {
        self.basis.get(id as usize).copied().flatten()
    }

    pub fn set_basis(&mut self, id: u32, basis: InheritedBasis) {
        let i = id as usize;
        if self.basis.len() <= i {
            self.basis.resize(i + 1, None);
        }
        self.basis[i] = Some(basis);
    }

    pub fn own(&self, id: u32) -> &PropertySet {
        &self.own[id as usize]
    }

    /// How many distinct halves exist. The point of the arena is that this stays far below the node
    /// count, so it is worth being able to check.
    pub fn len(&self) -> (usize, usize) {
        (self.inherited.len(), self.own.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == (1, 1) // the two empty sets are always there
    }

    /// A style's value for one property, as it is USED: what layout, paint and `getComputedStyle` read.
    ///
    /// Falls back to the initial value, because the sets hold only what was *set*: a style that
    /// mentions nothing is not an absent style, it is an element with every property at its initial
    /// value, and `getComputedStyle` has to answer for all of them either way.
    ///
    /// A border side whose style is `none` or `hidden` has no width here, whatever width it computed (Blink's
    /// `ComputedStyle::BorderTopWidth` answers 0 then, and `getComputedStyle` and layout read that): Chrome 152 (`bw.html`,
    /// 2026-10-23) reads `border-top-width: 10px` alone as `0px` and lays out no border, and `border-top-style: solid`
    /// alone as `3px` (`medium`) with a 3px border. Only here: the computed value keeps the width
    /// ([`StyleArena::computed_value`]) -- what `computedStyleMap()` reads (Chrome 152: `CSSUnitValue` `10px` for that
    /// first side, `3px` for a side nothing set; `bwt.html`), what a transition starts from and compares (a style
    /// toggled under `transition: all` starts no width transition; `border: none` -> `border: 1px solid` runs `3px` ->
    /// `1px`, `2px` at half way; `vtr2.html`), and what a child's `border-top-width: inherit` takes.
    pub fn value(&self, key: StyleKey, property: PropertyId) -> &str {
        if let Some(style) = side_style(property) {
            if !AS_COMPUTED.with(std::cell::Cell::get) && matches!(self.computed_text(key, style), "none" | "hidden") {
                return "0px";
            }
        }
        self.computed_value(key, property)
    }

    /// A style's COMPUTED value for one property: `currentcolor` in a colour property read as the element's `color`,
    /// and a border side's width whatever its style ([`StyleArena::value`] is the used one).
    pub fn computed_value(&self, key: StyleKey, property: PropertyId) -> &str {
        let value = self.computed_text(key, property);
        // `currentcolor` computes to ITSELF and is resolved where it is read, against this element's own `color`
        // (CSS Color 4): it is the initial value of the border, outline, rule, decoration, emphasis, caret and text
        // fill/stroke colours, and it inherits as the keyword. Chrome 152 (`cc.html`, 2026-10-23): under `color: red`
        // `border-top-color` reads `rgb(255, 0, 0)`, and a `color: blue` child whose `border-top-color` is `inherit`
        // from a `currentcolor` parent -- or that inherits `-webkit-text-fill-color: currentcolor`, `fill:
        // currentcolor` -- reads `rgb(0, 0, 255)`. korvus read the table's `rgb(0, 0, 0)` for the first, and the
        // parent's red for the rest; layout and paint read here too, so a border nothing coloured now paints in
        // the text's colour.
        // ...in a COLOUR property only: `animation-name: currentcolor` or `font-family: currentcolor` is a name (Chrome
        // 152 reads `currentcolor` back)
        if value == CURRENT_COLOR
            && property != PropertyId::Color
            && (property.computed_kind() == crate::style::property::Computed::Color || property == PropertyId::AccentColor)
        {
            return self.computed_value(key, PropertyId::Color);
        }
        value
    }

    /// [`StyleArena::computed_value`] as a transition interpolates it: `currentcolor` in a shadow or a `drop-shadow()`
    /// read as the element's `color` too ([`current_color_in`]), so `rgb(255, 0, 0) 1px 1px 0px 0px` meets `rgb(0, 0,
    /// 255) 2px 2px 0px 0px` value by value.
    pub fn transition_value(&self, key: StyleKey, property: PropertyId) -> std::borrow::Cow<'_, str> {
        let value = self.computed_value(key, property);
        match current_color_in(property, value, || self.computed_value(key, PropertyId::Color).to_string()) {
            Some(resolved) => std::borrow::Cow::Owned(resolved),
            None => std::borrow::Cow::Borrowed(value),
        }
    }

    /// [`StyleArena::value`] as the cascade computed it: `currentcolor` still the keyword -- in a colour property, a
    /// shadow and a `drop-shadow()` -- and a border side's width whatever its style. What a transition compares --
    /// Blink's `CSSPropertyEquality` holds two `currentcolor`s equal whatever `color` did, so a `color` change under
    /// `transition: all` starts no `border-top-color` transition (Chrome 152, `cctr3.html`: even with
    /// `border-top-color: currentcolor` written out, `getAnimations()` holds the `color` one alone), and none of
    /// `box-shadow`, `text-shadow` or `filter` whose shadow takes its colour from it (`ctr.html`, `vtr.html`).
    pub fn computed_text(&self, key: StyleKey, property: PropertyId) -> &str {
        // A LOGICAL property is never stored: the cascade writes it into its physical slot (see
        // `style::logical`), so reading one maps it again through this element's own computed
        // `writing-mode` and `direction` -- both inherited, so both in this half.
        if crate::style::logical::is_logical(property) {
            let inherited = self.inherited(key.inherited);
            let o = crate::style::logical::Orientation::of(
                inherited.get(PropertyId::WritingMode).unwrap_or_else(|| PropertyId::WritingMode.initial()),
                inherited.get(PropertyId::Direction).unwrap_or_else(|| PropertyId::Direction.initial()),
            );
            if let Some(physical) = crate::style::logical::to_physical(property, o) {
                return self.computed_text(key, physical);
            }
        }
        let set = if property.is_inherited() {
            self.inherited(key.inherited)
        } else {
            self.own(key.own)
        };
        // A border side with no style has no width in its USED value only ([`StyleArena::value`]); a column rule's keeps
        // it even there (Chrome 152, `crw.html` 2026-10-23: `column-rule-width: 10px` alone reads `10px`). An unset
        // `currentcolor` property is its initial keyword here, resolved against `color` by [`StyleArena::computed_value`].
        set.get(property).unwrap_or_else(|| property.initial())
    }

    /// A custom property's value. An unregistered one inherits, so it lives on the inherited half; a
    /// registered one that does not inherit lives on the own half, and a registered one nothing set
    /// is its initial value.
    pub fn custom_value(&self, key: StyleKey, name: &str) -> Option<&str> {
        self.own(key.own)
            .custom(name)
            .or_else(|| self.inherited(key.inherited).custom(name))
            .or_else(|| self.registry.get(name).and_then(|r| r.initial.as_deref()))
    }

    /// Every property, in table order, with the value this style gives it — what
    /// `getComputedStyle` enumerates.
    pub fn values(&self, key: StyleKey) -> impl Iterator<Item = (PropertyId, &str)> {
        crate::style::property::ALL.iter().map(move |&p| (p, self.value(key, p)))
    }

    /// The same, restricted to what `getComputedStyle` enumerates and in the order it enumerates.
    ///
    /// Not every property the cascade knows is one a page can read back: Chrome implements
    /// `border-spacing` as a shorthand and folds `background-position-x`/`-y` into
    /// `background-position`, so all three are absent from its enumeration while still being names a
    /// stylesheet writes. See `property::ENUMERATED`.
    pub fn enumerated_values(&self, key: StyleKey) -> impl Iterator<Item = (PropertyId, &str)> {
        crate::style::property::ENUMERATED.iter().map(move |&p| (p, self.value(key, p)))
    }

    /// Every custom property this style carries, in name order. `getComputedStyle` enumerates these
    /// alongside the standard ones.
    pub fn custom_values(&self, key: StyleKey) -> impl Iterator<Item = (&str, &str)> {
        let own = self.own(key.own);
        let inherited = self.inherited(key.inherited);
        let mut names: Vec<(&str, &str)> = own.custom_iter().chain(inherited.custom_iter()).collect();
        for (name, reg) in self.registry.iter() {
            if let Some(initial) = reg.initial.as_deref() {
                names.push((name, initial));
            }
        }
        // First occurrence wins: own, then inherited, then the registered initial value.
        let mut seen = std::collections::HashSet::new();
        names.retain(|(n, _)| seen.insert(*n));
        names.sort_by(|a, b| a.0.cmp(b.0));
        names.into_iter()
    }
}

fn intern(store: &mut Vec<PropertySet>, ids: &mut korvus_dom::FxHashMap<PropertySet, u32>, set: PropertySet) -> u32 {
    if let Some(&id) = ids.get(&set) {
        return id;
    }
    let id = store.len() as u32;
    store.push(set.clone());
    ids.insert(set, id);
    id
}

/// A stack slot per property, for the cascade's "which declaration wins" pass.
///
/// A fixed array rather than a map because there are [`COUNT`] properties and the cascade runs once
/// per element: a `HashMap` allocation per node would cost more than the whole pass.
pub type PerProperty<T> = [Option<T>; COUNT];

/// An empty per-property array. `[None; COUNT]` needs `Copy`, which the cascade's payload is not.
pub fn per_property<T>() -> PerProperty<T> {
    std::array::from_fn(|_| None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A shadow's `currentcolor` is read as the element's colour, a filter's `url()` passed over (`fix1.html`,
    // 2026-10-23).
    #[test]
    fn a_shadows_current_color_is_read_as_the_colour() {
        let blue = || "rgb(0, 0, 255)".to_string();
        assert_eq!(
            current_color_in(PropertyId::TextShadow, "currentcolor 1px 1px 0px, rgb(255, 0, 0) 2px 2px 0px", blue).as_deref(),
            Some("rgb(0, 0, 255) 1px 1px 0px, rgb(255, 0, 0) 2px 2px 0px")
        );
        assert_eq!(
            current_color_in(PropertyId::Filter, "url(#currentcolor) drop-shadow(currentcolor 1px 1px 0px)", blue).as_deref(),
            Some("url(#currentcolor) drop-shadow(rgb(0, 0, 255) 1px 1px 0px)")
        );
        assert_eq!(current_color_in(PropertyId::Filter, "url(#currentcolor) blur(1px)", blue), None);
        assert_eq!(current_color_in(PropertyId::AnimationName, "currentcolor", blue), None, "a name");
        assert_eq!(current_color_in(PropertyId::BoxShadow, "rgb(1, 2, 3) 1px 1px 0px 0px", || unreachable!()), None);
    }

    #[test]
    fn a_set_keeps_its_entries_sorted_and_answers_by_property() {
        let mut set = PropertySet::default();
        set.set(PropertyId::Width, "10px");
        set.set(PropertyId::Color, "red");
        set.set(PropertyId::Display, "block");
        let order: Vec<PropertyId> = set.iter().map(|(p, _)| p).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "iteration is in property order");
        assert_eq!(set.get(PropertyId::Color), Some("red"));
        assert_eq!(set.get(PropertyId::MarginTop), None);
    }

    #[test]
    fn setting_a_property_twice_replaces_it() {
        let mut set = PropertySet::default();
        set.set(PropertyId::Color, "red");
        set.set(PropertyId::Color, "blue");
        assert_eq!(set.len(), 1);
        assert_eq!(set.get(PropertyId::Color), Some("blue"));
    }

    // The whole point: the same style, interned twice, is one entry.
    #[test]
    fn identical_halves_intern_to_the_same_id() {
        let mut arena = StyleArena::default();
        let mut a = PropertySet::default();
        a.set(PropertyId::Color, "red");
        let mut b = PropertySet::default();
        b.set(PropertyId::Color, "red");
        assert_eq!(arena.intern_inherited(a), arena.intern_inherited(b));
        assert_eq!(arena.len().0, 2, "the empty set plus this one");
    }

    #[test]
    fn the_empty_sets_are_index_zero_so_an_unstyled_document_interns_nothing() {
        let arena = StyleArena::default();
        assert!(arena.is_empty());
        assert!(arena.inherited(0).is_empty());
        assert!(arena.own(0).is_empty());
        assert_eq!(StyleKey::INITIAL, StyleKey { inherited: 0, own: 0 });
    }

    // A set holds only what was *set*. Everything else is not missing, it is initial.
    #[test]
    fn an_unmentioned_property_reads_as_its_initial_value() {
        let mut arena = StyleArena::default();
        let mut own = PropertySet::default();
        own.set(PropertyId::Width, "10px");
        let key = StyleKey { inherited: 0, own: arena.intern_own(own) };
        assert_eq!(arena.value(key, PropertyId::Width), "10px");
        assert_eq!(arena.value(key, PropertyId::Display), "inline");
        assert_eq!(arena.value(key, PropertyId::Color), "rgb(0, 0, 0)");
    }

    // The halves are separate so that sharing one does not drag the other along -- the correction
    // to D6 that this whole file is shaped around.
    #[test]
    fn sharing_an_inherited_half_does_not_share_the_parents_own_values() {
        let mut arena = StyleArena::default();
        let mut inherited = PropertySet::default();
        inherited.set(PropertyId::Color, "red");
        let mut parent_own = PropertySet::default();
        parent_own.set(PropertyId::Width, "500px");

        let inherited_id = arena.intern_inherited(inherited);
        let parent = StyleKey { inherited: inherited_id, own: arena.intern_own(parent_own) };
        // The child takes the parent's inherited half by identity, and the initial own half.
        let child = StyleKey { inherited: inherited_id, own: 0 };

        assert_eq!(arena.value(parent, PropertyId::Color), "red");
        assert_eq!(arena.value(child, PropertyId::Color), "red", "colour is inherited");
        assert_eq!(arena.value(parent, PropertyId::Width), "500px");
        assert_eq!(arena.value(child, PropertyId::Width), "auto", "width is not");
    }

    // Custom property names are case-sensitive, unlike every other name in CSS.
    #[test]
    fn inherit_from_is_the_lookup_and_insert_merge_it_replaced() {
        // One linear merge of the two sorted halves, own values winning, must be the set the
        // per-entry loop built (look each parent entry up here, insert it when missing).
        let mut parent = PropertySet::default();
        parent.set(PropertyId::Color, "red");
        parent.set(PropertyId::FontSize, "16px");
        for (n, v) in [("--a", "1"), ("--c", "3"), ("--e", "5"), ("--z", "26")] {
            parent.set_custom(n, v);
        }
        let mut own = PropertySet::default();
        own.set(PropertyId::Color, "blue");
        for (n, v) in [("--b", "2"), ("--c", "own"), ("--f", "6")] {
            own.set_custom(n, v);
        }
        let mut looped = own.clone();
        for (property, value) in parent.iter() {
            if looped.get(property).is_none() {
                looped.set(property, value);
            }
        }
        for (name, value) in parent.custom_iter() {
            if looped.custom(name).is_none() {
                looped.set_custom(name, value);
            }
        }
        let mut merged = own.clone();
        merged.inherit_from(&parent);
        assert_eq!(merged, looped);
        assert_eq!(merged.custom("--c"), Some("own"));
        assert_eq!(merged.get(PropertyId::Color), Some("blue"));
        let mut empty = PropertySet::default();
        empty.inherit_from(&parent);
        assert_eq!(empty, parent);
    }

    #[test]
    fn custom_properties_are_kept_by_their_exact_name() {
        let mut set = PropertySet::default();
        set.set_custom("--foo", "1");
        set.set_custom("--Foo", "2");
        assert_eq!(set.custom("--foo"), Some("1"));
        assert_eq!(set.custom("--Foo"), Some("2"));
        assert_eq!(set.custom("--bar"), None);
        assert_eq!(set.len(), 2);
        assert!(!set.is_empty(), "a set with only custom properties is not empty");
    }

    #[test]
    fn a_style_enumerates_every_property_the_table_knows() {
        let arena = StyleArena::default();
        let all: Vec<_> = arena.values(StyleKey::INITIAL).collect();
        assert_eq!(all.len(), COUNT);
        assert!(all.iter().any(|(p, v)| *p == PropertyId::Display && *v == "inline"));
    }
}
