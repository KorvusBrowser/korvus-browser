//! Text measurement — the one number a page can read to four decimal places.
//!
//! # Why this is at the crate root
//!
//! `ctx.measureText("Hello").width` and the `offsetWidth` of a `<span>` containing "Hello" are the
//! same question asked twice, and a page can ask both. Two implementations that disagree are not
//! duplication, they are an *inconsistency* — a detector cross-checks them in three lines. So this
//! sits beside `value.rs`, below both the cascade and the canvas, and both call it (design D10).
//!
//! What it replaces on the canvas side is `text.chars().count() as f64 * 7.0`, which is the same
//! answer for `"iiiii"` and `"WWWWW"`. Every real font disagrees with that, and disagreeing with
//! every real font is a fingerprint.
//!
//! # Advances and kerning, not glyphs
//!
//! Measuring text does not need glyphs, outlines, or a rasterizer — only advance widths and the
//! kerning pairs that adjust them. That is what keeps this crate free of a font library and of
//! Skia: the table in `font_table.rs` is captured from a real browser, and the arithmetic is a sum.
//!
//! # What is approximate, and how much
//!
//! Advances are exact for the fonts in the table and for printable ASCII. Outside that:
//!
//!  - **A character the table does not cover** takes the font's `0` advance. That is right for a
//!    monospace font and an estimate elsewhere. Latin-1, CJK, and emoji all land here.
//!  - **A font the table does not carry** maps onto the closest one it does — see [`resolve`].
//!  - **Shaping** is not done. Ligatures and contextual forms change the width of a run in some
//!    fonts; the three in the table apply none for plain ASCII, which is why sum-plus-kerning is
//!    exact for them rather than close.

use crate::font_table::{self, FIRST_CHAR, LAST_CHAR};
use std::collections::HashMap;

/// One font's metrics, in fractions of the em. Multiply by the font size to get pixels.
///
/// Em-relative rather than per-size because advances scale linearly: one capture serves 12px and
/// 96px alike, and a table per size would be a table per size *per font*.
#[derive(Debug)]
pub struct FontMetrics {
    pub family: &'static str,
    pub ascent: f32,
    pub descent: f32,
    /// The font's `hhea` line gap (external leading), in ems.
    ///
    /// Not decoration: `line-height: normal` is ascent + descent + **line gap**, and the gap is what
    /// makes 16px Arial an 18px line box rather than a 17px one. Zero for the fonts that carry no
    /// gap (Courier New, the Noto CJK family); 67/2048 em for Arial and 87/2048 for Times New Roman.
    /// See [`crate::layout::inline::FontSpec::line_height`] for how the three are combined.
    pub line_gap: f32,
    pub x_height: f32,
    /// The advance of `0` — what `ch` units mean, and the fallback for a character not in the table.
    pub zero_advance: f32,
    /// The font's average character width as Blink reads it for sizing a text field — the OS/2
    /// `xAvgCharWidth` (`SkFontMetrics::fAvgCharWidth`) — in ems, or `None` when the platform reads no
    /// average off the font. DirectWrite leaves it `None` for EVERY Windows font, where Blink falls
    /// back to the advance of `'x'`; the legacy path sizes in the advance of `'0'`. Captured off the
    /// font by the persona rig, because no script API names it. See [`crate::layout::replaced`].
    pub avg_char_width: Option<f32>,
    /// The font's maximum character width as Blink reads it — `round(fMaxCharWidth)` on Windows, the
    /// `head` bounding box's width elsewhere — in ems. Independent of [`FontMetrics::avg_char_width`]:
    /// a Windows font has a maximum but no average, and Blink keeps the maximum whichever the case.
    pub max_char_width: Option<f32>,
    /// Printable ASCII, `FIRST_CHAR` through `LAST_CHAR`.
    pub advances: &'static [f32],
    /// Only the pairs that kern. Sorted, so a lookup is a binary search.
    pub kerning: &'static [(char, char, f32)],
    /// The cap height, in ems.
    pub cap_height: f32,
    /// The sampled non-ASCII characters whose advances vary one by one -- Latin-1, punctuation,
    /// currency, symbols, emoji -- sorted by character. For a face that has no glyph for
    /// one, the sample is the FALLBACK font's advance, which is what the browser measured.
    pub extra: &'static [(char, f32)],
    /// One advance per script class the capture found uniform in this face and style, in
    /// [`ScriptClass`] order; `0.0` is a class the probe did not sample.
    pub class_advances: [f32; 6],
    /// The printable-ASCII characters the face has a glyph for, bit `c - FIRST_CHAR` ([`ALL_ASCII`] for a
    /// face that has them all). The capture measured the face ALONE, so a character it lacks was measured at
    /// the platform fallback's advance; in a `font-family` list Blink sets that character in the next family
    /// that has it instead ([`PersonaFonts::metrics_styled`]). 41 of the 93 families
    /// Chrome 150 detects on the persona have no Latin letter at all -- a Noto script face is "installed"
    /// because its SPACE is its own.
    pub covered: u128,
}

/// Every printable-ASCII character, as [`FontMetrics::covered`] counts them.
pub const ALL_ASCII: u128 = (1u128 << (LAST_CHAR - FIRST_CHAR + 1)) - 1;

/// One bundle's names for the faces: which face each installed family and each generic
/// keyword is, on that device. Font metrics are the capturing DEVICE's -- the four bundles in the tree
/// come from one host and agree, a capture from another machine will not -- so the engine wears the
/// map of the persona it wears. See `font_table::FONT_MAPS`.
#[derive(Debug)]
pub struct FontMap {
    pub bundle: &'static str,
    /// [`spec::font_map_key`] of `bundle`.
    pub key: u64,
    /// Each installed family (lowercase), sorted, to its face in `font_table::FACES`.
    pub families: &'static [(&'static str, u16)],
    /// Each generic keyword (lowercase), sorted, likewise.
    pub generics: &'static [(&'static str, u16)],
    /// The bundle's standard font: the family a document with no `font-family` uses (`fonts/metrics.json`'s `default`).
    pub default_family: &'static str,
    /// Its face in `font_table::FACES`.
    pub default_face: u16,
}

/// The map `key` names, or the shipped bundle's when none does (a persona with no theme, a test).
pub fn font_map(key: u64) -> &'static FontMap {
    font_table::FONT_MAPS.iter().find(|m| m.key == key).unwrap_or(&font_table::FONT_MAPS[0])
}

thread_local! {
    /// The font map of the persona this thread's engine wears ([`set_worn_font_map`]); 0, the shipped bundle's, until
    /// one is worn.
    static WORN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The persona an engine on this thread wears names its font map (`spec::ThemeProfile::font_map`): the documents this
/// thread makes from now on take its standard font as the user-agent default (`style::ua`). A process wears one
/// persona, and its engine and documents live on one thread.
pub fn set_worn_font_map(key: u64) {
    WORN.with(|w| w.set(key));
}

/// The font map this thread wears ([`set_worn_font_map`]).
pub fn worn_font_map() -> &'static FontMap {
    font_map(WORN.with(|w| w.get()))
}

/// A persona's per-font-box shift of the VERTICAL font metrics (`ascent`/`descent`), in ems, keyed by the box it
/// applies to — the `(ascent, descent)` em-ratios a face was captured with.
///
/// WHY it lives here and not in the canvas readback tap. A font-box fingerprint is coherent only if `measureText`'s
/// `fontBoundingBox` AND the DOM line box (`line-height: normal` = ascent + descent + line gap) move together: both are
/// derived from the one [`FontMetrics`] [`PersonaFonts::metrics_styled`] returns. The earlier canvas-only tap (D95)
/// moved `measureText` but not layout, so a page comparing the two saw a mismatch Chrome never shows. Shifting
/// `ascent`/`descent` at that one funnel moves both by the same amount, so the gap between them — the untouched
/// `line_gap` term — is preserved. `line_gap` and the advances are NOT shifted: this is the vertical half; width stays
/// coherent trivially.
///
/// korvus holds only these opaque numbers and APPLIES them. The MODEL — how much to shift, keyed off which seed —
/// stays korvus-stealth's (J6); it computes the deltas and bakes them into the served persona (a later slice). An empty
/// shift, or no entry for a box, is the no-op.
#[derive(Debug, Default)]
pub struct FontShift {
    /// `(ascent.to_bits(), descent.to_bits())` → `(d_ascent, d_descent)`, all in ems.
    by_box: HashMap<(u32, u32), (f32, f32)>,
}

