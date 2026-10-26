//! The two at-rules the CSSOM hands back as *objects*: `@position-try` and `@counter-style`.
//!
//! Every other at-rule this engine meets is skipped whole (see `parse.rs`), which is the right
//! answer for the ones whose contents are ordinary rules — `@supports`, `@layer` — because
//! cssparser's recovery walks their block and the rules inside land in the sheet anyway. It is the
//! wrong answer for these two: their contents are **descriptors**, not properties, and the page
//! reads them back through `cssRules[i]`. A skipped `@position-try` is not a rule the cascade
//! ignores, it is a rule that is *not there* — `sheet.cssRules[0]` is `undefined` and every read
//! off it throws.
//!
//! # Why the block is not a `Span` into the sheet's pools
//!
//! An ordinary style rule keeps its declarations in the sheet's flat vectors, addressed by one
//! contiguous [`crate::style::rules::Span`], because there are tens of thousands of them and the
//! cascade reads them in bulk. An at-rule's block is different on both counts. There is a handful
//! of them, and the CSSOM *writes into one descriptor at a time*
//! (`rule.style.marginTop = "4px"`): appending a longhand to a rule in the middle of the sheet
//! would have to shift the span of every rule after it. So an at-rule owns its block outright.
//!
//! # Validation is the behaviour, not a nicety
//!
//! `o.marginTop = "x"` leaves `o.marginTop` empty in a browser, because `x` is not a
//! `<length-percentage>` and CSSOM drops an assignment it cannot parse. A descriptor block that
//! stored whatever it was handed would answer `"x"` — visibly not a browser. So each descriptor
//! carries its grammar here, and the setter is a parse.

use crate::style::shorthand;
use cssparser::{
    serialize_identifier, serialize_string, AtRuleParser, CowRcStr, DeclarationParser, ParseError,
    Parser, ParserInput, QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, Token,
};

/// A declaration block owned by one at-rule: descriptor name (dashed, lowercased) to its value as
/// the CSSOM serializes it, in insertion order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DescriptorBlock {
    entries: Vec<(String, String)>,
}

