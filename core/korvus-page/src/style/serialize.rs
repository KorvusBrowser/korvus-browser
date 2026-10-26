//! CSSOM SERIALIZATION AS BLINK WRITES IT: a declaration block's `cssText` and a
//! specified value's text, in Chromium 150's spelling.
//!
//! korvus echoed each value as the page wrote it and each shorthand as the longhands it expanded to. A page
//! that reads its own style sheet back sees the difference at once, and a managed bot challenge does:
//! its second stage reads the `cssText` of all 204 rules of the challenge frame's sheet. Chrome 150 against
//! korvus over that sheet (the nih.gov / boardgamegeek frame, 2026-10-07), 127 rules apart:
//!
//! ```text
//!   source                                   Chrome 150                                  korvus (before)
//!   opacity:.3                               opacity: 0.3                                opacity: .3
//!   fill:#fff                                fill: rgb(255, 255, 255)                    fill: #fff
//!   margin:0                                 margin: 0px                                 margin-top: 0; margin-right: 0; ...
//!   border-radius:50%                        border-radius: 50%                          border-top-left-radius: 50%; ...
//!   box-shadow:inset 0 0 0 #228b49           box-shadow: rgb(34, 139, 73) 0px 0px 0px inset    box-shadow: inset 0 0 0 #228b49
//!   transform:rotate(0)                      transform: rotate(0deg)                     transform: rotate(0)
//!   transform-origin:center                  transform-origin: center center             transform-origin: center
//!   animation:stroke .28s .41s cubic-bezier(.65, 0, .45, 1) forwards
//!       animation: 0.28s cubic-bezier(0.65, 0, 0.45, 1) 0.41s 1 normal forwards running stroke
//!   grid-area:1/1                            grid-area: 1 / 1                            grid-row-start: 1; ...
//! ```
//!
//! Two halves, both ports of Blink (`~/chromium-150/src/third_party/blink/renderer/core/css`):
//!
//! * [`value_text`] is `CSSValue::CssText` for a parsed longhand value: numbers through
//!   `CSSNumericLiteralValue::CustomCSSText` (an integer up to six digits, else `%.6g`), a unitless zero
//!   in a length slot as `0px` (the parser stores it as pixels), colours through
//!   `Color::SerializeLegacyColorAsCSSColor`, shadows in `CSSShadowValue`'s order, a single-keyword
//!   `transform-origin` as the pair the parser expands it to, lists with `, `.
//! * [`block_text`] is `StylePropertySerializer::AsText` over the block the parser kept
//!   (`FilterProperties`: `!important` declarations moved last, the last occurrence of a property winning),
//!   recombining every shorthand whose longhands are all present -- the shorthand candidates of a longhand
//!   in Blink's preferred order (most longhands first, unprefixed first, then by name).
use std::collections::HashMap;
use std::sync::OnceLock;

use cssparser::{Parser, ParserInput, Token};

/// One declaration of a block, as the serializer reads it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// The value's canonical text ([`value_text`]), or `initial` for an implicit initial value.
    pub value: String,
    pub important: bool,
    /// A component its shorthand omitted and filled with `CSSInitialValue` (Blink's `outline`,
    /// `list-style`, `background`, `text-emphasis`): it serializes as `initial` and is left out of the
    /// shorthand.
    pub implicit: bool,
}

// ---- numbers ------------------------------------------------------------------------------------------

/// `CSSNumericLiteralValue::CustomCSSText`'s number: an integer in [-999999, 999999] as an integer, any
/// other finite value as C's `%.6g`.
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return if v.is_nan() { "NaN".into() } else if v > 0.0 { "infinity".into() } else { "-infinity".into() };
    }
    if v.trunc() == v && (-999_999.0..=999_999.0).contains(&v) {
        return format!("{}", v as i64);
    }
    format_g6(v)
}

/// C's `printf("%.6g")`.
fn format_g6(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let sci = format!("{:.5e}", v); // d.ddddde[-]x
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        let mant = trim_zeros(mant);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", exp.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    trim_zeros(&format!("{:.*}", decimals, v))
}

fn trim_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

// ---- component values ---------------------------------------------------------------------------------

/// One top-level component of a value.
#[derive(Debug, Clone, PartialEq)]
enum C {
    Ident(String),
    Num(f64),
    Pct(f64),
    Dim(f64, String),
    Hash(String),
    Str(String),
    Url(String),
    Func(String, Vec<C>),
    Block(char, Vec<C>),
    Comma,
    Slash,
    Delim(char),
}

/// The numeric prefix of a number token's text, as `f64` (cssparser keeps `f32`, which would print
/// `0.65` as `0.649999976`).
pub(crate) fn number_of(text: &str) -> f64 {
    let t = text.trim();
    let end = t
        .char_indices()
        .take_while(|&(i, c)| {
            c.is_ascii_digit()
                || c == '.'
                || ((c == '+' || c == '-') && (i == 0 || matches!(t.as_bytes()[i - 1], b'e' | b'E')))
                || ((c == 'e' || c == 'E') && t[i + 1..].starts_with(|n: char| n.is_ascii_digit() || n == '+' || n == '-'))
        })
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    t[..end].parse().unwrap_or(0.0)
}

