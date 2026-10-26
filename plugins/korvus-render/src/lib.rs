//! `render` — the pixel engine behind canvas/WebGL. The `webapi` front-end records draw ops into a
//! display list and calls a [`RenderBackend`] for readback (`getImageData`/`toDataURL`). This replaces
//! the legacy static replay oracle with a live record→raster→readback pipeline.
//! Behind `RenderPlugin`: Skia-backed Canvas2D (`canvas2d`), a software
//! GLES rasterizer with its own GLSL interpreter (`gl`), and a Web Audio graph renderer (`audio`).
//! `NullBackend` (transparent) and `SolidBackend` (the trivial rect rasterizer) remain for tests.

pub mod audio;
mod canvas2d;
mod font_match;
mod local_font;
mod png_decode;
mod modern_decode;
mod video_decode;
mod text;
mod fontations;
mod font_style;
mod web_fonts;
pub mod gl;
/// Owner 2026-10-12: WebGL on this PC's GPU through Chrome's own ANGLE, as Chrome draws it.
pub mod gpu;
/// C1 (goal 1021, plugin ABI 3 wave 1): the frame tap -- the page's last painted RGBA buffer, kept for
/// the plugin frame slot (`plugin_host::on_frame`) when a plugin wants frames, zero-cost otherwise.
pub mod frame;

use skia_safe::jpeg_encoder;
use skia_safe::{surfaces, AlphaType, Color, ColorType, ImageInfo, Pixmap};
use spec::kernel::{Context, Plugin, PluginConfig};
use spec::{
    AudioOp, AudioQuery, AudioQueryResult, Canvas2DOp, GlContextAttributes, GlOp, GlQuery,
    GlQueryResult, HitTest, ImageFormat, RenderBackend, RenderHandle, RenderParams, Rgba8,
};
use std::sync::Arc;

/// The default render plugin: `provide`s a [`RenderHandle`] (Skia backend) as the render service on the
/// kernel `Context`. The canvas/WebGL bindings read it via `ctx.get::<RenderHandle>()`, so loading a
/// *different* render plugin instead swaps the backend without touching the consumer (A3.2).
pub struct RenderPlugin;
impl Plugin for RenderPlugin {
    fn name(&self) -> &str {
        "render.skia"
    }
    fn provides(&self) -> &[&str] {
        &["render"]
    }
    fn apply(&self, ctx: &Context, _config: &PluginConfig) -> spec::kernel::Result<()> {
        let backend: Arc<dyn RenderBackend> = Arc::new(SkiaBackend);
        ctx.provide(RenderHandle(backend));
        Ok(())
    }
}

/// Whether this run loads the render plugin. `KORVUS_RENDER` off (`KORVUS_RENDER=off`, an internal knob with no product
/// flag -- D111) leaves it out: no [`RenderHandle`] is provided, so every consumer takes its no-backend branch (a canvas
/// reads back transparent, WebGL and video have no backend, web fonts are not measured, there is no screenshot), and no
/// GPU service starts -- the lean engine, korvus's row against the no-render engines (the benchmark's C7).
pub fn wanted() -> bool {
    !matches!(std::env::var("KORVUS_RENDER").as_deref().map(str::trim), Ok("0" | "false" | "no" | "off"))
}

/// No-op backend: produces a transparent buffer. Used when a persona has no render target; also the
/// behavior every backend inherits by default from the trait.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullBackend;

impl RenderBackend for NullBackend {
    fn name(&self) -> &str {
        "null"
    }
}

/// R0 trivial backend: executes the fill-style + rectangle ops into an RGBA8 buffer — enough to prove
/// the record→raster→readback pipeline end to end WITHOUT a real rasterizer. R2 swaps in Skia (which
/// handles paths/text/gradients/compositing bit-identically). Blending here is plain source-over so a
/// semi-transparent fill composites correctly for the rectangle subset.
#[derive(Debug, Default, Clone, Copy)]
pub struct SolidBackend;

impl RenderBackend for SolidBackend {
    fn name(&self) -> &str {
        "solid"
    }

    fn raster_2d(&self, ops: &[Canvas2DOp], width: u32, height: u32, _params: &RenderParams) -> Rgba8 {
        let (w, h) = (width as usize, height as usize);
        let mut data = vec![0u8; w * h * 4];
        // Current fill style; the 2D default is opaque black.
        let mut fill = [0u8, 0u8, 0u8, 255u8];
        for op in ops {
            match *op {
                Canvas2DOp::SetFillStyle { color } => fill = color.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8),
                Canvas2DOp::FillRect { x, y, w: rw, h: rh } => {
                    blend_rect(&mut data, w, h, x, y, rw, rh, fill)
                }
                Canvas2DOp::ClearRect { x, y, w: rw, h: rh } => {
                    set_rect(&mut data, w, h, x, y, rw, rh, [0, 0, 0, 0])
                }
                // SolidBackend is the pure-Rust rect-only reference; path/text ops are Skia's job.
                _ => {}
            }
        }
        Rgba8 { width, height, data }
    }
}

/// Clamp a float rect to integer pixel bounds `[x0,x1) x [y0,y1)` within the buffer.
fn rect_bounds(w: usize, h: usize, x: f64, y: f64, rw: f64, rh: f64) -> (usize, usize, usize, usize) {
    let x0 = (x.floor().max(0.0) as usize).min(w);
    let y0 = (y.floor().max(0.0) as usize).min(h);
    let x1 = ((x + rw).ceil().max(0.0) as usize).min(w);
    let y1 = ((y + rh).ceil().max(0.0) as usize).min(h);
    (x0, y0, x1.max(x0), y1.max(y0))
}

/// Overwrite a region with `px` (used by clearRect → transparent).
fn set_rect(data: &mut [u8], w: usize, h: usize, x: f64, y: f64, rw: f64, rh: f64, px: [u8; 4]) {
    let (x0, y0, x1, y1) = rect_bounds(w, h, x, y, rw, rh);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let i = (yy * w + xx) * 4;
            data[i..i + 4].copy_from_slice(&px);
        }
    }
}

/// Source-over alpha composite `px` onto a region.
fn blend_rect(data: &mut [u8], w: usize, h: usize, x: f64, y: f64, rw: f64, rh: f64, px: [u8; 4]) {
    let (x0, y0, x1, y1) = rect_bounds(w, h, x, y, rw, rh);
    let sa = px[3] as f32 / 255.0;
    for yy in y0..y1 {
        for xx in x0..x1 {
            let i = (yy * w + xx) * 4;
            for c in 0..3 {
                let dst = data[i + c] as f32 / 255.0;
                let src = px[c] as f32 / 255.0;
                data[i + c] = ((src * sa + dst * (1.0 - sa)) * 255.0).round() as u8;
            }
            let da = data[i + 3] as f32 / 255.0;
            data[i + 3] = ((sa + da * (1.0 - sa)) * 255.0).round().min(255.0) as u8;
        }
    }
}

// `mk_font`, `draw_text_fallback` and `composite_to_blend` moved to `canvas2d.rs` with the rest of
// the 2D interpreter when `Canvas2DOp` grew from 14 variants to the full surface; they are the same
// functions, unchanged apart from `draw_text_fallback` learning to measure as well as draw.