impl DescriptorBlock {
    /// The value stored for `name`, or `""` — which is what a browser answers for a descriptor the
    /// rule does not set, and the reason this is not an `Option`.
    pub fn get(&self, name: &str) -> &str {
        self.entries.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    pub fn set(&mut self, name: &str, value: String) {
        match self.entries.iter_mut().find(|(k, _)| k == name) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((name.to_string(), value)),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.entries.retain(|(k, _)| k != name);
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == name)
    }

    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }

    /// The block as `cssText` writes it — `top: 0px; left: auto;`, with the trailing semicolon on
    /// the last declaration too, exactly as a style rule's block is written.
    pub fn css_text(&self) -> String {
        let mut out = String::new();
        for (name, value) in &self.entries {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push(';');
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Reading a descriptor block out of the source
// ---------------------------------------------------------------------------------------------

/// Walk a `{ … }` block as raw `name: value` pairs.
///
/// The cascade's own declaration parser cannot do this: it rejects any name the property table does
/// not know, and `system`, `symbols` and `suffix` are not CSS properties. This one understands
/// nothing and keeps everything; the grammar check happens per descriptor, afterwards.
pub fn read_descriptors(input: &mut Parser) -> Vec<(String, String)> {
    let mut parser = RawDescriptors;
    let mut out = Vec::new();
    let mut body = RuleBodyParser::new(input, &mut parser);
    while let Some(result) = body.next() {
        if let Ok(pair) = result {
            out.push(pair);
        }
    }
    out
}

struct RawDescriptors;

impl<'i> DeclarationParser<'i> for RawDescriptors {
    type Declaration = (String, String);
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<(String, String), ParseError<'i, ()>> {
        input.skip_whitespace();
        let start = input.position();
        while input.next().is_ok() {}
        let value = input.slice(start..input.position()).trim().to_string();
        if value.is_empty() {
            return Err(input.new_custom_error(()));
        }
        Ok((name.to_ascii_lowercase(), value))
    }
}

impl<'i> AtRuleParser<'i> for RawDescriptors {
    type Prelude = ();
    type AtRule = (String, String);
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for RawDescriptors {
    type Prelude = ();
    type QualifiedRule = (String, String);
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, (String, String), ()> for RawDescriptors {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------------------------
// @position-try
// ---------------------------------------------------------------------------------------------

/// What one `@position-try` descriptor accepts. The set of descriptors is closed — CSS Anchor
/// Positioning 1 §4.2 lists exactly the inset, margin, size, self-alignment and anchor properties,
/// and the members the Brave 151 capture puts on `CSSPositionTryDescriptors.prototype` are that
/// list and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Try {
    /// `<length-percentage> | auto` — every inset and every margin.
    Offset,
    /// `auto | <length-percentage> | <size-keyword>` — `width`, `height`, and the `min-` group.
    Size,
    /// The `max-` group: the same, with `none` where the others take `auto`.
    MaxSize,
    /// `<'align-self'>` / `<'justify-self'>`.
    SelfAlign,
    /// `auto | <dashed-ident>`.
    Anchor,
    /// `<position-area>`.
    Area,
}

/// The longhands a `@position-try` shorthand distributes over, in CSS order. `None` means the name
/// is not a shorthand — either a longhand, or not a descriptor at all.
fn try_shorthand(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "margin" => &["margin-top", "margin-right", "margin-bottom", "margin-left"][..],
        "inset" => &["top", "right", "bottom", "left"][..],
        "margin-block" => &["margin-block-start", "margin-block-end"][..],
        "margin-inline" => &["margin-inline-start", "margin-inline-end"][..],
        "inset-block" => &["inset-block-start", "inset-block-end"][..],
        "inset-inline" => &["inset-inline-start", "inset-inline-end"][..],
        "place-self" => &["align-self", "justify-self"][..],
        _ => return None,
    })
}

fn try_longhand(name: &str) -> Option<Try> {
    Some(match name {
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" | "margin-block-start"
        | "margin-block-end" | "margin-inline-start" | "margin-inline-end" | "top" | "right"
        | "bottom" | "left" | "inset-block-start" | "inset-block-end" | "inset-inline-start"
        | "inset-inline-end" => Try::Offset,
        "width" | "height" | "block-size" | "inline-size" | "min-width" | "min-height"
        | "min-block-size" | "min-inline-size" => Try::Size,
        "max-width" | "max-height" | "max-block-size" | "max-inline-size" => Try::MaxSize,
        "align-self" | "justify-self" => Try::SelfAlign,
        "position-anchor" => Try::Anchor,
        "position-area" => Try::Area,
        _ => return None,
    })
}

/// Is `name` a `@position-try` descriptor at all?
pub fn is_position_try_descriptor(name: &str) -> bool {
    try_longhand(name).is_some() || try_shorthand(name).is_some()
}

/// Read one `@position-try` descriptor, as the CSSOM serializes it.
///
/// A shorthand is *rebuilt* from its longhands rather than stored, which is what a browser does:
/// `margin` reads back only when all four sides are set, and collapses to the shortest form that
/// means the same thing.
pub fn position_try_get(block: &DescriptorBlock, name: &str) -> String {
    if let Some(parts) = try_shorthand(name) {
        return serialize_shorthand(block, parts);
    }
    if try_longhand(name).is_some() {
        return block.get(name).to_string();
    }
    String::new()
}

/// Write one `@position-try` descriptor. `false` means the value did not parse, and the CSSOM drops
/// such an assignment on the floor rather than throwing — the descriptor keeps what it had.
///
/// The empty string is not a failure: it is how the CSSOM removes a declaration.
pub fn position_try_set(block: &mut DescriptorBlock, name: &str, value: &str) -> bool {
    let value = value.trim();
    let parts: Vec<&'static str> = match try_shorthand(name) {
        Some(parts) => parts.to_vec(),
        None => match try_longhand(name) {
            Some(_) => vec![longhand_name(name)],
            None => return false,
        },
    };
    if value.is_empty() {
        for part in &parts {
            block.remove(part);
        }
        return true;
    }
    let components = components_of(value);
    let Some(assigned) = distribute(&parts, &components) else { return false };
    // Every component has to fit the longhand it lands on before ANY of them is stored: a shorthand
    // with one bad component is dropped whole, not applied in part.
    for (part, component) in &assigned {
        let Some(kind) = try_longhand(part) else { return false };
        if !accepts_try(kind, component) {
            return false;
        }
    }
    for (part, component) in assigned {
        block.set(part, component);
    }
    true
}

/// The descriptor names, in source order, that a `@position-try` block declares — used to build the
/// rule's block at parse time, where an unknown or unparseable descriptor is simply not there.
pub fn build_position_try_block(pairs: Vec<(String, String)>) -> DescriptorBlock {
    let mut block = DescriptorBlock::default();
    for (name, value) in pairs {
        position_try_set(&mut block, &name, &value);
    }
    block
}

/// A longhand name as a `&'static str`. Every one is already a literal in [`try_longhand`], so this
/// is a lookup rather than a leak; it returns `""` for anything else, which no caller reaches
/// because [`position_try_set`] has already checked the name.
fn longhand_name(name: &str) -> &'static str {
    const ALL: &[&str] = &[
        "margin-top", "margin-right", "margin-bottom", "margin-left", "margin-block-start",
        "margin-block-end", "margin-inline-start", "margin-inline-end", "top", "right", "bottom",
        "left", "inset-block-start", "inset-block-end", "inset-inline-start", "inset-inline-end",
        "width", "height", "block-size", "inline-size", "min-width", "min-height",
        "min-block-size", "min-inline-size", "max-width", "max-height", "max-block-size",
        "max-inline-size", "align-self", "justify-self", "position-anchor", "position-area",
    ];
    ALL.iter().copied().find(|n| *n == name).unwrap_or("")
}