fn components(p: &mut Parser) -> Vec<C> {
    let mut out = Vec::new();
    loop {
        let start = p.position();
        let tok = match p.next_including_whitespace() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        let text = p.slice_from(start).to_string();
        let c = match tok {
            Token::WhiteSpace(_) | Token::Comment(_) => continue,
            Token::Ident(s) => C::Ident(s.to_string()),
            Token::Number { .. } => C::Num(number_of(&text)),
            Token::Percentage { .. } => C::Pct(number_of(&text)),
            Token::Dimension { unit, .. } => C::Dim(number_of(&text), unit.to_ascii_lowercase()),
            Token::Hash(s) | Token::IDHash(s) => C::Hash(s.to_string()),
            Token::QuotedString(s) => C::Str(s.to_string()),
            Token::UnquotedUrl(s) => C::Url(s.to_string()),
            Token::Comma => C::Comma,
            Token::Delim('/') => C::Slash,
            Token::Delim(d) => C::Delim(d),
            Token::Function(name) => {
                let name = name.to_ascii_lowercase();
                let inner = p.parse_nested_block(|q| -> Result<Vec<C>, cssparser::ParseError<'_, ()>> { Ok(components(q)) }).unwrap_or_default();
                if name == "url" {
                    match inner.as_slice() {
                        [C::Str(s)] => C::Url(s.clone()),
                        _ => C::Func(name, inner),
                    }
                } else {
                    C::Func(name, inner)
                }
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                let open = match tok {
                    Token::ParenthesisBlock => '(',
                    Token::SquareBracketBlock => '[',
                    _ => '{',
                };
                let inner = p.parse_nested_block(|q| -> Result<Vec<C>, cssparser::ParseError<'_, ()>> { Ok(components(q)) }).unwrap_or_default();
                C::Block(open, inner)
            }
            Token::Colon => C::Delim(':'),
            Token::Semicolon => C::Delim(';'),
            _ => C::Delim('\u{0}'),
        };
        out.push(c);
    }
    out
}

fn parse(text: &str) -> Vec<C> {
    let mut input = ParserInput::new(text);
    let mut p = Parser::new(&mut input);
    components(&mut p)
}

// ---- colours ------------------------------------------------------------------------------------------

/// An sRGB colour: 0..255 channels and alpha 0..1.
#[derive(Debug, Clone, Copy)]
struct Rgba(f64, f64, f64, f64);

fn hex_color(h: &str) -> Option<Rgba> {
    let d = |s: &str| u8::from_str_radix(s, 16).ok().map(f64::from);
    let one = |c: char| d(&format!("{c}{c}"));
    let cs: Vec<char> = h.chars().collect();
    match cs.len() {
        3 => Some(Rgba(one(cs[0])?, one(cs[1])?, one(cs[2])?, 1.0)),
        4 => Some(Rgba(one(cs[0])?, one(cs[1])?, one(cs[2])?, one(cs[3])? / 255.0)),
        6 => Some(Rgba(d(&h[0..2])?, d(&h[2..4])?, d(&h[4..6])?, 1.0)),
        8 => Some(Rgba(d(&h[0..2])?, d(&h[2..4])?, d(&h[4..6])?, d(&h[6..8])? / 255.0)),
        _ => None,
    }
}

/// The arguments of a colour function, split on commas or on whitespace with a `/` before the alpha.
fn color_args(args: &[C]) -> Option<(Vec<C>, Option<C>)> {
    let commas = args.iter().any(|c| *c == C::Comma);
    if commas {
        let items: Vec<&[C]> = args.split(|c| *c == C::Comma).collect();
        if items.iter().any(|i| i.len() != 1) || !(3..=4).contains(&items.len()) {
            return None;
        }
        let v: Vec<C> = items.iter().map(|i| i[0].clone()).collect();
        let alpha = v.get(3).cloned();
        return Some((v[..3].to_vec(), alpha));
    }
    let mut parts = args.split(|c| *c == C::Slash);
    let main: Vec<C> = parts.next()?.to_vec();
    let alpha = match parts.next() {
        Some([a]) => Some(a.clone()),
        Some(_) => return None,
        None => None,
    };
    if parts.next().is_some() || main.len() != 3 {
        return None;
    }
    Some((main, alpha))
}

fn alpha_of(c: &Option<C>) -> Option<f64> {
    Some(match c {
        None => 1.0,
        Some(C::Num(n)) => *n,
        Some(C::Pct(p)) => p / 100.0,
        _ => return None,
    }
    .clamp(0.0, 1.0))
}

fn hue_of(c: &C) -> Option<f64> {
    Some(match c {
        C::Num(n) => *n,
        C::Dim(n, u) => match u.as_str() {
            "deg" => *n,
            "rad" => n.to_degrees(),
            "grad" => n * 0.9,
            "turn" => n * 360.0,
            _ => return None,
        },
        _ => return None,
    })
}

fn func_color(name: &str, args: &[C]) -> Option<Rgba> {
    let (m, a) = color_args(args)?;
    let alpha = alpha_of(&a)?;
    match name {
        "rgb" | "rgba" => {
            let ch = |c: &C| -> Option<f64> {
                Some(match c {
                    C::Num(n) => *n,
                    C::Pct(p) => p * 2.55,
                    _ => return None,
                })
            };
            Some(Rgba(ch(&m[0])?, ch(&m[1])?, ch(&m[2])?, alpha))
        }
        "hsl" | "hsla" => {
            let h = hue_of(&m[0])?;
            let pct = |c: &C| match c {
                C::Pct(p) => Some(p / 100.0),
                C::Num(n) => Some(n / 100.0),
                _ => None,
            };
            let (s, l) = (pct(&m[1])?.clamp(0.0, 1.0), pct(&m[2])?.clamp(0.0, 1.0));
            let (r, g, b) = hsl_to_rgb(h, s, l);
            Some(Rgba(r * 255.0 + 1e-7 * 255.0, g * 255.0 + 1e-7 * 255.0, b * 255.0 + 1e-7 * 255.0, alpha))
        }
        "hwb" => {
            let h = hue_of(&m[0])?;
            let pct = |c: &C| match c {
                C::Pct(p) => Some(p / 100.0),
                C::Num(n) => Some(n / 100.0),
                _ => None,
            };
            let (mut w, mut bl) = (pct(&m[1])?, pct(&m[2])?);
            if w + bl >= 1.0 {
                let gray = w / (w + bl);
                return Some(Rgba(gray * 255.0, gray * 255.0, gray * 255.0, alpha));
            }
            w = w.clamp(0.0, 1.0);
            bl = bl.clamp(0.0, 1.0);
            let (r, g, b) = hsl_to_rgb(h, 1.0, 0.5);
            let f = |c: f64| (c * (1.0 - w - bl) + w) * 255.0 + 1e-7 * 255.0;
            Some(Rgba(f(r), f(g), f(b), alpha))
        }
        _ => None,
    }
}

/// CSS Color 4 "HSL to RGB", channels 0..1.
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let h = h.rem_euclid(360.0);
    let f = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (-1.0f64).max((k - 3.0).min((9.0 - k).min(1.0)))
    };
    (f(0.0), f(8.0), f(4.0))
}

/// `Color::SerializeLegacyColorAsCSSColor`.
fn color_text(c: Rgba) -> String {
    let ch = |v: f64| format!("{}", v.clamp(0.0, 255.0).round() as i64);
    let opaque = c.3 >= 1.0;
    let mut out = String::from(if opaque { "rgb(" } else { "rgba(" });
    out.push_str(&format!("{}, {}, {}", ch(c.0), ch(c.1), ch(c.2)));
    if !opaque {
        let int_alpha = ((c.3 + 1e-7) * 255.0).round().clamp(0.0, 255.0);
        let two = (int_alpha * 100.0 / 255.0).round() / 100.0;
        let a = if (two * 255.0).round() == int_alpha { two } else { (int_alpha * 1000.0 / 255.0).round() / 1000.0 };
        out.push_str(", ");
        out.push_str(&format_number(a));
    }
    out.push(')');
    out
}

// ---- the value serializer ----------------------------------------------------------------------------

/// What a bare number means in a slot: a `<length>` (Blink stores a unitless zero as `0px`) or a number.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Number,
    Length,
    Angle,
}

/// Properties whose bare numbers are lengths: a unitless zero there is `0px` (Blink `ConsumeLength`).
fn length_property(name: &str) -> bool {
    let n = name;
    n.starts_with("margin")
        || n.starts_with("padding")
        || n.starts_with("inset")
        || n.starts_with("scroll-margin")
        || n.starts_with("scroll-padding")
        || (n.starts_with("border") && (n.ends_with("-width") || n.ends_with("radius") || n.ends_with("spacing")))
        || n.ends_with("-spacing")
        || matches!(
            n,
            "top" | "right" | "bottom" | "left" | "width" | "height" | "min-width" | "min-height" | "max-width" | "max-height"
                | "inline-size" | "block-size" | "min-inline-size" | "min-block-size" | "max-inline-size" | "max-block-size"
                | "outline-width" | "outline-offset" | "gap" | "row-gap" | "column-gap" | "column-width" | "font-size"
                | "text-indent" | "flex-basis" | "perspective" | "vertical-align" | "shape-margin" | "text-underline-offset"
                | "text-decoration-thickness" | "background-position-x" | "background-position-y" | "background-size"
                | "mask-position" | "object-position" | "transform-origin" | "perspective-origin" | "translate"
                | "box-shadow" | "text-shadow" | "column-rule-width" | "-webkit-text-stroke-width" | "x" | "y" | "r"
                | "rx" | "ry" | "cx" | "cy" | "offset-distance" | "-webkit-border-horizontal-spacing"
                | "-webkit-border-vertical-spacing" | "contain-intrinsic-width" | "contain-intrinsic-height"
        )
}

/// Properties whose numbers are `<integer>`s (Blink `UnitType::kInteger`).
fn integer_property(name: &str) -> bool {
    matches!(name, "z-index" | "order" | "orphans" | "widows" | "column-count" | "-webkit-line-clamp" | "counter-reset" | "counter-increment" | "counter-set" | "grid-row-start" | "grid-row-end" | "grid-column-start" | "grid-column-end")
}

