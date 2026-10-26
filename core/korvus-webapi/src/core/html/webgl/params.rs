//! `getParameter` as Blink answers it, and the context state it reads.
//!
//! ## What was here before
//!
//! `getParameter` read the persona's recorded table and nothing else. The table is one capture of one
//! fresh 64x64 context, so every STATE parameter answered that context's initial value forever:
//! `enable(BLEND)` then `getParameter(BLEND)` was `false`, `viewport(1,2,30,40)` read back
//! `[0,0,64,64]` (the capture canvas, not this one), a bound buffer read `null`. The values that
//! survived the table's JSON came back in the wrong types (`DEPTH_RANGE` an `Int32Array`,
//! `COLOR_WRITEMASK` the string `"true,true,true,true"`), and a name the table lacked answered `null`
//! WITHOUT an error -- all 65,536 of them, where Chrome 150 accepts 89 (WebGL 1) or 151 (WebGL 2) and
//! raises INVALID_ENUM for the rest. a managed bot challenge reads 140 of these in its second
//! stage, after enabling extensions.
//!
//! ## What it is now
//!
//! [`Params`] is the state Blink's switch reads (`WebGLRenderingContextBase::getParameter` and
//! `WebGL2RenderingContextBase::getParameter`, Chromium 150), kept per context and changed by the SAME
//! ops the backend replays: every op goes through [`Params::check`] (the validation Blink and ANGLE
//! apply before a setter takes effect -- an invalid call records only its error, as Chrome's leaves
//! the state alone) and then [`Params::apply`]. So `getParameter`, `getError` and the pixels see one
//! history.
//!
//! [`classify`] is the switch itself: which names are valid for which context version, which read
//! state, which follow a drawing-buffer rule, and which are the DEVICE's (the limits and strings the
//! persona recorded), each in Blink's return type.

use spec::GlOp;
use std::collections::{HashMap, HashSet};

use super::WebGLState;

// ── enums ───────────────────────────────────────────────────────────────────────────────────────

pub(super) const INVALID_ENUM: u32 = 0x0500;
pub(super) const INVALID_VALUE: u32 = 0x0501;
pub(super) const INVALID_OPERATION: u32 = 0x0502;

const TEXTURE0: u32 = 0x84C0;
const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_CUBE_MAP: u32 = 0x8513;
const TEXTURE_3D: u32 = 0x806F;
const TEXTURE_2D_ARRAY: u32 = 0x8C1A;

const ARRAY_BUFFER: u32 = 0x8892;
const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const COPY_READ_BUFFER: u32 = 0x8F36;
const COPY_WRITE_BUFFER: u32 = 0x8F37;
const PIXEL_PACK_BUFFER: u32 = 0x88EB;
const PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
const TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;
const UNIFORM_BUFFER: u32 = 0x8A11;

const FRAMEBUFFER: u32 = 0x8D40;
const READ_FRAMEBUFFER: u32 = 0x8CA8;
const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const RENDERBUFFER: u32 = 0x8D41;

const FRONT: u32 = 0x0404;
const BACK: u32 = 0x0405;
const FRONT_AND_BACK: u32 = 0x0408;
const CW: u32 = 0x0900;
const CCW: u32 = 0x0901;
const NONE: u32 = 0;

const NEVER: u32 = 0x0200;
const LESS: u32 = 0x0201;
const ALWAYS: u32 = 0x0207;
const KEEP: u32 = 0x1E00;

const FUNC_ADD: u32 = 0x8006;
const MIN: u32 = 0x8007;
const MAX: u32 = 0x8008;
const FUNC_SUBTRACT: u32 = 0x800A;
const FUNC_REVERSE_SUBTRACT: u32 = 0x800B;
const SRC_ALPHA_SATURATE: u32 = 0x0308;
const CONSTANT_COLOR: u32 = 0x8001;
const ONE_MINUS_CONSTANT_COLOR: u32 = 0x8002;
const CONSTANT_ALPHA: u32 = 0x8003;
const ONE_MINUS_CONSTANT_ALPHA: u32 = 0x8004;

const DONT_CARE: u32 = 0x1100;
const FASTEST: u32 = 0x1101;
const NICEST: u32 = 0x1102;
const GENERATE_MIPMAP_HINT: u32 = 0x8192;
const FRAGMENT_SHADER_DERIVATIVE_HINT: u32 = 0x8B8B;

const PACK_ALIGNMENT: u32 = 0x0D05;
const UNPACK_ALIGNMENT: u32 = 0x0CF5;
const UNPACK_FLIP_Y_WEBGL: u32 = 0x9240;
const UNPACK_PREMULTIPLY_ALPHA_WEBGL: u32 = 0x9241;
const UNPACK_COLORSPACE_CONVERSION_WEBGL: u32 = 0x9243;
const BROWSER_DEFAULT_WEBGL: u32 = 0x9244;
/// The WebGL 2 pixel-store names, each a non-negative integer.
const PIXEL_STORE_V2: &[u32] = &[0x0D02, 0x0D03, 0x0D04, 0x0CF2, 0x0CF3, 0x0CF4, 0x806D, 0x806E];

// The capabilities `enable` takes.
const BLEND: u32 = 0x0BE2;
const CULL_FACE: u32 = 0x0B44;
const DEPTH_TEST: u32 = 0x0B71;
const DITHER: u32 = 0x0BD0;
const POLYGON_OFFSET_FILL: u32 = 0x8037;
const SAMPLE_ALPHA_TO_COVERAGE: u32 = 0x809E;
const SAMPLE_COVERAGE: u32 = 0x80A0;
const SCISSOR_TEST: u32 = 0x0C11;
const STENCIL_TEST: u32 = 0x0B90;
const RASTERIZER_DISCARD: u32 = 0x8C89;
const DEPTH_CLAMP_EXT: u32 = 0x864F;
const POLYGON_OFFSET_LINE_WEBGL: u32 = 0x2A02;
const BASE_CAPS: &[u32] = &[
    BLEND, CULL_FACE, DEPTH_TEST, DITHER, POLYGON_OFFSET_FILL, SAMPLE_ALPHA_TO_COVERAGE, SAMPLE_COVERAGE,
    SCISSOR_TEST, STENCIL_TEST,
];