/// R2 backend: **real Skia** (Chrome's rasterizer), bound to the local CPU/CoreText font stack — the
/// bit-identical target. R2.a implements the same rect subset as `SolidBackend` but through Skia's
/// pipeline (AA, source-over, unpremultiplied readback matching `getImageData`); R2.b grows it to the
/// full probe scene (text via CoreText, gradients, arcs, compositing) and calibrates against the Brave
/// reference. A CPU raster surface keeps output deterministic; the GPU path is an R3/later decision.
#[derive(Debug, Default, Clone, Copy)]
pub struct SkiaBackend;

/// A canvas's raster surface with the text properties Chromium gives one on Linux: Skia built with
/// `SK_GAMMA_EXPONENT=1.2` / `SK_GAMMA_CONTRAST=0.2` (`skia/BUILD.gn`), passed per surface through
/// `LegacyDisplayGlobals::ComputeSurfaceProps`, with an unknown pixel geometry (no LCD text) unless the canvas is
/// opaque.
/// Skia's own defaults blit antialiased glyph edges lighter: a 20px stem read 233/175 where Chrome
/// draws 250/214 (against software-rasterized Chrome 150).
///
/// The backing is Blink's (`Canvas2DColorParams`): N32 or half floats, premultiplied (opaque for
/// `alpha: false`, 2026-10-10), TAGGED
/// with the context's colour space -- Skia takes every sRGB paint colour into it (`color(display-p3 1 0.25 0.5)`
/// stores `[255, 27, 127]` in an sRGB canvas and `[255, 64, 127]` in a display-p3 one, stock Chrome 150).
fn canvas_surface(width: i32, height: i32, params: &RenderParams) -> Option<skia_safe::Surface> {
    // An opaque canvas can draw LCD text: `ComputeSurfaceProps(can_use_lcd_text)` gives it the display's subpixel
    // layout, `LegacyDisplayGlobals`' horizontal RGB on Linux; any other canvas gets none. So can the document's
    // root layer: every page's text is LCD in Chrome's frames and screenshots.
    let lcd = params.opaque || params.document;
    let geometry = if lcd { skia_safe::PixelGeometry::RGBH } else { skia_safe::PixelGeometry::Unknown };
    let props = skia_safe::SurfaceProps::new_with_text_properties(skia_safe::SurfacePropsFlags::empty(), geometry, 0.2, 1.2);
    // A float16 backing is kept in floats: skia-safe's raster pipeline stores halves by truncating, where Chrome's
    // (F16C) store rounds to nearest -- `read_backing` rounds them as Chrome's backing would hold them.
    let ct = if params.float16 { ColorType::RGBAF32 } else { ColorType::N32 };
    let at = if lcd { AlphaType::Opaque } else { AlphaType::Premul };
    let info = ImageInfo::new((width, height), ct, at, Some(color_space(params.color_space)));
    surfaces::raster(&info, None, Some(&props))
}

/// What a canvas's backing holds before anything is drawn: transparent black, or opaque black for an opaque one
/// (Blink clears an `alpha: false` canvas's resource provider to `SkColors::kBlack`); the document's, Chrome's base
/// background, white.
fn initial_color(params: &RenderParams) -> Color {
    if params.document {
        Color::WHITE
    } else if params.opaque {
        Color::BLACK
    } else {
        Color::TRANSPARENT
    }
}

/// Blink's `PredefinedColorSpaceToSkColorSpace`: `SkColorSpace::MakeSRGB()`, or `MakeRGB(sRGB transfer function,
/// SkNamedGamut::kDisplayP3)` for display-p3 (gfx's slightly different sRGB curve snaps to Skia's own). skia-safe
/// binds no `MakeRGB`; `SkColorSpace::Deserialize` is `MakeRGB` over a serialized transfer function and matrix,
/// so display-p3 is built through it.
fn color_space(space: spec::CanvasColorSpace) -> skia_safe::ColorSpace {
    match space {
        spec::CanvasColorSpace::Srgb => skia_safe::ColorSpace::new_srgb(),
        spec::CanvasColorSpace::DisplayP3 => {
            static P3: std::sync::OnceLock<skia_safe::ColorSpace> = std::sync::OnceLock::new();
            P3.get_or_init(|| {
                const TF: [f32; 7] = [2.4, (1.0 / 1.055) as f32, (0.055 / 1.055) as f32, (1.0 / 12.92) as f32, 0.04045, 0.0, 0.0];
                const GAMUT: [f32; 9] = [0.515102, 0.291965, 0.157153, 0.241182, 0.692236, 0.0665819, -0.00104941, 0.0418818, 0.784378];
                // `ColorSpaceHeader { fVersion = 1 }`, then the seven transfer-function floats and the nine of the matrix.
                let mut b = vec![1u8, 0, 0, 0];
                for v in TF.iter().chain(GAMUT.iter()) {
                    b.extend_from_slice(&v.to_ne_bytes());
                }
                skia_safe::ColorSpace::deserialize(skia_safe::Data::new_copy(&b))
            })
            .clone()
        }
    }
}

/// Read the backing into `info`, from `src`, as Blink reads its snapshot (`readPixels`): Skia converts into the
/// requested colour type, alpha type and colour space. A float16 backing's floats are first rounded to the halves
/// Chrome's backing holds (stock Chrome 150, software raster: `#123456` on a float16 canvas reads 0.0706177, the
/// half nearest 18/255; a truncating store gives 0.0705566).
fn read_backing(surface: &mut skia_safe::Surface, params: &RenderParams, info: &ImageInfo, data: &mut [u8], row_bytes: usize, src: (i32, i32)) -> bool {
    let ok = read_backing_pixels(surface, params, info, data, row_bytes, src);
    if ok && params.opaque {
        force_opaque(info, data, row_bytes);
    }
    ok
}

/// An OPAQUE canvas reads back opaque whatever its pixels' alpha bytes hold: `clearRect` or a `copy` of a
/// translucent colour leaves alpha below 1 in the backing, which Blink's opaque snapshot ignores (stock Chrome 150,
/// 2026-10-10: a cleared pixel reads `[0, 0, 0, 255]`, a `copy` of `rgba(0,0,255,0.25)` `[0, 0, 64, 255]`). Skia
/// keeps the colour channels as they are stored and passes the alpha byte through, so only the alpha is set.
fn force_opaque(info: &ImageInfo, data: &mut [u8], row_bytes: usize) {
    let (w, h) = (info.width().max(0) as usize, info.height().max(0) as usize);
    match info.color_type() {
        ColorType::RGBA8888 | ColorType::BGRA8888 => {
            for y in 0..h {
                for x in 0..w {
                    if let Some(a) = data.get_mut(y * row_bytes + x * 4 + 3) {
                        *a = 255;
                    }
                }
            }
        }
        ColorType::RGBAF32 => {
            let one = 1.0f32.to_ne_bytes();
            for y in 0..h {
                for x in 0..w {
                    let at = y * row_bytes + x * 16 + 12;
                    if let Some(a) = data.get_mut(at..at + 4) {
                        a.copy_from_slice(&one);
                    }
                }
            }
        }
        _ => {}
    }
}