/// Properties whose keywords are case-sensitive names, kept as written.
fn case_sensitive(name: &str) -> bool {
    matches!(
        name,
        "animation-name" | "font-family" | "counter-reset" | "counter-increment" | "counter-set" | "list-style-type"
            | "grid-area" | "grid-row-start" | "grid-row-end" | "grid-column-start" | "grid-column-end"
            | "grid-template-areas" | "view-transition-name" | "container-name" | "anchor-name" | "position-anchor"
            | "font-feature-settings" | "font-variation-settings" | "quotes" | "content" | "font-palette"
            | "timeline-scope" | "scroll-timeline-name" | "view-timeline-name" | "animation-timeline" | "page"
            | "transition-property" | "will-change"
    )
}

fn transform_slot(func: &str) -> Slot {
    match func {
        "rotate" | "rotatex" | "rotatey" | "rotatez" | "skew" | "skewx" | "skewy" => Slot::Angle,
        "translate" | "translatex" | "translatey" | "translatez" | "translate3d" | "perspective" => Slot::Length,
        _ => Slot::Number,
    }
}

/// The canonical name of a function: Blink spells the transform functions in camelCase.
fn function_name(name: &str) -> &str {
    match name {
        "translatex" => "translateX",
        "translatey" => "translateY",
        "translatez" => "translateZ",
        "rotatex" => "rotateX",
        "rotatey" => "rotateY",
        "rotatez" => "rotateZ",
        "scalex" => "scaleX",
        "scaley" => "scaleY",
        "scalez" => "scaleZ",
        "skewx" => "skewX",
        "skewy" => "skewY",
        "matrix3d" | "translate3d" | "scale3d" | "rotate3d" => name,
        _ => name,
    }
}

struct Writer<'a> {
    prop: &'a str,
}

impl Writer<'_> {
    fn one(&self, c: &C, slot: Slot) -> String {
        match c {
            C::Ident(s) => {
                if s.eq_ignore_ascii_case("currentcolor") {
                    "currentcolor".into()
                } else if case_sensitive(self.prop) {
                    s.clone()
                } else {
                    s.to_ascii_lowercase()
                }
            }
            C::Num(n) => match slot {
                // an `<integer>` property's value is `kInteger`, written whole at any size
                _ if integer_property(self.prop) && n.trunc() == *n && n.abs() < 9.0e15 => format!("{}", *n as i64),
                Slot::Length if *n == 0.0 => "0px".into(),
                Slot::Angle if *n == 0.0 => "0deg".into(),
                _ => format_number(*n),
            },
            C::Pct(n) => format!("{}%", format_number(*n)),
            C::Dim(n, u) => format!("{}{}", format_number(*n), u),
            C::Hash(h) => match hex_color(h) {
                Some(rgba) => color_text(rgba),
                None => format!("#{h}"),
            },
            C::Str(s) => quote(s),
            C::Url(u) => format!("url({})", quote(u)),
            C::Func(name, args) => {
                if let Some(rgba) = matches!(name.as_str(), "rgb" | "rgba" | "hsl" | "hsla" | "hwb").then(|| func_color(name, args)).flatten() {
                    return color_text(rgba);
                }
                let inner_slot = match self.prop {
                    "transform" => transform_slot(name),
                    _ => slot,
                };
                format!("{}({})", function_name(name), self.list(args, inner_slot))
            }
            C::Block(open, inner) => {
                let close = match open {
                    '(' => ')',
                    '[' => ']',
                    _ => '}',
                };
                format!("{open}{}{close}", self.list(inner, slot))
            }
            C::Comma => ",".into(),
            C::Slash => "/".into(),
            C::Delim(d) => d.to_string(),
        }
    }

    /// A sequence: `, ` after a comma, ` / ` around a slash, single spaces otherwise.
    fn list(&self, cs: &[C], slot: Slot) -> String {
        let mut out = String::new();
        for (i, c) in cs.iter().enumerate() {
            match c {
                C::Comma => out.push_str(", "),
                C::Slash => out.push_str(" / "),
                _ => {
                    if i > 0 && !matches!(cs[i - 1], C::Comma | C::Slash) {
                        out.push(' ');
                    }
                    out.push_str(&self.one(c, slot));
                }
            }
        }
        out
    }
}

/// A CSS string, double-quoted, as Blink's `SerializeString` writes it.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\u{0}' => out.push('\u{FFFD}'),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\{:x} ", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_color_component(c: &C) -> bool {
    match c {
        C::Hash(_) => true,
        C::Func(n, _) => matches!(n.as_str(), "rgb" | "rgba" | "hsl" | "hsla" | "hwb" | "lab" | "lch" | "oklab" | "oklch" | "color" | "color-mix" | "light-dark"),
        C::Ident(s) => !s.eq_ignore_ascii_case("inset") && !s.eq_ignore_ascii_case("none"),
        _ => false,
    }
}

/// `CSSShadowValue::CustomCSSText`: colour, offsets, blur, spread, then `inset`.
fn shadows(w: &Writer, cs: &[C]) -> String {
    let mut out = Vec::new();
    for shadow in cs.split(|c| *c == C::Comma) {
        if let [C::Ident(none)] = shadow {
            if none.eq_ignore_ascii_case("none") {
                return "none".into();
            }
        }
        let color = shadow.iter().find(|c| is_color_component(c));
        let inset = shadow.iter().any(|c| matches!(c, C::Ident(s) if s.eq_ignore_ascii_case("inset")));
        let mut parts = Vec::new();
        if let Some(c) = color {
            parts.push(w.one(c, Slot::Number));
        }
        for c in shadow.iter().filter(|c| matches!(c, C::Num(_) | C::Dim(..) | C::Pct(_) | C::Func(..)) && !is_color_component(c)) {
            parts.push(w.one(c, Slot::Length));
        }
        if inset {
            parts.push("inset".into());
        }
        out.push(parts.join(" "));
    }
    out.join(", ")
}

/// `transform-origin`/`perspective-origin`: one value becomes the pair Blink's parser stores (`center`
/// -> `center center`, `top` -> `center top`), a vertical keyword first is swapped behind the horizontal one.
fn origin(w: &Writer, cs: &[C]) -> String {
    let kw = |c: &C| match c {
        C::Ident(s) => Some(s.to_ascii_lowercase()),
        _ => None,
    };
    let vertical = |c: &C| matches!(kw(c).as_deref(), Some("top" | "bottom"));
    let horizontal = |c: &C| matches!(kw(c).as_deref(), Some("left" | "right"));
    let mut v: Vec<C> = cs.to_vec();
    if v.len() == 1 {
        if vertical(&v[0]) {
            v.insert(0, C::Ident("center".into()));
        } else {
            v.push(C::Ident("center".into()));
        }
    } else if v.len() >= 2 && (vertical(&v[0]) || horizontal(&v[1])) && !(vertical(&v[0]) && vertical(&v[1])) {
        v.swap(0, 1);
    }
    v.iter().map(|c| w.one(c, Slot::Length)).collect::<Vec<_>>().join(" ")
}

/// A media query list as `MediaList.mediaText` writes it (Blink `MediaQuerySet::MediaText`): Blink's own parse and
/// serialization of each query (`style::media`) -- `(min-aspect-ratio: 16/9)` is
/// `(min-aspect-ratio: 16 / 9)`, an unknown `(foo:bar)` stays as written, `, ` between the queries.
pub fn media_text(text: &str) -> String {
    crate::style::media::serialize(text)
}