/// Spread `components` over `parts` by the CSS box rule: one value goes everywhere, two split the
/// axes, three name top/horizontal/bottom, four go clockwise. A two-part shorthand takes one or two.
fn distribute(parts: &[&'static str], components: &[String]) -> Option<Vec<(&'static str, String)>> {
    let picks: Vec<usize> = match (parts.len(), components.len()) {
        (4, 1) => vec![0, 0, 0, 0],
        (4, 2) => vec![0, 1, 0, 1],
        (4, 3) => vec![0, 1, 2, 1],
        (4, 4) => vec![0, 1, 2, 3],
        (2, 1) => vec![0, 0],
        (2, 2) => vec![0, 1],
        (1, 1) => vec![0],
        _ => return None,
    };
    Some(parts.iter().zip(picks).map(|(p, i)| (*p, components[i].clone())).collect())
}

/// Rebuild a shorthand from its longhands, collapsed to the shortest equivalent form. Empty when
/// any longhand is unset — a shorthand only serializes when it can say everything the block says.
fn serialize_shorthand(block: &DescriptorBlock, parts: &[&str]) -> String {
    if !parts.iter().all(|p| block.has(p)) {
        return String::new();
    }
    let values: Vec<&str> = parts.iter().map(|p| block.get(p)).collect();
    match values.as_slice() {
        [a, b] => {
            if a == b {
                a.to_string()
            } else {
                format!("{a} {b}")
            }
        }
        [top, right, bottom, left] => {
            if top == right && right == bottom && bottom == left {
                top.to_string()
            } else if top == bottom && right == left {
                format!("{top} {right}")
            } else if right == left {
                format!("{top} {right} {bottom}")
            } else {
                format!("{top} {right} {bottom} {left}")
            }
        }
        _ => String::new(),
    }
}

/// The top-level component values of `value`, as trimmed strings.
fn components_of(value: &str) -> Vec<String> {
    let mut pi = ParserInput::new(value);
    let mut parser = Parser::new(&mut pi);
    shorthand::components(&mut parser)
        .into_iter()
        .map(|s| value[s.start as usize..s.end as usize].trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

const SIZE_KEYWORDS: &[&str] =
    &["min-content", "max-content", "fit-content", "stretch", "-webkit-fill-available"];

fn accepts_try(kind: Try, text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    if is_css_wide(text) {
        return true;
    }
    match kind {
        Try::Offset => ident_is(text, &["auto"]) || shorthand::is_length_percentage(text),
        Try::Size => {
            ident_is(text, &["auto"])
                || ident_is(text, SIZE_KEYWORDS)
                || is_fit_content(text)
                || shorthand::is_length_percentage(text)
        }
        Try::MaxSize => {
            ident_is(text, &["none"])
                || ident_is(text, SIZE_KEYWORDS)
                || is_fit_content(text)
                || shorthand::is_length_percentage(text)
        }
        Try::SelfAlign => is_self_alignment(text),
        Try::Anchor => ident_is(text, &["auto"]) || is_dashed_ident(text),
        Try::Area => is_position_area(text),
    }
}

/// `<'align-self'>` and `<'justify-self'>` differ only in that the latter also takes `left` and
/// `right`; a descriptor block never has to tell them apart, so the union is checked once.
///
/// `anchor-center` is the addition CSS Anchor Positioning makes to both.
fn is_self_alignment(text: &str) -> bool {
    const SELF_POSITION: &[&str] = &[
        "center", "start", "end", "self-start", "self-end", "flex-start", "flex-end", "left",
        "right",
    ];
    let words: Vec<String> =
        text.split_ascii_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    match words.len() {
        1 => {
            matches!(words[0].as_str(), "auto" | "normal" | "stretch" | "anchor-center" | "baseline")
                || SELF_POSITION.contains(&words[0].as_str())
        }
        2 => {
            let (a, b) = (words[0].as_str(), words[1].as_str());
            (matches!(a, "first" | "last") && b == "baseline")
                || (matches!(a, "safe" | "unsafe") && SELF_POSITION.contains(&b))
        }
        _ => false,
    }
}

/// The `<position-area>` keyword grid: one keyword names a region on both axes, two name one each.
/// The grammar constrains which pairs are compatible (you cannot combine two row keywords); this
/// checks the vocabulary and the arity, which is what separates a position area from `x`.
fn is_position_area(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "none", "center", "span-all", "left", "right", "span-left", "span-right", "top", "bottom",
        "span-top", "span-bottom", "start", "end", "span-start", "span-end", "self-start",
        "self-end", "span-self-start", "span-self-end", "x-start", "x-end", "span-x-start",
        "span-x-end", "y-start", "y-end", "span-y-start", "span-y-end", "x-self-start",
        "x-self-end", "span-x-self-start", "span-x-self-end", "y-self-start", "y-self-end",
        "span-y-self-start", "span-y-self-end", "block-start", "block-end", "span-block-start",
        "span-block-end", "inline-start", "inline-end", "span-inline-start", "span-inline-end",
        "self-block-start", "self-block-end", "span-self-block-start", "span-self-block-end",
        "self-inline-start", "self-inline-end", "span-self-inline-start", "span-self-inline-end",
    ];
    let words: Vec<String> =
        text.split_ascii_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    if words.is_empty() || words.len() > 2 {
        return false;
    }
    if words.len() == 2 && words.iter().any(|w| w == "none") {
        return false;
    }
    words.iter().all(|w| KEYWORDS.contains(&w.as_str()))
}

fn is_fit_content(text: &str) -> bool {
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    let opens =
        matches!(parser.next(), Ok(Token::Function(name)) if name.eq_ignore_ascii_case("fit-content"));
    if !opens {
        return false;
    }
    let inner = parser.parse_nested_block(|p| -> Result<String, ParseError<'_, ()>> {
        let start = p.position();
        while p.next().is_ok() {}
        Ok(p.slice(start..p.position()).trim().to_string())
    });
    match inner {
        Ok(arg) => shorthand::is_length_percentage(&arg) && parser.expect_exhausted().is_ok(),
        Err(_) => false,
    }
}

fn is_dashed_ident(text: &str) -> bool {
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    let dashed = matches!(parser.next(), Ok(Token::Ident(name)) if name.starts_with("--"));
    dashed && parser.expect_exhausted().is_ok()
}

fn is_css_wide(text: &str) -> bool {
    ident_is(text, &["inherit", "initial", "unset", "revert", "revert-layer"])
}

fn ident_is(text: &str, set: &[&str]) -> bool {
    let t = text.trim();
    set.iter().any(|k| t.eq_ignore_ascii_case(k))
}

// ---------------------------------------------------------------------------------------------
// @counter-style
// ---------------------------------------------------------------------------------------------

/// The `@counter-style` descriptors, CSS Counter Styles 3 §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cs {
    /// `cyclic | numeric | alphabetic | symbolic | additive | fixed <integer>? | extends <name>`
    System,
    /// `<symbol>+`
    Symbols,
    /// `[<integer [0,∞]> && <symbol>]#`
    AdditiveSymbols,
    /// `<symbol> <symbol>?`
    Negative,
    /// `<symbol>`
    Symbol,
    /// `[[<integer> | infinite]{2}]# | auto`
    Range,
    /// `<integer [0,∞]> && <symbol>`
    Pad,
    /// `auto | bullets | numbers | words | spell-out | <counter-style-name>`
    SpeakAs,
    /// `<counter-style-name>`
    Name,
}