fn read_backing_pixels(surface: &mut skia_safe::Surface, params: &RenderParams, info: &ImageInfo, data: &mut [u8], row_bytes: usize, src: (i32, i32)) -> bool {
    if !params.float16 {
        return surface.read_pixels(info, data, row_bytes, src);
    }
    let (w, h) = (surface.width(), surface.height());
    let cs = color_space(params.color_space);
    let floats = ImageInfo::new((w, h), ColorType::RGBAF32, AlphaType::Premul, Some(cs.clone()));
    let mut f = vec![0u8; (w as usize) * (h as usize) * 16];
    if !surface.read_pixels(&floats, &mut f, (w as usize) * 16, (0, 0)) {
        return false;
    }
    // The halves stay in floats: Skia's portable half LOAD flushes subnormal halves to zero, which Chrome's F16C
    // load does not (`color(srgb 1 0.000001 0.5)` keeps its 1.01e-6 on a float16 canvas).
    for px in f.chunks_exact_mut(4) {
        let v = f32::from_le_bytes([px[0], px[1], px[2], px[3]]);
        px.copy_from_slice(&f16_to_f32(f32_to_f16(v)).to_le_bytes());
    }
    let Some(img) = skia_safe::images::raster_from_data(&floats, skia_safe::Data::new_copy(&f), (w as usize) * 16) else {
        return false;
    };
    img.read_pixels(info, data, row_bytes, src, skia_safe::image::CachingHint::Disallow)
}

/// Little-endian IEEE halves of `f32`s, rounded to nearest even -- what Skia's `store_f16` writes on a CPU with
/// F16C (`_mm256_cvtps_ph`), which Chrome's raster pipeline runs on any x86-64 desktop.
fn halves(floats: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(floats.len() / 2);
    for b in floats.chunks_exact(4) {
        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        out.extend_from_slice(&f32_to_f16(v).to_le_bytes());
    }
    out
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let (mut e, mut m) = (127 - 15 + 1, man);
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

fn f32_to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let rest = m & ((1u32 << shift) - 1);
        let mut h = (m >> shift) as u16;
        if rest > half || (rest == half && h & 1 == 1) {
            h += 1;
        }
        return sign | h;
    }
    let mut h = ((e as u32) << 10 | (man >> 13)) as u16;
    let rest = man & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    sign | h
}

impl RenderBackend for SkiaBackend {
    fn name(&self) -> &str {
        "skia"
    }

    fn raster_2d(&self, ops: &[Canvas2DOp], width: u32, height: u32, params: &RenderParams) -> Rgba8 {
        if width == 0 || height == 0 {
            return Rgba8::transparent(width, height);
        }
        let mut surface = match canvas_surface(width as i32, height as i32, params) {
            Some(s) => s,
            None => return Rgba8::transparent(width, height),
        };
        {
            let canvas = surface.canvas();
            canvas.clear(initial_color(params));
            canvas2d::execute(canvas, ops);
        }
        // Read back as unpremultiplied RGBA8888 in the canvas's own space — the layout getImageData exposes.
        let info = ImageInfo::new((width as i32, height as i32), ColorType::RGBA8888, AlphaType::Unpremul, Some(color_space(params.color_space)));
        let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
        let row_bytes = (width as usize) * 4;
        read_backing(&mut surface, params, &info, &mut data, row_bytes, (0, 0));
        Rgba8 { width, height, data }
    }

    /// Raster only the `(rx, ry, rw, rh)` sub-region. A `rw`×`rh` Skia surface is
    /// translated by `(-rx, -ry)` so the display list — run in full, in absolute page coordinates, so
    /// its state is correct — paints the region at the surface origin; Skia clips everything else. The
    /// pixels are bit-identical to the same region of a full `raster_2d` (same ops, same ordering, same
    /// unpremultiplied readback). `width`/`height` are not needed on this path (absolute coords +
    /// surface-clip); they stay in the signature for the full-raster-crop default on other backends.
    fn raster_2d_region(&self, ops: &[Canvas2DOp], _width: u32, _height: u32, rx: u32, ry: u32, rw: u32, rh: u32, params: &RenderParams) -> Rgba8 {
        if rw == 0 || rh == 0 {
            return Rgba8::transparent(rw, rh);
        }
        let mut surface = match canvas_surface(rw as i32, rh as i32, params) {
            Some(s) => s,
            None => return Rgba8::transparent(rw, rh),
        };
        {
            let canvas = surface.canvas();
            canvas.clear(initial_color(params));
            canvas.translate((-(rx as f32), -(ry as f32)));
            canvas2d::execute(canvas, ops);
        }
        let info = ImageInfo::new((rw as i32, rh as i32), ColorType::RGBA8888, AlphaType::Unpremul, Some(color_space(params.color_space)));
        let mut data = vec![0u8; (rw as usize) * (rh as usize) * 4];
        let row_bytes = (rw as usize) * 4;
        read_backing(&mut surface, params, &info, &mut data, row_bytes, (0, 0));
        Rgba8 { width: rw, height: rh, data }
    }

    /// `getImageData` with an `ImageDataSettings`: Skia's `readPixels` from the tagged backing into the requested
    /// space and format, as Blink reads into `ImageData::GetSkPixmap()`. A float16 read is Skia's float read rounded
    /// to halves as its F16C store would (see [`halves`]).
    fn read_2d(&self, ops: &[Canvas2DOp], width: u32, height: u32, sx: i32, sy: i32, sw: u32, sh: u32, params: &RenderParams, space: spec::CanvasColorSpace, format: spec::ReadFormat) -> Option<Vec<u8>> {
        let (ct, bpp) = match format {
            spec::ReadFormat::Unorm8 => (ColorType::RGBA8888, 4usize),
            spec::ReadFormat::Float16 | spec::ReadFormat::Float32 => (ColorType::RGBAF32, 16usize),
        };
        let mut data = vec![0u8; (sw as usize) * (sh as usize) * bpp];
        if width > 0 && height > 0 && sw > 0 && sh > 0 {
            let mut surface = canvas_surface(width as i32, height as i32, params)?;
            {
                let canvas = surface.canvas();
                canvas.clear(initial_color(params));
                canvas2d::execute(canvas, ops);
            }
            let info = ImageInfo::new((sw as i32, sh as i32), ct, AlphaType::Unpremul, Some(color_space(space)));
            read_backing(&mut surface, params, &info, &mut data, (sw as usize) * bpp, (sx, sy));
        }
        Some(if format == spec::ReadFormat::Float16 { halves(&data) } else { data })
    }

    /// `isPointInPath` / `isPointInStroke`, answered by the same curve flattener that draws.
    fn hit_test_2d(&self, ops: &[Canvas2DOp], x: f64, y: f64, what: &HitTest, _params: &RenderParams) -> bool {
        canvas2d::hit_test(ops, x, y, what)
    }

    /// R3: execute a recorded GL command list through the software ES 2.0 pipeline in [`gl`].
    ///
    /// Not Skia, and not a GL driver -- see `gl`'s module comment for why a software pipeline is the
    /// honest choice on a host whose persona claims a GPU it does not have. What matters at this seam
    /// is that the buffer DEPENDS ON THE COMMANDS: `readPixels` copies a region out of what this
    /// returns and `toDataURL` encodes the whole of it, so the two agree by construction.
    fn raster_gl(
        &self,
        cmds: &[GlOp],
        width: u32,
        height: u32,
        attrs: &GlContextAttributes,
        _params: &RenderParams,
        list: Option<u64>,
    ) -> Rgba8 {
        if width == 0 || height == 0 {
            return Rgba8::transparent(width, height);
        }
        // On this PC's GPU, as Chrome draws it, when it has one
        if let Some(px) = list.and_then(|l| gpu::raster(l, cmds, width, height, attrs)) {
            return px;
        }
        gl::raster(cmds, width, height, attrs)
    }

