//! Glyph outlines as Chrome 150 makes them on Linux, for the glyph MASKS of filled text.
//!
//! Chrome's font manager on Linux is fontconfig over Skia's Fontations scanner (`skia/ext/font_utils.cc`:
//! `SkFontMgr_New_FCI(fci, SkFontScanner_Make_Fontations())`), so every system font it draws is a
//! `SkTypeface_Fontations`: skrifa outlines the glyph, hinted as `SkFontationsScalerContext` asks, and Skia's
//! generic `GenerateImageFromPath` turns the path into the mask (the gamma pre-blend and `kGenA8FromLCD` are
//! Skia's own). This engine's Skia is FreeType-backed, and FreeType's own rasterizer made every filled glyph's
//! mask: measured against stock Chrome 150, `16px Arial` "Hwmqxli" differed in 201 of 382 pixels while the
//! same text STROKED -- which Skia rasterizes from the glyph path on both sides -- was identical.
//!
//! So a filled run's glyphs are handed to Skia as PATHS: skrifa (the version Chromium 150 vendors) outlines
//! each at the device pixel size through the same pen the Fontations bridge uses
//! (`src/ports/fontations/src/verbs_points_pen.rs`), hinted as its scaler context decides, and a custom
//! typeface carries them. The run is drawn in device space, at size 1 with an identity glyph matrix, so the
//! path Skia rasterizes is the outline exactly -- a rescale by the canvas transform would move hinted
//! coordinates that sit on the 1/64 pixel grid by an ulp, which Skia's fixed-point edge setup truncates a
//! whole step.

use std::cell::RefCell;
use std::collections::HashMap;

use skia_safe::{Path, PathFillType, Point, Typeface};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Engine, GlyphStyles, HintingInstance, HintingMode, HintingOptions, OutlineGlyphCollection,
    OutlineGlyphFormat, OutlinePen, SmoothMode, Target,
};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};

/// Skia's `SkFontHinting`, as Blink's `WebFontRenderStyle` sets it from fontconfig.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Hinting {
    None,
    Slight,
    Normal,
    Full,
}

/// How one strike is outlined: the hinting the scaler context ends up with and whether the font asked for the
/// autohinter (`kForceAutohinting_Flag`, fontconfig's `autohint`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Strike {
    pub hinting: Hinting,
    pub force_autohint: bool,
    /// An aliased (`kBW_Format`) mask: Fontations hints it with the mono instance.
    pub mono: bool,
}

/// `fontations_ffi::make_font_ref`: a font file's face `index` -- a collection's member, or the file itself
/// when it is one font and the index's low 16 bits are 0.
fn font_ref(data: &[u8], index: u32) -> Option<FontRef<'_>> {
    match skrifa::raw::FileRef::new(data).ok()? {
        skrifa::raw::FileRef::Font(f) => (index & 0xFFFF == 0).then_some(f),
        skrifa::raw::FileRef::Collection(c) => c.get(index & 0xFFFF).ok(),
    }
}

/// The variation location a face index names: FreeType's convention, which `SkTypeface_Fontations::MakeFromData`
/// keeps -- the upper 16 bits of the collection index a 1-based NAMED INSTANCE whose coordinates the typeface
/// takes (`coordinates_for_shifted_named_instance_index`); the default location for 0. fontconfig answers a variable
/// face's instance this way (`Ubuntu` bold: `Ubuntu[wdth,wght].ttf` at `0x50000`), and Chrome draws its glyphs at
/// the instance's weight (F9).
fn instance_location(font: &FontRef, index: u32) -> skrifa::instance::Location {
    match (index >> 16).checked_sub(1).and_then(|i| font.named_instances().get(i as usize)) {
        Some(named) => named.location(),
        None => skrifa::instance::Location::default(),
    }
}

