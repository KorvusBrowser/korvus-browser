//! A named font family to a font file, as Chrome 150 on Linux matches it.
//!
//! Blink makes a listed family's platform font through the font service (`FontCache::CreateTypeface` ->
//! `SkFontMgr_FCI::onMatchFamilyStyle` -> `FontServiceApp::MatchFamilyName`), whose answer is Skia's
//! `SkFontConfigInterfaceDirect::matchFamilyName` (`third_party/skia/src/ports/SkFontConfigInterface_direct.cpp`):
//!
//! * a pattern of the family, the style's fontconfig weight, width and slant, `scalable` and `fontwrapper SFNT`,
//!   substituted (`FcConfigSubstitute` + `FcDefaultSubstitute`) -- its first family is the POST-CONFIG family;
//! * `FcFontSort`'s first font that is an sfnt (`fontformat` TrueType or CFF: Chrome's `SkUserConfig.h` defines
//!   `SK_FONT_CONFIG_INTERFACE_ONLY_ALLOW_SFNT_FONTS`) whose file can be read (`MatchFont`);
//! * kept only when one of that font's family names is the post-config family or the requested one, or is
//!   METRIC-COMPATIBLE with the request (`IsMetricCompatibleReplacement` over `kFontEquivMap`) -- unless the
//!   request is empty, `sans`, `serif` or `monospace` (`IsFallbackFontAllowed`), which take any match.
//!
//! So fontconfig's alias for a family is not enough: Chrome rejects `Bitstream Vera Sans Mono` (fontconfig answers
//! DejaVu Sans Mono) and `Century` (C059), and accepts `PMingLiU`, `MS Gothic` and `SimSun` (Noto Serif CJK TC,
//! Noto Sans Mono CJK JP, Noto Serif CJK SC: the same equivalence class). Measured on stock Chrome 150 (a sensor cookie
//! font probe, a bot-detection service `font_detect.js`); korvus asked Skia's fontconfig manager for a strongly bound family and
//! answered the reverse on all five.
//!
//! When that fails Blink tries the family's alternate name (`AlternateFamilyName`, platform/fonts/
//! alternate_font_family.h): Courier <-> Courier New, Times <-> Times New Roman, Arial <-> Helvetica -- which is how
//! `Helvetica` is Liberation Sans here although fontconfig's own answer for it (Nimbus Sans) is rejected.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::Mutex;

#[cfg(target_os = "linux")]
#[repr(C)]
struct FcPattern {
    _p: [u8; 0],
}
#[cfg(target_os = "linux")]
#[repr(C)]
struct FcFontSet {
    nfont: c_int,
    sfont: c_int,
    fonts: *mut *mut FcPattern,
}

#[cfg(target_os = "linux")]
#[link(name = "fontconfig")]
extern "C" {
    fn FcPatternCreate() -> *mut FcPattern;
    fn FcPatternDestroy(p: *mut FcPattern);
    fn FcPatternAddString(p: *mut FcPattern, object: *const c_char, s: *const u8) -> c_int;
    fn FcPatternAddInteger(p: *mut FcPattern, object: *const c_char, i: c_int) -> c_int;
    fn FcPatternAddBool(p: *mut FcPattern, object: *const c_char, b: c_int) -> c_int;
    fn FcPatternGetString(p: *const FcPattern, object: *const c_char, n: c_int, s: *mut *mut u8) -> c_int;
    fn FcPatternGetInteger(p: *const FcPattern, object: *const c_char, n: c_int, i: *mut c_int) -> c_int;
    fn FcConfigSubstitute(config: *mut c_void, p: *mut FcPattern, kind: c_int) -> c_int;
    fn FcDefaultSubstitute(p: *mut FcPattern);
    fn FcFontSort(config: *mut c_void, p: *mut FcPattern, trim: c_int, csp: *mut *mut c_void, result: *mut c_int) -> *mut FcFontSet;
    fn FcFontSetDestroy(s: *mut FcFontSet);
    fn FcConfigGetSysRoot(config: *const c_void) -> *const u8;
}