    /// R3: the synchronous GL questions -- compile/link status, info logs, locations, `getError`.
    /// Answered by the same machine that draws, for the same reason `hit_test_2d` is.
    fn query_gl(
        &self,
        cmds: &[GlOp],
        q: &GlQuery,
        width: u32,
        height: u32,
        attrs: &GlContextAttributes,
        _params: &RenderParams,
        list: Option<u64>,
    ) -> GlQueryResult {
        if let Some(r) = list.and_then(|l| gpu::query(l, cmds, q, width, height, attrs)) {
            return r;
        }
        gl::query_list(cmds, q, width, height, attrs, list)
    }

    /// This PC's GPU through Chrome's own ANGLE ([`gpu`]), or none -- as Chrome 150 on Linux has
    /// no WebGL without one.
    fn gl_device(&self) -> spec::GlDevice {
        match gpu::webgl_block() {
            Some(block) => spec::GlDevice::Present(block),
            None => spec::GlDevice::Absent,
        }
    }

    fn release_gl(&self, list: u64) {
        gpu::release(list);
    }

    fn prepare_gl(&self) {
        gpu::warm();
    }

    /// R3.5: execute a recorded Web Audio graph through the DSP in [`audio`].
    ///
    /// Not a hardware audio path and not a DSP crate — see `audio`'s module comment. What matters at
    /// this seam is the same thing that matters at `raster_gl`'s: the samples DEPEND ON THE GRAPH.
    /// `AudioBuffer.getChannelData` reads what this returns, and `AnalyserNode`'s four readbacks come
    /// out of `query_audio`'s run of the same list, so the two cannot describe different graphs.
    fn render_audio(
        &self,
        graph: &[AudioOp],
        frames: u32,
        channels: u32,
        sample_rate: f32,
        _params: &RenderParams,
    ) -> Vec<f32> {
        audio::render(graph, frames, channels, sample_rate)
    }

    fn resample_audio(&self, samples: &[f32], from_rate: f64, to_rate: f64) -> Option<Vec<f32>> {
        audio::resample(samples, from_rate, to_rate)
    }

    fn query_audio(
        &self,
        graph: &[AudioOp],
        query: &AudioQuery,
        frames: u32,
        channels: u32,
        sample_rate: f32,
        _params: &RenderParams,
    ) -> AudioQueryResult {
        audio::query(graph, query, frames, channels, sample_rate)
    }

    /// Encode with the encoders Chrome encodes with.
    ///
    /// `toDataURL` returns a string a page hashes, so the ENCODER is part of the fingerprint, not an
    /// implementation detail: identical pixels through two different PNG writers give two different
    /// base64 strings, because filter selection and the deflate stream differ.
    ///
    /// PNG is BYTE-IDENTICAL to Chrome 150's. Chrome encodes with `SkPngRustEncoder` at
    /// `CompressionLevel::kLow` (`blink/renderer/platform/image-encoders/image_encoder.cc`), which is
    /// the `png` crate; skia-safe 0.99's prebuilt does not ship that encoder, and this used to fall back
    /// to `SkPngEncoder` (libpng) -- same pixels, but an `sBIT` chunk, a level-6 zlib stream with
    /// adaptive filters and one IDAT, where Chrome and Brave 151 write zlib `78 01`, the Up filter on
    /// every row, 4096-byte IDATs and a 6-byte final one. A canvas hash told the two apart with every
    /// pixel equal. Measured 2026-10-01 against stock Chrome 150 headed: `png` 0.18.1, deflate level 1,
    /// `Filter::Up`, through the stream writer, gives Chrome's exact bytes for 64x16 (noise, text),
    /// 300x300 noise, 517x389 and 1200x700 alpha gradients -- and costs a third of libpng's time.
    ///
    /// JPEG follows Blink's parameters exactly, since those are ours to match: quality 92 by default,
    /// `quality * 100` rounded when the page passes one, and 4:4:4 chroma at quality 100.
    ///
    /// WebP is Skia's `SkWebpEncoder` over the libwebp Chrome 150 links (PHASE 3 F7; skia-safe's
    /// `webp-encode` over the from-source Skia), with Blink's `ComputeWebpOptions`: lossy at quality 80
    /// by default or `quality * 100`, LOSSLESS (at 75) for a quality of exactly 1. It returned `None`
    /// before, and `toDataURL('image/webp')` handed back a PNG -- a bot-detection service's a sensor cookie reads the type.
    ///
    /// A snapshot that carries a colour space (`encode_image_tagged`'s `srgb`) embeds its ICC profile in
    /// a JPEG (APP2) and a WebP (ICCP, VP8X) as Skia's encoders do; PNG through `SkPngRustEncoder` never
    /// does.
    fn text_ink(&self, font: &spec::TextFontRequest, text: &str) -> Option<spec::TextInk> {
        thread_local! {
            static MGR: skia_safe::FontMgr = skia_safe::FontMgr::new();
        }
        MGR.with(|mgr| text::text_ink(mgr, font, text))
    }

    fn local_font(&self, name: &str) -> Option<spec::LocalFont> {
        local_font::local_font(name)
    }

    fn web_font(&self, url: &str, body: &[u8]) -> Option<String> {
        web_fonts::register(url, body)
    }

    fn web_font_metrics(&self, family: &str) -> Option<spec::FontFileMetrics> {
        web_fonts::metrics(family)
    }

    fn encode_image(&self, buf: &Rgba8, format: ImageFormat, quality: Option<f64>) -> Option<Vec<u8>> {
        self.encode_image_tagged(buf, format, quality, false)
    }

    /// Skia's PDF backend, as Chrome prints: each page a translated, clipped run of the one display list -- shapes as
    /// paths, text as glyphs of embedded fonts, images as images -- under the creator and producer Chrome writes.
    fn print_pdf(&self, ops: &[Canvas2DOp], width: u32, _height: u32, layout: &spec::PdfLayout) -> Option<Vec<u8>> {
        use skia_safe::{pdf, Rect};
        let mut out: Vec<u8> = Vec::new();
        {
            let md = pdf::Metadata { title: layout.title.clone(), creator: "Chromium".into(), ..Default::default() };
            let mut doc = pdf::new_document(&mut out, Some(&md));
            let [top, right, bottom, left] = layout.margins;
            let content_w = (layout.paper_width - left - right).max(1.0);
            let content_h = (layout.paper_height - top - bottom).max(1.0);
            for &page in &layout.pages {
                let mut on = doc.begin_page((layout.paper_width, layout.paper_height), None);
                {
                    let canvas = on.canvas();
                    canvas.save();
                    canvas.translate((left, top));
                    canvas.clip_rect(Rect::from_wh(content_w, content_h), None, None);
                    canvas.scale((layout.points_per_px, layout.points_per_px));
                    canvas.translate((0.0, -(page as f32) * layout.page_height_px));
                    canvas.clip_rect(Rect::from_xywh(0.0, page as f32 * layout.page_height_px, width as f32, layout.page_height_px), None, None);
                    canvas2d::execute(canvas, ops);
                    canvas.restore();
                }
                doc = on.end_page();
            }
            doc.close();
        }
        (!out.is_empty()).then_some(out)
    }

