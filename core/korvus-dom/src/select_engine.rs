//! The `selectors`-crate selector engine built against our arena [`Dom`].
//!
//! T2'a stood this up DORMANT (proven by unit tests only). T2'b wired the public
//! `Dom::query_selector*` / `matches_selector` / `closest` methods in `selector.rs` onto it,
//! replacing the hand-rolled matcher: parsing is strict (invalid selector => `SelectorParseError`
//! => a DOM `SyntaxError` at the webapi boundary) and matching is spec-driven. A per-thread
//! compiled-selector cache ([`compile`]) parses each selector string once. Everything here is
//! `pub(crate)`; a couple of `Element`-trait helpers are only reached through the trait object, so
//! the module keeps `allow(dead_code)`.
//!
//! T2'b's parser only recognised a hand-picked SUBSET of pseudo-classes/-elements, so every other
//! *real, standard* one (e.g. `:focus-within`, `:placeholder-shown`, `:valid`) incorrectly raised
//! `SyntaxError` — a hazard real browsers don't have: they parse any standard pseudo and simply
//! match nothing when they can't evaluate it live. T2'c widens `parse_non_ts_pseudo_class` /
//! `parse_non_ts_functional_pseudo_class` / `parse_pseudo_element` to accept the FULL standard
//! list; a name Korvus cannot evaluate parses into [`KorvusPseudoClass::UnsupportedValid`] and
//! simply never matches. Only a genuinely unknown pseudo name is still a parse error.
//!
//! Structure follows a working reference implementation
//! (same `selectors` 0.26 / `cssparser` 0.34), but is ADAPTED to our differences:
//!   * element/attr names are `String`, not `markup5ever` atoms — so `has_local_name` compares our
//!     `&str` tag to the selector atom via `&*local_name.0`;
//!   * children are a `Vec<NodeId>` with NO sibling links — so prev/next/first element siblings are
//!     computed by finding `self` in the parent's `children` vec and stepping to the adjacent element.
#![allow(dead_code)]

use cssparser::{CowRcStr, ToCss};
use markup5ever::{LocalName, Namespace};
use precomputed_hash::PrecomputedHash;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::{BloomFilter, BLOOM_HASH_MASK};
use selectors::matching::{
    ElementSelectorFlags, MatchingContext, MatchingForInvalidation, MatchingMode,
    NeedsSelectorFlags, QuirksMode,
};
use selectors::parser::{self, ParseRelative, SelectorParseErrorKind};
use selectors::visitor::{SelectorListKind, SelectorVisitor};
use selectors::{Element, OpaqueElement, SelectorList};
use std::cell::RefCell;
use std::rc::Rc;

use crate::tree::{ClassTokens, Dom, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// SelectorImpl + its associated types
// ---------------------------------------------------------------------------

/// The `selectors` "flavour" for Korvus. Zero-sized; carries only the associated type choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KorvusSelectorImpl;

impl parser::SelectorImpl for KorvusSelectorImpl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssAttrValue;
    type Identifier = CssString;
    type LocalName = CssLocalName;
    type NamespaceUrl = CssNamespace;
    type NamespacePrefix = CssString;
    type BorrowedLocalName = CssLocalName;
    type BorrowedNamespaceUrl = CssNamespace;
    type NonTSPseudoClass = KorvusPseudoClass;
    type PseudoElement = KorvusPseudoElement;
}

// The `selectors` associated types must implement foreign traits (`ToCss`, `PrecomputedHash`).
// The orphan rule forbids `impl ToCss for markup5ever::LocalName` / `for String` directly, so we
// wrap the reused atoms / strings in local newtypes and impl the traits on those.

/// `Identifier` / `NamespacePrefix`: a plain owned string that is a CSS *identifier*.
///
/// The distinction from [`CssAttrValue`] is only visible when serializing, and it is the whole
/// reason the two exist separately: `.a` must come back as `.a`, while `[href="a"]` must come back
/// with its quotes. One newtype serving both roles makes every class and id in `selectorText` a
/// quoted string — which is what it used to do.
///
/// The second field is [`string_hash`] of the first, computed once where the string is made:
/// `precomputed_hash` is asked per selector test by the ancestor bloom filter, and `has_class` compares
/// it against the element's cached class hashes before comparing any bytes. The third is
/// [`folded_string_hash`], which a QUIRKS document's `has_class` compares instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CssString(pub String, u32, u32);

impl Default for CssString {
    fn default() -> Self {
        CssString::from("")
    }
}

/// `AttrValue`: the right-hand side of an attribute selector, which serializes as a CSS string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub(crate) struct CssAttrValue(pub String);

impl<'a> From<&'a str> for CssAttrValue {
    fn from(s: &'a str) -> Self {
        CssAttrValue(s.to_string())
    }
}

impl AsRef<str> for CssAttrValue {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for CssAttrValue {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

impl<'a> From<&'a str> for CssString {
    fn from(s: &'a str) -> Self {
        CssString(s.to_string(), string_hash(s), folded_string_hash(s))
    }
}

impl AsRef<str> for CssString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for CssString {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        cssparser::serialize_identifier(&self.0, dest)
    }
}

impl PrecomputedHash for CssString {
    fn precomputed_hash(&self) -> u32 {
        self.1
    }
}

/// djb2 over the bytes — any stable hash suffices for the bloom pre-filter. One function for the
/// selector side ([`CssString`]) and the element side ([`AncestorBloom`]), which must agree, and it
/// takes a `&str` so the element side hashes an attribute value without copying it
/// (the copy was a `malloc`/`free` pair per id and class of every ancestor of every element styled).
pub(crate) fn string_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.as_bytes() {
        h = h.wrapping_mul(33).wrapping_add(*b as u32);
    }
    h
}

/// [`string_hash`] of `s` folded to ASCII lowercase, without making the folded string: what a quirks
/// document's class match compares, since it folds class case. Equal to `string_hash(s)`
/// when `s` has no ASCII capital.
pub(crate) fn folded_string_hash(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.as_bytes() {
        h = h.wrapping_mul(33).wrapping_add(b.to_ascii_lowercase() as u32);
    }
    h
}

/// `LocalName` — reuses the `markup5ever` interned atom.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CssLocalName(pub LocalName);

impl<'a> From<&'a str> for CssLocalName {
    fn from(s: &'a str) -> Self {
        CssLocalName(LocalName::from(s))
    }
}

impl ToCss for CssLocalName {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        dest.write_str(&self.0)
    }
}

impl PrecomputedHash for CssLocalName {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

/// `NamespaceUrl` — reuses the `markup5ever` interned atom.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub(crate) struct CssNamespace(pub Namespace);

impl PrecomputedHash for CssNamespace {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

// ---------------------------------------------------------------------------
// Non-tree-structural pseudo-classes
// ---------------------------------------------------------------------------

/// Every REAL, standard CSS pseudo-class the parser accepts (T2'c). Tree-structural pseudos
/// (`:first-child`, `:nth-child`, `:empty`, `:root`, `:scope`, …) are handled by the `selectors`
/// crate itself (via our `Element` sibling/child methods, or built in for `:scope`) and are NOT
/// represented here; likewise `:not()`/`:is()`/`:where()`/`:has()`/`:nth-child()`/`:nth-of-type()`
/// are crate built-ins, and `:lang()`/`:dir()` are parsed by
/// [`KorvusSelectorParser::parse_non_ts_functional_pseudo_class`].
///
/// MATCHING CONTRACT (T2'c, matches real browsers): a name that is a genuine CSS pseudo-class
/// always PARSES — only a name outside the standard list is a `SyntaxError`. Of the ones that
/// parse, only the STATIC, attribute-derived pseudos below are answered truthfully; every other
/// standard pseudo (dynamic UI/interaction state, form-validity state, language/direction,
/// custom-element/fullscreen/media state, …) parses into [`KorvusPseudoClass::UnsupportedValid`]
/// and simply never matches — exactly like a real browser that recognises the syntax but has
/// nothing to report for a headless, no-render, no-history, no-interaction DOM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KorvusPseudoClass {
    // --- Evaluated from the arena's attributes (unchanged approach since T2'a/T2'b) ---
    Enabled,
    Disabled,
    Checked,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    // --- Form-control state: what script set, read from the arena (`forms.rs`) ---
    Default,
    Indeterminate,
    /// `:popover-open` -- a popover script showed (`Dom::is_popover_open`).
    PopoverOpen,
    /// `:modal` -- a dialog opened with `showModal()` (`Dom::is_modal`).
    Modal,
    PlaceholderShown,
    Valid,
    Invalid,
    InRange,
    OutOfRange,
    /// `:link` / `:any-link` — an `<a>`/`<area>` with an `href`. Korvus tracks no navigation
    /// history, so both names behave identically (nothing is ever "visited").
    Link,
    /// `:any-link` and its alias `:-webkit-any-link`: matched as [`KorvusPseudoClass::Link`], and
    /// serialized under the name they were written with (Chrome 150's `selectorText`
    /// keeps `a:any-link` and `a:-webkit-any-link`; this engine wrote `a:link` for both).
    AnyLink(CssString),
    /// `:-webkit-any(<compound-selector>#)`: Blink's legacy `:is()` -- NOT forgiving and
    /// compound selectors only (`:-webkit-any(a b)` is a SyntaxError in Chrome 150), serialized as
    /// written. Matched by the crate's own list matcher.
    WebkitAny(SelectorList<KorvusSelectorImpl>),

    /// `:hover` — the element is in [`Dom::hover_chain`], i.e. it is the element under the pointer
    /// or an ancestor of it. The ONLY reader of that chain, which is what makes
    /// `Element.matches(":hover")`, `querySelectorAll(":hover")` and the cascade agree by
    /// construction rather than by three implementations happening to match.
    Hover,

    /// `:focus`, `:focus-visible` and `:focus-within`: the focus ring as [`Dom::set_focus`] last moved it.
    Focus,
    FocusVisible,
    FocusWithin,

    /// `:lang(<ident>)`: the element's language -- its nearest `lang` (or `xml:lang`) -- is the range or begins with it
    /// and a `-` (Blink's `kPseudoLang`: `ko` matches `ko-KR`). Chrome 152 takes one identifier only: a string, a list
    /// or a wildcard is a SyntaxError.
    Lang(CssString),
    /// `:dir(ltr)` / `:dir(rtl)`: the element's directionality -- its nearest `dir` of `ltr` or `rtl`, else `ltr`.
    Dir(CssString),

    /// Every other standard CSS pseudo-class Korvus recognises but cannot evaluate yet. Parses
    /// successfully; `match_non_ts_pseudo_class` always answers `false` for it. Carries the
    /// canonical (lowercase, argument-free) name, used for `ToCss`/debugging and to answer
    /// [`parser::NonTSPseudoClass::is_active_or_hover`] / `is_user_action_state`.
    UnsupportedValid(CssString),
}

impl parser::NonTSPseudoClass for KorvusPseudoClass {
    type Impl = KorvusSelectorImpl;

    fn is_active_or_hover(&self) -> bool {
        match self {
            KorvusPseudoClass::Hover => true,
            KorvusPseudoClass::UnsupportedValid(name) => matches!(name.0.as_str(), "active"),
            _ => false,
        }
    }

    fn is_user_action_state(&self) -> bool {
        match self {
            KorvusPseudoClass::Hover | KorvusPseudoClass::Focus | KorvusPseudoClass::FocusVisible | KorvusPseudoClass::FocusWithin => true,
            // The scrollbar parts' states, which may follow `::-webkit-scrollbar*`:
            // Chrome 150 answers `querySelectorAll("::-webkit-scrollbar:horizontal")` with 0 matches,
            // not a SyntaxError.
            KorvusPseudoClass::UnsupportedValid(name) => matches!(
                name.0.as_str(),
                "active" | "focus" | "focus-visible" | "focus-within" | "horizontal" | "vertical" | "decrement"
                    | "increment" | "start" | "end" | "double-button" | "single-button" | "no-button"
                    | "corner-present" | "window-inactive"
            ),
            _ => false,
        }
    }

    /// `:-webkit-any()` is the one pseudo-class here that holds selectors, and a visitor has to see
    /// them: the cascade decides from [`CacheUse`] whether a selector may share the never-cleared
    /// `UNUSED_CACHES`, and `:-webkit-any(li:nth-child(2))` writes the nth index there as `:is()` does.
    /// Answering `true` without looking let that index, keyed by node address, outlive a DOM change:
    /// after `ul.prepend(li)` the cascade still styled the old second item.
    fn visit<V>(&self, visitor: &mut V) -> bool
    where
        V: SelectorVisitor<Impl = Self::Impl>,
    {
        match self {
            KorvusPseudoClass::WebkitAny(list) => visitor.visit_selector_list(SelectorListKind::empty(), list.slice()),
            _ => true,
        }
    }
}

impl ToCss for KorvusPseudoClass {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        match self {
            KorvusPseudoClass::Enabled => dest.write_str(":enabled"),
            KorvusPseudoClass::Disabled => dest.write_str(":disabled"),
            KorvusPseudoClass::Checked => dest.write_str(":checked"),
            KorvusPseudoClass::Required => dest.write_str(":required"),
            KorvusPseudoClass::Optional => dest.write_str(":optional"),
            KorvusPseudoClass::ReadOnly => dest.write_str(":read-only"),
            KorvusPseudoClass::ReadWrite => dest.write_str(":read-write"),
            KorvusPseudoClass::Link => dest.write_str(":link"),
            KorvusPseudoClass::AnyLink(name) => {
                dest.write_char(':')?;
                dest.write_str(&name.0)
            }
            KorvusPseudoClass::WebkitAny(list) => {
                dest.write_str(":-webkit-any(")?;
                list.to_css(dest)?;
                dest.write_char(')')
            }
            KorvusPseudoClass::Default => dest.write_str(":default"),
            KorvusPseudoClass::Indeterminate => dest.write_str(":indeterminate"),
            KorvusPseudoClass::PopoverOpen => dest.write_str(":popover-open"),
            KorvusPseudoClass::Modal => dest.write_str(":modal"),
            KorvusPseudoClass::PlaceholderShown => dest.write_str(":placeholder-shown"),
            KorvusPseudoClass::Valid => dest.write_str(":valid"),
            KorvusPseudoClass::Invalid => dest.write_str(":invalid"),
            KorvusPseudoClass::InRange => dest.write_str(":in-range"),
            KorvusPseudoClass::OutOfRange => dest.write_str(":out-of-range"),
            KorvusPseudoClass::Hover => dest.write_str(":hover"),
            KorvusPseudoClass::Focus => dest.write_str(":focus"),
            KorvusPseudoClass::FocusVisible => dest.write_str(":focus-visible"),
            KorvusPseudoClass::FocusWithin => dest.write_str(":focus-within"),
            KorvusPseudoClass::Lang(range) => {
                dest.write_str(":lang(")?;
                cssparser::serialize_identifier(&range.0, dest)?;
                dest.write_char(')')
            }
            KorvusPseudoClass::Dir(d) => {
                dest.write_str(":dir(")?;
                cssparser::serialize_identifier(&d.0, dest)?;
                dest.write_char(')')
            }
            KorvusPseudoClass::UnsupportedValid(name) => {
                dest.write_char(':')?;
                dest.write_str(&name.0)
            }
        }
    }
}

/// A known, standard, non-functional CSS pseudo-element (`::before`, `::after`,
/// `::first-line`, `::placeholder`, …) — see [`KorvusSelectorParser::parse_pseudo_element`] for
/// the accept-list. `querySelector`/`querySelectorAll` never return pseudo-elements (they aren't
/// real DOM nodes), so `match_pseudo_element` unconditionally answers `false`; what matters here
/// (T2'c) is that a KNOWN pseudo-element name parses instead of raising `SyntaxError`, matching
/// real browsers. An unrecognised `::name` still fails to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KorvusPseudoElement {
    Known(CssString),
}

impl parser::PseudoElement for KorvusPseudoElement {
    type Impl = KorvusSelectorImpl;

    /// The scrollbar parts take state pseudo-classes (`::-webkit-scrollbar-thumb:hover`,
    /// `::-webkit-scrollbar:horizontal`), which Chrome keeps; without this the whole rule was a parse
    /// error and dropped.
    fn accepts_state_pseudo_classes(&self) -> bool {
        let KorvusPseudoElement::Known(name) = self;
        name.0.starts_with("-webkit-scrollbar")
    }
}

impl ToCss for KorvusPseudoElement {
    fn to_css<W: std::fmt::Write>(&self, dest: &mut W) -> std::fmt::Result {
        let KorvusPseudoElement::Known(name) = self;
        dest.write_str("::")?;
        dest.write_str(&name.0)
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Drives `SelectorList::parse` for our impl: maps pseudo-class/pseudo-element names to
/// [`KorvusPseudoClass`]/[`KorvusPseudoElement`] and enables `:is()`/`:where()`/`:has()` (the
/// crate itself already special-cases `:not()`/`:nth-child()`/`:nth-of-type()`/`:scope`/etc — see
/// the module doc). T2'c: accepts every REAL, standard CSS pseudo-class/-element; only a
/// genuinely unknown name is a parse error (=> DOM `SyntaxError` at the webapi boundary).
#[derive(Default)]
pub(crate) struct KorvusSelectorParser<'a> {
    /// The sheet's `@namespace` rules as (prefix, URL), `""` the default namespace: a query has none, so
    /// `querySelector('svg|rect')` is a SyntaxError there as in Chrome.
    namespaces: &'a [(String, String)],
}

impl<'i, 'a> parser::Parser<'i> for KorvusSelectorParser<'a> {
    type Impl = KorvusSelectorImpl;
    type Error = SelectorParseErrorKind<'i>;

    /// `@namespace url(...)`: a type or universal selector without a prefix is in that namespace.
    fn default_namespace(&self) -> Option<CssNamespace> {
        self.namespaces.iter().rev().find(|(p, _)| p.is_empty()).map(|(_, u)| CssNamespace(Namespace::from(u.as_str())))
    }

    /// `@namespace svg url(...)`: `svg|rect`. Prefixes are case-sensitive; one the sheet does not declare is
    /// an invalid selector.
    fn namespace_for_prefix(&self, prefix: &CssString) -> Option<CssNamespace> {
        self.namespaces
            .iter()
            .rev()
            .find(|(p, _)| !p.is_empty() && *p == prefix.0)
            .map(|(_, u)| CssNamespace(Namespace::from(u.as_str())))
    }

    fn parse_is_and_where(&self) -> bool {
        true
    }

    /// `&`, the CSS Nesting selector. Outside a style rule it means `:scope` -- the
    /// crate matches it against `MatchingContext::scope_element`, falling back to the root -- which is
    /// Chrome's answer to a query too: `document.querySelectorAll("& > div")` is 0 matches on a page
    /// whose `<body>` holds divs, where it used to be a `SyntaxError` (L14 C18, measured on Chrome
    /// 150). Inside a stylesheet's nested rule the cascade replaces it with the parent's selectors
    /// ([`Selector::parse_nested`] / [`Selector::replace_parent`]).
    fn parse_parent_selector(&self) -> bool {
        true
    }

    fn parse_has(&self) -> bool {
        true
    }

    /// `:host` / `:host()` are parsed by the `selectors` crate itself once this hook is enabled.
    /// Parsing is a prerequisite, not visibility: ordinary document/element matching still cannot
    /// match `:host` because those contexts leave `MatchingContext::current_host` as `None`
    /// (`matches_host` returns false). Only [`matches_scoped_in_shadow`], which pins `current_host`
    /// to the shadow host, resolves `:host` -- so `document.querySelector(":host")` parses and
    /// matches nothing rather than raising a `SyntaxError` (Korvus GS ②-A2).
    fn parse_host(&self) -> bool {
        true
    }

    /// `::slotted()` (Korvus GS ②-B2): let the `selectors` crate parse the pseudo-element and its
    /// slot-assignment combinator. Matching is supplied by [`NodeRef::is_html_slot_element`] /
    /// [`NodeRef::assigned_slot`] below (the crate crosses from a slotted light node to its
    /// `<slot>`). Like `:host`, parsing is a prerequisite, not visibility: only
    /// [`Dom::shadow_descendants_matching`] enumerates slotted light-DOM candidates, so
    /// `document.querySelector("::slotted(...)")` parses and simply matches nothing.
    fn parse_slotted(&self) -> bool {
        true
    }