/// `getParameter` names a WebGL 1 context answers with no extension enabled -- Blink's switch, and
/// exactly the 89 google-chrome 150.0.7871.46 accepts out of 0..65535 (measured 2026-10-08).
const V1_NAMES: &[u32] = &[
    2849, 2884, 2885, 2886, 2928, 2929, 2930, 2931, 2932, 2960, 2961, 2962, 2963, 2964, 2965, 2966,
    2967, 2968, 2978, 3024, 3042, 3088, 3089, 3106, 3107, 3317, 3333, 3379, 3386, 3408, 3410, 3411,
    3412, 3413, 3414, 3415, 7936, 7937, 7938, 10752, 32773, 32777, 32823, 32824, 32873, 32926, 32928,
    32936, 32937, 32938, 32939, 32968, 32969, 32970, 32971, 33170, 33901, 33902, 34016, 34024, 34068,
    34076, 34467, 34816, 34817, 34818, 34819, 34877, 34921, 34930, 34964, 34965, 35660, 35661, 35724,
    35725, 35738, 35739, 36003, 36004, 36005, 36006, 36007, 36347, 36348, 36349, 37440, 37441, 37443,
];
/// The names a WebGL 2 context adds (`DRAW_BUFFERi` aside, whose count is the device's
/// `MAX_DRAW_BUFFERS`): with [`V1_NAMES`], the 151 Chrome 150 accepts.
const V2_NAMES: &[u32] = &[
    3074, 3314, 3315, 3316, 3330, 3331, 3332, 32874, 32877, 32878, 32883, 33000, 33001, 34045, 34229,
    34852, 35053, 35055, 35071, 35076, 35077, 35097, 35368, 35371, 35373, 35374, 35375, 35376, 35377,
    35379, 35380, 35657, 35658, 35659, 35723, 35869, 35968, 35977, 35978, 35979, 35983, 36010, 36063,
    36183, 36203, 36387, 36388, 36389, 36662, 36663, 37137, 37154, 37157, 37447,
];
const DRAW_BUFFER0: u32 = 0x8825;
const MAX_DRAW_BUFFERS: u32 = 0x8824;
const MAX_COLOR_ATTACHMENTS: u32 = 0x8CDF;
const VERTEX_ARRAY_BINDING: u32 = 0x85B5;
const MAX_TEXTURE_MAX_ANISOTROPY_EXT: u32 = 0x84FF;
const MAX_DUAL_SOURCE_DRAW_BUFFERS_WEBGL: u32 = 0x88FC;
const TIMESTAMP_EXT: u32 = 0x8E28;
const GPU_DISJOINT_EXT: u32 = 0x8FBB;
const COMPRESSED_TEXTURE_FORMATS: u32 = 0x86A3;

/// Device limits an extension exposes that the recorded bundles do not carry (the capture asked
/// before enabling anything; `capture/collect.js` records them from now on). Each value is what every
/// desktop GL driver ANGLE runs on reports: `GL_MAX_TEXTURE_MAX_ANISOTROPY_EXT` 16 and
/// `GL_MAX_DUAL_SOURCE_DRAW_BUFFERS` 1. A bundle that recorded the name wins.
const EXT_DEVICE_DEFAULT: &[(u32, f64)] = &[(MAX_TEXTURE_MAX_ANISOTROPY_EXT, 16.0), (MAX_DUAL_SOURCE_DRAW_BUFFERS_WEBGL, 1.0)];

/// `OVR_multiview2`'s one `getParameter` name (Blink's `WebGL2RenderingContextBase::getParameter`: `GetIntParameter`
/// once the extension is enabled). Windows Chrome 152's D3D11 device has the extension (4, measured 2026-10-17);
/// Linux Chrome 152's GL one has not.
const MAX_VIEWS_OVR: u32 = 0x9631;

/// The formats each compressed-texture extension adds to `COMPRESSED_TEXTURE_FORMATS` when it is
/// enabled, in the order its Blink constructor adds them (`AddCompressedTextureFormat`).
pub(crate) fn compressed_formats(ext: &str) -> Vec<u32> {
    match ext {
        "WEBGL_compressed_texture_s3tc" => vec![0x83F0, 0x83F1, 0x83F2, 0x83F3],
        "WEBGL_compressed_texture_s3tc_srgb" => vec![0x8C4C, 0x8C4D, 0x8C4E, 0x8C4F],
        "WEBGL_compressed_texture_etc1" => vec![0x8D64],
        "WEBGL_compressed_texture_etc" => {
            vec![0x9270, 0x9271, 0x9274, 0x9275, 0x9276, 0x9277, 0x9272, 0x9273, 0x9278, 0x9279]
        }
        "WEBGL_compressed_texture_pvrtc" => vec![0x8C00, 0x8C01, 0x8C02, 0x8C03],
        "EXT_texture_compression_rgtc" => vec![0x8DBB, 0x8DBC, 0x8DBD, 0x8DBE],
        "EXT_texture_compression_bptc" => vec![0x8E8C, 0x8E8D, 0x8E8E, 0x8E8F],
        "WEBGL_compressed_texture_astc" => (0x93B0..=0x93BD).flat_map(|f| [f, f + 0x20]).collect(),
        _ => Vec::new(),
    }
}

/// Which binding a `bindBuffer` target writes, by the `getParameter` name that reads it back.
/// A buffer target WebGL 2 knows (`bufferData` on one with nothing bound is `INVALID_OPERATION`).
pub(crate) fn is_buffer_target(target: u32) -> bool {
    target == ELEMENT_ARRAY_BUFFER || buffer_binding(target).is_some()
}

fn buffer_binding(target: u32) -> Option<u32> {
    Some(match target {
        ARRAY_BUFFER => 0x8894,
        COPY_READ_BUFFER => 0x8F36,
        COPY_WRITE_BUFFER => 0x8F37,
        PIXEL_PACK_BUFFER => 0x88ED,
        PIXEL_UNPACK_BUFFER => 0x88EF,
        TRANSFORM_FEEDBACK_BUFFER => 0x8C8F,
        UNIFORM_BUFFER => 0x8A28,
        _ => return None,
    })
}

// ── the state ───────────────────────────────────────────────────────────────────────────────────

/// One face's stencil state (`STENCIL_*` / `STENCIL_BACK_*`), at GL's initial values.
#[derive(Clone, Debug)]
struct Face {
    func: u32,
    reference: i32,
    value_mask: u32,
    fail: u32,
    zfail: u32,
    zpass: u32,
    writemask: u32,
}

impl Default for Face {
    fn default() -> Self {
        Face { func: ALWAYS, reference: 0, value_mask: u32::MAX, fail: KEEP, zfail: KEEP, zpass: KEEP, writemask: u32::MAX }
    }
}

