//! Text as Chrome 150 shapes and measures it on Linux, for canvas `measureText`.
//!
//! a managed bot challenge measures ten emoji at `16px sans-serif` in its second stage and reads
//! every `TextMetrics` member. The ink box (`actualBoundingBox*`) and the font's baselines are facts
//! about the FONT FILE -- glyph outlines through FreeType's hinting, the `OS/2` and `BASE` tables -- so
//! they are computed here, beside the rasterizer that draws the same glyphs, by the rules Chrome follows
//! (each measured against google-chrome 150.0.7871.46 on this machine, 2026-10-08):
//!
//! * **Which font.** A family list is walked in order; a generic keyword is fontconfig's choice for
//!   the UI language (Chrome's `sans-serif` here is Noto Sans CJK); a named family is the font Chrome's
//!   font service matches for it -- Skia's `matchFamilyName`, which takes fontconfig's answer only when
//!   it is that family or a metric-compatible one ([`crate::font_match`]), then Blink's alternate name. A
//!   character with emoji presentation (an emoji codepoint, or any followed by U+FE0F) takes the emoji
//!   font fontconfig names for `und-Zsye`; one no listed font has takes fontconfig's choice for the
//!   character in the UI language -- with NO family, as `gfx::GetFallbackFontForChar` asks (so a Hangul
//!   syllable in Times New Roman is drawn in the sans CJK face, not the serif one).
//! * **Which glyphs.** HarfBuzz shapes each run in the UI language, in the script Blink's
//!   `HarfBuzzShaper::Shape` gives it: 8-bit (Latin-1) text is Latin throughout, so a Korean UI gets
//!   the font's Korean `locl` forms even for punctuation -- Noto Sans CJK's `(`, `:`, `-`, `!` and the
//!   space are other glyphs under `ko`; 16-bit text goes by its script runs, and a word of only Common
//!   characters stays Common, which selects no `locl` (the space of `😀 漢字 ok` is
//!   0.224 em in Noto Sans CJK KR, not the 0.28 em of `a b`).
//! * **Where.** A glyph's advance is FreeType's 16.16 `linearHoriAdvance` at the size floored to
//!   1/64 px (`FT_MulDiv(units, x_scale, 64)`, `x_scale = FT_DivFix(size26.6, upem)`); a GPOS
//!   adjustment is HarfBuzz's `em_mult` at the scale Blink sets (`size * 65536`).
//! * **The ink box.** Each glyph's `SkFont::getBounds` with slight hinting, rounded OUT to whole
//!   pixels at the glyph's own origin (`SkFontGetBoundsForGlyphs`), then offset by the pen position:
//!   which is why Chrome's `actualBoundingBoxAscent` is an integer and its `actualBoundingBoxRight`
//!   is not.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use skia_safe::{font::Edging, Font, FontHinting, FontMgr, FontStyle, Rect, Typeface};
use spec::{TextFontRequest, TextInk};

/// The CSS generic family keywords fontconfig resolves.
const GENERICS: &[&str] = &[
    "serif", "sans-serif", "monospace", "cursive", "fantasy", "system-ui", "ui-serif", "ui-sans-serif",
    "ui-monospace", "ui-rounded", "math", "emoji", "fangsong",
];

thread_local! {
    /// A typeface's bytes and collection index, by Skia's unique id: HarfBuzz parses the file.
    static FONT_DATA: RefCell<HashMap<u32, Option<Rc<(Vec<u8>, u32)>>>> = RefCell::new(HashMap::new());
    /// A typeface's HarfBuzz face, by Skia's unique id (see [`shaping_face`]).
    static HB_FACES: RefCell<HashMap<u32, Option<Rc<HbFace>>>> = RefCell::new(HashMap::new());
    /// `(family, bold, italic, lang)` -> its typeface, or none when the family does not exist here.
    static FAMILIES: RefCell<HashMap<(String, bool, bool, String), Option<Typeface>>> = RefCell::new(HashMap::new());
    /// `(char, emoji, bold, italic, lang)` -> the system fallback for it.
    static FALLBACK: RefCell<HashMap<(char, bool, bool, bool, String), Option<Typeface>>> = RefCell::new(HashMap::new());
    /// A fontconfig typeface's face made from its file alone, by the former's unique id (see [`plain_typeface`]).
    static PLAIN: RefCell<HashMap<u32, Option<Typeface>>> = RefCell::new(HashMap::new());
    /// The face of a font file the font service matched, by `(path, index)`: one typeface per face, whichever
    /// family names matched it (see [`file_typeface`]).
    static FILE_FACES: RefCell<HashMap<(String, u32), Option<Typeface>>> = RefCell::new(HashMap::new());
}

fn style(bold: bool, italic: bool) -> FontStyle {
    match (bold, italic) {
        (false, false) => FontStyle::normal(),
        (true, false) => FontStyle::bold(),
        (false, true) => FontStyle::italic(),
        (true, true) => FontStyle::bold_italic(),
    }
}

/// Is `family` (unquoted) one of the generic keywords?
fn is_generic(family: &str) -> bool {
    GENERICS.contains(&family.to_ascii_lowercase().as_str())
}

/// The names of a CSS `font-family` list, unquoted.
#[cfg(test)]
pub(crate) fn family_list(list: &str) -> Vec<String> {
    family_list_quoted(list).into_iter().map(|(name, _)| name).collect()
}

/// The names of a CSS `font-family` list, unquoted, each with whether it was QUOTED: a quoted `"serif"` is a family
/// NAME, never the generic keyword (CSS Fonts 4), and Blink asks the font service for it by that name.
fn family_list_quoted(list: &str) -> Vec<(String, bool)> {
    list.split(',')
        .map(|f| {
            let t = f.trim();
            let quoted = t.starts_with(['"', '\'']);
            (t.trim_matches(|c| c == '"' || c == '\'').trim().to_string(), quoted)
        })
        .filter(|(f, _)| !f.is_empty())
        .collect()
}

/// The typeface `family` names here, or `None` when no installed font answers to it. A generic is
/// fontconfig's choice for the language; a named family is the file Chrome's font service matches for it
/// ([`crate::font_match::match_family_name`]: fontconfig's answer, kept only when it is that family or
/// metric-compatible with it -- `Arial` -> Liberation Sans and `PMingLiU` -> Noto Serif CJK TC count,
/// `Bitstream Vera Sans Mono` -> DejaVu Sans Mono does not), else the one its alternate name matches
/// (`FontPlatformDataCache::GetOrCreateFontPlatformData` -> `AlternateFamilyName`: `Helvetica` is Arial's).
fn family_typeface(mgr: &FontMgr, family: &str, generic: bool, bold: bool, italic: bool, lang: &str) -> Option<Typeface> {
    // a quoted generic name is a family name: its own cache entry
    let lower = family.to_ascii_lowercase();
    let name = if !generic && GENERICS.contains(&lower.as_str()) { format!("\"{lower}") } else { lower };
    let key = (name, bold, italic, lang.to_string());
    if let Some(hit) = FAMILIES.with(|t| t.borrow().get(&key).cloned()) {
        return hit;
    }
    let st = style(bold, italic);
    let tf = if generic {
        mgr.match_family_style_character(family, st, &[lang], 'A' as i32)
            .or_else(|| mgr.legacy_make_typeface(Some(family), st))
    } else {
        crate::font_match::match_family_name(family, bold, italic)
            .or_else(|| crate::font_match::alternate_family_name(family).and_then(|alt| crate::font_match::match_family_name(alt, bold, italic)))
            .and_then(|m| file_typeface(mgr, &m))
            .or_else(|| host_family(mgr, family, st))
    };
    FAMILIES.with(|t| t.borrow_mut().insert(key, tf.clone()));
    tf
}