    fn parse_non_ts_pseudo_class(
        &self,
        location: cssparser::SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<KorvusPseudoClass, cssparser::ParseError<'i, Self::Error>> {
        use KorvusPseudoClass::*;
        // `match_ignore_ascii_case!` compares case-insensitively (patterns are already
        // lowercase), so `:HOVER`/`:Hover`/`:hover` all reach the same arm — real CSS
        // pseudo-class names are ASCII case-insensitive, unlike a plain `match name.as_ref()`.
        cssparser::match_ignore_ascii_case! { &name,
            // --- Evaluated from the arena's attributes ---
            "enabled" => Ok(Enabled),
            "disabled" => Ok(Disabled),
            "checked" => Ok(Checked),
            "required" => Ok(Required),
            "optional" => Ok(Optional),
            "read-only" => Ok(ReadOnly),
            "read-write" => Ok(ReadWrite),
            "link" => Ok(Link),
            "any-link" => Ok(AnyLink(CssString::from("any-link"))),

            // --- Blink's author `:-webkit-*` pseudo-classes ---
            // A CLOSED list (reference/chromium css_selector.cc's name table; an unknown
            // `:-webkit-foo` is a SyntaxError in Chrome 150, unlike `::-webkit-foo`), measured in
            // `tools/realweb/seed/webkitpseudo`. `-webkit-any-link` is `:any-link`; the rest parse and
            // never match here, as their standard forms do (`:autofill`, `:fullscreen`).
            "-webkit-any-link" => Ok(AnyLink(CssString::from("-webkit-any-link"))),
            "-webkit-autofill" => Ok(UnsupportedValid(CssString::from("-webkit-autofill"))),
            "-webkit-drag" => Ok(UnsupportedValid(CssString::from("-webkit-drag"))),
            "-webkit-full-page-media" => Ok(UnsupportedValid(CssString::from("-webkit-full-page-media"))),
            "-webkit-full-screen" => Ok(UnsupportedValid(CssString::from("-webkit-full-screen"))),
            "-webkit-full-screen-ancestor" => Ok(UnsupportedValid(CssString::from("-webkit-full-screen-ancestor"))),
            // Measured: Chrome 150 accepts this internal name in an author sheet too.
            "-internal-autofill-selected" => Ok(UnsupportedValid(CssString::from("-internal-autofill-selected"))),

            // --- Live pointer state ---
            "hover" => Ok(Hover),

            // --- Standard, but no live signal yet: PARSES, never MATCHES ---
            // user-action / navigation
            "active" => Ok(UnsupportedValid(CssString::from("active"))),
            "focus" => Ok(Focus),
            "focus-visible" => Ok(FocusVisible),
            "focus-within" => Ok(FocusWithin),
            "target" => Ok(UnsupportedValid(CssString::from("target"))),
            "target-within" => Ok(UnsupportedValid(CssString::from("target-within"))),
            "visited" => Ok(UnsupportedValid(CssString::from("visited"))),
            "local-link" => Ok(UnsupportedValid(CssString::from("local-link"))),
            // form / input validity
            "indeterminate" => Ok(Indeterminate),
            "default" => Ok(Default),
            "valid" => Ok(Valid),
            "invalid" => Ok(Invalid),
            "in-range" => Ok(InRange),
            "out-of-range" => Ok(OutOfRange),
            "placeholder-shown" => Ok(PlaceholderShown),
            "autofill" => Ok(UnsupportedValid(CssString::from("autofill"))),
            "user-valid" => Ok(UnsupportedValid(CssString::from("user-valid"))),
            "user-invalid" => Ok(UnsupportedValid(CssString::from("user-invalid"))),
            // element / media state
            "defined" => Ok(UnsupportedValid(CssString::from("defined"))),
            "fullscreen" => Ok(UnsupportedValid(CssString::from("fullscreen"))),
            "modal" => Ok(Modal),
            "popover-open" => Ok(PopoverOpen),
            "picture-in-picture" => Ok(UnsupportedValid(CssString::from("picture-in-picture"))),
            "playing" => Ok(UnsupportedValid(CssString::from("playing"))),
            "paused" => Ok(UnsupportedValid(CssString::from("paused"))),
            "muted" => Ok(UnsupportedValid(CssString::from("muted"))),
            "seeking" => Ok(UnsupportedValid(CssString::from("seeking"))),
            "buffering" => Ok(UnsupportedValid(CssString::from("buffering"))),
            "stalled" => Ok(UnsupportedValid(CssString::from("stalled"))),
            // the scrollbar parts' states (`::-webkit-scrollbar:horizontal`)
            "horizontal" => Ok(UnsupportedValid(CssString::from("horizontal"))),
            "vertical" => Ok(UnsupportedValid(CssString::from("vertical"))),
            "decrement" => Ok(UnsupportedValid(CssString::from("decrement"))),
            "increment" => Ok(UnsupportedValid(CssString::from("increment"))),
            "start" => Ok(UnsupportedValid(CssString::from("start"))),
            "end" => Ok(UnsupportedValid(CssString::from("end"))),
            "double-button" => Ok(UnsupportedValid(CssString::from("double-button"))),
            "single-button" => Ok(UnsupportedValid(CssString::from("single-button"))),
            "no-button" => Ok(UnsupportedValid(CssString::from("no-button"))),
            "corner-present" => Ok(UnsupportedValid(CssString::from("corner-present"))),
            "window-inactive" => Ok(UnsupportedValid(CssString::from("window-inactive"))),

            _ => Err(location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name))),
        }
    }

    fn parse_non_ts_functional_pseudo_class<'t>(
        &self,
        name: CowRcStr<'i>,
        input: &mut cssparser::Parser<'i, 't>,
        _after_part: bool,
    ) -> Result<KorvusPseudoClass, cssparser::ParseError<'i, Self::Error>> {
        // `:not()` / `:is()` / `:where()` / `:has()` / `:nth-child()` / `:nth-of-type()` (and
        // friends) never reach here — the crate intercepts those names itself before falling
        // back to this hook. Only `:lang()` / `:dir()` are left for us to recognise.
        use KorvusPseudoClass::*;
        cssparser::match_ignore_ascii_case! { &name,
            // `:lang( <ident> )`, matched against the element's language (see [`KorvusPseudoClass::Lang`]). It parsed
            // (lists and strings too, which Chrome refuses) and never matched: apple.com's `:lang(ko)` fonts never
            // applied to its Korean banner, which set wider than Chrome's.
            "lang" => {
                let range = input.expect_ident()?.clone();
                Ok(Lang(CssString::from(range.as_ref())))
            },
            // `:dir( <ident> )` — directionality, `ltr` or `rtl` (see [`KorvusPseudoClass::Dir`]).
            "dir" => {
                let d = input.expect_ident()?.to_ascii_lowercase();
                Ok(Dir(CssString::from(d.as_str())))
            },
            // `:-webkit-any( <compound-selector># )`: not forgiving, no combinators, no
            // pseudo-elements -- each a SyntaxError in Chrome 150.
            "-webkit-any" => {
                let list = SelectorList::parse(self, input, ParseRelative::No)?;
                let compound = list.slice().iter().all(|sel| {
                    !sel.has_pseudo_element()
                        && !sel.iter_raw_match_order().any(|c| matches!(c, parser::Component::Combinator(_)))
                });
                if !compound {
                    return Err(input.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name)));
                }
                Ok(WebkitAny(list))
            },
            _ => Err(input.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name))),
        }
    }

    fn parse_pseudo_element(
        &self,
        location: cssparser::SourceLocation,
        name: CowRcStr<'i>,
    ) -> Result<KorvusPseudoElement, cssparser::ParseError<'i, Self::Error>> {
        // Any `::-webkit-` name parses. Chrome 150 keeps `::-webkit-scrollbar-thumb {…}`
        // AND `::-webkit-foo-bar {…}` in `cssRules`, and `querySelectorAll("::-webkit-scrollbar")` is 0
        // matches rather than a SyntaxError: an unknown vendor pseudo-element parses and never
        // matches, where it used to drop the whole rule.
        if name.len() > 8 && name.is_char_boundary(8) && name[..8].eq_ignore_ascii_case("-webkit-") {
            return Ok(KorvusPseudoElement::Known(CssString::from(name.as_ref().to_ascii_lowercase().as_str())));
        }
        cssparser::match_ignore_ascii_case! { &name,
            "before" | "after" | "first-line" | "first-letter" | "selection" | "backdrop"
                | "placeholder" | "marker" | "file-selector-button" | "target-text"
                | "spelling-error" | "grammar-error" | "cue" | "details-content" => {
                Ok(KorvusPseudoElement::Known(CssString::from(name.as_ref().to_ascii_lowercase().as_str())))
            },
            _ => Err(location.new_custom_error(SelectorParseErrorKind::UnsupportedPseudoClassOrElement(name))),
        }
    }
}

// ---------------------------------------------------------------------------
// NodeRef: our arena node as a `selectors::Element`
// ---------------------------------------------------------------------------

/// A borrowed handle to one arena node, exposing it to the `selectors` matcher.
/// `Copy`, since it is just `(&Dom, NodeId)` and the matcher clones it freely as it walks the tree.
#[derive(Clone, Copy)]
pub(crate) struct NodeRef<'a> {
    pub dom: &'a Dom,
    pub id: NodeId,
}

impl<'a> std::fmt::Debug for NodeRef<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodeRef(#{})", self.id)
    }
}

impl<'a> NodeRef<'a> {
    fn is_element(&self, id: NodeId) -> bool {
        matches!(self.dom.node(id).kind, NodeKind::Element { .. })
    }

    fn wrap(&self, id: NodeId) -> Self {
        NodeRef { dom: self.dom, id }
    }

    /// Form controls that `:enabled` / `:disabled` / `:optional` apply to.
    fn is_form_control(&self) -> bool {
        matches!(
            self.dom.tag_name(self.id),
            Some(
                "input" | "button" | "select" | "textarea" | "optgroup" | "option" | "fieldset"
            )
        )
    }

    /// A boolean HTML attribute's mere presence (value is irrelevant).
    fn has_bool_attr(&self, name: &str) -> bool {
        self.dom.get_attribute(self.id, name).is_some()
    }

    /// `:link` / `:any-link` target: an `<a>`/`<area>` element with an `href` attribute. Distinct
    /// from the `Element::is_link` trait method below (which also treats `<link href>` as a link,
    /// for the crate's internal `:visited`-scan bookkeeping) — the CSS pseudo-classes themselves
    /// only ever apply to hyperlink-anchor elements.
    fn is_any_link(&self) -> bool {
        matches!(self.dom.tag_name(self.id), Some("a") | Some("area"))
            && self.dom.get_attribute(self.id, "href").is_some()
    }

    /// [`Dom::is_in_xml`], without the climb to the root when the arena holds no XML document -- nearly
    /// every page. Every type-selector test asks it (`has_local_name`), and the climb was a `root_of`
    /// call (a `RefCell` borrow, a revision and a generation check) per test: the same answer, since
    /// no root is in an empty set.
    #[inline]
    fn in_xml(&self) -> bool {
        !self.dom.xml_roots.is_empty() && self.dom.is_in_xml(self.id)
    }

    /// The element's language (HTML's "language of a node"): the `xml:lang` or `lang` of it or of its nearest ancestor
    /// that has one, lowercase -- an empty one is unknown, which no range matches. `None` when no element says.
    fn language(&self) -> Option<String> {
        let mut at = Some(self.id);
        while let Some(n) = at {
            if self.dom.node_type(n) == 1 {
                if let Some(l) = self.dom.get_attribute(n, "xml:lang").or_else(|| self.dom.get_attribute(n, "lang")) {
                    return Some(l.trim().to_ascii_lowercase());
                }
            }
            at = self.dom.parent(n);
        }
        None
    }

    /// The element's directionality: the `dir` of it or of its nearest ancestor that says `ltr` or `rtl`, else `ltr`.
    fn direction(&self) -> &'static str {
        let mut at = Some(self.id);
        while let Some(n) = at {
            if self.dom.node_type(n) == 1 {
                match self.dom.get_attribute(n, "dir").map(|d| d.trim().to_ascii_lowercase()).as_deref() {
                    Some("rtl") => return "rtl",
                    Some("ltr") => return "ltr",
                    _ => {}
                }
            }
            at = self.dom.parent(n);
        }
        "ltr"
    }

    /// The element's attributes, or none for any other node. Every attribute read the matcher makes
    /// that is not an id or a class goes through here, so a change to how an element's attributes are
    /// stored has one place to meet selector matching.
    #[inline]
    fn attr_list(&self) -> &'a [(String, String)] {
        match &self.dom.at(self.id).kind {
            NodeKind::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }
}

/// Does the language tag `lang` (lowercase) fall under `range`: it is the range, or begins with it and a `-`, ASCII
/// case-insensitively (Blink's `kPseudoLang`). An empty language -- unknown -- falls under none.
fn lang_matches(lang: &str, range: &str) -> bool {
    let range = range.to_ascii_lowercase();
    !lang.is_empty() && lang.starts_with(&range) && (lang.len() == range.len() || lang.as_bytes()[range.len()] == b'-')
}