impl FontShift {
    /// Build a shift from `((ascent_em, descent_em), (d_ascent_em, d_descent_em))` entries.
    pub fn new(entries: impl IntoIterator<Item = ((f32, f32), (f32, f32))>) -> Self {
        FontShift { by_box: entries.into_iter().map(|((a, d), delta)| ((a.to_bits(), d.to_bits()), delta)).collect() }
    }

    /// The `(d_ascent, d_descent)` in ems for a face whose captured box is `(ascent, descent)` ems, if this shifts it.
    fn delta(&self, ascent: f32, descent: f32) -> Option<(f32, f32)> {
        self.by_box.get(&(ascent.to_bits(), descent.to_bits())).copied()
    }
}

thread_local! {
    /// The vertical-metrics shift of the persona this thread's engine wears ([`set_worn_font_shift`]); `None` — no
    /// shift, every metric byte-identical to the captured table — until one is worn.
    static WORN_SHIFT: std::cell::RefCell<Option<std::sync::Arc<FontShift>>> = const { std::cell::RefCell::new(None) };
}

/// Set (or clear with `None`) the worn persona's font-metrics shift for this thread. Call it ONCE at persona provision,
/// before any measurement: [`PersonaFonts::metrics_styled`] caches the resolved (shifted) metrics per thread, so a
/// shift changed after a face is first measured would not take effect until the cache key changes. `None` is the
/// no-op — metrics read exactly as captured (I9/J2).
pub fn set_worn_font_shift(shift: Option<std::sync::Arc<FontShift>>) {
    WORN_SHIFT.with(|s| *s.borrow_mut() = shift);
}

/// Apply the worn persona's vertical-metrics shift to `m` (the metrics [`PersonaFonts::metrics_styled`] resolved for a
/// box). With no worn shift, or none for this box, the SAME `&'static` pointer is returned — byte-identical, no
/// allocation: this is the entire I9/J2 no-op, and it must stay pointer-exact (korvus-page is in the engine's own
/// dependency graph, so there is no "absent from the graph" shield). Otherwise a copy with `ascent`/`descent` moved in
/// ems (everything else `m`'s) is leaked once per `(map, face)` and memoized, so a measure after the first resolution
/// of a box costs nothing.
fn shift_metrics(map: &'static FontMap, m: &'static FontMetrics) -> &'static FontMetrics {
    let (d_asc, d_desc) = match WORN_SHIFT.with(|s| s.borrow().as_ref().and_then(|sh| sh.delta(m.ascent, m.descent))) {
        Some(d) => d,
        None => return m,
    };
    static MADE: std::sync::OnceLock<std::sync::Mutex<HashMap<(u64, usize), &'static FontMetrics>>> = std::sync::OnceLock::new();
    let key = (map.key, m as *const FontMetrics as usize);
    let mut made = MADE.get_or_init(|| std::sync::Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&shifted) = made.get(&key) {
        return shifted;
    }
    let shifted: &'static FontMetrics = Box::leak(Box::new(FontMetrics {
        family: m.family,
        ascent: m.ascent + d_asc,
        descent: m.descent + d_desc,
        line_gap: m.line_gap,
        x_height: m.x_height,
        zero_advance: m.zero_advance,
        avg_char_width: m.avg_char_width,
        max_char_width: m.max_char_width,
        advances: m.advances,
        kerning: m.kerning,
        cap_height: m.cap_height,
        extra: m.extra,
        class_advances: m.class_advances,
        covered: m.covered,
    }));
    made.insert(key, shifted);
    shifted
}

/// A face in its four styles -- normal, bold, italic, bold italic -- as `font_table::FACES` holds it.
#[derive(Debug)]
pub struct FontFace {
    pub key: &'static str,
    pub styles: [&'static FontMetrics; 4],
}

impl FontFace {
    /// The style a `font-weight` of 600 or more (bold) and an italic or oblique `font-style` pick.
    pub fn style(&self, bold: bool, italic: bool) -> &'static FontMetrics {
        self.styles[match (bold, italic) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        }]
    }
}

/// The script classes whose advance is one number per face and style (see `class_advances`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptClass {
    Hangul = 0,
    Jamo = 1,
    Cjk = 2,
    Kana = 3,
    CjkPunct = 4,
    Emoji = 5,
}

impl ScriptClass {
    pub fn of(ch: char) -> Option<ScriptClass> {
        let o = ch as u32;
        Some(match o {
            0xAC00..=0xD7A3 => ScriptClass::Hangul,
            0x1100..=0x11FF | 0x3130..=0x318F => ScriptClass::Jamo,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2FFFF => ScriptClass::Cjk,
            0x3040..=0x30FF | 0x31F0..=0x31FF => ScriptClass::Kana,
            0x3000..=0x303F | 0xFF00..=0xFFEF => ScriptClass::CjkPunct,
            0x1F000..=0x1FAFF | 0x2600..=0x27BF => ScriptClass::Emoji,
            _ => return None,
        })
    }
}

/// Characters that take no advance of their own: variation selectors, zero-width joiners and
/// spaces, emoji skin-tone modifiers and tag characters, and combining marks -- the pieces of a
/// cluster the browser shapes onto the character before them.
pub(crate) fn zero_width(ch: char) -> bool {
    matches!(ch as u32,
        0x200B..=0x200F | 0xFE00..=0xFE0F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0x0300..=0x036F | 0x20D0..=0x20FF)
}

fn is_regional_indicator(ch: char) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&(ch as u32))
}

impl FontMetrics {
    /// Does the face have a glyph for `ch`? Known for printable ASCII ([`FontMetrics::covered`]); any other
    /// character is taken as the face's.
    pub fn covers(&self, ch: char) -> bool {
        match (ch as u32).checked_sub(FIRST_CHAR) {
            Some(i) if ch as u32 <= LAST_CHAR => self.covered & (1u128 << i) != 0,
            _ => true,
        }
    }

    /// The advance of one character, in ems.
    pub fn advance(&self, ch: char) -> f32 {
        let code = ch as u32;
        if (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return self.advances[(code - FIRST_CHAR) as usize];
        }
        // A tab is a space here (layout expands it, this does not), a newline is nothing.
        match ch {
            '\t' => return self.advances[0],
            '\n' | '\r' => return 0.0,
            _ => {}
        }
        if zero_width(ch) {
            return 0.0;
        }
        // A sampled character: its own advance in this face, or the fallback font's.
        if let Ok(i) = self.extra.binary_search_by(|(c, _)| c.cmp(&ch)) {
            return self.extra[i].1;
        }
        // A script this face (or, failing that, the persona's default face) was measured to set at one
        // advance: every Hangul syllable of Noto Sans CJK KR is 0.92 em, every ideograph 1 em.
        if let Some(class) = ScriptClass::of(ch) {
            let own = self.class_advances[class as usize];
            if own > 0.0 {
                return own;
            }
            let fallback = font_table::FACES[worn_font_map().default_face as usize].styles[0].class_advances[class as usize];
            if fallback > 0.0 {
                return fallback;
            }
        }
        // Anything else takes the digit advance -- exact for monospace, an estimate otherwise, and
        // never zero, which would make an unsampled script measure as empty.
        self.zero_advance
    }

    /// The kerning adjustment between two characters, in ems. Zero for the overwhelming majority.
    pub fn kern(&self, a: char, b: char) -> f32 {
        self.kerning
            .binary_search_by(|(x, y, _)| (*x, *y).cmp(&(a, b)))
            .map(|i| self.kerning[i].2)
            .unwrap_or(0.0)
    }

