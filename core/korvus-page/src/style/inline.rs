//! AN ELEMENT'S INLINE DECLARATION BLOCK as Blink holds and serializes it (a bot-detection script D6).
//!
//! a bot-detection script's challenge builds its frame with `setAttribute('style', 'display: none; width: 100%; height: 52px;
//! border: 0;')` and reads `style.cssText` back: Chrome 150 answers `... border: 0px;` (a declaration block is parsed
//! into longhands and serialized from them -- `StylePropertySerializer::AsText`), this engine answered the attribute's
//! own text. The same block was the answer to `style.length` (Chrome 20: `border` is seventeen longhands; here 4),
//! to `style.borderTopWidth` (`0px`; here ``), to `getPropertyPriority`, and to the attribute Blink writes back after
//! a CSSOM change (`border: 0px; z-index: 3;`; here `border: 0; z-index: 3;`). Measured on Chrome 150
//! (`fixtures/css-answers/inline-style-probe.js`).
//!
//! The block here is Blink's `CSSPropertyValueSet` for a `style` attribute: its declarations one by one, each
//! expanded into its longhands by this crate's parser (the same expansion a style rule's block gets), each longhand's
//! value as `CSSValue::CssText` writes it ([`serialize::value_text`]), in the order Blink's property vector keeps:
//!
//! * a `style` attribute (`CSSParserImpl::ParseInlineStyleDeclaration`) keeps its declarations as parsed when there
//!   are fewer than two, or two different ones (`CreateCSSPropertyValueSet`'s fast path) -- `color: red !important;
//!   width: 0` stays in that order -- and is otherwise `FilterProperties`'d: `!important` last, the last of a
//!   property winning;
//! * `cssText = ...` (`MutableCSSPropertyValueSet::ParseDeclarationList`) is always filtered;
//! * a setter (`SetProperty`) replaces a longhand where it stands, or appends it.
//!
//! What the block reads back is this crate's own block serializer ([`serialize::block_text`],
//! [`serialize::shorthand_value`]), the one a style rule's `cssText` already goes through.

use crate::style::parse::parse_inline_style;
use crate::style::serialize::{self, Entry};
use cssparser::{Delimiter, ParseError, Parser, ParserInput};

/// The declarations of a `style` attribute, longhand by longhand, in Blink's property-vector order.
#[derive(Debug, Clone, Default)]
pub struct InlineStyle {
    entries: Vec<Entry>,
}

/// What a setter did to a block ([`InlineStyle::set_outcome`]): Blink's `MutableCSSPropertyValueSet::SetResult` --
/// a value that expands to nothing, one every longhand of which already stood as it is (`kUnchanged`), or a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOutcome {
    Rejected,
    Unchanged,
    Changed,
}

/// Two entries alike in every field (`Entry` has no `PartialEq`).
fn same(a: &Entry, b: &Entry) -> bool {
    a.name == b.name && a.value == b.value && a.important == b.important && a.implicit == b.implicit
}

/// Every token up to the delimiter, nested blocks whole.
fn rest<'i, 't>(p: &mut Parser<'i, 't>) -> Result<(), ParseError<'i, ()>> {
    while p.next_including_whitespace_and_comments().is_ok() {}
    Ok(())
}

/// `text` cut at its top-level semicolons: each declaration as written.
///
/// Each piece runs to the position `parse_until_before` stops at. It ran to the position before the `;` token
/// was asked for, which is not the end of the piece when a function or block comes last: cssparser skips a
/// block's contents only on the NEXT call, so `width: calc(3px); z-index: 3` was cut as `width: calc(`, and every
/// declaration ending in a function before a `;` -- `rgba(...)`, `scale(...)`, `calc(...)` -- was lost or read
/// back as `scale()`.
fn pieces(text: &str) -> Vec<&str> {
    let mut input = ParserInput::new(text);
    let mut p = Parser::new(&mut input);
    let mut out = Vec::new();
    loop {
        let start = p.position();
        let _ = p.parse_until_before(Delimiter::Semicolon, rest);
        out.push(p.slice_from(start));
        if p.next_including_whitespace_and_comments().is_err() {
            return out;
        }
    }
}

/// One declaration's longhands (a custom property, or a declaration holding `var()`, as written).
///
/// A function of `piece` alone, so the recent answers are kept: a removal asked it for the property and again for
/// every declaration the cascade's text holds (`longhand_names`), and a shorthand write for its value -- each a full
/// parse (~0.3 us; `el.style.left = ''` beside two declarations was 163k instructions, 2026-10-23). Bounded by
/// clearing.
fn entries_of(piece: &str) -> std::rc::Rc<[Entry]> {
    thread_local! {
        static ENTRIES: std::cell::RefCell<korvus_dom::FxHashMap<Box<str>, std::rc::Rc<[Entry]>>> = Default::default();
        static ENTRIES_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    if let Some(hit) = ENTRIES.with(|c| c.borrow().get(piece).cloned()) {
        return hit;
    }
    let fresh: std::rc::Rc<[Entry]> = entries_of_uncached(piece).into();
    ENTRIES.with(|c| {
        let mut c = c.borrow_mut();
        // bounded by its bytes too (`style::memo`): the piece and the entries' texts
        let cost = piece.len() + fresh.iter().map(entry_bytes).sum::<usize>();
        if ENTRIES_BYTES.with(|b| crate::style::memo::admit(c.len(), 1024, b, cost, || c.clear())) {
            c.insert(piece.into(), fresh.clone());
        }
    });
    fresh
}

/// The bytes of an entry's texts, as a memo counts them (`style::memo`).
fn entry_bytes(e: &Entry) -> usize {
    e.name.len() + e.value.len()
}

/// The bytes of a block's texts, as a memo counts them (`style::memo`).
fn block_bytes(b: &InlineStyle) -> usize {
    b.entries.iter().map(entry_bytes).sum()
}

fn entries_of_uncached(piece: &str) -> Vec<Entry> {
    // a declaration the property does not accept is not in the block (`style="width: 10px; height: x"` is
    // `width: 10px;` in Chrome 150) -- the same test `CSS.supports` and the setters ask
    let Some((name, value)) = piece.split_once(':') else { return Vec::new() };
    if !crate::style::supports::supports_declaration(name.trim(), value) {
        return Vec::new();
    }
    let sheet = parse_inline_style(piece);
    let mut out = Vec::new();
    for d in sheet.declarations() {
        let name = d.property.name();
        let value = if d.implicit { "initial".to_string() } else { serialize::value_text(name, sheet.value_of(d)) };
        out.push(Entry { name: name.to_string(), value, important: d.important, implicit: d.implicit });
    }
    for r in sheet.raw_declarations() {
        out.push(Entry {
            name: sheet.slice(r.name).to_string(),
            value: sheet.slice(r.value).trim().to_string(),
            important: r.important,
            implicit: false,
        });
    }
    if out.is_empty() {
        // a property Blink exposes that this engine's cascade does not implement (`corner-shape`): kept as written
        let name = name.trim();
        let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
        let v = value.trim();
        let (v, important) = match v.rfind('!') {
            Some(i) if v[i + 1..].trim().eq_ignore_ascii_case("important") => (v[..i].trim(), true),
            _ => (v, false),
        };
        if !v.is_empty() {
            out.push(Entry { name, value: v.to_string(), important, implicit: false });
        }
    }
    out
}

/// A property name as `getPropertyValue` resolves it: a custom property as written, anything else lowercased and
/// through its alias. Borrowed unless the name had a capital: every read and write resolves its name, and a
/// lowercase name's alias lookup is `canonical_name`'s own search without its two copies.
fn resolve_name(name: &str) -> std::borrow::Cow<'_, str> {
    if name.starts_with("--") {
        name.into()
    } else if !name.bytes().any(|c| c.is_ascii_uppercase()) {
        crate::style::blink_names::exposed(name).unwrap_or(name).into()
    } else {
        crate::style::property::canonical_name(name).into()
    }
}