impl<'a> Element for NodeRef<'a> {
    type Impl = KorvusSelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        // Must be stable per node across traversal steps (NodeRef is Copy and gets a fresh stack
        // slot each step). `Dom::node` hands back a reference into the arena's `Vec<Node>` slot,
        // which is not mutated during a query — so its address is a stable per-node identity.
        OpaqueElement::new(self.dom.node(self.id))
    }

    fn parent_element(&self) -> Option<Self> {
        let parent = self.dom.parent(self.id)?;
        self.is_element(parent).then(|| self.wrap(parent))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        // The `selectors` crate uses this to detect the shadow-tree boundary: when a child
        // combinator walks up and finds no `parent_element`, this tells it whether the missing
        // parent is a shadow root (so a featureless `:host` selector may still match the host).
        // The host<->root edge lives only in the shadow maps -- a shadow root is never an ordinary
        // `children` entry -- so we test the ordinary parent against `is_shadow_root`.
        self.dom.parent(self.id).is_some_and(|p| self.dom.is_shadow_root(p))
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        // Walk `self`'s tree to its parentless root (bounded against cycles inside `root_of`). If
        // that root is a shadow root, `shadow_host_of` yields its host element; a light-tree node's
        // root is the Document (or a detached fragment root), which is not a shadow root, so this
        // naturally returns `None` and never crosses a boundary for ordinary DOM.
        let root = self.dom.root_of(self.id);
        self.dom.shadow_host_of(root).map(|host| self.wrap(host))
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    // Sibling links: step from `self` (or its first child) to the nearest ELEMENT in the
    // requested direction, following prev_sibling / next_sibling / first_child directly.
    fn prev_sibling_element(&self) -> Option<Self> {
        let mut c = self.dom.previous_sibling(self.id);
        while let Some(cid) = c {
            if self.is_element(cid) {
                return Some(self.wrap(cid));
            }
            c = self.dom.previous_sibling(cid);
        }
        None
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let mut c = self.dom.next_sibling(self.id);
        while let Some(cid) = c {
            if self.is_element(cid) {
                return Some(self.wrap(cid));
            }
            c = self.dom.next_sibling(cid);
        }
        None
    }

    fn first_element_child(&self) -> Option<Self> {
        let mut c = self.dom.first_child(self.id);
        while let Some(cid) = c {
            if self.is_element(cid) {
                return Some(self.wrap(cid));
            }
            c = self.dom.next_sibling(cid);
        }
        None
    }

    fn is_html_element_in_html_document(&self) -> bool {
        // The crate uses this to pick which spelling of a type selector to hand [`has_local_name`]:
        // the ASCII-lowercased one in an HTML document, the as-authored one otherwise.
        //
        // Ruling: this stays "every element of an HTML document", NOT "every HTML-NAMESPACED element
        // of an HTML document" -- measured in Chrome 150 on `<svg><clipPath>`: `querySelectorAll`
        // matches `clipPath`, `clippath`, `CLIPPATH` and `ClipPath` alike, and `.matches()` agrees.
        // A type selector in an HTML document is ASCII case-insensitive for FOREIGN elements too;
        // `getElementsByTagName` is the algorithm that is case-sensitive for them (`clipPath` 1,
        // `clippath` 0), and that one lives in `collections.rs` and reads the namespace directly.
        !self.in_xml()
    }

    fn has_local_name(&self, local_name: &CssLocalName) -> bool {
        // Our tag is a `&str`; the selector's atom derefs to `&str` via `&*local_name.0`.
        let Some(tag) = self.dom.tag_name(self.id) else { return false };
        let want: &str = &local_name.0;
        // XML: exact, and the crate already handed us the as-authored spelling. HTML document: the
        // crate handed us the LOWERCASED selector, and the arena stores a foreign element's name in its
        // real case (`clipPath`), so the comparison has to fold. Equal bytes match in both, and
        // names that differ in more than ASCII case match in neither, so only the case where they
        // differ in case alone asks which document this is -- and with it the climb to the root.
        tag == want || (tag.eq_ignore_ascii_case(want) && !self.in_xml())
    }

    fn has_namespace(&self, ns: &CssNamespace) -> bool {
        // The element's own namespace, off the node. Ordinary un-prefixed selectors never reach here
        // (no namespace component is emitted); `@namespace`-prefixed ones now get a real answer
        // instead of "everything is HTML".
        match self.dom.namespace_uri(self.id) {
            Some(u) => &*ns.0 == u,
            None => ns.0 == Namespace::default(),
        }
    }

    fn is_same_type(&self, other: &Self) -> bool {
        // "Same type" is the same local name IN THE SAME NAMESPACE -- an HTML `<a>` and an SVG `<a>`
        // are different types for `:nth-of-type`.
        if self.dom.element_ns(self.id) != other.dom.element_ns(other.id) {
            return false;
        }
        match (self.dom.tag_name(self.id), other.dom.tag_name(other.id)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&CssNamespace>,
        local_name: &CssLocalName,
        operation: &AttrSelectorOperation<&CssAttrValue>,
    ) -> bool {
        let attrs = self.attr_list();
        // Our attributes carry no namespace, so only "any" or the empty namespace can match.
        let ns_ok = match ns {
            NamespaceConstraint::Any => true,
            NamespaceConstraint::Specific(want) => want.0 == Namespace::default(),
        };
        if !ns_ok {
            return false;
        }
        let want: &str = &local_name.0;
        attrs.iter().any(|(k, v)| k.as_str() == want && operation.eval_str(v))
    }

    fn has_attr_in_no_namespace(&self, local_name: &CssLocalName) -> bool {
        let want: &str = &local_name.0;
        self.attr_list().iter().any(|(k, _)| k.as_str() == want)
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &KorvusPseudoClass,
        context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        use KorvusPseudoClass::*;
        match pc {
            // Static, attribute-derived — answered truthfully (unchanged since T2'a/T2'b).
            Link | AnyLink(_) => self.is_any_link(),
            WebkitAny(list) => selectors::matching::matches_selector_list(list, self, context),
            // The STATE, not the attribute. `:checked` read `checked`/`selected`
            // attributes, so `input.checked = true` never matched it (kubernetes.io's nav, L19 E16),
            // and `:disabled` missed a control inside `<fieldset disabled>`. Each question is the
            // arena's (`forms.rs`), which IDL reads too, so script and the cascade cannot disagree.
            Enabled => self.dom.can_be_disabled(self.id) && !self.dom.actually_disabled(self.id),
            Disabled => self.dom.can_be_disabled(self.id) && self.dom.actually_disabled(self.id),
            Checked => self.dom.is_checked(self.id),
            Default => self.dom.is_default(self.id),
            Indeterminate => self.dom.is_indeterminate(self.id),
            PopoverOpen => self.dom.is_popover_open(self.id),
            Modal => self.dom.is_modal(self.id),
            PlaceholderShown => self.dom.placeholder_shown(self.id),
            Valid => self.dom.validity_state(self.id) == Some(true),
            Invalid => self.dom.validity_state(self.id) == Some(false),
            InRange => self.dom.range_state(self.id) == Some(true),
            OutOfRange => self.dom.range_state(self.id) == Some(false),
            Required => self.is_form_control() && self.has_bool_attr("required"),
            Optional => self.is_form_control() && !self.has_bool_attr("required"),
            ReadOnly => self.is_form_control() && self.has_bool_attr("readonly"),
            ReadWrite => self.is_form_control() && !self.has_bool_attr("readonly"),
            // The live pointer state, read from the arena's one hover chain. The
            // chain already holds the ancestors, so an ancestor answers `true` here without this
            // arm having to walk anything -- which is what makes `li:hover > ul` work.
            //
            // No arm for a disabled control and none for `pointer-events: none`, and both are
            // measured rather than overlooked (stock Chrome 150 over CDP): a `disabled`
            // `<input>`/`<button>` under the pointer DOES match `:hover` and DOES receive the
            // mouse events, and a `pointer-events: none` box is merely never the element the hit
            // test lands on -- it still matches `:hover` when a descendant that re-enables
            // pointer events is hit, because it is then in the chain as an ancestor. Both fall out
            // of "the chain is whatever was hit, plus its ancestors"; neither is a special case.
            Hover => self.dom.is_hovered(self.id),
            // the focus ring, as `Dom::set_focus` last moved it
            Focus => self.dom.is_focused(self.id),
            FocusVisible => self.dom.is_focus_visible(self.id),
            FocusWithin => self.dom.has_focus_within(self.id),
            Lang(range) => self.language().is_some_and(|lang| lang_matches(&lang, &range.0)),
            Dir(d) => self.direction() == d.0.as_str(),
            // T2'c: every other standard pseudo-class (dynamic UI/interaction state, form
            // validity, language/direction, custom-element/fullscreen/media state, …) has no live
            // signal in Korvus yet — it PARSED fine (see `parse_non_ts_pseudo_class`), it just
            // never matches. Only a genuinely unknown pseudo fails to parse at all.
            // `:defined` matches any BUILT-IN element (custom-element state "uncustomized"), connected or
            // not -- only an *undefined* custom element fails it (measured Chrome 150:
            // a detached `<div>` matches, an unregistered `<x-el>` does not). A built-in is any tag that
            // is not a valid custom element name: no ASCII hyphen, or one of the reserved hyphenated
            // names the custom-element grammar excludes (SVG/MathML built-ins). A REGISTERED custom
            // element should also match; that needs the registry the matcher cannot reach here, so a
            // hyphenated name stays unmatched -- no worse than before, correct for the unregistered case.
            UnsupportedValid(name) if name.0.as_str() == "defined" => {
                match self.dom.tag_name(self.id) {
                    Some(tag) => {
                        let t = tag.to_ascii_lowercase();
                        const RESERVED_BUILTIN: [&str; 8] = ["annotation-xml", "color-profile", "font-face", "font-face-src", "font-face-uri", "font-face-format", "font-face-name", "missing-glyph"];
                        !t.contains('-') || RESERVED_BUILTIN.contains(&t.as_str())
                    }
                    None => false,
                }
            }
            UnsupportedValid(_) => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &KorvusPseudoElement,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        false
    }

    /// Kept only while [`crate::invalidation::record_selector_flags`] runs: the element's own
    /// flags on it, its children's on its parent, as the crate's `for_self` / `for_parent` split them.
    fn apply_selector_flags(&self, flags: ElementSelectorFlags) {
        if !crate::invalidation::recording() {
            return;
        }
        let own = flags.for_self().bits() as u16;
        if own != 0 {
            crate::invalidation::record(self.id, own);
        }
        let children = flags.for_parent().bits() as u16;
        if children != 0 {
            if let Some(parent) = self.dom.parent(self.id) {
                crate::invalidation::record(parent, children);
            }
        }
    }

    fn is_link(&self) -> bool {
        matches!(self.dom.tag_name(self.id), Some("a" | "area" | "link"))
            && self.attr_list().iter().any(|(k, _)| k == "href")
    }

    fn is_html_slot_element(&self) -> bool {
        self.dom.is_html_slot_element(self.id)
    }

    /// The `<slot>` `self` is assigned to — required by `::slotted()`'s slot-assignment combinator.
    /// Delegates to korvus-dom's name-based assignment ([`Dom::assigned_slot`], "first same-name
    /// slot wins"), wrapped back into a `NodeRef`. `None` for a non-slottable, or a light child
    /// whose effective slot name matches no `<slot>` in its host's shadow tree.
    fn assigned_slot(&self) -> Option<Self> {
        self.dom.assigned_slot(self.id).map(|id| self.wrap(id))
    }

    fn has_id(&self, id: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        self.dom
            .get_attribute(self.id, "id")
            .is_some_and(|v| case_sensitivity.eq(v.as_bytes(), id.0.as_bytes()))
    }

    fn has_class(&self, name: &CssString, case_sensitivity: CaseSensitivity) -> bool {
        // The cached tokens. Quirks mode folds ASCII case, so it compares the tokens' folded
        // hashes: splitting the attribute for every test was a fifth of atlassian.com.
        if case_sensitivity == CaseSensitivity::CaseSensitive {
            return self.dom.has_class_token(self.id, name.1, &name.0);
        }
        self.dom.has_class_token_folded(self.id, name.2, &name.0)
    }

    fn has_custom_state(&self, _name: &CssString) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssString) -> Option<CssString> {
        None
    }

    fn is_part(&self, _name: &CssString) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        // `:empty` — no element children and no non-empty text/CDATA children.
        let mut c = self.dom.first_child(self.id);
        while let Some(cid) = c {
            match &self.dom.node(cid).kind {
                NodeKind::Element { .. } => return false,
                NodeKind::Text(t) | NodeKind::CData(t) if !t.is_empty() => return false,
                _ => {}
            }
            c = self.dom.next_sibling(cid);
        }
        true
    }

    fn is_root(&self) -> bool {
        // `:root` — the document element (an element whose parent is the Document node).
        match self.dom.parent(self.id) {
            Some(p) => matches!(self.dom.node(p).kind, NodeKind::Document),
            None => false,
        }
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Public(crate) helpers — the dormant entry points T2'b will wire in.
// ---------------------------------------------------------------------------

/// A compiled selector list, shared out of the per-thread [`compile`] cache. `Rc` so a cache hit is
/// a refcount bump, never a re-parse or a deep clone.
pub(crate) type CompiledSelector = Rc<CompiledQuery>;

/// A parsed selector list with what the query paths work out about it once, at compile, rather than
/// per call. Derefs to the list, so every matcher that takes a `&SelectorList` takes it unchanged.
pub(crate) struct CompiledQuery {
    pub list: SelectorList<KorvusSelectorImpl>,
    pub plan: QueryPlan,
}

impl std::ops::Deref for CompiledQuery {
    type Target = SelectorList<KorvusSelectorImpl>;

    fn deref(&self) -> &Self::Target {
        &self.list
    }
}

/// What a query can know of its selector list before it sees a node. Derived from the list alone,
/// like the parse it is cached with.
#[derive(Default)]
pub(crate) struct QueryPlan {
    /// Some selector of the list is `::slotted()` ([`list_is_slotted`]).
    pub slotted: bool,
    /// One per selector of the list, in its order.
    pub per: Box<[Planned]>,
    /// Some selector is answered or filtered before the crate's matcher: a key ([`Planned::prefilter`])
    /// or the direct matcher ([`Planned::fast`]). Without one the list goes straight to the crate's
    /// `matches_selector_list`: the planned loop around it cost such a list ~20 instructions a candidate
    /// (`div[class=cell]` before it was fast, measured with `perf stat`).
    pub planned: bool,
    /// For a list of one selector: an id its subject compound requires ([`narrow`]).
    pub subject_id: Option<CssString>,
    /// For a list of one selector: an id a proper ancestor of every element it matches carries
    /// ([`narrow`]).
    pub ancestor_id: Option<CssString>,
    /// For a list of one [`Planned::fast`] selector: up to four classes that proper ancestors of every
    /// element it matches carry, by the same rule ([`id_plan`]). A walk that counts them on its way down
    /// skips the candidates no ancestor of which has one ([`class_hash_present`]).
    pub ancestor_classes: Box<[CssString]>,
}

impl QueryPlan {
    fn of(list: &SelectorList<KorvusSelectorImpl>) -> QueryPlan {
        let per: Box<[Planned]> = list.slice().iter().map(Planned::of).collect();
        let planned = per.iter().any(|p| p.prefilter || p.fast.is_some());
        let (subject_id, ancestor_id, mut ancestor_classes) = match list.slice() {
            [sel] => id_plan(sel),
            _ => (None, None, Box::default()),
        };
        // Counted only for a selector the direct matcher takes: the reject-heavy descendant queries whose
        // candidates the per-parent memo cannot share (`.zz div` over deep chains). A crate-matched one
        // that passes its key is nearly always under its ancestor class (`.row > .cell + .cell`), and
        // the counting only cost it (+3 % instructions).
        if per.first().is_none_or(|p| p.fast.is_none()) {
            ancestor_classes = Box::default();
        }
        QueryPlan { slotted: list_is_slotted(list), per, planned, subject_id, ancestor_id, ancestor_classes }
    }
}

/// The ids a selector pins its matches with: one its subject compound requires, and one a compound
/// requires that can only be matched by a proper ancestor of the subject in the subject's own tree --
/// a compound whose right-hand combinator is child or descendant, with only child, descendant and
/// sibling combinators to its right. Each such step from an ancestor-or-self of the subject, or from a
/// previous sibling of one, lands on a proper ancestor of the subject; every step is a parent or
/// previous-sibling link, so the tree is the subject's. The one cross-tree hop the crate makes, to a
/// shadow host, enters only a compound that can match a featureless host (`:host`, `:scope`, or `:is()`
/// / `:where()` of those), after which the compounds to its left are matched in the host's tree: the
/// scan stops at the first compound that could be one. Selectors with `:host` or a pseudo-element,
/// `::slotted()` or `::part()` get none. The classes such ancestor compounds require come third, up
/// to four.
fn id_plan(sel: &parser::Selector<KorvusSelectorImpl>) -> (Option<CssString>, Option<CssString>, Box<[CssString]>) {
    use parser::{Combinator, Component};
    if sel.has_pseudo_element()
        || sel.is_slotted()
        || sel.is_part()
        || sel.iter_raw_match_order().any(|c| matches!(c, Component::Host(_)))
    {
        return (None, None, Box::default());
    }
    // The compound's first top-level id, its top-level classes into `classes` when asked, and whether
    // it is made only of what can match a featureless host.
    let scan = |iter: &mut parser::SelectorIter<'_, KorvusSelectorImpl>, classes: Option<&mut Vec<CssString>>| {
        let mut found = None;
        let mut classes = classes;
        let mut featureless = true;
        for c in iter.by_ref() {
            match c {
                Component::ID(id) if found.is_none() => found = Some(id.clone()),
                Component::Class(class) => {
                    if let Some(out) = classes.as_deref_mut() {
                        if out.len() < 4 && !out.contains(class) {
                            out.push(class.clone());
                        }
                    }
                }
                _ => {}
            }
            featureless &= matches!(
                c,
                Component::Host(_) | Component::Scope | Component::ImplicitScope | Component::Is(_) | Component::Where(_)
            );
        }
        (found, featureless)
    };
    let mut iter = sel.iter();
    let (subject, _) = scan(&mut iter, None);
    let mut ancestor = None;
    let mut classes = Vec::new();
    while let Some(combinator) = iter.next_sequence() {
        if !matches!(combinator, Combinator::Child | Combinator::Descendant | Combinator::NextSibling | Combinator::LaterSibling) {
            break;
        }
        let up = matches!(combinator, Combinator::Child | Combinator::Descendant);
        let (id, featureless) = scan(&mut iter, if up { Some(&mut classes) } else { None });
        if up && featureless {
            break;
        }
        if up && ancestor.is_none() {
            ancestor = id;
        }
    }
    (subject, ancestor, classes.into_boxed_slice())
}

/// Whether element `x` may carry `class`: its class HASH is among the element's (the folded ones in a
/// quirks document). Every class it has has its hash there, so `false` is certain.
#[inline]
pub(crate) fn class_hash_present(dom: &Dom, x: NodeId, class: &CssString, case: CaseSensitivity) -> bool {
    let (want, folded) = if case == CaseSensitivity::CaseSensitive { (class.1, false) } else { (class.2, true) };
    let hash = |t: &crate::tree::ClassToken| if folded { t.1 } else { t.0 };
    match &dom.at(x).classes {
        None => false,
        Some(ClassTokens::One(t)) => hash(t) == want,
        Some(ClassTokens::Many(ts)) => ts.iter().any(|t| hash(t) == want),
    }
}

/// Where a query under `t` has to look, from the plan's ids ([`id_plan`]) and the arena's id index.
pub(crate) enum Narrow {
    /// Walk this element's descendants: `t` itself, or the one element under it that carries the
    /// ancestor id.
    Walk(NodeId),
    /// Only this element can match: test it.
    One(NodeId),
    /// Nothing can match.
    Empty,
}

/// A query's candidates, cut down by an id: `querySelector("#grid")` was a walk to the element's
/// position, and `#root .x` on a React page a walk of every node outside `#root` too.
///
/// No-quirks only: there an id matches by exact bytes, which is the index's key, and the index holds
/// every element whose `id` is that value now (every writer reindexes: `push`, `set_attribute`,
/// `remove_attribute` -- what `getElementById` relies on). A quirks document folds id case, and it walks.
/// Candidates in another tree are dropped by their root, so a duplicate id elsewhere never counts.
/// Bounded: more than 16 elements ever indexed under the id (the index is not pruned on detach), or a
/// second candidate in the tree, and it walks as before. Every candidate still gets the full match.
pub(crate) fn narrow(dom: &Dom, q: &CompiledQuery, t: NodeId, facts: &Facts) -> Narrow {
    const MAX_CANDIDATES: usize = 16;
    if facts.quirks == QuirksMode::Quirks {
        return Narrow::Walk(t);
    }
    let plan = &q.plan;
    if let Some(id) = &plan.subject_id {
        let candidates = dom.elements_with_id(&id.0);
        if candidates.len() <= MAX_CANDIDATES {
            let tree = dom.root_of(t);
            let mut kept = None;
            let mut many = false;
            for &c in candidates {
                if c != t && dom.root_of(c) == tree && dom.contains(t, c) {
                    if kept.is_some() {
                        many = true;
                        break;
                    }
                    kept = Some(c);
                }
            }
            if !many {
                return kept.map_or(Narrow::Empty, Narrow::One);
            }
        }
    }
    if let Some(id) = &plan.ancestor_id {
        let candidates = dom.elements_with_id(&id.0);
        if candidates.len() <= MAX_CANDIDATES {
            let tree = dom.root_of(t);
            let mut kept = None;
            let mut many = false;
            for &c in candidates {
                if dom.root_of(c) == tree {
                    if kept.is_some() {
                        many = true;
                        break;
                    }
                    kept = Some(c);
                }
            }
            if !many {
                // Every match is a proper descendant of the one element in this tree that carries the id,
                // which cannot match itself (that would take a second one above it).
                return match kept {
                    None => Narrow::Empty,
                    Some(e) if e != t && dom.contains(t, e) => Narrow::Walk(e),
                    Some(e) if dom.contains(e, t) => Narrow::Walk(t),
                    Some(_) => Narrow::Empty,
                };
            }
        }
    }
    Narrow::Walk(t)
}

/// What one selector of a query's list asks of a candidate before the matcher runs.
pub(crate) struct Planned {
    /// A test the candidate must pass for the selector to match it.
    pub key: SubjectKey,
    /// The key IS the selector: one simple selector, no combinator.
    pub whole: bool,
    /// Whether to ask the key before the matcher when it is not [`Planned::whole`]: only an id or a
    /// class, which are cheap ([`SubjectKey::may_hold`]) and usually reject. A type or an attribute
    /// costs what the matcher's own test of it costs, so asking it first only doubled the work of the
    /// keys that pass (`div:not(.row)` and `div[class=cell]` 15-20 % slower in the native bench).
    pub prefilter: bool,
    /// The selector's compounds in match order, when the direct matcher ([`Matcher::fast`]) can answer
    /// it: ids, classes, types, `*`, attributes in no namespace, `:root`, `:scope` and `&`, joined by
    /// child and descendant combinators.
    pub fast: Option<Box<[FastCompound]>>,
}

/// One compound of a [`Planned::fast`] selector: its simple selectors, and the combinator to its left
/// (toward the next compound in match order), `None` for the leftmost.
pub(crate) struct FastCompound {
    comps: Box<[parser::Component<KorvusSelectorImpl>]>,
    next: Option<parser::Combinator>,
}

/// The compounds of `sel` if every simple selector and combinator in it is one [`Matcher::simple`] and
/// [`Matcher::ancestors`] answer exactly as the crate would.
fn fast_compounds(sel: &parser::Selector<KorvusSelectorImpl>) -> Option<Box<[FastCompound]>> {
    use parser::{Combinator, Component};
    if sel.has_pseudo_element() || sel.is_slotted() || sel.is_part() {
        return None;
    }
    let supported = sel.iter_raw_match_order().all(|c| {
        matches!(
            c,
            Component::ID(_)
                | Component::Class(_)
                | Component::LocalName(_)
                | Component::ExplicitUniversalType
                | Component::ExplicitAnyNamespace
                | Component::AttributeInNoNamespaceExists { .. }
                | Component::AttributeInNoNamespace { .. }
                | Component::Root
                | Component::Scope
                | Component::ParentSelector
                | Component::Combinator(Combinator::Child | Combinator::Descendant)
        )
    });
    if !supported {
        return None;
    }
    let mut out = Vec::new();
    let mut iter = sel.iter();
    loop {
        let comps: Box<[_]> = iter.by_ref().cloned().collect();
        let next = iter.next_sequence();
        out.push(FastCompound { comps, next });
        if next.is_none() {
            break;
        }
    }
    Some(out.into_boxed_slice())
}

impl Planned {
    fn of(sel: &parser::Selector<KorvusSelectorImpl>) -> Planned {
        use parser::Component;
        let whole = sel.len() == 1
            && matches!(
                sel.iter_raw_match_order().next(),
                Some(
                    Component::ID(_)
                        | Component::Class(_)
                        | Component::LocalName(_)
                        | Component::AttributeInNoNamespaceExists { .. }
                        | Component::ExplicitUniversalType
                )
            );
        let key = SubjectKey::of(sel);
        let fast = if whole { None } else { fast_compounds(sel) };
        let prefilter = whole || matches!(key, SubjectKey::Id(_) | SubjectKey::Class(_));
        Planned { key, whole, prefilter, fast }
    }
}

/// A simple selector of a selector's SUBJECT compound -- the one matched against the candidate itself --
/// in [`Selector::bucket`]'s order: an id, else a class, else an attribute, else the type. The compound
/// is an AND, so a candidate that fails the key fails the selector, and the key is asked with the very
/// call (and the same arguments) the crate's `matches_simple_selector` makes for that component.
///
/// It turns `querySelectorAll("style, link")` into two name compares per element, where each selector
/// was an entry into the matcher, and drops the `.cell .zz` candidates that are not `.zz` before any
/// walk up the tree.
pub(crate) enum SubjectKey {
    /// No usable key (none in the compound, or a pseudo-element, `::slotted()` or `::part()` selector,
    /// whose subject compound is not matched against the candidate alone).
    Any,
    Id(CssString),
    Class(CssString),
    /// `[name]` or `[name op value]` in no namespace: the attribute has to be there. `lower` is the
    /// spelling the crate passes in an HTML document (`select_name`); the two are one name for `[n=v]`.
    Attr { name: CssLocalName, lower: CssLocalName },
    Tag(parser::LocalName<KorvusSelectorImpl>),
}

impl SubjectKey {
    fn of(sel: &parser::Selector<KorvusSelectorImpl>) -> SubjectKey {
        use parser::Component;
        if sel.has_pseudo_element() || sel.is_slotted() || sel.is_part() {
            return SubjectKey::Any;
        }
        let (mut class, mut attr, mut tag) = (None, None, None);
        // `iter()` stops at the first combinator: the subject compound only, top-level components only.
        for component in sel.iter() {
            match component {
                Component::ID(id) => return SubjectKey::Id(id.clone()),
                Component::Class(c) if class.is_none() => class = Some(c.clone()),
                Component::AttributeInNoNamespaceExists { local_name, local_name_lower } if attr.is_none() => {
                    attr = Some((local_name.clone(), local_name_lower.clone()))
                }
                // The crate passes this name as written, in any document.
                Component::AttributeInNoNamespace { local_name, .. } if attr.is_none() => {
                    attr = Some((local_name.clone(), local_name.clone()))
                }
                Component::LocalName(name) if tag.is_none() => tag = Some(name.clone()),
                _ => {}
            }
        }
        match (class, attr, tag) {
            (Some(c), _, _) => SubjectKey::Class(c),
            (None, Some((name, lower)), _) => SubjectKey::Attr { name, lower },
            (None, None, Some(t)) => SubjectKey::Tag(t),
            (None, None, None) => SubjectKey::Any,
        }
    }

    /// [`SubjectKey::holds`], or a cheaper test it implies, for a key the matcher re-asks when it
    /// passes: a class is only looked for among the element's class HASHES (the folded ones in a quirks
    /// document), which hold the hash of every class it has. Comparing the bytes too, then again
    /// inside the matcher, cost the keys that rarely reject (`.cell:nth-child(7)` over 1,600 cells) about
    /// 10 % more than matching without a key.
    #[inline]
    fn may_hold(&self, el: &NodeRef<'_>, case: CaseSensitivity) -> bool {
        let SubjectKey::Class(c) = self else { return self.holds(el, case) };
        class_hash_present(el.dom, el.id, c, case)
    }

    /// Does `el` pass? `case` is the context's class and id case ([`Facts::case`]).
    #[inline]
    fn holds(&self, el: &NodeRef<'_>, case: CaseSensitivity) -> bool {
        match self {
            SubjectKey::Any => true,
            SubjectKey::Id(id) => el.has_id(id, case),
            SubjectKey::Class(c) => el.has_class(c, case),
            SubjectKey::Attr { name, lower } => {
                el.has_attr_in_no_namespace(selectors::matching::select_name(el, name, lower))
            }
            SubjectKey::Tag(t) => el.has_local_name(selectors::matching::select_name(el, &t.name, &t.lower_name)),
        }
    }
}

/// An invalid CSS selector. The webapi layer turns this into a DOM `SyntaxError` for
/// `querySelector` / `querySelectorAll` / `closest` / `matches`; the `getElementsBy*` family never
/// produces one (those methods do not parse selectors).
#[derive(Debug, Clone)]
pub struct SelectorParseError {
    /// The offending selector string, as the page passed it.
    pub selector: String,
    /// Parser diagnostic (for logs / messages; not part of the DOM-visible contract).
    pub detail: String,
}

impl std::fmt::Display for SelectorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}' is not a valid selector", self.selector)
    }
}

impl std::error::Error for SelectorParseError {}

/// The per-thread compile cache: the last string compiled, and every string compiled since the map
/// was last cleared. The one-entry memo answers the loop that asks the same selector each time
/// (`el.matches(s)`, `el.closest(s)` over a list) with a byte compare, not a hash of the string.
#[derive(Default)]
struct SelectorCache {
    last_key: String,
    last: Option<CompiledSelector>,
    map: rustc_hash::FxHashMap<Box<str>, CompiledSelector>,
}

impl SelectorCache {
    fn remember(&mut self, sel: &str, hit: &CompiledSelector) {
        self.last_key.clear();
        self.last_key.push_str(sel);
        self.last = Some(hit.clone());
    }
}

thread_local! {
    /// Compiled-selector cache: selector string -> parsed list. A selector's parse result is
    /// document-independent, so a per-thread map (each V8 isolate owns its thread) is safe and
    /// lets repeated `querySelectorAll(".x")` calls skip re-parsing. Only *successful* parses are
    /// cached; invalid selectors are rare and re-rejected cheaply.
    static SELECTOR_CACHE: RefCell<SelectorCache> = RefCell::new(SelectorCache::default());
}

/// Upper bound on distinct cached selectors before we drop the cache wholesale. Guards against a
/// page that synthesises unbounded distinct selector strings; real pages reuse a small set.
const SELECTOR_CACHE_CAP: usize = 8192;

/// Parse a selector string into a compiled [`SelectorList`]. `Err` on any invalid selector.
pub(crate) fn parse_list(sel: &str) -> Result<SelectorList<KorvusSelectorImpl>, String> {
    parse_list_in(sel, &[])
}

/// [`parse_list`] under a sheet's `@namespace` prefixes.
fn parse_list_in(sel: &str, namespaces: &[(String, String)]) -> Result<SelectorList<KorvusSelectorImpl>, String> {
    let mut input = cssparser::ParserInput::new(sel);
    let mut parser = cssparser::Parser::new(&mut input);
    SelectorList::parse(&KorvusSelectorParser { namespaces }, &mut parser, ParseRelative::No)
        .map_err(|e| format!("invalid selector '{sel}': {e:?}"))
}

/// Compile `sel`, serving a cached parse when one exists (parse-once-per-string). `Err` carries the
/// original selector so the caller can raise a `SyntaxError`.
///
/// A repeated string was a SipHash of it and a probe per call; the same string as the call before is
/// now a byte compare, and any other cached one an FxHash probe.
pub(crate) fn compile(sel: &str) -> Result<CompiledSelector, SelectorParseError> {
    let hit = SELECTOR_CACHE.with(|c| {
        let mut c = c.try_borrow_mut().ok()?;
        if c.last_key == sel {
            if let Some(last) = &c.last {
                return Some(last.clone());
            }
        }
        let found = c.map.get(sel).cloned()?;
        c.remember(sel, &found);
        Some(found)
    });
    if let Some(hit) = hit {
        return Ok(hit);
    }
    let list = parse_list(sel).map_err(|detail| SelectorParseError {
        selector: sel.to_string(),
        detail,
    })?;
    let plan = QueryPlan::of(&list);
    let rc: CompiledSelector = Rc::new(CompiledQuery { list, plan });
    SELECTOR_CACHE.with(|c| {
        if let Ok(mut c) = c.try_borrow_mut() {
            if c.map.len() >= SELECTOR_CACHE_CAP {
                c.map.clear();
            }
            c.map.insert(sel.into(), rc.clone());
            c.remember(sel, &rc);
        }
    });
    Ok(rc)
}

/// Match the element `id` against a compiled `selector_list`. Non-element nodes never match.
///
/// `scope` is the `:scope` / scoping root for element-scoped queries (`element.querySelector[All]`):
/// pass `Some(element)` there so `:scope` resolves to that element; pass `None` for document-level
/// queries and `matches` / `closest` (where `:scope` falls back to the document element).
///
/// Quirks/case: [`NodeRef::is_html_element_in_html_document`] already reports HTML-ness per node, so
/// the crate compares HTML type selectors ASCII-case-insensitively. Class and id are case-sensitive
/// except in a QUIRKS-mode document, where the crate folds them -- Chrome 150, no
/// doctype: `.CaseCls` matches `class=casecls`, in the cascade and in `querySelectorAll` alike.
pub(crate) fn matches_scoped(
    dom: &Dom,
    id: NodeId,
    selector_list: &SelectorList<KorvusSelectorImpl>,
    scope: Option<NodeId>,
) -> bool {
    if !matches!(dom.node(id).kind, NodeKind::Element { .. }) {
        return false;
    }
    let mut caches = selectors::context::SelectorCaches::default();
    let mut context = MatchingContext::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        quirks_mode_of(dom, id),
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    if let Some(scope_id) = scope {
        context.scope_element = Some(NodeRef { dom, id: scope_id }.opaque());
    }
    let element = NodeRef { dom, id };
    selectors::matching::matches_selector_list(selector_list, &element, &mut context)
}

/// What one query's matcher knows before it sees a candidate: the matching quirks mode and the class/id
/// case it implies, the `:scope` element and the shadow host. The same for every candidate: they all
/// share the anchor's root (a query's candidates are its descendants, `closest`'s its ancestors), and the
/// quirks mode is a function of the root alone ([`quirks_mode_of`]).
#[derive(Clone, Copy)]
pub(crate) struct Facts {
    pub quirks: QuirksMode,
    pub case: CaseSensitivity,
    pub scope: Option<NodeId>,
    pub host: Option<NodeId>,
}

impl Facts {
    pub(crate) fn of(dom: &Dom, anchor: NodeId, scope: Option<NodeId>, host: Option<NodeId>) -> Facts {
        let quirks = quirks_mode_of(dom, anchor);
        Facts { quirks, case: quirks.classes_and_ids_case_sensitivity(), scope, host }
    }
}

/// One QUERY's matcher: one `MatchingContext`, one quirks lookup and one set of selector caches
/// (`:nth-*` indices, `:has` results) for every candidate it tests. A context carries no state from
/// one top-level match to the next (every nesting the crate does is undone on the way out), and the
/// caches are keyed on node addresses the `&Dom` it holds keeps still, so sharing them across the
/// query is sound -- and they are dropped with it, never kept for a later DOM. Building the context
/// and asking `quirks_mode_of` per candidate was the per-element overhead of every query, a climb to
/// the root each in an arena that holds any quirks document.
pub(crate) struct Matcher<'a> {
    pub dom: &'a Dom,
    pub q: &'a CompiledQuery,
    pub facts: Facts,
    ctx: MatchingContext<'a, KorvusSelectorImpl>,
    /// Per selector (the first few of the list): the direct matcher's answer for the ancestors of the
    /// last candidate, keyed by that candidate's parent. See [`Matcher::fast`].
    memo: [Option<(Option<NodeId>, Tri)>; 4],
}

/// The direct matcher's answer for one selector: yes, no, or "ask the crate".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tri {
    Yes,
    No,
    Fallback,
}

/// The crate's `SelectorMatchingResult` for the child and descendant combinators, plus "ask the crate".
enum Walk {
    Matched,
    /// The compound failed: the crate's restart from the closest later sibling.
    Failed,
    RestartFromClosestDescendant,
    NotMatchedGlobally,
    Fallback,
}