#[cfg(target_os = "linux")]
const FC_RESULT_MATCH: c_int = 0;
#[cfg(target_os = "linux")]
const FC_MATCH_PATTERN: c_int = 0;
/// `fcpattern_from_skfontstyle` for the two weights and slants a canvas or a layout font asks for here: Skia's 400
/// is `FC_WEIGHT_REGULAR`, 700 `FC_WEIGHT_BOLD`; its normal width `FC_WIDTH_NORMAL`.
#[cfg(target_os = "linux")]
const FC_WEIGHT_REGULAR: c_int = 80;
#[cfg(target_os = "linux")]
const FC_WEIGHT_BOLD: c_int = 200;
#[cfg(target_os = "linux")]
const FC_WIDTH_NORMAL: c_int = 100;
#[cfg(target_os = "linux")]
const FC_SLANT_ROMAN: c_int = 0;
#[cfg(target_os = "linux")]
const FC_SLANT_ITALIC: c_int = 100;

/// The font file a family matched: its path (with fontconfig's sysroot) and the face's index in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FciMatch {
    pub path: String,
    pub index: u32,
}

/// Skia's `kFontEquivMap`, in its order: a name's class is its FIRST entry's (`GetFontEquivClass` returns at the first
/// match, so `Noto Serif CJK TC`, listed under five classes, is `PMINGLIU` -- which is why `MingLiU` is rejected
/// where `PMingLiU` is accepted).
const EQUIV: &[(u8, &str)] = &[
    (1, "Arial"), (1, "Arimo"), (1, "Liberation Sans"),
    (2, "Times New Roman"), (2, "Tinos"), (2, "Liberation Serif"),
    (3, "Courier New"), (3, "Cousine"), (3, "Liberation Mono"),
    (4, "Symbol"), (4, "Symbol Neu"),
    // ＭＳ Ｐゴシック
    (5, "MS PGothic"), (5, "\u{ff2d}\u{ff33} \u{ff30}\u{30b4}\u{30b7}\u{30c3}\u{30af}"), (5, "Noto Sans CJK JP"),
    (5, "IPAPGothic"), (5, "MotoyaG04Gothic"),
    // ＭＳ ゴシック
    (6, "MS Gothic"), (6, "\u{ff2d}\u{ff33} \u{30b4}\u{30b7}\u{30c3}\u{30af}"), (6, "Noto Sans Mono CJK JP"),
    (6, "IPAGothic"), (6, "MotoyaG04GothicMono"),
    // ＭＳ Ｐ明朝
    (7, "MS PMincho"), (7, "\u{ff2d}\u{ff33} \u{ff30}\u{660e}\u{671d}"), (7, "Noto Serif CJK JP"),
    (7, "IPAPMincho"), (7, "MotoyaG04Mincho"),
    // ＭＳ 明朝
    (8, "MS Mincho"), (8, "\u{ff2d}\u{ff33} \u{660e}\u{671d}"), (8, "Noto Serif CJK JP"),
    (8, "IPAMincho"), (8, "MotoyaG04MinchoMono"),
    // 宋体
    (9, "Simsun"), (9, "\u{5b8b}\u{4f53}"), (9, "Noto Serif CJK SC"), (9, "MSung GB18030"), (9, "Song ASC"),
    // 新宋体
    (10, "NSimsun"), (10, "\u{65b0}\u{5b8b}\u{4f53}"), (10, "Noto Serif CJK SC"), (10, "MSung GB18030"), (10, "N Song ASC"),
    // 黑体
    (11, "Simhei"), (11, "\u{9ed1}\u{4f53}"), (11, "Noto Sans CJK SC"), (11, "MYingHeiGB18030"), (11, "MYingHeiB5HK"),
    // 新細明體
    (12, "PMingLiU"), (12, "\u{65b0}\u{7d30}\u{660e}\u{9ad4}"), (12, "Noto Serif CJK TC"), (12, "MSung B5HK"),
    // 細明體
    (13, "MingLiU"), (13, "\u{7d30}\u{660e}\u{9ad4}"), (13, "Noto Serif CJK TC"), (13, "MSung B5HK"),
    // 新細明體_HKSCS
    (14, "PMingLiU_HKSCS"), (14, "\u{65b0}\u{7d30}\u{660e}\u{9ad4}_HKSCS"), (14, "Noto Serif CJK TC"), (14, "MSung B5HK"),
    // 細明體_HKSCS
    (15, "MingLiU_HKSCS"), (15, "\u{7d30}\u{660e}\u{9ad4}_HKSCS"), (15, "Noto Serif CJK TC"), (15, "MSung B5HK"),
    (16, "Cambria"), (16, "Caladea"),
    (17, "Calibri"), (17, "Carlito"),
];

/// `strcasecmp`: bytes equal under ASCII case folding (a UTF-8 name compares byte for byte otherwise).
fn strcasecmp_eq(a: &str, b: &str) -> bool {
    a.as_bytes().eq_ignore_ascii_case(b.as_bytes())
}