fn counter_style_descriptor(name: &str) -> Option<Cs> {
    Some(match name {
        "system" => Cs::System,
        "symbols" => Cs::Symbols,
        "additive-symbols" => Cs::AdditiveSymbols,
        "negative" => Cs::Negative,
        "prefix" | "suffix" => Cs::Symbol,
        "range" => Cs::Range,
        "pad" => Cs::Pad,
        "speak-as" => Cs::SpeakAs,
        "fallback" => Cs::Name,
        _ => return None,
    })
}

pub fn is_counter_style_descriptor(name: &str) -> bool {
    counter_style_descriptor(name).is_some()
}

pub fn counter_style_get(block: &DescriptorBlock, name: &str) -> String {
    block.get(name).to_string()
}

/// Write one `@counter-style` descriptor. The stored value is the CSSOM's **serialization**, not
/// the author's text: `suffix: '.'` reads back as `"."`, because a CSS string serializes in double
/// quotes whatever quotes it was written with.
pub fn counter_style_set(block: &mut DescriptorBlock, name: &str, value: &str) -> bool {
    let Some(kind) = counter_style_descriptor(name) else { return false };
    let value = value.trim();
    if value.is_empty() {
        block.remove(name);
        return true;
    }
    match serialize_counter_style(kind, value) {
        Some(text) => {
            block.set(name, text);
            true
        }
        None => false,
    }
}

pub fn build_counter_style_block(pairs: Vec<(String, String)>) -> DescriptorBlock {
    let mut block = DescriptorBlock::default();
    for (name, value) in pairs {
        counter_style_set(&mut block, &name, &value);
    }
    block
}