impl Matcher<'_> {
    /// Does `id` match the query's list? [`matches_scoped`] (or [`matches_scoped_in_shadow`], with a
    /// host) with this query's context: the crate's `matches_selector_list`, an in-order OR of its
    /// selectors, with each selector's [`SubjectKey`] asked first and a [`Planned::whole`] key's answer
    /// taken as the selector's, and a [`Planned::fast`] selector answered by [`Matcher::fast`] unless
    /// that hands it back.
    pub(crate) fn matches(&mut self, id: NodeId) -> bool {
        if !self.dom.is_element(id) {
            return false;
        }
        let element = NodeRef { dom: self.dom, id };
        let q = self.q;
        if !q.plan.planned {
            return selectors::matching::matches_selector_list(&q.list, &element, &mut self.ctx);
        }
        let case = self.facts.case;
        for (i, (sel, planned)) in q.list.slice().iter().zip(q.plan.per.iter()).enumerate() {
            if planned.whole {
                if planned.key.holds(&element, case) {
                    return true;
                }
                continue;
            }
            if planned.prefilter && !planned.key.may_hold(&element, case) {
                continue;
            }
            if let Some(compounds) = &planned.fast {
                match self.fast(i, compounds, id) {
                    Tri::Yes => return true,
                    Tri::No => continue,
                    Tri::Fallback => {}
                }
            }
            if selectors::matching::matches_selector(sel, 0, None, &element, &mut self.ctx) {
                return true;
            }
        }
        false
    }

    /// The direct matcher: `matches_complex_selector_internal` for a selector of ids, classes, types,
    /// attributes, `:root` and `:scope` joined by child and descendant combinators, without a context
    /// per compound, the Kleene fold, the visited-link bookkeeping or the dispatch over every kind of
    /// simple selector -- each simple selector is the NodeRef call the crate makes ([`Matcher::simple`]).
    ///
    /// What the ancestors answer depends only on the candidate's parent, so it is remembered for the
    /// next candidate with the same parent: `.a .cell` over 1,600 cells in 40 rows climbs 40 times, not
    /// 1,600. Anything the subset does not cover -- a shadow root above, which the crate may cross to
    /// the host -- hands the selector back to the crate, which matches it from scratch.
    #[inline(never)]
    fn fast(&mut self, i: usize, compounds: &[FastCompound], el: NodeId) -> Tri {
        let Some(subject) = compounds.first() else { return Tri::Fallback };
        for c in subject.comps.iter() {
            match self.simple(c, el) {
                Some(true) => {}
                Some(false) => return Tri::No,
                None => return Tri::Fallback,
            }
        }
        let Some(combinator) = subject.next else { return Tri::Yes };
        let parent = self.dom.at(el).parent;
        if let Some(Some((p, answer))) = self.memo.get(i) {
            if *p == parent {
                return *answer;
            }
        }
        let answer = match self.ancestors(compounds, 1, combinator, el) {
            Walk::Matched => Tri::Yes,
            Walk::Fallback => Tri::Fallback,
            Walk::Failed | Walk::RestartFromClosestDescendant | Walk::NotMatchedGlobally => Tri::No,
        };
        if let Some(slot) = self.memo.get_mut(i) {
            *slot = Some((parent, answer));
        }
        answer
    }

    /// Compound `at` against `el`, then on up through its combinator.
    fn compound(&self, compounds: &[FastCompound], at: usize, el: NodeId) -> Walk {
        let Some(compound) = compounds.get(at) else { return Walk::Fallback };
        for c in compound.comps.iter() {
            match self.simple(c, el) {
                Some(true) => {}
                Some(false) => return Walk::Failed,
                None => return Walk::Fallback,
            }
        }
        match compound.next {
            None => Walk::Matched,
            Some(combinator) => self.ancestors(compounds, at + 1, combinator, el),
        }
    }

    /// The crate's combinator loop for a child or descendant combinator: try the parent, and for a
    /// descendant combinator each ancestor after it, until one matches the rest of the selector or the
    /// rest cannot match higher up. No parent element ends it; a shadow root as the parent is the
    /// crate's to handle.
    fn ancestors(&self, compounds: &[FastCompound], at: usize, combinator: parser::Combinator, el: NodeId) -> Walk {
        let mut cur = el;
        loop {
            cur = match self.dom.at(cur).parent {
                Some(p) if self.dom.is_element(p) => p,
                Some(p) if self.dom.is_shadow_root(p) => return Walk::Fallback,
                _ => return Walk::NotMatchedGlobally,
            };
            match self.compound(compounds, at, cur) {
                Walk::Matched => return Walk::Matched,
                Walk::NotMatchedGlobally => return Walk::NotMatchedGlobally,
                Walk::Fallback => return Walk::Fallback,
                Walk::Failed | Walk::RestartFromClosestDescendant => {
                    if combinator == parser::Combinator::Child {
                        return Walk::RestartFromClosestDescendant;
                    }
                }
            }
        }
    }

    /// One simple selector of the direct matcher's subset against `el`: the call, with the arguments,
    /// the crate's `matches_simple_selector` makes for it. `None` for anything else.
    #[inline]
    fn simple(&self, c: &parser::Component<KorvusSelectorImpl>, el: NodeId) -> Option<bool> {
        use parser::Component;
        use selectors::matching::{select_name, to_unconditional_case_sensitivity};
        let r = NodeRef { dom: self.dom, id: el };
        Some(match c {
            Component::ID(id) => r.has_id(id, self.facts.case),
            Component::Class(class) => r.has_class(class, self.facts.case),
            Component::LocalName(name) => r.has_local_name(select_name(&r, &name.name, &name.lower_name)),
            Component::ExplicitUniversalType | Component::ExplicitAnyNamespace => true,
            Component::AttributeInNoNamespaceExists { local_name, local_name_lower } => {
                r.has_attr_in_no_namespace(select_name(&r, local_name, local_name_lower))
            }
            Component::AttributeInNoNamespace { local_name, operator, value, case_sensitivity } => r.attr_matches(
                &NamespaceConstraint::Specific(&parser::namespace_empty_string::<KorvusSelectorImpl>()),
                local_name,
                &AttrSelectorOperation::WithValue {
                    operator: *operator,
                    case_sensitivity: to_unconditional_case_sensitivity(*case_sensitivity, &r),
                    value,
                },
            ),
            Component::Root => r.is_root(),
            // `opaque()` is the node's arena slot, so the crate's comparison is this id's.
            Component::Scope | Component::ParentSelector => match self.facts.scope {
                Some(scope) => scope == el,
                None => r.is_root(),
            },
            _ => return None,
        })
    }
}

/// Run `f` with a [`Matcher`] for `q` and `facts`. The caches live on this frame: always a fresh set,
/// never the cascade's thread-local ones, so nothing one query writes outlives it.
pub(crate) fn with_matcher<R>(dom: &Dom, q: &CompiledQuery, facts: Facts, f: impl FnOnce(&mut Matcher<'_>) -> R) -> R {
    let mut caches = selectors::context::SelectorCaches::default();
    let mut ctx = MatchingContext::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        facts.quirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    if let Some(scope) = facts.scope {
        ctx.scope_element = Some(NodeRef { dom, id: scope }.opaque());
    }
    if let Some(host) = facts.host {
        ctx.current_host = Some(NodeRef { dom, id: host }.opaque());
    }
    let mut m = Matcher { dom, q, facts, ctx, memo: [None; 4] };
    f(&mut m)
}

/// Does the element `id` in `dom` match any selector in `selector_list`? (`:scope` => document
/// element.) Non-element nodes never match. Convenience wrapper over [`matches_scoped`].
pub(crate) fn matches_one(
    dom: &Dom,
    id: NodeId,
    selector_list: &SelectorList<KorvusSelectorImpl>,
) -> bool {
    matches_scoped(dom, id, selector_list, None)
}

/// Match the element `id` against `selector_list` from *within the shadow tree hosted by
/// `shadow_host`* (Korvus GS ②-A2). This is the only entry point that pins
/// [`MatchingContext::current_host`], which is what lets a featureless `:host` selector resolve:
/// the crate's `matches_host` matches `:host` iff `element.opaque() == current_host`, so `:host`
/// matches exactly when `id == shadow_host`. Every other (document/element) query leaves
/// `current_host` `None`, so `:host` can never leak out of its own shadow scope.
///
/// The `shadow_host`'s `OpaqueElement` is the address of its arena slot (see [`NodeRef::opaque`]),
/// identical to the opaque computed for `id` when `id == shadow_host`, so the comparison is exact.
/// Non-element `id`s never match. `:scope` is intentionally left unset here (a shadow-scoped
/// `:host` query has no `:scope` root); wire a scope in ②-A3 if/when element-scoped shadow queries
/// need one.
pub(crate) fn matches_scoped_in_shadow(
    dom: &Dom,
    id: NodeId,
    selector_list: &SelectorList<KorvusSelectorImpl>,
    shadow_host: NodeId,
) -> bool {
    if !matches!(dom.node(id).kind, NodeKind::Element { .. }) {
        return false;
    }
    let mut caches = selectors::context::SelectorCaches::default();
    let mut context = MatchingContext::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        quirks_mode_of(dom, id),
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    context.current_host = Some(NodeRef { dom, id: shadow_host }.opaque());
    let element = NodeRef { dom, id };
    selectors::matching::matches_selector_list(selector_list, &element, &mut context)
}

/// The selector-matching quirks mode of the document `id` is in. A limited-quirks document matches as
/// a no-quirks one: only quirks mode folds class and id case.
pub(crate) fn quirks_mode_of(dom: &Dom, id: NodeId) -> QuirksMode {
    match dom.document_mode_of(id) {
        crate::tree::DocumentMode::Quirks => QuirksMode::Quirks,
        _ => QuirksMode::NoQuirks,
    }
}

/// Whether any selector in `list` contains a `::slotted()` pseudo-element (Korvus GS ②-B2). Such a
/// selector matches a slot's ASSIGNED light-DOM nodes, which are not shadow-tree descendants, so
/// [`Dom::shadow_descendants_matching`] uses this to decide whether to also enumerate slotted
/// candidates. The flag is precomputed by the `selectors` crate at parse time.
pub(crate) fn list_is_slotted(list: &SelectorList<KorvusSelectorImpl>) -> bool {
    list.slice().iter().any(|s| s.is_slotted())
}

// ---------------------------------------------------------------------------
// Tests — prove the engine PARSES + MATCHES correctly against the arena.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{matches_one, parse_list};
    use crate::tree::{Dom, NodeId};

    /// Build `<div id=a class=x><p class=y>hi</p><span>s</span></div>` by hand and return the ids.
    fn fixture() -> (Dom, NodeId, NodeId, NodeId) {
        let mut dom = Dom::empty();
        let root = dom.root();

        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "a");
        dom.set_attribute(div, "class", "x");
        dom.append_child(root, div);

        let p = dom.create_element("p");
        dom.set_attribute(p, "class", "y");
        dom.append_child(div, p);
        let hi = dom.create_text_node("hi");
        dom.append_child(p, hi);

        let span = dom.create_element("span");
        dom.append_child(div, span);
        let s = dom.create_text_node("s");
        dom.append_child(span, s);

        (dom, div, p, span)
    }

    fn matches(dom: &Dom, id: NodeId, sel: &str) -> bool {
        let list = parse_list(sel).unwrap_or_else(|e| panic!("parse failed for {sel:?}: {e}"));
        matches_one(dom, id, &list)
    }

    #[test]
    fn class_selector_matches_the_right_element() {
        let (dom, div, p, _span) = fixture();
        assert!(matches(&dom, div, ".x"), ".x should match div#a.x");
        assert!(!matches(&dom, p, ".x"), ".x must not match p.y");
    }

    #[test]
    fn id_selector_matches() {
        let (dom, div, p, _span) = fixture();
        assert!(matches(&dom, div, "#a"), "#a should match div#a");
        assert!(!matches(&dom, p, "#a"), "#a must not match p");
    }

    #[test]
    fn child_combinator_and_compound() {
        let (dom, _div, p, span) = fixture();
        assert!(matches(&dom, p, "div > p.y"), "div > p.y should match the p");
        assert!(
            !matches(&dom, span, "div > p.y"),
            "div > p.y must not match the span"
        );
    }

    #[test]
    fn descendant_combinator() {
        let (dom, _div, p, span) = fixture();
        assert!(matches(&dom, p, "div p"), "div p should match the descendant p");
        assert!(matches(&dom, span, "div span"), "div span should match the span");
    }

    #[test]
    fn structural_pseudo_first_and_last_child() {
        let (dom, _div, p, span) = fixture();
        assert!(matches(&dom, p, "p:first-child"), "p is the first child of div");
        assert!(
            matches(&dom, span, "span:last-child"),
            "span is the last child of div"
        );
        assert!(
            !matches(&dom, span, "span:first-child"),
            "span is NOT the first child"
        );
    }

    #[test]
    fn invalid_selector_is_an_error() {
        assert!(
            parse_list(">>>").is_err(),
            ">>> is not a valid selector and must fail to parse"
        );
    }

    #[test]
    fn html_tag_selector_is_ascii_case_insensitive_but_class_is_not() {
        // T2'b quirks/case contract: in an HTML (non-XML) document a mixed-case *type* selector
        // matches the lower-cased tag, while class/id stay case-sensitive under NoQuirks.
        let (dom, div, _p, _span) = fixture();
        assert!(super::matches_one(&dom, div, &super::compile("DIV").unwrap()), "DIV must match <div> (HTML tags are ASCII case-insensitive)");
        assert!(super::matches_one(&dom, div, &super::compile("div").unwrap()), "div must match <div>");
        assert!(super::matches_one(&dom, div, &super::compile(".x").unwrap()), ".x must match class=x");
        assert!(!super::matches_one(&dom, div, &super::compile(".X").unwrap()), ".X must NOT match class=x (classes are case-sensitive)");
    }

    // A quirks document matches classes without ASCII case, from the cached tokens' folded
    // hashes, and follows a rewrite of the attribute. The same element in a no-quirks document keeps case.
    #[test]
    fn a_quirks_document_matches_classes_without_ascii_case() {
        let m = |dom: &Dom, n: NodeId, s: &str| super::matches_one(dom, n, &super::compile(s).unwrap());
        let mut dom = Dom::empty();
        let root = dom.root();
        let div = dom.create_element("div");
        dom.set_attribute(div, "class", "FooBar baz");
        dom.append_child(root, div);
        assert!(!m(&dom, div, ".foobar"), "no-quirks keeps class case");
        dom.set_document_mode(root, crate::tree::DocumentMode::Quirks);
        for s in [".foobar", ".FOOBAR", ".FooBar", ".BAZ", ".baz", "div.fOoBaR.BaZ"] {
            assert!(m(&dom, div, s), "{s} must match class=\"FooBar baz\" in quirks mode");
        }
        for s in [".foo", ".foobarx", ".bar", ".ba"] {
            assert!(!m(&dom, div, s), "{s} must not match class=\"FooBar baz\"");
        }
        dom.set_attribute(div, "class", "Other");
        assert!(!m(&dom, div, ".foobar") && m(&dom, div, ".OTHER"), "the tokens follow the new value");
    }

    // A type selector folds ASCII case in an HTML document, foreign elements included (Chrome 150 on
    // `<svg><clipPath>`), and is exact in an XML one; `[type=hidden]` folds its value only for an
    // element of an HTML document. Asked again once the arena holds an XML document, which is when
    // the HTML answers stop being free and have to climb to the root.
    #[test]
    fn type_selectors_fold_ascii_case_only_in_html_documents() {
        let mut dom = Dom::parse("<!doctype html><body><svg><clipPath id=c></clipPath></svg><input type=HIDDEN id=i></body>");
        let clip = dom.get_element_by_id("c").unwrap();
        let input = dom.get_element_by_id("i").unwrap();
        let m = |dom: &Dom, n: NodeId, s: &str| matches_one(dom, n, &super::compile(s).unwrap());
        let html_answers = |dom: &Dom| {
            for s in ["clipPath", "clippath", "CLIPPATH", "svg > clippath"] {
                assert!(m(dom, clip, s), "{s} matches <clipPath> in an HTML document");
            }
            assert!(!m(dom, clip, "clip"), "and only names equal but for case");
            assert!(m(dom, input, "[type=hidden]"), "`type`'s value folds in an HTML document");
            assert!(m(dom, input, "INPUT"));
        };
        html_answers(&dom);
        let x = dom.parse_xml_into("<r><clipPath/><input type='HIDDEN'/></r>");
        assert_eq!(dom.descendants_matching(x, "clipPath").unwrap().len(), 1, "XML is exact");
        assert_eq!(dom.descendants_matching(x, "clippath").unwrap().len(), 0, "and does not fold");
        assert_eq!(dom.descendants_matching(x, "[type=hidden]").unwrap().len(), 0, "nor fold an attribute value");
        assert_eq!(dom.descendants_matching(x, "[type=HIDDEN]").unwrap().len(), 1);
        html_answers(&dom);
        assert_eq!(dom.root_of(dom.root()), dom.root(), "a document is its own root");
        let lone = dom.create_element("div");
        let child = dom.create_element("p");
        dom.append_child(lone, child);
        assert_eq!(dom.root_of(lone), lone, "a detached element is its own root");
        assert_eq!(dom.root_of(child), lone);
        assert_eq!(dom.root_of(NodeId::MAX), NodeId::MAX, "an id that names no node reads as detached");
    }

    #[test]
    fn compile_caches_and_reuses_the_parse() {
        // Two compiles of the same string hand back the same Rc (cache hit, no re-parse).
        let a = super::compile("div > p.y").unwrap();
        let b = super::compile("div > p.y").unwrap();
        assert!(std::rc::Rc::ptr_eq(&a, &b), "identical selector strings must share one compiled list");
    }

    #[test]
    fn scope_pseudo_resolves_to_the_scoping_element() {
        // `:scope > p.y` scoped at the div matches the div's direct child p, not from elsewhere.
        let (dom, div, p, span) = fixture();
        let list = super::compile(":scope > p.y").unwrap();
        assert!(super::matches_scoped(&dom, p, &list, Some(div)), ":scope>p.y (scope=div) must match the child p");
        assert!(!super::matches_scoped(&dom, span, &list, Some(div)), ":scope>p.y must not match the span");
    }

    // -----------------------------------------------------------------------
    // T2'c — standard-but-unsupported pseudo-classes PARSE and never MATCH;
    // only a genuinely unknown pseudo is still a `SyntaxError`.
    // -----------------------------------------------------------------------

    #[test]
    fn standard_unsupported_pseudo_classes_parse_and_never_match() {
        // Real, standard CSS pseudo-classes Korvus cannot evaluate yet (no interaction/render/
        // validation engine backing them). A real browser parses these fine and simply matches
        // nothing — it never raises a SyntaxError. `matches()` panics loudly if parsing fails, so
        // a passing loop iteration proves BOTH halves of the contract for that selector.
        let (dom, div, _p, _span) = fixture();
        for sel in [
            ":focus-within",
            ":placeholder-shown",
            ":valid",
            ":invalid",
            // `:hover` WAS in this list and is not any more: it has a live signal now,
            // and its own tests are below. Leaving it here would have kept passing -- the fixture's
            // hover chain is empty, so it matches nothing -- which is exactly the sort of test that
            // looks like coverage of a feature while asserting the opposite of it.
            ":focus-visible",
            ":target",
            ":target-within",
            // `:defined` is NOT here any more: a built-in element matches it; its
            // own test is `defined_matches_builtin_elements` below.
            ":fullscreen",
            ":indeterminate",
            ":read-only", // div is not a form control, so :read-only's attribute check is false too
        ] {
            assert!(
                !matches(&dom, div, sel),
                "{sel:?} must parse without a SyntaxError and never match"
            );
        }
    }

    #[test]
    fn defined_matches_builtin_elements_not_undefined_custom() {
        // Measured Chrome 150: a built-in element matches `:defined` connected or
        // not; an unregistered autonomous custom element does not; the reserved hyphenated built-ins do.
        let mut dom = Dom::empty();
        let div = dom.create_element("div");
        let custom = dom.create_element("x-undefined-el");
        let reserved = dom.create_element("font-face");
        assert!(matches(&dom, div, ":defined"), "a detached built-in <div> matches :defined");
        assert!(!matches(&dom, custom, ":defined"), "an unregistered <x-undefined-el> does not match :defined");
        assert!(matches(&dom, reserved, ":defined"), "<font-face> is a reserved built-in and matches :defined");
    }

    // -----------------------------------------------------------------------
    // `:hover` reads the arena's one hover chain.
    //
    // Every expectation below is what stock Chrome 150.0.7871.46 answered when the same shapes
    // were driven over CDP (`Input.dispatchMouseEvent` mouseMoved), not what the spec says.
    // -----------------------------------------------------------------------

    #[test]
    fn hover_matches_nothing_until_the_pointer_has_been_somewhere() {
        let (dom, div, p, span) = fixture();
        assert!(dom.hover_chain().is_empty(), "a fresh arena has no pointer on it");
        for id in [div, p, span] {
            assert!(!matches(&dom, id, ":hover"), ":hover must match nothing with an empty chain");
        }
    }

    #[test]
    fn hover_matches_the_whole_chain_not_just_the_hit_element() {
        // Chrome, pointer inside the innermost box: querySelectorAll(":hover") answered
        // [html, body, #outer, #mid, #inner] -- the hit element AND its ancestors.
        let (mut dom, div, p, span) = fixture();
        dom.set_hover_chain(vec![div, p]);
        assert!(matches(&dom, p, ":hover"), "the hit element is hovered");
        assert!(matches(&dom, div, ":hover"), "its ancestor is hovered too -- this is the point");
        assert!(!matches(&dom, span, ":hover"), "a sibling outside the chain is not hovered");
    }

    #[test]
    fn hover_drives_the_descendant_combinator_that_opens_a_menu() {
        // `li:hover > ul` is the case a single-element implementation silently fails.
        let (mut dom, div, p, _span) = fixture();
        dom.set_hover_chain(vec![div, p]);
        assert!(matches(&dom, p, "div:hover > p"), "the child of a hovered ancestor must match");
        assert!(matches(&dom, p, ":hover > :hover"), "both ends of the chain are hovered");
        dom.set_hover_chain(vec![]);
        assert!(!matches(&dom, p, "div:hover > p"), "and stops matching when the pointer leaves");
    }

    #[test]
    fn setting_the_same_chain_twice_does_not_bump_the_revision() {
        // The revision is what invalidates the cached layout, so a pointer moving inside one
        // element must not bump it -- Chrome restyles nothing there either.
        let (mut dom, div, p, _span) = fixture();
        assert!(dom.set_hover_chain(vec![div, p]), "the first set is a change");
        let after = dom.revision();
        assert!(!dom.set_hover_chain(vec![div, p]), "an identical chain is not a change");
        assert_eq!(dom.revision(), after, "and must not invalidate anything");
        assert!(dom.set_hover_chain(vec![div]), "a shorter chain is a change");
        assert!(dom.revision() > after, "which must invalidate style");
    }

    #[test]
    fn a_disabled_control_under_the_pointer_is_hovered() {
        // Measured: Chrome 150 matched `#disin:hover` / `#disbtn:hover` on a `disabled`
        // <input> and a `disabled` <button>, and dispatched mouseover/enter/move on both.
        let mut dom = Dom::empty();
        let root = dom.root();
        let input = dom.create_element("input");
        dom.set_attribute(input, "disabled", "");
        dom.append_child(root, input);
        dom.set_hover_chain(vec![input]);
        assert!(matches(&dom, input, "input:hover"), "a disabled control still hovers");
        assert!(matches(&dom, input, ":disabled:hover"), "and is disabled at the same time");
    }

    /// `:lang()` matches the element's language -- its nearest `lang` -- by extended filtering, and `:dir()` its nearest
    /// `dir` (else `ltr`), as Chrome 152 does: apple.com styles its Korean banner with `:lang(ko)`, which never matched.
    #[test]
    fn lang_and_dir_match_the_language_and_direction_an_element_inherits() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let outer = dom.create_element("div");
        dom.set_attribute(outer, "lang", "ko-KR");
        dom.set_attribute(outer, "dir", "rtl");
        dom.append_child(root, outer);
        let inner = dom.create_element("span");
        dom.append_child(outer, inner);
        let other = dom.create_element("p");
        dom.append_child(root, other);
        for sel in [":lang(ko)", ":lang(ko-KR)", ":lang(KO)", ":dir(rtl)"] {
            assert!(matches(&dom, inner, sel), "{sel} matches what the span inherits");
        }
        for sel in [":lang(en)", ":lang(ko-US)", ":lang(k)", ":dir(ltr)"] {
            assert!(!matches(&dom, inner, sel), "{sel} does not");
        }
        // no language at all matches no range; the direction is `ltr`
        assert!(!matches(&dom, other, ":lang(ko)"));
        assert!(matches(&dom, other, ":dir(ltr)"));
        // Chrome 152 takes one identifier: no argument, a string, a list or a wildcard is a parse error
        for sel in [":lang()", ":lang('ko')", ":lang(en, fr)", ":lang(*-KR)"] {
            assert!(parse_list(sel).is_err(), "{sel} must fail to parse");
        }
    }

    #[test]
    fn checked_matches_only_a_checked_input() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let checked = dom.create_element("input");
        dom.set_attribute(checked, "type", "checkbox");
        dom.set_attribute(checked, "checked", "");
        dom.append_child(root, checked);
        let unchecked = dom.create_element("input");
        dom.set_attribute(unchecked, "type", "checkbox");
        dom.append_child(root, unchecked);
        // A text input has no checkedness to show: Chrome 150 answers `false` for
        // `<input checked>`.matches(":checked").
        let text = dom.create_element("input");
        dom.set_attribute(text, "checked", "");
        dom.append_child(root, text);
        let scripted = dom.create_element("input");
        dom.set_attribute(scripted, "type", "checkbox");
        dom.append_child(root, scripted);
        dom.set_checkedness(scripted, true);

        assert!(matches(&dom, checked, "input:checked"), ":checked must match <input type=checkbox checked>");
        assert!(!matches(&dom, unchecked, "input:checked"), ":checked must not match an unchecked checkbox");
        assert!(!matches(&dom, text, "input:checked"), ":checked must not match a text input");
        assert!(matches(&dom, scripted, "input:checked"), ":checked must match the checkedness script set");
        dom.remove_attribute(checked, "checked");
        assert!(!matches(&dom, checked, "input:checked"), "an untouched checkbox follows its attribute");
        dom.remove_attribute(scripted, "checked");
        assert!(matches(&dom, scripted, "input:checked"), "a dirty checkbox does not");
    }

    #[test]
    fn disabled_matches_a_disabled_input() {
        let mut dom = Dom::empty();
        let root = dom.root();
        let input = dom.create_element("input");
        dom.set_attribute(input, "disabled", "");
        dom.append_child(root, input);

        assert!(matches(&dom, input, "input:disabled"), ":disabled must match <input disabled>");
        assert!(!matches(&dom, input, "input:enabled"), "a disabled input must not match :enabled");
    }

    #[test]
    fn genuinely_unknown_pseudo_class_is_still_a_parse_error() {
        assert!(
            parse_list(":zzzz-not-real").is_err(),
            ":zzzz-not-real is not a real CSS pseudo-class and must still fail to parse"
        );
    }

    #[test]
    fn unsupported_pseudo_class_in_a_live_query_returns_empty_not_an_error() {
        // The webapi-visible end-to-end contract: `querySelectorAll("input:focus-within")` on a
        // real page must come back with an (empty) result, never throw.
        let mut dom = Dom::empty();
        let root = dom.root();
        let input = dom.create_element("input");
        dom.append_child(root, input);

        let results = dom
            .query_selector_all("input:focus-within")
            .expect("input:focus-within is a valid selector and must not raise a SyntaxError");
        assert!(results.is_empty(), "input:focus-within must match nothing (no live focus signal)");
    }

    #[test]
    fn known_pseudo_element_parses_but_unknown_one_errors() {
        // `querySelector`/`querySelectorAll` can never return a pseudo-element, but a *known* one
        // must still parse (matches real browsers); only a made-up `::name` is a SyntaxError.
        assert!(parse_list("div::before").is_ok(), "div::before is a real pseudo-element and must parse");
        assert!(parse_list("::first-letter").is_ok(), "::first-letter is a real pseudo-element and must parse");
        assert!(
            parse_list("div::not-a-real-pseudo-element").is_err(),
            "an unknown pseudo-element must still fail to parse"
        );
    }
}

