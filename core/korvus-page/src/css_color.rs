//! CSS colours as Blink keeps them -- `blink::Color` (platform/graphics/color.cc, Chromium 150) -- and the way a
//! canvas parses, paints and reads one back.
//!
//! The cascade's [`crate::value::Color`] is an 8-bit sRGB projection. Blink keeps a colour as three float parameters
//! in the space it was written in -- the full CSS parser keeps `RGB(10.5 ...)`'s `10.5`, `color(display-p3 1 0.25
//! 0.5)` stays display-p3 -- and converts it to extended-sRGB floats only to paint it (`toSkColor4f`, through
//! `ui/gfx/color_conversions.cc` and `skia/ext/skcms_ext.cc`); Skia then takes those floats into the canvas's own
//! colour space. `fillStyle` reads the colour back as written (`SerializeAsCanvasColor`). a bot challenge
//! frame fills four canvases with `color(display-p3 1 0.25 0.5)` and hashes what `getImageData` returns: the engine
//! parsed no `color()`, kept the previous fill, and read back black.
//!
//! Every function is a port, operation for operation -- an `f32` where Blink has a float, an `f64` where it has a
//! double, `powf` where it calls `std::pow` on floats, skcms's approximations where Blink calls skcms. The results
//! are read back as bytes, and a channel near .5 rounds on the last bit: Blink's display-p3 -> sRGB goes through
//! the rounded `SkNamedGamut` matrices and lands green at 0.10602, not the exact 0.10588, which is the difference
//! between Chrome's `[255, 27, 127, 255]` / `[255, 28, 126, 128]` and anything else (stock Chrome 150, measured on
//! float16 canvases, which keep those floats).
//!
//! Not ported yet -- such a string is not a colour here, so assigning one is ignored: `calc()` in a channel,
//! relative colours (`rgb(from ...)`), `color-mix()`.

use cssparser::{Parser, ParserInput, Token};

// ---- the colour -----------------------------------------------------------------------------------------

/// `Color::ColorSpace` (`rec2100-linear` is behind a runtime flag Chrome 150 ships off, so it is not one here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    SrgbLegacy,
    Hsl,
    Hwb,
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    A98Rgb,
    ProPhotoRgb,
    Rec2020,
    XyzD50,
    XyzD65,
    Lab,
    Oklab,
    Lch,
    Oklch,
}

impl Space {
    /// `IsLegacyColorSpace`: `rgb()`, `hsl()`, `hwb()`, hex and named colours.
    pub fn is_legacy(self) -> bool {
        matches!(self, Space::SrgbLegacy | Space::Hsl | Space::Hwb)
    }

    fn lightness_first(self) -> bool {
        matches!(self, Space::Lab | Space::Oklab | Space::Lch | Space::Oklch)
    }

    fn chroma_second(self) -> bool {
        matches!(self, Space::Lch | Space::Oklch)
    }

    /// `ColorChannelIsHue`.
    fn is_hue(self, i: usize) -> bool {
        (matches!(self, Space::Hsl | Space::Hwb) && i == 0) || (self.chroma_second() && i == 2)
    }

    /// `ColorSpaceToString`.
    pub fn name(self) -> &'static str {
        match self {
            Space::Srgb => "srgb",
            Space::SrgbLinear => "srgb-linear",
            Space::DisplayP3 => "display-p3",
            Space::DisplayP3Linear => "display-p3-linear",
            Space::A98Rgb => "a98-rgb",
            Space::ProPhotoRgb => "prophoto-rgb",
            Space::Rec2020 => "rec2020",
            Space::XyzD50 => "xyz-d50",
            Space::XyzD65 => "xyz-d65",
            Space::Lab => "lab",
            Space::Oklab => "oklab",
            Space::Lch => "lch",
            Space::Oklch => "oklch",
            Space::SrgbLegacy => "rgb",
            Space::Hsl => "hsl",
            Space::Hwb => "hwb",
        }
    }

    /// What 100% of each channel is (`ColorFunction::kMetadataMap`); NaN where a percentage is not allowed.
    fn percent_base(self) -> [f64; 3] {
        match self {
            Space::SrgbLegacy => [255.0; 3],
            Space::Lab => [100.0, 125.0, 125.0],
            Space::Oklab => [1.0, 0.4, 0.4],
            Space::Lch => [100.0, 150.0, f64::NAN],
            Space::Oklch => [1.0, 0.4, f64::NAN],
            Space::Hsl | Space::Hwb => [f64::NAN, 100.0, 100.0],
            _ => [1.0; 3],
        }
    }

    /// `ColorSpaceFromColorSpaceArgument`: the space a `color()` names.
    fn from_argument(name: &str) -> Option<Space> {
        Some(match name.to_ascii_lowercase().as_str() {
            "srgb" => Space::Srgb,
            "rec2020" => Space::Rec2020,
            "srgb-linear" => Space::SrgbLinear,
            "display-p3" => Space::DisplayP3,
            "display-p3-linear" => Space::DisplayP3Linear,
            "a98-rgb" => Space::A98Rgb,
            "prophoto-rgb" => Space::ProPhotoRgb,
            "xyz-d50" => Space::XyzD50,
            "xyz" | "xyz-d65" => Space::XyzD65,
            _ => return None,
        })
    }

    /// `GetSkColorSpaceParams`: the gamut and transfer function of an RGB or XYZ space.
    fn sk_params(self) -> (M33, Tf) {
        match self {
            Space::Srgb => (GAMUT_SRGB, TF_SRGB),
            Space::SrgbLinear => (GAMUT_SRGB, TF_LINEAR),
            Space::DisplayP3 => (GAMUT_DISPLAY_P3, TF_SRGB),
            Space::DisplayP3Linear => (GAMUT_DISPLAY_P3, TF_LINEAR),
            Space::A98Rgb => (GAMUT_ADOBE_RGB, TF_2DOT2),
            // `SkNamedPrimaries::kProPhotoRGB.toXYZD50`.
            Space::ProPhotoRgb => (primaries_to_xyz_d50(0.7347, 0.2653, 0.1596, 0.8404, 0.0366, 0.0001, 0.34567, 0.35850), TF_PROPHOTO),
            Space::Rec2020 => (GAMUT_REC2020, TF_REC2020),
            Space::XyzD50 => (IDENTITY, TF_LINEAR),
            Space::XyzD65 => (adapt_to_xyz_d50(0.3127, 0.3290), TF_LINEAR),
            _ => (IDENTITY, TF_LINEAR),
        }
    }
}

/// A colour as Blink holds one: its space, three parameters and alpha, each of which may be `none`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub space: Space,
    pub params: [f32; 3],
    pub alpha: f32,
    pub params_none: [bool; 3],
    pub alpha_none: bool,
}

impl Color {
    /// `Color(RGBA32)`: an ARGB word, as hex and named colours are held.
    pub fn from_rgba32(argb: u32) -> Color {
        Color {
            space: Space::SrgbLegacy,
            params: [((argb >> 16) & 0xff) as f32, ((argb >> 8) & 0xff) as f32, (argb & 0xff) as f32],
            alpha: ((argb >> 24) & 0xff) as f32 / 255.0,
            params_none: [false; 3],
            alpha_none: false,
        }
    }

    /// `Color::FromRGBA`: each channel clamped to [0, 255].
    pub fn from_rgba(r: i32, g: i32, b: i32, a: i32) -> Color {
        let c = |x: i32| x.clamp(0, 255) as u32;
        Color::from_rgba32(c(a) << 24 | c(r) << 16 | c(g) << 8 | c(b))
    }

    pub fn black() -> Color {
        Color::from_rgba32(0xff00_0000)
    }

    /// A legacy colour from 8-bit channels and a float alpha (what the cascade's [`crate::value::Rgba`] holds).
    pub fn from_legacy(r: u8, g: u8, b: u8, a: f32) -> Color {
        Color { space: Space::SrgbLegacy, params: [r as f32, g as f32, b as f32], alpha: a, params_none: [false; 3], alpha_none: false }
    }

    /// The 0-255 channels a legacy colour serializes with (`SerializeLegacyColorAsCSSColor`): `rgb()`'s parameters,
    /// `hsl()`/`hwb()` through sRGB with Blink's epsilon, each clamped and rounded.
    pub fn legacy_rgb8(&self) -> [u8; 3] {
        const EPS: f32 = 1e-7;
        let [mut r, mut g, mut b] = self.params;
        if matches!(self.space, Space::Hsl | Space::Hwb) {
            let [p0, p1, p2] = self.params;
            let c = if self.space == Space::Hsl { hsl_to_srgb(p0, p1, p2) } else { hwb_to_srgb(p0, p1, p2) };
            r = ((c[0] + EPS) as f64 * 255.0) as f32;
            g = ((c[1] + EPS) as f64 * 255.0) as f32;
            b = ((c[2] + EPS) as f64 * 255.0) as f32;
        }
        let ch = |v: f32| clamp_f64(v as f64, 0.0, 255.0).round() as u8;
        [ch(r), ch(g), ch(b)]
    }

    /// `Color::FromColorSpace`: a missing parameter is `none`; alpha is clamped to [0, 1], a lightness to its range,
    /// a chroma to non-negative.
    pub fn from_color_space(space: Space, p0: Option<f32>, p1: Option<f32>, p2: Option<f32>, alpha: Option<f32>) -> Color {
        let mut c = Color {
            space,
            params: [p0.unwrap_or(0.0), p1.unwrap_or(0.0), p2.unwrap_or(0.0)],
            alpha: alpha.map(|a| clamp_f32(a, 0.0, 1.0)).unwrap_or(0.0),
            params_none: [p0.is_none(), p1.is_none(), p2.is_none()],
            alpha_none: alpha.is_none(),
        };
        if space.lightness_first() && !c.params[0].is_nan() {
            let max = if matches!(space, Space::Lab | Space::Lch) { 100.0 } else { 1.0 };
            c.params[0] = min_f(max, max_f(c.params[0], 0.0));
        }
        if space.chroma_second() {
            c.params[1] = max_f(c.params[1], 0.0);
        }
        c
    }

    /// `IsOpaque`.
    pub fn is_opaque(&self) -> bool {
        self.alpha >= 1.0
    }

    /// `ToXYZD50(gamut_map = false)`.
    fn to_xyz_d50(&self) -> V3 {
        let [p0, p1, p2] = self.params;
        match self.space {
            Space::SrgbLegacy => srgb_to_xyz_d50(srgb_legacy_to_srgb(p0, p1, p2)),
            Space::Lab => lab_to_xyz_d50(p0, p1, p2),
            Space::Oklab => oklab_to_xyz_d50(p0, p1, p2),
            Space::Lch => {
                let [l, a, b] = lch_to_lab(p0, p1, p2);
                lab_to_xyz_d50(l, a, b)
            }
            Space::Oklch => {
                let [l, a, b] = lch_to_lab(p0, p1, p2);
                oklab_to_xyz_d50(l, a, b)
            }
            Space::Hsl => srgb_to_xyz_d50(hsl_to_srgb(p0, p1, p2)),
            Space::Hwb => srgb_to_xyz_d50(hwb_to_srgb(p0, p1, p2)),
            _ => {
                let (m, t) = self.space.sk_params();
                m_apply(&m, tf_apply(&t, [p0, p1, p2]))
            }
        }
    }

    /// `ToSRGB(gamut_map = false)`: extended sRGB, unclamped.
    pub fn to_srgb(&self) -> V3 {
        let [p0, p1, p2] = self.params;
        match self.space {
            Space::Srgb => [p0, p1, p2],
            Space::SrgbLegacy => srgb_legacy_to_srgb(p0, p1, p2),
            // "Several SVG rendering tests expect the inaccurate results from this formulation" (color.cc): skcms's
            // own approximate inverse and evaluation, not the precise ones the other spaces go through.
            Space::SrgbLinear => {
                let inv = skcms_invert(&TF_SRGB).unwrap_or(TF_LINEAR);
                [skcms_eval(&inv, p0), skcms_eval(&inv, p1), skcms_eval(&inv, p2)]
            }
            Space::Hsl => hsl_to_srgb(p0, p1, p2),
            Space::Hwb => hwb_to_srgb(p0, p1, p2),
            _ => xyz_d50_to_srgb(self.to_xyz_d50()),
        }
    }

    /// `GetColorInterpolationSpace() == kSRGBLegacy`: a legacy colour with no `none` -- a gradient whose stops are all
    /// such interpolates in sRGB, any other in OKLab (`Gradient::CreateShaderInternal`).
    pub fn interpolates_as_legacy(&self) -> bool {
        self.space.is_legacy() && !self.params_none.iter().any(|n| *n) && !self.alpha_none
    }

    /// The stop colour Skia gets from a gradient Blink interpolates in OKLab: the colour converted to OKLab
    /// (`ConvertToColorSpaceForInterpolation(kOklab)`, missing components as zero) and back to sRGB floats
    /// (`ToGradientStopSkColor4f`).
    pub fn oklab_gradient_stop(&self) -> [f32; 4] {
        let [p0, p1, p2] = self.params;
        let lab = match self.space {
            Space::Oklab => [p0, p1, p2],
            Space::Oklch => lch_to_lab(p0, p1, p2),
            _ => xyz_d50_to_oklab(self.to_xyz_d50()),
        };
        let c = Color { space: Space::Oklab, params: lab, alpha: self.alpha, params_none: [false; 3], alpha_none: self.alpha_none };
        c.to_sk_color4f()
    }

    /// `toSkColor4f()`: what the colour paints with -- unpremultiplied extended sRGB, alpha last (Chrome 150 ships
    /// `kBakedGamutMapping` off, so nothing is gamut-mapped here; Skia clamps when it stores).
    pub fn to_sk_color4f(&self) -> [f32; 4] {
        let [r, g, b] = self.to_srgb();
        [r, g, b, self.alpha]
    }

    /// `Red()` / `Green()` / `Blue()`: `toSkColor4f().toSkColor()`, each channel `c * 255 + 0.5` pinned to [0, 255]
    /// and truncated.
    pub fn rgb8(&self) -> [u8; 3] {
        let c = self.to_sk_color4f();
        let ch = |v: f32| -> u8 {
            let x = v * 255.0 + 0.5;
            let m = if 255.0 < x { 255.0 } else { x };
            (if 0.0 < m { m } else { 0.0 }) as u8
        };
        [ch(c[0]), ch(c[1]), ch(c[2])]
    }

    /// `SerializeAsCanvasColor`: an opaque legacy colour as `#rrggbb`, anything else as CSS serializes it.
    pub fn serialize_as_canvas_color(&self) -> String {
        if self.is_opaque() && self.space.is_legacy() {
            let [r, g, b] = self.rgb8();
            return format!("#{r:02x}{g:02x}{b:02x}");
        }
        self.serialize_as_css_color()
    }

    /// `SerializeAsCSSColor`.
    pub fn serialize_as_css_color(&self) -> String {
        if self.space.is_legacy() {
            self.serialize_legacy()
        } else {
            self.serialize_internal()
        }
    }

