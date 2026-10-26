//! CSS values — the layer under both the cascade and the canvas.
//!
//! This file sits at the crate root, outside `style/` and `layout/`, on purpose: it is the floor of
//! both, and the canvas bindings must call *this* rather than keep their own parsers. That is not
//! deduplication for its own sake. `ctx.fillStyle = "hsl(120 50% 50%)"` and
//! `getComputedStyle(el).color` are two ways of asking the same engine the same question, and a
//! detector can ask both. Two parsers that disagree are not duplication, they are an
//! *inconsistency* — a tell. `html_canvas_element.rs` currently carries a 57-line color parser that
//! knows seven color names and only comma-separated `rgb()`; it gets deleted in favor of this one.
//!
//! Parsing is error-tolerant by construction, which is why the error type is `()`. CSS has no
//! exceptions: a declaration that does not parse is dropped and the cascade carries on, so nothing
//! downstream ever inspects *why* a value failed.

use cssparser::{Parser, Token};

/// A parse that either produced a value or did not. See the module note on why there is no detail.
pub type Res<T> = Result<T, ()>;

/// A resolved colour, as 8-bit sRGB channels and the alpha CSS parsed -- plus, for a colour in a space those 8 bits
/// only approximate (`lab()`, `oklch()`, `color(display-p3 ...)`), the colour as Blink holds it:
/// that is what it serializes as and paints with. Chrome 150 keeps `lab(50 40 30)` as written through
/// `el.style.color` and `getComputedStyle`; this engine dropped the declaration and answered `rgb(0, 0, 0)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
    /// The non-legacy colour the channels above project, `None` for a legacy one (`rgb()`, `hsl()`, `hwb()`, hex,
    /// a name).
    pub wide: Option<crate::css_color::Color>,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0.0, wide: None };
    pub const BLACK: Rgba = Rgba { r: 0, g: 0, b: 0, a: 1.0, wide: None };

    pub const fn opaque(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 1.0, wide: None }
    }

    /// A colour as [`crate::css_color`] parsed it: a legacy one by the channels Blink serializes it with, any other
    /// kept whole beside its clamped sRGB projection.
    pub fn from_css(c: crate::css_color::Color) -> Rgba {
        let a = if c.alpha_none { 0.0 } else { c.alpha };
        if c.space.is_legacy() {
            let [r, g, b] = c.legacy_rgb8();
            return Rgba { r, g, b, a, wide: None };
        }
        let p = c.to_sk_color4f();
        let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgba { r: ch(p[0]), g: ch(p[1]), b: ch(p[2]), a, wide: Some(c) }
    }

    /// The colour as [`crate::css_color`] holds it.
    pub fn to_css(self) -> crate::css_color::Color {
        self.wide.unwrap_or_else(|| crate::css_color::Color::from_legacy(self.r, self.g, self.b, self.a))
    }

    /// Premultiplication-free 8-bit form, for handing to a rasterizer.
    pub fn to_rgba8(self) -> [u8; 4] {
        [self.r, self.g, self.b, (self.a.clamp(0.0, 1.0) * 255.0).round() as u8]
    }

    /// The paint colour (`spec::Color4f`): a wide colour's own `toSkColor4f`, a legacy one's 8-bit channels as Blink
    /// divides them.
    pub fn paint_color(self) -> [f32; 4] {
        match self.wide {
            Some(w) => w.to_sk_color4f(),
            None => {
                let [r, g, b, a] = self.to_rgba8();
                spec::legacy_color4(r, g, b, a as f32 / 255.0)
            }
        }
    }
}

/// A specified color. `currentColor` cannot be resolved here — it means "whatever `color` computes
/// to on this element", which is only known during the cascade — so it survives as its own variant
/// rather than being flattened to black at parse time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    CurrentColor,
    Rgba(Rgba),
}