// ---------------------------------------------------------------------------
// Shadow-aware traversal + encapsulation boundary (Korvus GS ②-A2).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod shadow_tests {
    use super::{compile, matches_scoped, matches_scoped_in_shadow, NodeRef};
    use crate::tree::{Dom, NodeId, ShadowRootMode};
    use selectors::Element; // brings `containing_shadow_host` / `parent_node_is_shadow_root` into scope

    /// Build:
    ///   Document
    ///     └─ <div id=h>            (the shadow host, `h`)
    ///          ├─ <span class=light>   (a LIGHT child of the host)
    ///          └╌ #shadow-root       (attached to h; lives only in the shadow maps)
    ///                └─ <p class=shadow>  (a SHADOW child)
    /// Returns `(dom, h, span_light, shadow_root, p_shadow)`.
    fn shadow_fixture() -> (Dom, NodeId, NodeId, NodeId, NodeId) {
        let mut dom = Dom::empty();
        let root = dom.root();

        let h = dom.create_element("div");
        dom.set_attribute(h, "id", "h");
        dom.append_child(root, h);

        let span = dom.create_element("span");
        dom.set_attribute(span, "class", "light");
        dom.append_child(h, span);

        let shadow_root = dom.attach_shadow(h, ShadowRootMode::Open).expect("attach should succeed");

        let p = dom.create_element("p");
        dom.set_attribute(p, "class", "shadow");
        dom.append_child(shadow_root, p);

        (dom, h, span, shadow_root, p)
    }

    #[test]
    fn document_query_selector_does_not_pierce_into_the_shadow_tree() {
        // Encapsulation: a document-scoped query must never reach shadow content, because the flat
        // arena scan filters on `is_in_document`, which is false for a shadow node.
        let (dom, _h, _span, _shadow_root, _p) = shadow_fixture();
        assert_eq!(
            dom.query_selector(".shadow").expect("valid selector"),
            None,
            "document.querySelector(\".shadow\") must not see shadow content"
        );
        // The light child is of course still reachable from the document.
        assert!(
            dom.query_selector(".light").expect("valid selector").is_some(),
            "the host's light child stays reachable from the document"
        );
    }

    #[test]
    fn shadow_root_scoped_query_walks_the_shadow_tree() {
        // A descendant query scoped at the shadow root walks exactly the shadow tree.
        let (dom, _h, _span, shadow_root, p) = shadow_fixture();
        assert_eq!(
            dom.descendants_matching(shadow_root, ".shadow").expect("valid selector"),
            vec![p],
            "descendants_matching(shadow_root, \".shadow\") must find the shadow <p>"
        );
    }

    #[test]
    fn shadow_root_scoped_query_never_escapes_to_the_light_tree() {
        // The light child (`.light`) is NOT a descendant of the shadow root, so a shadow-scoped
        // query must not find it -- the walk follows `children` and the host's light children are
        // not in the shadow root's subtree.
        let (dom, _h, _span, shadow_root, _p) = shadow_fixture();
        assert!(
            dom.descendants_matching(shadow_root, ".light").expect("valid selector").is_empty(),
            "a shadow-root-scoped query must not escape into the light tree"
        );
    }

    #[test]
    fn element_query_on_the_host_does_not_see_its_own_shadow() {
        // element.querySelector on the host walks the host's LIGHT children only; the shadow root is
        // not among them, so the host's own shadow content is invisible to it.
        let (dom, h, _span, _shadow_root, _p) = shadow_fixture();
        assert!(
            dom.descendants_matching(h, ".shadow").expect("valid selector").is_empty(),
            "element.querySelector on the host must not see its own shadow tree"
        );
    }

    #[test]
    fn shadow_descendants_matching_is_scoped_and_strict() {
        // The public shadow-scoped query (Korvus GS ②-A3) backing ShadowRoot.querySelector[All]:
        // it walks exactly the shadow subtree and raises on an invalid selector.
        let (dom, h, _span, shadow_root, p) = shadow_fixture();

        assert_eq!(
            dom.shadow_descendants_matching(shadow_root, ".shadow").expect("valid selector"),
            vec![p],
            "shadow_descendants_matching must find the shadow <p>"
        );
        // Encapsulation: the host's light child is not in the shadow subtree.
        assert!(
            dom.shadow_descendants_matching(shadow_root, ".light").expect("valid selector").is_empty(),
            "a shadow-scoped query never escapes into the light tree"
        );
        // A non-shadow-root node has no recorded host -> empty result, never a panic.
        assert!(
            dom.shadow_descendants_matching(h, "*").expect("valid selector").is_empty(),
            "querying a node that is not a shadow root yields nothing"
        );
        // Invalid selector still raises (SelectorParseError) so the webapi layer can map a SyntaxError.
        assert!(
            dom.shadow_descendants_matching(shadow_root, "!!!").is_err(),
            "an invalid selector must raise, like every other selector-taking query"
        );
    }

    #[test]
    fn shadow_including_root_crosses_the_host_edge() {
        let (dom, _h, span, shadow_root, p) = shadow_fixture();
        let root = dom.root();
        // A shadow node's LIGHT root is its shadow root; its shadow-INCLUDING root is the document.
        assert_eq!(dom.root_of(p), shadow_root, "light root of shadow content is the shadow root");
        assert_eq!(dom.shadow_including_root(p), root, "shadow-including root crosses to the document");
        // For a light node the two roots agree.
        assert_eq!(dom.shadow_including_root(span), root);
    }

    #[test]
    fn shadow_boundary_element_hooks_are_wired() {
        // The `selectors::Element` shadow hooks used by the crate's boundary logic.
        let (dom, h, span, _shadow_root, p) = shadow_fixture();

        let p_ref = NodeRef { dom: &dom, id: p };
        assert_eq!(
            p_ref.containing_shadow_host().map(|n| n.id),
            Some(h),
            "containing_shadow_host(p) must be the host h"
        );
        assert!(p_ref.parent_node_is_shadow_root(), "p's parent is the shadow root");

        // A light node has no containing shadow host and its parent is an ordinary element.
        let span_ref = NodeRef { dom: &dom, id: span };
        assert_eq!(
            span_ref.containing_shadow_host().map(|n| n.id),
            None,
            "a light node has no containing shadow host"
        );
        assert!(!span_ref.parent_node_is_shadow_root(), "the light span's parent is the host element, not a shadow root");
    }

    #[test]
    fn is_connected_is_shadow_including() {
        // A shadow node is connected iff its host chain reaches the document.
        let (mut dom, h, span, shadow_root, p) = shadow_fixture();
        let root = dom.root();

        assert!(dom.is_connected(p), "shadow <p> is connected while its host is in the document");
        assert!(dom.is_connected(span), "the light child is connected");
        assert!(dom.is_connected(h), "the host is connected");
        // is_in_document (light-only) is deliberately false for the shadow node.
        assert!(!dom.is_in_document(p), "is_in_document (light-only) is false for shadow content");
        assert!(dom.is_in_document(span), "is_in_document is true for a light child");

        // Detach the host from the document; the whole shadow subtree becomes disconnected.
        dom.remove_child(root, h);
        assert!(!dom.is_connected(h), "a detached host is not connected");
        assert!(!dom.is_connected(p), "shadow content of a detached host is not connected");
        // The shadow root still belongs to the (now detached) host -- its maps are untouched.
        assert_eq!(dom.shadow_host_of(shadow_root), Some(h));
    }

    #[test]
    fn host_selector_matches_only_the_host_in_its_own_shadow_scope() {
        // `:host` resolves to the shadow host when matched in that host's shadow scope, and matches
        // nothing anywhere else (document scope, or a non-host element).
        let (dom, h, span, _shadow_root, _p) = shadow_fixture();
        let host_sel = compile(":host").expect(":host must parse now that parse_host() is enabled");

        assert!(
            matches_scoped_in_shadow(&dom, h, &host_sel, h),
            ":host (scoped to h's shadow) must match the host h"
        );
        assert!(
            !matches_scoped_in_shadow(&dom, span, &host_sel, h),
            ":host must not match a non-host element even in shadow scope"
        );
        // In ordinary document scope there is no current host, so :host matches nothing (it parses,
        // it just never matches -- no SyntaxError, no leak).
        assert!(
            !matches_scoped(&dom, h, &host_sel, None),
            ":host must never match in document scope"
        );
    }

    #[test]
    fn host_selector_parses_in_a_document_query_and_matches_nothing() {
        // The webapi-visible contract: querySelector(":host") on the document parses fine and comes
        // back empty rather than raising a SyntaxError.
        let (dom, _h, _span, _shadow_root, _p) = shadow_fixture();
        assert_eq!(
            dom.query_selector(":host").expect(":host is a valid selector and must not raise"),
            None,
            ":host must match nothing at the document level"
        );
    }
}