    /// The width of a run, in ems.
    ///
    /// Advances plus kerning. Kerning is what makes this more than a sum: for Arial at 16px,
    /// `"AVATAR To Ya"` is 7.7px narrower than its advances add up to, which is far past anything
    /// a comparison against a real browser would forgive.
    pub fn width(&self, text: &str) -> f32 {
        self.extend(Run::default(), text).total
    }

    /// `run` continued by `text`: the width of the concatenation, with the kerning pair at the join,
    /// by the same additions in the same order as [`FontMetrics::width`] of the whole string -- so a
    /// line breaker can measure a line word by word, never the prefix again, and get the identical
    /// float.
    pub fn extend(&self, run: Run, text: &str) -> Run {
        let Run { mut total, mut previous, mut pending_flag } = run;
        for ch in text.chars() {
            if let Some(p) = previous {
                total += self.kern(p, ch);
            }
            // Two regional indicators are ONE flag glyph: the second adds nothing.
            if is_regional_indicator(ch) {
                if pending_flag {
                    pending_flag = false;
                    previous = Some(ch);
                    continue;
                }
                pending_flag = true;
            } else {
                pending_flag = false;
            }
            total += self.advance(ch);
            previous = Some(ch);
        }
        Run { total, previous, pending_flag }
    }
}

impl FontMetrics {
    /// The units per em this face's advances were measured in, when they show one: every printable
    /// ASCII advance a whole number of units, to within the capture's own error (it measured at
    /// 1000px, where Chrome's advances sit within 1/500 of a unit of the font's).
    pub fn units_per_em(&self) -> Option<u32> {
        thread_local! {
            static UPEM: std::cell::RefCell<HashMap<usize, Option<u32>>> = std::cell::RefCell::new(HashMap::new());
        }
        let key = self as *const FontMetrics as usize;
        if let Some(hit) = UPEM.with(|m| m.borrow().get(&key).copied()) {
            return hit;
        }
        const CANDIDATES: [u32; 12] = [1000, 2048, 1024, 2000, 4096, 256, 512, 1200, 1500, 1600, 2400, 2500];
        let whole = |upem: u32| {
            self.advances.iter().all(|a| {
                let u = a * upem as f32;
                (u - u.round()).abs() < 0.02
            })
        };
        let found = CANDIDATES.into_iter().find(|&u| whole(u));
        UPEM.with(|m| m.borrow_mut().insert(key, found));
        found
    }

    /// The width of a run in px at `size_px`, as Chrome 150 computes it
    /// (measured against google-chrome 150.0.7871.46): the platform font is made at the size in whole
    /// hundredths of a px ([`platform_size`]); each glyph's advance is FreeType's 16.16
    /// `FT_MulDiv(units, x_scale, 64)` with `x_scale = FT_DivFix(size26.6, upem)`; a kerning pair is
    /// HarfBuzz's `em_mult` on the first glyph's advance at Blink's scale (`size * 65536`); and the
    /// glyph advances are summed in `float`. Scaling the em width linearly is exact only when the units
    /// per em divide 65536 (Arial's 2048); for a 1000-unit face (Noto Sans CJK) it is off in the fifth
    /// significant figure -- `"Hello, world!"` at 16px is 96.54417419433594 in Chrome, not 96.544.
    /// A value that is not a whole number of this face's units (a fallback font's advance) scales
    /// linearly.
    pub fn width_px(&self, text: &str, size_px: f32) -> f32 {
        self.width_px_kerned(text, size_px, true)
    }

    /// [`FontMetrics::width_px`], with the kerning pairs only when `kerning` (`font-kerning: none` turns
    /// them off: Chrome 150 lays `abab...` in 16px Noto Sans CJK out 453.516 wide under it, 447.625 without).
    /// The glyphs' 16.16 advances are summed as integers and the sum made a float once -- a text run's
    /// width as Blink's `ShapeResult` holds it -- which layout then snaps up to a `LayoutUnit`
    /// (`ShapeResult::SnappedWidth`): 24 glyphs of 16px Noto Sans Mono CJK are 192.000366, laid out
    /// 192.015625 (the challenge's font probe; the em sum scaled linearly is 192).
    pub fn width_px_kerned(&self, text: &str, size_px: f32, kerning: bool) -> f32 {
        let Some(upem) = self.units_per_em() else {
            let em = if kerning { self.width(text) } else { text.chars().map(|c| self.advance(c)).sum() };
            return em * used_size(size_px);
        };
        let size = platform_size(size_px);
        let size64 = (size * 64.0).floor() as i64;
        let used = size64 as f32 / 64.0;
        let upem = upem as i64;
        let x_scale = mul_div(size64, 65536, upem);
        let mult = (((size * 65536.0) as i64) << 16) / upem;
        let units = |em: f32| {
            let u = em * upem as f32;
            ((u - u.round()).abs() < 0.02).then(|| u.round() as i64)
        };
        let advance = |em: f32| match units(em) {
            Some(u) => mul_div(u, x_scale, 64),
            None => (em as f64 * used as f64 * 65536.0).round() as i64,
        };
        let kern = |em: f32| match units(em) {
            Some(u) => (u * mult + 32768) >> 16,
            None => (em as f64 * size as f64 * 65536.0).round() as i64,
        };
        // Each glyph's advance in 16.16, a kerning pair added to its first glyph.
        let mut glyphs: Vec<i64> = Vec::new();
        let mut previous: Option<char> = None;
        let mut pending_flag = false;
        for ch in text.chars() {
            if let Some(p) = previous.filter(|_| kerning) {
                let k = self.kern(p, ch);
                if k != 0.0 {
                    if let Some(last) = glyphs.last_mut() {
                        *last += kern(k);
                    }
                }
            }
            if is_regional_indicator(ch) {
                if pending_flag {
                    pending_flag = false;
                    previous = Some(ch);
                    continue;
                }
                pending_flag = true;
            } else {
                pending_flag = false;
            }
            glyphs.push(advance(self.advance(ch)));
            previous = Some(ch);
        }
        glyphs.iter().sum::<i64>() as f32 / 65536.0
    }
}

/// FreeType's `FT_MulDiv`: `a * b / c`, rounded half away from zero.
fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    let p = a * b;
    let r = (p.abs() + c / 2) / c;
    if p < 0 { -r } else { r }
}

/// The size Blink makes a platform font at: the requested size in whole hundredths of a px
/// (`FontCacheKey` keys sizes times `kFontSizePrecisionMultiplier`, 100, truncated).
pub fn platform_size(size_px: f32) -> f32 {
    if size_px <= 0.0 {
        return 0.0;
    }
    (size_px * 100.0) as u32 as f32 / 100.0
}

/// A measured run's width so far, and what the next character needs from it: the previous character
/// (for the kerning pair) and whether a regional indicator is waiting for its partner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Run {
    pub total: f32,
    previous: Option<char>,
    pending_flag: bool,
}

/// Where measurements come from.
///
/// A trait because the metrics are the *persona's*, not this machine's: a persona that carries its
/// own captured tables plugs in here, and the built-in one is what a persona without them gets.
/// It is also the seam a real font stack would come in through, if a headful build ever wants one.
pub trait FontProvider: Send + Sync {
    /// The metrics for a `font-family` list, which may name several fonts in preference order.
    fn metrics(&self, families: &str) -> &'static FontMetrics;

    /// The same, in the style a bold weight and an italic slant pick. A provider with
    /// one style per face answers with it.
    fn metrics_styled(&self, families: &str, bold: bool, italic: bool) -> &'static FontMetrics {
        let _ = (bold, italic);
        self.metrics(families)
    }

    /// The same at a `font-weight` (1 to 1000): an installed face has the bold style from 600, a web font's family
    /// as many faces as its `@font-face` rules declare ([`WithWebFonts`]).
    fn metrics_weighted(&self, families: &str, weight: u16, italic: bool) -> &'static FontMetrics {
        self.metrics_styled(families, weight >= 600, italic)
    }

    /// The same at a font size, in px: a variable web font with an `opsz` axis is set at the size it is drawn at
    /// ([`WithWebFonts`]); every other face measures alike at every size.
    fn metrics_at(&self, families: &str, weight: u16, italic: bool, size_px: f32) -> &'static FontMetrics {
        let _ = size_px;
        self.metrics_weighted(families, weight, italic)
    }
}