/// The named family through the HOST font manager, the tail of the named-family path off Linux.
///
/// On Linux the font SERVICE ([`crate::font_match::match_family_name`], fontconfig) is authoritative and
/// this adds nothing -- it returns `None`, so an unmatched family stays unmatched (Chrome does not fall
/// back to a different family for a named request). On macOS and Windows there is no separate font
/// service: Blink matches through Skia's system `SkFontMgr` directly -- CoreText on macOS, DirectWrite on
/// Windows -- which is exactly `mgr` here, so a named family resolves to the same face Chrome renders.
#[cfg(target_os = "linux")]
fn host_family(_mgr: &FontMgr, _family: &str, _st: FontStyle) -> Option<Typeface> {
    None
}
#[cfg(not(target_os = "linux"))]
fn host_family(mgr: &FontMgr, family: &str, st: FontStyle) -> Option<Typeface> {
    mgr.match_family_style(family, st)
}

/// The typeface of a font file the font service matched: made from the file and the face's index, as a Blink
/// renderer makes it (`SkTypeface_FCI` over the identity the font service returned -- no fontconfig pattern rides
/// on it, see [`plain_typeface`]). Its bytes seed [`font_data`]'s table, so HarfBuzz reads the same copy.
fn file_typeface(mgr: &FontMgr, m: &crate::font_match::FciMatch) -> Option<Typeface> {
    let key = (m.path.clone(), m.index);
    if let Some(hit) = FILE_FACES.with(|t| t.borrow().get(&key).cloned()) {
        return hit;
    }
    let bytes = std::fs::read(&m.path).ok();
    let tf = bytes.as_ref().and_then(|b| mgr.new_from_data(b, m.index as usize));
    if let (Some(tf), Some(bytes)) = (&tf, bytes) {
        FONT_DATA.with(|t| t.borrow_mut().insert(tf.unique_id(), Some(Rc::new((bytes, m.index)))));
    }
    FILE_FACES.with(|t| t.borrow_mut().insert(key, tf.clone()));
    tf
}

/// The family name Blink makes a listed family's platform font under: a named family as the page wrote it, a
/// generic as Chrome's settings name it ([`chrome_generic_family`]; the face's own name for a generic they do
/// not cover). It names the strike to the font service, so it decides the hinting: here `sans-serif` is Noto
/// Sans CJK KR (slight), where fontconfig's own choice for the generic is the JP face (unhinted up to 18px).
fn blink_family(mgr: &FontMgr, family: &str, generic: bool, tf: &Typeface, lang: &str) -> String {
    let lower = family.to_ascii_lowercase();
    if generic {
        chrome_generic_family(mgr, &lower, lang).unwrap_or_else(|| tf.family_name())
    } else {
        family.to_string()
    }
}

/// Chrome 150 on Linux's family for a generic in the script of `lang` (`chrome/app/resources/
/// locale_settings_linux.grd`, read through Blink's `GenericFontFamilySettings`): a list's first family installed
/// here, else its first. The Japanese, Korean and Han scripts and Devanagari have their own lists; every other
/// script takes the global defaults.
fn chrome_generic_family(mgr: &FontMgr, generic: &str, lang: &str) -> Option<String> {
    let l = lang.to_ascii_lowercase();
    let script = if l.starts_with("ja") {
        "Jpan"
    } else if l.starts_with("ko") {
        "Hang"
    } else if l.starts_with("zh") {
        if l.contains("hant") || l.ends_with("-tw") || l.ends_with("-hk") || l.ends_with("-mo") { "Hant" } else { "Hans" }
    } else if ["hi", "mr", "ne", "sa", "kok", "mai", "bho"].iter().any(|p| l == *p || l.starts_with(&format!("{p}-"))) {
        "Deva"
    } else {
        ""
    };
    let list: &str = match (script, generic) {
        ("Jpan", "serif") => ",Noto Serif JP,Noto Serif CJK JP,Times New Roman",
        ("Jpan", "sans-serif") => ",Noto Sans JP,Noto Sans CJK JP,Arial",
        ("Jpan", "monospace") => "Noto Sans Mono CJK JP",
        ("Hang", "serif") => ",Noto Serif KR,Noto Serif CJK KR,Times New Roman",
        ("Hang", "sans-serif") => ",Noto Sans KR,Noto Sans CJK KR,Arial",
        ("Hans", "serif") => ",Noto Serif SC,Noto Serif CJK SC,Times New Roman",
        ("Hans", "sans-serif") => ",Noto Sans SC,Noto Sans CJK SC,Arial",
        ("Hant", "serif") => ",Noto Serif TC,Noto Serif CJK TC,Times New Roman",
        ("Hant", "sans-serif") => ",Noto Sans TC,Noto Sans CJK TC,Arial",
        ("Deva", "serif") => "Noto Serif Devanagari",
        ("Deva", "sans-serif") => "Noto Sans Devanagari",
        ("Deva", "monospace") => "Noto Sans Mono",
        (_, "serif") => "Times New Roman",
        (_, "sans-serif") => "Arial",
        (_, "monospace") => "Monospace",
        (_, "cursive") => "Comic Sans MS",
        (_, "fantasy") => "Impact",
        (_, "math") => "Latin Modern Math",
        _ => return None,
    };
    // `FirstAvailableOrFirst`: a leading comma marks a list
    let Some(names) = list.strip_prefix(',') else { return Some(list.to_string()) };
    let names: Vec<&str> = names.split(',').collect();
    names.iter().find(|n| installed_family(mgr, n)).or(names.first()).map(|n| n.to_string())
}

/// Is a font of exactly this family installed (no alias stands in, as Blink's `FontCache` takes a family only when
/// the font service matched that name)?
fn installed_family(mgr: &FontMgr, name: &str) -> bool {
    thread_local! {
        static NAMES: std::cell::OnceCell<std::collections::HashSet<String>> = const { std::cell::OnceCell::new() };
    }
    NAMES.with(|n| n.get_or_init(|| mgr.family_names().map(|f| f.to_ascii_lowercase()).collect()).contains(&name.to_ascii_lowercase()))
}