// ---------------------------------------------------------------------------
// The query paths: the compile cache, its plans, and the matcher a query walks with.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod query_tests {
    use super::{compile, matches_scoped, matches_scoped_in_shadow, SubjectKey};
    use crate::tree::{Dom, DocumentMode, ElementNs, NodeId, ShadowRootMode};
    use std::rc::Rc;

    /// xorshift64: a seeded, dependency-free source for the random trees below.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
            xs[self.below(xs.len())]
        }
    }

    const TAGS: [&str; 8] = ["div", "span", "p", "a", "li", "ul", "section", "input"];
    const CLASSES: [&str; 6] = ["a", "b", "A", "cell", "x y", "a cell"];
    const IDS: [&str; 3] = ["i1", "i2", "I1"];

    fn element(dom: &mut Dom, rng: &mut Rng) -> NodeId {
        let e = match rng.below(14) {
            0 => dom.create_element_ns("clipPath", ElementNs::Svg),
            1 => dom.create_element_ns("STYLE", ElementNs::Html),
            _ => dom.create_element(rng.pick(&TAGS)),
        };
        if rng.below(2) == 0 {
            let c = rng.pick(&CLASSES);
            dom.set_attribute(e, "class", c);
        }
        if rng.below(6) == 0 {
            let i = rng.pick(&IDS);
            dom.set_attribute(e, "id", i);
        }
        if rng.below(5) == 0 {
            let h = rng.pick(&["/x", "http://y", ""]);
            dom.set_attribute(e, "href", h);
        }
        if rng.below(5) == 0 {
            let v = rng.pick(&["v", "V"]);
            dom.set_attribute(e, "data-x", v);
        }
        if rng.below(7) == 0 {
            let t = rng.pick(&["hidden", "HIDDEN"]);
            dom.set_attribute(e, "type", t);
        }
        e
    }

    /// `n` random nodes under `top`: elements, empty and non-empty text, comments. Parents are drawn
    /// mostly from the last few elements made, so the trees are deep as well as wide.
    fn populate(dom: &mut Dom, rng: &mut Rng, top: NodeId, n: usize) {
        let mut parents = vec![top];
        for _ in 0..n {
            let parent = if rng.below(2) == 0 {
                parents[parents.len() - 1 - rng.below(parents.len().min(6))]
            } else {
                parents[rng.below(parents.len())]
            };
            let node = match rng.below(10) {
                0 => dom.create_text_node(""),
                1 => dom.create_text_node("t"),
                2 => dom.create_comment("c"),
                _ => element(dom, rng),
            };
            dom.append_child(parent, node);
            if dom.is_element(node) {
                parents.push(node);
            }
        }
    }

    fn first_tagged(dom: &Dom, root: NodeId, tag: &str) -> NodeId {
        dom.descendant_elements(root).into_iter().find(|&n| dom.tag_name(n) == Some(tag)).expect("tag present")
    }

    /// The trees a query can run over, in one arena: documents (no-quirks, quirks, XML), a detached
    /// subtree, and a shadow tree with slots and slotted light children.
    struct World {
        dom: Dom,
        roots: Vec<NodeId>,
        shadows: Vec<NodeId>,
    }

    fn world(seed: u64, everything: bool) -> World {
        let mut rng = Rng(seed);
        let mut dom = Dom::parse("<!doctype html><html><head></head><body></body></html>");
        let doc = dom.root();
        let body = first_tagged(&dom, doc, "body");
        populate(&mut dom, &mut rng, body, 300);
        let mut roots = vec![doc];
        if everything {
            let q = dom.parse_into("<p>");
            dom.set_document_mode(q, DocumentMode::Quirks);
            let qbody = first_tagged(&dom, q, "body");
            populate(&mut dom, &mut rng, qbody, 300);
            let x = dom.parse_xml_into("<root/>");
            let xroot = dom.descendant_elements(x)[0];
            populate(&mut dom, &mut rng, xroot, 300);
            roots.extend([q, x]);
        }
        let detached = dom.create_element("div");
        populate(&mut dom, &mut rng, detached, 300);
        roots.push(detached);
        // A shadow tree: a default and a named slot among random content, and light children some of
        // which are assigned to the named one.
        let host = dom.create_element("div");
        dom.set_attribute(host, "class", "a");
        dom.append_child(body, host);
        let sr = dom.attach_shadow(host, ShadowRootMode::Open).expect("attach");
        populate(&mut dom, &mut rng, sr, 200);
        let slot = dom.create_element("slot");
        let named = dom.create_element("slot");
        dom.set_attribute(named, "name", "s1");
        let mid = dom.descendant_elements(sr).get(5).copied().unwrap_or(sr);
        dom.append_child(mid, named);
        dom.append_child(sr, slot);
        for i in 0..12 {
            let e = element(&mut dom, &mut rng);
            if i % 3 == 0 {
                dom.set_attribute(e, "slot", "s1");
            }
            dom.append_child(host, e);
            if i % 4 == 0 {
                let t = dom.create_text_node("t");
                dom.append_child(host, t);
            }
        }
        World { dom, roots, shadows: vec![sr] }
    }

    /// Every selector shape the query paths treat differently, and the invalid ones.
    const CORPUS: &[&str] = &[
        ".a .cell", ".zz .cell", "div > .cell", "ul > li", "* > *", "*", "div", "DIV", "clippath", "clipPath",
        "STYLE", "style", ".A", "#i1", "#I1", "div#i1", "#i1 .a", "#i1 > *", ".b #i2", "p + p", "p ~ span",
        ".a + .b .cell", ".b ~ #i1 span", ":scope > div", "& > div", ":scope .a", ":root", ":root > body",
        ":empty", "[data-x]", "[DATA-X]", "[data-x=v]", "[data-x=v i]", "[type=hidden]", "a[href^='/']",
        "div[class=cell]", ":not(.a)", "div:not(.a) span", "li:nth-child(2)", ":nth-child(odd)", ":has(> .a)",
        ":is(.a, .b) span", ":where(#i1) .a", ":-webkit-any(li:nth-child(2))", ":-webkit-any(:has(> .a))",
        ":-webkit-any(.a, .b) > *", ":host", ":host .a", "::slotted(*)", "::slotted(.a)", "p::before",
        "style, link", "a, button, [tabindex]", ".a, #i1, span", "*|a", "|a", "input:checked", "a:hover",
        ".a, :not(.b)", "ul > li, li:nth-child(2)", "#i1 #i2", "#i2 ~ .a .cell", ".x.y", ".cell.a > *",
        "#i1 > div[data-x=v i]", ":root .a", "a[href^='/'] span", "div > p", ".a > .a .a", ".a .a > .cell", "* *",
        "section ul li", ":scope .a .cell", "& .a > *", ".a, .a .cell", "[data-x] > [type=hidden]", ".cell .a",
        ".a > .b .cell", ".A .cell", ".b .a .cell", ".a .b .A .x .cell", ".a.b .cell", "#i1 .a .cell", ".x.y > * .a",
        "div >>", "", ":zzz", "svg|a",
    ];

    fn descendants(dom: &Dom, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        dom.for_each_descendant(root, |n| out.push(n));
        out
    }

    /// Every query path against the per-node oracle -- `matches_scoped` / `matches_scoped_in_shadow`, a
    /// fresh context and quirks lookup per node, which the paths must agree with exactly.
    fn agree(w: &World, corpus: &[&str]) {
        let dom = &w.dom;
        let elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.is_element(n)).collect();
        for &sel in corpus {
            let Ok(q) = compile(sel) else {
                for &r in &w.roots {
                    assert!(dom.query_selector_all_in(r, sel).is_err() && dom.query_selector_in(r, sel).is_err(), "{sel:?}");
                    assert!(dom.descendants_matching(r, sel).is_err() && dom.first_descendant_matching(r, sel).is_err());
                }
                assert!(dom.matches_selector(elements[0], sel).is_err() && dom.closest(elements[0], sel).is_err());
                for &sr in &w.shadows {
                    assert!(dom.shadow_descendants_matching(sr, sel).is_err());
                    assert!(dom.shadow_first_descendant_matching(sr, sel).is_err());
                }
                continue;
            };
            let all = |root: NodeId, scope: Option<NodeId>| -> Vec<NodeId> {
                descendants(dom, root).into_iter().filter(|&n| matches_scoped(dom, n, &q.list, scope)).collect()
            };
            for &r in &w.roots {
                agree_under(dom, sel, r);
            }
            for (i, &e) in elements.iter().enumerate() {
                assert_eq!(dom.matches_selector(e, sel).unwrap(), matches_scoped(dom, e, &q.list, Some(e)), "#{e}.matches({sel:?})");
                if i % 7 == 0 {
                    let want = all(e, Some(e));
                    assert_eq!(dom.descendants_matching(e, sel).unwrap(), want, "#{e}.querySelectorAll({sel:?})");
                    assert_eq!(dom.first_descendant_matching(e, sel).unwrap(), want.first().copied(), "#{e}.querySelector({sel:?})");
                }
                if i % 5 == 0 {
                    let mut want = None;
                    let mut cur = Some(e);
                    while let Some(c) = cur {
                        if matches_scoped(dom, c, &q.list, Some(e)) {
                            want = Some(c);
                            break;
                        }
                        cur = dom.parent(c);
                    }
                    assert_eq!(dom.closest(e, sel).unwrap(), want, "#{e}.closest({sel:?})");
                }
            }
            for &sr in &w.shadows {
                let host = dom.shadow_host_of(sr).unwrap();
                let mut want: Vec<NodeId> =
                    descendants(dom, sr).into_iter().filter(|&n| matches_scoped_in_shadow(dom, n, &q.list, host)).collect();
                if super::list_is_slotted(&q.list) {
                    for slot in dom.descendant_elements(sr).into_iter().filter(|&n| dom.is_html_slot_element(n)) {
                        for n in dom.assigned_nodes(slot).unwrap_or_default() {
                            if matches_scoped_in_shadow(dom, n, &q.list, host) && !want.contains(&n) {
                                want.push(n);
                            }
                        }
                    }
                }
                assert_eq!(dom.shadow_descendants_matching(sr, sel).unwrap(), want, "shadowRoot.querySelectorAll({sel:?})");
                assert_eq!(dom.shadow_first_descendant_matching(sr, sel).unwrap(), want.first().copied(), "shadowRoot.querySelector({sel:?})");
            }
        }
    }

    /// The four walking entry points under `r` against the oracle: the document form (no scope) and the
    /// element form (`r` is `:scope`), each as a list and as a first match.
    fn agree_under(dom: &Dom, sel: &str, r: NodeId) {
        let q = compile(sel).unwrap();
        let all = |scope: Option<NodeId>| -> Vec<NodeId> {
            descendants(dom, r).into_iter().filter(|&n| matches_scoped(dom, n, &q.list, scope)).collect()
        };
        let want = all(None);
        assert_eq!(dom.query_selector_all_in(r, sel).unwrap(), want, "querySelectorAll({sel:?}) under #{r}");
        assert_eq!(dom.query_selector_in(r, sel).unwrap(), want.first().copied(), "querySelector({sel:?}) under #{r}");
        let want = all(Some(r));
        assert_eq!(dom.descendants_matching(r, sel).unwrap(), want, "#{r}.querySelectorAll({sel:?})");
        assert_eq!(dom.first_descendant_matching(r, sel).unwrap(), want.first().copied(), "#{r}.querySelector({sel:?})");
    }

    #[test]
    fn id_plan_shapes() {
        let p = |text: &str| {
            let q = compile(text).unwrap();
            (q.plan.subject_id.as_ref().map(|v| v.0.clone()), q.plan.ancestor_id.as_ref().map(|v| v.0.clone()))
        };
        let s = |v: &str| Some(v.to_string());
        assert_eq!(p("#a .x"), (None, s("a")));
        assert_eq!(p("#a > .x"), (None, s("a")));
        assert_eq!(p("#a + .p .x"), (None, None), "#a is a sibling of an ancestor, not one");
        assert_eq!(p(".p ~ #a .x"), (None, s("a")));
        assert_eq!(p(".x > #a ~ .y"), (None, None));
        assert_eq!(p("#a"), (s("a"), None));
        assert_eq!(p("div#a.k"), (s("a"), None));
        assert_eq!(p("#a #b"), (s("b"), s("a")));
        assert_eq!(p("#a :is(.x)"), (None, s("a")));
        assert_eq!(p(":is(#a) .x"), (None, None), "only a top-level id");
        assert_eq!(p("a, #b"), (None, None), "a list");
        assert_eq!(p(":host #a .x"), (None, None));
        assert_eq!(p("#a :scope .x"), (None, None), "past a compound that can step to a shadow host");
        assert_eq!(p("#a :is(:scope) > .x"), (None, None));
        assert_eq!(p(":scope #a .x"), (None, s("a")), "a featureless compound further left is harmless");
        assert_eq!(p("#a & .x"), (None, s("a")), "`&` alone never matches a featureless host");
        assert_eq!(p("#a::before"), (None, None));
        assert_eq!(p("#a ::slotted(.x)"), (None, None));
    }

    // The id paths answer from the index, and the index is only right while every writer keeps it:
    // duplicates in one tree, the same id in a detached subtree, another document and a shadow tree,
    // an id past the candidate bound, and rewrites, removals and moves between the checks.
    #[test]
    fn id_paths_agree_after_rewrites_moves_and_detach() {
        let mut dom = Dom::parse(
            "<!doctype html><body><div id=top><section class=p></section><div id=a class=k><p class=p></p>\
             <span class=x></span><div class=x><span class=x></span></div></div><p class=p></p></div>\
             <div id=other><span class=x></span><b id=z><i class=x></i></b></div></body>",
        );
        let doc = dom.root();
        let top = dom.get_element_by_id("top").unwrap();
        let a = dom.get_element_by_id("a").unwrap();
        let other = dom.get_element_by_id("other").unwrap();
        // A second #a in the same tree, inserted before the first.
        let a2 = dom.create_element("div");
        dom.set_attribute(a2, "id", "a");
        let x2 = dom.create_element("span");
        dom.set_attribute(x2, "class", "x");
        dom.append_child(a2, x2);
        dom.insert_before(top, a2, a);
        // The same id elsewhere: detached, another document, a shadow tree.
        let ghost = dom.create_element("div");
        dom.set_attribute(ghost, "id", "a");
        let gx = dom.create_element("span");
        dom.set_attribute(gx, "class", "x");
        dom.append_child(ghost, gx);
        let other_doc = dom.parse_into("<!doctype html><div id=a><span class=x></span></div>");
        let sr = dom.attach_shadow(other, ShadowRootMode::Open).unwrap();
        let sa = dom.create_element("div");
        dom.set_attribute(sa, "id", "a");
        dom.append_child(sr, sa);
        let sx = dom.create_element("span");
        dom.set_attribute(sx, "class", "x");
        dom.append_child(sa, sx);
        // Twenty detached elements carrying `z`: past the bound, so `#z .x` walks.
        for _ in 0..20 {
            let z = dom.create_element("i");
            dom.set_attribute(z, "id", "z");
        }
        let sels = ["#a", "div#a", "#a .x", "#a > .x", "#a .p ~ .x", "#a + .p .x", ".p ~ #a .x", "div#a.k", "#missing .x",
                    "#A .x", "#z .x", "#z", "#a #a .x", "#top #a .x", "#a, .x"];
        let check = |dom: &Dom| {
            let inner = dom.descendant_elements(a).first().copied().unwrap_or(a);
            for sel in sels {
                for r in [doc, top, a, a2, inner, other, ghost, other_doc, sr, dom.root_of(sx)] {
                    agree_under(dom, sel, r);
                }
            }
        };
        check(&dom);
        dom.remove_attribute(a2, "id");
        check(&dom);
        dom.set_attribute(a, "id", "b");
        check(&dom);
        dom.set_attribute(a2, "id", "a");
        check(&dom);
        dom.remove_child(top, a2);
        check(&dom);
        dom.append_child(other, a2);
        check(&dom);
        dom.set_attribute(a, "id", "a");
        dom.remove_child(doc, dom.descendant_elements(doc)[0]);
        check(&dom);
        // A quirks document folds id case, so its query never takes the index's exact answer.
        let q = dom.parse_into("<div id=A class=k><span class=x></span></div>");
        assert_eq!(dom.query_selector_all_in(q, "#a .x").unwrap().len(), 1, "#a matches id=A in quirks mode");
        assert_eq!(dom.query_selector_in(q, "#a").unwrap(), dom.descendant_elements(q).into_iter().find(|&n| dom.get_attribute(n, "id") == Some("A")));
        for sel in sels {
            agree_under(&dom, sel, q);
        }
    }

    /// Rewrites ids and classes, removes ids, moves nodes between and within trees (which shifts nth
    /// indices and roots), and detaches some.
    fn mutate(w: &mut World, rng: &mut Rng) {
        let dom = &mut w.dom;
        let elements: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.is_element(n)).collect();
        for _ in 0..40 {
            let e = elements[rng.below(elements.len())];
            match rng.below(6) {
                0 => {
                    let i = rng.pick(&IDS);
                    dom.set_attribute(e, "id", i);
                }
                1 => {
                    let c = rng.pick(&CLASSES);
                    dom.set_attribute(e, "class", c);
                }
                2 => dom.remove_attribute(e, "id"),
                3 | 4 => {
                    let r = elements[rng.below(elements.len())];
                    if let Some(p) = dom.parent(r) {
                        if !dom.contains(e, p) && dom.is_element(p) {
                            dom.insert_before(p, e, r);
                        }
                    }
                }
                _ => {
                    if let Some(p) = dom.parent(e) {
                        dom.remove_child(p, e);
                    }
                }
            }
        }
    }

    #[test]
    fn plan_marks_only_the_subset_fast() {
        let fast = |text: &str| compile(text).unwrap().plan.per[0].fast.is_some();
        for text in [".a .cell", "#i1 > div[data-x=v i]", ":scope > .a", ":root .a", "a[href^='/'] span", "& > div", "div.a#b"] {
            assert!(fast(text), "{text} is fast");
        }
        for text in ["p + p", ".a:hover", ":not(.a) b", "li:nth-child(2)", ":host .a", "p::before", "::slotted(*)", "a:link",
                     "|a", ":-webkit-any(.a) span", ".a ~ .b", "[TYPE=x i] p", ":is(.a) .b"] {
            assert!(!fast(text), "{text} is not fast");
        }
        // One simple selector is answered by its key instead, and needs no compounds.
        assert!(!fast("*") && compile("*").unwrap().plan.per[0].whole);
        assert!(!fast("*|a") && compile("*|a").unwrap().plan.per[0].whole);
        // No namespace prefix is ever declared to a query, so a prefixed name never parses.
        assert!(compile("svg|a").is_err());
    }

    // A shadow root above a candidate is the one place the crate can step off the parent chain (to
    // the host, for a featureless `:host`/`:scope`); the direct matcher hands those back.
    #[test]
    fn fast_matcher_hands_back_at_shadow_boundaries() {
        let mut dom = Dom::parse("<!doctype html><body><div id=h class=a><p class=b></p></div></body>");
        let h = dom.get_element_by_id("h").unwrap();
        let sr = dom.attach_shadow(h, ShadowRootMode::Open).unwrap();
        let mut made = Vec::new();
        for (parent, tag, class) in [(sr, "div", "a"), (sr, "p", "b"), (0, "p", "b"), (0, "div", "a"), (3, "p", "b")] {
            let e = dom.create_element(tag);
            dom.set_attribute(e, "class", class);
            let parent = if parent == sr { sr } else { made[parent as usize] };
            dom.append_child(parent, e);
            made.push(e);
        }
        let sels = [":scope > .a", ".a .b", "div > p", "* > *", ":scope .b", ".a > .b", "div p", "& > p"];
        for sel in sels {
            let q = compile(sel).unwrap();
            for &e in made.iter().chain([&h, &sr]) {
                assert_eq!(dom.matches_selector(e, sel).unwrap(), matches_scoped(&dom, e, &q.list, Some(e)), "#{e}.matches({sel:?})");
                let want: Vec<NodeId> = descendants(&dom, e).into_iter().filter(|&n| matches_scoped(&dom, n, &q.list, Some(e))).collect();
                assert_eq!(dom.descendants_matching(e, sel).unwrap(), want, "#{e}.querySelectorAll({sel:?})");
                let mut want = None;
                let mut cur = Some(e);
                while let Some(c) = cur {
                    if matches_scoped(&dom, c, &q.list, Some(e)) {
                        want = Some(c);
                        break;
                    }
                    cur = dom.parent(c);
                }
                assert_eq!(dom.closest(e, sel).unwrap(), want, "#{e}.closest({sel:?})");
            }
            let want: Vec<NodeId> = descendants(&dom, sr).into_iter().filter(|&n| matches_scoped_in_shadow(&dom, n, &q.list, h)).collect();
            assert_eq!(dom.shadow_descendants_matching(sr, sel).unwrap(), want, "shadowRoot.querySelectorAll({sel:?})");
        }
    }

    // A chain deep enough that the descendant combinator's walks are long, through every entry point.
    #[test]
    fn a_deep_chain_agrees() {
        let mut dom = Dom::parse("<!doctype html><body></body>");
        let doc = dom.root();
        let mut cur = first_tagged(&dom, doc, "body");
        let mut chain = Vec::new();
        for i in 0..400 {
            let e = dom.create_element(if i % 3 == 0 { "section" } else { "div" });
            dom.set_attribute(e, "class", if i % 7 == 6 { "b" } else { "a" });
            if i % 50 == 0 {
                dom.set_attribute(e, "id", "i1");
            }
            dom.append_child(cur, e);
            chain.push(e);
            cur = e;
        }
        let leaf = dom.create_element("span");
        dom.set_attribute(leaf, "class", "cell");
        dom.append_child(cur, leaf);
        let w = World { dom, roots: vec![doc, chain[0], chain[200]], shadows: vec![] };
        let corpus = [".a > .a > .a", ".a .b .cell", ".b > .a .cell", "section > div > div > section .cell", ".zz .cell",
                      ".a > .b > .cell", "#i1 .a > .b", ":scope > .a", ".a, .b > .a", "* > .b", "div .cell", ".b .b .b .b"];
        agree(&w, &corpus);
    }

    // The differential test every query fast path answers to: random trees of every kind a query
    // runs over, the corpus above, every entry point against the unchanged per-node oracle, then
    // again after rounds of mutation.
    #[test]
    fn reference_agreement() {
        for (seed, everything) in [(0x9E37_79B9_7F4A_7C15u64, true), (0x2545_F491_4F6C_DD1D, false), (7, true)] {
            let mut w = world(seed, everything);
            agree(&w, CORPUS);
            let mut rng = Rng(seed ^ 0xDEAD_BEEF);
            for _ in 0..3 {
                mutate(&mut w, &mut rng);
                agree(&w, CORPUS);
            }
        }
    }

    // The one-entry memo and the map behind it hand back the parse each string was first compiled
    // to, whichever answered; a failed compile caches nothing and moves nothing.
    #[test]
    fn compile_memo_keeps_identity_across_alternation() {
        let a = compile("a").unwrap();
        let b = compile(".b").unwrap();
        for _ in 0..3 {
            assert!(Rc::ptr_eq(&compile("a").unwrap(), &a));
            assert!(Rc::ptr_eq(&compile("a").unwrap(), &a), "the memo's answer");
            assert!(Rc::ptr_eq(&compile(".b").unwrap(), &b));
        }
        assert!(compile(">>>").is_err());
        assert!(compile(">>>").is_err(), "an invalid selector is rejected again, not remembered");
        assert!(Rc::ptr_eq(&compile("a").unwrap(), &a));
        assert!(compile("").is_err());
    }

    fn keys(text: &str) -> Vec<(String, bool)> {
        let q = compile(text).unwrap();
        q.plan
            .per
            .iter()
            .map(|p| {
                let key = match &p.key {
                    SubjectKey::Any => "any".to_string(),
                    SubjectKey::Id(v) => format!("id {}", v.0),
                    SubjectKey::Class(v) => format!("class {}", v.0),
                    SubjectKey::Attr { name, lower } => format!("attr {} {}", &*name.0, &*lower.0),
                    SubjectKey::Tag(t) => format!("tag {} {}", &*t.name.0, &*t.lower_name.0),
                };
                (key, p.whole)
            })
            .collect()
    }

    #[test]
    fn subject_keys_follow_bucket_order() {
        let k = |text: &str| keys(text).remove(0);
        assert_eq!(k("#a.b"), ("id a".into(), false));
        assert_eq!(k("div.b[x]"), ("class b".into(), false));
        assert_eq!(k("div[x]"), ("attr x x".into(), false));
        assert_eq!(k("div[x=y]"), ("attr x x".into(), false));
        assert_eq!(k("[viewBox]"), ("attr viewBox viewbox".into(), true));
        assert_eq!(k("div"), ("tag div div".into(), true));
        assert_eq!(k("DIV"), ("tag DIV div".into(), true));
        assert_eq!(k("*"), ("any".into(), true));
        assert_eq!(k(".a"), ("class a".into(), true));
        assert_eq!(k("#a"), ("id a".into(), true));
        assert_eq!(k(".a .b"), ("class b".into(), false));
        assert_eq!(k(".b > div"), ("tag div div".into(), false), "only the subject compound");
        for text in [":is(.a)", "p::before", "::slotted(span)", ":not(.a)", ":hover", "|a", "a.b"] {
            assert!(!k(text).1, "{text} is not whole");
        }
        // With no default namespace the parser keeps no namespace component for `*|`: `*|a` is `a`.
        assert_eq!(k("*|a"), ("tag a a".into(), true));
        assert_eq!(k("|a"), ("tag a a".into(), false), "`|a` keeps its no-namespace test");
        for text in [":is(.a)", "p::before", "::slotted(span)", ":host"] {
            assert_eq!(k(text).0, "any", "{text} has no key");
        }
        assert_eq!(keys("style, link"), vec![("tag style style".into(), true), ("tag link link".into(), true)]);
    }

    #[test]
    fn plan_records_slotted() {
        assert!(compile("::slotted(span)").unwrap().plan.slotted);
        assert!(compile("p, ::slotted(.x)").unwrap().plan.slotted);
        assert!(!compile(".a .b").unwrap().plan.slotted);
    }
}

// ---------------------------------------------------------------------------
// The cascade's view of a selector — the public, opaque form korvus-page holds.
// ---------------------------------------------------------------------------

/// One compiled CSS selector: a single selector, never a list.
///
/// `h1, .lede { … }` is one rule but two selectors, and the cascade has to keep them apart — each
/// carries its own specificity and each buckets under a different key. So a parsed stylesheet holds
/// these, not the list they were written in.
///
/// The `selectors` crate's own types stay behind this newtype deliberately. Two reasons, and the
/// second is the binding one:
///
///  - they are a deep generic tree parameterised by a trait with ten associated types, so leaking
///    them would put the matcher's version in every consumer's signatures;
///  - this is `Send + Sync`, and [`CompiledSelector`] — an `Rc` into a thread-local parse cache —
///    is not. The cascade lives in a service on the kernel `Context`, which requires both.
#[derive(Clone)]
pub struct Selector {
    inner: parser::Selector<KorvusSelectorImpl>,
    /// Hashes of the selector's *ancestor* parts, computed once at parse. Matched against
    /// [`AncestorBloom`] to reject a selector before its tree walk. See [`Selector::matches_with`].
    hashes: parser::AncestorHashes,
    /// The same with each class and id folded to ASCII lowercase, which a quirks document's filter
    /// is asked with. See [`folded_ancestor_hashes`].
    folded_hashes: parser::AncestorHashes,
    /// Whether matching can WRITE to a `SelectorCaches`: only an index pseudo-class (`:nth-*`,
    /// `:first-child` and kin, which parse to `Nth`/`NthOf`) or `:has()` does, at any depth. Computed
    /// once at parse. See [`Selector::matches_with`].
    uses_caches: bool,
    /// Whether an attribute selector anywhere in it names `style` or a `data-*` attribute -- the attributes
    /// whose writes [`Dom::match_revision`] leaves out.
    mentions_unrevised_attribute: bool,
    /// Whether one names `style` itself, and whether one names a `data-*` attribute. See
    /// [`Selector::mentions_style_attribute`].
    mentions_style_attribute: bool,
    mentions_data_attribute: bool,
}

/// Finds an attribute selector on `style` or on a `data-*` attribute, at any depth, and says which.
struct StyleAttribute {
    found: bool,
    style: bool,
    data: bool,
}

impl SelectorVisitor for StyleAttribute {
    type Impl = KorvusSelectorImpl;

    fn visit_attribute_selector(
        &mut self,
        _namespace: &NamespaceConstraint<&CssNamespace>,
        local_name: &CssLocalName,
        local_name_lower: &CssLocalName,
    ) -> bool {
        if local_name.0.eq_ignore_ascii_case(&LocalName::from("style")) || local_name_lower.0 == LocalName::from("style") {
            self.found = true;
            self.style = true;
        }
        // A `data-*` attribute is read by no matching rule but one that names it, as `style` is.
        if local_name_lower.0.len() > 5 && local_name_lower.0[..5].eq_ignore_ascii_case("data-") {
            self.found = true;
            self.data = true;
        }
        !(self.style && self.data)
    }

    fn visit_relative_selector_list(&mut self, list: &[parser::RelativeSelector<KorvusSelectorImpl>]) -> bool {
        for relative in list {
            if !relative.selector.visit(self) {
                return false;
            }
        }
        true
    }
}

/// Finds the components whose matching reads or writes a `SelectorCaches`.
struct CacheUse {
    found: bool,
}

impl SelectorVisitor for CacheUse {
    type Impl = KorvusSelectorImpl;

    fn visit_simple_selector(&mut self, component: &parser::Component<KorvusSelectorImpl>) -> bool {
        if matches!(component, parser::Component::Nth(_) | parser::Component::NthOf(_) | parser::Component::Has(_)) {
            self.found = true;
        }
        !self.found
    }

    fn visit_relative_selector_list(&mut self, _list: &[parser::RelativeSelector<KorvusSelectorImpl>]) -> bool {
        self.found = true;
        false
    }
}

thread_local! {
    /// The caches a selector that cannot write to them matches against. Never written, so never
    /// stale: see [`Selector::matches_with`].
    static UNUSED_CACHES: RefCell<selectors::context::SelectorCaches> = RefCell::new(Default::default());
    /// The caches the selectors that DO write them (`:nth-*`, `:has()`) share inside a
    /// [`with_pass_caches`], with how many of those are open. Emptied when the outermost closes.
    static PASS_CACHES: RefCell<(u32, selectors::context::SelectorCaches)> = RefCell::new((0, Default::default()));
}

/// [`UNUSED_CACHES`], borrowed for a run of tests ([`with_match_caches`]); `None` when it was already
/// borrowed, and then each test makes its own. With the [`PASS_CACHES`] when a pass is open.
///
/// The third field is whether [`crate::invalidation::record_selector_flags`] is open, read once here rather
/// than per test: the thread-local is one more call on a path that runs per candidate rule.
pub struct MatchCaches<'a>(Option<&'a mut selectors::context::SelectorCaches>, Option<&'a mut selectors::context::SelectorCaches>, bool);

/// Run `f` with [`UNUSED_CACHES`] borrowed once for all the tests it makes through
/// [`Selector::matches_in_tree_cached`]. Reaching the thread-local per test is a call LLVM
/// inlines or not by how this crate happens to partition: asana.com's style passes paid ~5 G more
/// instructions when an unrelated change to `tree.rs` made it stop.
pub fn with_match_caches<R>(f: impl FnOnce(&mut MatchCaches<'_>) -> R) -> R {
    UNUSED_CACHES.with(|caches| {
        PASS_CACHES.with(|pass| {
            let mut pass = pass.try_borrow_mut().ok().filter(|p| p.0 > 0);
            let pass = pass.as_deref_mut().map(|p| &mut p.1);
            let record = crate::invalidation::recording();
            match caches.try_borrow_mut() {
                Ok(mut borrowed) => f(&mut MatchCaches(Some(&mut borrowed), pass, record)),
                Err(_) => f(&mut MatchCaches(None, pass, record)),
            }
        })
    })
}

/// Run `f` as one pass over a tree that does not change until it returns: the tests made through
/// [`with_match_caches`] inside it share one set of the caches that `:nth-*` and `:has()` write.
/// Outside a pass, each such test makes its own and drops it -- the nth-child index of every
/// sibling walked again and a hash map made and freed per test: www.wolfram.com's
/// `.heirs-cols-2__2 > * > :nth-child(n+3)` rules cost it ~25 % of a load. The caches are keyed on node
/// addresses in the arena, which only a mutation moves, so they are emptied when the outermost pass
/// closes. The caller holds `&Dom` for the whole of `f`, so nothing mutates inside it.
pub fn with_pass_caches<R>(f: impl FnOnce() -> R) -> R {
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            PASS_CACHES.with(|pass| {
                if let Ok(mut p) = pass.try_borrow_mut() {
                    p.0 -= 1;
                    if p.0 == 0 {
                        p.1 = Default::default();
                    }
                }
            });
        }
    }
    let opened = PASS_CACHES.with(|pass| match pass.try_borrow_mut() {
        Ok(mut p) => {
            p.0 += 1;
            true
        }
        Err(_) => false,
    });
    let _close = opened.then_some(Close);
    f()
}

impl Selector {
    /// Parse a selector list into its individual selectors. The whole list is rejected if any one
    /// of them is invalid, which is what CSS asks for: an unparseable selector invalidates the rule
    /// it introduces rather than being skipped.
    pub fn parse_list(text: &str) -> Result<Vec<Selector>, SelectorParseError> {
        Selector::parse_list_in(text, &[])
    }

    /// [`Selector::parse_list`] under a sheet's `@namespace` rules, as (prefix, URL) with `""` the default.
    pub fn parse_list_in(text: &str, namespaces: &[(String, String)]) -> Result<Vec<Selector>, SelectorParseError> {
        let list = parse_list_in(text, namespaces).map_err(|detail| SelectorParseError {
            selector: text.to_string(),
            detail,
        })?;
        Ok(list
            .slice()
            .iter()
            .map(|inner| {
                let mut uses = CacheUse { found: false };
                inner.visit(&mut uses);
                let mut style = StyleAttribute { found: false, style: false, data: false };
                inner.visit(&mut style);
                Selector {
                    hashes: parser::AncestorHashes::new(inner, QuirksMode::NoQuirks),
                    folded_hashes: folded_ancestor_hashes(inner),
                    inner: inner.clone(),
                    uses_caches: uses.found,
                    mentions_unrevised_attribute: style.found,
                    mentions_style_attribute: style.style,
                    mentions_data_attribute: style.data,
                }
            })
            .collect())
    }

    fn wrap(inner: parser::Selector<KorvusSelectorImpl>) -> Selector {
        let mut uses = CacheUse { found: false };
        inner.visit(&mut uses);
        let mut style = StyleAttribute { found: false, style: false, data: false };
        inner.visit(&mut style);
        Selector {
            hashes: parser::AncestorHashes::new(&inner, QuirksMode::NoQuirks),
            folded_hashes: folded_ancestor_hashes(&inner),
            inner,
            uses_caches: uses.found,
            mentions_unrevised_attribute: style.found,
            mentions_style_attribute: style.style,
            mentions_data_attribute: style.data,
        }
    }

    /// Parse the selector list of a NESTED style rule (CSS Nesting 1 §2): a selector that does not
    /// begin with `&` is relative to it, so `.c` means `& .c` and `> .d` means `& > .d`. The `&` is
    /// kept as written; [`Selector::replace_parent`] substitutes the parent rule's selectors.
    pub fn parse_nested(text: &str) -> Result<Vec<Selector>, SelectorParseError> {
        Selector::parse_nested_in(text, &[])
    }