/// The optical size a font of `size_px` is set at (Chrome's `font-optical-sizing: auto`: the font size in CSS px),
/// to a quarter pixel -- layout and paint name a variable font's instance by it alike.
pub fn optical_size(size_px: f32) -> f32 {
    (size_px * 4.0).round() / 4.0
}

/// The name a variable web font's instance is registered under for text of `weight` and `size_px` (the render
/// backend's `<font>@<weight>@<size>`; a font without the axes draws as itself).
pub fn instance_name(font: &str, weight: u16, size_px: f32) -> String {
    format!("{font}@{weight}@{}", optical_size(size_px))
}

/// The captured table, under the SHIPPED bundle's names. See [`PersonaFonts`] for another bundle's.
#[derive(Debug, Default)]
pub struct BuiltinFonts;

impl FontProvider for BuiltinFonts {
    fn metrics(&self, families: &str) -> &'static FontMetrics {
        self.metrics_styled(families, false, false)
    }

    fn metrics_styled(&self, families: &str, bold: bool, italic: bool) -> &'static FontMetrics {
        PersonaFonts::shipped().metrics_styled(families, bold, italic)
    }
}

/// The captured table under one bundle's names: the fonts of the persona being worn.
#[derive(Debug, Clone, Copy)]
pub struct PersonaFonts {
    map: &'static FontMap,
}

impl PersonaFonts {
    /// The map [`spec::ThemeProfile::font_map`] names.
    pub fn for_key(key: u64) -> PersonaFonts {
        PersonaFonts { map: font_map(key) }
    }

    /// The fonts of the persona this thread wears ([`worn_font_map`]) -- the shipped bundle's until one is worn.
    pub fn shipped() -> PersonaFonts {
        PersonaFonts { map: worn_font_map() }
    }
}

impl FontProvider for PersonaFonts {
    fn metrics(&self, families: &str) -> &'static FontMetrics {
        self.metrics_styled(families, false, false)
    }

    /// The list's first face that exists (its primary font) -- and, when that face lacks printable-ASCII
    /// characters, the face Blink sets each of them in: the next family of the list that has it, else the
    /// platform fallback the primary face's capture measured ([`with_fallbacks`]).
    fn metrics_styled(&self, families: &str, bold: bool, italic: bool) -> &'static FontMetrics {
        // The last list resolved, per thread: a layout measures run after run in one `font-family`,
        // and resolving the list walks and searches every family in it (C3 VERIFY).
        thread_local! {
            static LAST: std::cell::RefCell<(usize, String, bool, bool, Option<&'static FontMetrics>)> =
                const { std::cell::RefCell::new((0, String::new(), false, false, None)) };
        }
        let map = self.map as *const FontMap as usize;
        LAST.with(|last| {
            let mut last = last.borrow_mut();
            if let (true, Some(m)) = (last.0 == map && last.1 == families && last.2 == bold && last.3 == italic, last.4) {
                return m;
            }
            let faces: Vec<&'static FontFace> = families.split(',').filter_map(|family| resolve_face_in(self.map, family)).collect();
            let m = match faces.split_first() {
                None => font_table::FACES[self.map.default_face as usize].style(bold, italic),
                Some((primary, rest)) => {
                    let primary = primary.style(bold, italic);
                    if primary.covered == ALL_ASCII {
                        primary
                    } else {
                        let rest: Vec<&'static FontMetrics> = rest.iter().map(|f| f.style(bold, italic)).collect();
                        with_fallbacks(primary, &rest)
                    }
                }
            };
            // The one point canvas measureText AND layout line metrics both read: a worn persona's vertical shift is
            // applied here, so both move together (the no-op returns `m` unchanged when nothing is worn). Cached below,
            // so it costs nothing after a box is first resolved.
            let m = shift_metrics(self.map, m);
            last.0 = map;
            last.1.clear();
            last.1.push_str(families);
            last.2 = bold;
            last.3 = italic;
            last.4 = Some(m);
            m
        })
    }
}

/// The faces a document's `@font-face` rules downloaded, as layout measures text in them: by family (lowercase), each
/// face's descriptors and its font's metrics ([`web_font_metrics`]). The realm hands the table to its page whenever a
/// face's font arrives (`Page::set_web_fonts`).
#[derive(Debug)]
pub struct WebFonts {
    families: HashMap<String, Vec<WebFace>>,
    /// Which table this is, for the process: a measurement cached under one is never another's.
    id: u64,
}

impl Default for WebFonts {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        WebFonts { families: HashMap::new(), id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }
    }
}

/// One `@font-face` face whose font has arrived (see [`WebFonts`]).
#[derive(Debug, Clone)]
pub struct WebFace {
    /// The `font-weight` descriptor's range.
    pub weight: (f32, f32),
    /// The `font-style` descriptor, lowercase (`normal`, `italic`, `oblique 10deg`).
    pub style: String,
    /// The `unicode-range` descriptor's ranges; empty = every character.
    pub ranges: Vec<(u32, u32)>,
    pub metrics: &'static FontMetrics,
    /// A variable font's metrics at a weight and size (its `wght` and `opsz` axes there), which text of the used
    /// `font-weight` -- clamped to the descriptor's range, as Chrome sets the axis -- and font size measures with;
    /// `None` for a font without either axis.
    pub instance: Option<WeightInstances>,
}

/// A variable font's metrics at each weight and font size (see [`WebFace::instance`]).
#[derive(Clone)]
pub struct WeightInstances(pub std::sync::Arc<dyn Fn(u16, f32) -> Option<&'static FontMetrics> + Send + Sync>);

impl std::fmt::Debug for WeightInstances {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WeightInstances")
    }
}

impl WebFonts {
    pub fn add(&mut self, family: &str, face: WebFace) {
        self.families.entry(family.to_ascii_lowercase()).or_default().push(face);
    }

    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    /// Which table this is, for the process (never another's, though it hold the same faces).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// What the table holds, as a number: two tables with one signature measure alike.
    pub fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut families: Vec<(&String, &Vec<WebFace>)> = self.families.iter().collect();
        families.sort_by(|a, b| a.0.cmp(b.0));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (family, faces) in families {
            family.hash(&mut h);
            for f in faces {
                (f.weight.0.to_bits(), f.weight.1.to_bits(), &f.style, &f.ranges, f.metrics as *const FontMetrics as usize, f.instance.is_some()).hash(&mut h);
            }
        }
        h.finish()
    }

    fn declares(&self, family: &str) -> bool {
        self.families.contains_key(family)
    }

    /// The face `family` sets Latin text in at `weight` and style: among its faces whose `unicode-range` holds `A`
    /// (all of them when none does), the best match -- style first, then weight (CSS Fonts 4 §5.2; a tie goes to the
    /// face declared last).
    fn face(&self, family: &str, weight: f32, italic: bool, size_px: f32) -> Option<&'static FontMetrics> {
        let faces = self.families.get(family)?;
        let latin = |f: &&WebFace| f.ranges.is_empty() || f.ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&0x41));
        let mut pool: Vec<&WebFace> = faces.iter().filter(latin).collect();
        if pool.is_empty() {
            pool = faces.iter().collect();
        }
        let style_rank = |f: &WebFace| -> u8 {
            let (is_italic, is_oblique) = (f.style.starts_with("italic"), f.style.starts_with("oblique"));
            match (italic, is_italic, is_oblique) {
                (true, true, _) | (false, false, false) => 0,
                (_, _, true) => 1,
                _ => 2,
            }
        };
        let best_style = pool.iter().map(|f| style_rank(f)).min()?;
        let distance = |f: &WebFace| -> (u8, f32) {
            let (lo, hi) = f.weight;
            if (lo..=hi).contains(&weight) {
                return (0, 0.0);
            }
            let heavier = lo > weight;
            let gap = if heavier { lo - weight } else { weight - hi };
            let order = if (400.0..=500.0).contains(&weight) {
                if heavier && lo <= 500.0 { 1 } else if !heavier { 2 } else { 3 }
            } else if (weight < 400.0) == heavier {
                2
            } else {
                1
            };
            (order, gap)
        };
        pool.into_iter()
            .rev()
            .filter(|f| style_rank(f) == best_style)
            .min_by(|a, b| distance(a).partial_cmp(&distance(b)).unwrap_or(std::cmp::Ordering::Equal))
            .map(|f| f.instance.as_ref().and_then(|i| (i.0)(instance_weight(f.weight, weight), size_px)).unwrap_or(f.metrics))
    }
}