fn fallback_typeface(mgr: &FontMgr, ch: char, emoji: bool, bold: bool, italic: bool, lang: &str) -> Option<Typeface> {
    let key = (ch, emoji, bold, italic, lang.to_string());
    if let Some(hit) = FALLBACK.with(|t| t.borrow().get(&key).cloned()) {
        return hit;
    }
    let tag = if emoji { "und-Zsye" } else { lang };
    let tf = mgr.match_family_style_character("", style(bold, italic), &[tag], ch as i32);
    FALLBACK.with(|t| t.borrow_mut().insert(key, tf.clone()));
    tf
}

/// `tf` without fontconfig's render-time edits. A Blink renderer gets a face from the font service as a file and an
/// index (`SkTypeface_FCI`), so no pattern edit rides on its glyphs -- `90-synthetic.conf` slants an upright face
/// matched for an italic request by 0.2 and marks it emboldened for a bold one, which Skia's fontconfig manager
/// applies (`onFilterRec`) -- and the only synthetic style is Blink's own (below). Measured on Chrome 150: `italic
/// 150px serif`'s `i` has ink right 62, the upright face skewed by -1/4; the fontconfig face read 56, and 84 skewed.
fn plain_typeface(mgr: &FontMgr, tf: &Typeface) -> Typeface {
    let id = tf.unique_id();
    if let Some(hit) = PLAIN.with(|t| t.borrow().get(&id).cloned()) {
        return hit.unwrap_or_else(|| tf.clone());
    }
    let plain = font_data(tf).and_then(|d| mgr.new_from_data(&d.0, d.1 as usize));
    // at the same place on its axes, when it was made at one (a variable web font's weight)
    let position = axis_position(tf);
    let plain = match plain {
        Some(p) if !position.is_empty() => {
            let args = skia_safe::FontArguments::new().set_variation_design_position(skia_safe::font_arguments::VariationPosition { coordinates: &position });
            p.clone_with_arguments(&args).or(Some(p))
        }
        p => p,
    };
    PLAIN.with(|t| t.borrow_mut().insert(id, plain.clone()));
    plain.unwrap_or_else(|| tf.clone())
}

/// Where on its font's axes a typeface was made (a variable web font's instance), empty for none.
fn axis_position(tf: &Typeface) -> Vec<skia_safe::font_arguments::variation_position::Coordinate> {
    tf.variation_design_position().unwrap_or_default()
}

/// A typeface's HarfBuzz face, made once per thread. `rustybuzz::Face::from_slice` builds every GSUB/GPOS lookup's
/// accelerator (its set digests), and making one per run was 69 % of a `measureText`: 3 000 calls took 860 ms where
/// they took 11 before canvas text was shaped (fixture `P-canvas-ops`, Chrome 2 ms for 1 000). The face borrows the
/// bytes [`FONT_DATA`] keeps for the thread's life, and `_data` holds them while the face exists -- fields drop in
/// order, the face first. `plans` keeps each run kind's shape plan -- `rustybuzz::shape` compiles one per call
/// (`hb_ot_map_builder_t::compile`, the language's features looked up), which was most of what remained.
struct HbFace {
    /// [`file_style`] of the file, read once
    style: (bool, bool, bool),
    plans: RefCell<HashMap<(rustybuzz::Direction, rustybuzz::Script, Option<rustybuzz::Language>), Rc<rustybuzz::ShapePlan>>>,
    face: rustybuzz::Face<'static>,
    _data: Rc<(Vec<u8>, u32)>,
}

impl HbFace {
    /// `rustybuzz::shape` with no features, its plan made once for the buffer's direction, script and language (the
    /// properties `shape` itself compiles a plan for, after the same `guess_segment_properties`).
    fn shape(&self, mut buf: rustybuzz::UnicodeBuffer) -> rustybuzz::GlyphBuffer {
        buf.guess_segment_properties();
        let key = (buf.direction(), buf.script(), buf.language());
        let plan = self.plans.borrow().get(&key).cloned();
        let plan = plan.unwrap_or_else(|| {
            let made = Rc::new(rustybuzz::ShapePlan::new(&self.face, key.0, Some(key.1), key.2.as_ref(), &[]));
            self.plans.borrow_mut().insert(key, made.clone());
            made
        });
        rustybuzz::shape_with_plan(&self.face, &plan, buf)
    }
}

fn shaping_face(tf: &Typeface, data: &Rc<(Vec<u8>, u32)>) -> Option<Rc<HbFace>> {
    let id = tf.unique_id();
    if let Some(hit) = HB_FACES.with(|t| t.borrow().get(&id).cloned()) {
        return hit;
    }
    // SAFETY: the slice is the heap buffer of a `Vec` inside an `Rc` that the `HbFace` keeps alive and that nothing
    // mutates (an `Rc`'s contents are shared, never borrowed mutably), so it outlives the face that borrows it.
    let bytes: &'static [u8] = unsafe { std::slice::from_raw_parts(data.0.as_ptr(), data.0.len()) };
    let made = hb_face(bytes, data.1).map(|mut face| {
        let mut style = file_style(&data.0, data.1);
        // a typeface made at a position on its font's axes, not a named instance (a variable web font drawn at a
        // weight, `web_fonts`): shaped there, and as bold as its `wght` -- a 700 is no face to embolden
        if data.1 >> 16 == 0 {
            for c in axis_position(tf) {
                let tag = [c.axis.a(), c.axis.b(), c.axis.c(), c.axis.d()];
                face.set_variation(rustybuzz::ttf_parser::Tag::from_bytes(&tag), c.value);
                if &tag == b"wght" {
                    style.0 = c.value >= 600.0;
                }
            }
        }
        Rc::new(HbFace { style, plans: RefCell::new(HashMap::new()), face, _data: data.clone() })
    });
    HB_FACES.with(|t| t.borrow_mut().insert(id, made.clone()));
    made
}

pub(crate) fn font_data(tf: &Typeface) -> Option<Rc<(Vec<u8>, u32)>> {
    let id = tf.unique_id();
    if let Some(hit) = FONT_DATA.with(|t| t.borrow().get(&id).cloned()) {
        return hit;
    }
    let data = tf.to_font_data().map(|(bytes, index)| Rc::new((bytes, index as u32)));
    FONT_DATA.with(|t| t.borrow_mut().insert(id, data.clone()));
    data
}

/// Whether `tf`'s cmap maps the variation sequence `ch` + `vs` (format 14), as HarfBuzz's `get_variation_glyph`.
fn maps_variation(tf: &Typeface, ch: char, vs: char) -> bool {
    font_data(tf).and_then(|d| shaping_face(tf, &d)).and_then(|hb| hb.face.glyph_variation_index(ch, vs)).is_some()
}