    fn encode_image_tagged(&self, buf: &Rgba8, format: ImageFormat, quality: Option<f64>, srgb: bool) -> Option<Vec<u8>> {
        if buf.width == 0 || buf.height == 0 {
            return None;
        }
        if matches!(format, ImageFormat::Png) {
            return encode_png_like_chrome(buf);
        }
        // `Pixmap::new` borrows mutably, and the caller's buffer is shared — copy first. The buffer is
        // UNPREMULTIPLIED (the layout `getImageData` exposes), which is also what PNG stores, so the
        // ImageInfo has to say so or Skia converts and the pixels shift. Its colour space is the
        // snapshot's: sRGB for a 2D context's resource provider, none for Blink's transparent image.
        let mut data = buf.data.clone();
        let info = ImageInfo::new(
            (buf.width as i32, buf.height as i32),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            if srgb { Some(skia_safe::ColorSpace::new_srgb()) } else { None },
        );
        let pixmap = Pixmap::new(&info, &mut data, (buf.width as usize) * 4)?;
        match format {
            ImageFormat::Png => encode_png_like_chrome(buf),
            ImageFormat::Jpeg => {
                // Blink's ComputeJpegQuality: 92 unless the page passed a number in [0,1].
                let q = match quality {
                    Some(q) if (0.0..=1.0).contains(&q) => (q * 100.0 + 0.5) as u32,
                    _ => 92,
                };
                let mut out = Vec::new();
                let opts = jpeg_encoder::Options {
                    quality: q,
                    // Blink: kBlendOnBlack, and k444 (no chroma downsampling) only at quality 100.
                    downsample: if q == 100 {
                        jpeg_encoder::Downsample::No
                    } else {
                        jpeg_encoder::Downsample::BothDirections
                    },
                    alpha_option: jpeg_encoder::AlphaOption::BlendOnBlack,
                    ..Default::default()
                };
                jpeg_encoder::encode(&pixmap, &mut out, &opts).then_some(out)
            }
            ImageFormat::Webp => {
                // Blink's ComputeWebpOptions: exactly 1 is lossless at 75; a number in [0,1] is
                // `quality * 100.0f`; anything else the default 80.
                let opts = match quality {
                    Some(q) if q == 1.0 => {
                        skia_safe::webp_encoder::Options { compression: skia_safe::webp_encoder::Compression::Lossless, quality: 75.0 }
                    }
                    Some(q) if (0.0..=1.0).contains(&q) => {
                        skia_safe::webp_encoder::Options { compression: skia_safe::webp_encoder::Compression::Lossy, quality: (q * 100.0) as f32 }
                    }
                    _ => skia_safe::webp_encoder::Options { compression: skia_safe::webp_encoder::Compression::Lossy, quality: 80.0 },
                };
                let mut out = Vec::new();
                skia_safe::webp_encoder::encode(&pixmap, &mut out, &opts).then_some(out)
            }
        }
    }

    /// A PNG's frame by Chrome's decoder, partial when its decode fails (see `png_decode::frame`); any other
    /// format's by Skia's codec, complete or nothing.
    fn decode_frame(&self, bytes: &[u8]) -> Option<(Rgba8, bool)> {
        if png_decode::is_png(bytes) {
            return png_decode::frame(bytes);
        }
        self.decode_image(bytes).map(|i| (i, true))
    }

    /// The size an image's header gives: a PNG's by Chrome's decoder, so an image whose
    /// pixel data does not decode still has one, as it does in Blink; any other format's by Skia's codec.
    fn image_size(&self, bytes: &[u8]) -> Option<(u32, u32)> {
        if png_decode::is_png(bytes) {
            return png_decode::header(bytes);
        }
        if modern_decode::is_modern(bytes) {
            return modern_decode::decode(bytes).map(|i| (i.width, i.height));
        }
        let codec = skia_safe::Codec::from_data(skia_safe::Data::new_copy(bytes))?;
        let d = codec.dimensions();
        if d.width <= 0 || d.height <= 0 {
            return None;
        }
        // the size the decode answers (`decode_image`: a JPEG turned by its EXIF orientation, as Blink's
        // `image-orientation: from-image` shows it)
        Some(if codec.origin().swaps_width_height() { (d.height as u32, d.width as u32) } else { (d.width as u32, d.height as u32) })
    }

    /// Skia's own codecs — the ones the prebuilt carries: PNG (libpng), JPEG
    /// (libjpeg-turbo), GIF (first frame), BMP, ICO and WBMP. WebP DECODE is off in this build for the
    /// same reason WebP encode is (above), and AVIF and SVG are not Skia codecs at all, so those answer
    /// `None` and the caller's decode-failure path runs. Decoded straight to unpremultiplied RGBA8 --
    /// the layout `getImageData` exposes -- in the image's own colour space, as Blink does for an image
    /// without an embedded profile.
    fn video_decoder(&self, codec: &spec::VideoCodec) -> Option<Box<dyn spec::VideoDecoder>> {
        video_decode::decoder(codec)
    }