/// `CSSValue::CssText` of a longhand's specified value.
pub fn value_text(name: &str, raw: &str) -> String {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    if matches!(lower.as_str(), "initial" | "inherit" | "unset" | "revert" | "revert-layer") {
        return lower;
    }
    // Already canonical when stored (`font_family::canonical`).
    if name == "font-family" {
        return raw.to_string();
    }
    // A math function is its parsed, simplified expression (`style::calc`): Chrome 150 writes
    // `width: calc(1px + 2px)` back as `calc(3px)`, `min(1px, 2px)` as `calc(1px)`.
    if let Some(e) = crate::style::calc::single(raw) {
        return e.css_text();
    }
    // An alpha value's percentage is its number (`ConsumeNumberOrPercent`, the `parse_helper` of `opacity` and its
    // kin): Chrome 150 reads `opacity: 25%` back as `0.25`, where this wrote `25%`. A percentage
    // inside a math function stays one (`calc(25%)`).
    if matches!(name, "opacity" | "fill-opacity" | "stroke-opacity" | "flood-opacity" | "stop-opacity" | "shape-image-threshold") {
        let percentage = raw
            .strip_suffix('%')
            .filter(|n| n.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'+' | b'-' | b'e' | b'E')));
        if let Some((p, _)) = percentage.and_then(crate::style::calc::number_of) {
            return format_number(p.clamp(f32::MIN as f64, f32::MAX as f64) / 100.0);
        }
    }
    let math = crate::style::calc::serialize_math_in(raw);
    let raw = math.as_str();
    let cs = parse(raw);
    let w = Writer { prop: name };
    match name {
        "box-shadow" | "text-shadow" => shadows(&w, &cs),
        "transform-origin" | "perspective-origin" => origin(&w, &cs),
        "stroke-dasharray" => {
            let items: Vec<C> = cs.into_iter().filter(|c| *c != C::Comma).collect();
            items.iter().map(|c| w.one(c, Slot::Number)).collect::<Vec<_>>().join(", ")
        }
        _ => {
            let slot = if length_property(name) { Slot::Length } else { Slot::Number };
            w.list(&cs, slot)
        }
    }
}

// ---- shorthands -------------------------------------------------------------------------------------

/// How a shorthand is serialized (`StylePropertySerializer::SerializeShorthand`'s switch).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Four,
    Two,
    BorderRadius,
    BorderSide,
    Border,
    BorderImage,
    Values,
    DoubleBar,
    GridArea,
    GridLine,
    Layered,
    Columns,
    /// `background-position` / `mask-position` over their halves: each layer's x, then its y.
    Position,
    /// `marker`: the one value all three share, or nothing.
    Same,
    /// `page-break-*` / `-webkit-column-break-*` over a `break-*` longhand (`PageBreakPropertyValue`).
    PageBreak,
    /// `-webkit-column-break-*` (`ColumnBreakPropertyValue`).
    ColumnBreak,
    /// `font` over its nineteen longhands (`FontValue`).
    Font,
    /// `white-space` over `white-space-collapse` and `text-wrap-mode` (`WhiteSpaceValue`).
    WhiteSpace,
}

/// Blink's shorthands, with their longhands in Blink's order (`css_properties.json5`).
const SHORTHANDS: &[(&str, Kind, &[&str])] = &[
    ("margin", Kind::Four, &["margin-top", "margin-right", "margin-bottom", "margin-left"]),
    ("padding", Kind::Four, &["padding-top", "padding-right", "padding-bottom", "padding-left"]),
    ("inset", Kind::Four, &["top", "right", "bottom", "left"]),
    ("scroll-margin", Kind::Four, &["scroll-margin-top", "scroll-margin-right", "scroll-margin-bottom", "scroll-margin-left"]),
    ("scroll-padding", Kind::Four, &["scroll-padding-top", "scroll-padding-right", "scroll-padding-bottom", "scroll-padding-left"]),
    ("border-width", Kind::Four, &["border-top-width", "border-right-width", "border-bottom-width", "border-left-width"]),
    ("border-style", Kind::Four, &["border-top-style", "border-right-style", "border-bottom-style", "border-left-style"]),
    ("border-color", Kind::Four, &["border-top-color", "border-right-color", "border-bottom-color", "border-left-color"]),
    ("border-radius", Kind::BorderRadius, &["border-top-left-radius", "border-top-right-radius", "border-bottom-right-radius", "border-bottom-left-radius"]),
    ("border-top", Kind::BorderSide, &["border-top-width", "border-top-style", "border-top-color"]),
    ("border-right", Kind::BorderSide, &["border-right-width", "border-right-style", "border-right-color"]),
    ("border-bottom", Kind::BorderSide, &["border-bottom-width", "border-bottom-style", "border-bottom-color"]),
    ("border-left", Kind::BorderSide, &["border-left-width", "border-left-style", "border-left-color"]),
    (
        "border",
        Kind::Border,
        &[
            "border-top-color", "border-top-style", "border-top-width", "border-right-color", "border-right-style", "border-right-width",
            "border-bottom-color", "border-bottom-style", "border-bottom-width", "border-left-color", "border-left-style",
            "border-left-width", "border-image-source", "border-image-slice", "border-image-width", "border-image-outset",
            "border-image-repeat",
        ],
    ),
    ("border-image", Kind::BorderImage, &["border-image-source", "border-image-slice", "border-image-width", "border-image-outset", "border-image-repeat"]),
    ("border-block-start", Kind::Values, &["border-block-start-width", "border-block-start-style", "border-block-start-color"]),
    ("border-block-end", Kind::Values, &["border-block-end-width", "border-block-end-style", "border-block-end-color"]),
    ("border-inline-start", Kind::Values, &["border-inline-start-width", "border-inline-start-style", "border-inline-start-color"]),
    ("border-inline-end", Kind::Values, &["border-inline-end-width", "border-inline-end-style", "border-inline-end-color"]),
    ("outline", Kind::Values, &["outline-color", "outline-style", "outline-width"]),
    ("overflow", Kind::Two, &["overflow-x", "overflow-y"]),
    ("gap", Kind::Two, &["row-gap", "column-gap"]),
    ("place-content", Kind::Two, &["align-content", "justify-content"]),
    ("place-items", Kind::Two, &["align-items", "justify-items"]),
    ("place-self", Kind::Two, &["align-self", "justify-self"]),
    ("margin-block", Kind::Two, &["margin-block-start", "margin-block-end"]),
    ("margin-inline", Kind::Two, &["margin-inline-start", "margin-inline-end"]),
    ("padding-block", Kind::Two, &["padding-block-start", "padding-block-end"]),
    ("padding-inline", Kind::Two, &["padding-inline-start", "padding-inline-end"]),
    ("inset-block", Kind::Two, &["inset-block-start", "inset-block-end"]),
    ("inset-inline", Kind::Two, &["inset-inline-start", "inset-inline-end"]),
    ("overscroll-behavior", Kind::Two, &["overscroll-behavior-x", "overscroll-behavior-y"]),
    ("border-block-color", Kind::Two, &["border-block-start-color", "border-block-end-color"]),
    ("border-block-style", Kind::Two, &["border-block-start-style", "border-block-end-style"]),
    ("border-block-width", Kind::Two, &["border-block-start-width", "border-block-end-width"]),
    ("border-inline-color", Kind::Two, &["border-inline-start-color", "border-inline-end-color"]),
    ("border-inline-style", Kind::Two, &["border-inline-start-style", "border-inline-end-style"]),
    ("border-inline-width", Kind::Two, &["border-inline-start-width", "border-inline-end-width"]),
    ("flex-flow", Kind::DoubleBar, &["flex-direction", "flex-wrap"]),
    ("flex", Kind::Values, &["flex-grow", "flex-shrink", "flex-basis"]),
    ("list-style", Kind::Values, &["list-style-position", "list-style-image", "list-style-type"]),
    ("text-emphasis", Kind::Values, &["text-emphasis-style", "text-emphasis-color"]),
    ("grid-area", Kind::GridArea, &["grid-row-start", "grid-column-start", "grid-row-end", "grid-column-end"]),
    ("grid-row", Kind::GridLine, &["grid-row-start", "grid-row-end"]),
    ("grid-column", Kind::GridLine, &["grid-column-start", "grid-column-end"]),
    ("columns", Kind::Columns, &["column-width", "column-count"]),
    ("background-position", Kind::Position, &["background-position-x", "background-position-y"]),
    ("mask-position", Kind::Position, &["-webkit-mask-position-x", "-webkit-mask-position-y"]),
    ("interest-delay", Kind::Two, &["interest-delay-start", "interest-delay-end"]),
    ("corner-shape", Kind::Four, &["corner-top-left-shape", "corner-top-right-shape", "corner-bottom-right-shape", "corner-bottom-left-shape"]),
    ("corner-top-shape", Kind::Two, &["corner-top-left-shape", "corner-top-right-shape"]),
    ("corner-right-shape", Kind::Two, &["corner-top-right-shape", "corner-bottom-right-shape"]),
    ("corner-bottom-shape", Kind::Two, &["corner-bottom-left-shape", "corner-bottom-right-shape"]),
    ("corner-left-shape", Kind::Two, &["corner-top-left-shape", "corner-bottom-left-shape"]),
    ("white-space", Kind::WhiteSpace, &["white-space-collapse", "text-wrap-mode"]),
    ("corner-block-start-shape", Kind::Two, &["corner-start-start-shape", "corner-start-end-shape"]),
    ("corner-block-end-shape", Kind::Two, &["corner-end-start-shape", "corner-end-end-shape"]),
    ("corner-inline-start-shape", Kind::Two, &["corner-start-start-shape", "corner-end-start-shape"]),
    ("corner-inline-end-shape", Kind::Two, &["corner-start-end-shape", "corner-end-end-shape"]),
    ("marker", Kind::Same, &["marker-start", "marker-mid", "marker-end"]),
    ("-webkit-text-stroke", Kind::Values, &["-webkit-text-stroke-width", "-webkit-text-stroke-color"]),
    ("page-break-after", Kind::PageBreak, &["break-after"]),
    ("page-break-before", Kind::PageBreak, &["break-before"]),
    ("page-break-inside", Kind::PageBreak, &["break-inside"]),
    ("-webkit-column-break-after", Kind::ColumnBreak, &["break-after"]),
    ("-webkit-column-break-before", Kind::ColumnBreak, &["break-before"]),
    ("-webkit-column-break-inside", Kind::ColumnBreak, &["break-inside"]),
    (
        "animation",
        Kind::Layered,
        &[
            "animation-duration", "animation-timing-function", "animation-delay", "animation-iteration-count", "animation-direction",
            "animation-fill-mode", "animation-play-state", "animation-name", "animation-timeline", "animation-range-start",
            "animation-range-end",
        ],
    ),
    ("transition", Kind::Layered, &["transition-property", "transition-duration", "transition-timing-function", "transition-delay", "transition-behavior"]),
    (
        "font",
        Kind::Font,
        &[
            "font-style", "font-variant-ligatures", "font-variant-caps", "font-variant-numeric", "font-variant-east-asian",
            "font-variant-alternates", "font-variant-position", "font-variant-emoji", "font-weight", "font-stretch", "font-size",
            "line-height", "font-family", "font-optical-sizing", "font-size-adjust", "font-kerning", "font-feature-settings",
            "font-variation-settings", "font-language-override",
        ],
    ),
    (
        "background",
        Kind::Layered,
        &[
            "background-image", "background-position-x", "background-position-y", "background-size", "background-repeat",
            "background-attachment", "background-origin", "background-clip", "background-color",
        ],
    ),
];