    /// `SerializeLegacyColorAsCSSColor`: `rgb(r, g, b)` / `rgba(r, g, b, a)`, the alpha with two decimals when
    /// they give back the same 8-bit alpha, else three.
    fn serialize_legacy(&self) -> String {
        let mut out = String::from(if self.is_opaque() && self.alpha.is_finite() { "rgb(" } else { "rgba(" });
        const EPS: f32 = 1e-7;
        // `SRGBToSRGBLegacy(r + kEpsilon, ...)` for hsl()/hwb(): "channels of exactly 0.5 can get incorrectly
        // rounded down".
        let [r, g, b] = self.legacy_rgb8();
        out += &format!("{r}, {g}, {b}");
        if !self.is_opaque() {
            out += ", ";
            let int_alpha = clamp_f64((((self.alpha + EPS) as f64) * 255.0).round(), 0.0, 255.0) as i32;
            let two = ((int_alpha as f64 * 100.0 / 255.0).round() / 100.0) as f32;
            if (two * 255.0).round() == int_alpha as f32 {
                out += &param_to_string(two as f64, 2);
            } else {
                let three = ((int_alpha as f64 * 1000.0 / 255.0).round() / 1000.0) as f32;
                out += &param_to_string(three as f64, 3);
            }
        }
        out.push(')');
        out
    }

    /// `SerializeInternal`: `lab(50 40 30)`, `color(display-p3 1 0.25 0.5 / 0.5)`.
    fn serialize_internal(&self) -> String {
        let mut out = if self.space.lightness_first() {
            format!("{}(", self.space.name())
        } else {
            format!("color({} ", self.space.name())
        };
        for i in 0..3 {
            if i > 0 {
                out.push(' ');
            }
            out += &if self.params_none[i] { "none".to_string() } else { param_to_string(self.params[i] as f64, 6) };
        }
        if self.alpha != 1.0 || self.alpha_none {
            out += " / ";
            out += &if self.alpha_none { "none".to_string() } else { to_precision(self.alpha as f64, 6) };
        }
        out.push(')');
        out
    }
}

// ---- parsing ------------------------------------------------------------------------------------------

/// What a canvas makes of a colour string.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasParse {
    Color(Color),
    /// `currentcolor`, which the caller resolves (Blink: a connected `<canvas>`'s inline `color`, else black).
    CurrentColor,
    /// A colour function to resolve against the current colour.
    Function(ColorValue),
}

/// `ParseCanvasColorString` (canvas_style.cc): HTML whitespace stripped, `currentcolor`, then
/// `CSSParser::ParseColor`, then a system colour. `element` = the canvas is a `<canvas>` element: Blink resolves a
/// system colour through the document's colour provider, which an `OffscreenCanvas` has none of, and the two
/// answer differently.
pub fn parse_canvas_color(text: &str, element: bool) -> Option<CanvasParse> {
    let s = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r'));
    if s.eq_ignore_ascii_case("currentcolor") {
        return Some(CanvasParse::CurrentColor);
    }
    if let Some(c) = parse_css_color(s) {
        return Some(CanvasParse::Color(c));
    }
    if let Some(c) = system_color(s, element) {
        return Some(CanvasParse::Color(c));
    }
    // A colour function `CSSParser::ParseColor` leaves unresolved -- `color-mix()`, a relative colour, `calc()` in
    // `lab()` -- resolved against the context's current colour (`kColorFunction`).
    match parse_color_value(s)? {
        v @ (ColorValue::Mix(_) | ColorValue::Relative(_)) => Some(CanvasParse::Function(v)),
        ColorValue::Color(c, calc) if calc.iter().any(|x| *x) => Some(CanvasParse::Color(c)),
        _ => None,
    }
}

/// `CSSParser::ParseColor`: a named colour from Blink's own table (which has three names CSS does not:
/// `transparent`, `violetred`, `lightslateblue`), else the fast path for an 8-bit string (hex, `rgb()`/`rgba()`,
/// `hsl()`/`hsla()` written as most pages write them), else the full parser. A keyword (`currentcolor`, a system
/// colour) is not a colour to it.
pub fn parse_css_color(s: &str) -> Option<Color> {
    if s.is_empty() {
        return None;
    }
    if let Some(c) = named_color(s) {
        return Some(c);
    }
    if s.chars().all(|c| (c as u32) <= 0xff) {
        let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
        if let Some(c) = fast_parse(&bytes) {
            return Some(c);
        }
    }
    full_parse(s)
}

/// `FindNamedColor`: ASCII, case-insensitive, at most 63 characters.
fn named_color(s: &str) -> Option<Color> {
    if s.len() > 63 || !s.bytes().all(|b| b != 0 && b < 0x80) {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    NAMED.iter().find(|(n, _)| *n == lower).map(|(_, argb)| Color::from_rgba32(*argb))
}

/// Blink's named colours (`platform/color_data.gperf`, Chromium 150).
const NAMED: [(&str, u32); 151] = [
    ("aliceblue", 0xfff0f8ff), ("antiquewhite", 0xfffaebd7), ("aqua", 0xff00ffff), ("aquamarine", 0xff7fffd4),
    ("azure", 0xfff0ffff), ("beige", 0xfff5f5dc), ("bisque", 0xffffe4c4), ("black", 0xff000000),
    ("blanchedalmond", 0xffffebcd), ("blue", 0xff0000ff), ("blueviolet", 0xff8a2be2), ("brown", 0xffa52a2a),
    ("burlywood", 0xffdeb887), ("cadetblue", 0xff5f9ea0), ("chartreuse", 0xff7fff00), ("chocolate", 0xffd2691e),
    ("coral", 0xffff7f50), ("cornflowerblue", 0xff6495ed), ("cornsilk", 0xfffff8dc), ("crimson", 0xffdc143c),
    ("cyan", 0xff00ffff), ("darkblue", 0xff00008b), ("darkcyan", 0xff008b8b), ("darkgoldenrod", 0xffb8860b),
    ("darkgray", 0xffa9a9a9), ("darkgrey", 0xffa9a9a9), ("darkgreen", 0xff006400), ("darkkhaki", 0xffbdb76b),
    ("darkmagenta", 0xff8b008b), ("darkolivegreen", 0xff556b2f), ("darkorange", 0xffff8c00),
    ("darkorchid", 0xff9932cc), ("darkred", 0xff8b0000), ("darksalmon", 0xffe9967a), ("darkseagreen", 0xff8fbc8f),
    ("darkslateblue", 0xff483d8b), ("darkslategray", 0xff2f4f4f), ("darkslategrey", 0xff2f4f4f),
    ("darkturquoise", 0xff00ced1), ("darkviolet", 0xff9400d3), ("deeppink", 0xffff1493), ("deepskyblue", 0xff00bfff),
    ("dimgray", 0xff696969), ("dimgrey", 0xff696969), ("dodgerblue", 0xff1e90ff), ("firebrick", 0xffb22222),
    ("floralwhite", 0xfffffaf0), ("forestgreen", 0xff228b22), ("fuchsia", 0xffff00ff), ("gainsboro", 0xffdcdcdc),
    ("ghostwhite", 0xfff8f8ff), ("gold", 0xffffd700), ("goldenrod", 0xffdaa520), ("gray", 0xff808080),
    ("grey", 0xff808080), ("green", 0xff008000), ("greenyellow", 0xffadff2f), ("honeydew", 0xfff0fff0),
    ("hotpink", 0xffff69b4), ("indianred", 0xffcd5c5c), ("indigo", 0xff4b0082), ("ivory", 0xfffffff0),
    ("khaki", 0xfff0e68c), ("lavender", 0xffe6e6fa), ("lavenderblush", 0xfffff0f5), ("lawngreen", 0xff7cfc00),
    ("lemonchiffon", 0xfffffacd), ("lightblue", 0xffadd8e6), ("lightcoral", 0xfff08080), ("lightcyan", 0xffe0ffff),
    ("lightgoldenrodyellow", 0xfffafad2), ("lightgray", 0xffd3d3d3), ("lightgrey", 0xffd3d3d3),
    ("lightgreen", 0xff90ee90), ("lightpink", 0xffffb6c1), ("lightsalmon", 0xffffa07a), ("lightseagreen", 0xff20b2aa),
    ("lightskyblue", 0xff87cefa), ("lightslateblue", 0xff8470ff), ("lightslategray", 0xff778899),
    ("lightslategrey", 0xff778899), ("lightsteelblue", 0xffb0c4de), ("lightyellow", 0xffffffe0), ("lime", 0xff00ff00),
    ("limegreen", 0xff32cd32), ("linen", 0xfffaf0e6), ("magenta", 0xffff00ff), ("maroon", 0xff800000),
    ("mediumaquamarine", 0xff66cdaa), ("mediumblue", 0xff0000cd), ("mediumorchid", 0xffba55d3),
    ("mediumpurple", 0xff9370db), ("mediumseagreen", 0xff3cb371), ("mediumslateblue", 0xff7b68ee),
    ("mediumspringgreen", 0xff00fa9a), ("mediumturquoise", 0xff48d1cc), ("mediumvioletred", 0xffc71585),
    ("midnightblue", 0xff191970), ("mintcream", 0xfff5fffa), ("mistyrose", 0xffffe4e1), ("moccasin", 0xffffe4b5),
    ("navajowhite", 0xffffdead), ("navy", 0xff000080), ("oldlace", 0xfffdf5e6), ("olive", 0xff808000),
    ("olivedrab", 0xff6b8e23), ("orange", 0xffffa500), ("orangered", 0xffff4500), ("orchid", 0xffda70d6),
    ("palegoldenrod", 0xffeee8aa), ("palegreen", 0xff98fb98), ("paleturquoise", 0xffafeeee),
    ("palevioletred", 0xffdb7093), ("papayawhip", 0xffffefd5), ("peachpuff", 0xffffdab9), ("peru", 0xffcd853f),
    ("pink", 0xffffc0cb), ("plum", 0xffdda0dd), ("powderblue", 0xffb0e0e6), ("purple", 0xff800080),
    ("rebeccapurple", 0xff663399), ("red", 0xffff0000), ("rosybrown", 0xffbc8f8f), ("royalblue", 0xff4169e1),
    ("saddlebrown", 0xff8b4513), ("salmon", 0xfffa8072), ("sandybrown", 0xfff4a460), ("seagreen", 0xff2e8b57),
    ("seashell", 0xfffff5ee), ("sienna", 0xffa0522d), ("silver", 0xffc0c0c0), ("skyblue", 0xff87ceeb),
    ("slateblue", 0xff6a5acd), ("slategray", 0xff708090), ("slategrey", 0xff708090), ("snow", 0xfffffafa),
    ("springgreen", 0xff00ff7f), ("steelblue", 0xff4682b4), ("tan", 0xffd2b48c), ("teal", 0xff008080),
    ("thistle", 0xffd8bfd8), ("tomato", 0xffff6347), ("transparent", 0x00000000), ("turquoise", 0xff40e0d0),
    ("violet", 0xffee82ee), ("violetred", 0xffd02090), ("wheat", 0xfff5deb3), ("white", 0xffffffff),
    ("whitesmoke", 0xfff5f5f5), ("yellow", 0xffffff00), ("yellowgreen", 0xff9acd32),
];

/// `CSSParser::ParseSystemColor` on a canvas: Blink's `LayoutTheme` colour for a system colour keyword
/// (`IsSystemColorIncludingDeprecated`: `activeborder` .. `windowtext` and `menu`) in the light scheme. Measured on
/// stock Chrome 150 (Linux), a `<canvas>` (the document's colour provider) and an `OffscreenCanvas` (none) apart --
/// `Highlight` and the seven border/shadow colours differ between them.
fn system_color(s: &str, element: bool) -> Option<Color> {
    const SYSTEM: [(&str, u32, u32); 40] = [
        ("activeborder", 0xff000000, 0xff767676), ("activecaption", 0xffffffff, 0xffffffff),
        ("activetext", 0xffff0000, 0xffff0000), ("appworkspace", 0xffffffff, 0xffffffff),
        ("background", 0xffffffff, 0xffffffff), ("buttonborder", 0xff000000, 0xff767676),
        ("buttonface", 0xffefefef, 0xffefefef), ("buttonhighlight", 0xffefefef, 0xffefefef),
        ("buttonshadow", 0xffefefef, 0xffefefef), ("buttontext", 0xff000000, 0xff000000),
        ("canvas", 0xffffffff, 0xffffffff), ("canvastext", 0xff000000, 0xff000000),
        ("captiontext", 0xff000000, 0xff000000), ("field", 0xffffffff, 0xffffffff),
        ("fieldtext", 0xff000000, 0xff000000), ("graytext", 0xff808080, 0xff808080),
        ("highlight", 0xcc0041c6, 0xcc0038ee), ("highlighttext", 0xffffffff, 0xffffffff),
        ("inactiveborder", 0xff000000, 0xff767676), ("inactivecaption", 0xffffffff, 0xffffffff),
        ("inactivecaptiontext", 0xff808080, 0xff808080), ("infobackground", 0xffffffff, 0xffffffff),
        ("infotext", 0xff000000, 0xff000000), ("linktext", 0xff0000ee, 0xff0000ee),
        ("mark", 0xffffff00, 0xffffff00), ("marktext", 0xff000000, 0xff000000),
        ("menu", 0xffffffff, 0xffffffff), ("menutext", 0xff000000, 0xff000000),
        ("selecteditem", 0xff0860f2, 0xff0860f2), ("selecteditemtext", 0xffffffff, 0xffffffff),
        ("scrollbar", 0xffffffff, 0xffffffff), ("threeddarkshadow", 0xff000000, 0xff767676),
        ("threedface", 0xffefefef, 0xffefefef), ("threedhighlight", 0xff000000, 0xff767676),
        ("threedlightshadow", 0xff000000, 0xff767676), ("threedshadow", 0xff000000, 0xff767676),
        ("visitedtext", 0xff551a8b, 0xff551a8b), ("window", 0xffffffff, 0xffffffff),
        ("windowframe", 0xff000000, 0xff767676), ("windowtext", 0xff000000, 0xff000000),
    ];
    let lower = s.to_ascii_lowercase();
    SYSTEM.iter().find(|(n, _, _)| *n == lower).map(|(_, el, off)| Color::from_rgba32(if element { *el } else { *off }))
}

/// The system colours a stylesheet resolves (`LayoutTheme::SystemColor` with the document's colour provider, light
/// scheme) -- the `<canvas>` table, plus the keywords only CSS takes: `AccentColor`/`AccentColorText` and the link
/// colours (stock Chrome 150, `getComputedStyle`).
pub fn css_system_color(name: &str) -> Option<Color> {
    let lower = name.to_ascii_lowercase();
    let extra = match lower.as_str() {
        "accentcolor" => Some(0xff0075ff),
        "accentcolortext" => Some(0xffffffff),
        "-webkit-link" => Some(0xff0000ee),
        "-webkit-activelink" => Some(0xffff0000),
        _ => None,
    };
    extra.map(Color::from_rgba32).or_else(|| system_color(&lower, true))
}

/// `ParseHexColorInternal`: 3, 4, 6 or 8 hex digits.
pub(crate) fn parse_hex(d: &[u8]) -> Option<Color> {
    if !matches!(d.len(), 3 | 4 | 6 | 8) {
        return None;
    }
    let mut v: u32 = 0;
    for &c in d {
        v = v << 4 | (c as char).to_digit(16)?;
    }
    Some(Color::from_rgba32(match d.len() {
        6 => 0xff00_0000 | v,
        8 => v << 24 | v >> 8,
        4 => (v & 0xf) << 28 | (v & 0xf) << 24 | (v & 0xf000) << 8 | (v & 0xf000) << 4 | (v & 0xf00) << 4 | (v & 0xf00)
            | (v & 0xf0) | (v & 0xf0) >> 4,
        _ => 0xff00_0000 | (v & 0xf00) << 12 | (v & 0xf00) << 8 | (v & 0xf0) << 8 | (v & 0xf0) << 4 | (v & 0xf) << 4 | (v & 0xf),
    }))
}

// -- the fast path (css_parser_fast_paths.cc), byte for byte: it rounds `rgb()` channels and alphas to 8 bits
// where the full parser keeps floats, and has its own decimal parser.

fn is_html_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0c | b'\r')
}