/// What a context's `getParameter` reads, at the values Blink's context starts with.
#[derive(Clone, Debug)]
pub(crate) struct Params {
    caps: HashSet<u32>,
    viewport: [i32; 4],
    scissor: [i32; 4],
    clear_color: [f32; 4],
    clear_depth: f32,
    clear_stencil: i32,
    color_mask: [bool; 4],
    depth_mask: bool,
    depth_func: u32,
    depth_range: [f32; 2],
    blend_color: [f32; 4],
    /// `src_rgb`, `dst_rgb`, `src_alpha`, `dst_alpha`.
    blend_func: [u32; 4],
    blend_eq: [u32; 2],
    cull: u32,
    front_face: u32,
    line_width: f32,
    polygon_offset: [f32; 2],
    coverage: (f32, bool),
    stencil: [Face; 2],
    hints: HashMap<u32, u32>,
    pixel: HashMap<u32, i32>,
    active_unit: u32,
    /// `(unit, texture target)` -> texture.
    units: HashMap<(u32, u32), u32>,
    /// unit -> sampler.
    samplers: HashMap<u32, u32>,
    /// A binding's `getParameter` name -> the object bound there.
    bindings: HashMap<u32, u32>,
    vao: u32,
    /// Vertex array -> its `ELEMENT_ARRAY_BUFFER` (0 is the default vertex array).
    vao_element: HashMap<u32, u32>,
    /// Framebuffer (0 = the default one) -> its `READ_BUFFER` and `DRAW_BUFFERi`.
    read_buffer: HashMap<u32, u32>,
    draw_buffers: HashMap<u32, Vec<u32>>,
    deleted: HashSet<u32>,
    /// Buffer -> bound as `ELEMENT_ARRAY_BUFFER` (WebGL never lets one buffer serve both kinds).
    element: HashMap<u32, bool>,
    /// Texture -> the target it was first bound to.
    texture_target: HashMap<u32, u32>,
    /// `MAX_COMBINED_TEXTURE_IMAGE_UNITS`: the units `activeTexture` accepts.
    pub(crate) max_units: u32,
    /// `drawingBufferColorSpace` / `unpackColorSpace`.
    pub(crate) drawing_color_space: &'static str,
    pub(crate) unpack_color_space: &'static str,
    /// The drawing buffer's size when the context was made.
    size: (i32, i32),
}

impl Default for Params {
    fn default() -> Self {
        Params::new(300, 150)
    }
}

impl Params {
    /// A new context's state. Blink sets the viewport and scissor box to the drawing buffer's size when
    /// it makes the context (`InitializeNewContext`), and they keep that size if the canvas changes.
    pub(crate) fn new(width: i32, height: i32) -> Self {
        Params {
            caps: [DITHER].into_iter().collect(),
            viewport: [0, 0, width, height],
            scissor: [0, 0, width, height],
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            color_mask: [true; 4],
            depth_mask: true,
            depth_func: LESS,
            depth_range: [0.0, 1.0],
            blend_color: [0.0; 4],
            blend_func: [1, 0, 1, 0],
            blend_eq: [FUNC_ADD, FUNC_ADD],
            cull: BACK,
            front_face: CCW,
            line_width: 1.0,
            polygon_offset: [0.0, 0.0],
            coverage: (1.0, false),
            stencil: [Face::default(), Face::default()],
            hints: HashMap::new(),
            pixel: HashMap::new(),
            active_unit: 0,
            units: HashMap::new(),
            samplers: HashMap::new(),
            bindings: HashMap::new(),
            vao: 0,
            vao_element: HashMap::new(),
            read_buffer: HashMap::new(),
            draw_buffers: HashMap::new(),
            deleted: HashSet::new(),
            element: HashMap::new(),
            texture_target: HashMap::new(),
            max_units: 32,
            drawing_color_space: "srgb",
            unpack_color_space: "srgb",
            size: (width, height),
        }
    }

    /// The state a RESTORED context starts with: Blink runs `InitializeNewContext` again, so everything
    /// is at its initial value except what belongs to the context object rather than to GL (the colour
    /// spaces) and the device's unit count.
    pub(crate) fn restored(&self) -> Self {
        let mut p = Params::new(self.size.0, self.size.1);
        p.max_units = self.max_units;
        p.drawing_color_space = self.drawing_color_space;
        p.unpack_color_space = self.unpack_color_space;
        p
    }

    fn draw_framebuffer(&self) -> u32 {
        self.bindings.get(&0x8CA6).copied().unwrap_or(0)
    }

    /// The framebuffer `readPixels` reads: `READ_FRAMEBUFFER_BINDING`, where WebGL 1's one binding is recorded too.
    pub(crate) fn read_framebuffer(&self) -> u32 {
        self.bindings.get(&0x8CAA).copied().unwrap_or(0)
    }

    pub(crate) fn is_enabled(&self, cap: u32) -> bool {
        self.caps.contains(&cap)
    }

    /// Is `cap` a capability `enable` takes on this context? Extension capabilities reach here only
    /// once their extension is enabled (`extensions::gated` answers before).
    pub(crate) fn valid_cap(cap: u32, v2: bool) -> bool {
        BASE_CAPS.contains(&cap)
            || (v2 && cap == RASTERIZER_DISCARD)
            || cap == DEPTH_CLAMP_EXT
            || cap == POLYGON_OFFSET_LINE_WEBGL
            || (v2 && (0x3000..=0x3007).contains(&cap))
    }