/// A character drawn with emoji presentation: an emoji codepoint whose default is emoji, or any
/// character the text follows with VARIATION SELECTOR-16.
fn emoji_presentation(ch: char, next: Option<char>) -> bool {
    let c = ch as u32;
    next == Some('\u{FE0F}')
        || matches!(c, 0x1F300..=0x1F5FF | 0x1F600..=0x1F64F | 0x1F680..=0x1F6FF | 0x1F900..=0x1F9FF
            | 0x1FA70..=0x1FAFF | 0x1F1E6..=0x1F1FF | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A)
}

/// Characters that join the glyph before them (variation selectors, ZWJ, skin-tone modifiers,
/// combining marks, tag characters): they stay in its run, whatever font they are in.
fn joins_previous(ch: char) -> bool {
    matches!(ch as u32, 0xFE00..=0xFE0F | 0x200D | 0x1F3FB..=0x1F3FF | 0x0300..=0x036F | 0x20D0..=0x20FF | 0xE0020..=0xE007F)
}

/// Blink's `NextWordEndIndex` (platform/fonts/plain_text_node.cc), as the set of indices where a new
/// word starts: a space or tab is a word of its own; Latin-1 text otherwise breaks only there; other
/// text also breaks around each CJK ideograph, CJK symbol or emoji, keeping a following modifier,
/// ZWJ, variation selector or emoji component with it -- and the emoji a ZWJ joins, and a flag's second regional
/// indicator: an emoji's grapheme cluster is one word, so `🇬🇧` is shaped as the flag and not as the letters G and B
/// (minea's testimonial flags read "G B" in blue).
fn word_ends(chars: &[char]) -> std::collections::HashSet<usize> {
    let delimiter = |c: char| c == ' ' || c == '\t' || c == '\u{200B}';
    let latin1 = chars.iter().all(|&c| (c as u32) < 0x100);
    let cjk = |c: char| {
        matches!(c as u32, 0x2E80..=0x2FFF | 0x3000..=0x30FF | 0x3190..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF | 0x1F000..=0x1FAFF | 0x2600..=0x27BF)
    };
    let mut starts = std::collections::HashSet::new();
    let mut i = 0;
    while i < chars.len() {
        starts.insert(i);
        let c = chars[i];
        let mut end = i + 1;
        if delimiter(c) {
            // a word of its own
        } else if latin1 || !cjk(c) {
            while end < chars.len() && !delimiter(chars[end]) && (latin1 || !cjk(chars[end])) {
                end += 1;
            }
        } else {
            let flag = |c: char| (0x1F1E6..=0x1F1FF).contains(&(c as u32));
            if flag(c) && end < chars.len() && flag(chars[end]) {
                end += 1;
            }
            while end < chars.len() && (joins_previous(chars[end]) || chars[end - 1] == '\u{200D}') {
                end += 1;
            }
        }
        i = end;
    }
    starts
}

/// Is `text` the 8-bit string Blink shapes it as? A `String` holds Latin-1 text in 8 bits, and canvas text keeps that
/// unless its space normalization rewrites a character -- `PlainTextNode::NormalizeSpacesAndMaybeBidi` copies the text
/// into a 16-bit buffer the moment it replaces U+0009..U+000D with U+0020.
fn eight_bit(text: &str, canvas: bool) -> bool {
    text.chars().all(|c| (c as u32) < 0x100 && !(canvas && matches!(c, '\t' | '\n' | '\x0B' | '\x0C' | '\r')))
}

/// A character's script as HarfBuzz's Unicode functions read it, `None` for Common, Inherited and Unknown: the
/// characters Blink's `ScriptRunIterator` folds into the run around them.
fn strong_script(ch: char) -> Option<rustybuzz::Script> {
    let mut b = rustybuzz::UnicodeBuffer::new();
    b.add(ch, 0);
    b.guess_segment_properties();
    let s = b.script();
    (s != rustybuzz::script::UNKNOWN).then_some(s)
}

/// The size Blink makes a platform font at: the requested size in whole hundredths of a pixel.
pub(crate) fn platform_size(size: f32) -> f32 {
    (size * 100.0) as u32 as f32 / 100.0
}

/// FreeType's `FT_MulDiv` for a non-negative product: `a * b / c`, rounded half up.
fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    let p = a * b;
    if p >= 0 {
        (p + c / 2) / c
    } else {
        -((-p + c / 2) / c)
    }
}

/// HarfBuzz's `em_mult`: a value in font units at a 16.16 multiplier.
fn em_mult(v: i64, mult: i64) -> i64 {
    (v * mult + 32768) >> 16
}

/// The scale Blink hands HarfBuzz (`SkiaScalarToHarfBuzzPosition(size)`: a float product, truncated)
/// as HarfBuzz's per-unit multiplier.
fn hb_mult(size: f32, upem: u16) -> i64 {
    let scale = (size * 65536.0) as i64;
    (scale << 16) / upem.max(1) as i64
}

/// One run of glyphs in one font, positioned from the text's origin on the alphabetic baseline.
pub(crate) struct GlyphRun {
    pub font: Font,
    pub glyphs: Vec<u16>,
    pub points: Vec<skia_safe::Point>,
    /// The family name Blink makes this run's platform font under (`None`: a font made for a character),
    /// which names its strike to the font service ([`crate::font_style`]).
    pub family: Option<String>,
}

/// Text shaped as Chrome shapes it -- see the module doc.
pub(crate) struct Shaped {
    pub runs: Vec<GlyphRun>,
    /// The advance, each glyph's 16.16 advance summed in `float`.
    pub width: f32,
    /// The union of the rounded-out glyph ink boxes, `[left, top, right, bottom]`.
    pub bounds: Option<[f32; 4]>,
    /// The caret before each UTF-16 code unit and after the last (see [`spec::TextInk::carets`]).
    pub carets: Vec<f32>,
    /// The first font of the list that exists (Blink's primary font), at the platform size.
    pub primary: Typeface,
    pub size: f32,
}

/// Shape `text` in the request's font and measure its ink box -- see the module doc.
pub(crate) fn text_ink(mgr: &FontMgr, req: &TextFontRequest, text: &str) -> Option<TextInk> {
    let shaped = shape(mgr, req, text)?;
    let t = primary_tables(&shaped)?;
    let metrics = Some(primary_ascent_descent(&shaped));
    Some(TextInk { bounds: shaped.bounds, width: shaped.width, units_per_em: t.0, typo: t.1, base: t.2, carets: shaped.carets, metrics })
}

/// The primary font's units per em, typo ascender/descender and BASE baselines in px.
fn primary_tables(shaped: &Shaped) -> Option<(u16, Option<(i16, i16)>, [Option<f32>; 3])> {
    let data = font_data(&shaped.primary)?;
    let face = rustybuzz::ttf_parser::RawFace::parse(&data.0, data.1 & 0xFFFF).ok()?;
    let table = |tag: &[u8; 4]| face.table(rustybuzz::ttf_parser::Tag::from_bytes(tag));
    let upem = table(b"head").and_then(|h| be_u16(h, 18)).unwrap_or(1000);
    let typo = table(b"OS/2").and_then(|t| Some((be_i16(t, 68)?, -be_i16(t, 70)?)));
    let mult = hb_mult(shaped.size, upem);
    let base = table(b"BASE")
        .map(base_baselines)
        .unwrap_or([None; 3])
        .map(|c| c.map(|v| em_mult(v as i64, mult) as f32 / 65536.0));
    Some((upem, typo, base))
}