/// `FindLengthOfValidDouble`, including its SSE2 branch (taken whenever 16 bytes remain), which accepts a run
/// ending in a period the scalar loop would reject.
fn valid_double_len(chars: &[u8]) -> usize {
    if chars.is_empty() {
        return 0;
    }
    let mut mark_seen = false;
    let mut valid = 0usize;
    if chars.len() >= 16 {
        let (mut dec, mut mark) = (0u32, 0u32);
        for (i, &c) in chars[..16].iter().enumerate() {
            if c.is_ascii_digit() {
                dec |= 1 << i;
            }
            if c == b'.' {
                mark |= 1 << i;
            }
        }
        // Only the first decimal mark counts.
        mark &= mark.wrapping_neg();
        if dec | mark == 0xffff {
            mark_seen = mark != 0;
            valid = 16;
        } else {
            // A period not followed by a digit is not part of the number.
            mark &= dec >> 1;
            return (!(dec | mark)).trailing_zeros() as usize;
        }
    }
    while valid < chars.len() {
        let c = chars[valid];
        if !c.is_ascii_digit() {
            if !mark_seen && c == b'.' {
                mark_seen = true;
            } else {
                break;
            }
        }
        valid += 1;
    }
    if valid > 0 && chars[valid - 1] == b'.' {
        return 0;
    }
    valid
}

/// `ParsePositiveDouble`: the characters consumed, 0 for none. At most seven decimals count; with seven or more
/// bytes left after the period the SSE2 path scales by its rounded-up 1e-7.
fn parse_positive_double(chars: &[u8], value: &mut f64) -> usize {
    let length = valid_double_len(chars);
    if length == 0 {
        return 0;
    }
    let mut position = 0usize;
    let mut local = 0.0f64;
    while position < length {
        if chars[position] == b'.' {
            break;
        }
        local = local * 10.0 + (chars[position] - b'0') as f64;
        position += 1;
    }
    position += 1;
    if position >= length {
        *value = local;
        return length;
    }
    let num_decimals = (length - position).min(7);
    if chars.len() - position >= 7 {
        const DIV_1E7: f64 = 0.000000100000000000000009;
        const WEIGHTS: [u32; 7] = [1_000_000, 100_000, 10_000, 1_000, 100, 10, 1];
        let fraction: u32 = (0..num_decimals).map(|i| (chars[position + i] - b'0') as u32 * WEIGHTS[i]).sum();
        *value = local + fraction as f64 * DIV_1E7;
        return length;
    }
    let (mut fraction, mut scale) = (0.0f64, 1.0f64);
    for i in 0..num_decimals {
        fraction = fraction * 10.0 + (chars[position + i] - b'0') as f64;
        scale *= 10.0;
    }
    *value = local + fraction / scale;
    length
}

/// `ParseFloatWithMaxValue`: optional spaces and minus, digits clamped at `max`, an optional fraction.
fn parse_float_with_max(chars: &mut &[u8], max: i32, value: &mut f64, negative: &mut bool) -> bool {
    *value = 0.0;
    let mut cur = *chars;
    while cur.first().is_some_and(|&c| is_html_space(c)) {
        cur = &cur[1..];
    }
    *negative = cur.first() == Some(&b'-');
    if *negative {
        cur = &cur[1..];
    }
    if !cur.first().is_some_and(|c| c.is_ascii_digit()) {
        return false;
    }
    while let Some(&c) = cur.first() {
        if !c.is_ascii_digit() {
            break;
        }
        let next = *value * 10.0 + (c - b'0') as f64;
        cur = &cur[1..];
        if next >= max as f64 {
            *value = max as f64;
            while cur.first().is_some_and(|c| c.is_ascii_digit()) {
                cur = &cur[1..];
            }
            break;
        }
        *value = next;
    }
    if cur.first() == Some(&b'.') {
        let mut fractional = 0.0;
        let n = parse_positive_double(cur, &mut fractional);
        if n == 0 {
            return false;
        }
        cur = &cur[n..];
        *value += fractional;
    }
    *chars = cur;
    true
}

#[derive(Clone, Copy, PartialEq)]
enum Term {
    MustWhitespace,
    MustChar,
    CouldWhitespace,
}

#[derive(Clone, Copy, PartialEq)]
enum Unit {
    Unknown,
    Number,
    Percentage,
}

/// `SkipToTerminator`.
fn skip_to_terminator(chars: &mut &[u8], terminator: u8, status: &mut Term) -> bool {
    let mut cur = *chars;
    while cur.first().is_some_and(|&c| is_html_space(c)) {
        cur = &cur[1..];
    }
    match *status {
        Term::CouldWhitespace if cur.first() == Some(&terminator) => {
            *status = Term::MustChar;
            cur = &cur[1..];
        }
        Term::CouldWhitespace | Term::MustWhitespace => {
            *status = Term::MustWhitespace;
            if cur.len() == chars.len() {
                return false;
            }
        }
        Term::MustChar => {
            if cur.first() != Some(&terminator) {
                return false;
            }
            cur = &cur[1..];
        }
    }
    *chars = cur;
    true
}

/// `ParseColorNumberOrPercentage`: an `rgb()` channel as an 8-bit integer (`lround`), a percentage of 255.
fn parse_color_number_or_percentage(chars: &mut &[u8], terminator: u8, status: &mut Term, expect: &mut Unit, value: &mut i32) -> bool {
    let mut cur = *chars;
    let (mut local, mut negative) = (0.0, false);
    if !parse_float_with_max(&mut cur, 255, &mut local, &mut negative) || cur.is_empty() {
        return false;
    }
    if (*expect == Unit::Percentage && cur[0] != b'%') || (*expect == Unit::Number && cur[0] == b'%') {
        return false;
    }
    if cur[0] == b'%' {
        *expect = Unit::Percentage;
        local = local / 100.0 * 255.0;
        if local > 255.0 {
            local = 255.0;
        }
        cur = &cur[1..];
    } else {
        *expect = Unit::Number;
    }
    if !skip_to_terminator(&mut cur, terminator, status) {
        return false;
    }
    *value = if negative { 0 } else { local.round() as i32 };
    *chars = cur;
    true
}

/// `ParsePercentage`: an `hsl()` saturation or lightness, as 0..1.
fn parse_percentage(chars: &mut &[u8], terminator: u8, status: &mut Term, value: &mut f64) -> bool {
    let mut cur = *chars;
    let mut negative = false;
    if !parse_float_with_max(&mut cur, 100, value, &mut negative) || cur.first() != Some(&b'%') {
        return false;
    }
    cur = &cur[1..];
    *value = if negative { 0.0 } else { min_f64(*value * 0.01, 1.0) };
    if !skip_to_terminator(&mut cur, terminator, status) {
        return false;
    }
    *chars = cur;
    true
}

fn contains_char_at(chars: &[u8], pos: usize, ch: u8) -> bool {
    chars.get(pos) == Some(&ch)
}

/// `ParseAlphaValue`: an alpha as an 8-bit integer; the terminator must be the last byte, right after a digit.
fn parse_alpha_value(chars: &mut &[u8], terminator: u8, value: &mut i32) -> bool {
    while chars.first().is_some_and(|&c| is_html_space(c)) {
        *chars = &chars[1..];
    }
    let negative = chars.first() == Some(&b'-');
    if negative {
        *chars = &chars[1..];
    }
    *value = 0;
    let length = chars.len();
    if length < 2 || chars[length - 1] != terminator || !chars[length - 2].is_ascii_digit() {
        return false;
    }
    if chars[0] != b'0' && chars[0] != b'1' && chars[0] != b'.' {
        let dl = valid_double_len(chars);
        if dl > 0 && contains_char_at(chars, dl, terminator) {
            *value = if negative { 0 } else { 255 };
            *chars = &[];
            return true;
        }
        return false;
    }
    if length == 2 && chars[0] != b'.' {
        *value = if !negative && chars[0] == b'1' { 255 } else { 0 };
        *chars = &[];
        return true;
    }
    let body = &chars[..length - 1];
    let tenth = (body.len() == 3 && body[0] == b'0' && body[1] == b'.' && body[2].is_ascii_digit())
        || (body.len() == 2 && body[0] == b'.' && body[1].is_ascii_digit());
    if tenth {
        const TENTHS: [i32; 10] = [0, 26, 51, 77, 102, 128, 153, 179, 204, 230];
        *value = if negative { 0 } else { TENTHS[(chars[length - 2] - b'0') as usize] };
        *chars = &[];
        return true;
    }
    let mut alpha = 0.0;
    let dl = parse_positive_double(chars, &mut alpha);
    if dl == 0 || !contains_char_at(chars, dl, terminator) {
        return false;
    }
    *value = if negative { 0 } else { (min_f64(alpha, 1.0) * 255.0).round() as i32 };
    *chars = &[];
    true
}

/// `ParseSimpleAngle`: a number with an optional `deg`/`grad`/`rad`/`turn`, clamped to the float range.
fn parse_simple_angle(chars: &[u8]) -> Option<(usize, &'static str, f64)> {
    let mut number = 0.0;
    let mut len;
    if chars.first() == Some(&b'-') {
        len = parse_positive_double(&chars[1..], &mut number);
        if len == 0 {
            return None;
        }
        len += 1;
        number = -min_f64(number, f32::MAX as f64);
    } else {
        len = parse_positive_double(chars, &mut number);
        if len == 0 {
            return None;
        }
        number = min_f64(number, f32::MAX as f64);
    }
    let rest = &chars[len..];
    let is = |unit: &[u8]| rest.len() >= unit.len() && rest.iter().zip(unit).all(|(c, u)| c | 0x20 == *u);
    Some(if is(b"deg") {
        (len + 3, "deg", number)
    } else if is(b"grad") {
        (len + 4, "grad", number)
    } else if is(b"rad") {
        (len + 3, "rad", number)
    } else if is(b"turn") {
        (len + 4, "turn", number)
    } else {
        (len, "", number)
    })
}

/// `FastParseColorInternal` (standard mode): `#hex`, and `rgb(`/`rgba(`/`hsl(`/`hsla(` -- lower case only.
fn fast_parse(chars: &[u8]) -> Option<Color> {
    if chars.len() >= 4 && chars[0] == b'#' {
        return parse_hex(&chars[1..]);
    }
    let opens = |name: &[u8]| chars.len() >= 5 && chars.starts_with(name) && (chars[3] == b'(' || (chars[3] == b'a' && chars[4] == b'('));
    if opens(b"rgb") {
        let mut cur = &chars[if chars[3] == b'a' { 5 } else { 4 }..];
        let (mut red, mut green, mut blue, mut alpha) = (0, 0, 0, 0);
        let mut should_have_alpha = false;
        let mut status = Term::CouldWhitespace;
        let mut expect = Unit::Unknown;
        if !parse_color_number_or_percentage(&mut cur, b',', &mut status, &mut expect, &mut red)
            || !parse_color_number_or_percentage(&mut cur, b',', &mut status, &mut expect, &mut green)
        {
            return None;
        }
        let mut no_ws = Term::MustChar;
        if !parse_color_number_or_percentage(&mut cur, b',', &mut no_ws, &mut expect, &mut blue) {
            if parse_color_number_or_percentage(&mut cur, b'/', &mut no_ws, &mut expect, &mut blue) {
                if status != Term::MustWhitespace {
                    return None;
                }
                should_have_alpha = true;
            } else if !parse_color_number_or_percentage(&mut cur, b')', &mut no_ws, &mut expect, &mut blue) {
                return None;
            }
        } else {
            if status != Term::MustChar {
                return None;
            }
            should_have_alpha = true;
        }
        if should_have_alpha {
            if !parse_alpha_value(&mut cur, b')', &mut alpha) {
                return None;
            }
            return Some(Color::from_rgba(red, green, blue, alpha));
        }
        return cur.is_empty().then(|| Color::from_rgba(red, green, blue, 255));
    }
    if opens(b"hsl") {
        let mut cur = &chars[if chars[3] == b'a' { 5 } else { 4 }..];
        while cur.first().is_some_and(|&c| is_html_space(c)) {
            cur = &cur[1..];
        }
        let (hue_len, unit, mut hue) = parse_simple_angle(cur)?;
        match unit {
            "rad" => hue *= 180.0 / std::f64::consts::PI,
            "grad" => hue *= 360.0 / 400.0,
            "turn" => hue *= 360.0,
            _ => {}
        }
        if hue < 0.0 {
            hue = hue % 360.0 + 360.0;
        } else if hue > 360.0 {
            hue %= 360.0;
        }
        cur = &cur[hue_len..];
        let mut status = Term::CouldWhitespace;
        if !skip_to_terminator(&mut cur, b',', &mut status) {
            return None;
        }
        let (mut saturation, mut lightness) = (0.0, 0.0);
        if !parse_percentage(&mut cur, b',', &mut status, &mut saturation) {
            return None;
        }
        let mut no_ws = Term::MustChar;
        let mut should_have_alpha = false;
        if !parse_percentage(&mut cur, b',', &mut no_ws, &mut lightness) {
            if parse_percentage(&mut cur, b'/', &mut no_ws, &mut lightness) {
                if status != Term::MustWhitespace {
                    return None;
                }
                should_have_alpha = true;
            } else if !parse_percentage(&mut cur, b')', &mut no_ws, &mut lightness) {
                return None;
            }
        } else {
            if status != Term::MustChar {
                return None;
            }
            should_have_alpha = true;
        }
        let alpha = if should_have_alpha {
            let mut a = 0;
            if !parse_alpha_value(&mut cur, b')', &mut a) {
                return None;
            }
            a as f32 * (1.0f32 / 255.0f32)
        } else {
            1.0
        };
        return cur.is_empty().then(|| {
            Color::from_color_space(Space::Hsl, Some(hue as f32), Some(saturation as f32), Some(lightness as f32), Some(alpha))
        });
    }
    None
}

