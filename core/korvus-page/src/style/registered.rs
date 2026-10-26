//! Registered custom properties: `@property` (CSS Properties and Values API 1).
//!
//! An unregistered `--x` is a token stream that inherits. A REGISTERED one has a grammar, may not
//! inherit, has an initial value, and computes to a TYPED value: `<length>` resolves `em` to px,
//! `<color>` to `rgb()`. A value that does not fit the grammar is invalid at computed-value time and
//! the property takes its initial (or inherited) value. Measured on `/usr/bin/google-chrome`
//! 150.0.7871.46 over loopback (the unit's `atrules.html`):
//!
//! ```text
//! @property --x { syntax: "<length>"; inherits: false; initial-value: 7px }
//! .px   { width: var(--x) }                                width 7px     --x "7px"
//! .pem  { --x: 2em; font-size: 10px; width: var(--x) }     width 20px    --x "20px"
//! .pbad { --x: red; width: var(--x) }                      width 7px     --x "7px"
//! .pinh { --x: 11px } > div                                 --x "7px"  (not inherited)
//! @property --inh { … inherits: true; initial-value: 3px }  body --inh "3px"; under --inh: 5px "5px"
//! @property --col { syntax: "<color>" … }  .pcol { --col: #00ff00 }         "rgb(0, 255, 0)"
//! @property --any { syntax: "*"; inherits: false }          body --any ""
//! ```
//!
//! Before this unit the rule was dropped at parse: `--x` read `""` and `width: var(--x)` fell back to
//! the element's auto width (1904px).

use crate::style::at_rules::DescriptorBlock;
use crate::value::{serialize_number, Color, Length, LengthPercentage};
use cssparser::{Parser, ParserInput, Token};
use std::collections::HashMap;

/// One `@property` that registered: the grammar, whether it inherits, and its initial value (already
/// computed, since `initial-value` must be computationally independent).
#[derive(Debug, Clone, PartialEq)]
pub struct Registration {
    pub syntax: Syntax,
    pub inherits: bool,
    pub initial: Option<String>,
}

/// A `syntax` descriptor.
#[derive(Debug, Clone, PartialEq)]
pub enum Syntax {
    /// `"*"`: any token stream, as an unregistered property.
    Universal,
    /// `a | b | c`, each a component with an optional `+` / `#` multiplier.
    Alternatives(Vec<(Component, Multiplier)>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Component {
    Length,
    Number,
    Percentage,
    LengthPercentage,
    Color,
    Integer,
    Angle,
    Time,
    Resolution,
    Url,
    Image,
    CustomIdent,
    TransformFunction,
    TransformList,
    String,
    Ident(String),
}

/// Why `CSS.registerProperty` refused a definition (each is a SyntaxError in Blink).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    BadSyntax,
    NoInitial,
    InitialDoesNotParse,
    NotIndependent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Multiplier {
    One,
    Space,
    Comma,
}

/// Every registration a document's sheets make, by name. The last `@property` of a name in cascade
/// order wins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropertyRegistry {
    map: HashMap<String, Registration>,
}

/// What resolving a typed value needs to know about the element.
#[derive(Debug, Clone, Copy)]
pub struct TypedContext {
    pub font_size: f32,
    pub root_font_size: f32,
    pub viewport: (f32, f32),
}

impl PropertyRegistry {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Registration> {
        self.map.get(name)
    }

    pub fn insert(&mut self, name: String, reg: Registration) {
        self.map.insert(name, reg);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Registration)> {
        self.map.iter().map(|(n, r)| (n.as_str(), r))
    }
}

impl Registration {
    /// Register from an `@property` block. `None` when a required descriptor is missing or invalid,
    /// which leaves the name unregistered (the rule stays in `cssRules` all the same).
    pub fn from_block(block: &DescriptorBlock) -> Option<Registration> {
        let syntax = parse_syntax(&unquote(block.get("syntax"))?)?;
        let inherits = match block.get("inherits").trim().to_ascii_lowercase().as_str() {
            "true" => true,
            "false" => false,
            _ => return None,
        };
        let raw_initial = block.has("initial-value").then(|| block.get("initial-value").trim().to_string());
        let initial = match (&syntax, raw_initial) {
            (Syntax::Universal, v) => v,
            (_, None) => return None,
            (s, Some(v)) => {
                // Computationally independent: no relative units, no `var()`.
                if v.contains("var(") || has_relative_unit(&v) {
                    return None;
                }
                let ctx = TypedContext { font_size: 16.0, root_font_size: 16.0, viewport: (0.0, 0.0) };
                Some(compute_typed(s, &v, &ctx)?)
            }
        };
        Some(Registration { syntax, inherits, initial })
    }