    /// The error a setter raises INSTEAD of taking effect, or `None` when it is valid -- the checks
    /// Blink makes before calling GL and the ones ANGLE's validation then makes, in that order.
    pub(crate) fn check(&self, op: &GlOp, v2: bool, enabled: &HashSet<String>) -> Option<u32> {
        let face = |f: u32| matches!(f, FRONT | BACK | FRONT_AND_BACK);
        let func = |f: u32| (NEVER..=ALWAYS).contains(&f);
        let stencil_op = |o: u32| matches!(o, 0 | KEEP | 0x1E01 | 0x1E02 | 0x1E03 | 0x150A | 0x8507 | 0x8508);
        let extended = enabled.contains("WEBGL_blend_func_extended");
        let factor = |f: u32, dst: bool| {
            matches!(f, 0 | 1 | 0x0300..=0x0307 | CONSTANT_COLOR..=ONE_MINUS_CONSTANT_ALPHA)
                || (f == SRC_ALPHA_SATURATE && (!dst || v2))
                || (extended && matches!(f, 0x88F9 | 0x8589 | 0x88FA | 0x88FB))
        };
        let gone = |id: u32| id != 0 && self.deleted.contains(&id);
        let err = match op {
            GlOp::Enable { cap } | GlOp::Disable { cap } => (!Self::valid_cap(*cap, v2)).then_some(INVALID_ENUM),
            GlOp::DepthFunc { func: f } => (!func(*f)).then_some(INVALID_ENUM),
            GlOp::StencilFunc { face: fc, func: f, .. } => (!face(*fc) || !func(*f)).then_some(INVALID_ENUM),
            GlOp::StencilOp { face: fc, fail, zfail, zpass } => {
                (!face(*fc) || !stencil_op(*fail) || !stencil_op(*zfail) || !stencil_op(*zpass)).then_some(INVALID_ENUM)
            }
            GlOp::BlendFuncSeparate { src_rgb, dst_rgb, src_alpha, dst_alpha } => {
                // WebGL forbids a constant COLOR factor with a constant ALPHA one in the same pair
                // (Blink's ValidateBlendFuncFactors, before GL sees the call).
                let color = |f: u32| f == CONSTANT_COLOR || f == ONE_MINUS_CONSTANT_COLOR;
                let alpha = |f: u32| f == CONSTANT_ALPHA || f == ONE_MINUS_CONSTANT_ALPHA;
                if (color(*src_rgb) && alpha(*dst_rgb)) || (alpha(*src_rgb) && color(*dst_rgb)) {
                    Some(INVALID_OPERATION)
                } else if !factor(*src_rgb, false) || !factor(*dst_rgb, true) || !factor(*src_alpha, false) || !factor(*dst_alpha, true) {
                    Some(INVALID_ENUM)
                } else {
                    None
                }
            }
            GlOp::BlendEquationSeparate { rgb, alpha } => {
                let minmax = v2 || enabled.contains("EXT_blend_minmax");
                let ok = |m: u32| matches!(m, FUNC_ADD | FUNC_SUBTRACT | FUNC_REVERSE_SUBTRACT) || (minmax && matches!(m, MIN | MAX));
                (!ok(*rgb) || !ok(*alpha)).then_some(INVALID_ENUM)
            }
            GlOp::CullFace { mode } => (!face(*mode)).then_some(INVALID_ENUM),
            GlOp::FrontFace { mode } => (!matches!(*mode, CW | CCW)).then_some(INVALID_ENUM),
            GlOp::LineWidth { width } => (width.is_nan() || *width <= 0.0).then_some(INVALID_VALUE),
            GlOp::DepthRange { near, far } => (near > far).then_some(INVALID_OPERATION),
            GlOp::Viewport { width, height, .. } | GlOp::Scissor { width, height, .. } => {
                (*width < 0 || *height < 0).then_some(INVALID_VALUE)
            }
            GlOp::Hint { target, mode } => {
                if !matches!(*target, GENERATE_MIPMAP_HINT | FRAGMENT_SHADER_DERIVATIVE_HINT) {
                    Some(INVALID_ENUM)
                } else {
                    (!matches!(*mode, DONT_CARE | FASTEST | NICEST)).then_some(INVALID_ENUM)
                }
            }
            GlOp::PixelStorei { pname, param } => match *pname {
                PACK_ALIGNMENT | UNPACK_ALIGNMENT => (!matches!(*param, 1 | 2 | 4 | 8)).then_some(INVALID_VALUE),
                UNPACK_FLIP_Y_WEBGL | UNPACK_PREMULTIPLY_ALPHA_WEBGL => None,
                UNPACK_COLORSPACE_CONVERSION_WEBGL => {
                    (!(*param as u32 == BROWSER_DEFAULT_WEBGL || *param == 0)).then_some(INVALID_VALUE)
                }
                p if v2 && PIXEL_STORE_V2.contains(&p) => (*param < 0).then_some(INVALID_VALUE),
                _ => Some(INVALID_ENUM),
            },
            GlOp::ActiveTexture { unit } => (unit.wrapping_sub(TEXTURE0) >= self.max_units).then_some(INVALID_ENUM),
            GlOp::BindBuffer { target, buffer } => {
                let known = matches!(*target, ARRAY_BUFFER | ELEMENT_ARRAY_BUFFER) || (v2 && buffer_binding(*target).is_some());
                if gone(*buffer) {
                    Some(INVALID_OPERATION)
                } else if !known {
                    Some(INVALID_ENUM)
                } else {
                    let want = *target == ELEMENT_ARRAY_BUFFER;
                    match self.element.get(buffer) {
                        Some(&is) if *buffer != 0 && is != want => Some(INVALID_OPERATION),
                        _ => None,
                    }
                }
            }
            GlOp::BindTexture { target, texture } => {
                let known = matches!(*target, TEXTURE_2D | TEXTURE_CUBE_MAP) || (v2 && matches!(*target, TEXTURE_3D | TEXTURE_2D_ARRAY));
                if gone(*texture) {
                    Some(INVALID_OPERATION)
                } else if !known {
                    Some(INVALID_ENUM)
                } else {
                    match self.texture_target.get(texture) {
                        Some(&t) if *texture != 0 && t != *target => Some(INVALID_OPERATION),
                        _ => None,
                    }
                }
            }
            GlOp::BindFramebuffer { target, framebuffer } => {
                if gone(*framebuffer) {
                    Some(INVALID_OPERATION)
                } else {
                    (!(*target == FRAMEBUFFER || (v2 && matches!(*target, READ_FRAMEBUFFER | DRAW_FRAMEBUFFER)))).then_some(INVALID_ENUM)
                }
            }
            GlOp::BindRenderbuffer { target, renderbuffer } => {
                if gone(*renderbuffer) {
                    Some(INVALID_OPERATION)
                } else {
                    (*target != RENDERBUFFER).then_some(INVALID_ENUM)
                }
            }
            GlOp::BindVertexArray { id } => gone(*id).then_some(INVALID_OPERATION),
            GlOp::UseProgram { program } => gone(*program).then_some(INVALID_OPERATION),
            // Blink's ValidateDrawArrays / ValidateDrawElements, before the command buffer: an index
            // type WebGL 1 lacks, then "no valid shader program in use".
            GlOp::DrawElements { ty, .. } | GlOp::DrawElementsInstanced { ty, .. }
                if *ty == 0x1405 && !v2 && !enabled.contains("OES_element_index_uint") =>
            {
                Some(INVALID_ENUM)
            }
            GlOp::DrawArrays { .. } | GlOp::DrawArraysInstanced { .. } | GlOp::DrawElements { .. } | GlOp::DrawElementsInstanced { .. } => {
                (self.bindings.get(&0x8B8D).copied().unwrap_or(0) == 0).then_some(INVALID_OPERATION)
            }
            _ => None,
        };
        err
    }