impl Color {
    /// Serialize as Blink does (`Color::SerializeAsCSSColor`): a legacy colour as `rgb(r, g, b)` / `rgba(r, g, b,
    /// a)` with its alpha in the fewest decimals (two, else three) that give its 8 bits back -- `rgba(0, 0, 0,
    /// 0.503)` reads back `rgba(0, 0, 0, 0.5)` in Chrome 150 -- and any other colour in its own notation.
    pub fn serialize(&self) -> String {
        let c = match self {
            // A `currentColor` that reaches serialization un-resolved is a caller bug, but the
            // CSSOM has no way to say "no value", so it takes the initial `color`.
            Color::CurrentColor => return "currentcolor".to_string(),
            Color::Rgba(c) => *c,
        };
        match c.wide {
            Some(w) => w.serialize_as_css_color(),
            None => crate::css_color::Color::from_legacy(c.r, c.g, c.b, c.a.clamp(0.0, 1.0)).serialize_as_css_color(),
        }
    }

    /// Parse a full color value from a string (the CSSOM/canvas entry point).
    pub fn parse_str(s: &str) -> Res<Color> {
        let mut pi = cssparser::ParserInput::new(s);
        let mut p = Parser::new(&mut pi);
        let c = Color::parse(&mut p)?;
        p.expect_exhausted().map_err(|_| ())?;
        Ok(c)
    }

    pub fn parse<'i>(input: &mut Parser<'i, '_>) -> Res<Color> {
        let token = input.next().map_err(|_| ())?.clone();
        match token {
            Token::Hash(ref v) | Token::IDHash(ref v) => {
                let c = crate::css_color::parse_hex(v.as_bytes()).ok_or(())?;
                Ok(Color::Rgba(Rgba::from_css(c)))
            }
            Token::Ident(ref name) => named(name),
            Token::Function(ref name) => {
                let name = name.to_ascii_lowercase();
                input
                    .parse_nested_block(|arg| -> Result<Color, cssparser::ParseError<'i, ()>> {
                        parse_color_function(&name, arg).map_err(|_| arg.new_custom_error(()))
                    })
                    .map_err(|_| ())
            }
            _ => Err(()),
        }
    }
}

/// A color keyword. `transparent` is `rgba(0,0,0,0)` and not in the named table; every other name
/// comes from cssparser's table, which is the full CSS list rather than the seven the canvas knew.
fn named(name: &str) -> Res<Color> {
    let lower = name.to_ascii_lowercase();
    if lower == "currentcolor" {
        return Ok(Color::CurrentColor);
    }
    if lower == "transparent" {
        return Ok(Color::Rgba(Rgba::TRANSPARENT));
    }
    if let Some(c) = system_color(&lower) {
        return Ok(Color::Rgba(c));
    }
    let (r, g, b) = cssparser::color::parse_named_color(&lower)?;
    Ok(Color::Rgba(Rgba::opaque(r, g, b)))
}

/// The CSS system colors, resolved as Chrome resolves them: `LayoutTheme`'s colour through the document's colour
/// provider, light scheme ([`crate::css_color::css_system_color`], measured on stock Chrome 150).
///
/// These are not a nicety. The user-agent stylesheet is written in them -- `input` gets
/// `color: FieldText`, `fieldset` gets a `ThreeDFace` border -- so without them those declarations
/// do not parse, the `border` shorthand they sit in is rejected whole, and a `<fieldset>` quietly
/// loses its border. They are also directly readable: `getComputedStyle(input).color` is one of
/// them, resolved, and Chrome answers with the colour, never the keyword.
fn system_color(name: &str) -> Option<Rgba> {
    crate::css_color::css_system_color(name).map(Rgba::from_css)
}

/// A colour function's arguments: `light-dark()` here, the colour functions Blink parses (`rgb()` ... `color()`)
/// through [`crate::css_color`].
fn parse_color_function(name: &str, input: &mut Parser) -> Res<Color> {
    match name {
        // `light-dark(a, b)` picks by the user's colour-scheme preference, which is in the media
        // context and not reachable from here. v1 takes the light branch: the persona's default,
        // and the only branch the user-agent stylesheet's one use of it needs.
        "light-dark" => {
            let light = Color::parse(input)?;
            input.expect_comma().map_err(|_| ())?;
            let _dark = Color::parse(input)?;
            Ok(light)
        }
        // A function resolved where it would need the current colour against black: what is VALID does not depend
        // on it, and a computed value resolves against the element's own ([`resolve_color`]).
        _ => crate::css_color::function_value(name, input)
            .map(|v| Color::Rgba(Rgba::from_css(v.resolve(&crate::css_color::Context::css()))))
            .ok_or(()),
    }
}