    /// Register from `CSS.registerProperty(definition)`: Blink's `PropertyRegistration::registerProperty`
    /// checks, in its order -- the syntax parses, an initial value is given unless the syntax is `*`,
    /// it parses for the syntax, and it is computationally independent.
    pub fn from_script(syntax: &str, inherits: bool, initial: Option<&str>) -> Result<Registration, RegisterError> {
        let syntax = parse_syntax(syntax).ok_or(RegisterError::BadSyntax)?;
        let initial = match (&syntax, initial) {
            (Syntax::Universal, v) => v.map(|v| v.trim().to_string()),
            (_, None) => return Err(RegisterError::NoInitial),
            (s, Some(v)) => {
                let ctx = TypedContext { font_size: 16.0, root_font_size: 16.0, viewport: (0.0, 0.0) };
                let computed = compute_typed(s, v, &ctx).ok_or(RegisterError::InitialDoesNotParse)?;
                if v.contains("var(") || has_relative_unit(v) {
                    return Err(RegisterError::NotIndependent);
                }
                Some(computed)
            }
        };
        Ok(Registration { syntax, inherits, initial })
    }

    /// The computed value of `value` under this registration, or `None` when it does not fit the
    /// grammar (invalid at computed-value time).
    pub fn compute(&self, value: &str, ctx: &TypedContext) -> Option<String> {
        match &self.syntax {
            Syntax::Universal => Some(value.trim().to_string()),
            s => compute_typed(s, value, ctx),
        }
    }
}

/// `"<length>"` -> `<length>`; the descriptor is a `<string>`.
fn unquote(text: &str) -> Option<String> {
    let t = text.trim();
    let mut pi = ParserInput::new(t);
    let mut p = Parser::new(&mut pi);
    let s = p.expect_string().ok()?.to_string();
    p.expect_exhausted().ok()?;
    Some(s)
}

fn has_relative_unit(value: &str) -> bool {
    let mut pi = ParserInput::new(value);
    let mut p = Parser::new(&mut pi);
    while let Ok(t) = p.next() {
        if let Token::Dimension { unit, .. } = t {
            let u = unit.to_ascii_lowercase();
            if matches!(u.as_str(), "em" | "rem" | "ex" | "ch" | "lh" | "rlh" | "vw" | "vh" | "vmin" | "vmax" | "cqw" | "cqh") {
                return true;
            }
        }
    }
    false
}

/// Parse a syntax string (CSS Properties and Values 1 §5.1).
pub fn parse_syntax(text: &str) -> Option<Syntax> {
    let t = text.trim();
    if t == "*" {
        return Some(Syntax::Universal);
    }
    let mut out = Vec::new();
    for part in t.split('|') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let (body, mult) = if let Some(b) = part.strip_suffix('+') {
            (b, Multiplier::Space)
        } else if let Some(b) = part.strip_suffix('#') {
            (b, Multiplier::Comma)
        } else {
            (part, Multiplier::One)
        };
        let component = if let Some(name) = body.strip_prefix('<').and_then(|b| b.strip_suffix('>')) {
            match name {
                "length" => Component::Length,
                "number" => Component::Number,
                "percentage" => Component::Percentage,
                "length-percentage" => Component::LengthPercentage,
                "color" => Component::Color,
                "integer" => Component::Integer,
                "angle" => Component::Angle,
                "time" => Component::Time,
                "resolution" => Component::Resolution,
                "url" => Component::Url,
                "image" => Component::Image,
                "custom-ident" => Component::CustomIdent,
                "transform-function" => Component::TransformFunction,
                "string" => Component::String,
                // `<transform-list>` takes no multiplier of its own.
                "transform-list" if mult == Multiplier::One => Component::TransformList,
                _ => return None,
            }
        } else {
            if body.is_empty() || !body.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || body.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            Component::Ident(body.to_string())
        };
        out.push((component, mult));
    }
    Some(Syntax::Alternatives(out))
}