/// The weight a variable face draws text of `weight` at: the used weight clamped to the face's `font-weight` range
/// (CSS Fonts 4 §5.2; the font then clamps it to its axis). Layout and paint name the same instance by it.
pub fn instance_weight(range: (f32, f32), weight: f32) -> u16 {
    weight.clamp(range.0.min(range.1), range.1.max(range.0)).round() as u16
}

/// The metrics a downloaded font measures with, made once per font (`name`: the family name the render backend
/// registered it under) from its file's (`file`, asked the first time only) and kept for the process, as every face
/// of the table is. `None` past the 4096th font: its family then measures as one not loaded.
pub fn web_font_metrics(name: &str, file: impl FnOnce() -> Option<spec::FontFileMetrics>) -> Option<&'static FontMetrics> {
    let mut made = web_fonts_made().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = made.get(name).map(|m| m.0) {
        return Some(hit);
    }
    if made.len() >= 4096 {
        return None;
    }
    let file = file()?;
    if file.advances.len() != (LAST_CHAR - FIRST_CHAR + 1) as usize {
        return None;
    }
    let m: &'static FontMetrics = Box::leak(Box::new(FontMetrics {
        family: Box::leak(file.family.clone().into_boxed_str()),
        ascent: file.ascent,
        descent: file.descent,
        line_gap: file.line_gap,
        x_height: file.x_height,
        zero_advance: file.zero_advance,
        avg_char_width: file.avg_char_width,
        max_char_width: file.max_char_width,
        advances: Box::leak(file.advances.clone().into_boxed_slice()),
        kerning: Box::leak(file.kerning.clone().into_boxed_slice()),
        cap_height: file.cap_height,
        extra: Box::leak(file.extra.clone().into_boxed_slice()),
        class_advances: file.class_advances,
        covered: file.covered,
    }));
    made.insert(name.to_string(), (m, file.weight_axis.or(file.opsz_axis.map(|_| (0.0, 0.0))), file.opsz_axis.is_some()));
    Some(m)
}

/// The fonts [`web_font_metrics`] made: each one's metrics, its `wght` axis, and whether it has an `opsz` axis.
#[allow(clippy::type_complexity)]
fn web_fonts_made() -> &'static std::sync::Mutex<HashMap<String, (&'static FontMetrics, Option<(f32, f32)>, bool)>> {
    static MADE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, (&'static FontMetrics, Option<(f32, f32)>, bool)>>> = std::sync::OnceLock::new();
    MADE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// A downloaded font's `wght` axis, once [`web_font_metrics`] has made its metrics: `Some` for a variable font (one with
/// an `opsz` axis alone answers an empty range).
pub fn web_font_weight_axis(name: &str) -> Option<(f32, f32)> {
    web_fonts_made().lock().unwrap_or_else(|e| e.into_inner()).get(name).and_then(|m| m.1)
}

/// Has a downloaded font an `opsz` axis (once [`web_font_metrics`] has made its metrics)? Its instances are then per
/// size as well as weight.
pub fn web_font_has_opsz(name: &str) -> bool {
    web_fonts_made().lock().unwrap_or_else(|e| e.into_inner()).get(name).is_some_and(|m| m.2)
}

/// The persona's fonts with a document's web fonts ahead of them: a family an `@font-face` rule declares is its faces
/// alone -- one whose font has not arrived is skipped, as a family nothing answers to -- and every other family is
/// the persona's.
pub struct WithWebFonts {
    pub persona: PersonaFonts,
    pub web: std::sync::Arc<WebFonts>,
}

impl FontProvider for WithWebFonts {
    fn metrics(&self, families: &str) -> &'static FontMetrics {
        self.metrics_weighted(families, 400, false)
    }

    fn metrics_styled(&self, families: &str, bold: bool, italic: bool) -> &'static FontMetrics {
        self.metrics_weighted(families, if bold { 700 } else { 400 }, italic)
    }

    fn metrics_weighted(&self, families: &str, weight: u16, italic: bool) -> &'static FontMetrics {
        self.metrics_at(families, weight, italic, 16.0)
    }

    fn metrics_at(&self, families: &str, weight: u16, italic: bool, size_px: f32) -> &'static FontMetrics {
        // the last list resolved, per thread, as the persona's own (a layout measures run after run in one list)
        thread_local! {
            static LAST: std::cell::RefCell<((usize, usize), String, u16, bool, u32, Option<&'static FontMetrics>)> =
                const { std::cell::RefCell::new(((0, 0), String::new(), 0, false, 0, None)) };
        }
        let table = (self.web.id as usize, self.persona.map as *const FontMap as usize);
        let size_key = optical_size(size_px).to_bits();
        if let Some(m) = LAST.with(|l| {
            let l = l.borrow();
            (l.0 == table && l.1 == families && l.2 == weight && l.3 == italic && l.4 == size_key).then_some(l.5).flatten()
        }) {
            return m;
        }
        let m = self.resolve(families, weight, italic, size_px);
        LAST.with(|l| *l.borrow_mut() = (table, families.to_string(), weight, italic, size_key, Some(m)));
        m
    }
}

impl WithWebFonts {
    fn resolve(&self, families: &str, weight: u16, italic: bool, size_px: f32) -> &'static FontMetrics {
        let bold = weight >= 600;
        let mut web = false;
        let mut faces: Vec<&'static FontMetrics> = Vec::new();
        for family in families.split(',') {
            let name = family.trim().trim_matches(['"', '\'']).trim().to_lowercase();
            if self.web.declares(&name) {
                web = true;
                faces.extend(self.web.face(&name, weight as f32, italic, size_px));
            } else if let Some(face) = resolve_face_in(self.persona.map, family) {
                faces.push(face.style(bold, italic));
            }
        }
        if !web {
            return self.persona.metrics_styled(families, bold, italic);
        }
        match faces.split_first() {
            None => font_table::FACES[self.persona.map.default_face as usize].style(bold, italic),
            Some((primary, _)) if primary.covered == ALL_ASCII => primary,
            Some((primary, rest)) => with_fallbacks(primary, rest),
        }
    }
}

/// `primary` with each printable-ASCII character it has no glyph for taken from the first of `rest` -- the
/// list's later families -- that has one (Blink's `FontFallbackIterator`: a character goes to the next font of
/// the list, and to the platform's fallback only after the last), its kerning pairs kept where both characters
/// are its own. A character no face of the list has keeps the primary's captured advance: the capture measured
/// the face alone, so that IS the platform fallback's. Made once per distinct list and kept for the process.
fn with_fallbacks(primary: &'static FontMetrics, rest: &[&'static FontMetrics]) -> &'static FontMetrics {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static MADE: OnceLock<Mutex<HashMap<Vec<usize>, &'static FontMetrics>>> = OnceLock::new();
    let key: Vec<usize> = std::iter::once(primary).chain(rest.iter().copied()).map(|m| m as *const FontMetrics as usize).collect();
    let made = MADE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = made.lock().unwrap_or_else(|e| e.into_inner()).get(&key).copied() {
        return hit;
    }
    let n = (LAST_CHAR - FIRST_CHAR + 1) as usize;
    let source = |i: usize| -> &'static FontMetrics {
        std::iter::once(primary).chain(rest.iter().copied()).find(|m| m.covered & (1u128 << i) != 0).unwrap_or(primary)
    };
    let sources: Vec<&'static FontMetrics> = (0..n).map(source).collect();
    let advances: Vec<f32> = (0..n).map(|i| sources[i].advances[i]).collect();
    let index = |c: char| (c as u32).checked_sub(FIRST_CHAR).map(|i| i as usize).filter(|i| *i < n);
    let own = |m: &'static FontMetrics, c: char| index(c).is_some_and(|i| std::ptr::eq(sources[i], m));
    // each face once, in list order (a list may name one face twice)
    let mut faces: Vec<&'static FontMetrics> = Vec::new();
    for m in std::iter::once(primary).chain(rest.iter().copied()) {
        if !faces.iter().any(|f| std::ptr::eq(*f, m)) {
            faces.push(m);
        }
    }
    let mut kerning: Vec<(char, char, f32)> = Vec::new();
    for m in faces {
        kerning.extend(m.kerning.iter().copied().filter(|(a, b, _)| own(m, *a) && own(m, *b)));
    }
    kerning.sort_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)));
    kerning.dedup_by(|x, y| x.0 == y.0 && x.1 == y.1);
    let covered = rest.iter().fold(primary.covered, |c, m| c | m.covered);
    let made_one: &'static FontMetrics = Box::leak(Box::new(FontMetrics {
        advances: Box::leak(advances.into_boxed_slice()),
        kerning: Box::leak(kerning.into_boxed_slice()),
        covered,
        ..*primary
    }));
    made.lock().unwrap_or_else(|e| e.into_inner()).insert(key, made_one);
    made_one
}