/// The hinting instance `SkFontationsScalerContext`'s constructor makes for `strike` at `ppem`, or `None` for
/// an unhinted draw. A font whose glyphs need their instructions (`hinting_reliant`) is always mono-hinted.
fn hinting_instance(outlines: &OutlineGlyphCollection, styles: &GlyphStyles, ppem: f32, strike: Strike, location: LocationRef) -> Option<HintingInstance> {
    if outlines.require_interpreter() || (strike.mono && strike.hinting != Hinting::None) {
        return HintingInstance::new(outlines, Size::new(ppem), location, HintingMode::Strong).ok();
    }
    if strike.mono {
        return None;
    }
    #[derive(PartialEq)]
    enum Auto {
        Fallback,
        ForceForGlyf,
        ForceForGlyfAndCff,
    }
    let mut auto = if strike.force_autohint { Auto::ForceForGlyfAndCff } else { Auto::Fallback };
    let mode = match strike.hinting {
        Hinting::None => return None,
        Hinting::Slight => {
            if auto != Auto::ForceForGlyfAndCff {
                auto = Auto::ForceForGlyf;
            }
            SmoothMode::Light
        }
        // `kFull` would ask for LCD hinting on an LCD strike; a canvas strike is not one.
        Hinting::Normal | Hinting::Full => SmoothMode::Normal,
    };
    let target = Target::Smooth { mode, symmetric_rendering: true, preserve_linear_metrics: false };
    let engine = match (auto, outlines.format()) {
        (Auto::ForceForGlyf, Some(OutlineGlyphFormat::Glyf)) | (Auto::ForceForGlyfAndCff, _) => Engine::Auto(Some(styles.clone())),
        _ => Engine::AutoFallback,
    };
    HintingInstance::new(outlines, Size::new(ppem), location, HintingOptions { engine, target }).ok()
}

/// The Fontations bridge's `VerbsPointsPen`: y flipped, a contour's move emitted only once it draws, a segment
/// that goes nowhere dropped, and a close only after a drawing verb.
struct Pen {
    verbs: Vec<u8>,
    points: Vec<Point>,
    started: bool,
    current: Point,
}

const MOVE: u8 = 0;
const LINE: u8 = 1;
const QUAD: u8 = 2;
const CUBIC: u8 = 4;
const CLOSE: u8 = 5;