    /// Take a VALID op's effect on the state (see [`Params::check`]).
    pub(crate) fn apply(&mut self, op: &GlOp) {
        let clamp = |x: f32| if x.is_nan() { x } else { x.clamp(0.0, 1.0) };
        match op {
            GlOp::Enable { cap } => {
                self.caps.insert(*cap);
            }
            GlOp::Disable { cap } => {
                self.caps.remove(cap);
            }
            GlOp::Viewport { x, y, width, height } => self.viewport = [*x, *y, *width, *height],
            GlOp::Scissor { x, y, width, height } => self.scissor = [*x, *y, *width, *height],
            // Blink's clearColor turns a NaN component into 0 before GL sees it.
            GlOp::ClearColor { r, g, b, a } => {
                self.clear_color = [*r, *g, *b, *a].map(|c| if c.is_nan() { 0.0 } else { c })
            }
            GlOp::BlendColor { r, g, b, a } => self.blend_color = [*r, *g, *b, *a],
            GlOp::ClearDepth { depth } => self.clear_depth = clamp(*depth),
            GlOp::ClearStencil { s } => self.clear_stencil = *s,
            GlOp::ColorMask { r, g, b, a } => self.color_mask = [*r, *g, *b, *a],
            GlOp::DepthMask { flag } => self.depth_mask = *flag,
            GlOp::DepthFunc { func } => self.depth_func = *func,
            GlOp::DepthRange { near, far } => self.depth_range = [clamp(*near), clamp(*far)],
            GlOp::StencilMask { mask } => {
                self.stencil[0].writemask = *mask;
                self.stencil[1].writemask = *mask;
            }
            GlOp::StencilFunc { face, func, reference, mask } => {
                for f in self.faces(*face) {
                    let s = &mut self.stencil[f];
                    s.func = *func;
                    s.reference = *reference;
                    s.value_mask = *mask;
                }
            }
            GlOp::StencilOp { face, fail, zfail, zpass } => {
                for f in self.faces(*face) {
                    let s = &mut self.stencil[f];
                    s.fail = *fail;
                    s.zfail = *zfail;
                    s.zpass = *zpass;
                }
            }
            GlOp::BlendFuncSeparate { src_rgb, dst_rgb, src_alpha, dst_alpha } => {
                self.blend_func = [*src_rgb, *dst_rgb, *src_alpha, *dst_alpha]
            }
            GlOp::BlendEquationSeparate { rgb, alpha } => self.blend_eq = [*rgb, *alpha],
            GlOp::CullFace { mode } => self.cull = *mode,
            GlOp::FrontFace { mode } => self.front_face = *mode,
            GlOp::LineWidth { width } => self.line_width = *width,
            GlOp::PolygonOffset { factor, units } => self.polygon_offset = [*factor, *units],
            GlOp::SampleCoverage { value, invert } => self.coverage = (clamp(*value), *invert),
            GlOp::Hint { target, mode } => {
                self.hints.insert(*target, *mode);
            }
            GlOp::PixelStorei { pname, param } => {
                let v = match *pname {
                    UNPACK_FLIP_Y_WEBGL | UNPACK_PREMULTIPLY_ALPHA_WEBGL => (*param != 0) as i32,
                    _ => *param,
                };
                self.pixel.insert(*pname, v);
            }
            GlOp::ActiveTexture { unit } => self.active_unit = unit - TEXTURE0,
            GlOp::BindTexture { target, texture } => {
                self.units.insert((self.active_unit, *target), *texture);
                if *texture != 0 {
                    self.texture_target.entry(*texture).or_insert(*target);
                }
            }
            GlOp::BindBuffer { target, buffer } => {
                if *target == ELEMENT_ARRAY_BUFFER {
                    self.vao_element.insert(self.vao, *buffer);
                } else if let Some(b) = buffer_binding(*target) {
                    self.bindings.insert(b, *buffer);
                }
                if *buffer != 0 {
                    self.element.entry(*buffer).or_insert(*target == ELEMENT_ARRAY_BUFFER);
                }
            }
            GlOp::BindFramebuffer { target, framebuffer } => {
                if matches!(*target, FRAMEBUFFER | DRAW_FRAMEBUFFER) {
                    self.bindings.insert(0x8CA6, *framebuffer);
                }
                if matches!(*target, FRAMEBUFFER | READ_FRAMEBUFFER) {
                    self.bindings.insert(0x8CAA, *framebuffer);
                }
            }
            GlOp::BindRenderbuffer { renderbuffer, .. } => {
                self.bindings.insert(0x8CA7, *renderbuffer);
            }
            GlOp::BindVertexArray { id } => self.vao = *id,
            GlOp::UseProgram { program } => {
                self.bindings.insert(0x8B8D, *program);
            }
            // Deleting an object unbinds it from every binding point that holds it, as Blink's
            // `delete*` does; the program in use stays current until another replaces it.
            GlOp::DeleteBuffer { id } => {
                self.deleted.insert(*id);
                self.bindings.retain(|k, v| !(*v == *id && *k != 0x8B8D && *k != 0x8CA6 && *k != 0x8CAA && *k != 0x8CA7));
                let vao = self.vao;
                if self.vao_element.get(&vao) == Some(id) {
                    self.vao_element.remove(&vao);
                }
            }
            GlOp::DeleteTexture { id } => {
                self.deleted.insert(*id);
                self.units.retain(|_, v| v != id);
            }
            GlOp::DeleteFramebuffer { id } => {
                self.deleted.insert(*id);
                for k in [0x8CA6, 0x8CAA] {
                    if self.bindings.get(&k) == Some(id) {
                        self.bindings.remove(&k);
                    }
                }
            }
            GlOp::DeleteRenderbuffer { id } => {
                self.deleted.insert(*id);
                if self.bindings.get(&0x8CA7) == Some(id) {
                    self.bindings.remove(&0x8CA7);
                }
            }
            GlOp::DeleteVertexArray { id } => {
                self.deleted.insert(*id);
                if self.vao == *id {
                    self.vao = 0;
                }
            }
            GlOp::DeleteProgram { id } | GlOp::DeleteShader { id } => {
                self.deleted.insert(*id);
            }
            _ => {}
        }
    }

    fn faces(&self, face: u32) -> Vec<usize> {
        match face {
            FRONT => vec![0],
            BACK => vec![1],
            _ => vec![0, 1],
        }
    }

    /// A WebGL 2 binding no GL op carries (`bindSampler`, `bindBufferBase`, `bindTransformFeedback`,
    /// `readBuffer`, `drawBuffers`). `None` = the call is valid and took effect; `Some(e)` = the error.
    /// The object bound at a binding's `getParameter` name (0 for none): `CURRENT_PROGRAM`, the draw /
    /// read framebuffer, the renderbuffer, the transform feedback.
    pub(crate) fn binding(&self, pname: u32) -> u32 {
        self.bindings.get(&pname).copied().unwrap_or(0)
    }

    /// The target a texture was first bound to.
    pub(crate) fn texture_target(&self, id: u32) -> Option<u32> {
        self.texture_target.get(&id).copied()
    }

    /// `(clear color, clear depth, clear stencil)` as set now.
    pub(crate) fn clear_values(&self) -> ([f32; 4], f32, i32) {
        (self.clear_color, self.clear_depth, self.clear_stencil)
    }

    /// Has `id` been deleted?
    pub(crate) fn is_deleted(&self, id: u32) -> bool {
        id != 0 && self.deleted.contains(&id)
    }

    /// The buffer bound to `target` (the current vertex array's for `ELEMENT_ARRAY_BUFFER`), 0 for none.
    pub(crate) fn buffer_for(&self, target: u32) -> u32 {
        if target == ELEMENT_ARRAY_BUFFER {
            return self.vao_element.get(&self.vao).copied().unwrap_or(0);
        }
        buffer_binding(target).and_then(|b| self.bindings.get(&b).copied()).unwrap_or(0)
    }

    /// The texture bound to `target` on the active unit, 0 for none.
    pub(crate) fn texture_for(&self, target: u32) -> u32 {
        self.units.get(&(self.active_unit, target)).copied().unwrap_or(0)
    }

    pub(crate) fn bind_sampler(&mut self, unit: u32, sampler: u32) -> Option<u32> {
        if sampler != 0 && self.deleted.contains(&sampler) {
            return Some(INVALID_OPERATION);
        }
        if unit >= self.max_units {
            return Some(INVALID_VALUE);
        }
        self.samplers.insert(unit, sampler);
        None
    }

    pub(crate) fn bind_buffer_base(&mut self, target: u32, buffer: u32) -> Option<u32> {
        if buffer != 0 && self.deleted.contains(&buffer) {
            return Some(INVALID_OPERATION);
        }
        let Some(b) = buffer_binding(target).filter(|_| matches!(target, UNIFORM_BUFFER | TRANSFORM_FEEDBACK_BUFFER)) else {
            return Some(INVALID_ENUM);
        };
        if buffer != 0 && self.element.get(&buffer) == Some(&true) {
            return Some(INVALID_OPERATION);
        }
        self.bindings.insert(b, buffer);
        if buffer != 0 {
            self.element.entry(buffer).or_insert(false);
        }
        None
    }