/// Parse a `@counter-style` descriptor and give back how the CSSOM writes it, or `None` when it
/// does not fit the grammar — in which case the assignment is dropped and the old value stands.
fn serialize_counter_style(kind: Cs, text: &str) -> Option<String> {
    let words = components_of(text);
    match kind {
        Cs::System => {
            let first = words.first()?.to_ascii_lowercase();
            match (words.len(), first.as_str()) {
                (1, "cyclic" | "numeric" | "alphabetic" | "symbolic" | "additive" | "fixed") => {
                    Some(first)
                }
                (2, "fixed") => Some(format!("fixed {}", parse_integer(&words[1])?)),
                (2, "extends") => Some(format!("extends {}", counter_style_name(&words[1])?)),
                _ => None,
            }
        }
        Cs::Symbols => {
            let out: Option<Vec<String>> = words.iter().map(|w| symbol(w)).collect();
            let out = out?;
            (!out.is_empty()).then(|| out.join(" "))
        }
        Cs::AdditiveSymbols => {
            // `#` is a comma-separated list, and `&&` inside each item means the two may come in
            // either order. Both facts matter: `3 x, 1 y` and `x 3` are equally valid.
            let mut items = Vec::new();
            for item in text.split(',') {
                let parts = components_of(item);
                if parts.len() != 2 {
                    return None;
                }
                let (n, s) = match (parse_integer(&parts[0]), symbol(&parts[1])) {
                    (Some(n), Some(s)) => (n, s),
                    _ => match (parse_integer(&parts[1]), symbol(&parts[0])) {
                        (Some(n), Some(s)) => (n, s),
                        _ => return None,
                    },
                };
                if n < 0 {
                    return None;
                }
                items.push(format!("{n} {s}"));
            }
            (!items.is_empty()).then(|| items.join(", "))
        }
        Cs::Negative => match words.len() {
            1 => symbol(&words[0]),
            2 => Some(format!("{} {}", symbol(&words[0])?, symbol(&words[1])?)),
            _ => None,
        },
        Cs::Symbol => (words.len() == 1).then(|| symbol(&words[0])).flatten(),
        Cs::Range => {
            if words.len() == 1 && words[0].eq_ignore_ascii_case("auto") {
                return Some("auto".to_string());
            }
            let mut items = Vec::new();
            for item in text.split(',') {
                let parts = components_of(item);
                if parts.len() != 2 {
                    return None;
                }
                let bound = |w: &String| -> Option<String> {
                    if w.eq_ignore_ascii_case("infinite") {
                        Some("infinite".to_string())
                    } else {
                        parse_integer(w).map(|n| n.to_string())
                    }
                };
                items.push(format!("{} {}", bound(&parts[0])?, bound(&parts[1])?));
            }
            (!items.is_empty()).then(|| items.join(", "))
        }
        Cs::Pad => {
            if words.len() != 2 {
                return None;
            }
            let (n, s) = match (parse_integer(&words[0]), symbol(&words[1])) {
                (Some(n), Some(s)) => (n, s),
                _ => match (parse_integer(&words[1]), symbol(&words[0])) {
                    (Some(n), Some(s)) => (n, s),
                    _ => return None,
                },
            };
            (n >= 0).then(|| format!("{n} {s}"))
        }
        Cs::SpeakAs => {
            if words.len() != 1 {
                return None;
            }
            let lower = words[0].to_ascii_lowercase();
            if matches!(lower.as_str(), "auto" | "bullets" | "numbers" | "words" | "spell-out") {
                return Some(lower);
            }
            counter_style_name(&words[0])
        }
        Cs::Name => (words.len() == 1).then(|| counter_style_name(&words[0])).flatten(),
    }
}

/// `<symbol> = <string> | <image> | <custom-ident>`, serialized the way the CSSOM writes it back:
/// a string in double quotes however it was written, an identifier with whatever escaping it needs.
///
/// `<image>` is covered only as far as `url()` — a gradient is a valid symbol nobody writes, and
/// echoing one back unnormalized would be worse than not accepting it.
fn symbol(text: &str) -> Option<String> {
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    let token = parser.next().ok()?.clone();
    if parser.expect_exhausted().is_err() {
        return None;
    }
    match token {
        Token::QuotedString(s) => {
            let mut out = String::new();
            serialize_string(&s, &mut out).ok()?;
            Some(out)
        }
        Token::Ident(i) => {
            if is_css_wide(&i) || i.eq_ignore_ascii_case("default") {
                return None;
            }
            let mut out = String::new();
            serialize_identifier(&i, &mut out).ok()?;
            Some(out)
        }
        Token::UnquotedUrl(u) => {
            let mut inner = String::new();
            serialize_string(&u, &mut inner).ok()?;
            Some(format!("url({inner})"))
        }
        _ => None,
    }
}

/// `<counter-style-name>` is a `<custom-ident>` that may not be `none` — nor, as `<custom-ident>`
/// already says, a CSS-wide keyword.
fn counter_style_name(text: &str) -> Option<String> {
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    let token = parser.next().ok()?.clone();
    if parser.expect_exhausted().is_err() {
        return None;
    }
    let Token::Ident(name) = token else { return None };
    if is_css_wide(&name) || name.eq_ignore_ascii_case("none") {
        return None;
    }
    let mut out = String::new();
    serialize_identifier(&name, &mut out).ok()?;
    Some(out)
}

fn parse_integer(text: &str) -> Option<i32> {
    let mut pi = ParserInput::new(text);
    let mut parser = Parser::new(&mut pi);
    let token = parser.next().ok()?.clone();
    if parser.expect_exhausted().is_err() {
        return None;
    }
    match token {
        Token::Number { int_value: Some(n), .. } => Some(n),
        _ => None,
    }
}

/// A `@counter-style` rule's own name, which is a `<counter-style-name>` and is settable.
pub fn counter_style_rule_name(text: &str) -> Option<String> {
    counter_style_name(text.trim())
}

// ---------------------------------------------------------------------------------------------
// @font-face