/// Where `textBaseline` puts the alphabetic baseline below the drawing origin, as Blink's
/// `GetFontBaseline` (core/html/canvas/text_metrics.cc) computes it -- the same function `measureText`
/// answers from: the em box for `top` / `middle` / `bottom`, the BASE table (else 80% of the ascent,
/// the descent, 0) for `hanging` / `ideographic` / `alphabetic`. Ascent and descent are the primary
/// font's, rounded as `SimpleFontData` rounds them.
/// The primary font's ascent and descent, rounded as `SimpleFontData` rounds them.
fn primary_ascent_descent(shaped: &Shaped) -> (f32, f32) {
    let mut font = Font::new(shaped.primary.clone(), Some((shaped.size * 64.0).floor() / 64.0));
    font.set_hinting(FontHinting::Slight);
    font.set_subpixel(true);
    let (_, m) = font.metrics();
    ((-m.ascent).round(), m.descent.round())
}

pub(crate) fn baseline_offset(shaped: &Shaped, baseline: spec::TextBaseline) -> f32 {
    use spec::TextBaseline as B;
    let size = shaped.size;
    let (ascent, descent) = primary_ascent_descent(shaped);
    let (_, typo, base) = primary_tables(shaped).unwrap_or((1000, None, [None; 3]));
    let layout_unit = |v: f32| (v * 64.0).round() / 64.0;
    let normalized = |a: f32, d: f32| -> Option<(f32, f32)> {
        let h = a + d;
        if h <= 0.0 || a < 0.0 || a > h {
            return None;
        }
        let nta = layout_unit(a * size / h);
        Some((nta, layout_unit(size) - nta))
    };
    let (nta, ntd) = typo
        .filter(|(a, _)| *a > 0)
        .and_then(|(a, d)| normalized(a as f32, d as f32))
        .or_else(|| normalized(ascent, descent))
        .unwrap_or((0.0, 0.0));
    match baseline {
        B::Top => nta,
        B::Hanging => base[1].unwrap_or(ascent * 80.0 / 100.0),
        B::Ideographic => base[2].unwrap_or(-descent),
        B::Bottom => -ntd,
        B::Middle => (nta - ntd) / 2.0,
        B::Alphabetic => base[0].unwrap_or(0.0),
    }
}