    /// [`Selector::parse_nested`] under a sheet's `@namespace` rules.
    pub fn parse_nested_in(text: &str, namespaces: &[(String, String)]) -> Result<Vec<Selector>, SelectorParseError> {
        let mut input = cssparser::ParserInput::new(text);
        let mut parser = cssparser::Parser::new(&mut input);
        let list = SelectorList::parse(&KorvusSelectorParser { namespaces }, &mut parser, ParseRelative::ForNesting)
            .map_err(|e| SelectorParseError { selector: text.to_string(), detail: format!("{e:?}") })?;
        Ok(list.slice().iter().map(|inner| Selector::wrap(inner.clone())).collect())
    }

    /// This selector with every `&` replaced by `parents` -- `:is(<parents>)`, with the specificity
    /// of the most specific of them, which is what CSS Nesting specifies and Chrome computes.
    pub fn replace_parent(&self, parents: &[Selector]) -> Selector {
        let list = SelectorList::from_iter(parents.iter().map(|p| p.inner.clone()).collect::<Vec<_>>().into_iter());
        Selector::wrap(self.inner.replace_parent_selector(&list))
    }

    /// Every feature this selector reads, with where a change to it can reach. See
    /// [`crate::invalidation`].
    pub fn invalidation_features(&self, sink: &mut dyn FnMut(crate::invalidation::Feature<'_>, crate::invalidation::Reach)) {
        crate::invalidation::features(&self.inner, crate::invalidation::REACH_SELF, sink);
    }

    /// The key every element this selector styles has. See [`crate::invalidation::Subject`].
    pub fn invalidation_subject(&self) -> crate::invalidation::Subject {
        crate::invalidation::subject(&self.inner)
    }

    /// For a subject with no key: a key an ancestor of every element this selector matches has
    /// (`crate::invalidation::ancestor_key`).
    pub fn invalidation_ancestor(&self) -> Option<crate::invalidation::Subject> {
        crate::invalidation::ancestor_key(&self.inner)
    }

    /// Whether this selector's subject compound holds `:root`.
    pub fn invalidation_subject_is_root(&self) -> bool {
        crate::invalidation::subject_is_root(&self.inner)
    }

    /// Whether this selector mentions `&` anywhere.
    pub fn has_parent_selector(&self) -> bool {
        self.inner.has_parent_selector()
    }

    /// Whether this selector can read an element's `style` attribute or a `data-*` attribute -- an attribute
    /// selector on one anywhere in it, `:is()`/`:not()`/`:has()` included. A cache of matched rules kept
    /// across [`Dom::match_revision`] is sound only while no selector it covers says yes.
    pub fn mentions_unrevised_attribute(&self) -> bool {
        self.mentions_unrevised_attribute
    }

    /// Whether this selector can read an element's `style` attribute itself, at any depth: the one case
    /// where a `style` rewrite that moves no box ([`Dom::inert_style_writes`]) can still move a match, and
    /// so the geometry. A `data-*` selector cannot: a `style` rewrite never touches a `data-*` attribute.
    pub fn mentions_style_attribute(&self) -> bool {
        self.mentions_style_attribute
    }

    /// Whether this selector can read a `data-*` attribute, at any depth: the rules a `data-*` write can
    /// make match or stop matching ([`Dom::unrevised_writes`]).
    pub fn mentions_data_attribute(&self) -> bool {
        self.mentions_data_attribute
    }

    /// Serialize back to CSS, in the canonical form the CSSOM's `selectorText` returns.
    ///
    /// Not the text the page wrote: `DIV.a` comes back as `div.a`, `*|e` as `e`, and the escaping
    /// is normalized. That is what a round trip through `selectorText` is *specified* to do, and it
    /// is the reason this goes through the matcher's own serializer rather than keeping the source
    /// string — a stored string would round-trip too faithfully to be right.
    pub fn to_css_string(&self) -> String {
        let mut out = String::new();
        // `ToCss` for a selector only fails if the writer does, and a String never does.
        let _ = self.inner.to_css(&mut out);
        out
    }

    /// The CSS specificity, packed as the cascade compares it: id count, then class/attribute/
    /// pseudo-class count, then type/pseudo-element count, most significant first.
    pub fn specificity(&self) -> u32 {
        self.inner.specificity()
    }

    /// Does `id` match? Non-element nodes never do.
    pub fn matches(&self, dom: &Dom, id: NodeId) -> bool {
        self.matches_with(dom, id, None)
    }

    /// Does `id` match, given a bloom filter over its ancestors?
    ///
    /// The filter is a fast *rejection*: a selector whose ancestor parts hash to something the
    /// filter has never seen cannot match, and is dropped without the tree walk it would otherwise
    /// cost. False positives are harmless (the walk happens and answers correctly); a false
    /// negative would be a wrong answer, which is why the filter has to hold every ancestor.
    ///
    /// Passing `None` is always correct, just slower.
    ///
    /// # The caches are made only for a selector that can use them
    ///
    /// This built a fresh `SelectorCaches` -- nine hash maps -- for every selector test and dropped
    /// it after, and the cascade runs one test per candidate rule per element per style pass. Sampling
    /// the engine thread on www.who.int, while the parity driver waited 30 s for a page that
    /// alternates style writes with layout reads, found that construction and drop in 11 of 20
    /// samples, more than the matching itself. Only `:nth-*`-family and `:has()` components touch the
    /// caches, and their entries are per-element facts that go stale when the DOM changes, so those
    /// selectors still get a fresh set per call exactly as before; every other selector matches
    /// against one thread-local set it cannot write to, which therefore stays empty.
    pub fn matches_with(&self, dom: &Dom, id: NodeId, bloom: Option<&AncestorBloom>) -> bool {
        if !matches!(dom.node(id).kind, NodeKind::Element { .. }) {
            return false;
        }
        if self.uses_caches {
            return self.match_in(dom, id, bloom, &mut selectors::context::SelectorCaches::default());
        }
        UNUSED_CACHES.with(|caches| match caches.try_borrow_mut() {
            Ok(mut caches) => self.match_in(dom, id, bloom, &mut caches),
            // Re-entered from inside a match: nothing here does that, but a fresh set is always right.
            Err(_) => self.match_in(dom, id, bloom, &mut selectors::context::SelectorCaches::default()),
        })
    }

    fn match_in(
        &self,
        dom: &Dom,
        id: NodeId,
        bloom: Option<&AncestorBloom>,
        caches: &mut selectors::context::SelectorCaches,
    ) -> bool {
        self.match_mode(dom, id, bloom, caches, None, None, None, false)
    }

    /// The one matching call: [`MatchingMode::Normal`] when `pseudo` is `None`, otherwise for the
    /// pseudo-element of that name.
    ///
    /// A QUIRKS document folds class and id case, so its filter holds folded hashes and is asked with
    /// the selector's folded ones: a filter built from `class=a` must not reject `.A b`, a
    /// false negative, the one answer the filter must never give. A filter built the other way for
    /// the element's document is not consulted at all.
    fn match_mode(
        &self,
        dom: &Dom,
        id: NodeId,
        bloom: Option<&AncestorBloom>,
        caches: &mut selectors::context::SelectorCaches,
        pseudo: Option<&str>,
        scope: Option<NodeId>,
        host: Option<NodeId>,
        record: bool,
    ) -> bool {
        let quirks = quirks_mode_of(dom, id);
        let bloom = bloom.filter(|b| b.folded == (quirks == QuirksMode::Quirks));
        let wanted = |pe: &KorvusPseudoElement| {
            let KorvusPseudoElement::Known(name) = pe;
            pseudo.is_some_and(|p| p == name.0.as_str())
        };
        let mut context = MatchingContext::new(
            if pseudo.is_some() { MatchingMode::ForStatelessPseudoElement } else { MatchingMode::Normal },
            bloom.map(|b| &b.filter),
            caches,
            quirks,
            if record { NeedsSelectorFlags::Yes } else { NeedsSelectorFlags::No },
            MatchingForInvalidation::No,
        );
        if pseudo.is_some() {
            context.pseudo_element_matching_fn = Some(&wanted);
        }
        if let Some(root) = scope {
            context.scope_element = Some(NodeRef { dom, id: root }.opaque());
        }
        if let Some(host) = host {
            context.current_host = Some(NodeRef { dom, id: host }.opaque());
        }
        let element = NodeRef { dom, id };
        let hashes = bloom.map(|b| if b.folded { &self.folded_hashes } else { &self.hashes });
        selectors::matching::matches_selector(&self.inner, 0, hashes, &element, &mut context)
    }

    /// The pseudo-element this selector styles (`p::before` -> `"before"`, lowercase), or `None` for a
    /// selector that styles elements.
    pub fn pseudo_element(&self) -> Option<&str> {
        self.inner.pseudo_element().map(|KorvusPseudoElement::Known(name)| name.0.as_str())
    }

    /// Does this selector style the `pseudo` pseudo-element of `id`? `p::before` does for every `<p>`;
    /// a selector with no pseudo-element, or with another one, never does.
    pub fn matches_pseudo(&self, dom: &Dom, id: NodeId, bloom: Option<&AncestorBloom>, pseudo: &str) -> bool {
        if !matches!(dom.node(id).kind, NodeKind::Element { .. }) || self.pseudo_element() != Some(pseudo) {
            return false;
        }
        self.matches_at(dom, id, bloom, Some(pseudo), None)
    }

    /// The general form: the element (`pseudo` `None`) or its `pseudo` pseudo-element, with `:scope`
    /// meaning `scope` rather than the root (`@scope`).
    pub fn matches_at(&self, dom: &Dom, id: NodeId, bloom: Option<&AncestorBloom>, pseudo: Option<&str>, scope: Option<NodeId>) -> bool {
        self.matches_in_tree(dom, id, bloom, pseudo, scope, None)
    }

    /// [`Selector::matches_at`] for a rule of a SHADOW TREE's stylesheet: `host` is that
    /// tree's host, which is what `:host`, `:host()` and `::slotted()` resolve against (CSS Scoping §3).
    /// `None` matches as a document's rule does, where those three match nothing.
    #[inline]
    pub fn matches_in_tree(
        &self,
        dom: &Dom,
        id: NodeId,
        bloom: Option<&AncestorBloom>,
        pseudo: Option<&str>,
        scope: Option<NodeId>,
        host: Option<NodeId>,
    ) -> bool {
        if !matches!(dom.node(id).kind, NodeKind::Element { .. }) || self.pseudo_element() != pseudo {
            return false;
        }
        if self.uses_caches {
            return self.match_mode(dom, id, bloom, &mut selectors::context::SelectorCaches::default(), pseudo, scope, host, false);
        }
        UNUSED_CACHES.with(|caches| match caches.try_borrow_mut() {
            Ok(mut caches) => self.match_mode(dom, id, bloom, &mut caches, pseudo, scope, host, false),
            Err(_) => self.match_mode(dom, id, bloom, &mut selectors::context::SelectorCaches::default(), pseudo, scope, host, false),
        })
    }

    /// [`Selector::matches_in_tree`] against caches borrowed once for a run of tests ([`with_match_caches`]).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn matches_in_tree_cached(
        &self,
        dom: &Dom,
        id: NodeId,
        bloom: Option<&AncestorBloom>,
        pseudo: Option<&str>,
        scope: Option<NodeId>,
        host: Option<NodeId>,
        caches: &mut MatchCaches<'_>,
    ) -> bool {
        if !matches!(dom.node(id).kind, NodeKind::Element { .. }) || self.pseudo_element() != pseudo {
            return false;
        }
        if self.uses_caches {
            return match caches.1.as_deref_mut() {
                Some(pass) => self.match_mode(dom, id, bloom, pass, pseudo, scope, host, caches.2),
                None => self.match_mode(dom, id, bloom, &mut selectors::context::SelectorCaches::default(), pseudo, scope, host, caches.2),
            };
        }
        match caches.0.as_deref_mut() {
            Some(shared) => self.match_mode(dom, id, bloom, shared, pseudo, scope, host, caches.2),
            None => self.match_mode(dom, id, bloom, &mut selectors::context::SelectorCaches::default(), pseudo, scope, host, caches.2),
        }
    }

    /// Does this selector match only a shadow host seen from inside its own shadow tree -- `:host`,
    /// `:host(…)`, optionally followed by a pseudo-element? The host is featureless
    /// there (CSS Scoping §3.2.1), so these are the only selectors of a shadow tree's sheets that can
    /// match the host, and they match nothing inside the tree.
    pub fn is_featureless_host(&self) -> bool {
        self.inner
            .matches_featureless_host_selector_or_pseudo_element()
            .intersects(parser::FeaturelessHostMatches::FOR_HOST)
    }

    /// Does this selector style slotted elements (`::slotted(…)`)? It matches only a
    /// host's light child that a slot of the rule's shadow tree has been assigned.
    pub fn is_slotted(&self) -> bool {
        self.inner.is_slotted()
    }

    /// The key this selector should be indexed under.
    ///
    /// A page with 3,000 rules and 5,000 nodes is 15 million selector tests if every node is tried
    /// against every rule. Almost all of them are answerable without matching at all: `#nav a` can
    /// only match an `<a>`, so it belongs in the `a` bucket and no other element ever sees it. The
    /// key comes from the *rightmost* compound because that is the part that must match the element
    /// itself; everything to the left constrains its ancestors.
    ///
    /// The order — id, then class, then type — is by selectivity: an id bucket holds at most a
    /// handful of rules, a type bucket can hold hundreds. [`SelectorBucket::Universal`] is the
    /// residue (`*`, `[hidden]`, `:hover`) that has to be tried against everything.
    pub fn bucket(&self) -> SelectorBucket {
        let mut class = None;
        let mut attribute = None;
        let mut local = None;
        let mut iter = self.inner.iter();
        // `p::before` keys on `p`: the pseudo-element is a compound of its own, to the right of the
        // one that has to match the element.
        if self.inner.has_pseudo_element() {
            for _ in iter.by_ref() {}
            iter.next_sequence();
        }
        for component in iter {
            match component {
                parser::Component::ID(name) => return SelectorBucket::Id(name.0.clone()),
                parser::Component::Class(name) if class.is_none() => class = Some(name.0.clone()),
                parser::Component::AttributeInNoNamespaceExists { local_name_lower, .. } if attribute.is_none() => {
                    attribute = Some(local_name_lower.0.to_string())
                }
                // The parser makes this only for a name already in lowercase.
                parser::Component::AttributeInNoNamespace { local_name, .. } if attribute.is_none() => {
                    attribute = Some(local_name.0.to_string())
                }
                // A name with an uppercase letter and no namespace (`[TYPE=button i]`) matches as the two above.
                parser::Component::AttributeOther(other) if attribute.is_none() && other.namespace.is_none() => {
                    attribute = Some(other.local_name_lower.0.to_string())
                }
                parser::Component::LocalName(name) if local.is_none() => {
                    local = Some(name.lower_name.0.to_string())
                }
                _ => {}
            }
        }
        match (class, attribute, local) {
            (Some(c), _, _) => SelectorBucket::Class(c),
            (None, Some(a), _) => SelectorBucket::Attribute(a),
            (None, None, Some(l)) => SelectorBucket::LocalName(l),
            (None, None, None) => SelectorBucket::Universal,
        }
    }

    /// Every class the rightmost compound requires (the compound [`Selector::bucket`] keys on), in the order it
    /// meets them. An element this selector matches carries all of them, so the selector may be indexed under
    /// any one -- and an index does best under the one the fewest selectors share ([`Selector::bucket`] takes the
    /// first, which on a page of atomic classes is often one on every element: airbnb.com's `.atm_….dir.dir-ltr`).
    pub fn key_classes(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut iter = self.inner.iter();
        if self.inner.has_pseudo_element() {
            for _ in iter.by_ref() {}
            iter.next_sequence();
        }
        for component in iter {
            if let parser::Component::Class(name) = component {
                out.push(name.0.clone());
            }
        }
        out
    }

    /// The hashes [`AncestorBloom::may_match`] asks, as `(no-quirks, quirks)`: what `match_mode` rejects
    /// with before it walks anything, for a caller that wants to reject a selector without touching it.
    pub fn ancestor_hashes(&self) -> ([u32; 3], [u32; 3]) {
        (self.hashes.packed_hashes, self.folded_hashes.packed_hashes)
    }
}

impl std::fmt::Debug for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Selector({:?})", self.to_css_string())
    }
}

/// A bloom filter holding the hashes of an element's ancestors.
///
/// # What it buys
///
/// The rule index cuts styling down to the rules whose *rightmost* part could match an element.
/// What it cannot cut is a stylesheet where thousands of rules share that rightmost part —
/// `.section-1 .row .cell a`, `.section-2 .row .cell a`, and so on. Every `<a>` becomes a candidate
/// for all of them, and each candidate costs a walk up the tree to find out it does not match.
/// Measured on a 10,000-node page with 3,000 such rules: 655ms, and not one of them matched.
///
/// This filter answers that in constant time. Each selector carries the hashes of its ancestor
/// parts; if the filter has never seen one of them, no ancestor can supply it, so the selector is
/// rejected before the walk starts.
///
/// # The direction the error has to go
///
/// A bloom filter can say "maybe" when the answer is no — that costs a walk that then answers
/// correctly. It must never say "no" when the answer is yes, which is why every ancestor's hashes
/// go in, and why the hashes on both sides have to be computed the same way: the element's
/// lowercase local name, its id, and each of its classes.
#[derive(Default)]
pub struct AncestorBloom {
    filter: BloomFilter,
    /// Ids and classes went in folded to ASCII lowercase: a QUIRKS document's filter, which only a
    /// selector's folded hashes ask.
    folded: bool,
}

impl AncestorBloom {
    /// Can a selector with these ancestor hashes ([`Selector::ancestor_hashes`]: the quirks ones when
    /// [`AncestorBloom::folded`]) match below this filter? `false` is certain; `true` is a maybe. The test
    /// `match_mode` starts with (`selectors::matching::selector_may_match`), for a rule index that keeps
    /// the hashes beside each rule and rejects it there, as Blink's RuleData does.
    #[inline(always)]
    pub fn may_match(&self, hashes: &[u32; 3]) -> bool {
        for &packed in hashes {
            if packed == 0 {
                return true;
            }
            if !self.filter.might_contain_hash(packed & BLOOM_HASH_MASK) {
                return false;
            }
        }
        let fourth = ((hashes[0] & 0xff00_0000) >> 24) | ((hashes[1] & 0xff00_0000) >> 16) | ((hashes[2] & 0xff00_0000) >> 8);
        fourth == 0 || self.filter.might_contain_hash(fourth)
    }

    /// Whether ids and classes went in folded: the filter of a quirks document.
    pub fn folded(&self) -> bool {
        self.folded
    }

    pub fn new() -> AncestorBloom {
        AncestorBloom { filter: BloomFilter::new(), folded: false }
    }

    /// An empty filter for a traversal of the document `id` is in: folded when that document is in
    /// quirks mode.
    pub fn for_document_of(dom: &Dom, id: NodeId) -> AncestorBloom {
        AncestorBloom { filter: BloomFilter::new(), folded: quirks_mode_of(dom, id) == QuirksMode::Quirks }
    }

    /// The filter for `id`'s ancestors, built by walking up from it.
    ///
    /// Rebuilt per element rather than maintained across a traversal, because the caller is as
    /// often `getComputedStyle(el)` — one element, no traversal — as it is a whole-document pass.
    /// The walk is a handful of nodes deep against thousands of candidates, so it pays either way;
    /// [`AncestorBloom::push`] and [`AncestorBloom::pop`] are there for a traversal that wants to
    /// maintain one incrementally.
    pub fn for_node(dom: &Dom, id: NodeId) -> AncestorBloom {
        let mut bloom = AncestorBloom::for_document_of(dom, id);
        let mut current = dom.node(id).parent;
        while let Some(ancestor) = current {
            bloom.push(dom, ancestor);
            current = dom.node(ancestor).parent;
        }
        bloom
    }

    /// Add an element's hashes — on the way *down* a traversal.
    pub fn push(&mut self, dom: &Dom, id: NodeId) {
        let filter = &mut self.filter;
        Self::each_hash(dom, id, self.folded, |h| filter.insert_hash(h));
    }

    /// Remove them again — on the way back *up*. The filter counts, so a hash two ancestors share
    /// survives one of them being popped.
    pub fn pop(&mut self, dom: &Dom, id: NodeId) {
        let filter = &mut self.filter;
        Self::each_hash(dom, id, self.folded, |h| filter.remove_hash(h));
    }

    /// The hashes one element contributes: its ASCII-lowercased local name, its namespace, its id, and its
    /// classes (`folded`: the id and classes folded to ASCII lowercase).
    ///
    /// These have to be the same hashes `AncestorHashes` computes on the selector side, which is
    /// why the local name goes through the interned atom rather than being hashed as a string. The
    /// selector side carries a type selector's hash only when it is written in lowercase, and in an
    /// HTML document that lowercase name matches a mixed-case element (`clippath rect` and an SVG
    /// `<clipPath>`, as in Chrome), so the element side holds its name lowercased. It skipped a
    /// mixed-case name, and the filter then rejected `clippath rect` -- a false negative, the one
    /// answer it must never give. In an XML document the extra hash is only a "maybe".
    fn each_hash(dom: &Dom, id: NodeId, folded: bool, mut add: impl FnMut(u32)) {
        let NodeKind::Element { name, .. } = &dom.node(id).kind else { return };
        let lowered;
        let lower: &str = if name.bytes().any(|b| b.is_ascii_uppercase()) {
            lowered = name.as_str().to_ascii_lowercase();
            &lowered
        } else {
            name.as_str()
        };
        add(CssLocalName(LocalName::from(lower)).precomputed_hash() & BLOOM_HASH_MASK);
        // and its namespace, which the selector side hashes for a namespaced compound (`html|* > svg|svg`, the UA
        // sheet's): left out, the filter rejected every such rule -- the same false negative
        add(CssNamespace(Namespace::from(dom.namespace_uri(id).unwrap_or(""))).precomputed_hash() & BLOOM_HASH_MASK);
        let hash = |s: &str| if folded { folded_string_hash(s) } else { string_hash(s) };
        if let Some(v) = dom.get_attribute(id, "id") {
            if !v.is_empty() {
                add(hash(v) & BLOOM_HASH_MASK);
            }
        }
        if let Some(v) = dom.get_attribute(id, "class") {
            for class in v.split_ascii_whitespace() {
                add(hash(class) & BLOOM_HASH_MASK);
            }
        }
    }
}

/// [`parser::AncestorHashes`] with each class and id hashed folded to ASCII lowercase, as a quirks
/// document's [`AncestorBloom`] holds them. The crate's own quirks form leaves class and id
/// out altogether, which filters almost nothing on a page styled by classes; its walk
/// (`collect_ancestor_hashes`) is private, so it is repeated here with only that hash changed.
fn folded_ancestor_hashes(selector: &parser::Selector<KorvusSelectorImpl>) -> parser::AncestorHashes {
    let (mut hashes, mut len) = ([0u32; 4], 0usize);
    collect_folded_hashes(FoldedAncestors::new(selector.iter()), &mut hashes, &mut len);
    if len == 4 {
        let fourth = hashes[3];
        hashes[0] |= (fourth & 0x0000_00ff) << 24;
        hashes[1] |= (fourth & 0x0000_ff00) << 16;
        hashes[2] |= (fourth & 0x00ff_0000) << 8;
    }
    parser::AncestorHashes { packed_hashes: [hashes[0], hashes[1], hashes[2]] }
}

/// The simple selectors of a selector's ancestor compounds: the crate's `AncestorIter`.
struct FoldedAncestors<'a>(parser::SelectorIter<'a, KorvusSelectorImpl>);

impl<'a> FoldedAncestors<'a> {
    /// Past the subject's compound, and any compound that is not reached by a child or descendant
    /// combinator.
    fn new(inner: parser::SelectorIter<'a, KorvusSelectorImpl>) -> Self {
        let mut this = FoldedAncestors(inner);
        this.skip_until_ancestor();
        this
    }