/// An `@font-face` descriptor as Chrome keeps it, or `None` when its grammar rejects the value -- which drops the
/// declaration, so an earlier one of the same name stands. Kept as written, `cssRules[i].cssText` read back the
/// author's spelling where Chrome reads its own: apple.com's `font-family: 'SF Pro Display'; src: url('/wss/…woff2')
/// format('woff2')` is `font-family: "SF Pro Display"; src: url("/wss/…woff2") format("woff2")` there. Chrome 150,
/// measured (`ffser.html`, `ffser2.html`):
///
/// ```text
/// font-family   Foo   Bar -> "Foo Bar"   "X" -> X           serif / a, b -> dropped
/// src           local(serif) -> local("serif")              url(x)format(woff) -> url("x") format("woff")
///               format(foo), format("embedded-opentype"), bogus -> that source dropped (all of them: the descriptor)
///               tech( color-COLRv1 ) -> tech(color-colrv1)
/// unicode-range u+4??, U+0000-00FF -> U+400-4FF, U+0-FF      U+110000 -> dropped
/// font-feature-settings  "liga" 1, 'kern' on, "smcp" off -> "liga", "kern", "smcp" 0
/// font-weight / -style / -stretch / -display  BOLD, Oblique 10deg, SWAP -> bold, oblique 10deg, swap
/// ```
pub fn font_face_descriptor(name: &str, value: &str) -> Option<String> {
    match name {
        "font-family" => crate::style::font_family::face_family(value),
        "src" => face_sources(value),
        "unicode-range" => face_unicode_ranges(value),
        "font-feature-settings" => face_tag_list(value, false),
        "font-variation-settings" => face_tag_list(value, true),
        "font-style" | "font-weight" | "font-stretch" | "font-display" => Some(lowercased_keywords(value)),
        _ => Some(value.to_string()),
    }
}

/// The `format()`s Chrome loads: a source naming another (`embedded-opentype`, `svg`, an unknown one) is dropped.
const FACE_FORMATS: &[&str] = &[
    "collection", "opentype", "truetype", "woff", "woff2", "opentype-variations", "truetype-variations",
    "woff-variations", "woff2-variations",
];

/// The `tech()`s Chrome supports (Blink's `FontFormatCheck::IsSupportedFontTechnology`).
const FACE_TECHS: &[&str] = &[
    "features-opentype", "features-aat", "color-colrv0", "color-colrv1", "color-cbdt", "color-sbix", "variations",
    "palettes", "incremental",
];

/// `src`: its sources Chrome takes, each written its way; `None` when it takes none.
fn face_sources(text: &str) -> Option<String> {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let mut out: Vec<String> = Vec::new();
    loop {
        if let Ok(Some(source)) = p.parse_until_before(cssparser::Delimiter::Comma, |p| Ok::<_, ParseError<()>>(face_source(p))) {
            out.push(source);
        }
        if p.next().is_err() {
            break;
        }
    }
    (!out.is_empty()).then(|| out.join(", "))
}

/// One source: `url() [format()] [tech()]` or `local(<family-name>)`.
fn face_source(p: &mut Parser) -> Option<String> {
    let quoted = |s: &str| {
        let mut out = String::new();
        let _ = serialize_string(s, &mut out);
        out
    };
    let url = match p.next().ok()?.clone() {
        Token::UnquotedUrl(u) => u.to_string(),
        Token::Function(f) if f.eq_ignore_ascii_case("url") => {
            p.parse_nested_block(|p| p.expect_string_cloned().map(|s| s.to_string()).map_err(Into::<ParseError<()>>::into)).ok()?
        }
        Token::Function(f) if f.eq_ignore_ascii_case("local") => {
            let name = p
                .parse_nested_block(|p| -> Result<String, ParseError<()>> {
                    if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
                        return Ok(s.to_string());
                    }
                    let mut words = vec![p.expect_ident_cloned()?.to_string()];
                    while let Ok(w) = p.try_parse(|p| p.expect_ident_cloned()) {
                        words.push(w.to_string());
                    }
                    Ok(words.join(" "))
                })
                .ok()?;
            p.expect_exhausted().ok()?;
            return Some(format!("local({})", quoted(&name)));
        }
        _ => return None,
    };
    let mut out = format!("url({})", quoted(&url));
    if p.try_parse(|p| p.expect_function_matching("format")).is_ok() {
        let format = p
            .parse_nested_block(|p| -> Result<String, ParseError<()>> {
                let format = match p.next()?.clone() {
                    Token::QuotedString(s) => s.to_string(),
                    Token::Ident(i) => i.to_ascii_lowercase(),
                    _ => return Err(p.new_custom_error(())),
                };
                p.expect_exhausted()?;
                Ok(format)
            })
            .ok()?;
        if !FACE_FORMATS.iter().any(|f| f.eq_ignore_ascii_case(&format)) {
            return None;
        }
        out.push_str(&format!(" format({})", quoted(&format)));
    }
    if p.try_parse(|p| p.expect_function_matching("tech")).is_ok() {
        let techs = p
            .parse_nested_block(|p| p.parse_comma_separated(|p| Ok::<_, ParseError<()>>(p.expect_ident()?.to_ascii_lowercase())))
            .ok()?;
        if !techs.iter().all(|t| FACE_TECHS.contains(&t.as_str())) {
            return None;
        }
        out.push_str(&format!(" tech({})", techs.join(", ")));
    }
    p.expect_exhausted().ok()?;
    Some(out)
}

/// `unicode-range`: each range UPPERCASE, with no leading zeros and its wildcards spelled out.
fn face_unicode_ranges(text: &str) -> Option<String> {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let ranges = p
        .parse_comma_separated(|p| cssparser::UnicodeRange::parse(p).map_err(Into::<ParseError<()>>::into))
        .ok()?;
    let one = |r: &cssparser::UnicodeRange| {
        if r.start == r.end {
            format!("U+{:X}", r.start)
        } else {
            format!("U+{:X}-{:X}", r.start, r.end)
        }
    };
    Some(ranges.iter().map(one).collect::<Vec<_>>().join(", "))
}