// -- the full parser (`ParseSingleValue(color)`): a hash or a colour function resolved at parse time (`CSSColor`).
// A keyword, `color-mix()`, a relative colour or `calc()` in a lightness-first space is a value
// `CSSParser::ParseColor` does not take ([`parse_color_value`] does).

fn full_parse(s: &str) -> Option<Color> {
    match parse_color_value(s)? {
        ColorValue::Color(c, calc) if !calc.iter().any(|x| *x) => Some(c),
        _ => None,
    }
}

// ---- converting between spaces (`Color::ConvertToColorSpace`) --------------------------------------------

/// `kAchromaticChromaThreshold`: below this chroma an lch/oklch hue is powerless (`none`).
const ACHROMATIC_CHROMA: f32 = 0.02;

/// `AngleToUnitCircleDegrees`.
fn unit_circle(angle: f32) -> f32 {
    ((angle % 360.0) + 360.0) % 360.0
}

impl Color {
    /// `ResolveMissingComponents`: `none` is zero outside interpolation.
    fn resolve_missing(&mut self) {
        for i in 0..3 {
            if self.params_none[i] {
                self.params[i] = 0.0;
                self.params_none[i] = false;
            }
        }
    }

    /// `ConvertToColorSpace(destination)`, missing components resolved first.
    pub fn convert_to(&mut self, dest: Space) {
        if self.space == dest {
            return;
        }
        self.resolve_missing();
        let [p0, p1, p2] = self.params;
        match dest {
            Space::XyzD65 | Space::XyzD50 | Space::SrgbLinear | Space::DisplayP3 | Space::DisplayP3Linear | Space::A98Rgb
            | Space::ProPhotoRgb | Space::Rec2020 => {
                let (m, t) = dest.sk_params();
                self.params = tf_apply_inverse(&t, m_apply_inverse(&m, self.to_xyz_d50()));
            }
            Space::Lab => {
                self.params = if self.space == Space::Lch { lch_to_lab(p0, p1, p2) } else { xyz_d50_to_lab(self.to_xyz_d50()) };
            }
            Space::Oklab => {
                if self.space == Space::Oklch {
                    self.params = lch_to_lab(p0, p1, p2);
                } else {
                    self.params = xyz_d50_to_oklab(self.to_xyz_d50());
                }
            }
            Space::Lch => {
                let lab = if self.space == Space::Lab { [p0, p1, p2] } else { xyz_d50_to_lab(self.to_xyz_d50()) };
                self.params = lab_to_lch(lab);
                self.params[2] = unit_circle(self.params[2]);
                if self.params[1] <= ACHROMATIC_CHROMA {
                    self.params_none[2] = true;
                }
            }
            Space::Oklch => {
                if self.space == Space::Oklab {
                    self.params = lab_to_lch([p0, p1, p2]);
                } else {
                    self.params = lab_to_lch(xyz_d50_to_oklab(self.to_xyz_d50()));
                    self.params[2] = unit_circle(self.params[2]);
                }
                if self.params[1] <= ACHROMATIC_CHROMA {
                    self.params_none[2] = true;
                }
            }
            Space::Srgb => self.params = self.to_srgb(),
            Space::SrgbLegacy => {
                let [r, g, b] = self.to_srgb();
                self.params = [(r as f64 * 255.0) as f32, (g as f64 * 255.0) as f32, (b as f64 * 255.0) as f32];
            }
            Space::Hsl => {
                self.params = srgb_to_hsl(self.to_srgb());
                if self.params[1] == 0.0 {
                    self.params_none[0] = true;
                }
            }
            Space::Hwb => {
                self.params = srgb_to_hwb(self.to_srgb());
                if self.params[1] + self.params[2] >= 1.0 {
                    self.params_none[0] = true;
                }
            }
        }
        self.space = dest;
    }

    /// `GetAnalogousMissingComponents`: which of this colour's `none`s survive conversion into `dest`.
    fn analogous_missing(&self, dest: Space) -> [bool; 3] {
        let rgb_or_xyz = |s: Space| !matches!(s, Space::Lab | Space::Oklab | Space::Lch | Space::Oklch | Space::Hsl | Space::Hwb);
        let lab = |s: Space| matches!(s, Space::Lab | Space::Oklab);
        let lch = |s: Space| matches!(s, Space::Lch | Space::Oklch);
        let [n0, n1, n2] = self.params_none;
        match self.space {
            s if rgb_or_xyz(s) && rgb_or_xyz(dest) => [n0, n1, n2],
            Space::Lab | Space::Oklab if lab(dest) => [n0, n1, n2],
            Space::Lab | Space::Oklab if lch(dest) => [n0, false, false],
            Space::Lab | Space::Oklab if dest == Space::Hsl => [false, false, n0],
            Space::Lch | Space::Oklch if lch(dest) => [n0, n1, n2],
            Space::Lch | Space::Oklch if dest == Space::Hsl => [n2, n1, n0],
            Space::Lch | Space::Oklch if lab(dest) => [n0, false, false],
            Space::Hsl if lch(dest) => [n2, n1, n0],
            Space::Hsl if lab(dest) => [n2, false, false],
            Space::Hsl if dest == Space::Hwb => [n0, false, false],
            Space::Hwb if dest == Space::Hsl => [n0, false, false],
            Space::Hwb if lch(dest) => [false, false, n0],
            _ => [false; 3],
        }
    }

    /// `ConvertToColorSpaceForInterpolation`: analogous `none`s carried forward across the conversion.
    fn convert_for_interpolation(&mut self, dest: Space) {
        if self.space == dest {
            return;
        }
        let carried = self.analogous_missing(dest);
        self.convert_to(dest);
        for (i, carry) in carried.iter().enumerate() {
            if *carry {
                self.params[i] = 0.0;
                self.params_none[i] = true;
            }
        }
    }

    /// `PremultiplyColor`: the alpha it had (a `none` alpha leaves the colour as it is).
    fn premultiply(&mut self) -> f32 {
        if self.alpha_none {
            return self.alpha;
        }
        let alpha = self.alpha;
        if !matches!(self.space, Space::Hsl | Space::Hwb) {
            self.params[0] *= alpha;
        }
        self.params[1] *= alpha;
        if !self.space.chroma_second() {
            self.params[2] *= alpha;
        }
        self.alpha = 1.0;
        alpha
    }

    /// `UnpremultiplyColor`.
    fn unpremultiply(&mut self) {
        if self.alpha_none || self.alpha == 0.0 {
            return;
        }
        if !matches!(self.space, Space::Hsl | Space::Hwb) {
            self.params[0] /= self.alpha;
        }
        self.params[1] /= self.alpha;
        if !self.space.chroma_second() {
            self.params[2] /= self.alpha;
        }
    }
}

/// `SubstituteMissingParameters`: a `none` takes the other colour's component.
fn substitute_missing(a: &mut Color, b: &mut Color) {
    for i in 0..3 {
        if a.params_none[i] && !b.params_none[i] {
            a.params[i] = b.params[i];
            a.params_none[i] = false;
        } else if b.params_none[i] && !a.params_none[i] {
            b.params[i] = a.params[i];
            b.params_none[i] = false;
        }
    }
    if a.alpha_none && !b.alpha_none {
        a.alpha = b.alpha;
        a.alpha_none = false;
    } else if b.alpha_none && !a.alpha_none {
        b.alpha = a.alpha;
        b.alpha_none = false;
    }
}

/// A `<hue-interpolation-method>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hue {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

/// `blink::Blend(float, float, double)`.
fn blend(from: f32, to: f32, progress: f64) -> f32 {
    (from as f64 + (to - from) as f64 * progress) as f32
}

/// `Color::HueInterpolation`.
fn hue_interpolation(mut v1: f32, mut v2: f32, pct: f32, method: Hue) -> f32 {
    match method {
        Hue::Shorter => {
            let diff = v2 - v1;
            if diff > 180.0 {
                v1 += 360.0;
            } else if diff < -180.0 {
                v2 += 360.0;
            }
        }
        Hue::Longer => {
            let diff = v2 - v1;
            if diff > 0.0 && diff < 180.0 {
                v1 += 360.0;
            } else if diff > -180.0 && diff <= 0.0 {
                v2 += 360.0;
            }
        }
        Hue::Increasing => {
            if v2 < v1 {
                v2 += 360.0;
            }
        }
        Hue::Decreasing => {
            if v1 < v2 {
                v1 += 360.0;
            }
        }
    }
    unit_circle(blend(v1, v2, pct as f64))
}

/// `Color::FromColorMix` (`InterpolateColors`): interpolated premultiplied in `space`, the alpha scaled, a legacy
/// result written as `color(srgb ...)`.
fn from_color_mix(space: Space, hue: Hue, mut c1: Color, mut c2: Color, pct: f32, alpha_multiplier: f32) -> Color {
    c1.convert_for_interpolation(space);
    c2.convert_for_interpolation(space);
    substitute_missing(&mut c1, &mut c2);
    let a1 = c1.premultiply();
    let a2 = c2.premultiply();
    let both_none = |i: usize| c1.params_none[i] && c2.params_none[i];
    let p0 = (!both_none(0)).then(|| {
        if matches!(space, Space::Hsl | Space::Hwb) {
            hue_interpolation(c1.params[0], c2.params[0], pct, hue)
        } else {
            blend(c1.params[0], c2.params[0], pct as f64)
        }
    });
    let p1 = (!both_none(1)).then(|| blend(c1.params[1], c2.params[1], pct as f64));
    let p2 = (!both_none(2)).then(|| {
        if space.chroma_second() {
            hue_interpolation(c1.params[2], c2.params[2], pct, hue)
        } else {
            blend(c1.params[2], c2.params[2], pct as f64)
        }
    });
    let alpha = (!(c1.alpha_none && c2.alpha_none)).then(|| blend(a1, a2, pct as f64));
    let mut result = Color::from_color_space(space, p0, p1, p2, alpha);
    result.unpremultiply();
    result.alpha *= alpha_multiplier;
    if result.space.is_legacy() {
        result.convert_to(Space::Srgb);
    }
    result
}