/// A specified colour resolved where `currentcolor` is `current` (`StyleColor::Resolve`): `currentcolor` itself,
/// and a `color-mix()` / relative colour / `contrast-color()` that names it.
pub fn resolve_color(specified: &str, current: Rgba) -> Option<Rgba> {
    let v = crate::css_color::parse_color_value(specified)?;
    let cx = crate::css_color::Context { current: current.to_css(), element: true };
    Some(match v {
        crate::css_color::ColorValue::CurrentColor => current,
        v => Rgba::from_css(v.resolve(&cx)),
    })
}

// ---------------------------------------------------------------------------------------------
// Lengths
// ---------------------------------------------------------------------------------------------

/// A length unit that cannot be resolved without knowing something about the element.
///
/// The absolute units (`px`, `in`, `cm`, `mm`, `q`, `pt`, `pc`) are deliberately absent: they are
/// fixed ratios to `px`, so they fold into `Px` at parse time and never need to be carried. What is
/// left is exactly the set that depends on context — the element's font, the root's font, or the
/// viewport — and that set is small enough to fit in a byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthUnit {
    /// Already in CSS pixels — every absolute unit arrives here.
    Px,
    /// The element's own `font-size`.
    Em,
    /// The font's x-height.
    Ex,
    /// The advance width of "0" in the element's font.
    Ch,
    /// The root element's `font-size`.
    Rem,
    Vw,
    Vh,
    Vmin,
    Vmax,
    /// Container query lengths (CSS Containment 3 §8.1): a percent of the query
    /// container's content box -- or, with no eligible container, of the viewport.
    Cqw,
    Cqh,
    Cqmin,
    Cqmax,
}

/// A specified length: a number and the unit it was written in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub value: f32,
    pub unit: LengthUnit,
}

impl Length {
    pub const ZERO: Length = Length { value: 0.0, unit: LengthUnit::Px };

    pub const fn px(value: f32) -> Length {
        Length { value, unit: LengthUnit::Px }
    }

    /// Resolve to CSS pixels.
    pub fn to_px(self, ctx: &ResolveContext) -> f32 {
        let scale = match self.unit {
            LengthUnit::Px => 1.0,
            LengthUnit::Em => ctx.font_size,
            LengthUnit::Ex => ctx.x_height,
            LengthUnit::Ch => ctx.zero_advance,
            LengthUnit::Rem => ctx.root_font_size,
            LengthUnit::Vw => ctx.viewport_width / 100.0,
            LengthUnit::Vh => ctx.viewport_height / 100.0,
            LengthUnit::Vmin => ctx.viewport_width.min(ctx.viewport_height) / 100.0,
            LengthUnit::Vmax => ctx.viewport_width.max(ctx.viewport_height) / 100.0,
            LengthUnit::Cqw => ctx.container.0 / 100.0,
            LengthUnit::Cqh => ctx.container.1 / 100.0,
            LengthUnit::Cqmin => ctx.container.0.min(ctx.container.1) / 100.0,
            LengthUnit::Cqmax => ctx.container.0.max(ctx.container.1) / 100.0,
        };
        self.value * scale
    }

    pub fn parse(input: &mut Parser) -> Res<Length> {
        match input.next().map_err(|_| ())? {
            // A bare `0` is a length in CSS, and only `0` is — `margin: 5` is invalid.
            Token::Number { value, .. } if *value == 0.0 => Ok(Length::ZERO),
            Token::Dimension { value, unit, .. } => length_from(*value, unit),
            _ => Err(()),
        }
    }
}