    pub(crate) fn stencil_mask_separate(&mut self, face: u32, mask: u32) -> Option<u32> {
        if !matches!(face, FRONT | BACK | FRONT_AND_BACK) {
            return Some(INVALID_ENUM);
        }
        for f in self.faces(face) {
            self.stencil[f].writemask = mask;
        }
        None
    }

    pub(crate) fn bind_transform_feedback(&mut self, tf: u32) {
        self.bindings.insert(0x8E25, tf);
    }

    pub(crate) fn read_buffer(&mut self, src: u32) -> Option<u32> {
        let fb = self.bindings.get(&0x8CAA).copied().unwrap_or(0);
        let ok = if fb == 0 { matches!(src, BACK | NONE) } else { src == NONE || (0x8CE0..0x8CE0 + 16).contains(&src) };
        if !ok {
            return Some(if fb == 0 || (0x8CE0..=0x8CFF).contains(&src) { INVALID_OPERATION } else { INVALID_ENUM });
        }
        self.read_buffer.insert(fb, src);
        None
    }

    pub(crate) fn draw_buffers(&mut self, list: Vec<u32>) -> Option<u32> {
        let fb = self.draw_framebuffer();
        if fb == 0 {
            if list.len() != 1 || !matches!(list[0], BACK | NONE) {
                return Some(INVALID_OPERATION);
            }
        } else {
            for (i, b) in list.iter().enumerate() {
                if *b != NONE && *b != 0x8CE0 + i as u32 {
                    return Some(INVALID_OPERATION);
                }
            }
        }
        self.draw_buffers.insert(fb, list);
        None
    }
}

// ── the switch ──────────────────────────────────────────────────────────────────────────────────

/// What `getParameter(pname)` answers, before it becomes a JS value.
pub(crate) enum Answer {
    /// INVALID_ENUM and `null`.
    Invalid,
    Bool(bool),
    Uint(u32),
    Int(i32),
    Float(f32),
    Number(f64),
    Floats(Vec<f32>),
    Ints(Vec<i32>),
    Uints(Vec<u32>),
    Bools(Vec<bool>),
    /// A bound object, by id (0 = `null`).
    Object(u32),
    /// The recorded device's value for this name.
    Device,
    /// The device's value from the WebGL 2 table (a WebGL 1 context reading an extension's limit).
    DeviceV2,
    /// The device's value, or this when the bundle did not record it.
    DeviceOr(f64),
}