/// One family name to the face the persona has under it -- an INSTALLED family or a generic keyword,
/// from the persona's own capture: `sans-serif` is Noto Sans CJK KR on this persona,
/// `monospace` the MS Gothic face, Helvetica and Liberation Sans are Arial's face.
///
/// A family the persona does not have resolves to NOTHING, so the list moves on to its next family
/// and, at the end, to the persona's default -- which is what the browser does, and what makes font
/// detection answer as it does there. Mapping every familiar name onto one of three tables (the
/// previous rule, "the persona is a Mac") made Georgia, Verdana, Segoe UI and Roboto measure as
/// installed on a persona that has none of them.
pub fn resolve_face(family: &str) -> Option<&'static FontFace> {
    resolve_face_in(worn_font_map(), family)
}

/// [`resolve_face`] under one bundle's names.
pub fn resolve_face_in(map: &FontMap, family: &str) -> Option<&'static FontFace> {
    let raw = family.trim();
    let quoted = raw.starts_with(['"', '\'']);
    let trimmed = raw.trim_matches(['"', '\'']).trim();
    // Lowercased only when it has to be: an ASCII name (nearly all of them) is compared folded, in
    // place, so a text measurement no longer allocates once per family in its list (C3
    // VERIFY: gitlab's hydration spent ~40 ms in `to_lowercase` here).
    let owned;
    let name: &str = if trimmed.bytes().any(|b| b.is_ascii_uppercase() || !b.is_ascii()) {
        owned = trimmed.to_lowercase();
        &owned
    } else {
        trimmed
    };
    if name.is_empty() {
        return None;
    }
    let lookup = |table: &'static [(&'static str, u16)], key: &str| {
        table.binary_search_by(|(n, _)| (*n).cmp(key)).ok().map(|i| &font_table::FACES[table[i].1 as usize])
    };
    if let Some(face) = lookup(map.families, name) {
        return Some(face);
    }
    if !quoted {
        // An unquoted generic keyword: `sans-serif`, `serif`, `monospace`, `system-ui`, ...
        return lookup(map.generics, name);
    }
    // A QUOTED name is a family name, never a generic keyword (CSS Fonts 4). Chrome on Linux still
    // lets three of them through to fontconfig's substitution, which answers with that alias's font:
    // Skia's `IsFallbackFontAllowed` is true for "sans", "serif" and "monospace" and nothing else.
    // Measured in the persona (the fp probe's `canvas2d.measure`, `32px "<name>", monospace`): quoted
    // "serif" is a serif font (555.81), quoted "sans-serif" and "Verdana" are not found and fall to
    // monospace (392.32).
    match name {
        "serif" | "monospace" => lookup(map.generics, name),
        "sans" => lookup(map.generics, "sans-serif"),
        _ => None,
    }
}

/// [`resolve_face`], normal style.
pub fn resolve(family: &str) -> Option<&'static FontMetrics> {
    resolve_face(family).map(|f| f.styles[0])
}

/// [`resolve`] under the map [`spec::ThemeProfile::font_map`] names.
pub fn resolve_in(key: u64, family: &str) -> Option<&'static FontMetrics> {
    resolve_face_in(font_map(key), family).map(|f| f.styles[0])
}

/// What `measureText` hands back, and what a line box needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// The font's line gap at this size, rounded the way `ascent` and `descent` are.
    ///
    /// Not a `TextMetrics` member on the JS side — Chrome's `TextMetrics` has no line-gap attribute
    /// and adding one would be a surface extra. It is here because this struct is also "what a line
    /// box needs", and a line box needs all three.
    pub line_gap: f32,
}

impl TextMetrics {
    /// The line box height a run of this text occupies with `line-height: normal`.
    pub fn line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

/// Measure `text` in `families` at `size_px`.
///
/// Two details are there because a real browser has them, and both were found by measuring one:
///
///  - **the size is quantized to 1/64 px** before it scales anything. Chrome does this, and it is
///    visible at any size that is not a whole number: `11pt` is 14.666667px, Chrome uses
///    14.65625px, and the difference shows up in the fourth significant figure of every width.
///    A page comparing `measureText` against a known font's metrics would see it.
///  - **the ascent and descent come back as integers.** Chrome rounds `fontBoundingBoxAscent`, so
///    a fractional one is a tell on its own.
pub fn measure(provider: &dyn FontProvider, families: &str, size_px: f32, text: &str) -> TextMetrics {
    measure_styled(provider, families, size_px, false, false, text)
}

/// [`measure`] in the style a bold weight and an italic slant pick.
pub fn measure_styled(provider: &dyn FontProvider, families: &str, size_px: f32, bold: bool, italic: bool, text: &str) -> TextMetrics {
    let m = provider.metrics_styled(families, bold, italic);
    let size = used_size(size_px);
    TextMetrics {
        width: m.width_px(text, size_px),
        ascent: (m.ascent * size).round(),
        descent: (m.descent * size).round(),
        line_gap: (m.line_gap * size).round(),
    }
}

/// The font size a browser actually uses: the specified one, floored to 1/64 of a pixel.
///
/// Font engines work in fixed point, and Chrome's unit is 1/64 px. Scaling by the un-quantized size
/// gives widths that are *nearly* right, which is the worst kind of wrong for a number a page reads
/// to four decimal places.
pub fn used_size(size_px: f32) -> f32 {
    if size_px <= 0.0 {
        return 0.0;
    }
    (size_px * 64.0).floor() / 64.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arial() -> &'static FontMetrics {
        &font_table::ARIAL
    }

    /// The engine-side vertical shift (D96) moves the canvas font box (`measureText` `fontBoundingBoxAscent+Descent`)
    /// and the DOM line box (`line-height: normal`) by the SAME amount, so the gap between them — the untouched line
    /// gap — is preserved. That coherence is exactly what the canvas-only readback tap (D95) could not give. Width
    /// (advances) and `line_gap` are left untouched; the no-op (no worn shift) returns the identical metrics.
    #[test]
    fn a_vertical_font_shift_moves_the_canvas_box_and_the_line_box_together() {
        // The two boxes a reader derives from one FontMetrics at `size`, by the same arithmetic as `measure_styled`
        // (canvas) and `FontSpec::line_height` (layout): the canvas box is ascent+descent (the alphabetic-baseline
        // case the probe uses); the line box adds line_gap.
        fn boxes(m: &FontMetrics, size: f32) -> (f32, f32, f32) {
            let s = used_size(size);
            let cbox = (m.ascent * s).round() + (m.descent * s).round();
            let lbox = cbox + (m.line_gap * s).round();
            let width: f32 = m.advances.iter().sum();
            (cbox, lbox, width)
        }
        let size = 16.0_f32;
        // OFF on its own thread: fresh WORN_SHIFT (None) and a fresh per-thread metrics cache.
        let (off_c, off_l, off_w, asc, desc) = std::thread::spawn(move || {
            let m = PersonaFonts::for_key(0).metrics_styled("Arial", false, false);
            let (c, l, w) = boxes(m, size);
            (c, l, w, m.ascent, m.descent)
        })
        .join()
        .unwrap();
        // ON on its own thread: the shift is set BEFORE the first measure (the production order), keyed on this face's box.
        let (on_c, on_l, on_w) = std::thread::spawn(move || {
            set_worn_font_shift(Some(std::sync::Arc::new(FontShift::new([((asc, desc), (0.25_f32, 0.125_f32))]))));
            let m = PersonaFonts::for_key(0).metrics_styled("Arial", false, false);
            boxes(m, size)
        })
        .join()
        .unwrap();

        assert!(on_c > off_c, "the canvas font box grows with the shift: {off_c} -> {on_c}");
        assert_eq!(on_l - on_c, off_l - off_c, "the gap (line_gap) is preserved -- the canvas box and the line box stay consistent");
        assert_eq!(on_c - off_c, on_l - off_l, "the line box moves by the SAME delta as the canvas box (coherent)");
        assert_eq!(on_w, off_w, "a vertical shift leaves the advances (width) untouched");
    }