    fn decode_image(&self, bytes: &[u8]) -> Option<Rgba8> {
        // PNG by Chrome's own decoder, not the prebuilt's libpng (see `png_decode`).
        if png_decode::is_png(bytes) {
            return png_decode::decode(bytes);
        }
        // WebP and AVIF, which Skia here does not decode (`modern_decode`)
        if modern_decode::is_modern(bytes) {
            return modern_decode::decode(bytes);
        }
        let image = skia_safe::Image::from_encoded(skia_safe::Data::new_copy(bytes))?;
        let (w, h) = (image.width(), image.height());
        if w <= 0 || h <= 0 {
            return None;
        }
        let info = ImageInfo::new((w, h), ColorType::RGBA8888, AlphaType::Unpremul, None);
        let mut data = vec![0u8; (w as usize) * (h as usize) * 4];
        image
            .read_pixels(&info, &mut data, (w as usize) * 4, (0, 0), skia_safe::image::CachingHint::Disallow)
            .then_some(Rgba8 { width: w as u32, height: h as u32, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::Canvas2DOp::{FillRect, SetFillStyle};
    use spec::{CanvasFilter, TextAlign, TextBaseline, Transform2D};

    /// A legacy colour as Blink paints it (`rgba(r, g, b, a / 255)`).
    fn rgba(r: u8, g: u8, b: u8, a: u8) -> spec::Color4f {
        spec::legacy_color4(r, g, b, a as f32 / 255.0)
    }

    #[test]
    fn solid_backend_fills_a_red_rect() {
        let ops = vec![
            SetFillStyle { color: rgba(255, 0, 0, 255) },
            FillRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ];
        let img = SolidBackend.raster_2d(&ops, 20, 20, &RenderParams::default());
        // (5,5) is inside the rect → opaque red; (15,15) is outside → transparent.
        let inside = (5 * 20 + 5) * 4;
        assert_eq!(&img.data[inside..inside + 4], &[255, 0, 0, 255]);
        let outside = (15 * 20 + 15) * 4;
        assert_eq!(&img.data[outside..outside + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn solid_backend_blends_semi_transparent_fill() {
        // Red opaque under, then 50% blue over → source-over blue≈128 over red.
        let ops = vec![
            SetFillStyle { color: rgba(255, 0, 0, 255) },
            FillRect { x: 0.0, y: 0.0, w: 4.0, h: 4.0 },
            SetFillStyle { color: rgba(0, 0, 255, 128) },
            FillRect { x: 0.0, y: 0.0, w: 4.0, h: 4.0 },
        ];
        let img = SolidBackend.raster_2d(&ops, 4, 4, &RenderParams::default());
        let p = &img.data[0..4];
        assert!(p[0] < 130 && p[0] > 120, "red channel blended: {}", p[0]);
        assert!(p[2] > 120, "blue channel present: {}", p[2]);
        assert_eq!(p[3], 255, "stays opaque");
    }

    #[test]
    fn null_backend_is_transparent() {
        let img = NullBackend.raster_2d(&[], 4, 4, &RenderParams::default());
        assert_eq!(img.width, 4);
        assert!(img.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn skia_backend_fills_a_red_rect() {
        // Real Skia raster: interior of a 10x10 rect is exact opaque red (AA only touches edges);
        // outside is transparent. Proves the skia-safe pipeline + unpremultiplied readback.
        let ops = vec![
            SetFillStyle { color: rgba(255, 0, 0, 255) },
            FillRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ];
        let img = SkiaBackend.raster_2d(&ops, 20, 20, &RenderParams::default());
        let inside = (5 * 20 + 5) * 4;
        assert_eq!(&img.data[inside..inside + 4], &[255, 0, 0, 255], "interior is opaque red");
        let outside = (15 * 20 + 15) * 4;
        assert_eq!(img.data[outside + 3], 0, "outside is transparent");
    }

    // ── the 3.1 vocabulary. One test per operation the brief measured as painting nothing or
    // ignoring what it was told; each reads the pixel the browser reads, at 50x50 like the probe. ──

    /// The pixel at (x, y) of a 50x50 raster of `ops`.
    fn at(ops: &[Canvas2DOp], x: usize, y: usize) -> [u8; 4] {
        let img = SkiaBackend.raster_2d(ops, 50, 50, &RenderParams::default());
        let i = (y * 50 + x) * 4;
        [img.data[i], img.data[i + 1], img.data[i + 2], img.data[i + 3]]
    }

    #[test]
    fn stroke_paints() {
        let ops = vec![
            Canvas2DOp::SetStrokeStyle { color: rgba(0, 255, 0, 255) },
            Canvas2DOp::SetLineWidth { width: 10.0 },
            Canvas2DOp::BeginPath,
            Canvas2DOp::MoveTo { x: 0.0, y: 20.0 },
            Canvas2DOp::LineTo { x: 50.0, y: 20.0 },
            Canvas2DOp::Stroke,
        ];
        assert_eq!(at(&ops, 20, 20), [0, 255, 0, 255]);
        assert_eq!(at(&ops, 20, 45)[3], 0, "nothing outside the stroked band");
    }

    /// A rounded clip under a rotation is the rotated round rect -- here a circle -- not the rotated square with
    /// corners of some other radius: minea's spinning `border-radius: 500px` conic glow clipped as a square.
    #[test]
    fn a_rotated_round_clip_keeps_its_radii() {
        let (s, c) = 30f64.to_radians().sin_cos();
        let ops = vec![
            Canvas2DOp::Transform { t: Transform2D { a: c, b: s, c: -s, d: c, e: 25.0 - 25.0 * c + 25.0 * s, f: 25.0 - 25.0 * s - 25.0 * c } },
            Canvas2DOp::BeginPath,
            Canvas2DOp::RoundRect { x: 5.0, y: 5.0, w: 40.0, h: 40.0, radii: [(20.0, 20.0); 4] },
            Canvas2DOp::Clip { even_odd: false },
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        assert_eq!(at(&ops, 25, 25), [255, 0, 0, 255], "the centre is inside");
        // 24px from the centre toward a rotated corner: inside the rotated square, outside the circle
        let (dx, dy) = (24.0 * 75f64.to_radians().cos(), 24.0 * 75f64.to_radians().sin());
        assert_eq!(at(&ops, (25.0 + dx) as usize, (25.0 + dy) as usize)[3], 0, "past the circle is clipped");
    }

    /// A tile drawn from ops repeats as a pattern: a CSS gradient layer sized by `background-size`.
    #[test]
    fn a_tile_pattern_repeats_its_ops() {
        let tile = vec![
            Canvas2DOp::DefineConicGradient { id: 0, start_angle: -std::f64::consts::FRAC_PI_2, x: 5.0, y: 5.0 },
            Canvas2DOp::AddColorStop { id: 0, offset: 0.0, color: [1.0, 0.0, 0.0, 1.0], oklab: [1.0, 0.0, 0.0, 1.0], legacy: true },
            Canvas2DOp::AddColorStop { id: 0, offset: 0.25, color: [1.0, 0.0, 0.0, 1.0], oklab: [1.0, 0.0, 0.0, 1.0], legacy: true },
            Canvas2DOp::AddColorStop { id: 0, offset: 0.25, color: [0.0, 0.0, 1.0, 1.0], oklab: [0.0, 0.0, 1.0, 1.0], legacy: true },
            Canvas2DOp::AddColorStop { id: 0, offset: 1.0, color: [0.0, 0.0, 1.0, 1.0], oklab: [0.0, 0.0, 1.0, 1.0], legacy: true },
            Canvas2DOp::SetFillGradient { id: 0 },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ];
        let ops = vec![
            Canvas2DOp::DefineTilePattern { id: 7, w: 10.0, h: 10.0, ops: tile, repeat: spec::PatternRepeat::Repeat },
            Canvas2DOp::SetFillPattern { id: 7 },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        // the top-right quarter of each tile is red, the rest blue
        for (x, y, red) in [(7, 2, true), (2, 2, false), (17, 2, true), (12, 7, false), (27, 12, true), (22, 17, false)] {
            let p = at(&ops, x, y);
            assert_eq!(p[0] > 200, red, "({x}, {y}) {p:?}");
            assert_eq!(p[2] > 200, !red, "({x}, {y}) {p:?}");
        }
    }

    #[test]
    fn transform_moves_the_rect() {
        let ops = vec![
            Canvas2DOp::Transform { t: Transform2D::translate(20.0, 20.0) },
            Canvas2DOp::SetFillStyle { color: rgba(0, 0, 255, 255) },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ];
        assert_eq!(at(&ops, 25, 25), [0, 0, 255, 255], "the rect moved with the transform");
        assert_eq!(at(&ops, 5, 5)[3], 0, "and is no longer at the origin");
    }

    #[test]
    fn transform_scales_the_stroke_width() {
        // `lineWidth` is a user-space length: under scale(4,4) a width of 1 covers four device px.
        let ops = vec![
            Canvas2DOp::Transform { t: Transform2D::scale(4.0, 4.0) },
            Canvas2DOp::SetStrokeStyle { color: rgba(0, 0, 0, 255) },
            Canvas2DOp::SetLineWidth { width: 1.0 },
            Canvas2DOp::BeginPath,
            Canvas2DOp::MoveTo { x: 0.0, y: 5.0 },
            Canvas2DOp::LineTo { x: 12.0, y: 5.0 },
            Canvas2DOp::Stroke,
        ];
        // The band is y in [18, 22) after scaling; sample its middle and just outside it.
        assert_eq!(at(&ops, 20, 19)[3], 255);
        assert_eq!(at(&ops, 20, 21)[3], 255);
        assert_eq!(at(&ops, 20, 25)[3], 0);
    }

    #[test]
    fn global_alpha_halves_the_alpha() {
        let ops = vec![
            Canvas2DOp::SetGlobalAlpha { alpha: 0.5 },
            Canvas2DOp::SetFillStyle { color: rgba(0, 0, 0, 255) },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        assert_eq!(at(&ops, 20, 20), [0, 0, 0, 128]);
    }

    #[test]
    fn clip_stops_the_paint_escaping() {
        let ops = vec![
            Canvas2DOp::BeginPath,
            Canvas2DOp::Rect { x: 40.0, y: 40.0, w: 10.0, h: 10.0 },
            Canvas2DOp::Clip { even_odd: false },
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        assert_eq!(at(&ops, 20, 20)[3], 0, "outside the clip stays empty");
        assert_eq!(at(&ops, 45, 45), [255, 0, 0, 255], "inside it paints");
    }

    #[test]
    fn save_restore_pops_the_clip_and_the_transform() {
        let ops = vec![
            Canvas2DOp::Save,
            Canvas2DOp::BeginPath,
            Canvas2DOp::Rect { x: 40.0, y: 40.0, w: 10.0, h: 10.0 },
            Canvas2DOp::Clip { even_odd: false },
            Canvas2DOp::Transform { t: Transform2D::translate(20.0, 20.0) },
            Canvas2DOp::Restore,
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        assert_eq!(at(&ops, 20, 20), [255, 0, 0, 255], "the clip and the transform were popped");
    }

    #[test]
    fn bezier_and_ellipse_paint() {
        let bez = vec![
            Canvas2DOp::SetFillStyle { color: rgba(0, 137, 0, 255) },
            Canvas2DOp::BeginPath,
            Canvas2DOp::MoveTo { x: 0.0, y: 0.0 },
            Canvas2DOp::BezierCurveTo { cp1x: 0.0, cp1y: 50.0, cp2x: 50.0, cp2y: 50.0, x: 50.0, y: 0.0 },
            Canvas2DOp::ClosePath,
            Canvas2DOp::Fill { even_odd: false },
        ];
        assert_eq!(at(&bez, 20, 20), [0, 137, 0, 255]);

        let ell = vec![
            Canvas2DOp::SetFillStyle { color: rgba(136, 0, 136, 255) },
            Canvas2DOp::BeginPath,
            Canvas2DOp::Ellipse {
                x: 25.0, y: 25.0, rx: 20.0, ry: 10.0, rotation: 0.0,
                start: 0.0, end: std::f64::consts::TAU, ccw: false,
            },
            Canvas2DOp::Fill { even_odd: false },
        ];
        assert_eq!(at(&ell, 25, 25), [136, 0, 136, 255], "inside the ellipse");
        assert_eq!(at(&ell, 25, 40)[3], 0, "outside its minor axis");
    }

    #[test]
    fn gradient_paints_its_stops() {
        let ops = vec![
            Canvas2DOp::DefineLinearGradient { id: 1, x0: 0.0, y0: 0.0, x1: 50.0, y1: 0.0 },
            Canvas2DOp::AddColorStop { id: 1, offset: 0.0, color: rgba(255, 0, 0, 255), oklab: rgba(255, 0, 0, 255), legacy: true },
            Canvas2DOp::AddColorStop { id: 1, offset: 1.0, color: rgba(0, 0, 255, 255), oklab: rgba(0, 0, 255, 255), legacy: true },
            Canvas2DOp::SetFillGradient { id: 1 },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        let left = at(&ops, 1, 25);
        let right = at(&ops, 48, 25);
        assert!(left[0] > 200 && left[2] < 60, "left end is red: {left:?}");
        assert!(right[2] > 200 && right[0] < 60, "right end is blue: {right:?}");
    }

    #[test]
    fn a_gradient_with_no_stops_paints_nothing() {
        let ops = vec![
            Canvas2DOp::DefineLinearGradient { id: 1, x0: 0.0, y0: 0.0, x1: 50.0, y1: 0.0 },
            Canvas2DOp::SetFillGradient { id: 1 },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 },
        ];
        assert_eq!(at(&ops, 20, 20)[3], 0);
    }

    #[test]
    fn put_image_data_ignores_transform_clip_and_alpha() {
        let mut image = Rgba8::transparent(10, 10);
        for px in image.data.chunks_mut(4) {
            px.copy_from_slice(&[200, 0, 0, 255]);
        }
        let ops = vec![
            Canvas2DOp::Transform { t: Transform2D::translate(30.0, 30.0) },
            Canvas2DOp::SetGlobalAlpha { alpha: 0.1 },
            Canvas2DOp::BeginPath,
            Canvas2DOp::Rect { x: 40.0, y: 40.0, w: 5.0, h: 5.0 },
            Canvas2DOp::Clip { even_odd: false },
            Canvas2DOp::PutImageData {
                image,
                dx: 15.0, dy: 15.0,
                dirty_x: 0.0, dirty_y: 0.0, dirty_w: 10.0, dirty_h: 10.0,
            },
        ];
        assert_eq!(at(&ops, 20, 20), [200, 0, 0, 255], "written where asked, at full alpha");
    }

    #[test]
    fn draw_image_blits_a_buffer() {
        let mut image = Rgba8::transparent(10, 10);
        for px in image.data.chunks_mut(4) {
            px.copy_from_slice(&[0, 255, 0, 255]);
        }
        let ops = vec![Canvas2DOp::DrawImage {
            image, sx: 0.0, sy: 0.0, sw: 10.0, sh: 10.0, dx: 15.0, dy: 15.0, dw: 10.0, dh: 10.0,
        }];
        assert_eq!(at(&ops, 20, 20), [0, 255, 0, 255]);
        assert_eq!(at(&ops, 5, 5)[3], 0);
    }

    #[test]
    fn a_dash_leaves_gaps() {
        let ink = |dash: Vec<f64>| {
            let ops = vec![
                Canvas2DOp::SetStrokeStyle { color: rgba(0, 0, 0, 255) },
                Canvas2DOp::SetLineWidth { width: 6.0 },
                Canvas2DOp::SetLineDash { segments: dash },
                Canvas2DOp::BeginPath,
                Canvas2DOp::MoveTo { x: 0.0, y: 25.0 },
                Canvas2DOp::LineTo { x: 50.0, y: 25.0 },
                Canvas2DOp::Stroke,
            ];
            let img = SkiaBackend.raster_2d(&ops, 50, 50, &RenderParams::default());
            img.data.chunks(4).filter(|p| p[3] > 0).count()
        };
        let solid = ink(Vec::new());
        let dashed = ink(vec![5.0, 5.0]);
        assert!(solid > 250, "solid line inks the whole band: {solid}");
        assert!(dashed < solid * 3 / 5, "5-on-5-off inks about half: {dashed} vs {solid}");
    }

    #[test]
    fn shadow_paints_beside_the_shape() {
        let ops = vec![
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::SetShadowColor { color: rgba(0, 255, 0, 255) },
            Canvas2DOp::SetShadowOffset { x: 20.0, y: 20.0 },
            Canvas2DOp::FillRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ];
        assert_eq!(at(&ops, 5, 5), [255, 0, 0, 255], "the shape itself");
        assert_eq!(at(&ops, 25, 25), [0, 255, 0, 255], "its shadow, offset by 20");
    }

    #[test]
    fn hit_test_answers_the_same_geometry_it_draws() {
        let ops = vec![
            Canvas2DOp::BeginPath,
            Canvas2DOp::Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        ];
        let p = RenderParams::default();
        assert!(SkiaBackend.hit_test_2d(&ops, 5.0, 5.0, &HitTest::InPath { even_odd: false }, &p));
        assert!(!SkiaBackend.hit_test_2d(&ops, 20.0, 20.0, &HitTest::InPath { even_odd: false }, &p));

        let line = vec![
            Canvas2DOp::SetLineWidth { width: 6.0 },
            Canvas2DOp::BeginPath,
            Canvas2DOp::MoveTo { x: 0.0, y: 0.0 },
            Canvas2DOp::LineTo { x: 20.0, y: 0.0 },
        ];
        assert!(SkiaBackend.hit_test_2d(&line, 10.0, 0.0, &HitTest::InStroke, &p));
        assert!(!SkiaBackend.hit_test_2d(&line, 10.0, 40.0, &HitTest::InStroke, &p));
    }

    #[test]
    fn an_explicit_path_fills_like_the_current_one() {
        let path = vec![spec::PathOp::Rect { x: 0.0, y: 0.0, w: 40.0, h: 40.0 }];
        let ops = vec![
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::FillPath { path, even_odd: false },
        ];
        assert_eq!(at(&ops, 20, 20), [255, 0, 0, 255]);
        assert_eq!(at(&ops, 45, 45)[3], 0);
    }

    #[test]
    fn a_filter_blurs_past_the_shape() {
        let sharp = vec![
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::FillRect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 },
        ];
        let blurred = vec![
            Canvas2DOp::SetFilter { filters: vec![CanvasFilter::Blur { sigma: 2.0 }] },
            Canvas2DOp::SetFillStyle { color: rgba(255, 0, 0, 255) },
            Canvas2DOp::FillRect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 },
        ];
        assert_eq!(at(&sharp, 6, 20)[3], 0, "a sharp edge stops at the rect");
        assert!(at(&blurred, 6, 20)[3] > 0, "a blurred one bleeds past it");
    }

    #[test]
    fn text_align_and_baseline_move_the_run() {
        // Not a metric assertion (that would be a fact about this machine's fonts): only that
        // `textAlign`/`textBaseline` MOVE the ink, and in the direction they name.
        let ink_box = |align: TextAlign, baseline: TextBaseline| {
            let ops = vec![
                Canvas2DOp::SetFont {
                    family: "sans-serif".into(),
                    size_px: 20.0,
                    bold: false,
                    italic: false,
                    weight: 400,
                    families: String::new(),
                    lang: String::new(),
                    default_family: String::new(),
                },
                Canvas2DOp::SetTextAlign { align },
                Canvas2DOp::SetTextBaseline { baseline },
                Canvas2DOp::FillText { text: "III".into(), x: 25.0, y: 25.0, max_width: None },
            ];
            let img = SkiaBackend.raster_2d(&ops, 50, 50, &RenderParams::default());
            let (mut min_x, mut min_y) = (50usize, 50usize);
            for y in 0..50 {
                for x in 0..50 {
                    if img.data[(y * 50 + x) * 4 + 3] > 0 {
                        min_x = min_x.min(x);
                        min_y = min_y.min(y);
                    }
                }
            }
            (min_x, min_y)
        };
        let (left_x, alpha_y) = ink_box(TextAlign::Left, TextBaseline::Alphabetic);
        let (right_x, _) = ink_box(TextAlign::Right, TextBaseline::Alphabetic);
        let (_, top_y) = ink_box(TextAlign::Left, TextBaseline::Top);
        assert!(right_x < left_x, "right-aligned ink starts further left: {right_x} vs {left_x}");
        assert!(top_y > alpha_y, "`top` puts the run BELOW the y it was given: {top_y} vs {alpha_y}");
    }
}

// ── ABI 3 (wave 1): the C2 viewport seam ─────────────────────────────────────────────────────────

/// C2 (goal 1021): the viewport a page process presents, after any plugin override. A plugin may force
/// the inner size and device-pixel ratio with `host->set_viewport` (`plugin_host::viewport_override`);
/// when it has, that wins over the persona's own screen-derived size, so the engine lays out and rasters
/// the page at the forced size. `default` is what the persona gives -- `(width, height, dpr)`, inner
/// size in CSS pixels and the device pixel ratio -- and is returned unchanged until a plugin overrides.
///
/// The RASTER surface a plugin overrides is the DOCUMENT's, sized from the viewport: it is not a
/// `<canvas>` element's surface (that is sized by the element and must not move). SkiaBackend's own
/// `canvas_surface` is handed the size to raster at, so it needs no change here; the override is applied
/// where the page viewport is decided and the document display list is sized.
// TODO(1021-verify): the application site is korvus-webapi's `render_document_rgba`, where `w`/`h` come
// from the media context's viewport and the DPR is still hardcoded to 1.0 -- that crate (wave-1, not
// this one's to edit) reaches `plugin_host::viewport_override()` directly, so this helper is the
// render-side statement of the C2 contract for a surface that IS sized from the persona screen.
pub fn effective_viewport(default: (u32, u32, f64)) -> (u32, u32, f64) {
    plugin_host::viewport_override().unwrap_or(default)
}

// ── C6: the PNG capture, reachable for the screenshot seam ────────────────────────────────────────

/// Chrome 150's PNG for an unpremultiplied RGBA8 buffer, exposed for the C6 screenshot seam
/// (`plugin_host::Screenshotter` / `host->capture_png`) and the frame tap's [`frame::latest_png`]. The
/// same encoder `encode_image(_, ImageFormat::Png, _)` and `korvus fetch --screenshot` use, as a free
/// function so a caller that holds only an [`Rgba8`] (a tapped frame, a page render) can encode it
/// without a backend instance.
// TODO(1021-verify): the screenshotter itself is installed by korvus-runtime (it has the engine and
// `Engine::render_png`, which renders a FRESH page frame and encodes it through the active
// `RenderHandle`); this is the render-side encoder it -- or the frame tap -- reaches for.
pub fn encode_png(buf: &Rgba8) -> Option<Vec<u8>> {
    encode_png_like_chrome(buf)
}

/// Chrome 150's PNG for unpremultiplied RGBA8 pixels (see `encode_image`): 8-bit RGBA, deflate level 1,
/// the Up filter on every row, written through the stream writer.
fn encode_png_like_chrome(buf: &Rgba8) -> Option<Vec<u8>> {
    use std::io::Write;
    let need = (buf.width as usize).checked_mul(buf.height as usize)?.checked_mul(4)?;
    let pixels = buf.data.get(..need)?;
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, buf.width, buf.height);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.set_deflate_compression(png::DeflateCompression::Level(1));
        e.set_filter(png::Filter::Up);
        let mut w = e.write_header().ok()?;
        let mut s = w.stream_writer().ok()?;
        s.write_all(pixels).ok()?;
        s.finish().ok()?;
    }
    Some(out)
}
