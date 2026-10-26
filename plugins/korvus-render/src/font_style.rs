//! The render style Chrome 150 on Linux gives a font strike: fontconfig's answer for the family at the size.
//!
//! Blink asks the font service for a strike's style (`FontPlatformData::QuerySystemRenderStyle` ->
//! `FontServiceApp::FontRenderStyleForStrike` -> `gfx::GetFontRenderParams`): a fontconfig pattern of the
//! family Blink named, the size in whole pixels (the float size truncated by the IPC's `int`), the weight and
//! the slant, plus `scalable` and `fontwrapper SFNT` (`gfx::QueryFontconfig`); substituted and matched, and the
//! match's `antialias`, `autohint`, `embeddedbitmap`, `hinting` + `hintstyle` and `rgba` read off it. A font made
//! for a CHARACTER (the fallback by codepoint) has no family name, and takes the renderer's defaults instead --
//! the same query with no family and no size (`FcConfigSubstituteWithPat`). Without anti-aliasing the style is
//! full hinting and no subpixel rendering; at a device scale factor of 1 there is no subpixel positioning.
//!
//! Measured on this host (stock Chrome 150, 2026-10-10): the generic `sans-serif` resolves to Noto Sans CJK KR,
//! whose strike is slightly hinted -- Ubuntu's `69-language-selector-ja.conf` unhints only the JP face at 18px
//! or less, which a query for the GENERIC name would have matched.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_double, c_int, c_void, CString};
use std::sync::Mutex;

use crate::fontations::Hinting;

/// One strike's style, as Blink's `WebFontRenderStyle` holds it after the font service answered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderStyle {
    pub antialias: bool,
    pub hinting: Hinting,
    pub autohint: bool,
    pub bitmaps: bool,
    pub subpixel_rendering: bool,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct FcPattern {
    _p: [u8; 0],
}

#[cfg(target_os = "linux")]
#[link(name = "fontconfig")]
extern "C" {
    fn FcPatternCreate() -> *mut FcPattern;
    fn FcPatternDestroy(p: *mut FcPattern);
    fn FcPatternDuplicate(p: *const FcPattern) -> *mut FcPattern;
    fn FcPatternDel(p: *mut FcPattern, object: *const c_char) -> c_int;
    fn FcPatternAddString(p: *mut FcPattern, object: *const c_char, s: *const u8) -> c_int;
    fn FcPatternAddBool(p: *mut FcPattern, object: *const c_char, b: c_int) -> c_int;
    fn FcPatternAddDouble(p: *mut FcPattern, object: *const c_char, d: c_double) -> c_int;
    fn FcPatternAddInteger(p: *mut FcPattern, object: *const c_char, i: c_int) -> c_int;
    fn FcPatternGetBool(p: *const FcPattern, object: *const c_char, n: c_int, b: *mut c_int) -> c_int;
    fn FcPatternGetInteger(p: *const FcPattern, object: *const c_char, n: c_int, i: *mut c_int) -> c_int;
    fn FcConfigSubstitute(config: *mut c_void, p: *mut FcPattern, kind: c_int) -> c_int;
    fn FcConfigSubstituteWithPat(config: *mut c_void, p: *mut FcPattern, p_pat: *mut FcPattern, kind: c_int) -> c_int;
    fn FcDefaultSubstitute(p: *mut FcPattern);
    fn FcFontMatch(config: *mut c_void, p: *mut FcPattern, result: *mut c_int) -> *mut FcPattern;
}

#[cfg(target_os = "linux")]
const FC_RESULT_MATCH: c_int = 0;
#[cfg(target_os = "linux")]
const FC_MATCH_PATTERN: c_int = 0;
#[cfg(target_os = "linux")]
const FC_MATCH_FONT: c_int = 1;
#[cfg(target_os = "linux")]
const FC_SLANT_ROMAN: c_int = 0;
#[cfg(target_os = "linux")]
const FC_SLANT_ITALIC: c_int = 100;
#[cfg(target_os = "linux")]
const FC_WEIGHT_NORMAL: c_int = 80;
#[cfg(target_os = "linux")]
const FC_WEIGHT_BOLD: c_int = 200;
#[cfg(target_os = "linux")]
const FC_RGBA_RGB: c_int = 1;
#[cfg(target_os = "linux")]
const FC_RGBA_VBGR: c_int = 4;

/// `(family, pixel size, bold, italic)` -> its style; the defaults under `None`.
static CACHE: Mutex<Option<HashMap<Option<(String, i32, bool, bool)>, RenderStyle>>> = Mutex::new(None);

/// The style of a strike of `family` (`None`: a font made for a character) at `size` px.
pub(crate) fn render_style(family: Option<&str>, size: f32, bold: bool, italic: bool) -> RenderStyle {
    // the IPC carries the size as an `int` and refuses one outside `uint16`'s range
    let px = size as i32;
    let key = family.filter(|f| !f.is_empty() && (0..=u16::MAX as i32).contains(&px)).map(|f| (f.to_string(), px, bold, italic));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(hit) = map.get(&key) {
        return *hit;
    }
    let style = query(key.as_ref().map(|(f, px, b, i)| (f.as_str(), *px, *b, *i)));
    map.insert(key, style);
    style
}