/// One number and its unit. Absolute units are converted here, once, using the ratios from CSS
/// Values 4 §5.2 — `1in` is 96px *by definition* in CSS, so none of these depend on a real display.
pub(crate) fn length_from(value: f32, unit: &str) -> Res<Length> {
    const IN: f32 = 96.0;
    let (scale, unit) = match unit.to_ascii_lowercase().as_str() {
        "px" => (1.0, LengthUnit::Px),
        "in" => (IN, LengthUnit::Px),
        "cm" => (IN / 2.54, LengthUnit::Px),
        "mm" => (IN / 25.4, LengthUnit::Px),
        "q" => (IN / 101.6, LengthUnit::Px), // a quarter-millimetre
        "pt" => (IN / 72.0, LengthUnit::Px),
        "pc" => (IN / 6.0, LengthUnit::Px),
        "em" => (1.0, LengthUnit::Em),
        "ex" => (1.0, LengthUnit::Ex),
        "ch" => (1.0, LengthUnit::Ch),
        "rem" => (1.0, LengthUnit::Rem),
        "vw" => (1.0, LengthUnit::Vw),
        "vh" => (1.0, LengthUnit::Vh),
        "vmin" => (1.0, LengthUnit::Vmin),
        "vmax" => (1.0, LengthUnit::Vmax),
        // `cqi`/`cqb` are the inline and block axes, which in `horizontal-tb` -- the one writing mode
        // this engine lays out -- are `cqw`/`cqh`.
        "cqw" | "cqi" => (1.0, LengthUnit::Cqw),
        "cqh" | "cqb" => (1.0, LengthUnit::Cqh),
        "cqmin" => (1.0, LengthUnit::Cqmin),
        "cqmax" => (1.0, LengthUnit::Cqmax),
        _ => return Err(()),
    };
    Ok(Length { value: value * scale, unit })
}

/// What a relative unit needs in order to become pixels.
///
/// `x_height` and `zero_advance` are here rather than derived on the spot because they are font
/// metrics, and the font is not this file's business — P3's `FontProvider` fills them in. Until
/// then they carry Blink's own fallback for a font whose metrics are unavailable: half the font
/// size. That is a real approximation and it is worth naming, because `ex` and `ch` are exactly the
/// units a detector would use to cross-check text measurement against layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolveContext {
    pub font_size: f32,
    pub root_font_size: f32,
    pub x_height: f32,
    pub zero_advance: f32,
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// The query container's content-box size for `cq*` units: the viewport when none is eligible.
    pub container: (f32, f32),
}

impl ResolveContext {
    /// The context for an element whose font metrics are not yet known: `ex` and `ch` fall back to
    /// half the font size, as Blink does.
    pub fn new(font_size: f32, root_font_size: f32, viewport_width: f32, viewport_height: f32) -> Self {
        ResolveContext {
            font_size,
            root_font_size,
            x_height: font_size * 0.5,
            zero_advance: font_size * 0.5,
            viewport_width,
            viewport_height,
            container: (viewport_width, viewport_height),
        }
    }
}

impl Default for ResolveContext {
    /// The initial state: a 16px root font in a 1024x768 viewport — the same medium `<html>` default
    /// Chrome starts from, so a stylesheet that resolves against this without ever being attached to
    /// a document gets the numbers a page would expect.
    fn default() -> Self {
        ResolveContext::new(16.0, 16.0, 1024.0, 768.0)
    }
}

/// A `<length-percentage>`: the two are one grammar in CSS and one type here, because a percentage
/// resolves against a basis the *property* supplies (`width` against the containing block's width,
/// `line-height` against the font size), which is not known at parse time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthPercentage {
    Length(Length),
    /// Stored as written: `50%` is `50.0`.
    Percentage(f32),
}

impl LengthPercentage {
    pub const ZERO: LengthPercentage = LengthPercentage::Length(Length::ZERO);

    pub fn parse(input: &mut Parser) -> Res<LengthPercentage> {
        match input.next().map_err(|_| ())? {
            Token::Percentage { unit_value, .. } => Ok(LengthPercentage::Percentage(unit_value * 100.0)),
            Token::Number { value, .. } if *value == 0.0 => Ok(LengthPercentage::ZERO),
            Token::Dimension { value, unit, .. } => length_from(*value, unit).map(LengthPercentage::Length),
            _ => Err(()),
        }
    }

    /// Resolve to pixels against a basis. `basis` is what 100% means for this property.
    pub fn to_px(self, ctx: &ResolveContext, basis: f32) -> f32 {
        match self {
            LengthPercentage::Length(l) => l.to_px(ctx),
            LengthPercentage::Percentage(p) => p / 100.0 * basis,
        }
    }
}

