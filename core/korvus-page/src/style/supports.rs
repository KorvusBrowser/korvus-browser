//! `@supports` conditions (CSS Conditional 3 §6, `selector()` from Conditional 4).
//!
//! A supports condition asks whether THIS ENGINE understands a declaration or a selector, so the
//! answer is a question put to the same parsers the cascade uses -- never a list of what some other
//! browser supports. A property this engine does not know is unsupported; a value its parser rejects
//! is unsupported; a selector `korvus-dom` cannot parse is unsupported.
//!
//! Values are only as validated as the cascade validates them (design D7 keeps most values as text),
//! with the three checks the cascade itself makes: colours parse as colours, lengths as lengths, and
//! `display` against its keyword grammar. Measured on `/usr/bin/google-chrome` 150.0.7871.46:
//!
//! ```text
//! @supports (display: grid)            applies        @supports (display: nope)      does not
//! @supports not (display: nope)        applies        @supports selector(:has(a))    applies
//! ```

use crate::style::calc;
use crate::style::parse::parse_inline_style;
use crate::style::property::{Computed, PropertyId};
use crate::style::shorthand;
use crate::value::Color;
use cssparser::{Parser, ParserInput, Token};
use korvus_dom::Selector;

/// Does this engine support `condition` (the text after `@supports`, or inside `supports()`)?
/// A condition that does not parse is false -- the rule it guards does not apply.
pub fn evaluate(condition: &str) -> bool {
    let mut input = ParserInput::new(condition);
    let mut p = Parser::new(&mut input);
    let r = supports_condition(&mut p);
    matches!(r, Ok(true)) && p.expect_exhausted().is_ok()
}

type Res = Result<bool, ()>;

/// `not <in-parens> | <in-parens> [ and <in-parens> ]* | <in-parens> [ or <in-parens> ]*`
fn supports_condition(p: &mut Parser) -> Res {
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return in_parens(p).map(|b| !b);
    }
    let mut result = in_parens(p)?;
    let mut op: Option<bool> = None; // Some(true) = and, Some(false) = or
    loop {
        let next = p.try_parse(|p| -> Result<bool, cssparser::ParseError<'_, ()>> {
            let t = p.expect_ident()?.clone();
            if t.eq_ignore_ascii_case("and") {
                Ok(true)
            } else if t.eq_ignore_ascii_case("or") {
                Ok(false)
            } else {
                Err(p.new_custom_error(()))
            }
        });
        let Ok(is_and) = next else { break };
        // `a and b or c` mixes operators without parentheses, which the grammar does not allow.
        if op.is_some_and(|o| o != is_and) {
            return Err(());
        }
        op = Some(is_and);
        let v = in_parens(p)?;
        result = if is_and { result && v } else { result || v };
    }
    Ok(result)
}

/// `( <supports-condition> ) | ( <declaration> ) | selector( … ) | <general-enclosed>`
fn in_parens(p: &mut Parser) -> Res {
    let token = p.next().map_err(|_| ())?.clone();
    match token {
        Token::ParenthesisBlock => p
            .parse_nested_block(|p| -> Result<bool, cssparser::ParseError<'_, ()>> {
                if let Ok(v) = p.try_parse(|p| {
                    let v = supports_condition(p).map_err(|_| p.new_custom_error(()))?;
                    p.expect_exhausted().map_err(|_| p.new_custom_error::<(), ()>(()))?;
                    Ok::<bool, cssparser::ParseError<'_, ()>>(v)
                }) {
                    return Ok(v);
                }
                if let Ok(v) = p.try_parse(declaration) {
                    return Ok(v);
                }
                // <general-enclosed>: valid syntax, evaluates to false.
                while p.next().is_ok() {}
                Ok(false)
            })
            .map_err(|_| ()),
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            p.parse_nested_block(|p| -> Result<bool, cssparser::ParseError<'_, ()>> {
                let start = p.position();
                while p.next().is_ok() {}
                let arg = p.slice_from(start).trim().to_string();
                Ok(match name.as_str() {
                    // A complex selector, which the matcher either parses or does not.
                    // ...and that names no `::-webkit-` pseudo-element Blink does not know: such a
                    // selector PARSES (for compatibility) but is not SUPPORTED (Chrome 150:
                    // `selector(::-webkit-foo)` false; `css_selector_parser.cc`
                    // `ContainsUnknownWebkitPseudoElements`).
                    "selector" => Selector::parse_list(&arg).is_ok_and(|l| l.len() == 1) && !unknown_webkit_pseudo_element(&arg),
                    // font-tech() / font-format() and anything unknown: this engine loads no fonts
                    // it could make such a claim about.
                    _ => false,
                })
            })
            .map_err(|_| ())
        }
        _ => Err(()),
    }
}

/// `<ident> : <value>` -- supported when the engine's own parser accepts it.
fn declaration<'i>(p: &mut Parser<'i, '_>) -> Result<bool, cssparser::ParseError<'i, ()>> {
    let name = p.expect_ident()?.to_string();
    p.expect_colon()?;
    let start = p.position();
    while p.next().is_ok() {}
    let value = p.slice_from(start).trim().to_string();
    Ok(supports_declaration(&name, &value))
}