    /// A face that has only the space (a Noto script face) sets every other ASCII character in the next family of
    /// the list -- the face Blink's fallback reaches -- and its kerning stays with the characters that are its own.
    #[test]
    fn a_character_the_primary_face_lacks_is_the_next_familys() {
        let times = &font_table::TIMES_NEW_ROMAN;
        let space_only: &'static FontMetrics = Box::leak(Box::new(FontMetrics { covered: 1, ..*arial() }));
        let m = with_fallbacks(space_only, &[times]);
        assert_eq!(m.advance(' '), arial().advance(' '), "the face's own space");
        assert_eq!(m.advance('m'), times.advance('m'));
        assert_eq!(m.advance('W'), times.advance('W'));
        assert_eq!(m.kern('A', 'V'), times.kern('A', 'V'), "the pair is the next family's, both characters set in it");
        assert!(std::ptr::eq(m, with_fallbacks(space_only, &[times])), "made once per list");
        // a face that has every character is its own answer
        assert_eq!(arial().covered, ALL_ASCII);
    }

    fn width(families: &str, size: f32, text: &str) -> f32 {
        measure(&BuiltinFonts, families, size, text).width
    }

    /// Chrome's own answer, captured at 1000px and scaled — the numbers this file exists to match.
    /// Any drift shows up here first.
    const CHROME_AT_1000PX: &[(&str, &str, f32)] = &[
        ("Arial", "1234567890", 5561.5234),
        ("Arial", "Hello, World!", 5705.5664),
        ("Arial", "AVATAR To Ya", 6464.8438),
        ("Arial", "mmmmmmmmmmlli", 8996.5820),
        ("Arial", "The quick brown fox jumps over the lazy dog", 19787.5977),
        ("Arial", "iiiiiiiiii", 2221.6797),
        ("Arial", "WWWWWWWWWW", 9438.4766),
        ("Arial", "korvus-browser", 6834.9609),
        ("Arial", "Wait, AWAY! To. Ya-Va", 10113.2812),
        ("Arial", "{}[]()<>|/\\@#$%^&*", 8415.0391),
        ("Times New Roman", "1234567890", 5000.0000),
        ("Times New Roman", "Hello, World!", 5529.2969),
        ("Times New Roman", "AVATAR To Ya", 6324.7070),
        ("Times New Roman", "mmmmmmmmmmlli", 8611.8164),
        ("Times New Roman", "The quick brown fox jumps over the lazy dog", 18273.4375),
        ("Times New Roman", "iiiiiiiiii", 2778.3203),
        ("Times New Roman", "WWWWWWWWWW", 9438.4766),
        ("Times New Roman", "korvus-browser", 6276.3672),
        ("Times New Roman", "Wait, AWAY! To. Ya-Va", 9769.0430),
        ("Times New Roman", "{}[]()<>|/\\@#$%^&*", 8676.7578),
        ("Courier New", "1234567890", 6000.9766),
        ("Courier New", "Hello, World!", 7801.2695),
        ("Courier New", "AVATAR To Ya", 7201.1719),
        ("Courier New", "mmmmmmmmmmlli", 7801.2695),
        ("Courier New", "The quick brown fox jumps over the lazy dog", 25804.1992),
        ("Courier New", "iiiiiiiiii", 6000.9766),
        ("Courier New", "WWWWWWWWWW", 6000.9766),
        ("Courier New", "korvus-browser", 8401.3672),
        ("Courier New", "Wait, AWAY! To. Ya-Va", 12602.0508),
        ("Courier New", "{}[]()<>|/\\@#$%^&*", 10801.7578),
    ];

    #[test]
    fn measurements_match_the_browser_they_were_captured_from() {
        for (family, text, expected) in CHROME_AT_1000PX {
            let got = width(family, 1000.0, text);
            assert!(
                (got - expected).abs() < 0.01,
                "{family} {text:?}: got {got}, Chrome says {expected}"
            );
        }
    }

    // The claim the whole table rests on: this is not a per-character sum. For Arial at 16px,
    // `"AVATAR To Ya"` is 7.7px narrower than its advances add up to.
    #[test]
    fn kerning_is_the_difference_between_right_and_nearly_right() {
        let m = arial();
        let text = "AVATAR To Ya";
        let unkerned: f32 = text.chars().map(|c| m.advance(c)).sum();
        let kerned = m.width(text);
        assert!(kerned < unkerned);
        assert!(
            (unkerned - kerned) * 16.0 > 7.0,
            "kerning is worth {}px at 16px",
            (unkerned - kerned) * 16.0
        );
        // And the kerned answer is the browser's.
        assert!((kerned * 1000.0 - 6464.8438).abs() < 0.01);
    }

    // A monospace font is the easy case to be wrong about: every character the same width, no
    // kerning at all, so anything that gets this wrong is broken in an obvious way.
    #[test]
    fn a_monospace_font_gives_every_character_the_same_advance() {
        let m = &font_table::COURIER_NEW;
        assert!(m.kerning.is_empty());
        let a = m.advance('i');
        for ch in ['W', 'm', '.', ' ', '0'] {
            assert_eq!(m.advance(ch), a, "{ch:?}");
        }
        assert!((width("monospace", 16.0, "iiii") - width("monospace", 16.0, "WWWW")).abs() < 1e-4);
    }

    #[test]
    fn advances_scale_linearly_with_the_font_size() {
        let at16 = width("Arial", 16.0, "Hello");
        let at32 = width("Arial", 32.0, "Hello");
        assert!((at32 - at16 * 2.0).abs() < 1e-3);
        assert_eq!(width("Arial", 0.0, "Hello"), 0.0);
    }

    // The replaced answer was `chars().count() * 7.0`, which is the same number for both of these.
    #[test]
    fn narrow_and_wide_text_of_the_same_length_measure_differently() {
        let narrow = width("Arial", 16.0, "iiiiiiiiii");
        let wide = width("Arial", 16.0, "WWWWWWWWWW");
        assert!(wide > narrow * 4.0, "{wide} vs {narrow}");
    }

    #[test]
    fn a_family_list_takes_the_first_font_it_knows() {
        assert_eq!(BuiltinFonts.metrics("Nonexistent, Arial").family, "Arial");
        assert_eq!(BuiltinFonts.metrics("\"Times New Roman\", serif").family, "Times New Roman");
        // Menlo is not installed on the persona, so the list moves on to `monospace`.
        assert_eq!(BuiltinFonts.metrics("Menlo, monospace").family, "MS Gothic");
        // And a list naming nothing installed ends at the persona's standard font.
        assert_eq!(BuiltinFonts.metrics("Nonexistent").family, "Noto Sans CJK KR", "and falls back");
        assert_eq!(BuiltinFonts.metrics("").family, "Noto Sans CJK KR");
    }