fn compute_typed(syntax: &Syntax, value: &str, ctx: &TypedContext) -> Option<String> {
    let Syntax::Alternatives(alts) = syntax else { return Some(value.trim().to_string()) };
    let value = value.trim();
    for (component, mult) in alts {
        let pieces: Vec<String> = match mult {
            Multiplier::One => vec![value.to_string()],
            Multiplier::Space => split_top_level(value, ' '),
            Multiplier::Comma => split_top_level(value, ','),
        };
        if pieces.is_empty() || pieces.iter().any(|p| p.trim().is_empty()) {
            continue;
        }
        let computed: Option<Vec<String>> = pieces.iter().map(|p| compute_one(component, p.trim(), ctx)).collect();
        if let Some(parts) = computed {
            return Some(match mult {
                Multiplier::Comma => parts.join(", "),
                _ => parts.join(" "),
            });
        }
    }
    None
}

fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && (ch == sep || (sep == ' ' && ch.is_ascii_whitespace())) {
            if !cur.trim().is_empty() || sep == ',' {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() || (sep == ',' && !out.is_empty()) {
        out.push(cur.trim().to_string());
    }
    out
}

fn single_token<'a>(value: &'a str) -> Option<Token<'a>> {
    let mut pi = ParserInput::new(value);
    let mut p = Parser::new(&mut pi);
    let t = p.next().ok()?.clone();
    p.expect_exhausted().ok()?;
    // Re-own through `into_owned`-like copies of the parts we need.
    Some(match t {
        Token::Dimension { value, int_value, has_sign, unit } => Token::Dimension { value, int_value, has_sign, unit: unit.to_string().into() },
        Token::Number { value, int_value, has_sign } => Token::Number { value, int_value, has_sign },
        Token::Percentage { unit_value, int_value, has_sign } => Token::Percentage { unit_value, int_value, has_sign },
        Token::Ident(i) => Token::Ident(i.to_string().into()),
        Token::QuotedString(s) => Token::QuotedString(s.to_string().into()),
        Token::UnquotedUrl(u) => Token::UnquotedUrl(u.to_string().into()),
        _ => return None,
    })
}

fn length_px(value: &str, ctx: &TypedContext) -> Option<f32> {
    let mut pi = ParserInput::new(value);
    let mut p = Parser::new(&mut pi);
    let l = Length::parse(&mut p).ok()?;
    p.expect_exhausted().ok()?;
    let rc = crate::value::ResolveContext::new(ctx.font_size, ctx.root_font_size, ctx.viewport.0, ctx.viewport.1);
    Some(l.to_px(&rc))
}