#[cfg(target_os = "linux")]
/// `gfx::GetFontRenderParams` for one query, without the desktop's own defaults (fontconfig answers every
/// field here: the system configuration sets antialias and rgba, and `FcDefaultSubstitute` the rest).
fn query(q: Option<(&str, i32, bool, bool)>) -> RenderStyle {
    let mut out = RenderStyle { antialias: true, hinting: Hinting::Slight, autohint: false, bitmaps: true, subpixel_rendering: false };
    // SAFETY: fontconfig's documented C API; every pattern made here is destroyed before returning.
    unsafe {
        let p = FcPatternCreate();
        if p.is_null() {
            return out;
        }
        FcPatternAddBool(p, c"scalable".as_ptr(), 1);
        FcPatternAddString(p, c"fontwrapper".as_ptr(), c"SFNT".as_ptr() as *const u8);
        let (bold, italic) = match q {
            Some((family, px, bold, italic)) => {
                let Ok(name) = CString::new(family) else {
                    FcPatternDestroy(p);
                    return out;
                };
                FcPatternAddString(p, c"family".as_ptr(), name.as_ptr() as *const u8);
                if px > 0 {
                    FcPatternAddDouble(p, c"pixelsize".as_ptr(), px as c_double);
                }
                (bold, italic)
            }
            None => (false, false),
        };
        FcPatternAddInteger(p, c"slant".as_ptr(), if italic { FC_SLANT_ITALIC } else { FC_SLANT_ROMAN });
        FcPatternAddInteger(p, c"weight".as_ptr(), if bold { FC_WEIGHT_BOLD } else { FC_WEIGHT_NORMAL });
        FcConfigSubstitute(std::ptr::null_mut(), p, FC_MATCH_PATTERN);
        FcDefaultSubstitute(p);
        let r = if q.is_none() {
            let r = FcPatternDuplicate(p);
            if !r.is_null() {
                FcPatternDel(r, c"family".as_ptr());
                FcPatternDel(r, c"pixelsize".as_ptr());
                FcPatternDel(r, c"size".as_ptr());
                FcConfigSubstituteWithPat(std::ptr::null_mut(), r, p, FC_MATCH_FONT);
            }
            r
        } else {
            let mut result: c_int = 0;
            FcFontMatch(std::ptr::null_mut(), p, &mut result)
        };
        FcPatternDestroy(p);
        if r.is_null() {
            return out;
        }
        let get_bool = |name: &std::ffi::CStr| {
            let mut v: c_int = 0;
            (FcPatternGetBool(r, name.as_ptr(), 0, &mut v) == FC_RESULT_MATCH).then_some(v != 0)
        };
        let get_int = |name: &std::ffi::CStr| {
            let mut v: c_int = 0;
            (FcPatternGetInteger(r, name.as_ptr(), 0, &mut v) == FC_RESULT_MATCH).then_some(v)
        };
        if let Some(v) = get_bool(c"antialias") {
            out.antialias = v;
        }
        if let Some(v) = get_bool(c"autohint") {
            out.autohint = v;
        }
        if let Some(v) = get_bool(c"embeddedbitmap") {
            out.bitmaps = v;
        }
        if let Some(hinting) = get_bool(c"hinting") {
            let style = if hinting { get_int(c"hintstyle").unwrap_or(0) } else { 0 };
            out.hinting = match style {
                0 => Hinting::None,
                1 => Hinting::Slight,
                2 => Hinting::Normal,
                _ => Hinting::Full,
            };
        }
        if let Some(rgba) = get_int(c"rgba") {
            out.subpixel_rendering = (FC_RGBA_RGB..=FC_RGBA_VBGR).contains(&rgba);
        }
        FcPatternDestroy(r);
    }
    if !out.antialias {
        // "Cairo forces full hinting when antialiasing is disabled"
        out.hinting = Hinting::Full;
        out.subpixel_rendering = false;
    }
    out
}

/// macOS: Chrome renders text through CoreText, which is not a fontconfig query. A scalable face is drawn
/// with grayscale anti-aliasing, NO FreeType-style hinting (CoreText does its own light rendering), and no
/// LCD subpixel AA -- Chrome turned subpixel text off on Mac, so every strike is grayscale. This holds for
/// all strikes, named or character fallback, which is why the query argument does not enter.
#[cfg(target_os = "macos")]
fn query(_q: Option<(&str, i32, bool, bool)>) -> RenderStyle {
    RenderStyle { antialias: true, hinting: Hinting::None, autohint: false, bitmaps: false, subpixel_rendering: false }
}

/// Windows and any other host: the render style is DirectWrite's, not fontconfig's. Until that port lands,
/// a strike takes Blink's own defaults (antialias on, slight hinting) -- a placeholder, not Chrome's
/// ClearType answer, which the Windows font layer will supply.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn query(_q: Option<(&str, i32, bool, bool)>) -> RenderStyle {
    RenderStyle { antialias: true, hinting: Hinting::Slight, autohint: false, bitmaps: true, subpixel_rendering: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_family_is_asked_at_its_size_and_a_character_font_takes_the_defaults() {
        // this host's configuration (2026-10-10); another machine answers by its own
        if !std::path::Path::new("/etc/fonts/conf.d/69-language-selector-ja.conf").exists() {
            return;
        }
        assert_eq!(render_style(Some("Noto Sans CJK KR"), 16.0, false, false).hinting, Hinting::Slight);
        // the generic name matches the JP face, which this configuration unhints at 18px or less
        assert_eq!(render_style(Some("sans-serif"), 16.9, false, false).hinting, Hinting::None);
        assert_eq!(render_style(Some("sans-serif"), 19.0, false, false).hinting, Hinting::Slight);
        assert_eq!(render_style(None, 16.0, false, false).hinting, Hinting::Slight);
    }
}