    // The generic families resolve the way the PERSONA's browser resolves them, as the Chrome 152 bundle
    // (chrome-152-linux-20261016T1022, desktop Chrome on its ko_KR host) recorded them -- not a macOS guess.
    #[test]
    fn the_generic_families_resolve_as_the_persona_would() {
        // The persona resolves `serif` to Noto Serif CJK JP (F9 font capture 2026-10-11; the face was keyed `serif`).
        assert_eq!(BuiltinFonts.metrics("serif").family, "Noto Serif CJK JP");
        assert_eq!(BuiltinFonts.metrics("sans-serif").family, "Noto Sans CJK KR");
        assert_eq!(BuiltinFonts.metrics("monospace").family, "MS Gothic");
        // `system-ui` is the desktop's UI font, which desktop Chrome on this host reports as Ubuntu Sans.
        assert_eq!(BuiltinFonts.metrics("system-ui").family, "Ubuntu Sans");
    }

    // A character outside the ASCII table takes the advance the browser measured for its script:
    // Arial has no ideographs, and Chrome sets `中` in the fallback font at 1 em. Never
    // zero -- a CJK paragraph must not measure as empty.
    #[test]
    fn a_character_outside_the_table_falls_back_rather_than_vanishing() {
        let m = arial();
        assert!((m.advance('中') - 1.0).abs() < 1e-3, "{}", m.advance('中'));
        assert_eq!(m.advance('\n'), 0.0, "a newline is not a character on the line");
        assert!((width("Arial", 16.0, "中文") - 32.0).abs() < 1e-2);
        // Hangul in Arial is Noto Sans CJK KR's 0.92 em: Chrome measures `16px Arial` "가나다라" 58.88.
        assert!((width("Arial", 16.0, "가나다라") - 58.88).abs() < 1e-2);
    }

    // Chrome rounds these, so a fractional ascent is a tell on its own.
    #[test]
    fn the_metrics_a_line_box_needs_are_there_and_are_integers() {
        let m = measure(&BuiltinFonts, "Arial", 16.0, "Hg");
        assert_eq!(m.ascent, 14.0, "Chrome says 14 at 16px Arial");
        assert_eq!(m.descent, 3.0);
        assert_eq!(m.ascent, m.ascent.round());
        assert_eq!(m.line_gap, 1.0, "16px Arial's 0.523px gap rounds to a whole pixel of leading");
        assert_eq!(m.line_height(), 18.0, "14 + 3 + 1, which is what the control Brave reports");
        assert!(m.line_height() > 16.0, "Arial's em box overflows its size, as it does in Chrome");
    }

    // The line gap is the third term of `line-height: normal`, and it is not a rounding detail:
    // dropping it makes a 24px Arial line box 27px where the browser says 28. These four are read
    // back from the control Brave 151 at 2048px, where the reported numbers ARE the font's design
    // units -- ascent 1854 / descent 434 / line box 2355 for Arial, so the gap is 67/2048.
    #[test]
    fn every_font_carries_the_line_gap_the_browser_reports() {
        for (family, gap_2048) in
            [("Arial", 67.0), ("Times New Roman", 87.0), ("Courier New", 0.0)]
        {
            let m = BuiltinFonts.metrics(family);
            assert_eq!(m.family, family);
            assert!(
                (m.line_gap * 2048.0 - gap_2048).abs() < 0.5,
                "{family}: {} em is not {gap_2048}/2048",
                m.line_gap
            );
        }
    }

    // Found by measuring a real Chrome: `11pt` is 14.666667px, and Chrome uses 14.65625 -- the
    // specified size floored to 1/64 px, which is the unit font engines work in. Without it every
    // width at a fractional size is wrong in its fourth significant figure.
    #[test]
    fn the_font_size_is_quantized_to_a_sixty_fourth_of_a_pixel() {
        assert_eq!(used_size(16.0), 16.0, "a whole number is already exact");
        assert_eq!(used_size(11.0 * 96.0 / 72.0), 14.65625);
        assert_eq!(used_size(0.0), 0.0);
        // Chrome's answer for `11pt Arial` measuring "Korvus 1.0".
        let width = width("Arial", 11.0 * 96.0 / 72.0, "Korvus 1.0");
        assert!((width - 70.0609).abs() < 0.001, "got {width}");
    }

    #[test]
    fn the_kerning_tables_are_sorted_so_the_lookup_is_valid() {
        for font in font_table::ALL {
            let mut previous: Option<(char, char)> = None;
            for (a, b, _) in font.kerning {
                if let Some(p) = previous {
                    assert!(p < (*a, *b), "{} kerning is not sorted at {a:?}{b:?}", font.family);
                }
                previous = Some((*a, *b));
            }
            assert_eq!(
                font.advances.len(),
                (LAST_CHAR - FIRST_CHAR + 1) as usize,
                "{} has the wrong number of advances",
                font.family
            );
        }
    }
}

/// The [`spec::CssValues`] contract: text measurement and colour parsing, which are the two things
/// the canvas needs from the CSS layer.
///
/// The canvas bindings live in another plugin and reach these through the kernel `Context`, never
/// through a crate dependency — the same shape `RenderHandle` uses (design D3). One service rather
/// than two because they are one answer to one question: *what does this CSS value mean here*.
impl spec::CssValues for BuiltinFonts {
    fn measure_text(&self, families: &str, size_px: f32, text: &str) -> [f32; 3] {
        let m = measure(self, families, size_px, text);
        [m.width, m.ascent, m.descent]
    }

    fn measure_text_styled(&self, families: &str, size_px: f32, bold: bool, italic: bool, text: &str) -> [f32; 3] {
        let m = measure_styled(self, families, size_px, bold, italic, text);
        [m.width, m.ascent, m.descent]
    }

    fn measure_text_in(&self, font_map: u64, families: &str, size_px: f32, bold: bool, italic: bool, text: &str) -> [f32; 3] {
        let m = measure_styled(&PersonaFonts::for_key(font_map), families, size_px, bold, italic, text);
        [m.width, m.ascent, m.descent]
    }

    fn default_family(&self, font_map: u64) -> String {
        self::font_map(font_map).default_family.to_string()
    }

    fn parse_color(&self, text: &str) -> Option<[u8; 4]> {
        match crate::value::Color::parse_str(text).ok()? {
            // `currentColor` on a canvas has no element to take a colour from; per spec the
            // assignment is ignored rather than guessed at.
            crate::value::Color::CurrentColor => None,
            crate::value::Color::Rgba(c) => Some(c.to_rgba8()),
        }
    }

    /// The parser already holds the alpha as an f32; handing it over unquantized is what lets
    /// `ctx.fillStyle = "rgba(0,0,0,0.5)"` read back as `0.5` rather than `0.502`.
    fn parse_color_alpha(&self, text: &str) -> Option<([u8; 3], f32)> {
        match crate::value::Color::parse_str(text).ok()? {
            crate::value::Color::CurrentColor => None,
            crate::value::Color::Rgba(c) => Some(([c.r, c.g, c.b], c.a.clamp(0.0, 1.0))),
        }
    }

    /// Blink's own colour model ([`crate::css_color`]): the canvas keeps what it was given -- floats, in the space
    /// it was written in -- where the cascade above keeps 8 bits.
    fn parse_canvas_color(&self, text: &str, element: bool, current: Option<&str>) -> Option<spec::CanvasColor> {
        use crate::css_color::{parse_canvas_color, parse_css_color, CanvasParse, Color, Context};
        // `GetCurrentColor`: the colour text parsed as `CSSParser::ParseColor` parses it, else black.
        let cur = || current.and_then(parse_css_color).unwrap_or_else(Color::black);
        let c = match parse_canvas_color(text, element)? {
            CanvasParse::Color(c) => c,
            CanvasParse::CurrentColor => cur(),
            CanvasParse::Function(v) => v.resolve(&Context { current: cur(), element }),
        };
        Some(canvas_color(&c))
    }
}

fn canvas_color(c: &crate::css_color::Color) -> spec::CanvasColor {
    spec::CanvasColor {
        paint: c.to_sk_color4f(),
        text: c.serialize_as_canvas_color(),
        oklab_stop: c.oklab_gradient_stop(),
        legacy: c.interpolates_as_legacy(),
    }
}