    fn skip_until_ancestor(&mut self) {
        loop {
            while self.0.next().is_some() {}
            if self.0.next_sequence().is_none_or(|c| matches!(c, parser::Combinator::Child | parser::Combinator::Descendant)) {
                break;
            }
        }
    }
}

impl<'a> Iterator for FoldedAncestors<'a> {
    type Item = &'a parser::Component<KorvusSelectorImpl>;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.0.next();
        if next.is_some() {
            return next;
        }
        if let Some(c) = self.0.next_sequence() {
            if !matches!(c, parser::Combinator::Child | parser::Combinator::Descendant) {
                self.skip_until_ancestor();
            }
        }
        self.0.next()
    }
}

/// The crate's `collect_selector_hashes`, with a class or id hashed folded. False when the four
/// hashes are taken.
fn collect_folded_hashes<'a>(
    iter: impl Iterator<Item = &'a parser::Component<KorvusSelectorImpl>>,
    hashes: &mut [u32; 4],
    len: &mut usize,
) -> bool {
    use parser::{Component, SelectorImpl};
    for component in iter {
        let hash = match component {
            Component::LocalName(parser::LocalName { name, lower_name }) => {
                if name != lower_name {
                    continue;
                }
                name.precomputed_hash()
            }
            Component::DefaultNamespace(url) | Component::Namespace(_, url) => url.precomputed_hash(),
            Component::ID(id) => id.2,
            Component::Class(class) => class.2,
            Component::AttributeInNoNamespace { local_name, .. } if KorvusSelectorImpl::should_collect_attr_hash(local_name) => {
                local_name.precomputed_hash()
            }
            Component::AttributeInNoNamespaceExists { local_name, local_name_lower } => {
                if local_name != local_name_lower || !KorvusSelectorImpl::should_collect_attr_hash(local_name) {
                    continue;
                }
                local_name.precomputed_hash()
            }
            Component::AttributeOther(attr) => {
                if attr.local_name != attr.local_name_lower || !KorvusSelectorImpl::should_collect_attr_hash(&attr.local_name) {
                    continue;
                }
                attr.local_name.precomputed_hash()
            }
            Component::Is(list) | Component::Where(list) => {
                // One alternative only: with more, a hash from one would reject an element the other matches.
                let alternatives = list.slice();
                if alternatives.len() == 1 && !collect_folded_hashes(FoldedAncestors(alternatives[0].iter()), hashes, len) {
                    return false;
                }
                continue;
            }
            _ => continue,
        };
        hashes[*len] = hash & BLOOM_HASH_MASK;
        *len += 1;
        if *len == hashes.len() {
            return false;
        }
    }
    true
}

impl std::fmt::Debug for AncestorBloom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AncestorBloom")
    }
}

/// Which index bucket a selector belongs in. See [`Selector::bucket`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelectorBucket {
    Id(String),
    Class(String),
    /// A lowercase tag name — lowercase because that is how HTML elements are stored, so the key
    /// matches without a case-folding step at lookup time.
    LocalName(String),
    /// The lowercase name of an attribute the rightmost compound requires (`[data-aos]`,
    /// `[type="button"]`): only an element with an attribute of that name, in any case, can match. Blink
    /// buckets these too; without it, a page's attribute-only rules are tried against every element
    /// (www.jlcpcb.com: ~300 AOS rules, 98 % of its 309 M candidates).
    Attribute(String),
    /// No key: it has to be tried against every element.
    Universal,
}

#[cfg(test)]
mod cascade_selector_tests {
    use super::*;
    use crate::tree::Dom;

    fn one(text: &str) -> Selector {
        let mut list = Selector::parse_list(text).unwrap();
        assert_eq!(list.len(), 1, "{text:?} is one selector");
        list.remove(0)
    }

    fn find(dom: &Dom, tag: &str) -> NodeId {
        (0..dom.len() as NodeId)
            .find(|&id| match &dom.node(id).kind {
                NodeKind::Element { name, .. } => name.eq_ignore_ascii_case(tag),
                _ => false,
            })
            .expect("element present")
    }

    // `&` outside a nested rule is `:scope`. Chrome 150 on a page whose body
    // holds two divs: `document.querySelectorAll("& > div").length` is 0 (the scope is the root, and
    // no div is the root's child), `document.body.querySelectorAll("& > div").length` is 2.
    #[test]
    fn the_nesting_selector_outside_a_rule_is_the_scope() {
        let dom = Dom::parse("<html><body><div></div><div><div></div></div></body></html>");
        assert_eq!(dom.query_selector_all("& > div").expect("`&` parses").len(), 0);
        let body = find(&dom, "body");
        assert_eq!(dom.descendants_matching(body, "& > div").unwrap().len(), 2);
    }

    // A nested rule's selector is relative to its parent, and `&` becomes the parent's selectors with
    // `:is()` specificity.
    #[test]
    fn a_nested_selector_resolves_against_its_parent() {
        let dom = Dom::parse("<html><body><div class=n><p class=c></p></div><p class=c></p></body></html>");
        let parents = Selector::parse_list(".n").unwrap();
        let nested = Selector::parse_nested(".c").unwrap();
        assert_eq!(nested[0].to_css_string(), "& .c");
        let resolved = nested[0].replace_parent(&parents);
        let ps: Vec<NodeId> = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n) == Some("p")).collect();
        assert!(resolved.matches(&dom, ps[0]));
        assert!(!resolved.matches(&dom, ps[1]), "the second .c is not under .n");
        assert_eq!(resolved.specificity(), one(".n .c").specificity());
        let child = Selector::parse_nested("> .c").unwrap()[0].replace_parent(&Selector::parse_list("#x, .n").unwrap());
        assert_eq!(child.specificity(), one("#x > .c").specificity(), "the most specific parent counts");
    }

    // Only a selector that can write a `SelectorCaches` gets a fresh one per test. The
    // flag has to find the cache users at ANY depth -- a nested `:nth-child` inside `:is()` or a
    // `:has()` inside `:not()` writes the caches exactly as a top-level one does -- or a shared set
    // would carry an index from one DOM state into the next.
    #[test]
    fn only_index_and_has_selectors_are_marked_as_cache_users() {
        for text in [":first-child", "li:nth-child(2n+1)", "p:nth-last-of-type(2)", ":is(.a, :nth-child(2))",
                     "div :not(:has(> a))", ":where(ul > li:only-child) span", ":-webkit-any(li:nth-child(2))"] {
            assert!(one(text).uses_caches, "{text} writes the caches");
        }
        for text in ["div.a > p", "#main *", ":not(.x):hover", "a[href^=http]", ":is(h1, h2) + p", ":-webkit-any(.a)"] {
            assert!(!one(text).uses_caches, "{text} does not");
        }
    }

    // `:-webkit-any()` holds selectors as `:is()` does, and the visitors see inside it: the flags a
    // selector's matching depends on are found at any depth of it too.
    #[test]
    fn webkit_any_lists_are_visited() {
        assert!(one(":-webkit-any(li:nth-child(2))").uses_caches);
        assert!(one(":-webkit-any(p:nth-of-type(2), .x)").uses_caches);
        assert!(one(":-webkit-any(:has(> a))").uses_caches);
        assert!(!one(":-webkit-any(.a, b)").uses_caches);
        assert!(one(":-webkit-any([style])").mentions_style_attribute());
        assert!(one("p:-webkit-any([data-x])").mentions_data_attribute());
        assert!(!one(":-webkit-any([title])").mentions_unrevised_attribute());
    }

    // The case the flag guards: matched against the shared, never-cleared set, the nth index a
    // `:-webkit-any()` wrote there was asked again after the list changed under it.
    #[test]
    fn webkit_any_nth_child_is_not_cached_across_mutations() {
        let mut dom = Dom::parse("<!doctype html><ul><li id=a></li><li id=b></li><li id=c></li></ul>");
        let (a, b) = (dom.get_element_by_id("a").unwrap(), dom.get_element_by_id("b").unwrap());
        let ul = find(&dom, "ul");
        let s = one(":-webkit-any(li:nth-child(2))");
        assert!(s.matches_with(&dom, b, None) && !s.matches_with(&dom, a, None));
        let li = dom.create_element("li");
        dom.insert_before(ul, li, a);
        assert!(!s.matches_with(&dom, b, None), "#b is third now");
        assert!(s.matches_with(&dom, a, None), "and #a second");
    }

    // A lowercase type selector matches a mixed-case foreign element in an HTML document, so the
    // filter of its descendants has to hold that name lowercased, or `clippath rect` is rejected
    // before the matcher that would say yes is asked. XML stays exact with and without the filter.
    #[test]
    fn bloom_keeps_mixed_case_ancestors() {
        let mut dom = Dom::parse("<!doctype html><body><svg><clipPath><rect id=r></rect></clipPath></svg>");
        let r = dom.get_element_by_id("r").unwrap();
        for text in ["clippath rect", "clipPath rect", "svg clippath > rect", "CLIPPATH rect"] {
            let s = one(text);
            assert!(s.matches_with(&dom, r, None), "{text} matches");
            assert!(s.matches_with(&dom, r, Some(&AncestorBloom::for_node(&dom, r))), "{text} matches through the filter");
        }
        let x = dom.parse_xml_into("<svg><clipPath><rect/></clipPath></svg>");
        let xr = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n) == Some("rect") && dom.root_of(n) == x).next().unwrap();
        assert!(!one("clippath rect").matches_with(&dom, xr, None), "XML does not fold");
        assert!(!one("clippath rect").matches_with(&dom, xr, Some(&AncestorBloom::for_node(&dom, xr))));
        assert!(one("clipPath rect").matches_with(&dom, xr, Some(&AncestorBloom::for_node(&dom, xr))));
    }

    // ...and the ones that share the set never leave anything in it: an index answer asked twice
    // across a DOM change is recomputed, not remembered, whatever was matched in between.
    #[test]
    fn an_index_selector_sees_the_dom_change_between_two_matches() {
        let mut dom = Dom::parse("<ul><li class='a'>1</li><li class='b'>2</li></ul>");
        let ul = find(&dom, "ul");
        let second = (0..dom.len() as NodeId).find(|&n| dom.get_attribute(n, "class") == Some("b")).unwrap();
        let nth2 = one("li:nth-child(2)");
        let plain = one("ul > li");
        assert!(nth2.matches(&dom, second));
        assert!(plain.matches(&dom, second));
        let li = dom.create_element("li");
        dom.prepend_child(ul, li);
        assert!(plain.matches(&dom, second));
        assert!(!nth2.matches(&dom, second), "the .b item is third now");
        assert!(one("li:nth-child(3)").matches(&dom, second));
    }

    // A rule's selector list is N selectors, not one: they specify separately and bucket
    // separately, so the cascade never sees the comma.
    #[test]
    fn a_list_comes_back_as_its_individual_selectors() {
        let list = Selector::parse_list("h1, .lede, #main p").unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].bucket(), SelectorBucket::LocalName("h1".into()));
        assert_eq!(list[1].bucket(), SelectorBucket::Class("lede".into()));
        assert_eq!(list[2].bucket(), SelectorBucket::LocalName("p".into()));
    }

    // CSS invalidates the whole rule when any selector in its list fails to parse, rather than
    // dropping the bad one and keeping the rest.
    #[test]
    fn one_bad_selector_rejects_the_whole_list() {
        assert!(Selector::parse_list("h1, ((").is_err());
        assert!(Selector::parse_list("").is_err());
    }

    #[test]
    fn specificity_orders_id_over_class_over_type() {
        let id = one("#main").specificity();
        let class = one(".lede").specificity();
        let tag = one("p").specificity();
        assert!(id > class && class > tag);
        // Ten classes still lose to one id: the fields do not carry.
        assert!(id > one(".a.b.c.d.e.f.g.h.i.j").specificity());
        assert_eq!(one("*").specificity(), 0);
    }

    // The key comes from the rightmost compound, because that is the part that must match the
    // element itself -- everything left of it constrains ancestors.
    #[test]
    fn the_bucket_key_is_the_rightmost_compound() {
        assert_eq!(one("#nav a").bucket(), SelectorBucket::LocalName("a".into()));
        assert_eq!(one("div .lede").bucket(), SelectorBucket::Class("lede".into()));
        assert_eq!(one("a > #target").bucket(), SelectorBucket::Id("target".into()));
        assert_eq!(one("DIV").bucket(), SelectorBucket::LocalName("div".into()), "keys are lowercase");
    }

    #[test]
    fn within_a_compound_the_most_selective_key_wins() {
        assert_eq!(one("div.foo#bar").bucket(), SelectorBucket::Id("bar".into()));
        assert_eq!(one("div.foo.baz").bucket(), SelectorBucket::Class("foo".into()));
    }

    // The residue: selectors that constrain nothing about the element's identity have to be tried
    // against every element, so they cannot be indexed away.
    #[test]
    fn selectors_with_no_key_fall_into_the_universal_bucket() {
        assert_eq!(one("*").bucket(), SelectorBucket::Universal);
        assert_eq!(one(":hover").bucket(), SelectorBucket::Universal);
        assert_eq!(one(".a > *").bucket(), SelectorBucket::Universal);
        assert_eq!(one(":not([hidden])").bucket(), SelectorBucket::Universal, "a negated attribute requires nothing");
        assert_eq!(one("[*|href]").bucket(), SelectorBucket::Universal, "nor one in any namespace");
    }

    // An attribute the rightmost compound requires keys it, by its lowercase name, after an id or
    // a class and before the tag (Blink's order).
    #[test]
    fn a_required_attribute_keys_the_bucket() {
        assert_eq!(one("[hidden]").bucket(), SelectorBucket::Attribute("hidden".into()));
        assert_eq!(one("[viewBox]").bucket(), SelectorBucket::Attribute("viewbox".into()));
        assert_eq!(one("[TYPE='button' i]").bucket(), SelectorBucket::Attribute("type".into()));
        assert_eq!(one("input[type=text]").bucket(), SelectorBucket::Attribute("type".into()));
        assert_eq!(one(".x[type=text]").bucket(), SelectorBucket::Class("x".into()));
        assert_eq!(one("[data-a] p").bucket(), SelectorBucket::LocalName("p".into()), "only the rightmost compound");
        assert_eq!(one("[class^='el-icon-']::before").bucket(), SelectorBucket::Attribute("class".into()));
    }

    // `:hover` used to be a `UnsupportedValid("hover")`, which serialised by writing
    // the name it stored. The dedicated variant must produce the same text, or every
    // `cssRules[n].selectorText` carrying `:hover` changes under a page's feet.
    #[test]
    fn hover_serialises_back_as_hover() {
        assert_eq!(one(":hover").to_css_string(), ":hover");
        assert_eq!(one("a:hover").to_css_string(), "a:hover");
        assert_eq!(one("li:hover > ul").to_css_string(), "li:hover > ul");
        assert_eq!(one(":HOVER").to_css_string(), ":hover", "pseudo names are case-insensitive");
        assert_eq!(one("a:hover").bucket(), SelectorBucket::LocalName("a".into()));
    }

    // The same Dom, the same hover chain, asked the two ways a page asks it. These are the two
    // halves the brief forbids from being separate sources; here they are one call apart.
    #[test]
    fn element_matches_and_the_query_engine_agree_about_hover() {
        // `<!doctype html>`: in a quirks-mode document a bare `:hover` matches only links
        // (the :hover quirk), which is not what this test is about.
        let mut dom = Dom::parse(
            "<!doctype html><html><body><div id='outer'><div id='mid'><div id='inner'></div></div></div>\
             <div id='other'></div></body></html>",
        );
        let by_id = |n: &str| dom.get_element_by_id(n).expect("element present");
        let (outer, mid, inner, other) =
            (by_id("outer"), by_id("mid"), by_id("inner"), by_id("other"));
        dom.set_hover_chain(vec![outer, mid, inner]);

        let sel = one(":hover");
        let by_matches: Vec<bool> =
            [outer, mid, inner, other].iter().map(|n| sel.matches(&dom, *n)).collect();
        let by_query: Vec<bool> = [outer, mid, inner, other]
            .iter()
            .map(|n| super::matches_one(&dom, *n, &super::parse_list(":hover").unwrap()))
            .collect();
        assert_eq!(by_matches, vec![true, true, true, false]);
        assert_eq!(by_query, by_matches, "one state, or the design is wrong");
    }

    #[test]
    fn matching_is_the_same_answer_the_query_engine_gives() {
        let dom = Dom::parse("<html><body><div id='main' class='box'><p>hi</p></div></body></html>");
        let div = find(&dom, "div");
        let p = find(&dom, "p");
        assert!(one("#main").matches(&dom, div));
        assert!(one(".box").matches(&dom, div));
        assert!(one("div.box#main").matches(&dom, div));
        assert!(one("#main p").matches(&dom, p));
        assert!(!one("#main").matches(&dom, p));
        assert!(!one("span").matches(&dom, p));
    }

    // The cascade holds these in a service on the kernel Context, which is `Send + Sync`. The
    // `Rc`-based CompiledSelector cannot go there, which is the whole reason this type exists.
    // `selectorText` is specified to return the *canonical* serialization, not what the page wrote,
    // which is why it comes from the matcher rather than from a kept source string.
    #[test]
    fn serialization_is_canonical_and_not_the_source_text() {
        // Case is *preserved* in a type selector -- matching is case-insensitive in HTML but the
        // serialization is not a normalization of it, and Chrome returns `DIV.a` here too. Only the
        // whitespace, the quoting, and the escaping are canonicalized.
        assert_eq!(one("DIV.a").to_css_string(), "DIV.a");
        // The one that was actually broken: a class is an identifier, not a string.
        assert_eq!(one(".a").to_css_string(), ".a");
        assert_eq!(one("[href=\"q\"]").to_css_string(), "[href=\"q\"]", "an attr value still quotes");
        assert_eq!(one("  p   >   a  ").to_css_string(), "p > a");
        assert_eq!(one("a[href]").to_css_string(), "a[href]");
        assert_eq!(one("#x").to_css_string(), "#x");
        assert_eq!(one("*").to_css_string(), "*");
        assert_eq!(one("a:hover").to_css_string(), "a:hover");
        assert_eq!(one("li:nth-child(2n+1)").to_css_string(), "li:nth-child(2n+1)");
    }

    #[test]
    fn serialization_round_trips_through_the_parser() {
        for text in ["div.a", "p > a", "#x .y z", "a[href=\"q\"]", "ul li:first-child"] {
            let once = one(text).to_css_string();
            let twice = one(&once).to_css_string();
            assert_eq!(once, twice, "{text:?} does not round-trip");
        }
    }

    // The only way a bloom filter can be wrong that matters: saying "no" when the answer is yes.
    // A false positive costs a walk that then answers correctly; a false negative is a rule that
    // silently never applies. So the filter is checked against the unfiltered answer for every
    // element of a document, not spot-checked.
    #[test]
    fn the_bloom_filter_never_turns_a_match_into_a_miss() {
        let dom = Dom::parse(
            "<html><body><div class='wrap page' id='top'>\
               <main><ul class='list'>\
                 <li class='item first' id='a'><span class='label'><a href='#'>x</a></span></li>\
                 <li class='item' id='b'><span><em>y</em></span></li>\
               </ul></main>\
               <section class='row'><div class='cell'><a>z</a></div></section>\
             </div></body></html>",
        );
        let selectors = [
            "a", "*", ".item a", ".wrap .list .item", "#top a", "div.cell a", "ul > li > span",
            ".label a", "section .row a", ".wrap .row .cell a", "li.item.first", "em",
            ".missing a", "#nope span", "body div ul li span a", "[href]", ".page :not(.item)",
        ];
        for text in selectors {
            let sel = Selector::parse_list(text).unwrap().remove(0);
            for id in 0..dom.len() as NodeId {
                let bloom = AncestorBloom::for_node(&dom, id);
                assert_eq!(
                    sel.matches_with(&dom, id, Some(&bloom)),
                    sel.matches(&dom, id),
                    "{text:?} disagrees with itself on node {id}"
                );
            }
        }
    }

    // A quirks document (no doctype) folds class and id case, and its filter is consulted,
    // folded on both sides. Every case mix of the page's classes and ids, checked against the
    // unfiltered answer for every element; and the filter does reject what no ancestor supplies.
    #[test]
    fn a_quirks_documents_folded_filter_never_turns_a_match_into_a_miss() {
        let dom = Dom::parse(
            "<html><body><div class='Wrap page' id='Top'><ul class='LIST'>\
               <li class='item First' id='a'><span class='label'><a href='#'>x</a></span></li>\
             </ul></div><p class='nope-ish'><a>y</a></p></body></html>",
        );
        let a = (0..dom.len() as NodeId).filter(|&n| dom.tag_name(n) == Some("a")).collect::<Vec<_>>();
        assert_eq!(super::quirks_mode_of(&dom, a[0]), QuirksMode::Quirks);
        let selectors = [
            ".wrap a", ".WRAP .list .ITEM a", "#top a", "#TOP span", ".page .First a", ":is(.wRaP) a",
            "div.wrap > ul.list li", ".nope a", "#a a", "#A > span > A", ".Nope-ISH a", ".label a",
        ];
        for text in selectors {
            let sel = Selector::parse_list(text).unwrap().remove(0);
            for id in 0..dom.len() as NodeId {
                let bloom = AncestorBloom::for_node(&dom, id);
                assert!(bloom.folded, "a quirks document's filter is folded");
                assert_eq!(sel.matches_with(&dom, id, Some(&bloom)), sel.matches(&dom, id), "{text:?} disagrees with itself on node {id}");
            }
        }
        let may = |text: &str, n: NodeId| {
            let sel = Selector::parse_list(text).unwrap().remove(0);
            selectors::matching::selector_may_match(&sel.folded_hashes, &AncestorBloom::for_node(&dom, n).filter)
        };
        assert!(may(".WRAP .list a", a[0]) && may("#TOP a", a[0]), "the ancestors' folded classes and id are in");
        assert!(!may(".nope a", a[0]) && !may(".wrap a", a[1]), "and a class no ancestor has is rejected");
    }

    // push/pop is the traversal form of `for_node`, and the two have to agree or a document pass
    // and a single-element query would answer differently.
    #[test]
    fn pushing_down_a_chain_gives_the_same_filter_as_building_it_at_the_bottom() {
        // `<!doctype html>`: `AncestorBloom::new` is the no-quirks filter (a quirks document's is
        // folded; see the test above).
        let dom = Dom::parse("<!doctype html><html><body><div class='a'><p class='b'><span id='s'>x</span></p></div></body></html>");
        let span = (0..dom.len() as NodeId)
            .find(|&n| dom.get_attribute(n, "id") == Some("s"))
            .unwrap();
        // Walk down to the span, pushing as we go.
        let mut chain = Vec::new();
        let mut cur = dom.node(span).parent;
        while let Some(n) = cur {
            chain.push(n);
            cur = dom.node(n).parent;
        }
        chain.reverse();
        let mut incremental = AncestorBloom::new();
        for &n in &chain {
            incremental.push(&dom, n);
        }
        let sel = Selector::parse_list(".a .b span").unwrap().remove(0);
        assert!(sel.matches_with(&dom, span, Some(&incremental)));
        assert!(sel.matches_with(&dom, span, Some(&AncestorBloom::for_node(&dom, span))));

        // And popping back out again leaves nothing behind: a selector needing `.a` is now rejected.
        for &n in chain.iter().rev() {
            incremental.pop(&dom, n);
        }
        let mut only_p = incremental;
        only_p.push(&dom, chain[chain.len() - 1]);
        assert!(!Selector::parse_list(".a span").unwrap().remove(0).matches_with(&dom, span, Some(&only_p)));
    }

    #[test]
    fn a_selector_can_cross_a_thread_boundary() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Selector>();
        assert_send_sync::<SelectorBucket>();
    }
}