/// `color_utils::GetRelativeLuminance` of an `SkColor`.
fn relative_luminance(c: [u8; 3]) -> f32 {
    let lin = |v: u8| {
        let x = v as f32 * (1.0f32 / 255.0);
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// `color_utils::GetContrastRatio`.
fn contrast_ratio(a: f32, b: f32) -> f32 {
    let (a, b) = (a + 0.05, b + 0.05);
    if a > b { a / b } else { b / a }
}

// ---- colour values: what the CSS parser holds before computed-value time --------------------------------

/// What `currentcolor` is where a colour resolves, and whose system colours apply (a document's, or none -- an
/// `OffscreenCanvas`, a gradient stop).
#[derive(Clone, Copy, Debug)]
pub struct Context {
    pub current: Color,
    pub element: bool,
}

impl Context {
    /// A stylesheet's: system colours through the document's colour provider, `currentcolor` black until a cascade
    /// says otherwise.
    pub fn css() -> Context {
        Context { current: Color::black(), element: true }
    }
}

/// A `<color>` as Blink's parser holds it -- `CSSColor`, a keyword, `currentcolor`, or a function resolved only when
/// it is used (`CSSColorMixValue`, `CSSRelativeColorValue`, `CSSContrastColorValue`).
#[derive(Clone, Debug, PartialEq)]
pub enum ColorValue {
    /// Resolved as it was parsed; a lightness-first space keeps a channel written as `calc()` in that form for its
    /// specified value (`lab(calc(51) 40 30)`).
    Color(Color, [bool; 4]),
    /// A named or system colour, specified as the keyword.
    Keyword(String),
    CurrentColor,
    Mix(Box<Mix>),
    Relative(Box<Relative>),
    Contrast(Box<ColorValue>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    space: Space,
    hue: Hue,
    c1: ColorValue,
    p1: Option<Expr>,
    c2: ColorValue,
    p2: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Relative {
    /// The function as written (`rgb`, `hsla`, `color`, ...).
    name: String,
    space: Space,
    origin: ColorValue,
    ch: [Expr; 3],
    alpha: Option<Expr>,
}

/// A channel: a number, a percentage, `none`, a channel keyword of a relative colour, or arithmetic of them
/// (`calc()`, `min()`, `max()`, `clamp()`). An angle is held in degrees, as a number.
#[derive(Clone, Debug, PartialEq)]
enum Expr {
    None,
    Num(f64),
    Pct(f64),
    /// The origin's channel 0-2, or 3 = its alpha.
    Key(usize, String),
    Op(Box<Expr>, char, Box<Expr>),
    Calc(Box<Expr>),
    Min(Vec<Expr>),
    Max(Vec<Expr>),
    Clamp(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Percent,
}

impl Expr {
    fn is_calc(&self) -> bool {
        matches!(self, Expr::Calc(_) | Expr::Min(_) | Expr::Max(_) | Expr::Clamp(..))
    }

    /// The kind a valid expression has (numbers and percentages do not mix in a colour channel).
    fn kind(&self) -> Option<Kind> {
        match self {
            Expr::None => None,
            Expr::Num(_) | Expr::Key(..) => Some(Kind::Number),
            Expr::Pct(_) => Some(Kind::Percent),
            Expr::Calc(e) => e.kind(),
            Expr::Op(a, op, b) => {
                let (ka, kb) = (a.kind()?, b.kind()?);
                match op {
                    '+' | '-' => (ka == kb).then_some(ka),
                    '*' => match (ka, kb) {
                        (Kind::Number, k) | (k, Kind::Number) => Some(k),
                        _ => None,
                    },
                    _ => (kb == Kind::Number).then_some(ka),
                }
            }
            Expr::Min(v) | Expr::Max(v) => {
                let k = v.first()?.kind()?;
                v.iter().all(|e| e.kind() == Some(k)).then_some(k)
            }
            Expr::Clamp(a, b, c) => {
                let k = b.kind()?;
                [a, c].iter().all(|e| matches!(***e, Expr::None) || e.kind() == Some(k)).then_some(k)
            }
        }
    }

    /// Whether it names a channel keyword anywhere.
    fn has_key(&self) -> bool {
        match self {
            Expr::Key(..) => true,
            Expr::Op(a, _, b) => a.has_key() || b.has_key(),
            Expr::Calc(e) => e.has_key(),
            Expr::Min(v) | Expr::Max(v) => v.iter().any(Expr::has_key),
            Expr::Clamp(a, b, c) => a.has_key() || b.has_key() || c.has_key(),
            _ => false,
        }
    }

    /// The value, a percentage taken of `base` (`CalculationValue::Evaluate`); `None` for `none`.
    fn eval(&self, base: f64, keys: &[f64; 4]) -> Option<f64> {
        Some(match self {
            Expr::None => return None,
            Expr::Num(v) => *v,
            Expr::Pct(v) => v / 100.0 * base,
            Expr::Key(i, _) => keys[*i],
            Expr::Calc(e) => e.eval(base, keys)?,
            Expr::Op(a, op, b) => {
                let (x, y) = (a.eval(base, keys)?, b.eval(base, keys)?);
                match op {
                    '+' => x + y,
                    '-' => x - y,
                    '*' => x * y,
                    _ => x / y,
                }
            }
            Expr::Min(v) => v.iter().map(|e| e.eval(base, keys)).collect::<Option<Vec<_>>>()?.into_iter().fold(f64::INFINITY, f64::min),
            Expr::Max(v) => v.iter().map(|e| e.eval(base, keys)).collect::<Option<Vec<_>>>()?.into_iter().fold(f64::NEG_INFINITY, f64::max),
            Expr::Clamp(lo, v, hi) => {
                let v = v.eval(base, keys)?;
                let lo = lo.eval(base, keys).unwrap_or(f64::NEG_INFINITY);
                let hi = hi.eval(base, keys).unwrap_or(f64::INFINITY);
                v.min(hi).max(lo)
            }
        })
    }

    /// The channel's specified text: a number as CSS prints one, a constant `calc()` simplified to its value.
    fn css_text(&self) -> String {
        match self {
            Expr::None => "none".into(),
            Expr::Num(v) => to_precision(*v, 6),
            Expr::Pct(v) => format!("{}%", to_precision(*v, 6)),
            Expr::Key(_, name) => name.clone(),
            e if e.is_calc() && !e.has_key() => {
                let v = e.eval(100.0, &[0.0; 4]).unwrap_or(0.0);
                if e.kind() == Some(Kind::Percent) { format!("calc({}%)", to_precision(v, 6)) } else { format!("calc({})", param_to_string(v, 6)) }
            }
            Expr::Calc(e) => format!("calc({})", e.inner_text()),
            e => e.inner_text(),
        }
    }

    fn inner_text(&self) -> String {
        match self {
            Expr::Op(a, op, b) => format!("{} {op} {}", a.inner_text(), b.inner_text()),
            Expr::Calc(e) => format!("({})", e.inner_text()),
            Expr::Min(v) => format!("min({})", v.iter().map(Expr::inner_text).collect::<Vec<_>>().join(", ")),
            Expr::Max(v) => format!("max({})", v.iter().map(Expr::inner_text).collect::<Vec<_>>().join(", ")),
            Expr::Clamp(a, b, c) => format!("clamp({}, {}, {})", a.inner_text(), b.inner_text(), c.inner_text()),
            e => e.css_text(),
        }
    }
}

/// A relative colour's channel keywords for `space` (`ColorFunction::Metadata::channel_name`).
fn channel_names(space: Space) -> [&'static str; 3] {
    match space {
        Space::SrgbLegacy | Space::Srgb | Space::SrgbLinear | Space::DisplayP3 | Space::DisplayP3Linear | Space::A98Rgb
        | Space::ProPhotoRgb | Space::Rec2020 => ["r", "g", "b"],
        Space::XyzD50 | Space::XyzD65 => ["x", "y", "z"],
        Space::Lab | Space::Oklab => ["l", "a", "b"],
        Space::Lch | Space::Oklch => ["l", "c", "h"],
        Space::Hsl => ["h", "s", "l"],
        Space::Hwb => ["h", "w", "b"],
    }
}

/// One operand of a `calc()`-like function, `keys` the relative colour's channel names (empty outside one).
fn calc_value(b: &mut Parser, keys: &[&'static str; 3], relative: bool, hue: bool) -> Option<Expr> {
    b.skip_whitespace();
    let start = b.position();
    let tok = b.next_including_whitespace().ok()?.clone();
    let text = b.slice_from(start);
    Some(match tok {
        Token::Number { .. } => Expr::Num(crate::style::serialize::number_of(text)),
        Token::Percentage { .. } => Expr::Pct(crate::style::serialize::number_of(text)),
        Token::Dimension { ref unit, .. } if hue => Expr::Num(angle_degrees(crate::style::serialize::number_of(text), unit)?),
        Token::Ident(ref v) => {
            let v = v.to_ascii_lowercase();
            match v.as_str() {
                "e" => Expr::Num(std::f64::consts::E),
                "pi" => Expr::Num(std::f64::consts::PI),
                "infinity" => Expr::Num(f64::INFINITY),
                "-infinity" => Expr::Num(f64::NEG_INFINITY),
                "nan" => Expr::Num(f64::NAN),
                k if relative => {
                    let i = if k == "alpha" { 3 } else { keys.iter().position(|n| *n == k)? };
                    Expr::Key(i, k.to_string())
                }
                _ => return None,
            }
        }
        Token::ParenthesisBlock => {
            let e = b.parse_nested_block(|n| calc_sum(n, keys, relative, hue).ok_or_else(|| n.new_custom_error::<(), ()>(()))).ok()?;
            Expr::Calc(Box::new(e))
        }
        Token::Function(ref name) => calc_function(&name.to_ascii_lowercase(), b, keys, relative, hue)?,
        _ => return None,
    })
}

fn calc_product(b: &mut Parser, keys: &[&'static str; 3], relative: bool, hue: bool) -> Option<Expr> {
    let mut left = calc_value(b, keys, relative, hue)?;
    loop {
        let state = b.state();
        match b.next() {
            Ok(Token::Delim(c)) if *c == '*' || *c == '/' => {
                let op = *c;
                let right = calc_value(b, keys, relative, hue)?;
                left = Expr::Op(Box::new(left), op, Box::new(right));
            }
            _ => {
                b.reset(&state);
                return Some(left);
            }
        }
    }
}

fn calc_sum(b: &mut Parser, keys: &[&'static str; 3], relative: bool, hue: bool) -> Option<Expr> {
    let mut left = calc_product(b, keys, relative, hue)?;
    loop {
        let state = b.state();
        match b.next() {
            Ok(Token::Delim(c)) if *c == '+' || *c == '-' => {
                let op = *c;
                let right = calc_product(b, keys, relative, hue)?;
                left = Expr::Op(Box::new(left), op, Box::new(right));
            }
            // `10 -5`: a signed number where an operator belongs.
            Ok(Token::Number { has_sign: true, value, .. }) => {
                let v = *value as f64;
                left = Expr::Op(Box::new(left), '+', Box::new(Expr::Num(v)));
            }
            _ => {
                b.reset(&state);
                return Some(left);
            }
        }
    }
}

fn calc_function(name: &str, b: &mut Parser, keys: &[&'static str; 3], relative: bool, hue: bool) -> Option<Expr> {
    let mut args = b
        .parse_nested_block(|n| -> Result<Vec<Expr>, cssparser::ParseError<'_, ()>> {
            let mut args = Vec::new();
            loop {
                match calc_sum(n, keys, relative, hue) {
                    Some(e) => args.push(e),
                    None => return Err(n.new_custom_error(())),
                }
                if n.try_parse(|i| i.expect_comma()).is_err() {
                    break;
                }
            }
            if !n.is_exhausted() {
                return Err(n.new_custom_error(()));
            }
            Ok(args)
        })
        .ok()?;
    let e = match (name, args.len()) {
        ("calc", 1) => Expr::Calc(Box::new(args.pop()?)),
        ("min", n) if n > 0 => Expr::Min(args),
        ("max", n) if n > 0 => Expr::Max(args),
        ("clamp", 3) => {
            let c = args.pop()?;
            let v = args.pop()?;
            let a = args.pop()?;
            Expr::Clamp(Box::new(a), Box::new(v), Box::new(c))
        }
        _ => return None,
    };
    e.kind().map(|_| e)
}

/// `CSSNumericLiteralValue::ComputeDegrees`.
fn angle_degrees(v: f64, unit: &str) -> Option<f64> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "deg" => v,
        "rad" => v * (180.0 / std::f64::consts::PI),
        "grad" => v * (360.0 / 400.0),
        "turn" => v * 360.0,
        _ => return None,
    })
}

/// A channel as written: `none`, a number, a percentage (not for a hue), an angle (a hue), a channel keyword (in a
/// relative colour) or a `calc()`-like function.
fn channel_expr(b: &mut Parser, hue: bool, keys: &[&'static str; 3], relative: bool) -> Option<Expr> {
    b.skip_whitespace();
    let start = b.position();
    let tok = b.next_including_whitespace().ok()?.clone();
    let text = b.slice_from(start);
    match tok {
        Token::Ident(ref v) if v.eq_ignore_ascii_case("none") => Some(Expr::None),
        Token::Ident(ref v) if relative => {
            let k = v.to_ascii_lowercase();
            let i = if k == "alpha" { 3 } else { keys.iter().position(|n| *n == k)? };
            Some(Expr::Key(i, k))
        }
        Token::Number { .. } => Some(Expr::Num(crate::style::serialize::number_of(text))),
        Token::Percentage { .. } if !hue => Some(Expr::Pct(crate::style::serialize::number_of(text))),
        Token::Dimension { ref unit, .. } if hue => Some(Expr::Num(angle_degrees(crate::style::serialize::number_of(text), unit)?)),
        Token::Function(ref name) if matches!(name.to_ascii_lowercase().as_str(), "calc" | "min" | "max" | "clamp") => {
            let e = calc_function(&name.to_ascii_lowercase(), b, keys, relative, hue)?;
            // A hue takes a number (an angle is one); any other channel a number or a percentage.
            (!hue || e.kind() == Some(Kind::Number)).then_some(e)
        }
        _ => None,
    }
}

/// Any `<color>` (`ConsumeColorInternal`), in a stylesheet's terms: a hash, a CSS colour keyword, a system colour,
/// `currentcolor`, or a colour function (relative ones, `color-mix()`, `contrast-color()` included).
pub fn consume_color_value(p: &mut Parser) -> Option<ColorValue> {
    match p.next().ok()?.clone() {
        Token::Hash(v) | Token::IDHash(v) => Some(ColorValue::Color(parse_hex(v.as_bytes())?, [false; 4])),
        Token::Ident(name) => {
            let lower = name.to_ascii_lowercase();
            if lower == "currentcolor" {
                return Some(ColorValue::CurrentColor);
            }
            let known = lower == "transparent" || cssparser::color::parse_named_color(&lower).is_ok() || css_system_color(&lower).is_some();
            known.then_some(ColorValue::Keyword(lower))
        }
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            p.parse_nested_block(|b| function_value(&name, b).ok_or_else(|| b.new_custom_error::<(), ()>(()))).ok()
        }
        _ => None,
    }
}

/// A colour function's arguments as a value.
pub(crate) fn function_value(name: &str, b: &mut Parser) -> Option<ColorValue> {
    match name {
        "color-mix" => return color_mix(b).map(|m| ColorValue::Mix(Box::new(m))),
        "contrast-color" => {
            let c = consume_color_value(b)?;
            return b.is_exhausted().then(|| ColorValue::Contrast(Box::new(c)));
        }
        _ => {}
    }
    // `[from <color>]?`: a relative colour.
    let relative_origin = if b.try_parse(|i| i.expect_ident_matching("from")).is_ok() { Some(consume_color_value(b)?) } else { None };
    let space = match name {
        "rgb" | "rgba" => Space::SrgbLegacy,
        "hsl" | "hsla" => Space::Hsl,
        "hwb" => Space::Hwb,
        "lab" => Space::Lab,
        "oklab" => Space::Oklab,
        "lch" => Space::Lch,
        "oklch" => Space::Oklch,
        "color" => {
            let Token::Ident(arg) = b.next().ok()?.clone() else { return None };
            Space::from_argument(&arg)?
        }
        _ => return None,
    };
    let keys = channel_names(space);
    let relative = relative_origin.is_some();
    let mut ch = [Expr::None, Expr::None, Expr::None];
    let mut comma = [false; 2];
    for (i, c) in ch.iter_mut().enumerate() {
        *c = channel_expr(b, space.is_hue(i), &keys, relative)?;
        if i < 2 {
            comma[i] = b.try_parse(|i| i.expect_comma()).is_ok();
        }
    }
    if comma[0] != comma[1] {
        return None;
    }
    let legacy = comma[0];
    if legacy && (relative || !space.is_legacy()) {
        return None;
    }
    let has_alpha = if legacy { b.try_parse(|i| i.expect_comma()).is_ok() } else { b.try_parse(|i| i.expect_delim('/')).is_ok() };
    let alpha = if has_alpha { Some(channel_expr(b, false, &keys, relative)?) } else { None };
    if !b.is_exhausted() {
        return None;
    }
    if let Some(origin) = relative_origin {
        return Some(ColorValue::Relative(Box::new(Relative { name: name.to_string(), space, origin, ch, alpha })));
    }
    let kinds: Vec<Option<Kind>> = ch.iter().map(Expr::kind).collect();
    if legacy {
        let has_none = ch.iter().chain(alpha.iter()).any(|e| matches!(e, Expr::None));
        if has_none || space == Space::Hwb {
            return None;
        }
        if space == Space::SrgbLegacy && kinds.contains(&Some(Kind::Percent)) && kinds.contains(&Some(Kind::Number)) {
            return None;
        }
        if space == Space::Hsl && kinds[1..].contains(&Some(Kind::Number)) {
            return None;
        }
    }
    // Resolved at parse time (`ResolveColorChannelForNumericLiteral`): constant `calc()`s simplify to their value.
    let base = space.percent_base();
    let mut channels: [Option<f64>; 3] = [None; 3];
    for i in 0..3 {
        channels[i] = ch[i].eval(base[i], &[0.0; 4]);
    }
    let mut alpha_v = match &alpha {
        None => Some(1.0),
        Some(e) => e.eval(1.0, &[0.0; 4]).map(|v| if v.is_nan() { 0.0 } else { clamp_f64(v, 0.0, 1.0) }),
    };
    adjust_channels(space, legacy, false, &mut channels, &mut alpha_v);
    let calc = [ch[0].is_calc(), ch[1].is_calc(), ch[2].is_calc(), alpha.as_ref().is_some_and(Expr::is_calc)];
    let keep_calc = if space.lightness_first() { calc } else { [false; 4] };
    Some(ColorValue::Color(
        Color::from_color_space(space, channels[0].map(|v| v as f32), channels[1].map(|v| v as f32), channels[2].map(|v| v as f32), alpha_v.map(|v| v as f32)),
        keep_calc,
    ))
}

/// `ColorFunctionParser::MakePerColorSpaceAdjustments`.
fn adjust_channels(space: Space, legacy: bool, relative: bool, channels: &mut [Option<f64>; 3], alpha: &mut Option<f64>) {
    for (i, c) in channels.iter_mut().enumerate() {
        if let Some(v) = c {
            if space.is_hue(i) {
                if !v.is_finite() {
                    *v = 0.0;
                }
                *v = (*v % 360.0 + 360.0) % 360.0;
            }
        }
    }
    if space == Space::SrgbLegacy {
        for v in channels.iter_mut().flatten() {
            if !v.is_finite() {
                *v = if *v > 0.0 { 255.0 } else { 0.0 };
            } else if !relative {
                *v = clamp_f64(*v, 0.0, 255.0);
            }
        }
        if !relative {
            if let Some(a) = alpha.as_mut() {
                *a = (*a * 255.0).round() / 255.0;
            }
        }
    }
    if matches!(space, Space::Hsl | Space::Hwb) {
        for v in channels[1..].iter_mut().flatten() {
            let val = *v / 100.0;
            *v = if legacy { clamp_f64(val, 0.0, 1.0) } else if !relative { max_f64(0.0, val) } else { val };
        }
    }
}

/// `color-mix([in <space> [<hue> hue]?,]? <color> <percentage>?, <color> <percentage>?)`.
fn color_mix(b: &mut Parser) -> Option<Mix> {
    let (mut space, mut hue) = (Space::Oklab, Hue::Shorter);
    if b.try_parse(|i| i.expect_ident_matching("in")).is_ok() {
        let Token::Ident(s) = b.next().ok()?.clone() else { return None };
        space = match s.to_ascii_lowercase().as_str() {
            "xyz" | "xyz-d65" => Space::XyzD65,
            "xyz-d50" => Space::XyzD50,
            "srgb-linear" => Space::SrgbLinear,
            "display-p3" => Space::DisplayP3,
            "display-p3-linear" => Space::DisplayP3Linear,
            "a98-rgb" => Space::A98Rgb,
            "prophoto-rgb" => Space::ProPhotoRgb,
            "rec2020" => Space::Rec2020,
            "lab" => Space::Lab,
            "oklab" => Space::Oklab,
            "lch" => Space::Lch,
            "oklch" => Space::Oklch,
            "srgb" => Space::Srgb,
            "hsl" => Space::Hsl,
            "hwb" => Space::Hwb,
            _ => return None,
        };
        if matches!(space, Space::Hsl | Space::Hwb | Space::Lch | Space::Oklch) {
            let method = b.try_parse(|i| -> Result<Hue, cssparser::ParseError<'_, ()>> {
                let t = i.expect_ident()?.to_ascii_lowercase();
                let m = match t.as_str() {
                    "shorter" => Hue::Shorter,
                    "longer" => Hue::Longer,
                    "increasing" => Hue::Increasing,
                    "decreasing" => Hue::Decreasing,
                    _ => return Err(i.new_custom_error(())),
                };
                Ok(m)
            });
            if let Ok(m) = method {
                b.expect_ident_matching("hue").ok()?;
                hue = m;
            }
        }
        b.expect_comma().ok()?;
    }
    let operand = |b: &mut Parser| -> Option<(ColorValue, Option<Expr>)> {
        let pct = |b: &mut Parser| b.try_parse(|i| -> Result<Expr, cssparser::ParseError<'_, ()>> {
            let e = channel_expr(i, false, &["", "", ""], false).ok_or_else(|| i.new_custom_error(()))?;
            if e.kind() != Some(Kind::Percent) {
                return Err(i.new_custom_error(()));
            }
            Ok(e)
        }).ok();
        let mut p = pct(b);
        let c = b.try_parse(|i| consume_color_value(i).ok_or_else(|| i.new_custom_error::<(), ()>(()))).ok()?;
        if p.is_none() {
            p = pct(b);
        }
        // A literal percentage outside [0, 100] is refused (a `calc()` is not).
        if let Some(Expr::Pct(v)) = p {
            if !(0.0..=100.0).contains(&v) {
                return None;
            }
        }
        Some((c, p))
    };
    let (c1, p1) = operand(b)?;
    b.expect_comma().ok()?;
    let (c2, p2) = operand(b)?;
    if let (Some(Expr::Pct(a)), Some(Expr::Pct(z))) = (&p1, &p2) {
        if *a == 0.0 && *z == 0.0 {
            return None;
        }
    }
    b.is_exhausted().then_some(Mix { space, hue, c1, p1, c2, p2 })
}

impl ColorValue {
    /// The colour it is where `cx` says what `currentcolor` and the system colours are (`StyleColor::Resolve`): an
    /// unresolved function's legacy result is written `color(srgb ...)`, except `contrast-color()`'s.
    pub fn resolve(&self, cx: &Context) -> Color {
        let mut c = self.operand(cx);
        if matches!(self, ColorValue::Mix(_) | ColorValue::Relative(_)) && c.space.is_legacy() {
            c.convert_to(Space::Srgb);
        }
        c
    }

    /// `ResolveColorOperand`.
    fn operand(&self, cx: &Context) -> Color {
        match self {
            ColorValue::Color(c, _) => *c,
            ColorValue::CurrentColor => cx.current,
            ColorValue::Keyword(k) => keyword_color(k, cx.element).unwrap_or_else(Color::black),
            ColorValue::Contrast(c) => {
                let rgb = c.operand(cx).rgb8();
                let lum = relative_luminance(rgb);
                let white = contrast_ratio(relative_luminance([255; 3]), lum);
                let black = contrast_ratio(relative_luminance([0; 3]), lum);
                Color::from_rgba32(if black > white { 0xff00_0000 } else { 0xffff_ffff })
            }
            ColorValue::Mix(m) => {
                // `CSSColorMixValue::NormalizePercentages`.
                let pct = |e: &Option<Expr>| e.as_ref().and_then(|e| e.eval(100.0, &[0.0; 4])).map(|v| clamp_f64(v, 0.0, 100.0) / 100.0);
                let (q1, q2) = (pct(&m.p1), pct(&m.p2));
                let (mut p1, mut p2) = (q1.unwrap_or(0.5), q2.unwrap_or(0.5));
                if q1.is_some() && q2.is_none() {
                    p2 = 1.0 - p1;
                } else if q2.is_some() && q1.is_none() {
                    p1 = 1.0 - p2;
                }
                if p1 == 0.0 && p2 == 0.0 {
                    return Color::from_color_space(Space::SrgbLegacy, Some(0.0), Some(0.0), Some(0.0), Some(0.0));
                }
                let mut alpha_multiplier = 1.0;
                let scale = p1 + p2;
                if scale != 0.0 {
                    p1 /= scale;
                    p2 /= scale;
                    if scale <= 1.0 {
                        alpha_multiplier = scale;
                    }
                }
                let _ = p1;
                from_color_mix(m.space, m.hue, m.c1.operand(cx), m.c2.operand(cx), p2 as f32, alpha_multiplier as f32)
            }
            ColorValue::Relative(r) => {
                // `UnresolvedRelativeColor::Resolve`.
                let mut origin = r.origin.operand(cx);
                origin.convert_to(r.space);
                let mut keys = [origin.params[0] as f64, origin.params[1] as f64, origin.params[2] as f64, origin.alpha as f64];
                // `keyword_values[i].second *= 100.`: a float scaled in double.
                if matches!(r.space, Space::Hsl | Space::Hwb) {
                    keys[1] = (keys[1] * 100.0) as f32 as f64;
                    keys[2] = (keys[2] * 100.0) as f32 as f64;
                }
                let base = r.space.percent_base();
                let limit = |b: f64| if b.is_nan() { f32::MAX as f64 } else { b };
                let mut params: [Option<f64>; 3] = [None; 3];
                for i in 0..3 {
                    params[i] = r.ch[i].eval(limit(base[i]), &keys);
                }
                // An alpha left out is the origin's.
                let mut alpha = match &r.alpha {
                    None => Some(keys[3]),
                    Some(e) => e.eval(1.0, &keys),
                };
                adjust_channels(r.space, false, true, &mut params, &mut alpha);
                Color::from_color_space(r.space, params[0].map(|v| v as f32), params[1].map(|v| v as f32), params[2].map(|v| v as f32), alpha.map(|v| v as f32))
            }
        }
    }

    /// Its specified value's text (`CSSValue::CssText`).
    pub fn css_text(&self) -> String {
        match self {
            ColorValue::Color(c, calc) if calc.iter().any(|x| *x) => {
                // An unresolved lightness-first colour: its channels as written, a `calc()` as its value.
                let mut out = format!("{}(", c.space.name());
                for i in 0..3 {
                    if i > 0 {
                        out.push(' ');
                    }
                    out += &if c.params_none[i] {
                        "none".to_string()
                    } else if calc[i] {
                        format!("calc({})", param_to_string(c.params[i] as f64, 6))
                    } else {
                        param_to_string(c.params[i] as f64, 6)
                    };
                }
                if c.alpha != 1.0 || c.alpha_none || calc[3] {
                    out += " / ";
                    out += &if c.alpha_none { "none".to_string() } else if calc[3] { format!("calc({})", to_precision(c.alpha as f64, 6)) } else { to_precision(c.alpha as f64, 6) };
                }
                out + ")"
            }
            ColorValue::Color(c, _) => c.serialize_as_css_color(),
            ColorValue::Keyword(k) => k.clone(),
            ColorValue::CurrentColor => "currentcolor".into(),
            ColorValue::Contrast(c) => format!("contrast-color({})", c.css_text()),
            ColorValue::Mix(m) => {
                let mut out = String::from("color-mix(");
                if !(m.space == Space::Oklab && m.hue == Hue::Shorter) {
                    out += "in ";
                    out += interpolation_space_name(m.space);
                    if matches!(m.space, Space::Hsl | Space::Hwb | Space::Lch | Space::Oklch) {
                        out += match m.hue {
                            Hue::Decreasing => " decreasing hue",
                            Hue::Increasing => " increasing hue",
                            Hue::Longer => " longer hue",
                            Hue::Shorter => "",
                        };
                    }
                    out += ", ";
                }
                // `PercentageValuesForSerialization`.
                let lit = |e: &Option<Expr>| match e {
                    Some(Expr::Pct(v)) => Some(*v),
                    _ => None,
                };
                let (mut p1, mut p2) = (m.p1.clone(), m.p2.clone());
                match (lit(&m.p1), lit(&m.p2), m.p1.is_some(), m.p2.is_some()) {
                    (Some(a), Some(z), _, _) if a == 50.0 && z == 50.0 => (p1, p2) = (None, None),
                    (Some(a), Some(z), _, _) if a + z == 100.0 => p2 = None,
                    (Some(a), None, true, false) if a == 50.0 => (p1, p2) = (None, None),
                    (None, Some(z), false, true) if z == 50.0 => p2 = None,
                    _ => {}
                }
                out += &m.c1.css_text();
                if let Some(p) = &p1 {
                    out += " ";
                    out += &p.css_text();
                }
                out += ", ";
                out += &m.c2.css_text();
                if let Some(p) = &p2 {
                    out += " ";
                    out += &p.css_text();
                }
                out + ")"
            }
            ColorValue::Relative(r) => {
                let mut out = format!("{}(from {}", r.name, r.origin.css_text());
                if r.name == "color" {
                    out += " ";
                    out += r.space.name();
                }
                for c in &r.ch {
                    out += " ";
                    out += &c.css_text();
                }
                if let Some(a) = &r.alpha {
                    out += " / ";
                    out += &a.css_text();
                }
                out + ")"
            }
        }
    }

    /// Whether resolving it needs the current colour.
    pub fn uses_current_color(&self) -> bool {
        match self {
            ColorValue::CurrentColor => true,
            ColorValue::Mix(m) => m.c1.uses_current_color() || m.c2.uses_current_color(),
            ColorValue::Relative(r) => r.origin.uses_current_color(),
            ColorValue::Contrast(c) => c.uses_current_color(),
            _ => false,
        }
    }
}

/// A named colour keyword or a system colour.
fn keyword_color(k: &str, element: bool) -> Option<Color> {
    if k == "transparent" {
        return Some(Color::from_rgba32(0));
    }
    if let Ok((r, g, b)) = cssparser::color::parse_named_color(k) {
        return Some(Color::from_rgba(r as i32, g as i32, b as i32, 255));
    }
    if element { css_system_color(k) } else { system_color(k, false) }
}

/// `Color::SerializeInterpolationSpace` (without the hue method).
fn interpolation_space_name(s: Space) -> &'static str {
    match s {
        Space::SrgbLegacy | Space::Srgb => "srgb",
        other => other.name(),
    }
}

/// A whole string as one `<color>` value (`ConsumeColor` for a property), `None` when it is not one.
pub fn parse_color_value(s: &str) -> Option<ColorValue> {
    let mut input = ParserInput::new(s);
    let mut p = Parser::new(&mut input);
    let v = consume_color_value(&mut p)?;
    p.is_exhausted().then_some(v)
}

/// A colour string resolved where `currentcolor` is `current` -- a computed value, or a canvas colour that is a
/// function `CSSParser::ParseColor` leaves unresolved (`color-mix()`, a relative colour, `calc()` in `lab()`).
pub fn resolve_color(s: &str, cx: &Context) -> Option<Color> {
    parse_color_value(s).map(|v| v.resolve(cx))
}

// ---- numbers as Blink prints them ----------------------------------------------------------------------

/// `ColorParamToString`: a non-finite parameter as `calc(...)`.
fn param_to_string(v: f64, precision: usize) -> String {
    if v.is_finite() {
        to_precision(v, precision)
    } else if v.is_nan() {
        "calc(NaN)".to_string()
    } else if v < 0.0 {
        "calc(-infinity)".to_string()
    } else {
        "calc(infinity)".to_string()
    }
}

/// `DoubleToStringConverter::ToStringWithFixedPrecision`: ECMAScript `toPrecision` (ties away from zero; fixed
/// notation for exponents -6 .. precision-1, else `d.ddddde-x` with its zeros kept), then the fixed form's trailing
/// zeros stripped.
pub fn to_precision(v: f64, precision: usize) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let p = precision.max(1);
    let (digits, decimal_point) = if v == 0.0 {
        (vec![b'0'; p], 1i32)
    } else {
        // Exact digits well past the cut, so a tie (`...5000...`) is told from a near-tie.
        let sci = format!("{:.*e}", p - 1 + 60, v.abs());
        let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
        let mut exp: i32 = exp.parse().unwrap_or(0);
        let all: Vec<u8> = mant.bytes().filter(|c| c.is_ascii_digit()).collect();
        let mut d = all[..p].to_vec();
        if all[p] >= b'5' {
            let mut i = p;
            loop {
                if i == 0 {
                    d.insert(0, b'1');
                    d.truncate(p);
                    exp += 1;
                    break;
                }
                i -= 1;
                if d[i] == b'9' {
                    d[i] = b'0';
                } else {
                    d[i] += 1;
                    break;
                }
            }
        }
        (d, exp + 1)
    };
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    let exponent = decimal_point - 1;
    let pi = p as i32;
    if 1 - decimal_point > 6 || decimal_point - pi > 0 {
        out.push(digits[0] as char);
        if p > 1 {
            out.push('.');
            out.extend(digits[1..].iter().map(|&c| c as char));
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        out += &exponent.abs().to_string();
        return out;
    }
    let digits_after_point = (pi - decimal_point).max(0) as usize;
    if decimal_point <= 0 {
        out.push('0');
        if digits_after_point > 0 {
            out.push('.');
            out.extend(std::iter::repeat('0').take((-decimal_point) as usize));
            out.extend(digits.iter().map(|&c| c as char));
        }
    } else {
        let dp = decimal_point as usize;
        out.extend(digits[..dp.min(p)].iter().map(|&c| c as char));
        if dp < p {
            out.push('.');
            out.extend(digits[dp..].iter().map(|&c| c as char));
        }
    }
    if out.contains('.') {
        let trimmed = out.trim_end_matches('0').trim_end_matches('.').len();
        out.truncate(trimmed);
    }
    out
}

// ---- the conversions (ui/gfx/color_conversions.cc, skia/ext/skcms_ext.cc, modules/skcms) --------------

pub type V3 = [f32; 3];
type M33 = [[f32; 3]; 3];

/// `skcms_TransferFunction`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tf {
    g: f32,
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

const TF_SRGB: Tf = Tf { g: 2.4, a: (1.0 / 1.055) as f32, b: (0.055 / 1.055) as f32, c: (1.0 / 12.92) as f32, d: 0.04045, e: 0.0, f: 0.0 };
const TF_LINEAR: Tf = Tf { g: 1.0, a: 1.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0, f: 0.0 };
const TF_2DOT2: Tf = Tf { g: 2.2, a: 1.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0, f: 0.0 };
const TF_REC2020: Tf = Tf { g: 2.22222, a: 0.909672, b: 0.0903276, c: 0.222222, d: 0.0812429, e: 0.0, f: 0.0 };
const TF_PROPHOTO: Tf = Tf { g: 1.8, a: 1.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0, f: 0.0 };

/// `SkFixedToFloat`.
const fn fx(x: i32) -> f32 {
    x as f32 * 1.52587890625e-5
}

const IDENTITY: M33 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const GAMUT_SRGB: M33 = [[fx(0x6FA2), fx(0x6299), fx(0x24A0)], [fx(0x38F5), fx(0xB785), fx(0x0F84)], [fx(0x0390), fx(0x18DA), fx(0xB6CF)]];
const GAMUT_ADOBE_RGB: M33 = [[fx(0x9c18), fx(0x348d), fx(0x2631)], [fx(0x4fa5), fx(0xa02c), fx(0x102f)], [fx(0x04fc), fx(0x0f95), fx(0xbe9c)]];
const GAMUT_DISPLAY_P3: M33 = [[0.515102, 0.291965, 0.157153], [0.241182, 0.692236, 0.0665819], [-0.00104941, 0.0418818, 0.784378]];
const GAMUT_REC2020: M33 = [[0.673459, 0.165661, 0.125100], [0.279033, 0.675338, 0.0456288], [-0.00193139, 0.0299794, 0.797162]];

/// `std::max(a, b)` -- `a` unless `a < b` (so a NaN `a` survives).
fn max_f(a: f32, b: f32) -> f32 {
    if a < b { b } else { a }
}
/// `std::min(a, b)` -- `a` unless `b < a`.
fn min_f(a: f32, b: f32) -> f32 {
    if b < a { b } else { a }
}
fn max_f64(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}
fn min_f64(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}
/// `ClampTo`: `max` when `v >= max`, `min` when `v <= min`, else `v` (a NaN passes through).
fn clamp_f32(v: f32, min: f32, max: f32) -> f32 {
    if v >= max { max } else if v <= min { min } else { v }
}
fn clamp_f64(v: f64, min: f64, max: f64) -> f64 {
    if v >= max { max } else if v <= min { min } else { v }
}

/// `skcms::Matrix3x3_apply` / skcms's `mv_mul`.
fn m_apply(m: &M33, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// `skcms_Matrix3x3_concat`.
fn concat(a: &M33, b: &M33) -> M33 {
    let mut m = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            m[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    m
}

/// `skcms_Matrix3x3_invert`, in double as skcms computes it.
fn m_invert(src: &M33) -> Option<M33> {
    let (a00, a01, a02) = (src[0][0] as f64, src[1][0] as f64, src[2][0] as f64);
    let (a10, a11, a12) = (src[0][1] as f64, src[1][1] as f64, src[2][1] as f64);
    let (a20, a21, a22) = (src[0][2] as f64, src[1][2] as f64, src[2][2] as f64);
    let (mut b0, mut b1, mut b2) = (a00 * a11 - a01 * a10, a00 * a12 - a02 * a10, a01 * a12 - a02 * a11);
    let (mut b3, mut b4, mut b5) = (a20, a21, a22);
    let determinant = b0 * b5 - b1 * b4 + b2 * b3;
    if determinant == 0.0 {
        return None;
    }
    let invdet = 1.0 / determinant;
    if invdet > f32::MAX as f64 || invdet < -(f32::MAX as f64) || (invdet as f32) * 0.0 != 0.0 {
        return None;
    }
    b0 *= invdet;
    b1 *= invdet;
    b2 *= invdet;
    b3 *= invdet;
    b4 *= invdet;
    b5 *= invdet;
    let d = [
        [(a11 * b5 - a12 * b4) as f32, (a12 * b3 - a10 * b5) as f32, (a10 * b4 - a11 * b3) as f32],
        [(a02 * b4 - a01 * b5) as f32, (a00 * b5 - a02 * b3) as f32, (a01 * b3 - a00 * b4) as f32],
        [b2 as f32, (-b1) as f32, b0 as f32],
    ];
    d.iter().flatten().all(|v| v * 0.0 == 0.0).then_some(d)
}

/// `skcms::Matrix3x3_apply_inverse`: the identity as is, else through the inverse.
fn m_apply_inverse(m: &M33, v: V3) -> V3 {
    if *m == IDENTITY {
        return v;
    }
    m_apply(&m_invert(m).unwrap_or(IDENTITY), v)
}

/// `skcms_AdaptToXYZD50`: Bradford adaptation from white point (wx, wy) to D50, in float.
fn adapt_to_xyz_d50(wx: f32, wy: f32) -> M33 {
    let w = [wx / wy, 1.0, (1.0 - wx - wy) / wy];
    let w_d50 = [0.96422f32, 1.0, 0.82521];
    const XYZ_TO_LMS: M33 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    const LMS_TO_XYZ: M33 = [[0.9869929, -0.1470543, 0.1599627], [0.4323053, 0.5183603, 0.0492912], [-0.0085287, 0.0400428, 0.9684867]];
    let src = m_apply(&XYZ_TO_LMS, w);
    let dst = m_apply(&XYZ_TO_LMS, w_d50);
    let m = [[dst[0] / src[0], 0.0, 0.0], [0.0, dst[1] / src[1], 0.0], [0.0, 0.0, dst[2] / src[2]]];
    let m = concat(&m, &XYZ_TO_LMS);
    concat(&LMS_TO_XYZ, &m)
}

/// `skcms_PrimariesToXYZD50`.
#[allow(clippy::too_many_arguments)]
fn primaries_to_xyz_d50(rx: f32, ry: f32, gx: f32, gy: f32, bx: f32, by: f32, wx: f32, wy: f32) -> M33 {
    let primaries = [[rx, gx, bx], [ry, gy, by], [1.0 - rx - ry, 1.0 - gx - gy, 1.0 - bx - by]];
    let inv = m_invert(&primaries).unwrap_or(IDENTITY);
    let w = [wx / wy, 1.0, (1.0 - wx - wy) / wy];
    let xyz = m_apply(&inv, w);
    let to_xyz = concat(&primaries, &[[xyz[0], 0.0, 0.0], [0.0, xyz[1], 0.0], [0.0, 0.0, xyz[2]]]);
    concat(&adapt_to_xyz_d50(wx, wy), &to_xyz)
}

/// skia/ext's `TransferFunction_eval`: skcms's piecewise function with `std::pow`, odd around zero.
fn tf_eval(f: &Tf, x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0f32 } else { 1.0 };
    let ax = x.abs();
    if ax <= f.d {
        return sign * (f.c * ax + f.f);
    }
    let base = max_f(f.a * ax + f.b, 0.0);
    if f.g == 1.0 {
        return sign * (base + f.e);
    }
    sign * (base.powf(f.g) + f.e)
}

/// skia/ext's `TransferFunction_invert`: `skcms_TransferFunction_invert` with `std::pow`.
fn tf_invert(src: &Tf) -> Option<Tf> {
    let mut inv = Tf { g: 0.0, a: 0.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0, f: 0.0 };
    let d_l = src.c * src.d + src.f;
    let d_r = (src.a * src.d + src.b).powf(src.g) + src.e;
    if (d_l - d_r).abs() > 1.0 / 512.0 {
        return None;
    }
    inv.d = d_l;
    if inv.d > 0.0 {
        inv.c = 1.0 / src.c;
        inv.f = -src.f / src.c;
    }
    let k = src.a.powf(-src.g);
    inv.g = 1.0 / src.g;
    inv.a = k;
    inv.b = -k * src.e;
    inv.e = -src.b / src.a;
    if inv.a < 0.0 {
        return None;
    }
    if inv.a * inv.d + inv.b < 0.0 {
        inv.b = -inv.a * inv.d;
    }
    let mut s = tf_eval(src, 1.0);
    if !s.is_finite() {
        return None;
    }
    let sign = if s < 0.0 { -1.0f32 } else { 1.0 };
    s *= sign;
    if s < inv.d {
        inv.f = 1.0 - sign * inv.c * s;
    } else {
        inv.e = 1.0 - sign * (inv.a * s + inv.b).powf(inv.g);
    }
    Some(inv)
}

fn tf_apply(t: &Tf, v: V3) -> V3 {
    [tf_eval(t, v[0]), tf_eval(t, v[1]), tf_eval(t, v[2])]
}

/// `skcms::TransferFunction_apply_inverse`: the linear function as is, else through the precise inverse.
fn tf_apply_inverse(t: &Tf, v: V3) -> V3 {
    if *t == TF_LINEAR {
        return v;
    }
    tf_apply(&tf_invert(t).unwrap_or(TF_LINEAR), v)
}

// skcms's own approximations (`powf_` = `exp2f_(log2f_(x) * y)`), which `srgb-linear` goes through.

fn floorf_(x: f32) -> f32 {
    let roundtrip = (x as i32) as f32;
    if roundtrip > x { roundtrip - 1.0 } else { roundtrip }
}

fn log2f_(x: f32) -> f32 {
    let bits = x.to_bits() as i32;
    let e = bits as f32 * (1.0f32 / (1 << 23) as f32);
    let m = f32::from_bits(((bits & 0x007f_ffff) | 0x3f00_0000) as u32);
    e - 124.225514990 - 1.498030302 * m - 1.725879990 / (0.3520887068 + m)
}

fn exp2f_(x: f32) -> f32 {
    if x > 128.0 {
        return f32::INFINITY;
    } else if x < -127.0 {
        return 0.0;
    }
    let fract = x - floorf_(x);
    let fbits = (1.0f32 * (1 << 23) as f32) * (x + 121.274057500 - 1.490129070 * fract + 27.728023300 / (4.84252568 - fract));
    if fbits >= i32::MAX as f32 {
        return f32::INFINITY;
    } else if fbits < 0.0 {
        return 0.0;
    }
    f32::from_bits(fbits as i32 as u32)
}

fn powf_(x: f32, y: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x == 1.0 {
        return 1.0;
    }
    exp2f_(log2f_(x) * y)
}

/// skcms `classify(...) == sRGBish` for a function without the HDR markers.
fn srgbish(t: &Tf) -> bool {
    let sum = t.a + t.b + t.c + t.d + t.e + t.f + t.g;
    t.g >= 0.0 && sum * 0.0 == 0.0 && t.a >= 0.0 && t.c >= 0.0 && t.d >= 0.0 && t.a * t.d + t.b >= 0.0
}

/// `skcms_TransferFunction_eval` on an sRGB-ish function.
fn skcms_eval(t: &Tf, x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0f32 } else { 1.0 };
    let x = x * sign;
    sign * if x < t.d { t.c * x + t.f } else { powf_(t.a * x + t.b, t.g) + t.e }
}

/// `skcms_TransferFunction_invert` on an sRGB-ish function.
fn skcms_invert(src: &Tf) -> Option<Tf> {
    let mut inv = Tf { g: 0.0, a: 0.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0, f: 0.0 };
    let d_l = src.c * src.d + src.f;
    let d_r = powf_(src.a * src.d + src.b, src.g) + src.e;
    if (d_l - d_r).abs() > 1.0 / 512.0 {
        return None;
    }
    inv.d = d_l;
    if inv.d > 0.0 {
        inv.c = 1.0 / src.c;
        inv.f = -src.f / src.c;
    }
    let k = powf_(src.a, -src.g);
    inv.g = 1.0 / src.g;
    inv.a = k;
    inv.b = -k * src.e;
    inv.e = -src.b / src.a;
    if inv.a < 0.0 {
        return None;
    }
    if inv.a * inv.d + inv.b < 0.0 {
        inv.b = -inv.a * inv.d;
    }
    if !srgbish(&inv) {
        return None;
    }
    let mut s = skcms_eval(src, 1.0);
    if s * 0.0 != 0.0 {
        return None;
    }
    let sign = if s < 0.0 { -1.0f32 } else { 1.0 };
    s *= sign;
    if s < inv.d {
        inv.f = 1.0 - sign * inv.c * s;
    } else {
        inv.e = 1.0 - sign * powf_(inv.a * s + inv.b, inv.g);
    }
    srgbish(&inv).then_some(inv)
}

/// `SRGBLegacyToSRGB`: `r / 255.0` in double, back to float.
fn srgb_legacy_to_srgb(r: f32, g: f32, b: f32) -> V3 {
    [(r as f64 / 255.0) as f32, (g as f64 / 255.0) as f32, (b as f64 / 255.0) as f32]
}

fn xyz_d50_to_srgb(c: V3) -> V3 {
    tf_apply_inverse(&TF_SRGB, m_apply_inverse(&GAMUT_SRGB, c))
}

fn srgb_to_xyz_d50(c: V3) -> V3 {
    m_apply(&GAMUT_SRGB, tf_apply(&TF_SRGB, c))
}

const D50: V3 = [0.9642, 1.0, 0.8251];

fn lab_to_xyz_d50(l: f32, a: f32, b: f32) -> V3 {
    let y = (l + 16.0) / 116.0;
    let x = y + a / 500.0;
    let z = y - b / 200.0;
    let inv = |t: f32| -> f32 {
        const DELTA: f32 = 24.0 / 116.0;
        if t <= DELTA {
            (108.0f32 / 841.0) * (t - (16.0f32 / 116.0))
        } else {
            t * t * t
        }
    };
    [inv(x) * D50[0], inv(y) * D50[1], inv(z) * D50[2]]
}

const OKLAB_TO_LMS: M33 = [
    [0.99999999845051981432, 0.39633779217376785678, 0.21580375806075880339],
    [1.0000000088817607767, -0.1055613423236563494, -0.063854174771705903402],
    [1.0000000546724109177, -0.089484182094965759684, -1.2914855378640917399],
];
const XYZD65_TO_LMS: M33 = [
    [0.8190224432164319, 0.3619062562801221, -0.12887378261216414],
    [0.0329836671980271, 0.9292868468965546, 0.03614466816999844],
    [0.048177199566046255, 0.26423952494422764, 0.6335478258136937],
];

/// `OklabToXYZD50(..., gamut_map = false)`.
fn oklab_to_xyz_d50(l: f32, a: f32, b: f32) -> V3 {
    let lms = m_apply(&OKLAB_TO_LMS, [l, a, b]);
    let cubed = [lms[0] * lms[0] * lms[0], lms[1] * lms[1] * lms[1], lms[2] * lms[2] * lms[2]];
    let xyz_d65 = m_apply_inverse(&XYZD65_TO_LMS, cubed);
    m_apply(&adapt_to_xyz_d50(0.3127, 0.3290), xyz_d65)
}

/// `XYZD50ToOklab`.
fn xyz_d50_to_oklab(xyz: V3) -> V3 {
    let xyz_d65 = m_apply_inverse(&adapt_to_xyz_d50(0.3127, 0.3290), xyz);
    let cubed = m_apply(&XYZD65_TO_LMS, xyz_d65);
    let pow_ext = |x: f32| if x < 0.0 { -(-x).powf(1.0 / 3.0) } else { x.powf(1.0 / 3.0) };
    m_apply_inverse(&OKLAB_TO_LMS, [pow_ext(cubed[0]), pow_ext(cubed[1]), pow_ext(cubed[2])])
}

/// `XYZD50ToLab`.
fn xyz_d50_to_lab(c: V3) -> V3 {
    let f = |t: f32| -> f32 {
        const LIMIT: f32 = (24.0f32 / 116.0) * (24.0f32 / 116.0) * (24.0f32 / 116.0);
        if t <= LIMIT { (841.0f32 / 108.0) * t + (16.0f32 / 116.0) } else { t.powf(1.0 / 3.0) }
    };
    let (x, y, z) = (f(c[0] / D50[0]), f(c[1] / D50[1]), f(c[2] / D50[2]));
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// `LabToLch`, with `base::RadToDeg` in float.
fn lab_to_lch(c: V3) -> V3 {
    let [l, a, b] = c;
    [l, (a * a + b * b).sqrt(), b.atan2(a) * 180.0 / std::f32::consts::PI]
}

/// `std::minmax({r, g, b})`.
fn min_max(c: V3) -> (f32, f32) {
    let mut lo = c[0];
    let mut hi = c[0];
    for &v in &c[1..] {
        if v < lo {
            lo = v;
        }
        if !(v < hi) {
            hi = v;
        }
    }
    (lo, hi)
}

/// `SRGBToHSL`.
fn srgb_to_hsl(c: V3) -> V3 {
    let [r, g, b] = c;
    let (min, max) = min_max(c);
    let (mut hue, mut saturation) = (0.0f32, 0.0f32);
    let lightness = (min + max) / 2.0;
    let d = max - min;
    if d != 0.0 {
        saturation = if lightness == 0.0 || lightness == 1.0 { 0.0 } else { (max - lightness) / min_f(lightness, 1.0 - lightness) };
        hue = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        hue *= 60.0;
    }
    if saturation < 0.0 {
        hue += 180.0;
        saturation = saturation.abs();
    }
    if hue >= 360.0 {
        hue -= 360.0;
    }
    [hue, saturation, lightness]
}

/// `SRGBToHWB` (`SRGBToHue` for the hue).
fn srgb_to_hwb(c: V3) -> V3 {
    let [r, g, b] = c;
    let (min, max) = min_max(c);
    let mut hue = 0.0f32;
    let d = max - min;
    if d != 0.0 {
        hue = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        hue *= 60.0;
    }
    if hue >= 360.0 {
        hue -= 360.0;
    }
    [hue, min, 1.0 - max]
}

/// `LchToLab`, with `base::DegToRad` in float.
fn lch_to_lab(l: f32, c: f32, h: f32) -> V3 {
    let rad = h * std::f32::consts::PI / 180.0;
    [l, c * rad.cos(), c * rad.sin()]
}

/// `HSLToSRGB` (CSS Color 4's `hslToRgb`).
fn hsl_to_srgb(h: f32, s: f32, l: f32) -> V3 {
    if s == 0.0 {
        return [l, l, l];
    }
    let f = |n: f32| -> f32 {
        let k = ((n + h / 30.0) as f64 % 12.0) as f32;
        let a = s * min_f(l, 1.0 - l);
        let (x, y) = (k - 3.0, 9.0 - k);
        let mut m = x;
        if y < m {
            m = y;
        }
        if 1.0 < m {
            m = 1.0;
        }
        l - a * max_f(-1.0, m)
    };
    [f(0.0), f(8.0), f(4.0)]
}

/// `HWBToSRGB`.
fn hwb_to_srgb(h: f32, w: f32, b: f32) -> V3 {
    if w + b >= 1.0 {
        let gray = w / (w + b);
        return [gray, gray, gray];
    }
    let [mut r, mut g, mut bl] = hsl_to_srgb(h, 1.0, 0.5);
    r += w - (w + b) * r;
    g += w - (w + b) * g;
    bl += w - (w + b) * bl;
    [r, g, bl]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(s: &str) -> Option<String> {
        match parse_canvas_color(s, false)? {
            CanvasParse::Color(c) => Some(c.serialize_as_canvas_color()),
            CanvasParse::CurrentColor => Some("currentcolor".into()),
            CanvasParse::Function(v) => Some(v.resolve(&Context { current: Color::black(), element: false }).serialize_as_canvas_color()),
        }
    }

    /// `ctx.fillStyle = s; ctx.fillStyle` on an `OffscreenCanvas`, stock Chrome 150 (scratch col/p.js).
    #[test]
    fn fill_style_reads_back_as_chrome_serializes_it() {
        let cases = [
            ("#ff8000", "#ff8000"), ("#F80", "#ff8800"), ("#ff800080", "rgba(255, 128, 0, 0.5)"),
            ("#f808", "rgba(255, 136, 0, 0.533)"), ("violetred", "#d02090"), ("lightslateblue", "#8470ff"),
            ("transparent", "rgba(0, 0, 0, 0)"), (" red ", "#ff0000"), ("\tred\n", "#ff0000"),
            ("rgb(10.5, 200.4, 30.6)", "#0bc81f"), ("rgb(10.5,200.5,30.5)", "#0bc91f"), ("rgb(1e1, 2e2, 3e1)", "#0ac81e"),
            ("RGB(10.5, 200.4, 30.6)", "#0bc81f"), ("rgb(10 200 30 / 0.5)", "rgba(10, 200, 30, 0.5)"),
            ("rgba(0, 0, 0, 0.503)", "rgba(0, 0, 0, 0.5)"), ("rgba(0,0,0,0.123)", "rgba(0, 0, 0, 0.12)"),
            ("rgba(0,0,0,0.1234567)", "rgba(0, 0, 0, 0.12)"), ("rgba(0, 0, 0, 0.5 )", "rgba(0, 0, 0, 0.5)"),
            ("rgba(0,0,0,2)", "#000000"), ("rgba(0,0,0,-1)", "rgba(0, 0, 0, 0)"), ("rgb(300, -20, 0)", "#ff0000"),
            ("rgb(50%, 20%, 100%)", "#8033ff"), ("rgb(33.3%, 66.7%, 99.9%)", "#55aaff"), ("rgb(50% 20 10)", "#80140a"),
            ("rgb(none 20 30)", "#00141e"), ("rgb(10, 20, 30, 0.5)", "rgba(10, 20, 30, 0.5)"),
            ("rgb(255 255 255 / none)", "rgba(255, 255, 255, 0)"),
            ("hsl(120, 50%, 50%)", "#40bf40"), ("hsl(120deg 50% 50% / 0.25)", "rgba(64, 191, 64, 0.25)"),
            ("hsl(2rad, 50%, 50%)", "#4bbf40"), ("hsl(0.5turn 100% 25%)", "#008080"), ("hsl(-60, 100%, 50%)", "#ff00ff"),
            ("hsl(400, 100%, 50%)", "#ffaa00"), ("hsla(210, 60%, 40%, 0.7)", "rgba(41, 102, 163, 0.7)"),
            ("hsl(210 60 40)", "#2966a3"), ("HSL(120, 50%, 50%)", "#40bf40"), ("hsl(120.5, 33.3%, 66.6%)", "#8dc68e"),
            ("hsl(none 50% 50%)", "#bf4040"), ("hwb(120 20% 30%)", "#33b333"), ("hwb(120 60% 60%)", "#808080"),
            ("hwb(30deg 10% 20% / 0.4)", "rgba(204, 115, 26, 0.4)"),
            ("lab(50 40 30)", "lab(50 40 30)"), ("lab(50% 40% -30%)", "lab(50 50 -37.5)"), ("lab(150 0 0)", "lab(100 0 0)"),
            ("lab(50 40 30 / 0.3)", "lab(50 40 30 / 0.3)"), ("lab(none 40 30)", "lab(none 40 30)"),
            ("lch(70 30 400)", "lch(70 30 40)"), ("lch(50 -10 30)", "lch(50 0 30)"), ("oklab(70% 25% 0)", "oklab(0.7 0.1 0)"),
            ("oklch(0.5 0.4 30deg / 50%)", "oklch(0.5 0.4 30 / 0.5)"), ("color(srgb 20% 40% 60%)", "color(srgb 0.2 0.4 0.6)"),
            ("color(srgb 1.2 -0.1 0.5)", "color(srgb 1.2 -0.1 0.5)"), ("color(display-p3 1 0.25 0.5)", "color(display-p3 1 0.25 0.5)"),
            ("color(display-p3-linear 0.5 0.5 0.5)", "color(display-p3-linear 0.5 0.5 0.5)"),
            ("color(xyz 0.2 0.4 0.6)", "color(xyz-d65 0.2 0.4 0.6)"),
            ("color(display-p3 1 0.25 0.5 / 0.5)", "color(display-p3 1 0.25 0.5 / 0.5)"),
            ("color(display-p3 none 0.25 0.5)", "color(display-p3 none 0.25 0.5)"),
            ("color(display-p3 0.123456789 0.25 0.5)", "color(display-p3 0.123457 0.25 0.5)"),
            ("color(srgb 1 0.000001 0.5)", "color(srgb 1 0.000001 0.5)"), ("color(srgb 1 1e-7 0.5)", "color(srgb 1 1.00000e-7 0.5)"),
            ("canvas", "#ffffff"), ("menu", "#ffffff"), ("highlight", "rgba(0, 56, 238, 0.8)"), ("buttonborder", "#767676"),
        ];
        for (input, want) in cases {
            assert_eq!(canvas(input).as_deref(), Some(want), "fillStyle = {input:?}");
        }
        assert_eq!(canvas("currentColor").as_deref(), Some("currentcolor"));
    }

    /// What Chrome 150 refuses (the assignment is ignored).
    #[test]
    fn what_chrome_refuses_is_not_a_colour() {
        for s in ["#12", "#1234567", "accentcolor", "-webkit-link", "text", "red/**/", "/**/red", "rgb(10, 20 30)",
                  "hsl(210, 60, 40)", "hwb(120, 20%, 30%)", "color(rec2100-linear 0.2 0.4 0.6)", "color(display-p3 1, 0.25, 0.5)",
                  "light-dark(red, blue)", "notacolor", "", "rgb(", "#zzzzzz"] {
            assert_eq!(canvas(s), None, "{s:?}");
        }
    }

    /// The colour a float16 canvas stores is `toSkColor4f`'s, to a half's precision (stock Chrome 150: the sRGB
    /// float16 canvas of col/p.js) -- the display-p3 green lands at 0.10602 through Blink's rounded matrices.
    #[test]
    fn paint_colours_are_blinks_floats() {
        // The half below (toward zero), as Chrome's accelerated float16 canvas stores a paint colour: Skia hands the
        // GPU a wide colour as halves through `skvx::to_half`, whose portable form truncates.
        let half = |v: f32| -> f32 { f32::from_bits(v.to_bits() & !0x1fff) };
        let cases: [(&str, [f32; 3]); 12] = [
            ("color(display-p3 1 0.25 0.5)", [1.087890625, 0.10601806640625, 0.4970703125]),
            ("color(display-p3 0 1 0)", [-0.51171875, 1.017578125, -0.310546875]),
            ("lab(50 40 30)", [0.73388671875, 0.343994140625, 0.276123046875]),
            ("lab(none 40 30)", [0.2181396484375, -0.10174560546875, -0.158447265625]),
            ("lch(50 40 30)", [0.6982421875, 0.3662109375, 0.34130859375]),
            ("oklab(0.7 0.1 -0.1)", [0.74951171875, 0.50732421875, 0.85595703125]),
            ("oklch(0.7 0.2 150)", [-0.11279296875, 0.748046875, 0.33056640625]),
            ("color(srgb-linear 0.2 0.4 0.6)", [0.484375, 0.6650390625, 0.79736328125]),
            ("color(a98-rgb 0.2 0.4 0.6)", [-0.1151123046875, 0.400390625, 0.6123046875]),
            ("color(prophoto-rgb 0.2 0.4 0.6)", [-0.42333984375, 0.50927734375, 0.689453125]),
            ("color(rec2020 0.2 0.4 0.6)", [-0.209716796875, 0.468505859375, 0.658203125]),
            ("color(xyz-d65 0.2 0.4 0.6)", [-0.55224609375, 0.78662109375, 0.77587890625]),
        ];
        for (input, want) in cases {
            let Some(CanvasParse::Color(c)) = parse_canvas_color(input, false) else { panic!("{input} did not parse") };
            let p = c.to_sk_color4f();
            assert_eq!([half(p[0]), half(p[1]), half(p[2])], want, "{input}: {p:?}");
        }
    }

    #[test]
    fn numbers_print_as_blink_prints_them() {
        for (v, want) in [(0.0, "0"), (1.0, "1"), (0.25, "0.25"), (0.123456789, "0.123457"), (1e-6, "0.000001"),
                          (1e-7, "1.00000e-7"), (1234567.0, "1.23457e+6"), (-37.5, "-37.5"), (100.0, "100"), (255.0, "255")] {
            assert_eq!(to_precision(v, 6), want, "{v}");
        }
        assert_eq!(to_precision(0.5f32 as f64, 2), "0.5");
        assert_eq!(to_precision(0.533f32 as f64, 3), "0.533");
    }
}