/// The shorthands a longhand belongs to, in Blink's preferred order (`make_style_shorthands.py`).
fn matching(longhand: &str) -> &'static [usize] {
    static MAP: OnceLock<HashMap<&'static str, Vec<usize>>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m: HashMap<&'static str, Vec<usize>> = HashMap::new();
        for (i, (_, _, longs)) in SHORTHANDS.iter().enumerate() {
            for l in *longs {
                m.entry(l).or_default().push(i);
            }
        }
        for v in m.values_mut() {
            v.sort_by_key(|&i| {
                let (name, _, longs) = SHORTHANDS[i];
                let hyphen = if name.starts_with("-webkit-") { 1 } else if name.starts_with('-') { 2 } else { 0 };
                (std::cmp::Reverse(longs.len()), hyphen, name)
            });
        }
        m
    });
    map.get(longhand).map(|v| v.as_slice()).unwrap_or(&[])
}

/// The declared initial value Blink compares against (`Longhand::InitialValue()`'s text).
fn initial_of(name: &str) -> &'static str {
    match name {
        "flex-direction" => "row",
        "flex-wrap" => "nowrap",
        "border-image-source" => "none",
        "border-image-slice" => "100%",
        "border-image-width" => "1",
        "border-image-outset" => "0",
        "border-image-repeat" => "stretch",
        _ => "",
    }
}

/// `AllowInitialInShorthand`.
fn allow_initial(shorthand: &str) -> bool {
    matches!(
        shorthand,
        "background" | "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-block-start"
            | "border-block-end" | "border-inline-start" | "border-inline-end" | "border-block" | "border-inline" | "outline"
            | "column-rule" | "columns" | "grid-column" | "grid-row" | "grid-area" | "gap" | "list-style" | "text-decoration"
            | "text-emphasis" | "text-wrap" | "mask" | "-webkit-text-stroke" | "white-space"
    )
}

fn is_css_wide(v: &str) -> bool {
    matches!(v, "initial" | "inherit" | "unset" | "revert" | "revert-layer")
}

struct Block<'a> {
    entries: &'a [Entry],
}