/// Shape `text` in the request's font: font runs, glyphs, positions, ink box -- see the module doc.
pub(crate) fn shape(mgr: &FontMgr, req: &TextFontRequest, text: &str) -> Option<Shaped> {
    let lang = req.lang.as_str();
    let (bold, italic) = (req.bold, req.italic);
    // The fonts of the list that exist, in order; the default family when none does.
    // Each with the family name Blink makes its platform font under, which names the strike to the font service.
    // An unquoted generic keyword is the generic; a quoted name, even `"serif"`, is a family name.
    let mut fonts: Vec<(Typeface, String)> = family_list_quoted(&req.families)
        .iter()
        .filter_map(|(f, quoted)| {
            // a web font the page's painter registered (`web_fonts`): its own file, under no installed family
            if crate::web_fonts::is_web_family(f) {
                return crate::web_fonts::typeface(mgr, f).map(|tf| (f.clone(), tf));
            }
            let generic = !quoted && is_generic(f);
            family_typeface(mgr, f, generic, bold, italic, lang).map(|tf| (blink_family(mgr, f, generic, &tf, lang), tf))
        })
        .map(|(n, tf)| (tf, n))
        .collect();
    if fonts.is_empty() {
        let generic = is_generic(&req.default_family);
        fonts.extend(
            family_typeface(mgr, &req.default_family, generic, bold, italic, lang)
                .map(|tf| (tf.clone(), blink_family(mgr, &req.default_family, generic, &tf, lang))),
        );
    }
    let primary = fonts.first()?.clone();

    // Runs: consecutive characters in one typeface, within one WORD -- Blink's canvas shapes word by
    // word (`PlainTextNode::SegmentWord`), so no kerning pair or ligature spans a space: `"AVATAR To
    // Ya"` in Arial is 103.4375px in Chrome, not the 102.859375 the pairs around its spaces would give.
    // Canvas text first has its space characters normalized to U+0020
    // (`Character::IsNormalizedCanvasSpaceCharacter`: U+0009 through U+000D).
    let chars: Vec<char> = text.chars().map(|c| if matches!(c, '\t' | '\n' | '\x0B' | '\x0C' | '\r') { ' ' } else { c }).collect();
    // The script each run is shaped in, as `HarfBuzzShaper::Shape` picks it: 8-bit text is "guaranteed to be
    // horizontal latin-1" and every run of it is USCRIPT_LATIN; 16-bit text goes through `RunSegmenter`, whose
    // `ScriptRunIterator` folds Common and Inherited characters into the script run around them and leaves a run
    // of nothing else USCRIPT_COMMON. A word is shaped on its own, so the space of `😀 漢字 ok` is a Common run --
    // and under Common, Noto Sans CJK's Korean `locl` does not apply: its space is 224 units, not the 280 of the
    // space in `a b` (stock Chrome 150 draws `漢` of `x 漢` at 24px sans-serif 1.34 px
    // left of where the Latin space would put it).
    let latin1 = eight_bit(text, req.words);
    // Layout's inline text is one item, shaped whole (`InlineNode::ShapeText`): only a font change splits it.
    let words = if req.words { word_ends(&chars) } else { std::collections::HashSet::from([0]) };
    // Each run's typeface, its text, its first character, its family name and its script (`None`: Common so far).
    let mut runs: Vec<(Typeface, String, usize, Option<String>, Option<rustybuzz::Script>)> = Vec::new();
    let mut word_start = true;
    for (i, &ch) in chars.iter().enumerate() {
        if i > 0 && words.contains(&i) {
            word_start = true;
        }
        if joins_previous(ch) && !word_start {
            if let Some(last) = runs.last_mut() {
                last.1.push(ch);
                continue;
            }
        }
        let emoji = emoji_presentation(ch, chars.get(i + 1).copied());
        // The listed families come first for EVERY character, an emoji included (Blink's FontFallbackIterator has
        // no emoji case: the fallback priority only steers the system fallback below) -- `24px "Noto Color Emoji"`
        // draws 😀 from the named face, at its full-hinting whole-pixel advance (Chrome 150: 30 px; F9).
        // With VARIATION SELECTOR-16 after it the sequence must be the font's own (its cmap format 14 maps
        // the pair): a font with only the plain glyph shapes it as .notdef and Blink reshapes the cluster
        // with the next font (`kEmojiEmojiWithVS`; Chrome 150 draws `16px sans-serif` ☀️ from the emoji font).
        let vs16 = chars.get(i + 1) == Some(&'\u{FE0F}');
        let listed = fonts
            .iter()
            .find(|f| f.0.unichar_to_glyph(ch as i32) != 0 && (!vs16 || maps_variation(&f.0, ch, '\u{FE0F}')))
            .cloned();
        // a character's fallback font is made by file and index, under no family name
        let (tf, name) = listed
            .map(|(tf, n)| (tf, Some(n)))
            .or_else(|| fallback_typeface(mgr, ch, emoji, bold, italic, lang).map(|tf| (tf, None)))
            .unwrap_or_else(|| (primary.0.clone(), Some(primary.1.clone())));
        let script = if latin1 { Some(rustybuzz::script::LATIN) } else { strong_script(ch) };
        match runs.last_mut() {
            Some(last)
                if !word_start
                    && last.0.unique_id() == tf.unique_id()
                    && last.3 == name
                    && (script.is_none() || last.4.is_none() || last.4 == script) =>
            {
                last.1.push(ch);
                last.4 = last.4.or(script);
            }
            _ => runs.push((tf, ch.to_string(), i, name, script)),
        }
        word_start = false;
    }

    // The size the font is made at: Blink's font cache keys a size in hundredths of a pixel
    // (`FontCacheKey`, `kFontSizePrecisionMultiplier` 100, truncated) and makes the platform font at
    // that key's size, so `27.77777777777778px` is a 27.77px font everywhere below -- FreeType's
    // 1/64 px size, HarfBuzz's scale, the em box.
    let size = platform_size(req.size);
    let size64 = (size * 64.0).floor() as i64;
    let used = size64 as f32 / 64.0;
    let mut pen: i64 = 0; // 16.16 px
    let mut width = 0.0f32;
    let mut bounds: Option<[f32; 4]> = None;
    let mut glyph_runs: Vec<GlyphRun> = Vec::new();
    // each character's first UTF-16 code unit, and each glyph's cluster (as one) with its advance
    let mut utf16_at: Vec<usize> = Vec::with_capacity(chars.len() + 1);
    let mut n16 = 0usize;
    for c in &chars {
        utf16_at.push(n16);
        n16 += c.len_utf16();
    }
    utf16_at.push(n16);
    // (cluster, 16.16 advance, run) per glyph; each run's width. Blink sums a run's advances in its 16.16
    // `TextRunLayoutUnit` and makes the run's width a float of the sum (`ShapeResult::ComputeGlyphPositions`),
    // then sums the runs' floats: summing each glyph's float drifted by an ulp over a long run (Chrome 150:
    // `italic 150px serif` "mmmmmmmmmmlli" measures 1611.0010986328125, the per-glyph float sum 1611.0012207).
    let mut glyph_advances: Vec<(usize, i64, usize)> = Vec::new();
    let mut run_widths: Vec<f32> = Vec::with_capacity(runs.len());
    for (run_index, (tf, run, first, name, script)) in runs.iter().enumerate() {
        run_widths.push(0.0);
        // each byte offset's character index (a cluster is a character's first byte)
        let mut char_of_byte = vec![0usize; run.len() + 1];
        for (n, (b, _)) in run.char_indices().enumerate() {
            char_of_byte[b] = n;
        }
        let Some(data) = font_data(tf) else { continue };
        let Some(hb) = shaping_face(tf, &data) else { continue };
        let face = &hb.face;
        let upem = face.units_per_em().max(1) as i64;
        let x_scale = mul_div(size64, 65536, upem); // FT_DivFix(size26.6, upem)
        let mult = hb_mult(size, upem as u16);
        let mut run_advance: i64 = 0; // 16.16 px
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(run);
        if let Ok(l) = lang.parse() {
            buf.set_language(l);
        }
        // the run's script (see `latin1` above): a run of only Common characters is USCRIPT_COMMON
        buf.set_script(script.unwrap_or(rustybuzz::script::COMMON));
        buf.set_direction(rustybuzz::Direction::LeftToRight);
        let shaped = hb.shape(buf);
        let (file_bold, file_italic, outline) = hb.style;
        let synthetic = (italic && !file_italic) || (bold && !file_bold);
        let mut font = Font::new(if synthetic { plain_typeface(mgr, tf) } else { tf.clone() }, Some(used));
        font.set_edging(Edging::AntiAlias);
        // Measured with a non-subpixel font: Chrome's canvas ink boxes are rounded out per glyph, which
        // `SkFontGetBoundsForGlyphs` does only for one. Slight hinting, as fontconfig asks here.
        font.set_subpixel(false);
        font.set_hinting(FontHinting::Slight);
        // Linux's synthetic styles: a weight of 600 or more the face does not have is emboldened, an italic it
        // does not have is skewed by -1/4 (`font_platform_data.cc`) -- for a face the LIST names and for a
        // character's FALLBACK alike. Blink makes both from the font service's file and index, upright to Skia
        // (no fontconfig edit rides on them, [`plain_typeface`]), and `CreateFontPlatformData` synthesizes the
        // italic the typeface lacks; a fallback keeps the italic request because fontconfig's full match, whose
        // `90-synthetic.conf` marks an outline face it would slant as italic, answered italic
        // (`PlatformFallbackFontForCharacter`). Measured on Chrome 150: `italic 150px serif`'s `i` has ink right
        // 62 where its upright face's is 56; `italic 15px Times New Roman`'s 中文 (fallback Noto Sans CJK) ink
        // right 32 and left -2; 😀 ink right 23 instead of 19 at 15px.
        // Both from the face's file, without fontconfig's own synthetic edits ([`plain_typeface`]).
        if bold && !file_bold {
            font.set_embolden(true);
        }
        if italic && !file_italic {
            font.set_skew_x(-0.25);
        }
        // Subpixel positioning is forced on unless the strike's render style is FULL hinting
        // (`WebFontRenderStyle::ApplyToSkFont`), and a font without it advances by whole pixels
        // (`SkFontGetGlyphWidthForHarfBuzz`: `SkScalarRoundToInt` of the strike's width). Full hinting is what gfx
        // makes of a strike fontconfig does not anti-alias -- `Noto Color Emoji` NAMED in the list: Chrome 150's
        // `24px "Noto Color Emoji"` advances 😀 30 px, where the same emoji as a fallback advances 29.9449 (F9).
        // a web font's platform font is made from its file under no family name, so its strike takes the defaults
        let family = name.clone().filter(|n| !crate::web_fonts::is_web_family(n));
        let subpixel = crate::font_style::render_style(family.as_deref(), size, file_bold, file_italic).hinting != crate::fontations::Hinting::Full;
        // Drawn at subpixel positions: Chrome's second glyph lands at its fractional pen position, and
        // `strokeText` is pixel-identical to software-rasterized Chrome 150 only this way.
        let mut draw_font = font.clone();
        draw_font.set_subpixel(subpixel);
        let mut out = GlyphRun { font: draw_font, glyphs: Vec::new(), points: Vec::new(), family };
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let gid = info.glyph_id as u16;
            out.glyphs.push(gid);
            out.points.push(skia_safe::Point::new(
                (pen + em_mult(pos.x_offset as i64, mult)) as f32 / 65536.0,
                -(em_mult(pos.y_offset as i64, mult) as f32 / 65536.0),
            ));
            let hmtx = face.glyph_hor_advance(rustybuzz::ttf_parser::GlyphId(gid)).unwrap_or(0) as i64;
            let mut r = [Rect::default()];
            font.get_bounds(&[gid], &mut r, None);
            let r = r[0];
            if !r.is_empty() {
                let x = (pen + em_mult(pos.x_offset as i64, mult)) as f32 / 65536.0;
                let y = -(em_mult(pos.y_offset as i64, mult) as f32 / 65536.0);
                let g = [r.left.floor() + x, r.top.floor() + y, r.right.ceil() + x, r.bottom.ceil() + y];
                bounds = Some(match bounds {
                    None => g,
                    Some(b) => [b[0].min(g[0]), b[1].min(g[1]), b[2].max(g[2]), b[3].max(g[3])],
                });
            }
            // A bitmap-only face (the colour emoji font) advances by its STRIKE's advance scaled to the
            // size (Noto Color Emoji: 136px at 109ppem -> 19.9633px at 16px), which is what Skia's
            // width is; Blink takes it as `SkiaScalarToHarfBuzzPosition` does, a float product truncated.
            let advance = if outline {
                mul_div(hmtx, x_scale, 64)
            } else {
                let ppem = face
                    .glyph_raster_image(rustybuzz::ttf_parser::GlyphId(gid), u16::MAX)
                    .map(|img| img.pixels_per_em as f32)
                    .unwrap_or(used);
                let mut strike = font.clone();
                strike.set_size(ppem);
                let mut w = [0.0f32];
                strike.get_widths(&[gid], &mut w);
                (w[0] * (size / ppem) * 65536.0) as i64
            };
            // whole pixels without subpixel positioning (the strike's hinted width, rounded)
            let advance = if subpixel { advance } else { ((advance as f64 / 65536.0 + 0.5).floor() as i64) << 16 };
            let adjust = em_mult(pos.x_advance as i64 - hmtx, mult);
            pen += advance + adjust;
            run_advance += advance + adjust;
            let cluster = first + char_of_byte.get(info.cluster as usize).copied().unwrap_or(0);
            glyph_advances.push((utf16_at[cluster.min(chars.len())], advance + adjust, run_index));
        }
        run_widths[run_index] = run_advance.max(0) as f32 / 65536.0;
        width += run_widths[run_index];
        glyph_runs.push(out);
    }
    // the caret before code unit k: the runs before its run, as floats, and the glyphs of the clusters before it in
    // its run, as a 16.16 sum -- in glyph order (left to right)
    let mut carets = Vec::with_capacity(n16 + 1);
    let (mut base, mut fixed, mut current, mut g) = (0.0f32, 0i64, 0usize, 0usize);
    for k in 0..=n16 {
        while g < glyph_advances.len() && glyph_advances[g].0 < k {
            let (_, advance, run) = glyph_advances[g];
            while current < run {
                base += run_widths[current];
                current += 1;
                fixed = 0;
            }
            fixed += advance;
            g += 1;
        }
        carets.push(base + fixed.max(0) as f32 / 65536.0);
    }
    Some(Shaped { runs: glyph_runs, width, bounds, carets, primary: primary.0, size })
}