/// `GetFontEquivClass`: the class of the first `kFontEquivMap` entry naming `name`, `None` for `OTHER`.
fn equiv_class(name: &str) -> Option<u8> {
    EQUIV.iter().find(|(_, n)| strcasecmp_eq(n, name)).map(|(c, _)| *c)
}

/// `IsMetricCompatibleReplacement`.
fn metric_compatible(a: &str, b: &str) -> bool {
    matches!((equiv_class(a), equiv_class(b)), (Some(x), Some(y)) if x == y)
}

/// `IsFallbackFontAllowed`: an empty request, or `sans` / `serif` / `monospace`, takes whatever fontconfig answers.
fn fallback_allowed(family: &str) -> bool {
    family.is_empty() || ["sans", "serif", "monospace"].iter().any(|g| strcasecmp_eq(g, family))
}

/// Blink's `AlternateFamilyName`, tried when a family matched nothing.
pub(crate) fn alternate_family_name(family: &str) -> Option<&'static str> {
    const PAIRS: &[(&str, &str)] = &[
        ("Courier", "Courier New"),
        ("Courier New", "Courier"),
        ("Times", "Times New Roman"),
        ("Times New Roman", "Times"),
        ("Arial", "Helvetica"),
        ("Helvetica", "Arial"),
    ];
    PAIRS.iter().find(|(a, _)| a.eq_ignore_ascii_case(family)).map(|(_, b)| *b)
}

/// Every family asked so far and its answer, by `(family, bold, italic)`: fontconfig sorts every font on each ask.
static CACHE: Mutex<Option<HashMap<(String, bool, bool), Option<FciMatch>>>> = Mutex::new(None);

/// `SkFontConfigInterfaceDirect::matchFamilyName(family, style)`: the font file Chrome's font service hands Blink
/// for `family` in a weight of 400 or 700 and an upright or italic slant, or `None` when it answers none.
pub(crate) fn match_family_name(family: &str, bold: bool, italic: bool) -> Option<FciMatch> {
    // `kMaxFontFamilyLength`
    if family.len() > 2048 {
        return None;
    }
    let key = (family.to_string(), bold, italic);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(hit) = map.get(&key) {
        return hit.clone();
    }
    let found = query(family, bold, italic);
    map.insert(key, found.clone());
    found
}

/// The string `object` of `p` at `n`, copied.
///
/// # Safety
/// `p` is a live fontconfig pattern.
#[cfg(target_os = "linux")]
unsafe fn get_string(p: *const FcPattern, object: &CStr, n: c_int) -> Option<String> {
    let mut s: *mut u8 = std::ptr::null_mut();
    if FcPatternGetString(p, object.as_ptr(), n, &mut s) != FC_RESULT_MATCH || s.is_null() {
        return None;
    }
    Some(CStr::from_ptr(s as *const c_char).to_string_lossy().into_owned())
}

/// `FcConfigGetSysRoot` prefixed to a pattern's `file`.
///
/// # Safety
/// `p` is a live fontconfig pattern.
#[cfg(target_os = "linux")]
unsafe fn file_of(p: *const FcPattern) -> Option<String> {
    let file = get_string(p, c"file", 0)?;
    let root = FcConfigGetSysRoot(std::ptr::null());
    let root = if root.is_null() { String::new() } else { CStr::from_ptr(root as *const c_char).to_string_lossy().into_owned() };
    Some(root + &file)
}

/// `isValidPattern`: an sfnt font whose file can be read.
///
/// # Safety
/// `p` is a live fontconfig pattern.
#[cfg(target_os = "linux")]
unsafe fn valid(p: *const FcPattern) -> bool {
    if !matches!(get_string(p, c"fontformat", 0).as_deref(), Some("TrueType") | Some("CFF")) {
        return false;
    }
    match file_of(p) {
        Some(path) => std::fs::File::open(&path).is_ok(),
        None => false,
    }
}