impl Block<'_> {
    fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    fn val(&self, name: &str) -> &str {
        self.get(name).map(|e| e.value.as_str()).unwrap_or("")
    }

    fn initial(&self, name: &str) -> bool {
        self.get(name).is_some_and(|e| e.implicit || e.value == "initial")
    }

    /// `CommonShorthandChecks`: `Some(text)` decides the result (`""` is failure), `None` goes on.
    fn common_checks(&self, shorthand: &str, longs: &[&str]) -> Option<String> {
        let mut vals = Vec::new();
        let (mut imp, mut non) = (false, false);
        for l in longs {
            let Some(e) = self.get(l) else { return Some(String::new()) };
            imp |= e.important;
            non |= !e.important;
            vals.push(e);
        }
        if imp && non {
            return Some(String::new());
        }
        // A longhand holding an unsubstituted `var()` or `env()` is Blink's unparsed value, which no shorthand writes
        // (`CommonShorthandChecks`): Chrome 150 writes `.a { padding-inline-start: max(22px, var(--a));
        // padding-inline-end: max(22px, var(--b)) }` back as those two, and `style.paddingInline` is "" (`varsh.html`).
        if vals.iter().any(|v| (v.value.contains("var(") || v.value.contains("env(")) && crate::style::variables::references(&v.value) == crate::style::variables::References::Some) {
            return Some(String::new());
        }
        if is_css_wide(&vals[0].value) && vals.iter().all(|v| v.value == vals[0].value) {
            return Some(vals[0].value.clone());
        }
        let allow = allow_initial(shorthand);
        for v in &vals {
            if !allow && (v.implicit || v.value == "initial") {
                return Some(String::new());
            }
            if is_css_wide(&v.value) && v.value != "initial" {
                return Some(String::new());
            }
        }
        None
    }

    fn shorthand(&self, idx: usize) -> String {
        let (name, kind, longs) = SHORTHANDS[idx];
        if let Some(r) = self.common_checks(name, longs) {
            return r;
        }
        let v = |i: usize| self.val(longs[i]).to_string();
        match kind {
            Kind::Four => {
                let (t, r, b, l) = (v(0), v(1), v(2), v(3));
                let show_left = r != l;
                let show_bottom = t != b || show_left;
                let show_right = t != r || show_bottom;
                let mut out = t;
                if show_right {
                    out.push(' ');
                    out.push_str(&r);
                }
                if show_bottom {
                    out.push(' ');
                    out.push_str(&b);
                }
                if show_left {
                    out.push(' ');
                    out.push_str(&l);
                }
                out
            }
            Kind::Two => {
                let (a, b) = (v(0), v(1));
                if a == b {
                    a
                } else {
                    format!("{a} {b}")
                }
            }
            Kind::BorderRadius => {
                let split = |s: String| -> (String, String) {
                    let parts: Vec<&str> = s.split(' ').collect();
                    match parts.as_slice() {
                        [a, b] => (a.to_string(), b.to_string()),
                        _ => (s.clone(), s),
                    }
                };
                let corners: Vec<(String, String)> = (0..4).map(|i| split(v(i))).collect();
                let ser = |vals: [&String; 4]| -> String {
                    let [tl, tr, br, bl] = vals;
                    let show_bl = tr != bl;
                    let show_br = tl != br || show_bl;
                    let show_tr = tl != tr || show_br;
                    let mut out = tl.clone();
                    for (show, x) in [(show_tr, tr), (show_br, br), (show_bl, bl)] {
                        if show {
                            out.push(' ');
                            out.push_str(x);
                        }
                    }
                    out
                };
                let mut out = ser([&corners[0].0, &corners[1].0, &corners[2].0, &corners[3].0]);
                if corners.iter().any(|(a, b)| a != b) {
                    out.push_str(" / ");
                    out.push_str(&ser([&corners[0].1, &corners[1].1, &corners[2].1, &corners[3].1]));
                }
                out
            }
            Kind::BorderSide => border_values([v(0), v(1), v(2)]),
            Kind::Border => {
                for l in &longs[12..] {
                    let e = self.val(l);
                    if !self.initial(l) && e != initial_of(l) {
                        return String::new();
                    }
                }
                let common = |names: [&str; 4]| -> Option<String> {
                    let first = self.val(names[0]);
                    names.iter().all(|n| self.val(n) == first).then(|| first.to_string())
                };
                let side = |kind: &str| ["top", "right", "bottom", "left"].map(|s| format!("border-{s}-{kind}"));
                let get = |kind: &str| {
                    let n = side(kind);
                    common([&n[0], &n[1], &n[2], &n[3]])
                };
                match (get("width"), get("style"), get("color")) {
                    (Some(w), Some(s), Some(c)) => border_values([w, s, c]),
                    _ => String::new(),
                }
            }
            Kind::BorderImage => {
                if (1..5).all(|i| self.val(longs[i]) == initial_of(longs[i])) {
                    return v(0);
                }
                let mut out = String::new();
                for i in 0..5 {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    if i == 2 || i == 3 {
                        out.push_str("/ ");
                    }
                    out.push_str(&v(i));
                }
                out
            }
            Kind::Values => longs
                .iter()
                .filter(|l| !self.initial(l))
                .map(|l| self.val(l).to_string())
                .collect::<Vec<_>>()
                .join(" "),
            Kind::DoubleBar => {
                let parts: Vec<String> = longs.iter().filter(|l| self.val(l) != initial_of(l)).map(|l| self.val(l).to_string()).collect();
                if parts.is_empty() {
                    initial_of(longs[0]).to_string()
                } else {
                    parts.join(" ")
                }
            }
            Kind::GridArea => {
                let (rs, cs, re, ce) = (v(0), v(1), v(2), v(3));
                let inc_cs = include_grid_end(&rs, &cs);
                let inc_re = include_grid_end(&rs, &re);
                let inc_ce = include_grid_end(&cs, &ce);
                let mut out = rs;
                if inc_cs || inc_re || inc_ce {
                    out.push_str(" / ");
                    out.push_str(&cs);
                }
                if inc_re || inc_ce {
                    out.push_str(" / ");
                    out.push_str(&re);
                }
                if inc_ce {
                    out.push_str(" / ");
                    out.push_str(&ce);
                }
                out
            }
            Kind::GridLine => {
                let (s, e) = (v(0), v(1));
                if include_grid_end(&s, &e) {
                    format!("{s} / {e}")
                } else {
                    s
                }
            }
            Kind::Columns => {
                let (w, c) = (v(0), v(1));
                match (w == "auto", c == "auto") {
                    (true, true) => "auto".into(),
                    (false, true) => w,
                    (true, false) => c,
                    (false, false) => format!("{w} {c}"),
                }
            }
            Kind::Layered => self.layered(name, longs),
            Kind::Same => {
                let first = v(0);
                if longs.iter().all(|l| self.val(l) == first) { first } else { String::new() }
            }
            // `always` for a page (column) break, `auto`/`avoid` (and `left`/`right` for a page) as they are, else nothing
            Kind::PageBreak => match v(0).as_str() {
                "page" => "always".into(),
                "auto" | "avoid" | "left" | "right" if !name.ends_with("inside") => v(0),
                "auto" | "avoid" => v(0),
                _ => String::new(),
            },
            Kind::ColumnBreak => match v(0).as_str() {
                "column" => "always".into(),
                "auto" | "avoid" => v(0),
                _ => String::new(),
            },
            Kind::Position => {
                let (x, y) = (split_layers(self.val(longs[0])), split_layers(self.val(longs[1])));
                // a layer's implicit initial half cannot be written as a position (Chrome 150: "")
                if x.len() != y.len() || x.iter().chain(&y).any(|v| v == "initial") {
                    return String::new();
                }
                x.iter().zip(&y).map(|(a, b)| format!("{a} {b}")).collect::<Vec<_>>().join(", ")
            }
            Kind::Font => self.font(),
            Kind::WhiteSpace => white_space_value(&v(0), &v(1)),
        }
    }

    /// `StylePropertySerializer::FontValue`: the sub-properties the shorthand cannot write at their
    /// initial values, `font-variant-caps` `normal` or `small-caps`, `font-stretch` a keyword (a percentage that is
    /// one, `GetFontStretchKeyword`), then `[style] [caps] [weight] [stretch] size[ / line-height] family` with the
    /// `normal` ones left out (`AppendFontLonghandValueIfNotNormal`). Chrome 150 reads `font: bold 12px/1.5 serif`
    /// back as `font: bold 12px / 1.5 serif`.
    fn font(&self) -> String {
        const RESET: [(&str, &str); 12] = [
            ("font-variant-ligatures", "normal"),
            ("font-variant-numeric", "normal"),
            ("font-variant-east-asian", "normal"),
            ("font-kerning", "auto"),
            ("font-optical-sizing", "auto"),
            ("font-variation-settings", "normal"),
            ("font-feature-settings", "normal"),
            ("font-variant-alternates", "normal"),
            ("font-variant-position", "normal"),
            ("font-variant-emoji", "normal"),
            ("font-size-adjust", "none"),
            ("font-language-override", "normal"),
        ];
        if RESET.iter().any(|(name, initial)| self.val(name) != *initial) {
            return String::new();
        }
        let caps = self.val("font-variant-caps");
        if caps != "normal" && caps != "small-caps" {
            return String::new();
        }
        const STRETCH: [(f64, &str); 9] = [
            (50.0, "ultra-condensed"),
            (62.5, "extra-condensed"),
            (75.0, "condensed"),
            (87.5, "semi-condensed"),
            (100.0, "normal"),
            (112.5, "semi-expanded"),
            (125.0, "expanded"),
            (150.0, "extra-expanded"),
            (200.0, "ultra-expanded"),
        ];
        let stretch = self.val("font-stretch");
        let stretch = match stretch.strip_suffix('%').and_then(|n| n.parse::<f64>().ok()) {
            Some(p) => match STRETCH.iter().find(|(v, _)| *v == p) {
                Some((_, keyword)) => *keyword,
                None => return String::new(),
            },
            None => stretch,
        };
        let mut out = String::new();
        for v in [self.val("font-style"), caps, self.val("font-weight"), stretch] {
            if v != "normal" && !v.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(v);
            }
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(self.val("font-size"));
        let line_height = self.val("line-height");
        if line_height != "normal" && !line_height.is_empty() {
            out.push_str(" / ");
            out.push_str(line_height);
        }
        out.push(' ');
        out.push_str(self.val("font-family"));
        out
    }

    /// `GetLayeredShorthandValue` for `animation`, `transition` and `background`.
    fn layered(&self, name: &str, longs: &[&str]) -> String {
        let lists: Vec<Vec<String>> = longs.iter().map(|l| split_layers(self.val(l))).collect();
        let layers = lists.iter().map(|l| l.len()).max().unwrap_or(1).max(1);
        let mut result: Vec<String> = Vec::new();
        for layer in 0..layers {
            let mut out = String::new();
            let (mut pos_x, mut pos_y) = (false, false);
            for (i, prop) in longs.iter().enumerate() {
                let implicit = self.get(prop).is_some_and(|e| e.implicit);
                let value = if lists[i].len() > 1 {
                    lists[i].get(layer).cloned()
                } else if (layer == 0 && *prop != "background-color") || (layer == layers - 1 && *prop == "background-color") {
                    lists[i].first().cloned()
                } else {
                    None
                };
                let Some(value) = value else { continue };
                let mut omit = implicit || value == "initial";
                match *prop {
                    "animation-timeline" => {
                        if value != "auto" || layer > 0 {
                            return String::new();
                        }
                        omit = true;
                    }
                    "animation-range-start" | "animation-range-end" => {
                        if value != "normal" || layer > 0 {
                            return String::new();
                        }
                        omit = true;
                    }
                    "transition-behavior" if value == "normal" => omit = true,
                    "transition-delay" | "transition-duration" if value == "0s" || value == "0ms" => omit = true,
                    "transition-timing-function" if value == "ease" => omit = true,
                    "transition-property" if value == "all" => omit = true,
                    _ => {}
                }
                if omit {
                    continue;
                }
                if *prop == "background-size" {
                    out.push_str(if pos_x || pos_y { " / " } else { " 0% 0% / " });
                } else if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&value);
                pos_x |= *prop == "background-position-x";
                pos_y |= *prop == "background-position-y";
            }
            if name == "transition" && out.is_empty() {
                out.push_str("all");
            }
            if !out.is_empty() {
                result.push(out);
            }
        }
        result.join(", ")
    }
}