/// A value whose declaration `name: value` the parser reads as ONE declaration, whatever `name` is: printable
/// ASCII (so `trim` drops only the spaces CSS drops too), not blank, and no `;`, `!`, brace, backslash or comment
/// that could end it, add a priority, nest a rule or hide text.
fn plain_value(value: &str) -> bool {
    let b = value.as_bytes();
    !value.trim().is_empty()
        && b.iter().all(|&c| (0x20..=0x7e).contains(&c) && !matches!(c, b';' | b'!' | b'{' | b'}' | b'\\'))
        && !value.contains("/*")
}

/// A name whose declaration the parser files under exactly `name`: a custom property of plain characters, or a
/// lowercase longhand this engine stores as itself -- no alias (`canonical_name` keeps it), no shorthand, and not a
/// table row a declaration still expands (`shorthand::expands`).
fn plain_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("--") {
        return !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    }
    name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && crate::style::blink_names::exposed(name).map_or(true, |t| t == name)
        && crate::style::property::PropertyId::from_name(name)
            .is_some_and(|p| !crate::style::shorthand::expands(p) && p.name() == name)
}

/// The longhands `name` stands for in a declaration block: a shorthand's, or the property itself (a longhand, a custom
/// property, a name this engine does not expand).
pub fn longhand_names(name: &str) -> Vec<String> {
    let name = resolve_name(name);
    if name.starts_with("--") {
        return vec![name.into_owned()];
    }
    let expanded = entries_of(&format!("{name}: initial"));
    if expanded.is_empty() {
        vec![name.into_owned()]
    } else {
        expanded.iter().map(|e| e.name.clone()).collect()
    }
}