/// The HarfBuzz face for a face index as fontconfig and FreeType write it: the collection member in the low 16 bits
/// and a variable font's NAMED INSTANCE (1-based) above them -- `Ubuntu` bold is `Ubuntu[wdth,wght].ttf` at
/// `0x50000` here. Chrome's scanner takes the instance's coordinates (`SkTypeface_Fontations::MakeFromData`: "the
/// upper 15 bits of collection index representing a named instance choice"); this face set them, so its advances
/// are the instance's (`HVAR`), where parsing the whole index failed and the run was dropped (F9).
fn hb_face(data: &[u8], index: u32) -> Option<rustybuzz::Face<'_>> {
    let mut face = rustybuzz::Face::from_slice(data, index & 0xFFFF)?;
    let instance = index >> 16;
    if instance > 0 {
        use skrifa::MetadataProvider;
        let font = skrifa::FontRef::from_index(data, index & 0xFFFF).ok()?;
        if let Some(named) = font.named_instances().get(instance as usize - 1) {
            let axes = font.axes();
            let variations: Vec<rustybuzz::Variation> = named
                .user_coords()
                .enumerate()
                .filter_map(|(i, value)| {
                    let tag = axes.get(i)?.tag().to_be_bytes();
                    Some(rustybuzz::Variation { tag: rustybuzz::ttf_parser::Tag::from_bytes(&tag), value })
                })
                .collect();
            face.set_variations(&variations);
        }
    }
    Some(face)
}