/// Would `name: value` survive this engine's parse as a declaration? THE one answer to that question:
/// `@supports`, `CSS.supports` and `style.setProperty` all ask it here (`CSS.supports`
/// had a second, name-only implementation that said yes to `color: not-a-color` and `width: -5px`).
///
/// Beyond the cascade's own three checks (colours, lengths, `display`), the value is held to Blink's
/// grammar where this file knows it, each measured on Chrome 150 (`tools/realweb/seed/csssupports`):
/// a keyword-only property to Blink's own keyword list (`blink_names::single_keyword`), a shorthand to
/// a successful expansion, `opacity` to a number, `z-index` to an integer, `transform` to a list of
/// transform functions, and the non-negative lengths to a non-negative value.
pub fn supports_declaration(name: &str, value: &str) -> bool {
    let value = value.trim().trim_end_matches("!important").trim();
    if value.is_empty() {
        return false;
    }
    if name.starts_with("--") {
        // any value but one with a reference that is not one (`--b: var(x)`, `--a: env()`: Chrome 150 drops both)
        return !referenced(value)
            || crate::style::variables::references(value) != crate::style::variables::References::Invalid;
    }
    let lower: std::borrow::Cow<str> =
        if name.bytes().any(|c| c.is_ascii_uppercase()) { name.to_ascii_lowercase().into() } else { name.into() };
    // A function of the two strings alone, and asked twice for each `el.style.x = v` (the setter's check, then the
    // block's own): the recent answers are kept. Each ask parses the declaration whole, and the second was 13 % of a
    // style write (perf, 2026-10-22: 4.9 us a write against Chrome 152's 0.35 us). Keyed by the strings' hash and
    // verified against the strings, so a collision only asks again: the 32 answers it kept in a list were scanned
    // and compared one by one on every ask (2026-10-23).
    use std::hash::BuildHasher;
    type Recent = korvus_dom::FxHashMap<u64, (Box<str>, Box<str>, bool)>;
    thread_local! {
        static RECENT: std::cell::RefCell<Recent> = Default::default();
        static RECENT_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let key = RECENT.with(|r| r.borrow().hasher().hash_one((&*lower, value)));
    if let Some(hit) = RECENT.with(|r| r.borrow().get(&key).filter(|(n, v, _)| **n == *lower && &**v == value).map(|(_, _, ok)| *ok)) {
        return hit;
    }
    let ok = supports_declaration_parsed(&lower, value);
    RECENT.with(|r| {
        let mut r = r.borrow_mut();
        // bounded by its bytes too (`style::memo`)
        if RECENT_BYTES.with(|b| crate::style::memo::admit(r.len(), 1024, b, lower.len() + value.len(), || r.clear())) {
            r.insert(key, (lower.into(), value.into(), ok));
        }
    });
    ok
}

/// `CSS.supports(property, value)`, the two-argument form: a VALUE of `name`, which carries no priority -- Blink
/// parses it alone (`CSSParser::ParseValue`), where [`supports_declaration`] reads a declaration's value and strips
/// its `!important`. Chrome 152 answers `CSS.supports('width', '1px !important')` false, and `CSS.supports('(width:
/// 1px !important)')` true; this answered true for both. A `!` at the value's top level is no part of any value -- a
/// custom property's too: `('--x', '1 !important')` and `('--x', 'a!b')` are false, `('--x', '(a!b)')`, `'"!"'`,
/// `'url(a!b)'` and `'a /*!*/ b'` true -- except beside a `var()`, which makes the value Blink's unparsed one, priority
/// and all: `('width', 'var(--y) !important')` is true (`vsup.html`, `f4sup.html`, `f4sup2.html`, 2026-10-23).
pub fn supports_value(name: &str, value: &str) -> bool {
    if top_level_bang(value) && (name.starts_with("--") || !referenced(value)) {
        return false;
    }
    supports_declaration(name, value)
}

/// Does `value` mention a `var()` or an `env()` -- an unparsed value, supported whatever it substitutes to
/// (Chrome 150: `CSS.supports('color', 'env(safe-area-inset-left)')` and `('padding: env(x)')` are true)?
fn referenced(value: &str) -> bool {
    value.contains("var(") || value.contains("env(")
}

/// Does `value` hold a `!` at its top level, as the tokenizer reads it -- outside its strings, `url()`s, comments,
/// escapes and blocks?
fn top_level_bang(value: &str) -> bool {
    if !value.contains('!') {
        return false;
    }
    let mut input = ParserInput::new(value);
    let mut p = Parser::new(&mut input);
    while let Ok(t) = p.next_including_whitespace_and_comments() {
        if matches!(t, Token::Delim('!')) {
            return true;
        }
    }
    false
}

/// [`supports_declaration`] for a name in lowercase and a value trimmed of its priority.
fn supports_declaration_parsed(lower: &str, value: &str) -> bool {
    supports_declaration_parsed_with(lower, value, true)
}

/// [`supports_declaration_parsed`], with the plain-number lane on or (to test it against) off.
fn supports_declaration_parsed_with(lower: &str, value: &str, lane: bool) -> bool {
    let wide = ["initial", "inherit", "unset", "revert", "revert-layer"].iter().any(|k| value.eq_ignore_ascii_case(k));
    let property = PropertyId::from_name(lower);
    if property.is_none() && !shorthand::is_shorthand(lower) {
        // A property Blink exposes and this cascade does not implement: supported when Blink's own
        // grammar takes the value (`corner-shape: squircle` -- Chrome 150 true).
        return crate::style::blink_names::exposed(lower).is_some()
            && (wide || referenced(value) || blink_only_value(lower, value).unwrap_or(false));
    }
    // A plain number, percentage or dimension on a longhand the parser stores as itself is one declaration, every
    // time: one numeric token is no `var()`, no math function, not `match-parent`, no `!`, and a non-expanding
    // property's declaration is pushed as it is -- dropped only for `font-family` (`push_declaration`). The parse
    // that asks it was the cost of every distinct `el.style.width = N + 'px'` (2026-10-23); every check after it
    // still runs.
    let plain = lane
        && property.is_some_and(|p| {
            p != PropertyId::FontFamily && !shorthand::expands(p) && p.name() == lower
        })
        && crate::style::blink_names::exposed(lower).map_or(true, |t| t == lower)
        && plain_number(value);
    if !plain {
        let sheet = parse_inline_style(&format!("{lower}: {value}"));
        if sheet.declarations().is_empty() && sheet.raw_declarations().is_empty() {
            return false;
        }
    }
    if wide || referenced(value) {
        return true;
    }
    // A math function that does not parse or type-check makes the declaration invalid
    // (`style::calc`): Chrome 150 drops `width: round(10.4px)`, `width: calc(1px+2px)` and
    // `transition-duration: calc(1s + 1px)`, which this engine kept.
    if !calc::math_valid(value) {
        return false;
    }
    let Some(property) = property else { return shorthand_expands(lower, value) };
    // `auto`, which Blink's own parsers add to the keyword lists of `css_properties.json5`: `outline-style: auto` and
    // `resize: auto` (Chrome 152 keeps both; `CSS.supports` true).
    if matches!(lower, "outline-style" | "resize") && value.eq_ignore_ascii_case("auto") {
        return true;
    }
    if let Some(words) = crate::style::blink_names::single_keyword(lower) {
        return one_keyword(value, words);
    }
    match lower {
        // Chrome 152's `RubyOverhang::ParseSingleValue`: `auto`, `spaces`, or `none` (its legacy alias).
        "ruby-overhang" => return one_keyword(value, &["auto", "none", "spaces"]),
        // a math function stands for its number or percentage (`ConsumeNumber`/`ConsumePercent`: its category)
        "opacity" | "fill-opacity" | "stroke-opacity" | "flood-opacity" | "stop-opacity" => {
            return number_or_percentage(value) || calc::single_of(value, &[calc::Cat::Number, calc::Cat::Percent])
        }
        "z-index" => {
            return value.eq_ignore_ascii_case("auto")
                || value.parse::<i64>().is_ok()
                || calc::single_of(value, &[calc::Cat::Number])
        }
        "transform" => return transform_list(value),
        // A fill layer's size and its one-axis position, each layer on its own, by the grammars Blink parses them with
        // (Chrome 152, `sup.html` / `vaccept.html`, 2026-10-23): see [`size_layer`] and [`position_layer`].
        "background-size" | "mask-size" | "-webkit-mask-size" => return layers(value, size_layer),
        "background-position-x" | "-webkit-mask-position-x" => return layers(value, |l| position_layer(l, ["left", "right"])),
        "background-position-y" | "-webkit-mask-position-y" => return layers(value, |l| position_layer(l, ["top", "bottom"])),
        // SVG geometry: a bare number is a length here as in the stroke properties below (Chrome 152 keeps `x: 1`,
        // `r: 0.5`, `rx: 100` and reads them `1px`, `0.5px`, `100px`; a negative radius it drops)
        "x" | "y" | "cx" | "cy" => return number_or_length_percentage(value),
        "r" => return !negative_number(value) && number_or_length_percentage(value),
        // The SVG paint and stroke properties, as Blink's parsers take them (Chrome 150, `CSS.supports`): a
        // `<paint>`, a non-negative width, an offset, a dash list, a miter limit -- a bare number is a length there,
        // and so is a math function of numbers (Chrome 152 keeps `stroke-width: calc(2 * 3)`).
        "fill" | "stroke" => return paint(value),
        "stroke-width" => return !value.trim_start().starts_with('-') && number_or_length_percentage(value),
        "stroke-dashoffset" => return number_or_length_percentage(value),
        "stroke-dasharray" => return dash_array(value),
        "stroke-miterlimit" => {
            return value.trim().parse::<f64>().is_ok_and(|n| n.is_finite() && n >= 0.0) || calc::single_of(value, &[calc::Cat::Number])
        }
        // `auto` is the element's `currentcolor` (Chrome 152 keeps `caret-color: auto`)
        "caret-color" => return value.eq_ignore_ascii_case("auto") || Color::parse_str(value).is_ok(),
        "font-size" => {
            const KEYWORDS: &[&str] = &[
                "xx-small", "x-small", "small", "medium", "large", "x-large", "xx-large", "xxx-large", "-webkit-xxx-large",
                "larger", "smaller", "math",
            ];
            return one_keyword(value.trim(), KEYWORDS) || (!value.trim_start().starts_with('-') && shorthand::is_length_percentage(value));
        }
        // a unitless number is a length here, a negative is not (Chrome 152: `rx: 5` kept, `rx: -1` and `-5px` refused)
        "rx" | "ry" => return value.eq_ignore_ascii_case("auto") || (!negative_number(value) && number_or_length_percentage(value)),
        _ => {}
    }
    if NON_NEGATIVE.contains(&lower) && negative_number(value) {
        return false;
    }
    match property.computed_kind() {
        Computed::Color => Color::parse_str(value).is_ok(),
        Computed::Lengthy => shorthand::is_length_percentage(value) || lengthy_value(lower, value),
        Computed::Display => crate::style::resolve::is_display_value(&value.to_ascii_lowercase()),
        _ => true,
    }
}

/// `-?[0-9]{1,9}(\.[0-9]{1,9})?([a-z]{1,5}|%)?`, the whole value: one number, percentage or dimension token.
fn plain_number(value: &str) -> bool {
    let b = value.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let run = |i: &mut usize, f: fn(&u8) -> bool| {
        let start = *i;
        while *i < b.len() && f(&b[*i]) {
            *i += 1;
        }
        *i - start
    };
    if !(1..=9).contains(&run(&mut i, u8::is_ascii_digit)) {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !(1..=9).contains(&run(&mut i, u8::is_ascii_digit)) {
            return false;
        }
    }
    if b.get(i) == Some(&b'%') {
        return i + 1 == b.len();
    }
    run(&mut i, u8::is_ascii_lowercase) <= 5 && i == b.len()
}

/// Does `selector` name a `::-webkit-` pseudo-element outside Blink's known set? The set is
/// `FeatureForWebKitCustomPseudoElement` (reference/chromium `css_selector_parser.cc`) plus the ones
/// Blink types on their own (the scrollbar parts and the resizer); any other `::-webkit-x` is
/// `kCSSSelectorWebkitUnknownPseudo` there and makes `selector()` false.
fn unknown_webkit_pseudo_element(selector: &str) -> bool {
    const KNOWN: &[&str] = &[
        "-webkit-calendar-picker-indicator", "-webkit-clear-button", "-webkit-color-swatch",
        "-webkit-color-swatch-wrapper", "-webkit-date-and-time-value", "-webkit-datetime-edit",
        "-webkit-datetime-edit-ampm-field", "-webkit-datetime-edit-day-field", "-webkit-datetime-edit-fields-wrapper",
        "-webkit-datetime-edit-hour-field", "-webkit-datetime-edit-millisecond-field", "-webkit-datetime-edit-minute-field",
        "-webkit-datetime-edit-month-field", "-webkit-datetime-edit-second-field", "-webkit-datetime-edit-text",
        "-webkit-datetime-edit-week-field", "-webkit-datetime-edit-year-field", "-webkit-file-upload-button",
        "-webkit-inner-spin-button", "-webkit-input-placeholder", "-webkit-media-controls",
        "-webkit-media-controls-current-time-display", "-webkit-media-controls-enclosure",
        "-webkit-media-controls-fullscreen-button", "-webkit-media-controls-mute-button",
        "-webkit-media-controls-overlay-enclosure", "-webkit-media-controls-overlay-play-button",
        "-webkit-media-controls-panel", "-webkit-media-controls-play-button", "-webkit-media-controls-timeline",
        "-webkit-media-controls-timeline-container", "-webkit-media-controls-time-remaining-display",
        "-webkit-media-controls-toggle-closed-captions-button", "-webkit-media-controls-volume-slider",
        "-webkit-media-slider-container", "-webkit-media-slider-thumb", "-webkit-media-text-track-container",
        "-webkit-media-text-track-display", "-webkit-media-text-track-region",
        "-webkit-media-text-track-region-container", "-webkit-meter-bar", "-webkit-meter-even-less-good-value",
        "-webkit-meter-inner-element", "-webkit-meter-optimum-value", "-webkit-meter-suboptimum-value",
        "-webkit-progress-bar", "-webkit-progress-inner-element", "-webkit-progress-value",
        "-webkit-search-cancel-button", "-webkit-slider-container", "-webkit-slider-runnable-track",
        "-webkit-slider-thumb", "-webkit-textfield-decoration-container",
        // Typed on their own in Blink, so never "unknown".
        "-webkit-scrollbar", "-webkit-scrollbar-button", "-webkit-scrollbar-corner", "-webkit-scrollbar-thumb",
        "-webkit-scrollbar-track", "-webkit-scrollbar-track-piece", "-webkit-resizer",
    ];
    let lower = selector.to_ascii_lowercase();
    lower.match_indices("::-webkit-").any(|(at, _)| {
        let name: String = lower[at + 2..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        !KNOWN.contains(&name.as_str())
    })
}

/// The lengths CSS forbids to be negative, among the properties this engine implements: a negative
/// value is a parse error (Chrome 150: `width: -5px` unsupported).
const NON_NEGATIVE: &[&str] = &[
    "width", "height", "min-width", "min-height", "max-width", "max-height", "inline-size", "block-size",
    "min-inline-size", "min-block-size", "max-inline-size", "max-block-size", "padding-top", "padding-right",
    "padding-bottom", "padding-left", "padding-block-start", "padding-block-end", "padding-inline-start",
    "padding-inline-end", "border-top-width", "border-right-width", "border-bottom-width", "border-left-width",
    "outline-width", "column-width", "column-gap", "row-gap", "flex-basis", "font-size", "perspective",
    "border-top-left-radius", "border-top-right-radius", "border-bottom-left-radius", "border-bottom-right-radius",
];

/// Does `value` open with a negative number -- `-5px`, `-.5em`, `-1%`? An identifier that starts with a
/// dash is not one: `width: -webkit-fill-available` is supported (Chrome 150), where
/// a leading `-` alone read as a negative length.
fn negative_number(value: &str) -> bool {
    let mut input = ParserInput::new(value);
    let mut p = Parser::new(&mut input);
    match p.next() {
        Ok(Token::Number { value, .. }) | Ok(Token::Dimension { value, .. }) => *value < 0.0,
        Ok(Token::Percentage { unit_value, .. }) => *unit_value < 0.0,
        _ => false,
    }
}

/// `value` is exactly one of `words`, ASCII case-insensitively.
fn one_keyword(value: &str, words: &[&str]) -> bool {
    !value.contains(char::is_whitespace) && words.iter().any(|w| w.eq_ignore_ascii_case(value))
}

/// `<number> | <length-percentage>`, a math function of either included.
fn number_or_length_percentage(value: &str) -> bool {
    value.trim().parse::<f64>().is_ok_and(f64::is_finite)
        || shorthand::is_length_percentage(value)
        || calc::single_of(value.trim(), &[calc::Cat::Number])
}

/// `<paint>`: `none | <color> | <url> [none | <color>]? | context-fill | context-stroke` (CSS Fill and Stroke 3).
fn paint(value: &str) -> bool {
    let v = value.trim();
    if one_keyword(v, &["none", "context-fill", "context-stroke"]) {
        return true;
    }
    // the prefix compared as bytes: `v[..4]` of `"éé"` cuts the `é`, and `style.fill = '"éé"'` aborted the
    // process from the setter (exit 134, 2026-10-23)
    if v.len() > 4 && v.as_bytes()[..4].eq_ignore_ascii_case(b"url(") {
        let Some(close) = v.find(')') else { return false };
        let rest = v[close + 1..].trim();
        return rest.is_empty() || rest.eq_ignore_ascii_case("none") || Color::parse_str(rest).is_ok();
    }
    Color::parse_str(v).is_ok()
}

/// `stroke-dasharray`: `none`, or a comma- or space-separated list of non-negative lengths, percentages and numbers.
fn dash_array(value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("none") {
        return true;
    }
    let item = |t: &str| !t.starts_with('-') && number_or_length_percentage(t);
    // cut at the top level: `min(10px, 5em)` is one item (Chrome 152 keeps it; its comma cut it in two here)
    let parts = top_level(v, |c| c == ',');
    !parts.is_empty() && parts.iter().all(|p| !p.trim().is_empty() && top_level(p, char::is_whitespace).iter().filter(|w| !w.is_empty()).all(|w| item(w)))
}

/// `value` cut where `at` matches outside parentheses.
fn top_level(value: &str, at: impl Fn(char) -> bool) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, 0usize, Vec::new());
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if depth == 0 && at(c) => {
                out.push(&value[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&value[start..]);
    out
}

/// A comma-separated list, every item non-empty and `item`.
fn layers(value: &str, item: impl Fn(&str) -> bool) -> bool {
    top_level(value, |c| c == ',').iter().all(|l| !l.trim().is_empty() && item(l.trim()))
}

/// One layer's `<bg-size>`: `[ <length-percentage [0,inf]> | auto ]{1,2} | cover | contain`. Chrome 152 keeps `cover`,
/// `auto 2em` and `10px 20px` and drops `cover auto`, `contain contain`, `10px cover`, `-1px` and `10px -5px` (a math
/// function it keeps, clamping it when it computes).
fn size_layer(layer: &str) -> bool {
    let words: Vec<&str> = top_level(layer, char::is_whitespace).into_iter().filter(|w| !w.is_empty()).collect();
    if let [w] = words.as_slice() {
        if w.eq_ignore_ascii_case("cover") || w.eq_ignore_ascii_case("contain") {
            return true;
        }
    }
    (1..=2).contains(&words.len())
        && words.iter().all(|w| {
            w.eq_ignore_ascii_case("auto") || (!w.starts_with('-') && shorthand::is_length_percentage(w)) || unchecked_function(w)
        })
}

/// A function in a `<length-percentage>`'s place that the grammar does not check, as [`lengthy_value`] takes one:
/// a substitution (Chrome 152 keeps `background-size: attr(title)` and `-webkit-mask-position-x: attr(title)`,
/// the getComputedStyle sweep, 2026-10-23) or any other function that is not a math one.
fn unchecked_function(w: &str) -> bool {
    w.find('(').filter(|&o| o > 0 && w.ends_with(')')).is_some_and(|o| {
        let name = &w[..o];
        !calc::is_math_function(name) && !name.eq_ignore_ascii_case("fit-content")
    })
}

/// One layer of `background-position-x` or `-webkit-mask-position-x` (`-y` with `top` and `bottom`): `center`, a side,
/// a `<length-percentage>`, or a side and its offset. Chrome 152 keeps `right 20%`, `left`, `10px` and `right -5px`, and
/// drops `center 10px`, `x-start`, `x-end 5px`, `10px 20px`, `10px left`, `left right` and `top` (`vaccept.html`).
fn position_layer(layer: &str, sides: [&str; 2]) -> bool {
    let words: Vec<&str> = top_level(layer, char::is_whitespace).into_iter().filter(|w| !w.is_empty()).collect();
    let side = |w: &str| sides.iter().any(|s| s.eq_ignore_ascii_case(w));
    let offset = |w: &str| shorthand::is_length_percentage(w) || unchecked_function(w);
    match words.as_slice() {
        [w] => w.eq_ignore_ascii_case("center") || side(w) || offset(w),
        [s, o] => (side(s) || unchecked_function(s)) && offset(o),
        _ => false,
    }
}

/// `<number> | <percentage>`.
fn number_or_percentage(value: &str) -> bool {
    let v = value.strip_suffix('%').unwrap_or(value);
    !v.is_empty() && v.parse::<f64>().is_ok_and(f64::is_finite)
}

/// `none | <transform-function>+`, the functions by name (CSS Transforms 1 and 2).
fn transform_list(value: &str) -> bool {
    const FUNCTIONS: &[&str] = &[
        "matrix", "matrix3d", "translate", "translatex", "translatey", "translatez", "translate3d", "scale",
        "scalex", "scaley", "scalez", "scale3d", "rotate", "rotatex", "rotatey", "rotatez", "rotate3d", "skew",
        "skewx", "skewy", "perspective",
    ];
    if value.eq_ignore_ascii_case("none") {
        return true;
    }
    let mut input = ParserInput::new(value);
    let mut p = Parser::new(&mut input);
    let mut any = false;
    loop {
        match p.next() {
            Ok(Token::Function(name)) => {
                if !FUNCTIONS.contains(&name.to_ascii_lowercase().as_str()) {
                    return false;
                }
                if p.parse_nested_block(|p| -> Result<(), cssparser::ParseError<'_, ()>> {
                    while p.next().is_ok() {}
                    Ok(())
                })
                .is_err()
                {
                    return false;
                }
                any = true;
            }
            Ok(_) => return false,
            Err(_) => return any,
        }
    }
}

/// A shorthand's value is supported when the shorthand expands it. A shorthand `expand` does not
/// handle itself (`animation`, `transition`: their layers are built in `push_expansion`) keeps the
/// cascade's answer.
fn shorthand_expands(name: &str, value: &str) -> bool {
    use crate::style::rules::{Origin, Span, Stylesheet};
    let sheet = Stylesheet::new(Origin::Author, value.to_string());
    match shorthand::expand(name, Span::of(0..value.len()), &sheet) {
        Some(expanded) => expanded.is_some(),
        None => true,
    }
}

/// The grammar of a Blink-exposed property this cascade does not implement, when this file knows it:
/// a single-keyword property's Blink list, and `corner-shape` (CSS Borders 4: one to four of Blink's
/// `corner-shape` keywords -- `css_properties.json5` -- or `superellipse(<number> | infinity |
/// -infinity)`). `None` for any other.
/// A length-valued property's value beyond a lone `<length-percentage>`: Blink's grammar for it --
/// the property's own keywords (`auto`, `none`, the sizing keywords, `thin`/`medium`/`thick`, ...), a
/// math or sizing function, and as many components as the property takes. Any other identifier is
/// not a value of it: Chrome 150 answers `CSS.supports("width", "red")` false
/// (measured; this answered true for every identifier).
fn lengthy_value(name: &str, value: &str) -> bool {
    // A view timeline's list: each item on its own (Chrome 152 keeps `view-timeline-inset: 10px, auto`).
    let layers = top_level(value, |c| c == ',');
    if layers.len() > 1 && name == "view-timeline-inset" {
        return layers.iter().all(|item| {
            let item = item.trim();
            !item.is_empty() && (shorthand::is_length_percentage(item) || lengthy_value(name, item))
        });
    }
    const SIZING: &[&str] = &[
        "min-content", "max-content", "fit-content", "-webkit-min-content", "-webkit-max-content", "-webkit-fit-content",
        "-webkit-fill-available", "stretch",
    ];
    let (words, max): (Vec<&str>, usize) = match name {
        "width" | "height" | "min-width" | "min-height" => ([&["auto"][..], SIZING].concat(), 1),
        "max-width" | "max-height" => ([&["none"][..], SIZING].concat(), 1),
        "flex-basis" => ([&["auto", "content"][..], SIZING].concat(), 1),
        "top" | "left" | "right" | "bottom" | "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => (vec!["auto"], 1),
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" | "outline-width" => (vec!["thin", "medium", "thick"], 1),
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-left-radius" | "border-bottom-right-radius" | "border-spacing" => (vec![], 2),
        "column-gap" | "row-gap" | "letter-spacing" | "word-spacing" => (vec!["normal"], 1),
        "object-position" => (vec!["left", "right", "center", "top", "bottom"], 4),
        "view-timeline-inset" => (vec!["auto"], 2),
        "-webkit-text-stroke-width" => (vec!["thin", "medium", "thick"], 1),
        "text-decoration-thickness" => (vec!["auto", "from-font"], 1),
        "text-indent" => (vec!["each-line", "hanging"], 3),
        "vertical-align" => (vec!["baseline", "sub", "super", "text-top", "text-bottom", "middle", "top", "bottom"], 1),
        _ => (vec![], 1),
    };
    let mut input = ParserInput::new(value);
    let mut p = Parser::new(&mut input);
    let mut n = 0;
    loop {
        p.skip_whitespace();
        let start = p.position();
        let tok = match p.next() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        n += 1;
        let ok = match &tok {
            Token::Ident(w) => words.iter().any(|k| k.eq_ignore_ascii_case(w)),
            // `fit-content(<length-percentage>)` is a grid track size, not a box size: Blink drops
            // `width: fit-content(50px)` (Chrome 150)
            Token::Function(f) if f.eq_ignore_ascii_case("fit-content") => false,
            Token::Function(f) => {
                let math = calc::is_math_function(f);
                let _ = p.parse_nested_block(|inner| -> Result<(), cssparser::ParseError<'_, ()>> {
                    while inner.next().is_ok() {}
                    Ok(())
                });
                // a math function stands for a length or a percentage only when its type is one:
                // Chrome 150 drops `width: calc(1px * 1px)` and `width: sin(1)`
                let text = p.slice_from(start);
                !math || calc::has_foreign_function(text) || calc::single_of(text, calc::LENGTH_PERCENTAGE)
            }
            Token::Number { .. } | Token::Percentage { .. } | Token::Dimension { .. } => {
                use cssparser::ToCss;
                shorthand::is_length_percentage(&tok.to_css_string())
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    (1..=max).contains(&n)
}

fn blink_only_value(name: &str, value: &str) -> Option<bool> {
    if let Some(words) = crate::style::blink_names::single_keyword(name) {
        return Some(one_keyword(value, words));
    }
    if !matches!(
        name,
        "corner-shape" | "corner-top-left-shape" | "corner-top-right-shape" | "corner-bottom-left-shape"
            | "corner-bottom-right-shape" | "corner-start-start-shape" | "corner-start-end-shape"
            | "corner-end-start-shape" | "corner-end-end-shape"
    ) {
        return None;
    }
    const WORDS: &[&str] = &["notch", "scoop", "bevel", "round", "squircle", "square"];
    let max = if name == "corner-shape" { 4 } else { 1 };
    let mut input = ParserInput::new(value);
    let mut p = Parser::new(&mut input);
    let mut n = 0;
    loop {
        match p.next() {
            Ok(Token::Ident(w)) if WORDS.iter().any(|k| k.eq_ignore_ascii_case(w)) => n += 1,
            Ok(Token::Function(f)) if f.eq_ignore_ascii_case("superellipse") => {
                let ok = p
                    .parse_nested_block(|p| -> Result<bool, cssparser::ParseError<'_, ()>> {
                        let ok = match p.next()? {
                            Token::Number { .. } => true,
                            Token::Ident(i) => i.eq_ignore_ascii_case("infinity") || i.eq_ignore_ascii_case("-infinity"),
                            _ => false,
                        };
                        Ok(ok && p.is_exhausted())
                    })
                    .unwrap_or(false);
                if !ok {
                    return Some(false);
                }
                n += 1;
            }
            Ok(_) => return Some(false),
            Err(_) => return Some((1..=max).contains(&n)),
        }
    }
}

#[cfg(test)]
mod tests {
    // Chrome 152's answers (`CSS.supports`)
    #[test]
    fn background_size_takes_its_keywords_pairs_and_layers() {
        for ok in ["cover", "contain", "auto", "10px", "10px 20px", "50% auto", "auto auto", "cover, 10px", "calc(10px + 5%) auto"] {
            assert!(super::supports_declaration("background-size", ok), "{ok}");
        }
        for bad in ["-10px", "cover cover", "10px 20px 30px", "left", ""] {
            assert!(!super::supports_declaration("background-size", bad), "{bad}");
        }
    }

    use super::*;

    #[test]
    fn the_conditions_chrome_was_asked_about() {
        assert!(evaluate("(display: grid)"));
        assert!(!evaluate("(display: nope)"));
        assert!(evaluate("not (display: nope)"));
        assert!(evaluate("selector(:has(a))"));
        assert!(evaluate("(color: red)"));
        assert!(!evaluate("(colr: red)"));
        assert!(evaluate("(display: grid) and (color: red)"));
        assert!(evaluate("(display: nope) or (color: red)"));
        assert!(!evaluate("(display: grid) and (display: nope)"));
        assert!(!evaluate("(display: grid) and (color: red) or (x: y)"), "mixed operators do not parse");
        assert!(evaluate("((display: grid))"));
        assert!(!evaluate("garbage"));
        assert!(evaluate("(--x: 1)"));
    }

    /// The plain-number lane skips only a parse that was never empty, for every name the engine or Blink knows.
    #[test]
    fn plain_dimension_lane_is_the_parse() {
        let mut names: Vec<&str> = crate::style::property::ALL.iter().map(|p| p.name()).collect();
        names.extend(crate::style::blink_names::BLINK_CSS_NAMES.iter().map(|(n, _)| *n));
        let values = [
            "0", "1", "-1", "10px", "-5px", "1.5em", "50%", "0.25", "100vh", "12.345678px", "007px", "1q", "999999999px", "1.5",
            "-0.0001", "1e", "1e3", "1e3px", "1.", ".5", "1px2", "10PX", "1234567890px", "1.1234567890", "1abcdef", "-", "%",
            "1%%", "--1", "1 px", "",
        ];
        let lane = |v: &str| plain_number(v);
        assert!(["0", "-5px", "1.5em", "50%", "999999999px", "1e"].iter().all(|v| lane(v)));
        assert!(!["1e3", ".5", "1.", "10PX", "1234567890px", "1abcdef", "-", "%", "1%%", "1 px", ""].iter().any(|v| lane(v)));
        let mut applied = 0;
        for n in &names {
            for v in values {
                let p = PropertyId::from_name(n);
                let applies = p.is_some_and(|p| p != PropertyId::FontFamily && !shorthand::expands(p) && p.name() == *n)
                    && crate::style::blink_names::exposed(n).map_or(true, |t| t == *n)
                    && plain_number(v);
                if applies {
                    applied += 1;
                    let sheet = parse_inline_style(&format!("{n}: {v}"));
                    assert!(!(sheet.declarations().is_empty() && sheet.raw_declarations().is_empty()), "{n}: {v} parses to nothing");
                }
                assert_eq!(supports_declaration_parsed_with(n, v, true), supports_declaration_parsed_with(n, v, false), "{n}: {v}");
            }
        }
        assert!(applied > 1000, "the lane is taken ({applied})");
    }

    /// `CSS.supports`'s two forms: a value carries no priority, a condition's declaration may (Chrome 152,
    /// `vsup.html`, `f4sup.html`, `f4sup2.html`, 2026-10-23).
    #[test]
    fn a_value_has_no_priority_and_a_declaration_may() {
        for (n, v) in [
            ("width", "1px !important"), ("color", "red !important"), ("background-size", "cover !important"),
            ("background-size", "cover, contain !important"), ("mask-size", "cover !important"), ("x", "1 !important"),
            ("view-timeline-inset", "auto !important"), ("-webkit-mask-position-x", "left !important"),
            ("caret-color", "auto !important"), ("margin", "1px !important"), ("width", "1px!important"),
            ("font-family", "a !important"), ("background-image", "url(a!b) !important"), ("content", "\"!\" !important"),
            ("--x", "1 !important"), ("--x", "!important"), ("--x", "a!b"), ("--x", "1 ! important"),
        ] {
            assert!(!supports_value(n, v), "{n}: {v}");
        }
        for (n, v) in [
            ("width", "1px"), ("--x", "1"), ("--x", "(a!b)"), ("--x", "f(!)"), ("--x", "[!]"), ("--x", "{!}"), ("--x", "\"!\""),
            ("--x", "url(a!b)"), ("--x", "a /*!*/ b"), ("--x", "\\!"), ("content", "\"a!b\""), ("background-image", "url(a!b)"),
            ("background-image", "url(\"a!b\")"), ("width", "calc(1px) /*!important*/"), ("width", "var(--y) !important"),
        ] {
            assert!(supports_value(n, v), "{n}: {v}");
        }
        for (n, v) in [("width", "1px !important"), ("background-size", "cover !important"), ("x", "1 !important")] {
            assert!(evaluate(&format!("({n}: {v})")), "({n}: {v})");
        }
    }

    /// Values Chrome 152 keeps that a style write dropped here (`getComputedStyle` sweep, 2026-10-23), and their
    /// neighbours it drops.
    #[test]
    fn the_values_chrome_keeps_are_supported() {
        let kept = [
            ("background-size", "cover"), ("background-size", "10px 20%"), ("background-size", "cover, contain"),
            ("background-size", "auto 2em"), ("mask-size", "contain"), ("view-timeline-inset", "2em auto"),
            ("-webkit-mask-position-x", "right 20%"), ("-webkit-mask-position-y", "top"), ("-webkit-text-stroke-width", "thin"),
            ("caret-color", "auto"), ("outline-style", "auto"), ("resize", "auto"), ("x", "1"), ("cy", "-1"), ("r", "0.5"),
            ("rx", "100"), ("stroke-width", "calc(2 * 3)"), ("stroke-miterlimit", "calc(2 * 3)"),
            ("stroke-dasharray", "min(10px, 5em)"), ("stroke-dasharray", "1 2, 3"),
        ];
        for (n, v) in kept {
            assert!(supports_declaration(n, v), "{n}: {v}");
        }
        let dropped = [
            ("background-size", "red"), ("background-size", "1px 2px 3px"), ("r", "-1"), ("rx", "-2"), ("caret-color", "10px"),
            ("outline-style", "bogus"), ("view-timeline-inset", "red"), ("stroke-dasharray", "red"),
        ];
        for (n, v) in dropped {
            assert!(!supports_declaration(n, v), "{n}: {v}");
        }
        // the fill layers' grammars (Chrome 152, `sup.html` / `vaccept.html`, 2026-10-23)
        let kept = [
            ("background-size", "contain"), ("background-size", "auto"), ("background-size", "10px 20px"),
            ("mask-size", "auto 10px"), ("mask-size", "1px, 2px 3px"), ("mask-size", "auto, cover"),
            ("background-size", "calc(10px - 20px)"), ("-webkit-mask-position-x", "left"), ("-webkit-mask-position-x", "center"),
            ("-webkit-mask-position-x", "left 10px"), ("-webkit-mask-position-x", "10%"), ("-webkit-mask-position-x", "10px, 20px"),
            ("-webkit-mask-position-x", "left, right 5px"), ("-webkit-mask-position-x", "right -5px"),
            ("-webkit-mask-position-x", "calc(5px + 1%)"), ("-webkit-mask-position-y", "bottom 1em"),
            ("-webkit-mask-position-y", "top, 50%"), ("background-position-x", "right 1em"), ("background-position-x", "10px, right"),
            ("background-position-y", "bottom 1em"), ("background-size", "attr(title)"), ("mask-size", "attr(title)"),
            ("-webkit-mask-position-x", "attr(title)"), ("-webkit-mask-position-y", "attr(title)"),
        ];
        for (n, v) in kept {
            assert!(supports_declaration(n, v), "{n}: {v}");
        }
        let dropped = [
            ("background-size", "cover auto"), ("background-size", "auto cover"), ("background-size", "contain contain"),
            ("background-size", "cover contain"), ("background-size", "cover 10px"), ("background-size", "10px cover"),
            ("background-size", "10px -5px"), ("background-size", "1px, -1px"), ("background-size", "-1px"),
            ("background-size", "-10%"), ("background-size", "cover, cover cover"), ("mask-size", "contain auto"),
            ("-webkit-mask-position-x", "center 10px"), ("-webkit-mask-position-x", "x-start"), ("-webkit-mask-position-x", "x-end"),
            ("-webkit-mask-position-x", "10px 20px"), ("-webkit-mask-position-x", "10px left"), ("-webkit-mask-position-x", "left right"),
            ("-webkit-mask-position-x", "top"), ("-webkit-mask-position-x", "right 5px 5px"), ("-webkit-mask-position-y", "center 10px"),
            ("-webkit-mask-position-y", "y-end"), ("-webkit-mask-position-y", "left"), ("background-position-x", "center 10px"),
            ("background-position-x", "x-end"), ("background-position-x", "x-start 5px"), ("background-position-y", "y-end"),
        ];
        for (n, v) in dropped {
            assert!(!supports_declaration(n, v), "{n}: {v}");
        }
    }

    /// A `<paint>` with a multi-byte character across byte 4 is answered, not sliced through (it aborted the process
    /// from `style.fill = '"éé"'`, 2026-10-23).
    #[test]
    fn a_non_ascii_paint_is_answered() {
        for v in ["\"éé\"", "aéé", "\"😀\"", "éééé"] {
            assert!(!paint(v), "{v}");
            assert!(!supports_declaration("fill", v), "{v}");
            assert!(!supports_declaration("stroke", v), "{v}");
        }
        assert!(paint("url(é) red"));
        assert!(paint("url(#a) none"));
    }

    /// A value too large to keep is answered every time (`style::memo`).
    #[test]
    fn a_large_value_is_answered_not_kept() {
        let big = format!("url(data:image/png;base64,{})", "A".repeat(400_000));
        for _ in 0..2 {
            assert!(supports_declaration("background-image", &big));
            assert!(!supports_declaration("width", &big));
        }
    }

    /// The kept answers are the parse's, asked once or twice, in any case of the name.
    #[test]
    fn the_memo_answers_what_the_parse_answers() {
        let asks = [
            ("WIDTH", "10px"), ("Color", "red"), ("opacity", "50%"), ("z-index", "0010"), ("display", "nope"),
            ("corner-shape", "squircle"), ("--X", "1"), ("width", "var(--x)"), ("width", " 10px !important "),
            ("font-family", "10px"), ("width", "INITIAL"), ("margin", "1px 2px"), ("width", "-5px"),
        ];
        for _ in 0..2 {
            for (n, v) in asks {
                let v_trim = v.trim().trim_end_matches("!important").trim();
                let parsed = n.starts_with("--") || supports_declaration_parsed(&n.to_ascii_lowercase(), v_trim);
                assert_eq!(supports_declaration(n, v), parsed, "{n}: {v}");
            }
        }
    }
}