/// `font-feature-settings` (`<string> [<integer> | on | off]?`, a value of 1 left out) or, `variations`,
/// `font-variation-settings` (`<string> <number>`): `normal`, or each tag in double quotes.
fn face_tag_list(text: &str, variations: bool) -> Option<String> {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
        p.expect_exhausted().ok()?;
        return Some("normal".to_string());
    }
    let tags = p
        .parse_comma_separated(|p| -> Result<String, ParseError<()>> {
            let tag = p.expect_string_cloned()?.to_string();
            if tag.len() != 4 || !tag.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                return Err(p.new_custom_error(()));
            }
            let mut out = String::new();
            let _ = serialize_string(&tag, &mut out);
            if variations {
                let v = p.expect_number()?;
                out.push_str(&format!(" {v}"));
                return Ok(out);
            }
            let value = match p.try_parse(|p| p.next().cloned()) {
                Err(_) => 1,
                Ok(Token::Number { int_value: Some(n), .. }) if n >= 0 => n,
                Ok(Token::Ident(w)) if w.eq_ignore_ascii_case("on") => 1,
                Ok(Token::Ident(w)) if w.eq_ignore_ascii_case("off") => 0,
                Ok(_) => return Err(p.new_custom_error(())),
            };
            if value != 1 {
                out.push_str(&format!(" {value}"));
            }
            Ok(out)
        })
        .ok()?;
    Some(tags.join(", "))
}

/// A descriptor of keywords and numbers (`font-weight`, `font-style`, `font-stretch`, `font-display`): its keywords
/// and units lowercase, one space between its parts. Anything else is left as written.
fn lowercased_keywords(text: &str) -> String {
    let mut pi = ParserInput::new(text);
    let mut p = Parser::new(&mut pi);
    let mut parts: Vec<String> = Vec::new();
    while let Ok(t) = p.next() {
        parts.push(match t {
            Token::Ident(i) => i.to_ascii_lowercase(),
            Token::Dimension { value, unit, .. } => format!("{value}{}", unit.to_ascii_lowercase()),
            Token::Number { .. } | Token::Percentage { .. } => cssparser::ToCss::to_css_string(t),
            _ => return text.to_string(),
        });
    }
    parts.join(" ")
}