#[cfg(target_os = "linux")]
fn query(family: &str, bold: bool, italic: bool) -> Option<FciMatch> {
    let name = CString::new(family).ok()?;
    // SAFETY: fontconfig's documented C API; the pattern and the font set made here are destroyed before returning,
    // and every string read out of them is copied first.
    unsafe {
        let pattern = FcPatternCreate();
        if pattern.is_null() {
            return None;
        }
        // `familyName` is never null here: Blink names a family (an empty one is the default font's request).
        FcPatternAddString(pattern, c"family".as_ptr(), name.as_ptr() as *const u8);
        FcPatternAddInteger(pattern, c"weight".as_ptr(), if bold { FC_WEIGHT_BOLD } else { FC_WEIGHT_REGULAR });
        FcPatternAddInteger(pattern, c"width".as_ptr(), FC_WIDTH_NORMAL);
        FcPatternAddInteger(pattern, c"slant".as_ptr(), if italic { FC_SLANT_ITALIC } else { FC_SLANT_ROMAN });
        FcPatternAddBool(pattern, c"scalable".as_ptr(), 1);
        FcPatternAddString(pattern, c"fontwrapper".as_ptr(), c"SFNT".as_ptr() as *const u8);
        FcConfigSubstitute(std::ptr::null_mut(), pattern, FC_MATCH_PATTERN);
        FcDefaultSubstitute(pattern);
        let post_config_family = get_string(pattern, c"family", 0).unwrap_or_default();
        let mut result: c_int = 0;
        let set = FcFontSort(std::ptr::null_mut(), pattern, 0, std::ptr::null_mut(), &mut result);
        FcPatternDestroy(pattern);
        if set.is_null() {
            return None;
        }
        let out = (|| {
            // `MatchFont`: the first valid font of the sorted set.
            let fonts = (*set).fonts;
            let n = (*set).nfont.max(0) as usize;
            if fonts.is_null() {
                return None;
            }
            let matched = (0..n).map(|i| *fonts.add(i)).find(|&p| valid(p))?;
            if !fallback_allowed(family) {
                let acceptable = (0..255).map_while(|id| get_string(matched, c"family", id)).any(|post_match| {
                    strcasecmp_eq(&post_config_family, &post_match) || strcasecmp_eq(family, &post_match) || metric_compatible(family, &post_match)
                });
                if !acceptable {
                    return None;
                }
            }
            let path = file_of(matched)?;
            let mut index: c_int = 0;
            if FcPatternGetInteger(matched, c"index".as_ptr(), 0, &mut index) != FC_RESULT_MATCH {
                index = 0;
            }
            Some(FciMatch { path, index: index.max(0) as u32 })
        })();
        FcFontSetDestroy(set);
        out
    }
}

/// macOS/Windows: family->file resolution is CoreText / DirectWrite, not fontconfig. Until that port
/// lands no family strongly binds, so the caller falls back -- honest for a build that cannot yet ask
/// the platform's matcher. THE font layer (GPU-port phase) replaces this with the real one.
#[cfg(not(target_os = "linux"))]
fn query(_family: &str, _bold: bool, _italic: bool) -> Option<FciMatch> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_equivalence_classes_are_skias_first_entry_wins() {
        assert!(metric_compatible("PMingLiU", "Noto Serif CJK TC"));
        // `Noto Serif CJK TC` is PMINGLIU (its first entry), so MingLiU does not take it
        assert!(!metric_compatible("MingLiU", "Noto Serif CJK TC"));
        assert!(metric_compatible("ms gothic", "Noto Sans Mono CJK JP"));
        assert!(metric_compatible("SimSun", "noto serif cjk sc"));
        assert!(metric_compatible("Arial", "Liberation Sans"));
        assert!(!metric_compatible("Bitstream Vera Sans Mono", "DejaVu Sans Mono"));
        assert!(!metric_compatible("Century", "C059"));
        assert!(fallback_allowed("Monospace") && fallback_allowed("") && !fallback_allowed("sans-serif"));
        assert_eq!(alternate_family_name("helvetica"), Some("Arial"));
        assert_eq!(alternate_family_name("Verdana"), None);
    }

    #[test]
    fn fontconfig_aliases_are_not_enough_and_metric_equivalents_are() {
        // the fonts of the persona machine (stock Chrome 150's answers, a sensor cookie probe, 2026-10-11)
        if match_family_name("DejaVu Sans Mono", false, false).is_none() || match_family_name("Noto Serif CJK TC", false, false).is_none() {
            return; // a machine without the persona's fonts
        }
        assert!(match_family_name("Bitstream Vera Sans Mono", false, false).is_none());
        assert!(match_family_name("PMingLiU", false, false).is_some());
        assert!(match_family_name("MingLiU", false, false).is_none());
        assert!(match_family_name("Helvetica", false, false).is_none(), "fontconfig's Nimbus Sans is no equivalent");
        assert!(match_family_name("Arial", false, false).is_some());
    }
}