/// Serialize a CSS number the way the CSSOM does: shortest round-trip, and no trailing `.0`.
/// `Display` for `f32` already gives the shortest string that parses back to the same float, so the
/// only thing left is that CSS writes `0`, never `0.0`.
pub fn serialize_number(v: f32) -> String {
    serialize_number_f64(v as f64)
}

/// A CSS number as Blink writes it (`String::Number(value, 6)`): six significant digits, trailing zeros
/// dropped, and the exponent form C's `%g` uses below 1e-4 and from 1e6 -- two exponent digits at least.
/// Chrome 150 (`tools/realweb/seed/layoutunit`, and the computed `transform`/`opacity` of
/// 2026-10-08): 9.921875 -> `9.92188`, 12345.671875 -> `12345.7`, 0.000123456 -> `0.000123456`,
/// 0.00001 -> `1e-05`, 1234567 -> `1.23457e+06`, 1e8 -> `1e+08`, -7.75795e-05 in a `matrix3d()`.
pub fn serialize_number_f64(v: f64) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let sci = format!("{:.5e}", v);
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let trim = |t: &str| if t.contains('.') { t.trim_end_matches('0').trim_end_matches('.').to_string() } else { t.to_string() };
    let text = if !(-4..6).contains(&exponent) {
        format!("{}e{}{:02}", trim(mantissa), if exponent < 0 { '-' } else { '+' }, exponent.abs())
    } else {
        trim(&format!("{:.*}", (5 - exponent) as usize, v))
    };
    if text == "-0" { "0".to_string() } else { text }
}