/// A font file's own weight and slant -- `OS/2` usWeightClass of 600 or more (or the fsSelection BOLD
/// bit), the fsSelection ITALIC or OBLIQUE bit -- and whether it has outlines (`glyf`, `CFF `,
/// `CFF2`) rather than only bitmaps. A variable font's named instance (the index's upper 16 bits, [`hb_face`]) has
/// the weight of its `wght` coordinate and is slanted by a non-zero `ital` or `slnt` one, as Skia's Fontations
/// typeface reports its style (`fontations_ffi::get_font_style` over the instance's coordinates).
fn file_style(data: &[u8], index: u32) -> (bool, bool, bool) {
    let Ok(face) = rustybuzz::ttf_parser::RawFace::parse(data, index & 0xFFFF) else { return (false, false, true) };
    let has = |tag: &[u8; 4]| face.table(rustybuzz::ttf_parser::Tag::from_bytes(tag)).is_some();
    let outline = has(b"glyf") || has(b"CFF ") || has(b"CFF2");
    let Some(os2) = face.table(rustybuzz::ttf_parser::Tag::from_bytes(b"OS/2")) else { return (false, false, outline) };
    let mut weight = be_u16(os2, 4).unwrap_or(400);
    let selection = be_u16(os2, 62).unwrap_or(0);
    let mut slanted = selection & 0x201 != 0;
    if let Some(instance) = (index >> 16).checked_sub(1) {
        use skrifa::MetadataProvider;
        if let Ok(font) = skrifa::FontRef::from_index(data, index & 0xFFFF) {
            if let Some(named) = font.named_instances().get(instance as usize) {
                let axes = font.axes();
                for (i, value) in named.user_coords().enumerate() {
                    match axes.get(i).map(|a| a.tag().to_be_bytes()) {
                        Some(t) if &t == b"wght" => weight = value.round().clamp(0.0, 1000.0) as u16,
                        Some(t) if (&t == b"ital" || &t == b"slnt") && value != 0.0 => slanted = true,
                        _ => {}
                    }
                }
            }
        }
    }
    (weight >= 600 || selection & 0x20 != 0, slanted, outline)
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn be_i16(b: &[u8], at: usize) -> Option<i16> {
    be_u16(b, at).map(|v| v as i16)
}

/// The `BASE` table's horizontal `romn`, `hang` and `ideo` baselines for the default script, as
/// HarfBuzz's `hb_ot_layout_get_baseline(..., HB_OT_TAG_DEFAULT_SCRIPT, ...)` finds them: the
/// `DFLT` script record's BaseValues, indexed by the tag's position in the BaseTagList.
pub(crate) fn base_baselines(t: &[u8]) -> [Option<i16>; 3] {
    let mut out = [None; 3];
    let Some(axis) = be_u16(t, 4).filter(|&o| o != 0).map(|o| o as usize) else { return out };
    let (Some(tags_off), Some(scripts_off)) = (be_u16(t, axis), be_u16(t, axis + 2)) else { return out };
    if tags_off == 0 || scripts_off == 0 {
        return out;
    }
    let tags = axis + tags_off as usize;
    let scripts = axis + scripts_off as usize;
    let tag_count = be_u16(t, tags).unwrap_or(0) as usize;
    let tag_at = |i: usize| t.get(tags + 2 + 4 * i..tags + 6 + 4 * i);
    let script_count = be_u16(t, scripts).unwrap_or(0) as usize;
    let mut base_script = None;
    for i in 0..script_count {
        let rec = scripts + 2 + 6 * i;
        if t.get(rec..rec + 4) == Some(b"DFLT") {
            base_script = be_u16(t, rec + 4).map(|o| scripts + o as usize);
        }
    }
    let Some(bs) = base_script else { return out };
    let Some(values) = be_u16(t, bs).filter(|&o| o != 0).map(|o| bs + o as usize) else { return out };
    let coord_count = be_u16(t, values + 2).unwrap_or(0) as usize;
    for (slot, want) in [b"romn", b"hang", b"ideo"].iter().enumerate() {
        let Some(i) = (0..tag_count).find(|&i| tag_at(i) == Some(&want[..])) else { continue };
        if i >= coord_count {
            continue;
        }
        let Some(off) = be_u16(t, values + 4 + 2 * i).filter(|&o| o != 0) else { continue };
        out[slot] = be_i16(t, values + off as usize + 2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_lists_are_split_and_unquoted() {
        assert_eq!(family_list("\"Noto Sans CJK KR\", Arial ,sans-serif"), vec!["Noto Sans CJK KR", "Arial", "sans-serif"]);
    }

    #[test]
    fn a_quoted_generic_is_a_family_name() {
        assert_eq!(
            family_list_quoted("\"serif\", serif , 'A b'"),
            vec![("serif".to_string(), true), ("serif".to_string(), false), ("A b".to_string(), true)]
        );
    }

    /// An emoji's grapheme cluster is one shaping word: a flag's two regional indicators, a ZWJ sequence with the
    /// emoji it joins; two flags side by side are two words.
    #[test]
    fn an_emoji_cluster_is_one_word() {
        let starts = |s: &str| {
            let chars: Vec<char> = s.chars().collect();
            let mut w: Vec<usize> = word_ends(&chars).into_iter().collect();
            w.sort_unstable();
            w
        };
        assert_eq!(starts("\u{1F1EC}\u{1F1E7}"), vec![0]);
        assert_eq!(starts("\u{1F1EC}\u{1F1E7}\u{1F1FA}\u{1F1F8}"), vec![0, 2]);
        assert_eq!(starts("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"), vec![0]);
        assert_eq!(starts("\u{4E2D}\u{6587}"), vec![0, 1]);
    }

    /// `HarfBuzzShaper::Shape`'s two paths: Latin-1 text stays 8-bit -- unless canvas space normalization rewrote
    /// one of U+0009..U+000D, which makes it 16-bit -- and a 16-bit word's Common characters have no script.
    #[test]
    fn eight_bit_text_is_latin_and_a_common_character_has_no_script_of_its_own() {
        assert!(eight_bit("a b \u{e9}", true));
        assert!(!eight_bit("a\tb", true), "a tab rewritten to a space makes the canvas text 16-bit");
        assert!(eight_bit("a\tb", false));
        assert!(!eight_bit("\u{1F600} x", true));
        assert_eq!(strong_script(' '), None);
        assert_eq!(strong_script(','), None);
        assert_eq!(strong_script('\u{1F600}'), None, "emoji are Common");
        assert_eq!(strong_script('\u{6F22}'), Some(rustybuzz::script::HAN));
        assert_eq!(strong_script('a'), Some(rustybuzz::script::LATIN));
    }

    #[test]
    fn freetype_and_harfbuzz_rounding() {
        // 'H' (728 units, upem 1000) at 12px: Chrome 8.736068725585938 = 572527 / 65536 (572526.5, half up).
        let x_scale = mul_div(12 * 64, 65536, 1000);
        assert_eq!(mul_div(728, x_scale, 64), 572527);
        // Noto Sans CJK's 'wo' kern (-4 units) at 12px: Chrome -3146 / 65536.
        assert_eq!(em_mult(-4, hb_mult(12.0, 1000)), -3146);
    }

    #[test]
    fn a_base_table_is_read_for_the_default_script() {
        // BASE v1.0, horizAxis at 8; Axis: tagList at 4, scriptList at 14; tags romn ideo;
        // one DFLT script -> BaseScript -> BaseValues with coords 0 and -120.
        let mut t: Vec<u8> = vec![0, 1, 0, 0, 0, 8, 0, 0];
        t.extend([0, 4, 0, 14]); // Axis (at 8)
        t.extend([0, 2]); // BaseTagList (at 12)
        t.extend(b"ideo"); // tags must be sorted: ideo < romn
        t.extend(b"romn");
        // BaseScriptList at 8+14 = 22
        t.extend([0, 1]);
        t.extend(b"DFLT");
        t.extend([0, 8]); // BaseScript at 22+8 = 30
        t.extend([0, 6, 0, 0, 0, 0]); // BaseScript: values at 30+6 = 36
        t.extend([0, 0, 0, 2, 0, 8, 0, 12]); // BaseValues: default 0, 2 coords at 36+8, 36+12
        t.extend([0, 1, 0xFF, 0x88]); // coord ideo = -120
        t.extend([0, 1, 0, 0]); // coord romn = 0
        assert_eq!(base_baselines(&t), [Some(0), None, Some(-120)]);
    }
}