impl Pen {
    fn going_to(&mut self, p: Point) {
        if !self.started {
            self.started = true;
            self.verbs.push(MOVE);
            self.points.push(self.current);
        }
        self.current = p;
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = Point::new(x, -y);
        if self.started {
            self.close();
            self.started = false;
        }
        self.current = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = Point::new(x, -y);
        if self.current != p {
            self.going_to(p);
            self.verbs.push(LINE);
            self.points.push(p);
        }
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let p0 = Point::new(cx0, -cy0);
        let p1 = Point::new(x, -y);
        if self.current != p0 || self.current != p1 {
            self.going_to(p1);
            self.verbs.push(QUAD);
            self.points.push(p0);
            self.points.push(p1);
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let p0 = Point::new(cx0, -cy0);
        let p1 = Point::new(cx1, -cy1);
        let p2 = Point::new(x, -y);
        if self.current != p0 || self.current != p1 || self.current != p2 {
            self.going_to(p2);
            self.verbs.push(CUBIC);
            self.points.push(p0);
            self.points.push(p1);
            self.points.push(p2);
        }
    }

    fn close(&mut self) {
        if matches!(self.verbs.last(), Some(&(QUAD | CUBIC | LINE | MOVE))) {
            self.verbs.push(CLOSE);
        }
    }
}

/// A glyph's outline, as `generatePathForGlyphId` gives it to Skia: `SkPath::Make` over the pen's verbs and
/// points, winding. `None` when skrifa cannot draw it (no outline: a bitmap or colour glyph, a bad id).
fn glyph_path(outlines: &OutlineGlyphCollection, gid: u16, ppem: f32, instance: Option<&HintingInstance>, location: LocationRef) -> Option<Path> {
    let glyph = outlines.get(GlyphId::new(gid as u32))?;
    let settings = match instance {
        Some(i) => DrawSettings::hinted(i, false),
        None => DrawSettings::unhinted(Size::new(ppem), location),
    };
    let mut pen = Pen { verbs: Vec::new(), points: Vec::new(), started: false, current: Point::default() };
    glyph.draw(settings, &mut pen).ok()?;
    #[allow(deprecated)]
    Some(Path::new_from(&pen.points, &pen.verbs, &[], PathFillType::Winding, false))
}

/// Does the face have colour or bitmap glyph tables? Their glyphs are images, which this path does not draw.
pub(crate) fn has_color_tables(data: &[u8], index: u32) -> bool {
    font_ref(data, index).map(|f| f.cbdt().is_ok() || f.sbix().is_ok() || f.colr().is_ok()).unwrap_or(true)
}

thread_local! {
    /// A face's glyph styles (the autohinter's per-font precomputation), by the font data's address and index.
    static STYLES: RefCell<HashMap<(usize, u32), GlyphStyles>> = RefCell::new(HashMap::new());
}

/// The device-space paths of `glyphs` at `ppem` for `strike`, in order; `None` for the whole run when the face
/// cannot be read or any glyph has no outline (the caller then draws the run as before). `position`: where on its
/// axes the typeface was made, when its index names no instance (a variable web font's weight).
pub(crate) fn run_paths(data: &[u8], index: u32, ppem: f32, strike: Strike, glyphs: &[u16], position: &[skia_safe::font_arguments::variation_position::Coordinate]) -> Option<Vec<Path>> {
    let font = font_ref(data, index)?;
    let outlines = font.outline_glyphs();
    let key = (data.as_ptr() as usize, index);
    let styles = STYLES.with(|s| s.borrow_mut().entry(key).or_insert_with(|| GlyphStyles::new(&outlines)).clone());
    let location = if index >> 16 == 0 && !position.is_empty() {
        let tag = |c: &skia_safe::font_arguments::variation_position::Coordinate| skrifa::Tag::new(&[c.axis.a(), c.axis.b(), c.axis.c(), c.axis.d()]);
        font.axes().location(position.iter().map(|c| (tag(c), c.value)))
    } else {
        instance_location(&font, index)
    };
    let instance = hinting_instance(&outlines, &styles, ppem, strike, LocationRef::from(&location));
    glyphs.iter().map(|&g| glyph_path(&outlines, g, ppem, instance.as_ref(), LocationRef::from(&location))).collect()
}

/// A typeface whose glyph `i` is `paths[i]` (advance 0: a run is drawn at explicit positions).
pub(crate) fn path_typeface(paths: &[Path]) -> Option<Typeface> {
    let mut b = skia_safe::utils::CustomTypefaceBuilder::new();
    for (i, p) in paths.iter().enumerate() {
        b.set_glyph(i as u16, 0.0, p);
    }
    b.detach()
}

/// `sk_relax`: a device-matrix scale as a glyph strike keys it, rounded to 1/1024 (`SkScalarRoundToScalar` is
/// `floor(x + 0.5)`). Under `scale(0.384)` Chrome outlines glyphs at 0.3837890625 and places them at 0.384.
fn relax(x: f32) -> f32 {
    (x * 1024.0 + 0.5).floor() / 1024.0
}

/// Does a strike of `size` px drawn with `paint` on `c` under the scale `(sx, sy)` get LCD masks
/// (`SkScalerContext::MakeRecAndEffects`)? Only on a surface with a subpixel layout whose blitter draws LCD (N32,
/// source-over: otherwise the painter hands the scaler `fBitmapFallbackProps`), and only for a strike no bigger
/// than 48 px (`too_big_for_lcd`: the area under the relaxed scale when the transform scales, else the size).
fn lcd_mask(c: &skia_safe::Canvas, paint: &skia_safe::Paint, size: f32, (sx, sy): (f32, f32)) -> bool {
    use skia_safe::{BlendMode, ColorType, PixelGeometry};
    let geometry = c.base_props().pixel_geometry();
    let layout = matches!(geometry, PixelGeometry::RGBH | PixelGeometry::BGRH | PixelGeometry::RGBV | PixelGeometry::BGRV);
    let blitter = c.image_info().color_type() == ColorType::N32 && paint.blend_mode_or(BlendMode::SrcOver) == BlendMode::SrcOver;
    let too_big = if sx != 1.0 || sy != 1.0 {
        let (rx, ry) = (relax(sx), relax(sy));
        (rx * ry - 0.0 * 0.0) * (size * size) > 48.0 * 48.0
    } else {
        size > 48.0
    };
    layout && blitter && !too_big
}

/// The run's own (FreeType) font for a draw [`fill_run`] does not make -- a stroke above all -- with the edging
/// Chrome's strike would have on this surface: LCD where [`lcd_mask`] allows it and the strike's style asks for
/// subpixel rendering (a stroke's mask is Skia's own, from the stroked path, so its LCD is Chrome's), no
/// anti-aliasing where the style has none.
pub(crate) fn surface_font(c: &skia_safe::Canvas, run: &crate::text::GlyphRun, size: f32, paint: &skia_safe::Paint) -> skia_safe::Font {
    use skia_safe::font::Edging;
    let mut font = run.font.clone();
    let fs = run.font.typeface().font_style();
    let style = crate::font_style::render_style(run.family.as_deref(), size, *fs.weight() >= 600, fs.slant() != skia_safe::font_style::Slant::Upright);
    let m = c.local_to_device_as_3x3();
    if !style.antialias {
        font.set_edging(Edging::Alias);
    } else if style.subpixel_rendering && !m.has_perspective() && lcd_mask(c, paint, size, (m.scale_x(), m.scale_y())) {
        font.set_edging(Edging::SubpixelAntiAlias);
    }
    font
}

/// Draw one shaped run of FILLED text as Chrome 150 on Linux draws it: `true` when it did, `false` when the run
/// is one this does not draw (a stroke, a colour or bitmap face, a synthetic style, a rotated, skewed, flipped
/// or non-uniformly scaled transform, a filter) and the caller draws it through its typeface as before.
///
/// Skia's routing (`GlyphRunListPainter::drawForBitmapDevice`), with the run's font as Blink makes it -- the
/// strike's style from [`crate::font_style`], subpixel positioning unless the hinting is full:
/// * a run whose text matrix is over 256 px (`SkStrikeSpec::ShouldDrawAsPath`) is drawn as PATHS: each glyph
///   unhinted at 64 px (`SkFont::setupForAsPaths`), scaled by `size / 64` from its position, under the
///   canvas's own transform;
/// * any other is drawn as MASKS from the glyphs hinted at the strike's pixel size -- the text size times the
///   RELAXED scale -- in device space (see the module doc), with Fontations' own rules: no `kGenA8FromLCD`, full
///   hinting collapsed to normal unless the mask is LCD, LCD only on a surface with a subpixel layout that Skia's
///   blitter draws it to (N32, source-over) and a strike no bigger than 48 px.
pub(crate) fn fill_run(c: &skia_safe::Canvas, run: &crate::text::GlyphRun, size: f32, origin: (f32, f32), paint: &skia_safe::Paint) -> bool {
    use skia_safe::{font::Edging, Font, FontHinting, Matrix, PaintStyle};
    if run.glyphs.is_empty() || paint.style() != PaintStyle::Fill || paint.image_filter().is_some() || paint.path_effect().is_some() {
        return false;
    }
    if run.font.is_embolden() || run.font.skew_x() != 0.0 || run.font.scale_x() != 1.0 {
        return false;
    }
    let m = c.local_to_device_as_3x3();
    if m.has_perspective() || m.skew_x() != 0.0 || m.skew_y() != 0.0 {
        return false;
    }
    let (sx, sy, tx, ty) = (m.scale_x(), m.scale_y(), m.translate_x(), m.translate_y());
    if !(sx > 0.0 && sy > 0.0 && size > 0.0) {
        return false;
    }
    let Some(data) = crate::text::font_data(&run.font.typeface()) else { return false };
    let position = run.font.typeface().variation_design_position().unwrap_or_default();
    if has_color_tables(&data.0, data.1) {
        return false;
    }
    let fs = run.font.typeface().font_style();
    let style = crate::font_style::render_style(run.family.as_deref(), size, *fs.weight() >= 600, fs.slant() != skia_safe::font_style::Slant::Upright);
    let antialias = style.antialias;

    // `ShouldDrawAsPath`: the text matrix under the REAL transform
    let (wx, wy) = (sx * size, sy * size);
    if wx * wx > 256.0 * 256.0 || wy * wy > 256.0 * 256.0 {
        let paths = match run_paths(&data.0, data.1, 64.0, Strike { hinting: Hinting::None, force_autohint: false, mono: false }, &run.glyphs, &position) {
            Some(p) => p,
            None => return false,
        };
        let mut path_paint = paint.clone();
        path_paint.set_anti_alias(antialias);
        let exact_ctm = paint.shader().is_some() || paint.mask_filter().is_some();
        let scale = size / 64.0;
        for (path, pos) in paths.iter().zip(run.points.iter()) {
            let gm = Matrix::scale_translate((scale, scale), (origin.0 + pos.x, origin.1 + pos.y));
            if exact_ctm {
                c.draw_path(&path.with_transform(&gm), &path_paint);
            } else {
                let saved = c.save();
                c.concat(&gm);
                c.draw_path(path, &path_paint);
                c.restore_to_count(saved);
            }
        }
        return true;
    }

    let (rx, ry) = (relax(sx), relax(sy));
    if rx != ry {
        return false;
    }
    let ppem = ry * size;
    let lcd = antialias && style.subpixel_rendering && lcd_mask(c, paint, size, (sx, sy));
    let mut hinting = style.hinting;
    if hinting == Hinting::Full && !lcd {
        hinting = Hinting::Normal;
    }
    let strike = Strike { hinting, force_autohint: style.autohint, mono: !antialias };
    let Some(paths) = run_paths(&data.0, data.1, ppem, strike, &run.glyphs, &position) else { return false };
    let Some(tf) = path_typeface(&paths) else { return false };
    let mut font = Font::new(tf, Some(1.0));
    // Blink forces subpixel positioning unless the hinting is full; the style's own positioning is off at a
    // device scale factor of 1, and so are linear metrics
    font.set_subpixel(style.hinting != Hinting::Full);
    font.set_edging(if !antialias {
        Edging::Alias
    } else if lcd {
        Edging::SubpixelAntiAlias
    } else {
        Edging::AntiAlias
    });
    font.set_hinting(FontHinting::None);
    font.set_linear_metrics(false);
    // Device space: `positionMatrix = CTM.preTranslate(origin)` keeps its translation, the positions are scaled
    // as `mapPoints` scales them, so Skia adds the same numbers in the same order.
    let (dtx, dty) = (tx + sx * origin.0, ty + sy * origin.1);
    let positions: Vec<skia_safe::Point> = run.points.iter().map(|p| skia_safe::Point::new(p.x * sx, p.y * sy)).collect();
    let mut device_paint = paint.clone();
    if let Some(shader) = paint.shader() {
        // the shader still maps from the user space: translate(d) * local == the transform
        device_paint.set_shader(shader.with_local_matrix(&Matrix::new_all(sx, 0.0, tx - dtx, 0.0, sy, ty - dty, 0.0, 0.0, 1.0)));
    }
    let ids: Vec<u16> = (0..paths.len() as u16).collect();
    let saved = c.save();
    c.set_matrix(&skia_safe::M44::from(Matrix::translate((dtx, dty))));
    c.draw_glyphs_at(&ids, positions.as_slice(), (0.0, 0.0), &font, &device_paint);
    c.restore_to_count(saved);
    true
}

#[cfg(test)]
mod tests;