/// A layered longhand's per-layer values, split on top-level `, `.
fn split_layers(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut cur) = (0i32, String::new());
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur.trim().to_string());
    out
}

/// `SerializeBorderValues`: width, style and colour, each left out at its initial value.
fn border_values(v: [String; 3]) -> String {
    let initial = ["medium", "none", "currentcolor"];
    v.iter().zip(initial).filter(|(x, i)| !x.is_empty() && x.as_str() != *i).map(|(x, _)| x.clone()).collect::<Vec<_>>().join(" ")
}

/// `CSSOMUtils::IncludeDependentGridLineEndValue`: an integer or `span` line is a list and always
/// written; a repeated name or an `auto` after a non-name start is left out.
fn include_grid_end(start: &str, end: &str) -> bool {
    let is_list = |s: &str| s.split(' ').any(|t| t == "span" || t.parse::<i64>().is_ok());
    let is_custom_ident = |s: &str| !is_list(s) && s != "auto" && !s.contains(' ');
    is_list(end) || (end != start && (is_custom_ident(start) || end != "auto"))
}

/// Blink's `FilterProperties`: `!important` declarations moved after the others (stably), then each
/// property kept at its last occurrence.
pub fn filter(entries: Vec<Entry>) -> Vec<Entry> {
    let (mut normal, important): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| !e.important);
    normal.extend(important);
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Entry> = Vec::new();
    for e in normal.into_iter().rev() {
        if seen.insert(e.name.clone()) {
            out.push(e);
        }
    }
    out.reverse();
    out
}

fn property_text(name: &str, value: &str, important: bool) -> String {
    format!("{name}: {value}{};", if important { " !important" } else { "" })
}

/// `StylePropertySerializer::AsText` over a filtered block.
pub fn block_text(entries: &[Entry]) -> String {
    block_text_counted(entries).0
}

/// [`block_text`] and how many declarations it wrote (a shorthand one for all its longhands).
pub fn block_text_counted(entries: &[Entry]) -> (String, usize) {
    let block = Block { entries };
    let mut serialized_longhand: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut tried: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        if serialized_longhand.contains(e.name.as_str()) {
            continue;
        }
        let mut done = false;
        for &idx in matching(&e.name) {
            let (sh, _, longs) = SHORTHANDS[idx];
            // an alias Blink implements as a one-longhand shorthand is not preferred (`StylePropertySerializer::AsText`):
            // `break-after: page` stays `break-after: page`, though `page-break-after` reads `always`
            if longs.len() == 1 {
                continue;
            }
            if !tried.insert(idx) {
                continue;
            }
            if longs.iter().any(|l| serialized_longhand.contains(l)) {
                continue;
            }
            let text = block.shorthand(idx);
            if text.is_empty() {
                continue;
            }
            out.push(property_text(sh, &text, e.important));
            serialized_longhand.extend(longs.iter().copied());
            done = true;
            break;
        }
        if !done {
            out.push(property_text(&e.name, &e.value, e.important));
        }
    }
    (out.join(" "), out.len())
}

/// Is `name` one of the shorthands this serializer writes?
pub fn is_shorthand_name(name: &str) -> bool {
    SHORTHANDS.iter().any(|(n, _, _)| *n == name)
}

/// A shorthand's value for `getPropertyValue`, or `None` when `name` is not one of these shorthands
/// (`Some("")` when its longhands cannot be written as it).
/// `StylePropertySerializer::WhiteSpaceValue`: the `white-space` keyword the pair spells (`ToWhiteSpace` +
/// `IsValidWhiteSpace`), else the longhands that are not at their initial values (`collapse`, `wrap`), space-separated.
/// Chrome 150 (`S-white-space-shorthand`, three runs alike): an inline `white-space: pre-wrap`
/// reads `style.whiteSpace` "pre-wrap" and cssText `white-space: pre-wrap;`, and so do the two longhands written
/// out; korvus read "" and wrote the longhands, since the declared block had no `white-space` to recombine.
fn white_space_value(collapse: &str, wrap: &str) -> String {
    match (collapse, wrap) {
        ("collapse", "wrap") => "normal".into(),
        ("preserve", "nowrap") => "pre".into(),
        ("preserve", "wrap") => "pre-wrap".into(),
        ("preserve-breaks", "wrap") => "pre-line".into(),
        ("collapse", "nowrap") => "nowrap".into(),
        ("break-spaces", "wrap") => "break-spaces".into(),
        _ => [(collapse, "collapse"), (wrap, "wrap")].iter().filter(|(v, init)| v != init).map(|(v, _)| *v).collect::<Vec<_>>().join(" "),
    }
}