/// Blink's `getParameter` switch for the context `s`.
pub(crate) fn classify(s: &WebGLState, pname: u32, max_draw_buffers: u32) -> Answer {
    use Answer::*;
    let p = &s.params;
    let v2 = s.v2;
    let on = |e: &str| s.enabled.contains(e);
    let fb = p.draw_framebuffer();
    // `DRAW_BUFFERi`, in WebGL 2 or with `WEBGL_draw_buffers`, up to the device's MAX_DRAW_BUFFERS.
    if (v2 || on("WEBGL_draw_buffers")) && pname >= DRAW_BUFFER0 && pname < DRAW_BUFFER0 + max_draw_buffers {
        let i = (pname - DRAW_BUFFER0) as usize;
        // The default framebuffer answers its one back buffer for EVERY index (Blink's emulated
        // backbuffer, `back_draw_buffer_`); a framebuffer object its own list, COLOR_ATTACHMENT0 then
        // NONE until `drawBuffers` says otherwise.
        let v = match p.draw_buffers.get(&fb) {
            Some(list) if fb == 0 => list.first().copied().unwrap_or(BACK),
            Some(list) => list.get(i).copied().unwrap_or(NONE),
            None if fb == 0 => BACK,
            None => if i == 0 { 0x8CE0 } else { NONE },
        };
        return Uint(v);
    }
    let stencil = |f: usize| &p.stencil[f];
    match pname {
        // Capabilities.
        BLEND | CULL_FACE | DEPTH_TEST | DITHER | POLYGON_OFFSET_FILL | SAMPLE_ALPHA_TO_COVERAGE | SAMPLE_COVERAGE
        | SCISSOR_TEST | STENCIL_TEST => Bool(p.caps.contains(&pname)),
        RASTERIZER_DISCARD if v2 => Bool(p.caps.contains(&pname)),
        DEPTH_CLAMP_EXT if on("EXT_depth_clamp") => Bool(p.caps.contains(&pname)),
        POLYGON_OFFSET_LINE_WEBGL if on("WEBGL_polygon_mode") => Bool(p.caps.contains(&pname)),
        0x3000..=0x3007 if v2 && on("WEBGL_clip_cull_distance") => Bool(p.caps.contains(&pname)),
        // Fixed-function state.
        0x0BA2 => Ints(p.viewport.to_vec()),
        0x0C10 => Ints(p.scissor.to_vec()),
        0x0C22 => Floats(p.clear_color.to_vec()),
        0x0B73 => Float(p.clear_depth),
        0x0B91 => Int(p.clear_stencil),
        0x0C23 => Bools(p.color_mask.to_vec()),
        0x0B72 => Bool(p.depth_mask),
        0x0B74 => Uint(p.depth_func),
        0x0B70 => Floats(p.depth_range.to_vec()),
        0x8005 => Floats(p.blend_color.to_vec()),
        0x80C9 => Uint(p.blend_func[0]),
        0x80C8 => Uint(p.blend_func[1]),
        0x80CB => Uint(p.blend_func[2]),
        0x80CA => Uint(p.blend_func[3]),
        0x8009 => Uint(p.blend_eq[0]),
        0x883D => Uint(p.blend_eq[1]),
        0x0B45 => Uint(p.cull),
        0x0B46 => Uint(p.front_face),
        0x0B21 => Float(p.line_width),
        0x8038 => Float(p.polygon_offset[0]),
        0x2A00 => Float(p.polygon_offset[1]),
        0x80AA => Float(p.coverage.0),
        0x80AB => Bool(p.coverage.1),
        0x0B92 => Uint(stencil(0).func),
        0x0B97 => Int(stencil(0).reference),
        0x0B93 => Uint(stencil(0).value_mask),
        0x0B94 => Uint(stencil(0).fail),
        0x0B95 => Uint(stencil(0).zfail),
        0x0B96 => Uint(stencil(0).zpass),
        0x0B98 => Uint(stencil(0).writemask),
        0x8800 => Uint(stencil(1).func),
        0x8CA3 => Int(stencil(1).reference),
        0x8CA4 => Uint(stencil(1).value_mask),
        0x8801 => Uint(stencil(1).fail),
        0x8802 => Uint(stencil(1).zfail),
        0x8803 => Uint(stencil(1).zpass),
        0x8CA5 => Uint(stencil(1).writemask),
        GENERATE_MIPMAP_HINT => Uint(p.hints.get(&pname).copied().unwrap_or(DONT_CARE)),
        FRAGMENT_SHADER_DERIVATIVE_HINT if v2 || on("OES_standard_derivatives") => {
            Uint(p.hints.get(&pname).copied().unwrap_or(DONT_CARE))
        }
        PACK_ALIGNMENT | UNPACK_ALIGNMENT => Int(p.pixel.get(&pname).copied().unwrap_or(4)),
        UNPACK_FLIP_Y_WEBGL | UNPACK_PREMULTIPLY_ALPHA_WEBGL => Bool(p.pixel.get(&pname).copied().unwrap_or(0) != 0),
        UNPACK_COLORSPACE_CONVERSION_WEBGL => {
            Uint(p.pixel.get(&pname).map(|v| *v as u32).unwrap_or(BROWSER_DEFAULT_WEBGL))
        }
        x if v2 && PIXEL_STORE_V2.contains(&x) => Int(p.pixel.get(&x).copied().unwrap_or(0)),
        0x84E0 => Uint(TEXTURE0 + p.active_unit),
        // Bindings.
        0x8894 | 0x8B8D | 0x8CA6 | 0x8CA7 => Object(p.bindings.get(&pname).copied().unwrap_or(0)),
        0x8895 => Object(p.vao_element.get(&p.vao).copied().unwrap_or(0)),
        0x8069 => Object(p.units.get(&(p.active_unit, TEXTURE_2D)).copied().unwrap_or(0)),
        0x8514 => Object(p.units.get(&(p.active_unit, TEXTURE_CUBE_MAP)).copied().unwrap_or(0)),
        VERTEX_ARRAY_BINDING if v2 || on("OES_vertex_array_object") => Object(p.vao),
        0x806A if v2 => Object(p.units.get(&(p.active_unit, TEXTURE_3D)).copied().unwrap_or(0)),
        0x8C1D if v2 => Object(p.units.get(&(p.active_unit, TEXTURE_2D_ARRAY)).copied().unwrap_or(0)),
        0x8CAA | 0x8F36 | 0x8F37 | 0x88ED | 0x88EF | 0x8C8F | 0x8A28 | 0x8E25 if v2 => {
            Object(p.bindings.get(&pname).copied().unwrap_or(0))
        }
        0x8919 if v2 => Object(p.samplers.get(&p.active_unit).copied().unwrap_or(0)),
        0x0C02 if v2 => {
            let rfb = p.bindings.get(&0x8CAA).copied().unwrap_or(0);
            Uint(p.read_buffer.get(&rfb).copied().unwrap_or(if rfb == 0 { BACK } else { 0x8CE0 }))
        }
        0x8E24 | 0x8E23 if v2 => Bool(false),
        // The drawing buffer's own rules (`DEPTH_BITS`/`STENCIL_BITS` are 0 for a default framebuffer the
        // page asked to have none of; a context without `antialias` is not multisampled).
        0x0D56 if fb == 0 && !s.attrs.depth => Int(0),
        0x0D57 if fb == 0 => Int(if s.attrs.stencil { 8 } else { 0 }),
        0x80A8 | 0x80A9 if fb != 0 || !s.attrs.antialias => Int(0),
        COMPRESSED_TEXTURE_FORMATS => {
            let mut out: Vec<u32> = Vec::new();
            for e in &s.enabled_order {
                for f in compressed_formats(e) {
                    if !out.contains(&f) {
                        out.push(f);
                    }
                }
            }
            Uints(out)
        }
        // Extension names, reached only once their extension is enabled (`extensions::gated`).
        MAX_TEXTURE_MAX_ANISOTROPY_EXT | MAX_DUAL_SOURCE_DRAW_BUFFERS_WEBGL => {
            DeviceOr(EXT_DEVICE_DEFAULT.iter().find(|(n, _)| *n == pname).map(|(_, v)| *v).unwrap_or(0.0))
        }
        TIMESTAMP_EXT if on(if v2 { "EXT_disjoint_timer_query_webgl2" } else { "EXT_disjoint_timer_query" }) => Number(0.0),
        GPU_DISJOINT_EXT if on(if v2 { "EXT_disjoint_timer_query_webgl2" } else { "EXT_disjoint_timer_query" }) => Bool(false),
        MAX_DRAW_BUFFERS | MAX_COLOR_ATTACHMENTS if !v2 && on("WEBGL_draw_buffers") => DeviceV2,
        0x9245 | 0x9246 if on("WEBGL_debug_renderer_info") => Device,
        MAX_VIEWS_OVR if v2 && on("OVR_multiview2") => Device,
        // Everything else Blink's switch names is the device's.
        x if V1_NAMES.contains(&x) || (v2 && V2_NAMES.contains(&x)) => Device,
        _ => Invalid,
    }
}

/// The names Blink reads from GL (`return Get*Parameter(...)`, `GetString`) that Chrome's GL client does NOT answer
/// from its own cache -- `GLES2Implementation::GetHelper` / `GetFloatvHelper` / `GetInteger64vHelper` keep the limits
/// and bindings, and `GetString` keeps nothing -- so each is a GPU-process round trip in Chrome.
/// Derived from Chromium 150's two `getParameter` switches and `gles2_implementation.cc`, and measured on desktop
/// Chrome 150 on this PC's GPU: ~60 calls per ms for these names, ~16,000 for the rest.
const ROUND_TRIP: &[u32] = &[
    0x0B21, 0x0B40, 0x0B44, 0x0B45, 0x0B46, 0x0B70, 0x0B72, 0x0B73, 0x0B74, 0x0B91, 0x0B92, 0x0B93,
    0x0B94, 0x0B95, 0x0B96, 0x0B97, 0x0B98, 0x0BD0, 0x0BE2, 0x0C10, 0x0C11, 0x0C22, 0x0C23, 0x0CF2,
    0x0D02, 0x0D05, 0x0D32, 0x0D50, 0x0D52, 0x0D53, 0x0D54, 0x0D55, 0x0D56, 0x0D57, 0x1F02, 0x2A00,
    0x2A02, 0x3000, 0x3001, 0x3002, 0x3003, 0x3004, 0x3005, 0x3006, 0x3007, 0x8005, 0x8009, 0x8037,
    0x8038, 0x806E, 0x809E, 0x80A0, 0x80A8, 0x80A9, 0x80AA, 0x80AB, 0x80C8, 0x80C9, 0x80CA, 0x80CB,
    0x8192, 0x82F9, 0x82FA, 0x846D, 0x846E, 0x84FF, 0x864F, 0x8800, 0x8801, 0x8802, 0x8803, 0x883D,
    0x88FC, 0x8B8B, 0x8B8C, 0x8B9A, 0x8B9B, 0x8C89, 0x8CA3, 0x8CA4, 0x8CA5, 0x8E1B, 0x8E23, 0x8E24,
    0x8E4F, 0x8E5B, 0x8E5C, 0x8E5D, 0x935C, 0x935D, 0x9631, 0x96E0, 0x96E1, 0x96E2,
];