/// A `@position-try` rule's name, which is a `<dashed-ident>` and is NOT settable.
pub fn position_try_rule_name(text: &str) -> Option<String> {
    let text = text.trim();
    is_dashed_ident(text).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_try_descriptor_keeps_a_value_its_grammar_accepts() {
        let mut b = DescriptorBlock::default();
        assert!(position_try_set(&mut b, "top", "0px"));
        assert_eq!(position_try_get(&b, "top"), "0px");
    }

    // The whole point of the grammar: an assignment a browser drops must be dropped here too, or
    // `o.marginTop = "x"` reads back "x" where every browser answers "".
    #[test]
    fn a_value_the_grammar_rejects_leaves_the_descriptor_alone() {
        let mut b = DescriptorBlock::default();
        assert!(position_try_set(&mut b, "top", "0px"));
        assert!(!position_try_set(&mut b, "top", "x"));
        assert_eq!(position_try_get(&b, "top"), "0px");
        assert!(!position_try_set(&mut b, "margin-top", "x"));
        assert_eq!(position_try_get(&b, "margin-top"), "");
    }

    #[test]
    fn a_shorthand_spreads_over_its_longhands_and_reads_back_collapsed() {
        let mut b = DescriptorBlock::default();
        assert!(position_try_set(&mut b, "margin", "1px 2px"));
        assert_eq!(position_try_get(&b, "margin-top"), "1px");
        assert_eq!(position_try_get(&b, "margin-right"), "2px");
        assert_eq!(position_try_get(&b, "margin-bottom"), "1px");
        assert_eq!(position_try_get(&b, "margin-left"), "2px");
        assert_eq!(position_try_get(&b, "margin"), "1px 2px");
    }

    #[test]
    fn a_shorthand_reads_empty_until_every_longhand_is_set() {
        let mut b = DescriptorBlock::default();
        assert!(position_try_set(&mut b, "margin-top", "1px"));
        assert_eq!(position_try_get(&b, "margin"), "");
    }

    #[test]
    fn one_bad_component_drops_the_whole_shorthand() {
        let mut b = DescriptorBlock::default();
        assert!(!position_try_set(&mut b, "margin", "1px x"));
        assert_eq!(position_try_get(&b, "margin-top"), "");
    }

    #[test]
    fn a_name_that_is_not_a_position_try_descriptor_is_refused() {
        let mut b = DescriptorBlock::default();
        assert!(!position_try_set(&mut b, "color", "red"));
    }

    #[test]
    fn position_anchor_takes_a_dashed_ident_and_nothing_else() {
        let mut b = DescriptorBlock::default();
        assert!(position_try_set(&mut b, "position-anchor", "--a"));
        assert!(!position_try_set(&mut b, "position-anchor", "a"));
        assert_eq!(position_try_get(&b, "position-anchor"), "--a");
    }

    // The capture's own answers, in the smallest form that states them.
    #[test]
    fn a_counter_style_string_descriptor_serializes_in_double_quotes() {
        let mut b = DescriptorBlock::default();
        assert!(counter_style_set(&mut b, "suffix", "'.'"));
        assert_eq!(counter_style_get(&b, "suffix"), "\".\"");
    }

    #[test]
    fn counter_style_grammars_split_the_valid_from_the_invalid() {
        let mut b = DescriptorBlock::default();
        assert!(counter_style_set(&mut b, "system", "cyclic"));
        assert!(!counter_style_set(&mut b, "system", "x"), "`x` is not a counter system");
        assert_eq!(counter_style_get(&b, "system"), "cyclic");
        assert!(counter_style_set(&mut b, "symbols", "x"), "`x` is a custom-ident symbol");
        assert!(!counter_style_set(&mut b, "additive-symbols", "x"), "needs an integer too");
        assert!(!counter_style_set(&mut b, "range", "x"));
        assert!(!counter_style_set(&mut b, "pad", "x"));
        assert!(counter_style_set(&mut b, "speak-as", "x"), "`x` is a counter-style-name");
        assert!(counter_style_set(&mut b, "fallback", "x"));
        assert!(counter_style_set(&mut b, "negative", "x"));
        assert!(counter_style_set(&mut b, "prefix", "x"));
    }

    #[test]
    fn a_block_writes_its_declarations_with_a_trailing_semicolon() {
        let mut b = DescriptorBlock::default();
        position_try_set(&mut b, "top", "0px");
        position_try_set(&mut b, "left", "auto");
        assert_eq!(b.css_text(), "top: 0px; left: auto;");
    }

    // Chrome 150 (`ffser.html`, `ffser2.html`).
    #[test]
    fn a_font_face_descriptor_reads_as_chrome_writes_it() {
        let d = |n: &str, v: &str| font_face_descriptor(n, v);
        assert_eq!(d("font-family", "'SF Pro Display'").as_deref(), Some("\"SF Pro Display\""));
        assert_eq!(d("font-family", "Foo   Bar").as_deref(), Some("\"Foo Bar\""));
        assert_eq!(d("font-family", "\"X\"").as_deref(), Some("X"));
        assert_eq!(d("font-family", "serif"), None);
        assert_eq!(d("font-family", "a, b"), None);
        assert_eq!(
            d("src", "url('/a.woff2') format('woff2'), url(b.woff) format(\"woff\"), local('Arial Bold'), local(Foo   Bar)").as_deref(),
            Some("url(\"/a.woff2\") format(\"woff2\"), url(\"b.woff\") format(\"woff\"), local(\"Arial Bold\"), local(\"Foo Bar\")")
        );
        assert_eq!(
            d("src", "local(serif), url(x.woff)  format( \"woff\" ) ,url(y.otf) format(opentype) tech( color-COLRv1 )").as_deref(),
            Some("local(\"serif\"), url(\"x.woff\") format(\"woff\"), url(\"y.otf\") format(\"opentype\") tech(color-colrv1)")
        );
        assert_eq!(d("src", "url(d.woff2)format(\"woff2\")").as_deref(), Some("url(\"d.woff2\") format(\"woff2\")"));
        assert_eq!(d("src", "url(a.woff) format(foo), url(b.woff)").as_deref(), Some("url(\"b.woff\")"));
        assert_eq!(d("src", "bogus, url(b.woff)").as_deref(), Some("url(\"b.woff\")"));
        assert_eq!(d("src", "url(a.woff) format(\"embedded-opentype\")"), None);
        assert_eq!(d("src", "url(a.woff) format(\"woff2\", \"woff\")"), None);
        assert_eq!(d("src", "url()").as_deref(), Some("url(\"\")"));
        assert_eq!(d("unicode-range", "U+0000-00FF, U+0131").as_deref(), Some("U+0-FF, U+131"));
        assert_eq!(d("unicode-range", "u+4??, U+0-7f").as_deref(), Some("U+400-4FF, U+0-7F"));
        assert_eq!(d("unicode-range", "U+110000"), None);
        assert_eq!(d("font-feature-settings", "\"liga\" 1, 'kern' on, \"smcp\" off, \"dlig\"").as_deref(), Some("\"liga\", \"kern\", \"smcp\" 0, \"dlig\""));
        assert_eq!(d("font-feature-settings", "'liga' 0").as_deref(), Some("\"liga\" 0"));
        assert_eq!(d("font-variation-settings", "'wght' 400").as_deref(), Some("\"wght\" 400"));
        assert_eq!(d("font-weight", "BOLD").as_deref(), Some("bold"));
        assert_eq!(d("font-weight", "400.5 700").as_deref(), Some("400.5 700"));
        assert_eq!(d("font-style", "Oblique 10deg").as_deref(), Some("oblique 10deg"));
        assert_eq!(d("font-display", "SWAP").as_deref(), Some("swap"));
        assert_eq!(d("size-adjust", "90%").as_deref(), Some("90%"));
    }
}