/// A computed length as `getComputedStyle` gives it back: always in pixels, always with the unit.
pub fn serialize_px(v: f32) -> String {
    format!("{}px", serialize_number(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(s: &str) -> Option<Rgba> {
        match Color::parse_str(s) {
            Ok(Color::Rgba(c)) => Some(c),
            _ => None,
        }
    }
    fn ser(s: &str) -> Option<String> {
        Color::parse_str(s).ok().map(|c| c.serialize())
    }

    #[test]
    fn hex_in_every_length() {
        assert_eq!(rgba("#f00"), Some(Rgba::opaque(255, 0, 0)));
        assert_eq!(rgba("#ff0000"), Some(Rgba::opaque(255, 0, 0)));
        // The 4- and 8-digit forms carry alpha; #0000 is fully transparent black.
        assert_eq!(rgba("#0000"), Some(Rgba::TRANSPARENT));
        assert_eq!(rgba("#ff000080").map(|c| c.to_rgba8()), Some([255, 0, 0, 128]));
        assert_eq!(rgba("#ff000"), None, "five digits is not a hex color");
    }

    // The canvas parser knew seven names. This is the reason it gets deleted.
    #[test]
    fn the_full_named_table_not_the_seven_the_canvas_knew() {
        assert_eq!(rgba("rebeccapurple"), Some(Rgba::opaque(102, 51, 153)));
        assert_eq!(rgba("cornflowerblue"), Some(Rgba::opaque(100, 149, 237)));
        assert_eq!(rgba("MidnightBlue"), Some(Rgba::opaque(25, 25, 112)), "names are ASCII-insensitive");
        assert_eq!(rgba("transparent"), Some(Rgba::TRANSPARENT));
        assert_eq!(Color::parse_str("currentColor"), Ok(Color::CurrentColor));
        assert_eq!(rgba("notacolor"), None);
    }

    // The user-agent stylesheet is written in these, so without them `input { color: FieldText }`
    // does not parse and `fieldset { border: groove 2px ThreeDFace }` is rejected whole.
    #[test]
    fn the_system_colors_resolve_rather_than_passing_the_keyword_through() {
        assert_eq!(rgba("FieldText"), Some(Rgba::BLACK));
        assert_eq!(rgba("Field"), Some(Rgba::opaque(255, 255, 255)));
        assert_eq!(rgba("ThreeDFace"), Some(Rgba::opaque(239, 239, 239)));
        assert_eq!(rgba("buttonface"), Some(Rgba::opaque(239, 239, 239)), "and case-insensitively");
        assert_eq!(rgba("GrayText"), Some(Rgba::opaque(128, 128, 128)));
        assert_eq!(ser("CanvasText").as_deref(), Some("rgb(0, 0, 0)"), "never the keyword");
        // The deprecated names are aliases, per CSS Color 4 6.2 -- and pages still use them.
        assert_eq!(rgba("Menu"), Some(Rgba::opaque(255, 255, 255)));
        assert_eq!(rgba("MenuText"), Some(Rgba::BLACK));
        // Measured, stock Chrome 150 `getComputedStyle`: the border/shadow names through the
        // document's colour provider are black, not ButtonBorder's grey.
        assert_eq!(rgba("WindowFrame"), Some(Rgba::BLACK));
        assert_eq!(rgba("ButtonBorder"), Some(Rgba::BLACK));
        assert_eq!(rgba("SelectedItem"), Some(Rgba::opaque(8, 96, 242)));
        assert_eq!(rgba("text"), None, "not a system colour");
        assert_eq!(rgba("-webkit-link"), Some(Rgba::opaque(0, 0, 238)));
    }

    #[test]
    fn light_dark_takes_the_light_branch() {
        assert_eq!(rgba("light-dark(#767676, #858585)"), Some(Rgba::opaque(0x76, 0x76, 0x76)));
        assert_eq!(rgba("light-dark(red)"), None, "it needs both branches");
    }

    #[test]
    fn rgb_legacy_and_modern_are_the_same_color() {
        let red = Some(Rgba::opaque(255, 0, 0));
        assert_eq!(rgba("rgb(255, 0, 0)"), red);
        assert_eq!(rgba("rgb(255 0 0)"), red);
        assert_eq!(rgba("rgba(255, 0, 0)"), red, "rgba() with no alpha is rgb()");
        assert_eq!(rgba("rgb(100%, 0%, 0%)"), red);
        assert_eq!(rgba("rgb(255 0 0 / 1)"), red);
    }

    #[test]
    fn alpha_arrives_by_comma_or_slash_and_as_number_or_percent() {
        // `rgb()` keeps its alpha in 8 bits, as Blink's parser does (`round(a * 255) / 255`).
        let half = Rgba { r: 0, g: 0, b: 0, a: 128.0 / 255.0, wide: None };
        assert_eq!(rgba("rgba(0, 0, 0, 0.5)"), Some(half));
        assert_eq!(rgba("rgb(0 0 0 / 0.5)"), Some(half));
        assert_eq!(rgba("rgb(0 0 0 / 50%)"), Some(half));
        assert_eq!(rgba("rgba(0 0 0 / 50%)"), Some(half));
    }

    // Chrome rejects this; accepting it would be a divergence a page can measure.
    #[test]
    fn legacy_rgb_may_not_mix_numbers_and_percentages() {
        assert_eq!(rgba("rgb(255, 0%, 0)"), None);
        assert_eq!(rgba("rgb(100%, 0, 0%)"), None);
        assert_eq!(rgba("rgb(255 0% 0)"), Some(Rgba::opaque(255, 0, 0)), "the modern syntax allows it");
        // And each mixed component keeps its own scale: 100 stays 100, it is not read as 100%.
        assert_eq!(rgba("rgb(100% 100 0)"), Some(Rgba::opaque(255, 100, 0)));
    }

    #[test]
    fn out_of_range_components_clamp_rather_than_fail() {
        assert_eq!(rgba("rgb(300, -20, 0)"), Some(Rgba::opaque(255, 0, 0)));
        assert_eq!(rgba("rgba(0, 0, 0, 2)").map(|c| c.a), Some(1.0));
        assert_eq!(rgba("rgba(0, 0, 0, -1)").map(|c| c.a), Some(0.0));
    }

    #[test]
    fn hsl_hits_the_primaries_and_the_grays() {
        assert_eq!(rgba("hsl(0, 100%, 50%)"), Some(Rgba::opaque(255, 0, 0)));
        assert_eq!(rgba("hsl(120, 100%, 50%)"), Some(Rgba::opaque(0, 255, 0)));
        assert_eq!(rgba("hsl(240, 100%, 50%)"), Some(Rgba::opaque(0, 0, 255)));
        assert_eq!(rgba("hsl(0, 0%, 100%)"), Some(Rgba::opaque(255, 255, 255)));
        // Saturation 0 is gray at any hue at all.
        assert_eq!(rgba("hsl(999, 0%, 50%)"), Some(Rgba::opaque(128, 128, 128)));
    }

    #[test]
    fn hue_accepts_every_angle_unit_and_wraps() {
        let green = Some(Rgba::opaque(0, 255, 0));
        assert_eq!(rgba("hsl(120deg 100% 50%)"), green);
        assert_eq!(rgba("hsl(-240deg, 100%, 50%)"), green, "negative hue wraps forward");
        assert_eq!(rgba("hsl(480, 100%, 50%)"), green, "so does a hue past 360");
        assert_eq!(rgba("hsl(0.3333333turn 100% 50%)"), green);
        assert_eq!(rgba("hsl(133.333grad 100% 50%)"), green);
        assert_eq!(rgba("hsl(120px 100% 50%)"), None, "px is not an angle");
    }

    // CSSOM: opaque serializes as rgb(), translucent as rgba(), and the alpha in the fewest decimals (two, else
    // three) that give its 8 bits back -- Blink's `SerializeLegacyColorAsCSSColor`; stock Chrome 150 reads
    // `rgba(0, 0, 0, 0.503)` back as `rgba(0, 0, 0, 0.5)` and `0.123` as `0.12`.
    #[test]
    fn serialization_matches_the_cssom_form() {
        assert_eq!(ser("#f00").as_deref(), Some("rgb(255, 0, 0)"));
        assert_eq!(ser("red").as_deref(), Some("rgb(255, 0, 0)"));
        assert_eq!(ser("rgba(1, 2, 3, 0.5)").as_deref(), Some("rgba(1, 2, 3, 0.5)"));
        assert_eq!(ser("rgba(0, 0, 0, 0.503)").as_deref(), Some("rgba(0, 0, 0, 0.5)"));
        assert_eq!(ser("rgba(0, 0, 0, 0.123)").as_deref(), Some("rgba(0, 0, 0, 0.12)"));
        assert_eq!(ser("hsla(120, 50%, 50%, 0.503)").as_deref(), Some("rgba(64, 191, 64, 0.5)"));
        assert_eq!(ser("lab(50 40 30)").as_deref(), Some("lab(50 40 30)"));
        assert_eq!(ser("color(display-p3 1 0.25 0.5 / 0.5)").as_deref(), Some("color(display-p3 1 0.25 0.5 / 0.5)"));
        assert_eq!(ser("hwb(120 20% 30%)").as_deref(), Some("rgb(51, 179, 51)"));
        assert_eq!(ser("color-mix(in srgb, red, blue)").as_deref(), Some("color(srgb 0.5 0 0.5)"));
        assert_eq!(ser("rgb(from red r g b)").as_deref(), Some("color(srgb 1 0 0)"));
        assert_eq!(ser("transparent").as_deref(), Some("rgba(0, 0, 0, 0)"));
        assert_eq!(ser("hsl(120, 100%, 50%)").as_deref(), Some("rgb(0, 255, 0)"));
    }


    // ---- lengths ----

    fn len(s: &str) -> Option<Length> {
        let mut pi = cssparser::ParserInput::new(s);
        let mut p = Parser::new(&mut pi);
        let v = Length::parse(&mut p).ok()?;
        p.expect_exhausted().ok()?;
        Some(v)
    }
    fn lp(s: &str) -> Option<LengthPercentage> {
        let mut pi = cssparser::ParserInput::new(s);
        let mut p = Parser::new(&mut pi);
        let v = LengthPercentage::parse(&mut p).ok()?;
        p.expect_exhausted().ok()?;
        Some(v)
    }

    // CSS defines 1in as 96px outright, so none of these depend on a real display, and all of them
    // can fold at parse time instead of being carried around as units.
    #[test]
    fn absolute_units_fold_to_pixels_at_parse_time() {
        assert_eq!(len("1in"), Some(Length::px(96.0)));
        assert_eq!(len("1pc"), Some(Length::px(16.0)));
        assert_eq!(len("72pt"), Some(Length::px(96.0)));
        assert_eq!(len("2.54cm"), Some(Length::px(96.0)));
        assert_eq!(len("25.4mm"), Some(Length::px(96.0)));
        assert_eq!(len("101.6q"), Some(Length::px(96.0)));
        assert_eq!(len("10PX"), Some(Length::px(10.0)), "units are ASCII-insensitive");
    }

    #[test]
    fn only_zero_may_be_written_without_a_unit() {
        assert_eq!(len("0"), Some(Length::ZERO));
        assert_eq!(len("5"), None, "`margin: 5` is not a length");
        assert_eq!(len("10foo"), None);
        assert_eq!(len("10%"), None, "a percentage is not a length");
    }

    #[test]
    fn relative_units_keep_their_unit_until_a_context_exists() {
        assert_eq!(len("2em"), Some(Length { value: 2.0, unit: LengthUnit::Em }));
        let ctx = ResolveContext::new(20.0, 16.0, 800.0, 600.0);
        assert_eq!(len("2em").unwrap().to_px(&ctx), 40.0);
        assert_eq!(len("2rem").unwrap().to_px(&ctx), 32.0, "rem is the root's font, not this one's");
        assert_eq!(len("50vw").unwrap().to_px(&ctx), 400.0);
        assert_eq!(len("50vh").unwrap().to_px(&ctx), 300.0);
        assert_eq!(len("100vmin").unwrap().to_px(&ctx), 600.0);
        assert_eq!(len("100vmax").unwrap().to_px(&ctx), 800.0);
    }

    // Named because it is an approximation that will be replaced, not because it is right: real
    // `ex`/`ch` come from font metrics, and P3's FontProvider is what supplies them.
    #[test]
    fn ex_and_ch_use_blinks_no_metrics_fallback_for_now() {
        let ctx = ResolveContext::new(20.0, 16.0, 800.0, 600.0);
        assert_eq!(len("1ex").unwrap().to_px(&ctx), 10.0);
        assert_eq!(len("1ch").unwrap().to_px(&ctx), 10.0);
    }

    #[test]
    fn a_percentage_needs_the_basis_its_property_supplies() {
        assert_eq!(lp("50%"), Some(LengthPercentage::Percentage(50.0)));
        let ctx = ResolveContext::default();
        // The same 50% is 400px against a 800px containing block and 8px against a 16px font.
        assert_eq!(lp("50%").unwrap().to_px(&ctx, 800.0), 400.0);
        assert_eq!(lp("50%").unwrap().to_px(&ctx, 16.0), 8.0);
        assert_eq!(lp("10px").unwrap().to_px(&ctx, 800.0), 10.0, "a length ignores the basis");
        assert_eq!(lp("0"), Some(LengthPercentage::ZERO));
    }

    #[test]
    fn the_default_context_is_chromes_starting_page() {
        let ctx = ResolveContext::default();
        assert_eq!(ctx.font_size, 16.0);
        assert_eq!(ctx.root_font_size, 16.0);
        assert_eq!(len("1em").unwrap().to_px(&ctx), 16.0);
    }

    // CSS writes `0`, not `0.0`, and never pads. Both matter: these strings go straight out of
    // `getComputedStyle`, where an extra digit is a difference a page can read.
    #[test]
    fn numbers_serialize_the_way_the_cssom_writes_them() {
        assert_eq!(serialize_px(0.0), "0px");
        assert_eq!(serialize_px(16.0), "16px");
        assert_eq!(serialize_px(13.5), "13.5px");
        assert_eq!(serialize_px(-3.0), "-3px");
        assert_eq!(serialize_number(2.0), "2");
        assert_eq!(serialize_number(0.5), "0.5");
    }

    #[test]
    fn trailing_junk_is_not_a_color() {
        assert_eq!(rgba("red red"), None);
        assert_eq!(rgba("rgb(0, 0, 0) 1px"), None);
        assert_eq!(rgba(""), None);
        assert_eq!(rgba("rgb(0, 0)"), None, "too few components");
    }
}