/// [`longhand_names`], kept by name: a CSSOM write asks it of every declaration of the element's list it cannot tell
/// apart otherwise (`CSSStyleDeclaration`'s `respell`), and each answer is a parse (`entries_of`'s piece, formatted).
/// Bounded by clearing, by count and by bytes (`style::memo`: a custom property's name is the page's).
pub fn longhand_set(name: &str) -> std::rc::Rc<[String]> {
    thread_local! {
        static NAMES: std::cell::RefCell<korvus_dom::FxHashMap<Box<str>, std::rc::Rc<[String]>>> = Default::default();
        static NAMES_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    if let Some(hit) = NAMES.with(|m| m.borrow().get(name).cloned()) {
        return hit;
    }
    let set: std::rc::Rc<[String]> = longhand_names(name).into();
    NAMES.with(|m| {
        let mut m = m.borrow_mut();
        let cost = name.len() + set.iter().map(String::len).sum::<usize>();
        if NAMES_BYTES.with(|b| crate::style::memo::admit(m.len(), 1024, b, cost, || m.clear())) {
            m.insert(name.into(), set.clone());
        }
    });
    set
}

/// The four-side shorthands, which read back from their four longhands (as a style rule's block reads them).
fn sides(name: &str) -> Option<[&'static str; 4]> {
    Some(match name {
        "margin" => ["margin-top", "margin-right", "margin-bottom", "margin-left"],
        "padding" => ["padding-top", "padding-right", "padding-bottom", "padding-left"],
        "inset" => ["top", "right", "bottom", "left"],
        "border-width" => ["border-top-width", "border-right-width", "border-bottom-width", "border-left-width"],
        "border-style" => ["border-top-style", "border-right-style", "border-bottom-style", "border-left-style"],
        "border-color" => ["border-top-color", "border-right-color", "border-bottom-color", "border-left-color"],
        _ => return None,
    })
}

thread_local! {
    /// The blocks of the attribute texts read last: a page reads `el.style.x` in loops, and a block is a function
    /// of its text alone. Only a block of at most [`SHARED_BYTES`]; bounded by clearing.
    static PARSED: std::cell::RefCell<korvus_dom::FxHashMap<String, InlineStyle>> = Default::default();
    /// The blocks read off a CSSOM write's cascade text and attribute ([`InlineStyle::with_written`]), by the cascade
    /// text, with the attribute: a cache of a function of the two texts. Only a block of at most [`SHARED_BYTES`].
    static WRITTEN: std::cell::RefCell<korvus_dom::FxHashMap<String, (String, InlineStyle)>> = Default::default();
    /// The bytes [`PARSED`] and [`WRITTEN`] hold (`style::memo`).
    static PARSED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static WRITTEN_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The most a block [`PARSED`] or [`WRITTEN`] keeps costs, in bytes of the texts it holds (`style::memo`): 2 KiB, so
/// [`PARSED`]'s 512 entries are its byte bound too. A larger block is its element's to keep, not these maps'
/// ([`InlineStyle::with_written`] hands it back: `CSSStyleDeclaration`'s slot, stamped with the element's style-write
/// count), as Blink keeps each element's parsed block.
///
/// Kept here up to a quarter of the byte bound, a few elements' large attributes were all the map could hold: it was
/// cleared whole when they passed it, and anything over that quarter had one slot of its own. Forty elements parsed
/// with `style="background-image: url(data:...30 KB...); margin: 1px"`, read in turn, parsed an attribute again on
/// almost every read -- 3.2M instructions a read where the base, bounded by its count alone, answered in 23k; 58 ms for
/// 400 reads where Chrome 152 and the base take 0-1 (`vrr.html`, 2026-10-23).
pub const SHARED_BYTES: usize = crate::style::memo::BYTES / 512;

/// Keep `block` as `text`'s parse in [`PARSED`], within its bounds (`style::memo`); the block back when it costs
/// more than [`SHARED_BYTES`].
fn keep_parsed(text: &str, block: InlineStyle) -> Option<InlineStyle> {
    let cost = text.len() + block_bytes(&block);
    if cost > SHARED_BYTES {
        return Some(block);
    }
    PARSED.with(|c| {
        let mut c = c.borrow_mut();
        if PARSED_BYTES.with(|b| crate::style::memo::admit(c.len(), 512, b, cost, || c.clear())) {
            c.insert(text.to_string(), block);
        }
    });
    None
}

/// `f` on the block [`PARSED`] keeps for `text`, or `Err(f)`.
fn with_kept<R, F: FnOnce(&InlineStyle) -> R>(text: &str, f: F) -> Result<R, F> {
    // `f` reads the block (values, priorities, the serializer, `entries_of`) and never this cache
    PARSED.with(|c| match c.borrow().get(text) { Some(b) => Ok(f(b)), None => Err(f) })
}

/// What opens the note of a block's longhand order the cascade's text carries ([`InlineStyle::note_order`]).
const ORDER: &str = "/*order ";

/// The longhand order [`InlineStyle::note_order`] noted at the end of `cascade`: its names, each escaped
/// ([`escaped_name`]), one space apart.
fn noted_order(cascade: &str) -> Option<&str> {
    let body = cascade.strip_suffix("*/")?;
    let at = body.rfind(ORDER)?;
    Some(&body[at + ORDER.len()..])
}

/// `name` as an order note spells it: every byte but an ASCII letter, digit, `-` or `_` as `%XX`, so no name ends
/// the comment, holds a `:` or a space, or is mistaken for another.
fn escaped_name(name: &str, out: &mut String) {
    let plain = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    // a property's own name is always plain; only a custom property's is the page's
    if !name.starts_with("--") || name.bytes().all(plain) {
        out.push_str(name);
        return;
    }
    for &c in name.as_bytes() {
        if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
}

/// Is `noted` (escaped) the name `name`?
fn is_noted_name(name: &str, noted: &str) -> bool {
    if !noted.contains('%') {
        return name == noted;
    }
    let mut e = String::with_capacity(noted.len());
    escaped_name(name, &mut e);
    e == noted
}

impl InlineStyle {
    /// A `style` attribute's block (`ParseInlineStyleDeclaration`): what `setAttribute('style', text)` makes.
    pub fn parse_attribute(text: &str) -> InlineStyle {
        InlineStyle::with_attribute(text, InlineStyle::clone)
    }

    /// `f` on the block [`InlineStyle::parse_attribute`] answers for `text`, read where it is kept rather than
    /// copied out. A read of `el.style.x` hashed the attribute's text and cloned its whole block (2026-10-23).
    pub fn with_attribute<R>(text: &str, f: impl FnOnce(&InlineStyle) -> R) -> R {
        InlineStyle::with_attribute_own(text, f).0
    }

    /// [`InlineStyle::with_attribute`], and the block when [`PARSED`] did not keep it (larger than [`SHARED_BYTES`]).
    fn with_attribute_own<R>(text: &str, f: impl FnOnce(&InlineStyle) -> R) -> (R, Option<InlineStyle>) {
        if text.trim().is_empty() {
            return (f(&InlineStyle::default()), None);
        }
        let f = match with_kept(text, f) {
            Ok(r) => return (r, None),
            Err(f) => f,
        };
        let block = InlineStyle::parse_attribute_uncached(text);
        let r = f(&block);
        (r, keep_parsed(text, block))
    }

    fn parse_attribute_uncached(text: &str) -> InlineStyle {
        let all: Vec<Entry> = pieces(text).into_iter().flat_map(|p| entries_of(p).to_vec()).collect();
        // `CreateCSSPropertyValueSet`'s fast path: two custom properties are the same property id there
        let same = |a: &Entry, b: &Entry| a.name == b.name || (a.name.starts_with("--") && b.name.starts_with("--"));
        let fast = all.len() < 2 || (all.len() == 2 && !same(&all[0], &all[1]));
        InlineStyle { entries: if fast { all } else { serialize::filter(all) } }
    }

    /// The block `cssText = text` makes (`ParseDeclarationList`): always filtered.
    pub fn parse_css_text(text: &str) -> InlineStyle {
        InlineStyle { entries: serialize::filter(pieces(text).into_iter().flat_map(|p| entries_of(p).to_vec()).collect()) }
    }

    /// `cssText`.
    pub fn css_text(&self) -> String {
        serialize::block_text(&self.entries)
    }

    /// [`InlineStyle::css_text`] and how many declarations it is ([`InlineStyle::note_order`]'s `declarations`).
    pub fn css_text_counted(&self) -> (String, usize) {
        serialize::block_text_counted(&self.entries)
    }

    /// The bytes [`InlineStyle::note_order`] appends for `declarations`, at most: what a write reserves for it, so
    /// the note is not a second allocation of the whole text (`realloc`, 1.5% of an `el.style.width = v`).
    pub fn order_note_bytes(&self, declarations: usize) -> usize {
        if self.parses_as_is(declarations) {
            return 0;
        }
        // a custom property's name escapes to at most three bytes a byte
        ORDER.len() + 3 + self.entries.iter().map(|e| 1 + if e.name.starts_with("--") { 3 * e.name.len() } else { e.name.len() }).sum::<usize>()
    }

    /// Note this block's longhand order at the end of `cascade`, the declarations' text a CSSOM write leaves for the
    /// cascade beside the attribute it serialized the block to (`Dom::set_inline_style`): `/*order <name> ...*/`, a
    /// comment the cascade reads past. [`InlineStyle::with_written`] reads it back.
    ///
    /// Blink keeps the mutated block itself, and the attribute is only its serialization, never parsed back. A parse of
    /// it is not the same block: a parse of three or more declarations is filtered (`!important` last), and a
    /// shorthand written back re-expands in its own order, not the order the setters left -- after
    /// `setProperty('display', 'none', 'important')`, `width` and `height`, Chrome 152 reads a clone's `cssText`
    /// `display: none !important; width: 10px; height: 10px;` where the parse put `display` last; after `marginLeft`
    /// then `margin`, `margin-left` first where the parse puts it last (`c2rev.html`, `c2rev2.html`, 2026-10-23). The
    /// order travels with the text the element and its clones carry (`Dom::clone_node` copies it, as Blink's
    /// `CloneAttributesFrom` copies `inline_style_`; `setAttribute('style', ...)` drops it), so it is the element's own,
    /// whatever another element left: it was looked up by the texts in a map every write filled, so two elements
    /// whose writes left the same texts read the order of whichever wrote last, and one whose texts were one kept
    /// none, nor did a block the map had dropped.
    ///
    /// Nothing is noted where the attribute's parse is this block in this order already ([`InlineStyle::parses_as_is`]):
    /// most writes (`width` beside `height`), whose cascade text then stays the attribute's own and costs `Dom` no copy.
    pub fn note_order(&self, cascade: &mut String, declarations: usize) {
        if self.parses_as_is(declarations) {
            return;
        }
        cascade.push(' ');
        cascade.push_str(ORDER);
        for (i, e) in self.entries.iter().enumerate() {
            if i > 0 {
                cascade.push(' ');
            }
            escaped_name(&e.name, cascade);
        }
        cascade.push_str("*/");
    }

    /// Does the attribute this block serializes to -- `declarations` declarations -- parse back to this block in this
    /// order? It does when it is one declaration per longhand (no shorthand gathered any: a shorthand re-expands in its
    /// own order) and no `!important` stands before a longhand without one (the parse files those last, keeping each
    /// side's order: `serialize::filter`). A block of fewer than two longhands has one order.
    pub fn parses_as_is(&self, declarations: usize) -> bool {
        self.entries.len() < 2
            || (declarations == self.entries.len()
                && self.entries.windows(2).all(|w| !w[0].important || w[1].important))
    }

    /// This block with its longhands in the order `noted` names them ([`noted_order`]), any it does not name after
    /// them as they stood.
    fn reordered(&self, noted: &str) -> InlineStyle {
        let mut rest: Vec<Option<&Entry>> = self.entries.iter().map(Some).collect();
        let mut entries = Vec::with_capacity(self.entries.len());
        for n in noted.split(' ').filter(|n| !n.is_empty()) {
            if let Some(e) = rest.iter_mut().find(|e| e.is_some_and(|e| is_noted_name(&e.name, n))).and_then(Option::take) {
                entries.push(e.clone());
            }
        }
        entries.extend(rest.into_iter().flatten().cloned());
        InlineStyle { entries }
    }

    /// `f` on the block of an element whose `style` attribute is `attribute` and whose cascade reads `cascade`
    /// (`Dom::style_for_cascade`): the attribute's parse, in the longhand order a CSSOM write noted in the cascade's
    /// text ([`InlineStyle::note_order`]). A note counts only in a text the CSSOM wrote -- one apart from the
    /// attribute: a cascade that reads the attribute itself reads the page's own text.
    ///
    /// And the block itself when it costs more than [`SHARED_BYTES`]: kept by its texts it would push every other
    /// element's out, so it is the element's to keep (`CSSStyleDeclaration`'s slot).
    pub fn with_written<R>(cascade: &str, attribute: &str, f: impl FnOnce(&InlineStyle) -> R) -> (R, Option<InlineStyle>) {
        let Some(noted) = noted_order(cascade).filter(|_| cascade != attribute) else {
            return InlineStyle::with_attribute_own(attribute, f);
        };
        // `f` reads the block and never this cache
        let f = match WRITTEN.with(|c| match c.borrow().get(cascade) {
            Some((text, b)) if text == attribute => Ok(f(b)),
            _ => Err(f),
        }) {
            Ok(r) => return (r, None),
            Err(f) => f,
        };
        let block = InlineStyle::with_attribute(attribute, |b| b.reordered(noted));
        let r = f(&block);
        // a function of the two texts, so kept by them (and dropped at its bounds, `style::memo`: it is read again)
        let cost = cascade.len() + attribute.len() + block_bytes(&block);
        if cost > SHARED_BYTES {
            return (r, Some(block));
        }
        WRITTEN.with(|c| {
            let mut c = c.borrow_mut();
            if WRITTEN_BYTES.with(|b| crate::style::memo::admit(c.len(), 4096, b, cost, || c.clear())) {
                c.insert(cascade.to_string(), (attribute.to_string(), block));
            }
        });
        (r, None)
    }

    /// [`InlineStyle::with_written`]'s block: a copy of a shared one, a large one as it comes back.
    pub fn written_block(cascade: &str, attribute: &str) -> InlineStyle {
        match InlineStyle::with_written(cascade, attribute, |b| (b.bytes() <= SHARED_BYTES).then(|| b.clone())) {
            (_, Some(own)) => own,
            (Some(copy), None) => copy,
            (None, None) => InlineStyle::with_written(cascade, attribute, InlineStyle::clone).0,
        }
    }

    /// The bytes of this block's texts, as the style memos count them (`style::memo`).
    pub fn bytes(&self) -> usize {
        block_bytes(self)
    }

    /// [`InlineStyle::css_text`] of the longhands `keep` names alone.
    pub fn css_text_of(&self, keep: impl Fn(&str) -> bool) -> String {
        let entries: Vec<Entry> = self.entries.iter().filter(|e| keep(&e.name)).cloned().collect();
        serialize::block_text(&entries)
    }

    /// `length`, and `item(i)`'s names: the longhands (and custom properties) in order.
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// `length`: [`InlineStyle::names`]' count.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `item(i)`: [`InlineStyle::names`]' `i`th.
    pub fn name_at(&self, i: usize) -> Option<&str> {
        self.entries.get(i).map(|e| e.name.as_str())
    }

    /// `getPropertyValue(name)`: a longhand's value, or a shorthand written from its longhands ("" when they cannot
    /// be).
    pub fn value(&self, name: &str) -> String {
        let name = resolve_name(name);
        if let Some(e) = self.entries.iter().rev().find(|e| e.name == name) {
            return if e.implicit { "initial".to_string() } else { e.value.clone() };
        }
        if let Some(four) = sides(&name) {
            let values: Option<Vec<&str>> =
                four.iter().map(|l| self.entries.iter().rev().find(|e| e.name == *l).map(|e| e.value.as_str())).collect();
            let Some(v) = values else { return String::new() };
            let (t, r, b, l) = (v[0], v[1], v[2], v[3]);
            return if l == r {
                if t == b {
                    if t == r { t.to_string() } else { format!("{t} {r}") }
                } else {
                    format!("{t} {r} {b}")
                }
            } else {
                format!("{t} {r} {b} {l}")
            };
        }
        serialize::shorthand_value(&name, &self.entries).unwrap_or_default()
    }

    /// The longhands a shorthand `name` stands for here (the ones the block holds), `None` for a longhand.
    fn longhands_of(&self, name: &str) -> Option<Vec<String>> {
        if self.entries.iter().any(|e| e.name == name) || name.starts_with("--") {
            return None;
        }
        let expanded = entries_of(&format!("{name}: initial"));
        (!expanded.is_empty() && expanded.iter().all(|e| e.name != name)).then(|| expanded.iter().map(|e| e.name.clone()).collect())
    }

    /// `getPropertyPriority(name)`: `important` for a longhand set so, and for a shorthand all of whose longhands are.
    pub fn priority(&self, name: &str) -> &'static str {
        let name = resolve_name(name);
        let important = match self.entries.iter().rev().find(|e| e.name == name) {
            Some(e) => e.important,
            None => match self.longhands_of(&name) {
                Some(longs) => {
                    !longs.is_empty()
                        && longs.iter().all(|l| self.entries.iter().rev().find(|e| e.name == *l).is_some_and(|e| e.important))
                }
                None => false,
            },
        };
        if important {
            "important"
        } else {
            ""
        }
    }

    /// `setProperty(name, value, priority)` for a value the caller has validated: each longhand replaced where it
    /// stands, or appended (`MutableCSSPropertyValueSet::SetLonghandProperty`). `false` when the value expands to
    /// nothing.
    ///
    /// A plain longhand or custom property with a plain value ([`plain_name`], [`plain_value`]) skips the parse
    /// [`InlineStyle::set_parsed`] makes: that parse can only answer one entry, named `name`, which the setter's
    /// own text then overwrites -- so the entry is built directly, after the same `supports` question. The parse
    /// was a second full `parse_inline_style` of every `el.style.x = v` (the setter's check had parsed it once):
    /// ~0.5 us of a 2.8 us write against Chrome 152's 0.2 (2026-10-23).
    pub fn set(&mut self, name: &str, value: &str, important: bool) -> bool {
        self.set_outcome(name, value, important) != SetOutcome::Rejected
    }

    /// [`InlineStyle::set`], saying whether the block changed.
    pub fn set_outcome(&mut self, name: &str, value: &str, important: bool) -> SetOutcome {
        if !important && plain_value(value) && plain_name(name) {
            let v = value.trim();
            if !crate::style::supports::supports_declaration(name, v) {
                return SetOutcome::Rejected;
            }
            let e = Entry { name: name.to_string(), value: v.to_string(), important: false, implicit: false };
            return if self.put(e) { SetOutcome::Changed } else { SetOutcome::Unchanged };
        }
        self.set_parsed(name, value, important)
    }

    /// `e` where its first namesake stands, or appended; whether that changed the block.
    fn put(&mut self, e: Entry) -> bool {
        match self.entries.iter_mut().find(|x| x.name == e.name) {
            Some(slot) => {
                let changed = !same(slot, &e);
                *slot = e;
                changed
            }
            None => {
                self.entries.push(e);
                true
            }
        }
    }

    /// [`InlineStyle::set_outcome`] through a parse of the declaration: which longhands it sets, and a shorthand's
    /// values.
    fn set_parsed(&mut self, name: &str, value: &str, important: bool) -> SetOutcome {
        let piece = format!("{name}: {value}{}", if important { " !important" } else { "" });
        let mut fresh = entries_of(&piece).to_vec();
        if fresh.is_empty() {
            return SetOutcome::Rejected;
        }
        // a longhand reads back as the caller wrote it -- `value` is the setter's canonical text already
        // (`CSSStyleDeclaration`'s `inline_value`, the form every setter answered in before this block existed); the
        // parse above only says which longhands it sets
        let resolved = resolve_name(name);
        if let [one] = fresh.as_mut_slice() {
            if one.name == resolved && !one.implicit {
                one.value = value.trim().to_string();
            }
        }
        // A shorthand whose value holds a `var()` is kept whole (its longhands wait on the substitution). Written, it
        // takes the place of itself and of every longhand of it already here, where the first of them stands -- Blink
        // sets each longhand to a pending substitution where it stands. Appended after them, they stayed: Framer's
        // `background: rgba(0,0,0,0)` then `background: linear-gradient(var(..))` computed no image (minea's header).
        if let [one] = fresh.as_slice() {
            let longs = longhand_set(&one.name);
            if one.name == resolved && !(longs.len() == 1 && longs[0] == one.name) {
                let covered = |n: &str| n == one.name || longs.iter().any(|l| l == n);
                if let Some(at) = self.entries.iter().position(|e| covered(&e.name)) {
                    let before = self.entries.len();
                    let changed = !(before == 1 + self.entries.iter().filter(|e| !covered(&e.name)).count() && same(&self.entries[at], one));
                    self.entries.retain(|e| !covered(&e.name));
                    self.entries.insert(at.min(self.entries.len()), one.clone());
                    return if changed { SetOutcome::Changed } else { SetOutcome::Unchanged };
                }
            }
        }
        let mut changed = false;
        for e in fresh {
            changed |= self.put(e);
        }
        if changed {
            SetOutcome::Changed
        } else {
            SetOutcome::Unchanged
        }
    }

    /// What `removeProperty(name)` answers: the value of a longhand or custom property, `""` for a shorthand (Blink's
    /// `RemoveProperty` fills its result for a longhand only; Chrome 152 answers `""` for `removeProperty('margin')`).
    pub fn removal_value(&self, name: &str) -> String {
        let name = resolve_name(name);
        if self.longhands_of(&name).is_some() {
            return String::new();
        }
        self.value(&name)
    }

    /// `removeProperty(name)`: the value it had, and every longhand of it gone.
    pub fn remove(&mut self, name: &str) -> String {
        let name = resolve_name(name);
        let old = self.value(&name);
        let longs = self.longhands_of(&name).unwrap_or_else(|| vec![name.to_string()]);
        self.entries.retain(|e| !longs.contains(&e.name));
        old
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A written `var()` shorthand takes the place of its longhands already in the block, where the first stood; a
    /// longhand written after it follows it, and wins.
    #[test]
    fn a_var_shorthand_takes_its_longhands_place() {
        let mut b = InlineStyle::parse_css_text("opacity: 1; background: rgba(0, 0, 0, 0); width: 1px");
        assert!(b.set("background", "linear-gradient(var(--t, red), blue)", false));
        let names = b.names();
        assert_eq!(names.iter().filter(|n| n.starts_with("background")).count(), 1, "{names:?}");
        assert_eq!(names, vec!["opacity", "background", "width"]);
        assert!(b.set("background-color", "rgba(0, 0, 0, 0)", false));
        assert_eq!(b.names(), vec!["opacity", "background", "width", "background-color"]);
    }

    /// Chrome 150's own answers for a `style` attribute (`fixtures/css-answers/inline-style-chrome-150.json`).
    #[test]
    fn an_attribute_block_reads_back_as_chrome_150_writes_it() {
        let b = InlineStyle::parse_attribute("display: none; width: 100%; height: 52px; border: 0;");
        assert_eq!(b.css_text(), "display: none; width: 100%; height: 52px; border: 0px;");
        assert_eq!(b.value("border-top-width"), "0px");
        let b = InlineStyle::parse_attribute("color: red; color: blue");
        assert_eq!((b.css_text().as_str(), b.names().len()), ("color: blue;", 1));
        let b = InlineStyle::parse_attribute("color: red !important; width: 0");
        assert_eq!(b.css_text(), "color: red !important; width: 0px;", "two different declarations keep their order");
        let b = InlineStyle::parse_css_text("color: red !important; width: 0");
        assert_eq!(b.css_text(), "width: 0px; color: red !important;", "cssText = ... moves !important last");
        let b = InlineStyle::parse_attribute("margin-top: 1px; margin-right: 1px; margin-bottom: 1px; margin-left: 1px");
        assert_eq!((b.css_text().as_str(), b.value("margin").as_str()), ("margin: 1px;", "1px"));
        let b = InlineStyle::parse_attribute("--x: 1px; color: red");
        assert_eq!(b.css_text(), "--x: 1px; color: red;");
    }

    #[test]
    fn a_setter_replaces_in_place_and_priority_reads_back() {
        let mut b = InlineStyle::parse_attribute("");
        assert!(b.set("margin", "1px 2px", false));
        assert!(b.set("margin-top", "3px", false));
        assert_eq!(b.css_text(), "margin: 3px 2px 1px;");
        assert!(b.set("padding", "4px", true));
        assert_eq!((b.priority("padding"), b.priority("padding-top"), b.priority("margin")), ("important", "important", ""));
        assert_eq!(b.remove("margin"), "3px 2px 1px");
        assert_eq!(b.css_text(), "padding: 4px !important;");
    }

    /// A setter says whether it changed the block, on the direct path and through the parse alike.
    #[test]
    fn set_reports_unchanged() {
        let mut b = InlineStyle::parse_attribute("width: 1px; margin: 0px; color: red !important");
        assert_eq!(b.set_outcome("width", "1px", false), SetOutcome::Unchanged, "the same longhand value");
        assert_eq!(b.set_outcome("width", "2px", false), SetOutcome::Changed);
        assert_eq!(b.set_outcome("height", "2px", false), SetOutcome::Changed, "appended");
        assert_eq!(b.set_outcome("height", "2px", false), SetOutcome::Unchanged);
        assert_eq!(b.set_outcome("margin", "0px", false), SetOutcome::Unchanged, "a shorthand over identical longhands");
        assert_eq!(b.set_outcome("margin", "0px 1px", false), SetOutcome::Changed);
        assert_eq!(b.set_outcome("width", "2px", true), SetOutcome::Changed, "important over not");
        assert_eq!(b.set_outcome("width", "2px", true), SetOutcome::Unchanged, "through the parse");
        assert_eq!(b.set_outcome("color", "red", true), SetOutcome::Unchanged);
        assert_eq!(b.set_outcome("color", "red", false), SetOutcome::Changed, "not important over important");
        assert_eq!(b.set_outcome("width", "", false), SetOutcome::Rejected);
        assert_eq!(b.set_outcome("--x", "a", false), SetOutcome::Changed);
        assert_eq!(b.set_outcome("--x", "a", false), SetOutcome::Unchanged);
        assert_eq!(b.set_outcome("--x", " a ", false), SetOutcome::Unchanged, "trimmed as it is stored");
        assert!(b.set("width", "3px", false) && !b.set("width", "", false));
    }

    /// The block a write left is read off its cascade text's order note, beside that attribute alone: a text the
    /// page set (the cascade reading the attribute itself) or another attribute reads the parse (Chrome 152,
    /// `c2.html`, `c2rev.html`, 2026-10-23).
    #[test]
    fn a_written_block_is_read_off_its_cascade_texts_order_note() {
        let mut x = InlineStyle::default();
        for side in ["left", "top", "right", "bottom"] {
            x.set(&format!("margin-{side}"), "1px", false);
        }
        let attribute = x.css_text();
        assert_eq!(attribute, "margin: 1px;");
        let mut cascade = "margin-left: 1px; margin-top: 1px; margin-right: 1px; margin-bottom: 1px;".to_string();
        x.note_order(&mut cascade, 1);
        assert_eq!(cascade, "margin-left: 1px; margin-top: 1px; margin-right: 1px; margin-bottom: 1px; /*order margin-left margin-top margin-right margin-bottom*/");
        let names = |b: &InlineStyle| b.names().join(",");
        for _ in 0..2 {
            assert_eq!(InlineStyle::with_written(&cascade, &attribute, names).0, "margin-left,margin-top,margin-right,margin-bottom");
            assert_eq!(names(&InlineStyle::written_block(&cascade, &attribute)), "margin-left,margin-top,margin-right,margin-bottom");
        }
        assert_eq!(InlineStyle::with_attribute(&attribute, names), "margin-top,margin-right,margin-bottom,margin-left", "the parse");
        assert_eq!(InlineStyle::with_written(&attribute, &attribute, names).0, "margin-top,margin-right,margin-bottom,margin-left");
        assert_eq!(InlineStyle::with_written(&cascade, "margin: 2px;", names).0, "margin-left,margin-top,margin-right,margin-bottom", "the order of another attribute's longhands");
        let page = format!("{attribute} {}", &cascade[cascade.find("/*").unwrap()..]);
        assert_eq!(InlineStyle::with_written(&page, &page, names).0, "margin-top,margin-right,margin-bottom,margin-left", "a note the page wrote");
        // `!important` kept where it was set, and a custom property's name escaped in the note
        let mut y = InlineStyle::default();
        y.set("display", "none", true);
        y.set("--x", "1", false);
        y.set("width", "10px", false);
        let (mut cascade, attribute) = (String::from("display: none !important; --x: 1; width: 10px;"), y.css_text());
        y.note_order(&mut cascade, 3);
        assert!(cascade.ends_with("/*order display --x width*/"), "{cascade}");
        assert_eq!(InlineStyle::with_written(&cascade, &attribute, |b| b.css_text()).0, y.css_text());
        assert_ne!(InlineStyle::with_attribute(&attribute, |b| b.css_text()), y.css_text(), "the parse files it last");
        let mut e = String::new();
        escaped_name("--a:b */ c", &mut e);
        assert_eq!(e, "--a%3Ab%20%2A%2F%20c");
        assert!(is_noted_name("--a:b */ c", &e) && !is_noted_name("--a:b", &e));
        let mut one = InlineStyle::default();
        one.set("width", "1px", false);
        let mut text = String::from("width: 1px;");
        one.note_order(&mut text, 1);
        assert_eq!(text, "width: 1px;", "one longhand has one order");
        // nor a block its attribute's parse keeps in order: one declaration per longhand, `!important` last
        one.set("height", "2px", false);
        one.set("color", "red", true);
        let mut text = String::from("width: 1px; height: 2px; color: red !important;");
        one.note_order(&mut text, 3);
        assert_eq!(text, "width: 1px; height: 2px; color: red !important;");
        assert_eq!(InlineStyle::with_attribute(&one.css_text(), |b| b.names().join(",")), "width,height,color");
        one.set("width", "3px", true);
        assert!(!one.parses_as_is(3), "an `!important` before one without");
    }

    /// A block read in place answers what its copy answers -- parsed, kept, or written by a CSSOM write.
    #[test]
    fn a_borrowed_block_is_the_parsed_one() {
        let read = |b: &InlineStyle| {
            (b.css_text(), b.len(), b.name_at(0).map(str::to_string), b.name_at(99).map(str::to_string), b.value("margin"), b.value("MARGIN-top"), b.priority("padding"))
        };
        let names = |b: &InlineStyle| (b.names().len(), b.names().first().cloned(), b.names().get(99).cloned());
        let texts = [
            "display: none; width: 100%; height: 52px; border: 0;", "color: red; color: blue", "color: red !important; width: 0",
            "margin-top: 1px; margin-right: 1px; margin-bottom: 1px; margin-left: 1px", "--x: 1px; color: red", "padding: 1px !important",
            "", "  ",
        ];
        for t in texts {
            for _ in 0..2 {
                let copy = InlineStyle::parse_attribute(t);
                assert_eq!(InlineStyle::with_attribute(t, read), read(&copy), "{t}");
                let (n, first, past) = names(&copy);
                assert_eq!((copy.len(), copy.name_at(0).map(str::to_string), copy.name_at(99).map(str::to_string)), (n, first, past));
            }
        }
        let mut b = InlineStyle::parse_attribute("color: red !important; width: 0; height: 0");
        b.set("z-index", "3", false);
        let t = b.css_text();
        let mut cascade = String::from("color: red !important; width: 0; height: 0; z-index: 3;");
        b.note_order(&mut cascade, 4);
        assert_eq!(InlineStyle::with_written(&cascade, &t, read).0, read(&b), "the block a write left");
    }

    /// `removeProperty`'s answer: a longhand's or a custom property's value, `""` for a shorthand (Chrome 152,
    /// `c3c.html`: `margin` and `border-top` answer `""`, `color` `blue`, `--z` `1`, `border-left-width` `1px`).
    #[test]
    fn a_removal_answers_a_longhands_value_and_nothing_for_a_shorthand() {
        let b = InlineStyle::parse_css_text("margin: 1px 2px; border: 1px solid red; color: blue; --z: 1");
        assert_eq!(b.removal_value("margin"), "");
        assert_eq!(b.removal_value("border-top"), "");
        assert_eq!(b.removal_value("COLOR"), "blue");
        assert_eq!(b.removal_value("--z"), "1");
        assert_eq!(b.removal_value("border-left-width"), "1px");
        assert_eq!(b.removal_value("padding"), "");
        assert_eq!(b.value("margin"), "1px 2px", "a read still answers the shorthand");
    }

    /// A large value is answered and not kept, and the parsed blocks kept stay within their bytes (`style::memo`):
    /// a page writing distinct 100 KB values kept every one until the count bound (followup-issues.json[9]). A block
    /// over [`SHARED_BYTES`] is handed back for its element to keep, a smaller one is kept by its text.
    #[test]
    fn the_memos_are_bounded_by_their_bytes() {
        let before = PARSED.with(|c| c.borrow().len());
        let big = format!("background-image: url(data:image/png;base64,{})", "A".repeat(400_000));
        assert_eq!(InlineStyle::parse_attribute(&big).len(), 1);
        assert_eq!(PARSED.with(|c| c.borrow().len()), before, "too large to keep in the map");
        let (n, own) = InlineStyle::with_written(&big, &big, |b| b.len());
        assert_eq!((n, own.map(|b| b.len())), (1, Some(1)), "handed back to its element");
        let mid = format!("background-image: url(data:image/png;base64,{})", "A".repeat(2_000));
        assert!(InlineStyle::with_written(&mid, &mid, |_| ()).1.is_some(), "a 2 KB attribute is its element's");
        assert!(!PARSED.with(|c| c.borrow().contains_key(&mid)));
        let small = "width: 1px; margin: 2px";
        assert!(InlineStyle::with_written(small, small, |_| ()).1.is_none(), "a small one is shared");
        assert!(PARSED.with(|c| c.borrow().contains_key(small)));
        for i in 0..1200 {
            let text = format!("width: {i}px; --x: {}", "a".repeat(600));
            assert_eq!(InlineStyle::parse_attribute(&text).len(), 2);
        }
        assert!(PARSED_BYTES.with(|b| b.get()) <= crate::style::memo::BYTES);
        assert!(PARSED.with(|c| c.borrow().len()) <= 512);
        let piece = format!("--y: {}", "b".repeat(400_000));
        assert_eq!(entries_of(&piece).len(), 1);
        assert_eq!(entries_of(&piece).len(), 1, "answered again, not kept");
    }

    /// A kept declaration's longhands are the parse's, however often asked.
    #[test]
    fn entries_are_the_same_from_the_memo() {
        let pieces = [
            "border: 0", "margin: 1px 2px", "width: 10px", "--x: a", "color: var(--y)", "corner-shape: squircle",
            "font: 12px serif", "outline: initial", "x: y", "width: 10px !important", "transition: opacity 0.3s ease",
        ];
        for p in pieces {
            let fresh = format!("{:?}", entries_of_uncached(p));
            assert_eq!(format!("{:?}", entries_of(p).to_vec()), fresh, "{p}");
            assert_eq!(format!("{:?}", entries_of(p).to_vec()), fresh, "{p}, again");
        }
        assert_eq!(longhand_names("border"), longhand_names("border"));
        assert_eq!(longhand_names("border").len(), 17);
        for n in ["border", "margin", "width", "-webkit-transform", "WIDTH", "--x", "foo"] {
            assert_eq!(longhand_set(n).to_vec(), longhand_names(n), "{n}");
            assert_eq!(longhand_set(n).to_vec(), longhand_names(n), "{n}, again");
        }
    }

    /// [`InlineStyle::set`]'s direct entry is the one the parse makes, for every name the engine or Blink knows.
    #[test]
    fn a_plain_longhand_is_set_as_the_parse_sets_it() {
        let mut names: Vec<&str> = crate::style::property::ALL.iter().map(|p| p.name()).collect();
        names.extend(crate::style::blink_names::BLINK_CSS_NAMES.iter().map(|(n, _)| *n));
        names.extend(["--x", "--a-b", "--A", "--_1", "Width", "-", "--"]);
        let values = [
            "10px", "0", "-5px", "50%", "1.5", "1e1px", "auto", "none", "red", "RED", "#fff", "rgb(1, 2, 3)", "initial",
            "INHERIT", "calc(1px + 2px)", "calc(1px+2px)", "calc(1px", "translateX(1px)", "scale(1.000998)", "url(a.png)",
            "'a'", "\"a", "match-parent", "1px 2px", "Arial", "serif", "var(--a)", "x:y", "10px ", " 10px", "pointer} color: red",
            "a{b}", "1px)", "a]", "1px/**/", "\u{a0}10px", "10px\t", "",
        ];
        // the lane is taken where it pays, and not by a shorthand, an alias, a table row that expands or a capital
        assert!(["width", "transform", "opacity", "left", "color", "--x"].iter().all(|n| plain_name(n)));
        assert!(!["margin", "-webkit-transform", "background-position", "Width", "--", "--a b"].iter().any(|n| plain_name(n)));
        let starts = [InlineStyle::default(), InlineStyle::parse_attribute("width: 1px; margin: 0; color: red !important")];
        for n in &names {
            for v in values {
                for s in &starts {
                    let (mut fast, mut slow) = (s.clone(), s.clone());
                    let (a, b) = (fast.set_outcome(n, v, false), slow.set_parsed(n, v, false));
                    assert_eq!((a, format!("{:?}", fast.entries)), (b, format!("{:?}", slow.entries)), "{n}: {v:?}");
                }
            }
        }
    }
}