pub fn shorthand_value(name: &str, entries: &[Entry]) -> Option<String> {
    let idx = SHORTHANDS.iter().position(|(n, _, _)| *n == name)?;
    Some(Block { entries }.shorthand(idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_as_blink_formats_them() {
        assert_eq!(format_number(0.65), "0.65");
        assert_eq!(format_number(0.3), "0.3");
        assert_eq!(format_number(242.0), "242");
        assert_eq!(format_number(1.0 / 3.0), "0.333333");
        assert_eq!(format_number(1e-7), "1e-07");
        assert_eq!(format_number(1234567.0), "1.23457e+06");
        assert_eq!(format_number(-0.5), "-0.5");
        assert_eq!(format_number(100.5), "100.5");
    }

    #[test]
    fn values_as_chrome_150_serializes_them() {
        // each row read off Chrome 150's cssText of the challenge sheet (module note)
        assert_eq!(value_text("opacity", ".3"), "0.3");
        assert_eq!(value_text("fill", "#fff"), "rgb(255, 255, 255)");
        assert_eq!(value_text("color", "#f2f2f2"), "rgb(242, 242, 242)");
        assert_eq!(value_text("margin-left", "0"), "0px");
        assert_eq!(value_text("stroke-dashoffset", "0"), "0");
        assert_eq!(value_text("stroke-width", "2"), "2");
        assert_eq!(value_text("stroke-dasharray", "0,100,0"), "0, 100, 0");
        assert_eq!(value_text("box-shadow", "inset 0 0 0 #228b49"), "rgb(34, 139, 73) 0px 0px 0px inset");
        assert_eq!(value_text("transform", "rotate(0) scale(.6)"), "rotate(0deg) scale(0.6)");
        assert_eq!(value_text("transform", "translateY(0)"), "translateY(0px)");
        assert_eq!(value_text("transform-origin", "center"), "center center");
        assert_eq!(value_text("animation-timing-function", "cubic-bezier(.65, 0, .45, 1)"), "cubic-bezier(0.65, 0, 0.45, 1)");
        assert_eq!(value_text("animation-duration", ".28s"), "0.28s");
        assert_eq!(value_text("color", "rgba(0,0,0,.5)"), "rgba(0, 0, 0, 0.5)");
        assert_eq!(value_text("color", "#ff000080"), "rgba(255, 0, 0, 0.5)");
        assert_eq!(value_text("color", "hsl(120, 100%, 50%)"), "rgb(0, 255, 0)");
    }

    fn e(name: &str, value: &str) -> Entry {
        Entry { name: name.into(), value: value_text(name, value), important: false, implicit: false }
    }

    #[test]
    fn shorthands_recombine_as_blink_does() {
        let m = ["margin-top", "margin-right", "margin-bottom", "margin-left"].map(|n| e(n, "0"));
        assert_eq!(block_text(&m), "margin: 0px;");
        let g = [e("grid-row-start", "1"), e("grid-column-start", "1"), e("grid-row-end", "auto"), e("grid-column-end", "auto")];
        assert_eq!(block_text(&g), "grid-area: 1 / 1;");
        let o = [e("overflow-x", "visible"), e("overflow-y", "visible")];
        assert_eq!(block_text(&o), "overflow: visible;");
        let mut b: Vec<Entry> = Vec::new();
        for side in ["top", "right", "bottom", "left"] {
            b.push(e(&format!("border-{side}-width"), "2px"));
            b.push(e(&format!("border-{side}-style"), "solid"));
            b.push(e(&format!("border-{side}-color"), "#999"));
        }
        for (n, v) in [("border-image-source", "none"), ("border-image-slice", "100%"), ("border-image-width", "1"), ("border-image-outset", "0"), ("border-image-repeat", "stretch")] {
            b.push(e(n, v));
        }
        assert_eq!(block_text(&b), "border: 2px solid rgb(153, 153, 153);");
        let ws = [e("white-space-collapse", "preserve"), e("text-wrap-mode", "wrap")];
        assert_eq!(block_text(&ws), "white-space: pre-wrap;");
        let ws2 = [e("white-space-collapse", "preserve-breaks"), e("text-wrap-mode", "nowrap")];
        assert_eq!(block_text(&ws2), "white-space: preserve-breaks nowrap;");
        let dup = filter(vec![e("stroke-width", "1px"), e("fill", "#fff"), e("stroke-width", "6px")]);
        assert_eq!(block_text(&dup), "fill: rgb(255, 255, 255); stroke-width: 6px;");
    }

    /// Chrome 150 (`fixtures/css-answers/inline-style-chrome-150.json`): `font` over its nineteen
    /// longhands, and an alpha value's percentage.
    #[test]
    fn font_and_alpha_values_as_chrome_150_writes_them() {
        let font = |style: &str, weight: &str, line_height: &str| -> Vec<Entry> {
            let mut out = vec![e("font-style", style), e("font-variant-caps", "normal")];
            for (n, v) in [
                ("font-variant-ligatures", "normal"),
                ("font-variant-numeric", "normal"),
                ("font-variant-east-asian", "normal"),
                ("font-variant-alternates", "normal"),
                ("font-size-adjust", "none"),
                ("font-language-override", "normal"),
                ("font-kerning", "auto"),
                ("font-optical-sizing", "auto"),
                ("font-feature-settings", "normal"),
                ("font-variation-settings", "normal"),
                ("font-variant-position", "normal"),
                ("font-variant-emoji", "normal"),
            ] {
                out.push(e(n, v));
            }
            out.extend([e("font-weight", weight), e("font-stretch", "normal"), e("font-size", "12px"), e("line-height", line_height)]);
            out.push(Entry { name: "font-family".into(), value: "serif".into(), important: false, implicit: false });
            out
        };
        assert_eq!(block_text(&font("normal", "normal", "normal")), "font: 12px serif;");
        assert_eq!(block_text(&font("italic", "bold", "1.5")), "font: italic bold 12px / 1.5 serif;");
        let mut kerned = font("normal", "normal", "normal");
        kerned[8] = e("font-kerning", "none");
        assert!(block_text(&kerned).starts_with("font-style: normal;"), "a sub-property the shorthand cannot write: longhands");
        assert_eq!(value_text("opacity", "25%"), "0.25");
        assert_eq!(value_text("opacity", "calc(25%)"), "calc(25%)");
    }

    // Chrome 150 (`varsh.html`): longhands holding `var()` are written as themselves, never as their shorthand.
    #[test]
    fn a_longhand_with_a_reference_is_never_folded_into_its_shorthand() {
        let raw = |n: &str, v: &str| Entry { name: n.into(), value: v.into(), important: false, implicit: false };
        let inline = [raw("padding-inline-start", "max(22px, var(--a))"), raw("padding-inline-end", "max(22px, var(--b))")];
        assert_eq!(block_text(&inline), "padding-inline-start: max(22px, var(--a)); padding-inline-end: max(22px, var(--b));");
        let four = [raw("padding-top", "var(--a)"), e("padding-right", "1px"), e("padding-bottom", "1px"), e("padding-left", "1px")];
        assert_eq!(block_text(&four), "padding-top: var(--a); padding-right: 1px; padding-bottom: 1px; padding-left: 1px;");
        let content = [raw("margin-top", "0px"), raw("margin-right", "0px"), raw("margin-bottom", "0px"), raw("margin-left", "0px")];
        assert_eq!(block_text(&content), "margin: 0px;");
    }
}