fn compute_one(component: &Component, value: &str, ctx: &TypedContext) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if matches!(lower.as_str(), "initial" | "inherit" | "unset" | "revert" | "revert-layer") {
        return None;
    }
    match component {
        Component::Length => length_px(value, ctx).map(crate::value::serialize_px),
        Component::LengthPercentage => {
            if let Some(Token::Percentage { unit_value, .. }) = single_token(value) {
                return Some(format!("{}%", serialize_number(unit_value * 100.0)));
            }
            let mut pi = ParserInput::new(value);
            let mut p = Parser::new(&mut pi);
            match LengthPercentage::parse(&mut p) {
                Ok(LengthPercentage::Length(_)) if p.expect_exhausted().is_ok() => length_px(value, ctx).map(crate::value::serialize_px),
                _ => None,
            }
        }
        Component::Percentage => match single_token(value)? {
            Token::Percentage { unit_value, .. } => Some(format!("{}%", serialize_number(unit_value * 100.0))),
            _ => None,
        },
        Component::Number => match single_token(value)? {
            Token::Number { value, .. } => Some(serialize_number(value)),
            _ => None,
        },
        Component::Integer => match single_token(value)? {
            Token::Number { int_value: Some(i), .. } => Some(i.to_string()),
            _ => None,
        },
        Component::Angle => match single_token(value)? {
            Token::Dimension { value: v, unit, .. } => {
                let deg = match unit.to_ascii_lowercase().as_str() {
                    "deg" => v,
                    "rad" => v.to_degrees(),
                    "grad" => v * 0.9,
                    "turn" => v * 360.0,
                    _ => return None,
                };
                Some(format!("{}deg", serialize_number(deg)))
            }
            Token::Number { value: 0.0, .. } => Some("0deg".to_string()),
            _ => None,
        },
        Component::Time => match single_token(value)? {
            Token::Dimension { value: v, unit, .. } => match unit.to_ascii_lowercase().as_str() {
                "s" => Some(format!("{}s", serialize_number(v))),
                "ms" => Some(format!("{}s", serialize_number(v / 1000.0))),
                _ => None,
            },
            _ => None,
        },
        Component::Resolution => match single_token(value)? {
            Token::Dimension { value: v, unit, .. } => match unit.to_ascii_lowercase().as_str() {
                "dppx" | "x" => Some(format!("{}dppx", serialize_number(v))),
                "dpi" => Some(format!("{}dppx", serialize_number(v / 96.0))),
                "dpcm" => Some(format!("{}dppx", serialize_number(v * 2.54 / 96.0))),
                _ => None,
            },
            _ => None,
        },
        Component::Color => match Color::parse_str(value).ok()? {
            Color::CurrentColor => Some("currentcolor".to_string()),
            c => Some(c.serialize()),
        },
        Component::Url => {
            let t = value.trim();
            (t.to_ascii_lowercase().starts_with("url(") && t.ends_with(')')).then(|| t.to_string())
        }
        Component::Image => {
            let t = value.trim().to_ascii_lowercase();
            (t.starts_with("url(") || t.contains("gradient(") || t.starts_with("image-set(")).then(|| value.trim().to_string())
        }
        Component::String => match single_token(value)? {
            Token::QuotedString(_) => Some(value.trim().to_string()),
            _ => None,
        },
        Component::CustomIdent => match single_token(value)? {
            Token::Ident(i) if !matches!(i.to_ascii_lowercase().as_str(), "default") => Some(i.to_string()),
            _ => None,
        },
        Component::Ident(want) => match single_token(value)? {
            Token::Ident(i) if i.as_ref() == want => Some(i.to_string()),
            _ => None,
        },
        Component::TransformFunction | Component::TransformList => {
            let t = value.trim();
            (t.contains('(') && t.ends_with(')')).then(|| t.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(pairs: &[(&str, &str)]) -> DescriptorBlock {
        let mut b = DescriptorBlock::default();
        for (k, v) in pairs {
            b.set(k, v.to_string());
        }
        b
    }

    const CTX: TypedContext = TypedContext { font_size: 10.0, root_font_size: 16.0, viewport: (800.0, 600.0) };

    #[test]
    fn a_length_registration_computes_to_px_and_rejects_what_is_not_a_length() {
        let r = Registration::from_block(&block(&[("syntax", "\"<length>\""), ("inherits", "false"), ("initial-value", "7px")])).unwrap();
        assert_eq!(r.initial.as_deref(), Some("7px"));
        assert!(!r.inherits);
        assert_eq!(r.compute("2em", &CTX).as_deref(), Some("20px"));
        assert_eq!(r.compute("red", &CTX), None);
    }

    #[test]
    fn required_descriptors_decide_whether_it_registers() {
        assert!(Registration::from_block(&block(&[("syntax", "\"<length>\""), ("inherits", "false")])).is_none());
        assert!(Registration::from_block(&block(&[("syntax", "\"*\""), ("inherits", "false")])).is_some());
        assert!(Registration::from_block(&block(&[("syntax", "\"<length>\""), ("inherits", "false"), ("initial-value", "1em")])).is_none());
    }

    #[test]
    fn colors_and_alternatives() {
        let r = Registration::from_block(&block(&[("syntax", "\"<color> | none\""), ("inherits", "true"), ("initial-value", "red")])).unwrap();
        assert_eq!(r.initial.as_deref(), Some("rgb(255, 0, 0)"));
        assert_eq!(r.compute("#00ff00", &CTX).as_deref(), Some("rgb(0, 255, 0)"));
        assert_eq!(r.compute("none", &CTX).as_deref(), Some("none"));
        let list = parse_syntax("<length>+").unwrap();
        assert_eq!(compute_typed(&list, "1px 2em", &CTX).as_deref(), Some("1px 20px"));
    }
}