/// The GL name a round trip for `pname` asks, when Chrome's client has to ask its GPU process for it ([`ROUND_TRIP`],
/// and `WEBGL_debug_renderer_info`'s two strings, `GetString(GL_VENDOR / GL_RENDERER)`); `None` when Chrome answers
/// `pname` in the renderer.
pub(crate) fn round_trip(s: &WebGLState, pname: u32) -> Option<u32> {
    let p = &s.params;
    match pname {
        0x9245 => Some(0x1F00),
        0x9246 => Some(0x1F01),
        // Blink answers these 0 itself for a drawing buffer that has none (`if (!framebuffer_binding_ &&
        // !CreationAttributes().depth / .stencil)`), before any GL call
        0x0D56 if p.draw_framebuffer() == 0 && !s.attrs.depth => None,
        0x0D57 if p.draw_framebuffer() == 0 && !s.attrs.stencil => None,
        // `VIEWPORT`: the client keeps it once it is a positive size (Blink's `InitializeNewContext` sets one)
        0x0BA2 => (p.viewport[2] <= 0 || p.viewport[3] <= 0).then_some(pname),
        x if ROUND_TRIP.contains(&x) => Some(x),
        _ => None,
    }
}

/// Every name [`classify`] accepts on a fresh context with no extension enabled, for the tests.
#[cfg(test)]
pub(crate) fn base_names(v2: bool) -> Vec<u32> {
    let mut out: Vec<u32> = V1_NAMES.to_vec();
    if v2 {
        out.extend_from_slice(V2_NAMES);
    }
    out.sort();
    out
}

/// `PredefinedColorSpace` (`core/html/canvas/predefined_color_space.idl`): the values the enum has,
/// and whether this build lets a page set each one. `rec2020`, `rec2100-*` and the linear spaces sit
/// behind runtime flags that are `experimental` in Chrome 150, so setting one throws a TypeError
/// (`ValidateAndConvertColorSpace`); a string that is not in the enum at all is ignored by the
/// binding, as WebIDL says for an enumeration attribute.
pub(crate) fn predefined_color_space(s: &str) -> Option<(&'static str, bool)> {
    Some(match s {
        "srgb" => ("srgb", true),
        "display-p3" => ("display-p3", true),
        "rec2020" => ("rec2020", false),
        "rec2100-hlg" => ("rec2100-hlg", false),
        "rec2100-pq" => ("rec2100-pq", false),
        "srgb-linear" => ("srgb-linear", false),
        "display-p3-linear" => ("display-p3-linear", false),
        "rec2100-linear" => ("rec2100-linear", false),
        _ => return None,
    })
}

// ── `getInternalformatParameter` ────────────────────────────────────────────────────────────────

/// What Blink's `getInternalformatParameter` does with `internalformat` before it asks GL
/// (`WebGL2RenderingContextBase::getInternalformatParameter`): `Err` = INVALID_ENUM and `null`,
/// `Ok(false)` = an empty `Int32Array` (the unsized and integer formats, which multisample nothing),
/// `Ok(true)` = the device's sample counts.
pub(crate) fn internalformat_class(format: u32, enabled: &HashSet<String>) -> Result<bool, ()> {
    const EMPTY: &[u32] = &[
        0x1907, 0x1908, 0x8232, 0x8231, 0x8234, 0x8233, 0x8236, 0x8235, 0x8238, 0x8237, 0x823A, 0x8239,
        0x823C, 0x823B, 0x8D7C, 0x8D8E, 0x906F, 0x8D76, 0x8D88, 0x8D70, 0x8D82,
    ];
    const SAMPLED: &[u32] = &[
        0x8229, 0x822B, 0x8051, 0x8D62, 0x8058, 0x8C43, 0x8057, 0x8056, 0x8059, 0x81A5, 0x81A6, 0x8CAC,
        0x88F0, 0x8CAD, 0x8D48,
    ];
    let on = |e: &str| enabled.contains(e);
    if EMPTY.contains(&format) {
        Ok(false)
    } else if SAMPLED.contains(&format) {
        Ok(true)
    } else if matches!(format, 0x822A | 0x822C | 0x805B) && on("EXT_texture_norm16") {
        Ok(true)
    } else if matches!(format, 0x822D | 0x822F | 0x881A) && (on("EXT_color_buffer_float") || on("EXT_color_buffer_half_float")) {
        Ok(true)
    } else if matches!(format, 0x822E | 0x8230 | 0x8814 | 0x8C3A) && on("EXT_color_buffer_float") {
        Ok(true)
    } else {
        Err(())
    }
}

/// The sample counts ANGLE reports for a renderable format when the bundle recorded none: every power
/// of two from the device's `MAX_SAMPLES` down to 2, largest first -- the list ANGLE builds from the
/// driver's conformant counts (`renderergl_utils.cpp`), which is `[8, 4, 2]` for the MAX_SAMPLES 8
/// both recorded GPUs report (measured on the AMD one, google-chrome 150, 2026-10-08).
pub(crate) fn derived_samples(max_samples: u32) -> Vec<i32> {
    let mut out = Vec::new();
    let mut n = max_samples.max(1).next_power_of_two();
    if n > max_samples {
        n /= 2;
    }
    while n >= 2 {
        out.push(n as i32);
        n /= 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_valid_names_are_chromes_89_and_151() {
        assert_eq!(base_names(false).len(), 89);
        // + DRAW_BUFFER0..7 for a device with MAX_DRAW_BUFFERS 8 = 151.
        assert_eq!(base_names(true).len() + 8, 151);
    }

    #[test]
    fn an_invalid_setter_changes_nothing() {
        let p = Params::new(300, 150);
        let none = HashSet::new();
        assert_eq!(p.check(&GlOp::DepthFunc { func: 0x1234 }, false, &none), Some(INVALID_ENUM));
        assert_eq!(p.check(&GlOp::Enable { cap: RASTERIZER_DISCARD }, false, &none), Some(INVALID_ENUM));
        assert_eq!(p.check(&GlOp::Enable { cap: RASTERIZER_DISCARD }, true, &none), None);
        assert_eq!(
            p.check(&GlOp::BlendFuncSeparate { src_rgb: CONSTANT_COLOR, dst_rgb: CONSTANT_ALPHA, src_alpha: 1, dst_alpha: 0 }, false, &none),
            Some(INVALID_OPERATION)
        );
        assert_eq!(p.check(&GlOp::DepthRange { near: 0.8, far: 0.2 }, false, &none), Some(INVALID_OPERATION));
        assert_eq!(p.check(&GlOp::PixelStorei { pname: UNPACK_ALIGNMENT, param: 3 }, false, &none), Some(INVALID_VALUE));
    }

    #[test]
    fn sample_counts_from_max_samples() {
        assert_eq!(derived_samples(8), vec![8, 4, 2]);
        assert_eq!(derived_samples(4), vec![4, 2]);
        assert_eq!(derived_samples(16), vec![16, 8, 4, 2]);
        assert_eq!(derived_samples(1), Vec::<i32>::new());
    }
}
