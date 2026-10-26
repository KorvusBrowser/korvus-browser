//! `raster_gl` — a software OpenGL ES 2.0 pipeline that executes a recorded [`spec::GlOp`] list.
//!
//! ## Why software, on this machine
//!
//! The persona this engine wears claims an Apple M3 Pro behind ANGLE-Metal. This host is Linux with
//! no such GPU, so there is no configuration in which our pixels are the *reference device's* pixels;
//! the metadata strings stay served from the recorded persona oracle exactly as before, and the
//! rasterizer's job is the other half of the question — that a page which draws something gets that
//! something back. A software pipeline answers that completely, deterministically, with no driver,
//! no display server and no GPU. Binding a real GL context would buy pixels from *this* box's
//! llvmpipe or Intel driver, which is no closer to the claimed device and is one more thing that can
//! fail to initialize inside a synchronous `toDataURL()`.
//!
//! The cost is stated plainly: a page that hashes a WebGL frame and compares it against a database of
//! real Apple-M3 hashes will not find a match. That is already true of every other pixel axis on this
//! host (canvas 2D measures 7.69% different from the reference Brave here), and it is a smaller,
//! measurable gap than the one this replaces, which was *nothing drawn at all* — two different
//! `clearColor` values producing byte-identical 174-byte data URLs, which no GPU on earth does.
//!
//! ## Shape
//!
//! [`GlMachine`] is a plain interpreter over the op list: objects in maps, fixed-function state in
//! fields, a float framebuffer. `glsl.rs` compiles shader source, `interp.rs` runs it per vertex and
//! per fragment, `builtins.rs` is the standard library. The whole thing is re-run from the start on
//! every readback, which is what makes `readPixels` and `toDataURL` the same pixels by construction
//! rather than by agreement.

pub mod builtins;
pub mod glsl;
pub mod interp;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use glsl::{Qual, Shader, Type};
use interp::{Interp, RunError, TextureSource, Val};
use spec::{GlContextAttributes, GlObjectKind, GlOp, GlQuery, GlQueryResult, GlShaderKind, GlUniformValue, Rgba8};

// ── the GL enums this pipeline reads. Named rather than inlined, because a bare 0x8892 in a match
// arm is the kind of thing that is wrong for a year. ──────────────────────────────────────────

const ARRAY_BUFFER: u32 = 0x8892;
const ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
/// WebGL 2's other buffer binding points (COPY_READ/WRITE, PIXEL_PACK/UNPACK, TRANSFORM_FEEDBACK, UNIFORM): bound and
/// filled on a GPU context, nothing this pipeline reads.
const WEBGL2_BUFFER_TARGETS: [u32; 6] = [0x8F36, 0x8F37, 0x88EB, 0x88EC, 0x8C8E, 0x8A11];

const BYTE: u32 = 0x1400;
const UNSIGNED_BYTE: u32 = 0x1401;
const SHORT: u32 = 0x1402;
const UNSIGNED_SHORT: u32 = 0x1403;
const INT: u32 = 0x1404;
const UNSIGNED_INT: u32 = 0x1405;
const FLOAT: u32 = 0x1406;

const DEPTH_BUFFER_BIT: u32 = 0x0100;
const STENCIL_BUFFER_BIT: u32 = 0x0400;
const COLOR_BUFFER_BIT: u32 = 0x4000;

const NEVER: u32 = 0x0200;
const LESS: u32 = 0x0201;
const EQUAL: u32 = 0x0202;
const LEQUAL: u32 = 0x0203;
const GREATER: u32 = 0x0204;
const NOTEQUAL: u32 = 0x0205;
const GEQUAL: u32 = 0x0206;
const ALWAYS: u32 = 0x0207;

const FRONT: u32 = 0x0404;
const BACK: u32 = 0x0405;
const FRONT_AND_BACK: u32 = 0x0408;
const CW: u32 = 0x0900;

const BLEND: u32 = 0x0BE2;
const CULL_FACE: u32 = 0x0B44;
const DEPTH_TEST: u32 = 0x0B71;
const DITHER: u32 = 0x0BD0;
const SCISSOR_TEST: u32 = 0x0C11;

const POINTS: u32 = 0x0000;
const LINES: u32 = 0x0001;
const LINE_LOOP: u32 = 0x0002;
const LINE_STRIP: u32 = 0x0003;
const TRIANGLES: u32 = 0x0004;
const TRIANGLE_STRIP: u32 = 0x0005;
const TRIANGLE_FAN: u32 = 0x0006;

const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_CUBE_MAP: u32 = 0x8513;
const TEXTURE_CUBE_MAP_POSITIVE_X: u32 = 0x8515;
const TEXTURE_MAG_FILTER: u32 = 0x2800;
const TEXTURE_MIN_FILTER: u32 = 0x2801;
const TEXTURE_WRAP_S: u32 = 0x2802;
const TEXTURE_WRAP_T: u32 = 0x2803;
const NEAREST: u32 = 0x2600;
const LINEAR: u32 = 0x2601;
const NEAREST_MIPMAP_NEAREST: u32 = 0x2700;
const LINEAR_MIPMAP_NEAREST: u32 = 0x2701;
const NEAREST_MIPMAP_LINEAR: u32 = 0x2702;
const LINEAR_MIPMAP_LINEAR: u32 = 0x2703;
const REPEAT: u32 = 0x2901;
const CLAMP_TO_EDGE: u32 = 0x812F;
const MIRRORED_REPEAT: u32 = 0x8370;

const VERTEX_SHADER: u32 = 0x8B31;
const FRAGMENT_SHADER: u32 = 0x8B30;
const DELETE_STATUS: u32 = 0x8B80;
const COMPILE_STATUS: u32 = 0x8B81;
const LINK_STATUS: u32 = 0x8B82;
const VALIDATE_STATUS: u32 = 0x8B83;
const ATTACHED_SHADERS: u32 = 0x8B85;
const ACTIVE_UNIFORMS: u32 = 0x8B86;
const ACTIVE_ATTRIBUTES: u32 = 0x8B89;
const SHADER_TYPE: u32 = 0x8B4F;

const NO_ERROR: u32 = 0;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;
const OUT_OF_MEMORY: u32 = 0x0505;

const BUFFER_SIZE: u32 = 0x8764;
const BUFFER_USAGE: u32 = 0x8765;

const VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;
const VERTEX_ATTRIB_ARRAY_SIZE: u32 = 0x8623;
const VERTEX_ATTRIB_ARRAY_STRIDE: u32 = 0x8624;
const VERTEX_ATTRIB_ARRAY_TYPE: u32 = 0x8625;
const VERTEX_ATTRIB_ARRAY_NORMALIZED: u32 = 0x886A;
const VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: u32 = 0x889F;
const CURRENT_VERTEX_ATTRIB: u32 = 0x8626;

const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const FRAMEBUFFER_INCOMPLETE_ATTACHMENT: u32 = 0x8CD6;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const RENDERBUFFER_WIDTH: u32 = 0x8D42;
const RENDERBUFFER_HEIGHT: u32 = 0x8D43;
const RENDERBUFFER_INTERNAL_FORMAT: u32 = 0x8D44;

const FUNC_ADD: u32 = 0x8006;
const FUNC_SUBTRACT: u32 = 0x800A;
const FUNC_REVERSE_SUBTRACT: u32 = 0x800B;
const MIN_EXT: u32 = 0x8007;
const MAX_EXT: u32 = 0x8008;

const MAX_VERTEX_ATTRIBS: usize = 16;
const MAX_TEXTURE_UNITS: usize = 32;

/// How many interpreter steps one whole `raster_gl` may spend. A page authors the shaders, so the
/// only thing standing between `toDataURL()` and a hung tab is a bound; running out is a GL error
/// and an unfinished frame, which is what a driver's watchdog produces for the same input.
const FRAME_FUEL: i64 = 60_000_000;
/// And one shader invocation's share of it, so a single pathological fragment cannot eat the frame.
const INVOCATION_FUEL: i64 = 2_000_000;

/// Multisampling is only worth its 4x cost where the frame is small enough for it to be free in
/// wall-clock terms; above this many pixels `antialias: true` still resolves, just at one sample.
const ANTIALIAS_PIXEL_LIMIT: usize = 1 << 20;

/// The 4x multisample pattern of an `antialias: true` drawing buffer: each sample's slot in the
/// target's 2x2 sample grid and its position in the pixel, in GL's window space (y UP). These are
/// D3D's standard 4x positions, (0.375, 0.125), (0.875, 0.375), (0.125, 0.625), (0.625, 0.875), taken
/// as given in GL's orientation -- measured on the persona's GPU (Chrome 150, ANGLE on an RTX 4060):
/// every pixel of seven slivers, diagonals and a bot-detection script's
/// proof-of-work triangle resolves to the coverage these positions give, and to none of the y-flipped
/// pattern's or a 2x2 grid's.
const SAMPLES_4X: [((usize, usize), (f32, f32)); 4] =
    [((0, 0), (0.375, 0.125)), ((1, 0), (0.875, 0.375)), ((0, 1), (0.125, 0.625)), ((1, 1), (0.625, 0.875))];

/// The rasterizer's sub-pixel grid: a vertex's window position snaps to 1/256 of a pixel before any
/// edge is evaluated (8 bits of sub-pixel precision, the GPU's). 1/16 misplaces four of the measured
/// pixels above; 1/256 and finer place them all.
const SUBPIXEL: f32 = 256.0;

// ── objects ───────────────────────────────────────────────────────────────────────────────────

struct ShaderObj {
    kind: GlShaderKind,
    source: String,
    compiled: Option<Rc<Shader>>,
    log: String,
    /// `false` until `compileShader` runs — `COMPILE_STATUS` on a fresh shader is `false`.
    status: bool,
    deleted: bool,
}

#[derive(Clone)]
struct AttrInfo {
    name: String,
    ty: Type,
    location: u32,
}

#[derive(Clone)]
struct UniInfo {
    name: String,
    ty: Type,
    /// The uniform block it is a member of, `-1` for the default block.
    block: i32,
}

/// One active uniform block of a linked program (WebGL 2): its members' indices into `unis`, its
/// std140 size, the binding `uniformBlockBinding` gave it, and which stages declare it.
#[derive(Clone)]
struct BlockInfo {
    name: String,
    members: Vec<u32>,
    size: u32,
    binding: u32,
    vs: bool,
    fs: bool,
}

/// std140's base alignment and size of `ty` (OpenGL ES 3.0 §2.12.6.4): scalars 4, `vec2` 8, `vec3`
/// and `vec4` 16-aligned, a matrix as an array of its column vectors, an array's stride rounded up
/// to 16, a struct 16-aligned.
fn std140(ty: &Type, structs: &HashMap<String, Vec<(Type, String)>>) -> (u32, u32) {
    let round = |n: u32, a: u32| n.div_ceil(a) * a;
    match ty {
        Type::Bool | Type::Int | Type::UInt | Type::Float => (4, 4),
        Type::Vec(n) | Type::IVec(n) | Type::UVec(n) | Type::BVec(n) => match n {
            2 => (8, 8),
            3 => (16, 12),
            _ => (16, 16),
        },
        Type::Mat(c, _) => (16, 16 * *c as u32),
        Type::Array(inner, n) => {
            let (a, sz) = std140(inner, structs);
            let stride = round(sz, a.max(16));
            (a.max(16), stride * *n as u32)
        }
        Type::Struct(name) => {
            let mut off = 0;
            for (t, _) in structs.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
                let (a, sz) = std140(t, structs);
                off = round(off, a) + sz;
            }
            (16, round(off, 16))
        }
        _ => (4, 4),
    }
}

/// A uniform's std140 offset / array stride / matrix stride inside its block.
fn std140_layout(block: &[(Type, String)], member: &str, structs: &HashMap<String, Vec<(Type, String)>>) -> (i32, i32, i32) {
    let round = |n: u32, a: u32| n.div_ceil(a) * a;
    let mut off = 0;
    for (t, name) in block {
        let (a, sz) = std140(t, structs);
        off = round(off, a);
        if name == member {
            let (arr_stride, elem) = match t {
                Type::Array(inner, _) => {
                    let (ia, isz) = std140(inner, structs);
                    (round(isz, ia.max(16)) as i32, inner.as_ref().clone())
                }
                _ => (0, t.clone()),
            };
            let mat_stride = if matches!(elem, Type::Mat(..)) { 16 } else { 0 };
            return (off as i32, arr_stride, mat_stride);
        }
        off += sz;
    }
    (-1, -1, -1)
}

struct ProgramObj {
    shaders: Vec<u32>,
    vs: Option<Rc<Shader>>,
    fs: Option<Rc<Shader>>,
    attrs: Vec<AttrInfo>,
    unis: Vec<UniInfo>,
    /// The varyings the vertex stage writes and the fragment stage reads, by name.
    varyings: Vec<(String, Type)>,
    /// The ES 3.00 `out` variable a fragment shader writes instead of `gl_FragColor`.
    frag_out: Option<String>,
    /// Every ES 3.00 fragment `out`, in declaration order -- `getFragDataLocation` answers its position.
    frag_outs: Vec<String>,
    /// The active uniform blocks (WebGL 2).
    blocks: Vec<BlockInfo>,
    /// What `transformFeedbackVaryings` asked for, applied at the next link.
    tf_pending: (Vec<String>, u32),
    /// The linked transform feedback varyings and their buffer mode.
    tf: Vec<(String, Type)>,
    tf_mode: u32,
    /// Locations the page requested with `bindAttribLocation`, honoured at the next link.
    bound_locs: HashMap<String, u32>,
    values: HashMap<String, Val>,
    status: bool,
    validated: bool,
    log: String,
    deleted: bool,
}

impl ProgramObj {
    fn new() -> Self {
        Self {
            shaders: Vec::new(),
            vs: None,
            fs: None,
            attrs: Vec::new(),
            unis: Vec::new(),
            varyings: Vec::new(),
            frag_out: None,
            frag_outs: Vec::new(),
            blocks: Vec::new(),
            tf_pending: (Vec::new(), 0x8C8C),
            tf: Vec::new(),
            tf_mode: 0x8C8C,
            bound_locs: HashMap::new(),
            values: HashMap::new(),
            status: false,
            validated: false,
            log: String::new(),
            deleted: false,
        }
    }
}

#[derive(Clone, Default)]
struct TexImage {
    width: u32,
    height: u32,
    /// RGBA8, row 0 = the first row `texImage2D` was handed (GL's t=0 edge).
    data: Vec<u8>,
}

#[derive(Clone)]
struct TextureObj {
    /// `TEXTURE_2D` level 0, and the six cube faces keyed by their face enum.
    images: HashMap<u32, TexImage>,
    min_filter: u32,
    mag_filter: u32,
    wrap_s: u32,
    wrap_t: u32,
    has_mipmaps: bool,
}

impl Default for TextureObj {
    fn default() -> Self {
        Self {
            images: HashMap::new(),
            min_filter: NEAREST_MIPMAP_LINEAR,
            mag_filter: LINEAR,
            wrap_s: REPEAT,
            wrap_t: REPEAT,
            has_mipmaps: false,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct AttribState {
    enabled: bool,
    buffer: u32,
    size: u32,
    ty: u32,
    normalized: bool,
    stride: u32,
    offset: u32,
    divisor: u32,
    constant: [f32; 4],
    pointer_set: bool,
}

#[derive(Clone, Copy)]
struct BlendState {
    src_rgb: u32,
    dst_rgb: u32,
    src_a: u32,
    dst_a: u32,
    eq_rgb: u32,
    eq_a: u32,
    color: [f32; 4],
}

impl Default for BlendState {
    fn default() -> Self {
        Self { src_rgb: 1, dst_rgb: 0, src_a: 1, dst_a: 0, eq_rgb: FUNC_ADD, eq_a: FUNC_ADD, color: [0.0; 4] }
    }
}

#[derive(Clone, Default)]
struct RenderbufferObj {
    width: u32,
    height: u32,
    format: u32,
}

#[derive(Default)]
struct FramebufferObj {
    /// `COLOR_ATTACHMENT0`: (texture id, cube face or `TEXTURE_2D`).
    color_tex: Option<(u32, u32)>,
    color_rb: Option<u32>,
}

/// One render target: a float colour buffer plus depth, at `ss` samples per axis. Row 0 is the
/// BOTTOM row, which is GL's own orientation — the flip to [`Rgba8`]'s top-left origin happens once,
/// at readback, instead of in every draw.
struct Target {
    w: usize,
    h: usize,
    ss: usize,
    color: Vec<[f32; 4]>,
    depth: Vec<f32>,
}

impl Target {
    fn new(w: usize, h: usize, ss: usize) -> Self {
        Self { w, h, ss, color: vec![[0.0; 4]; w * ss * h * ss], depth: vec![1.0; w * ss * h * ss] }
    }
    fn sw(&self) -> usize {
        self.w * self.ss
    }
    fn sh(&self) -> usize {
        self.h * self.ss
    }
}

// ── the machine ───────────────────────────────────────────────────────────────────────────────

pub struct GlMachine {
    attrs_cfg: GlContextAttributes,

    buffers: HashMap<u32, (Vec<u8>, u32)>,
    shaders: HashMap<u32, ShaderObj>,
    programs: HashMap<u32, ProgramObj>,
    textures: HashMap<u32, TextureObj>,
    framebuffers: HashMap<u32, FramebufferObj>,
    renderbuffers: HashMap<u32, RenderbufferObj>,
    vertex_arrays: HashSet<u32>,

    array_buffer: u32,
    element_buffer: u32,
    cur_program: u32,
    cur_framebuffer: u32,
    cur_renderbuffer: u32,
    cur_vertex_array: u32,
    active_unit: usize,
    /// Per unit: the `TEXTURE_2D` and `TEXTURE_CUBE_MAP` bindings.
    units: [(u32, u32); MAX_TEXTURE_UNITS],
    attribs: [AttribState; MAX_VERTEX_ATTRIBS],

    clear_color: [f32; 4],
    clear_depth: f32,
    clear_stencil: i32,
    color_mask: [bool; 4],
    depth_mask: bool,
    depth_func: u32,
    depth_range: (f32, f32),
    stencil_mask: u32,
    viewport: (i32, i32, i32, i32),
    scissor: (i32, i32, i32, i32),
    caps: HashSet<u32>,
    blend: BlendState,
    cull: u32,
    front_face: u32,
    line_width: f32,
    polygon_offset: (f32, f32),
    pixel_store: HashMap<u32, i32>,
    hints: HashMap<u32, u32>,

    target: Target,
    /// Offscreen targets for framebuffer objects, created on first draw into one.
    fbo_targets: HashMap<u32, Target>,

    /// GL's error flags, in the order they were raised. A queue rather than one slot because
    /// `getError` is read-and-clear and the recorder replays the whole list on every call, so it
    /// carries a cursor into this (see `GlQuery::Error`).
    errors: Vec<u32>,
    fuel: i64,
    /// Set when a draw could not finish (out of fuel, or an unsupported construct). It is a real GL
    /// error, not a silent black frame.
    pub aborted: Option<String>,
}

impl GlMachine {
    pub fn new(width: u32, height: u32, attrs: &GlContextAttributes) -> Self {
        let (w, h) = (width.max(1) as usize, height.max(1) as usize);
        let ss = if attrs.antialias && w * h <= ANTIALIAS_PIXEL_LIMIT { 2 } else { 1 };
        let mut caps = HashSet::new();
        // GL's own defaults: DITHER on, everything else off.
        caps.insert(DITHER);
        Self {
            attrs_cfg: attrs.clone(),
            buffers: HashMap::new(),
            shaders: HashMap::new(),
            programs: HashMap::new(),
            textures: HashMap::new(),
            framebuffers: HashMap::new(),
            renderbuffers: HashMap::new(),
            vertex_arrays: HashSet::new(),
            array_buffer: 0,
            element_buffer: 0,
            cur_program: 0,
            cur_framebuffer: 0,
            cur_renderbuffer: 0,
            cur_vertex_array: 0,
            active_unit: 0,
            units: [(0, 0); MAX_TEXTURE_UNITS],
            attribs: [AttribState { constant: [0.0, 0.0, 0.0, 1.0], ..Default::default() }; MAX_VERTEX_ATTRIBS],
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            color_mask: [true; 4],
            depth_mask: true,
            depth_func: LESS,
            depth_range: (0.0, 1.0),
            stencil_mask: 0xFFFF_FFFF,
            viewport: (0, 0, w as i32, h as i32),
            scissor: (0, 0, w as i32, h as i32),
            caps,
            blend: BlendState::default(),
            cull: BACK,
            front_face: 0x0901, // CCW
            line_width: 1.0,
            polygon_offset: (0.0, 0.0),
            pixel_store: HashMap::new(),
            hints: HashMap::new(),
            target: Target::new(w, h, ss),
            fbo_targets: HashMap::new(),
            errors: Vec::new(),
            fuel: FRAME_FUEL,
            aborted: None,
        }
    }

    fn fail(&mut self, e: u32) {
        // A cap, because a draw call that fails per fragment would otherwise grow this without
        // bound; GL's own flag set is small for the same reason.
        if self.errors.len() < 64 {
            self.errors.push(e);
        }
    }

    /// Run the whole list.
    pub fn run(&mut self, ops: &[GlOp]) {
        for op in ops {
            self.exec(op);
        }
    }

    /// Run the list, but skip every draw and clear — what a synchronous query needs. Compiling and
    /// linking still happen (that is what the query is usually about); rasterizing does not, because
    /// `getUniformLocation` in a render loop would otherwise re-render every frame ever drawn.
    pub fn run_metadata_only(&mut self, ops: &[GlOp]) {
        for op in ops {
            match op {
                GlOp::DrawArrays { .. }
                | GlOp::DrawElements { .. }
                | GlOp::DrawArraysInstanced { .. }
                | GlOp::DrawElementsInstanced { .. }
                | GlOp::Clear { .. }
                | GlOp::ResizeDrawingBuffer { .. } => {}
                other => self.exec(other),
            }
        }
    }

    fn exec(&mut self, op: &GlOp) {
        match op {
            // --- lifecycle ---
            GlOp::CreateBuffer { id } => {
                self.buffers.insert(*id, (Vec::new(), 0));
            }
            GlOp::DeleteBuffer { id } => {
                self.buffers.remove(id);
                if self.array_buffer == *id {
                    self.array_buffer = 0;
                }
                if self.element_buffer == *id {
                    self.element_buffer = 0;
                }
            }
            GlOp::CreateShader { id, kind } => {
                self.shaders.insert(
                    *id,
                    ShaderObj {
                        kind: *kind,
                        source: String::new(),
                        compiled: None,
                        log: String::new(),
                        status: false,
                        deleted: false,
                    },
                );
            }
            GlOp::DeleteShader { id } => {
                if let Some(s) = self.shaders.get_mut(id) {
                    s.deleted = true;
                }
            }
            GlOp::ShaderSource { shader, source } => {
                if let Some(s) = self.shaders.get_mut(shader) {
                    s.source = source.clone();
                    s.compiled = None;
                    s.status = false;
                } else {
                    self.fail(INVALID_VALUE);
                }
            }
            GlOp::CompileShader { shader } => {
                let Some(s) = self.shaders.get_mut(shader) else {
                    self.fail(INVALID_VALUE);
                    return;
                };
                match glsl::compile(&s.source) {
                    Ok(sh) => {
                        s.compiled = Some(Rc::new(sh));
                        s.status = true;
                        s.log.clear();
                    }
                    Err(e) => {
                        s.compiled = None;
                        s.status = false;
                        s.log = e;
                    }
                }
            }
            GlOp::CreateProgram { id } => {
                self.programs.insert(*id, ProgramObj::new());
            }
            GlOp::DeleteProgram { id } => {
                if let Some(p) = self.programs.get_mut(id) {
                    p.deleted = true;
                }
                if self.cur_program == *id {
                    self.cur_program = 0;
                }
            }
            GlOp::AttachShader { program, shader } => {
                if let Some(p) = self.programs.get_mut(program) {
                    if !p.shaders.contains(shader) {
                        p.shaders.push(*shader);
                    }
                } else {
                    self.fail(INVALID_VALUE);
                }
            }
            GlOp::DetachShader { program, shader } => {
                if let Some(p) = self.programs.get_mut(program) {
                    p.shaders.retain(|s| s != shader);
                }
            }
            GlOp::BindAttribLocation { program, index, name } => {
                if let Some(p) = self.programs.get_mut(program) {
                    p.bound_locs.insert(name.clone(), *index);
                }
            }
            GlOp::LinkProgram { program } => self.link(*program),
            GlOp::ValidateProgram { program } => {
                if let Some(p) = self.programs.get_mut(program) {
                    p.validated = p.status;
                }
            }
            GlOp::UseProgram { program } => {
                if *program == 0 || self.programs.contains_key(program) {
                    self.cur_program = *program;
                } else {
                    self.fail(INVALID_OPERATION);
                }
            }
            GlOp::CreateTexture { id } => {
                self.textures.insert(*id, TextureObj::default());
            }
            GlOp::DeleteTexture { id } => {
                self.textures.remove(id);
                for u in self.units.iter_mut() {
                    if u.0 == *id {
                        u.0 = 0;
                    }
                    if u.1 == *id {
                        u.1 = 0;
                    }
                }
            }
            GlOp::CreateFramebuffer { id } => {
                self.framebuffers.insert(*id, FramebufferObj::default());
            }
            GlOp::DeleteFramebuffer { id } => {
                self.framebuffers.remove(id);
                self.fbo_targets.remove(id);
                if self.cur_framebuffer == *id {
                    self.cur_framebuffer = 0;
                }
            }
            GlOp::CreateRenderbuffer { id } => {
                self.renderbuffers.insert(*id, RenderbufferObj::default());
            }
            GlOp::DeleteRenderbuffer { id } => {
                self.renderbuffers.remove(id);
                if self.cur_renderbuffer == *id {
                    self.cur_renderbuffer = 0;
                }
            }
            GlOp::CreateVertexArray { id } => {
                self.vertex_arrays.insert(*id);
            }
            GlOp::DeleteVertexArray { id } => {
                self.vertex_arrays.remove(id);
                if self.cur_vertex_array == *id {
                    self.cur_vertex_array = 0;
                }
            }
            GlOp::BindVertexArray { id } => self.cur_vertex_array = *id,

            // --- buffers ---
            GlOp::BindBuffer { target, buffer } => match *target {
                ARRAY_BUFFER => self.array_buffer = *buffer,
                ELEMENT_ARRAY_BUFFER => self.element_buffer = *buffer,
                // a WebGL 2 binding point: no data this pipeline reads goes through it
                t if WEBGL2_BUFFER_TARGETS.contains(&t) => {}
                _ => self.fail(INVALID_ENUM),
            },
            // a WebGL 2 binding point's buffer: no data this pipeline reads
            GlOp::BufferSubData { target, .. } | GlOp::BufferData { target, .. }
                if WEBGL2_BUFFER_TARGETS.contains(target) => {}
            GlOp::BufferData { target, size, data, usage } => {
                let id = self.buffer_for(*target);
                if id == 0 {
                    self.fail(INVALID_OPERATION);
                    return;
                }
                let mut bytes = data.clone();
                if bytes.is_empty() && *size > 0 {
                    if *size > (1 << 28) {
                        self.fail(OUT_OF_MEMORY);
                        return;
                    }
                    bytes = vec![0u8; *size];
                }
                self.buffers.insert(id, (bytes, *usage));
            }
            GlOp::BufferSubData { target, offset, data } => {
                let id = self.buffer_for(*target);
                let Some((buf, _)) = self.buffers.get_mut(&id) else {
                    self.fail(INVALID_OPERATION);
                    return;
                };
                if offset + data.len() > buf.len() {
                    self.fail(INVALID_VALUE);
                    return;
                }
                buf[*offset..*offset + data.len()].copy_from_slice(data);
            }

            // --- vertex state ---
            GlOp::EnableVertexAttribArray { index } => {
                if let Some(a) = self.attribs.get_mut(*index as usize) {
                    a.enabled = true;
                } else {
                    self.fail(INVALID_VALUE);
                }
            }
            GlOp::DisableVertexAttribArray { index } => {
                if let Some(a) = self.attribs.get_mut(*index as usize) {
                    a.enabled = false;
                }
            }
            GlOp::VertexAttribPointer { index, size, ty, normalized, stride, offset } => {
                let buf = self.array_buffer;
                let Some(a) = self.attribs.get_mut(*index as usize) else {
                    self.fail(INVALID_VALUE);
                    return;
                };
                // GL captures the ARRAY_BUFFER binding AT THIS CALL; rebinding later does not move
                // the attribute. Pages rely on that to interleave several buffers.
                a.buffer = buf;
                a.size = *size;
                a.ty = *ty;
                a.normalized = *normalized;
                a.stride = *stride;
                a.offset = *offset;
                a.pointer_set = true;
            }
            GlOp::VertexAttrib { index, values } => {
                if let Some(a) = self.attribs.get_mut(*index as usize) {
                    a.constant = *values;
                }
            }
            GlOp::VertexAttribDivisor { index, divisor } => {
                if let Some(a) = self.attribs.get_mut(*index as usize) {
                    a.divisor = *divisor;
                }
            }

            // --- uniforms ---
            GlOp::TransformFeedbackVaryings { program, varyings, mode } => {
                if let Some(p) = self.programs.get_mut(program) {
                    p.tf_pending = (varyings.clone(), *mode);
                }
            }
            GlOp::UniformBlockBinding { program, index, binding } => {
                if let Some(b) = self.programs.get_mut(program).and_then(|p| p.blocks.get_mut(*index as usize)) {
                    b.binding = *binding;
                }
            }
            GlOp::Uniform { program, name, value } => {
                let Some(p) = self.programs.get(program) else {
                    self.fail(INVALID_OPERATION);
                    return;
                };
                let ty = p.unis.iter().find(|u| u.name == *name).map(|u| u.ty.clone());
                let base = base_name(name);
                let arr_ty = p.unis.iter().find(|u| u.name == base).map(|u| u.ty.clone());
                let v = uniform_val(value);
                let Some(p) = self.programs.get_mut(program) else { return };
                match (ty, arr_ty) {
                    (Some(t), _) => {
                        // A whole-array set: `uniform3fv(loc, [9 floats])` on `vec3 u[3]`.
                        if let Type::Array(inner, n) = &t {
                            let per = inner.components().max(1);
                            let f = v.floats();
                            let items: Vec<Val> = (0..*n)
                                .map(|i| {
                                    let slice: Vec<f32> =
                                        f.iter().skip(i * per).take(per).copied().collect();
                                    interp::coerce(&Val::F(slice), inner)
                                })
                                .collect();
                            p.values.insert(name.clone(), Val::Arr(items));
                        } else {
                            p.values.insert(name.clone(), interp::coerce(&v, &t));
                        }
                    }
                    // `u[1]` — an element of a declared array.
                    (None, Some(Type::Array(inner, n))) => {
                        let idx = element_index(name).unwrap_or(0);
                        let entry = p
                            .values
                            .entry(base.to_string())
                            .or_insert_with(|| Val::Arr(vec![Val::F(vec![0.0; inner.components()]); n]));
                        if let Val::Arr(items) = entry {
                            if idx < items.len() {
                                items[idx] = interp::coerce(&v, &inner);
                            }
                        }
                    }
                    _ => {
                        // A uniform the linked program does not declare. WebGL makes this a silent
                        // no-op (the location would have been null), not an error.
                    }
                }
            }

            // --- textures ---
            GlOp::ActiveTexture { unit } => {
                // The page passes TEXTURE0 + n.
                let n = unit.wrapping_sub(0x84C0) as usize;
                if n < MAX_TEXTURE_UNITS {
                    self.active_unit = n;
                } else {
                    self.fail(INVALID_ENUM);
                }
            }
            GlOp::BindTexture { target, texture } => {
                let u = self.active_unit;
                match *target {
                    TEXTURE_2D => self.units[u].0 = *texture,
                    TEXTURE_CUBE_MAP => self.units[u].1 = *texture,
                    // TEXTURE_3D / TEXTURE_2D_ARRAY: never sampled here
                    0x806F | 0x8C1A => {}
                    _ => self.fail(INVALID_ENUM),
                }
                if *texture != 0 {
                    self.textures.entry(*texture).or_default();
                }
            }
            GlOp::TexImage2D { target, level, width, height, pixels, .. } => {
                if *level != 0 {
                    // Only level 0 is stored: this rasterizer samples one level (see `sample_2d`),
                    // so keeping the rest would be storage that never affects a pixel.
                    return;
                }
                let Some(id) = self.bound_texture(*target) else {
                    self.fail(INVALID_OPERATION);
                    return;
                };
                let img = match pixels {
                    Some(p) => TexImage { width: p.width, height: p.height, data: p.data.clone() },
                    None => TexImage {
                        width: *width,
                        height: *height,
                        data: vec![0u8; (*width as usize) * (*height as usize) * 4],
                    },
                };
                let key = if *target == TEXTURE_2D { TEXTURE_2D } else { *target };
                if let Some(t) = self.textures.get_mut(&id) {
                    t.images.insert(key, img);
                }
            }
            GlOp::TexStorage { target, width, height, depth: 0, .. } => {
                // level 0 of each face, cleared (the only level this rasterizer samples)
                let Some(id) = self.bound_texture(*target) else {
                    self.fail(INVALID_OPERATION);
                    return;
                };
                let (w, h) = ((*width).max(0) as u32, (*height).max(0) as u32);
                let faces: Vec<u32> = if *target == TEXTURE_2D { vec![TEXTURE_2D] } else { (0x8515..=0x851A).collect() };
                if let Some(t) = self.textures.get_mut(&id) {
                    for f in faces {
                        t.images.insert(f, TexImage { width: w, height: h, data: vec![0u8; (w as usize) * (h as usize) * 4] });
                    }
                }
            }
            // 3D and array textures are not this ES 2.0 pipeline's
            GlOp::TexStorage { .. } => {}
            GlOp::TexSubImage2D { target, level, xoffset, yoffset, pixels, .. } => {
                if *level != 0 {
                    return;
                }
                let Some(id) = self.bound_texture(*target) else {
                    self.fail(INVALID_OPERATION);
                    return;
                };
                let key = if *target == TEXTURE_2D { TEXTURE_2D } else { *target };
                if let Some(t) = self.textures.get_mut(&id) {
                    if let Some(dst) = t.images.get_mut(&key) {
                        for row in 0..pixels.height as usize {
                            for col in 0..pixels.width as usize {
                                let (dx, dy) = (*xoffset as usize + col, *yoffset as usize + row);
                                if dx >= dst.width as usize || dy >= dst.height as usize {
                                    continue;
                                }
                                let s = (row * pixels.width as usize + col) * 4;
                                let d = (dy * dst.width as usize + dx) * 4;
                                dst.data[d..d + 4].copy_from_slice(&pixels.data[s..s + 4]);
                            }
                        }
                    }
                }
            }
            GlOp::TexParameter { target, pname, param } => {
                let Some(id) = self.bound_texture(*target) else {
                    self.fail(INVALID_OPERATION);
                    return;
                };
                let v = *param as u32;
                if let Some(t) = self.textures.get_mut(&id) {
                    match *pname {
                        TEXTURE_MIN_FILTER => t.min_filter = v,
                        TEXTURE_MAG_FILTER => t.mag_filter = v,
                        TEXTURE_WRAP_S => t.wrap_s = v,
                        TEXTURE_WRAP_T => t.wrap_t = v,
                        // TEXTURE_MAX_ANISOTROPY etc. are accepted and stored nowhere, because no
                        // sampling decision here reads them.
                        _ => {}
                    }
                }
            }
            GlOp::GenerateMipmap { target } => {
                if let Some(id) = self.bound_texture(*target) {
                    if let Some(t) = self.textures.get_mut(&id) {
                        t.has_mipmaps = true;
                    }
                }
            }
            GlOp::PixelStorei { pname, param } => {
                self.pixel_store.insert(*pname, *param);
            }

            // --- framebuffers ---
            // READ_FRAMEBUFFER alone does not change where this pipeline draws, which is all its binding means
            GlOp::BindFramebuffer { target: 0x8CA8, .. } => {}
            GlOp::BindFramebuffer { framebuffer, .. } => {
                if *framebuffer == 0 || self.framebuffers.contains_key(framebuffer) {
                    self.cur_framebuffer = *framebuffer;
                } else {
                    self.fail(INVALID_OPERATION);
                }
            }
            GlOp::BindRenderbuffer { renderbuffer, .. } => self.cur_renderbuffer = *renderbuffer,
            GlOp::RenderbufferStorage { internal_format, width, height, .. } => {
                let id = self.cur_renderbuffer;
                if id == 0 {
                    self.fail(INVALID_OPERATION);
                    return;
                }
                self.renderbuffers
                    .insert(id, RenderbufferObj { width: *width, height: *height, format: *internal_format });
            }
            GlOp::FramebufferTexture2D { attachment, textarget, texture, .. } => {
                let fb = self.cur_framebuffer;
                if let Some(f) = self.framebuffers.get_mut(&fb) {
                    if *attachment == COLOR_ATTACHMENT0 {
                        f.color_tex = (*texture != 0).then_some((*texture, *textarget));
                    }
                } else {
                    self.fail(INVALID_OPERATION);
                }
            }
            GlOp::FramebufferRenderbuffer { attachment, renderbuffer, .. } => {
                let fb = self.cur_framebuffer;
                if let Some(f) = self.framebuffers.get_mut(&fb) {
                    if *attachment == COLOR_ATTACHMENT0 {
                        f.color_rb = (*renderbuffer != 0).then_some(*renderbuffer);
                    }
                }
            }

            // --- fixed-function state ---
            GlOp::Viewport { x, y, width, height } => {
                if *width < 0 || *height < 0 {
                    self.fail(INVALID_VALUE);
                    return;
                }
                self.viewport = (*x, *y, *width, *height);
            }
            GlOp::Scissor { x, y, width, height } => {
                if *width < 0 || *height < 0 {
                    self.fail(INVALID_VALUE);
                    return;
                }
                self.scissor = (*x, *y, *width, *height);
            }
            GlOp::ClearColor { r, g, b, a } => self.clear_color = [*r, *g, *b, *a],
            GlOp::ClearDepth { depth } => self.clear_depth = depth.clamp(0.0, 1.0),
            GlOp::ClearStencil { s } => self.clear_stencil = *s,
            GlOp::Clear { mask } => self.clear(*mask),
            GlOp::ColorMask { r, g, b, a } => self.color_mask = [*r, *g, *b, *a],
            GlOp::DepthMask { flag } => self.depth_mask = *flag,
            GlOp::DepthFunc { func } => self.depth_func = *func,
            GlOp::DepthRange { near, far } => self.depth_range = (near.clamp(0.0, 1.0), far.clamp(0.0, 1.0)),
            GlOp::StencilMask { mask } => self.stencil_mask = *mask,
            GlOp::StencilFunc { .. } | GlOp::StencilOp { .. } => {
                // Recorded and replayed into state, but no stencil buffer is allocated (the default
                // context attributes ask for none) and no draw consults one. Recording it without
                // executing it would be the display-list failure this whole seam exists to avoid, so
                // this arm says so out loud rather than pretending.
            }
            GlOp::Enable { cap } => {
                self.caps.insert(*cap);
            }
            GlOp::Disable { cap } => {
                self.caps.remove(cap);
            }
            GlOp::BlendFuncSeparate { src_rgb, dst_rgb, src_alpha, dst_alpha } => {
                self.blend.src_rgb = *src_rgb;
                self.blend.dst_rgb = *dst_rgb;
                self.blend.src_a = *src_alpha;
                self.blend.dst_a = *dst_alpha;
            }
            GlOp::BlendEquationSeparate { rgb, alpha } => {
                self.blend.eq_rgb = *rgb;
                self.blend.eq_a = *alpha;
            }
            GlOp::BlendColor { r, g, b, a } => self.blend.color = [*r, *g, *b, *a],
            GlOp::CullFace { mode } => self.cull = *mode,
            GlOp::FrontFace { mode } => self.front_face = *mode,
            GlOp::LineWidth { width } => self.line_width = width.max(0.0),
            GlOp::PolygonOffset { factor, units } => self.polygon_offset = (*factor, *units),
            GlOp::SampleCoverage { .. } => {}
            GlOp::Hint { target, mode } => {
                self.hints.insert(*target, *mode);
            }
            // the software pipeline has every extension's formats and enums: there is nothing to enable
            GlOp::Finish | GlOp::Flush | GlOp::EnableExtension { .. } => {}
            GlOp::RaiseError { code } => self.fail(*code),
            GlOp::ResizeDrawingBuffer { width, height } => {
                let (w, h) = ((*width).max(1) as usize, (*height).max(1) as usize);
                let ss = if self.attrs_cfg.antialias && w * h <= ANTIALIAS_PIXEL_LIMIT { 2 } else { 1 };
                self.target = Target::new(w, h, ss);
            }

            // --- draw ---
            GlOp::DrawArrays { mode, first, count } => self.draw_arrays(*mode, *first, *count, 1),
            GlOp::DrawArraysInstanced { mode, first, count, instances } => {
                self.draw_arrays(*mode, *first, *count, *instances)
            }
            GlOp::DrawElements { mode, count, ty, offset } => {
                self.draw_elements(*mode, *count, *ty, *offset, 1)
            }
            GlOp::DrawElementsInstanced { mode, count, ty, offset, instances } => {
                self.draw_elements(*mode, *count, *ty, *offset, *instances)
            }
        }
    }

    fn buffer_for(&self, target: u32) -> u32 {
        match target {
            ELEMENT_ARRAY_BUFFER => self.element_buffer,
            _ => self.array_buffer,
        }
    }

    fn bound_texture(&self, target: u32) -> Option<u32> {
        let u = self.units[self.active_unit];
        let id = if target == TEXTURE_2D { u.0 } else { u.1 };
        (id != 0).then_some(id)
    }

    // ── linking ───────────────────────────────────────────────────────────────────────────────

    fn link(&mut self, program: u32) {
        let shader_ids = match self.programs.get(&program) {
            Some(p) => p.shaders.clone(),
            None => {
                self.fail(INVALID_VALUE);
                return;
            }
        };
        let mut vs = None;
        let mut fs = None;
        for id in &shader_ids {
            if let Some(s) = self.shaders.get(id) {
                if !s.status {
                    continue;
                }
                match s.kind {
                    GlShaderKind::Vertex => vs = s.compiled.clone(),
                    GlShaderKind::Fragment => fs = s.compiled.clone(),
                }
            }
        }
        let bound = self.programs.get(&program).map(|p| p.bound_locs.clone()).unwrap_or_default();
        let Some(p) = self.programs.get_mut(&program) else { return };
        let (Some(v), Some(f)) = (vs, fs) else {
            p.status = false;
            p.vs = None;
            p.fs = None;
            p.log = "ERROR: a linked program needs one compiled vertex shader and one compiled fragment shader"
                .to_string();
            return;
        };

        // Attributes: the vertex shader's `attribute` (ES1) or `in` (ES3) globals. Explicitly bound
        // locations win; the rest fill the lowest free slots in declaration order, which is what a
        // driver does and what a page that never calls `bindAttribLocation` depends on.
        let mut attrs: Vec<AttrInfo> = Vec::new();
        let mut used: HashSet<u32> = bound.values().copied().collect();
        let mut pending: Vec<(String, Type)> = Vec::new();
        for d in &v.globals {
            let is_attr = d.qual == Qual::Attribute || (v.es3 && d.qual == Qual::In);
            if !is_attr {
                continue;
            }
            match bound.get(&d.name) {
                Some(loc) => attrs.push(AttrInfo { name: d.name.clone(), ty: d.ty.clone(), location: *loc }),
                None => pending.push((d.name.clone(), d.ty.clone())),
            }
        }
        let mut next = 0u32;
        for (name, ty) in pending {
            while used.contains(&next) {
                next += 1;
            }
            used.insert(next);
            attrs.push(AttrInfo { name, ty, location: next });
        }
        attrs.sort_by_key(|a| a.location);

        // Uniforms: from both stages, de-duplicated by name (a uniform declared in both is one
        // uniform, which is why a page can set it once and have both stages see it).
        let mut unis: Vec<UniInfo> = Vec::new();
        // The uniform blocks, from both stages (one block declared in both is one block).
        let mut blocks: Vec<BlockInfo> = Vec::new();
        let mut block_decls: Vec<Vec<(Type, String)>> = Vec::new();
        for (stage, sh) in [&v, &f].into_iter().enumerate() {
            for (name, members) in &sh.blocks {
                let i = match blocks.iter().position(|b| b.name == *name) {
                    Some(i) => i,
                    None => {
                        let mut off = 0u32;
                        for (t, _) in members {
                            let (a, sz) = std140(t, &sh.structs);
                            off = off.div_ceil(a) * a + sz;
                        }
                        blocks.push(BlockInfo { name: name.clone(), members: Vec::new(), size: off.div_ceil(16) * 16, binding: 0, vs: false, fs: false });
                        block_decls.push(members.clone());
                        blocks.len() - 1
                    }
                };
                if stage == 0 {
                    blocks[i].vs = true;
                } else {
                    blocks[i].fs = true;
                }
            }
        }
        for sh in [&v, &f] {
            for d in &sh.globals {
                if d.qual == Qual::Uniform && !unis.iter().any(|u| u.name == d.name) {
                    let block = block_decls.iter().position(|m| m.iter().any(|(_, n)| *n == d.name)).map(|i| i as i32).unwrap_or(-1);
                    if block >= 0 {
                        blocks[block as usize].members.push(unis.len() as u32);
                    }
                    unis.push(UniInfo { name: d.name.clone(), ty: d.ty.clone(), block });
                }
            }
        }
        // Transform feedback: every requested varying must be one the vertex stage writes (or a
        // built-in it can), else the link fails (OpenGL ES 3.0 §2.15.1).
        let (tf_names, tf_mode) = p.tf_pending.clone();
        let mut tf: Vec<(String, Type)> = Vec::new();
        for n in &tf_names {
            let found = v.globals.iter().find(|d| d.name == *n && (d.qual == Qual::Varying || (v.es3 && d.qual == Qual::Out))).map(|d| d.ty.clone());
            let found = found.or_else(|| match n.as_str() {
                "gl_Position" => Some(Type::Vec(4)),
                "gl_PointSize" => Some(Type::Float),
                _ => None,
            });
            match found {
                Some(t) => tf.push((n.clone(), t)),
                None => {
                    p.status = false;
                    p.log = format!("ERROR: Transform feedback varying {n} does not exist in the vertex shader.");
                    return;
                }
            }
        }

        let mut varyings: Vec<(String, Type)> = Vec::new();
        for d in &v.globals {
            let is_varying = d.qual == Qual::Varying || (v.es3 && d.qual == Qual::Out);
            if is_varying {
                varyings.push((d.name.clone(), d.ty.clone()));
            }
        }
        let frag_out = f
            .globals
            .iter()
            .find(|d| d.qual == Qual::Out && !d.name.starts_with("gl_"))
            .map(|d| d.name.clone());
        let frag_outs: Vec<String> = f.globals.iter().filter(|d| f.es3 && d.qual == Qual::Out && !d.name.starts_with("gl_")).map(|d| d.name.clone()).collect();

        p.vs = Some(v);
        p.fs = Some(f);
        p.attrs = attrs;
        p.unis = unis;
        p.varyings = varyings;
        p.frag_out = frag_out;
        p.frag_outs = frag_outs;
        p.blocks = blocks;
        p.tf = tf;
        p.tf_mode = tf_mode;
        p.status = true;
        p.log.clear();
    }

    // ── clear ─────────────────────────────────────────────────────────────────────────────────

    fn clear(&mut self, mask: u32) {
        let (mut cc, cd, cm, dm) = (self.clear_color, self.clear_depth, self.color_mask, self.depth_mask);
        // A context created with `alpha: false` has no alpha channel: the drawing buffer reads back
        // opaque whatever the page cleared to. `shade` enforces the same rule for drawn fragments.
        if !self.attrs_cfg.alpha {
            cc[3] = 1.0;
        }
        // The drawing buffer holds 8 bits a channel: a clear to 0.5 leaves 127 there, which a blend
        // reads back (see `write_color`).
        if self.cur_framebuffer == 0 {
            cc = cc.map(stored);
        }
        let scissor_on = self.caps.contains(&SCISSOR_TEST);
        let sc = self.scissor;
        let t = self.current_target_mut();
        let ss = t.ss;
        let (sw, sh) = (t.w * ss, t.h * ss);
        // A clear is subject to the scissor box and the colour/depth write masks, exactly like a
        // draw. A page that scissors a strip and clears twice in two colours can see the difference.
        for y in 0..sh {
            for x in 0..sw {
                if scissor_on {
                    let (px, py) = (x as i32 / ss as i32, y as i32 / ss as i32);
                    if px < sc.0 || py < sc.1 || px >= sc.0 + sc.2 || py >= sc.1 + sc.3 {
                        continue;
                    }
                }
                let i = y * sw + x;
                if mask & COLOR_BUFFER_BIT != 0 {
                    for c in 0..4 {
                        if cm[c] {
                            t.color[i][c] = cc[c];
                        }
                    }
                }
                if mask & DEPTH_BUFFER_BIT != 0 && dm {
                    t.depth[i] = cd;
                }
            }
        }
        let _ = STENCIL_BUFFER_BIT;
        self.sync_fbo_texture();
    }

    fn current_target_mut(&mut self) -> &mut Target {
        let fb = self.cur_framebuffer;
        if fb == 0 {
            return &mut self.target;
        }
        // An FBO's target is the size of its colour attachment. Created on first use so an
        // unattached framebuffer costs nothing.
        if !self.fbo_targets.contains_key(&fb) {
            let (w, h) = self.fbo_size(fb);
            self.fbo_targets.insert(fb, Target::new(w.max(1), h.max(1), 1));
        }
        self.fbo_targets.get_mut(&fb).expect("just inserted")
    }

    fn fbo_size(&self, fb: u32) -> (usize, usize) {
        let Some(f) = self.framebuffers.get(&fb) else { return (1, 1) };
        if let Some((tex, face)) = f.color_tex {
            let key = if face == TEXTURE_2D { TEXTURE_2D } else { face };
            if let Some(img) = self.textures.get(&tex).and_then(|t| t.images.get(&key)) {
                return (img.width as usize, img.height as usize);
            }
        }
        if let Some(rb) = f.color_rb {
            if let Some(r) = self.renderbuffers.get(&rb) {
                return (r.width as usize, r.height as usize);
            }
        }
        (1, 1)
    }

    /// After anything writes into a framebuffer object whose colour attachment is a texture, push
    /// the pixels back into that texture — so a page that renders to a texture and then samples it
    /// sees what it rendered, which is the whole reason to have FBOs.
    fn sync_fbo_texture(&mut self) {
        let fb = self.cur_framebuffer;
        if fb == 0 {
            return;
        }
        let Some((tex, face)) = self.framebuffers.get(&fb).and_then(|f| f.color_tex) else { return };
        let Some(t) = self.fbo_targets.get(&fb) else { return };
        let (w, h) = (t.w, t.h);
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let px = t.color[y * w * t.ss * t.ss + x * t.ss];
                // Row 0 of the target is the bottom; a texture's row 0 is its t=0 edge, which is the
                // same end, so no flip here.
                let d = (y * w + x) * 4;
                for c in 0..4 {
                    data[d + c] = (px[c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
        let key = if face == TEXTURE_2D { TEXTURE_2D } else { face };
        if let Some(tx) = self.textures.get_mut(&tex) {
            tx.images.insert(key, TexImage { width: w as u32, height: h as u32, data });
        }
    }

    // ── drawing ───────────────────────────────────────────────────────────────────────────────

    fn draw_arrays(&mut self, mode: u32, first: i32, count: i32, instances: i32) {
        if count < 0 || first < 0 {
            self.fail(INVALID_VALUE);
            return;
        }
        let idx: Vec<u32> = (0..count as u32).map(|i| first as u32 + i).collect();
        self.draw(mode, &idx, instances.max(1));
    }

    fn draw_elements(&mut self, mode: u32, count: i32, ty: u32, offset: u32, instances: i32) {
        if count < 0 {
            self.fail(INVALID_VALUE);
            return;
        }
        let Some((buf, _)) = self.buffers.get(&self.element_buffer) else {
            self.fail(INVALID_OPERATION);
            return;
        };
        let stride = match ty {
            UNSIGNED_BYTE => 1usize,
            UNSIGNED_SHORT => 2,
            UNSIGNED_INT => 4,
            _ => {
                self.fail(INVALID_ENUM);
                return;
            }
        };
        let mut idx = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let at = offset as usize + i * stride;
            if at + stride > buf.len() {
                self.fail(INVALID_OPERATION);
                return;
            }
            idx.push(match stride {
                1 => buf[at] as u32,
                2 => u16::from_le_bytes([buf[at], buf[at + 1]]) as u32,
                _ => u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]),
            });
        }
        self.draw(mode, &idx, instances.max(1));
    }

    fn draw(&mut self, mode: u32, indices: &[u32], instances: i32) {
        if self.aborted.is_some() {
            return;
        }
        // Materialize the bound framebuffer's target now: it is sized from the colour attachment's
        // texture, and the texture table is moved out for the duration of the draw below.
        self.current_target_mut();
        let prog_id = self.cur_program;
        let Some(prog) = self.programs.get(&prog_id) else {
            self.fail(INVALID_OPERATION);
            return;
        };
        if !prog.status {
            self.fail(INVALID_OPERATION);
            return;
        }
        let (Some(vs), Some(fs)) = (prog.vs.clone(), prog.fs.clone()) else {
            self.fail(INVALID_OPERATION);
            return;
        };
        let attrs = prog.attrs.clone();
        let unis = prog.values.clone();
        let varyings = prog.varyings.clone();
        let frag_out = prog.frag_out.clone();

        for inst in 0..instances.max(1) {
            // The texture table moves out for the duration of the draw so the fragment stage can
            // read it while the framebuffer is being written; nothing in a draw mutates textures.
            let textures = std::mem::take(&mut self.textures);
            let verts = {
                let env = TexEnv { textures: &textures, units: self.units };
                self.run_vertex_stage(&vs, &attrs, &unis, &varyings, indices, inst, &env)
            };
            self.textures = textures;
            let Some(verts) = verts else { return };

            let textures = std::mem::take(&mut self.textures);
            {
                let env = TexEnv { textures: &textures, units: self.units };
                self.assemble_and_raster(mode, &verts, &fs, &unis, frag_out.as_deref(), &env);
            }
            self.textures = textures;
            if self.aborted.is_some() {
                break;
            }
        }
        self.sync_fbo_texture();
    }

    #[allow(clippy::too_many_arguments)]
    fn run_vertex_stage(
        &mut self,
        vs: &Rc<Shader>,
        attrs: &[AttrInfo],
        unis: &HashMap<String, Val>,
        varyings: &[(String, Type)],
        indices: &[u32],
        instance: i32,
        env: &TexEnv,
    ) -> Option<Vec<VertexOut>> {
        let mut out = Vec::with_capacity(indices.len());
        for &vi in indices {
            let mut it = Interp::new(vs, env, INVOCATION_FUEL.min(self.fuel));
            if it.init_globals().is_err() {
                self.abort("vertex shader failed to initialize");
                return None;
            }
            for (name, v) in unis {
                it.set(name, v.clone());
            }
            for a in attrs {
                let raw = self.fetch_attrib(a.location as usize, vi, instance);
                it.set(&a.name, interp::coerce(&Val::F(raw.to_vec()), &a.ty));
            }
            it.set("gl_VertexID", Val::scalar_i(vi as i32));
            it.set("gl_InstanceID", Val::scalar_i(instance));
            it.set("gl_PointSize", Val::scalar_f(1.0));
            it.set("gl_Position", Val::F(vec![0.0, 0.0, 0.0, 1.0]));
            if let Err(e) = it.run_main() {
                self.abort(&describe_run_error(&e));
                return None;
            }
            self.fuel -= INVOCATION_FUEL.min(self.fuel) - it.fuel_left();
            if self.fuel <= 0 {
                self.abort("the frame ran out of shader budget");
                return None;
            }
            let pos = it.get("gl_Position").map(|v| v.vec4()).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let psize = it.get("gl_PointSize").map(|v| v.as_f()).unwrap_or(1.0);
            let vary = varyings
                .iter()
                .map(|(n, _)| (n.clone(), it.get(n).cloned().unwrap_or(Val::F(vec![0.0]))))
                .collect();
            out.push(VertexOut { pos, point_size: psize, varyings: vary });
        }
        Some(out)
    }

    fn abort(&mut self, why: &str) {
        if self.aborted.is_none() {
            self.aborted = Some(why.to_string());
        }
        self.fail(INVALID_OPERATION);
    }

    /// Read one attribute for one vertex out of its bound buffer (or its constant value).
    fn fetch_attrib(&self, loc: usize, vertex: u32, instance: i32) -> [f32; 4] {
        let Some(a) = self.attribs.get(loc) else { return [0.0, 0.0, 0.0, 1.0] };
        if !a.enabled || !a.pointer_set {
            return a.constant;
        }
        let Some((buf, _)) = self.buffers.get(&a.buffer) else { return a.constant };
        let comp = match a.ty {
            BYTE | UNSIGNED_BYTE => 1usize,
            SHORT | UNSIGNED_SHORT => 2,
            _ => 4,
        };
        let size = (a.size as usize).clamp(1, 4);
        let stride = if a.stride == 0 { comp * size } else { a.stride as usize };
        // An instanced attribute advances once per `divisor` instances, not per vertex.
        let element = if a.divisor > 0 { (instance as u32 / a.divisor) as usize } else { vertex as usize };
        let base = a.offset as usize + element * stride;
        let mut out = [0.0f32, 0.0, 0.0, 1.0];
        for c in 0..size {
            let at = base + c * comp;
            if at + comp > buf.len() {
                return out;
            }
            out[c] = match a.ty {
                FLOAT => f32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]),
                BYTE => {
                    let v = buf[at] as i8 as f32;
                    if a.normalized {
                        (v / 127.0).max(-1.0)
                    } else {
                        v
                    }
                }
                UNSIGNED_BYTE => {
                    let v = buf[at] as f32;
                    if a.normalized {
                        v / 255.0
                    } else {
                        v
                    }
                }
                SHORT => {
                    let v = i16::from_le_bytes([buf[at], buf[at + 1]]) as f32;
                    if a.normalized {
                        (v / 32767.0).max(-1.0)
                    } else {
                        v
                    }
                }
                UNSIGNED_SHORT => {
                    let v = u16::from_le_bytes([buf[at], buf[at + 1]]) as f32;
                    if a.normalized {
                        v / 65535.0
                    } else {
                        v
                    }
                }
                INT => i32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as f32,
                UNSIGNED_INT => u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as f32,
                _ => 0.0,
            };
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble_and_raster(
        &mut self,
        mode: u32,
        v: &[VertexOut],
        fs: &Rc<Shader>,
        unis: &HashMap<String, Val>,
        frag_out: Option<&str>,
        env: &TexEnv,
    ) {
        let ctx = FragCtx { fs: fs.clone(), unis, frag_out, env };
        match mode {
            POINTS => {
                for p in v {
                    self.raster_point(p, &ctx);
                }
            }
            LINES => {
                for c in v.chunks(2) {
                    if c.len() == 2 {
                        self.raster_line(&c[0], &c[1], &ctx);
                    }
                }
            }
            LINE_STRIP => {
                for w in v.windows(2) {
                    self.raster_line(&w[0], &w[1], &ctx);
                }
            }
            LINE_LOOP => {
                for w in v.windows(2) {
                    self.raster_line(&w[0], &w[1], &ctx);
                }
                if v.len() > 2 {
                    self.raster_line(&v[v.len() - 1], &v[0], &ctx);
                }
            }
            TRIANGLES => {
                for c in v.chunks(3) {
                    if c.len() == 3 {
                        self.raster_triangle(&c[0], &c[1], &c[2], &ctx);
                    }
                }
            }
            TRIANGLE_STRIP => {
                for i in 0..v.len().saturating_sub(2) {
                    // Every other triangle in a strip has reversed winding, and culling can see it.
                    if i % 2 == 0 {
                        self.raster_triangle(&v[i], &v[i + 1], &v[i + 2], &ctx);
                    } else {
                        self.raster_triangle(&v[i + 1], &v[i], &v[i + 2], &ctx);
                    }
                }
            }
            TRIANGLE_FAN => {
                for i in 1..v.len().saturating_sub(1) {
                    self.raster_triangle(&v[0], &v[i], &v[i + 1], &ctx);
                }
            }
            _ => self.fail(INVALID_ENUM),
        }
    }

    /// Clip space -> window space, in SAMPLE units (so supersampling is one multiply, not a second
    /// coordinate system).
    fn to_window(&self, p: [f32; 4], ss: usize, th: usize) -> [f32; 4] {
        let w = if p[3] == 0.0 { 1e-6 } else { p[3] };
        let inv_w = 1.0 / w;
        let (ndc_x, ndc_y, ndc_z) = (p[0] * inv_w, p[1] * inv_w, p[2] * inv_w);
        let (vx, vy, vw, vh) = self.viewport;
        let s = ss as f32;
        let x = (ndc_x * 0.5 + 0.5) * vw as f32 * s + vx as f32 * s;
        let y = (ndc_y * 0.5 + 0.5) * vh as f32 * s + vy as f32 * s;
        let (n, f) = self.depth_range;
        let z = ndc_z * (f - n) * 0.5 + (n + f) * 0.5;
        let _ = th;
        [x, y, z, inv_w]
    }

    fn raster_triangle(&mut self, a: &VertexOut, b: &VertexOut, c: &VertexOut, ctx: &FragCtx) {
        // Near-plane clipping: a vertex behind the eye has w <= 0 and its perspective divide is
        // meaningless. Sutherland-Hodgman against w > epsilon, then fan the result.
        let poly = clip_near(&[a.clone(), b.clone(), c.clone()]);
        if poly.len() < 3 {
            return;
        }
        for i in 1..poly.len() - 1 {
            self.raster_triangle_clipped(&poly[0], &poly[i], &poly[i + 1], ctx);
        }
    }

    fn raster_triangle_clipped(&mut self, a: &VertexOut, b: &VertexOut, c: &VertexOut, ctx: &FragCtx) {
        let (ss, tw, th) = {
            let t = self.current_target_ref();
            (t.ss, t.sw(), t.sh())
        };
        let wa = self.to_window(a.pos, ss, th);
        let mut wb = self.to_window(b.pos, ss, th);
        let mut wc = self.to_window(c.pos, ss, th);
        let (mut vb, mut vc) = (b, c);

        // Signed area in window space. Positive = counter-clockwise with y up.
        let mut area = (wb[0] - wa[0]) * (wc[1] - wa[1]) - (wc[0] - wa[0]) * (wb[1] - wa[1]);
        if area == 0.0 || !area.is_finite() {
            return;
        }
        let ccw = area > 0.0;
        let front = if self.front_face == CW { !ccw } else { ccw };
        if self.caps.contains(&CULL_FACE) {
            let culled = match self.cull {
                FRONT => front,
                BACK => !front,
                FRONT_AND_BACK => true,
                _ => false,
            };
            if culled {
                return;
            }
        }
        // Rasterize counter-clockwise always, so the edge functions are positive inside and the
        // fill rule below has one orientation to reason about. The facing was decided above, from
        // the ORIGINAL winding, so flipping here changes no visible behaviour.
        if !ccw {
            std::mem::swap(&mut wb, &mut wc);
            std::mem::swap(&mut vb, &mut vc);
            area = -area;
        }
        if ss == 2 {
            self.raster_triangle_msaa(a, vb, vc, front, ctx);
            return;
        }

        let minx = wa[0].min(wb[0]).min(wc[0]).floor().max(0.0) as usize;
        let maxx = (wa[0].max(wb[0]).max(wc[0]).ceil() as i64).clamp(0, tw as i64) as usize;
        let miny = wa[1].min(wb[1]).min(wc[1]).floor().max(0.0) as usize;
        let maxy = (wa[1].max(wb[1]).max(wc[1]).ceil() as i64).clamp(0, th as i64) as usize;
        if minx >= maxx || miny >= maxy {
            return;
        }
        let depth_bias = self.polygon_offset.1 * 1e-6;
        // The fill rule. A sample exactly ON a shared edge belongs to exactly ONE of the two
        // triangles that share it, or a blended draw composites it twice and a diagonal seam shows
        // through everything drawn over it. Both triangles see the same edge with opposite
        // direction, so a rule stated in terms of that direction picks one of them.
        let incl_ab = top_left(wa, wb);
        let incl_bc = top_left(wb, wc);
        let incl_ca = top_left(wc, wa);

        for y in miny..maxy {
            for x in minx..maxx {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let e_ab = edge(wa, wb, px, py);
                let e_bc = edge(wb, wc, px, py);
                let e_ca = edge(wc, wa, px, py);
                let inside = (e_ab > 0.0 || (e_ab == 0.0 && incl_ab))
                    && (e_bc > 0.0 || (e_bc == 0.0 && incl_bc))
                    && (e_ca > 0.0 || (e_ca == 0.0 && incl_ca));
                if !inside {
                    continue;
                }
                // Each edge function is proportional to the area of the sub-triangle opposite the
                // vertex it does not touch, so the weights fall out directly.
                let bary = [e_bc / area, e_ca / area, e_ab / area];
                let z = bary[0] * wa[2] + bary[1] * wb[2] + bary[2] * wc[2] + depth_bias;
                // Perspective-correct varying weights: divide each by its vertex's w, renormalize.
                let pw = [bary[0] * wa[3], bary[1] * wb[3], bary[2] * wc[3]];
                let sum = pw[0] + pw[1] + pw[2];
                let pw = if sum != 0.0 { [pw[0] / sum, pw[1] / sum, pw[2] / sum] } else { bary };
                let frag_w = if sum != 0.0 { sum } else { 1.0 };
                let vary = interpolate3(&a.varyings, &vb.varyings, &vc.varyings, pw);
                self.shade(x, y, z, frag_w, front, &vary, [0.0, 0.0], ctx);
                if self.aborted.is_some() {
                    return;
                }
            }
        }
    }

    /// A triangle into the 4x multisampled drawing buffer, as the GPU rasterizes one (measured, see
    /// [`SAMPLES_4X`]): coverage is decided at each of the pixel's four sample positions, the fragment
    /// shader runs ONCE for a pixel any sample of which is covered, with its inputs and `gl_FragCoord`
    /// at the pixel's CENTRE (inside the triangle or not), and its colour goes to every covered sample
    /// whose own depth passes. This supersampled instead -- the shader ran at each sample with a
    /// `gl_FragCoord` of its own -- so a shader that branches on `gl_FragCoord` answered unlike any GPU:
    /// a bot-detection script's proof of work (`mod(gl_FragCoord.x, 2.0)`) read back 17 colours where Chrome's
    /// buffer holds 5. `a`, `b`, `c` run counter-clockwise (the caller has culled and ordered them).
    fn raster_triangle_msaa(&mut self, a: &VertexOut, b: &VertexOut, c: &VertexOut, front: bool, ctx: &FragCtx) {
        let (tw, th) = {
            let t = self.current_target_ref();
            (t.w, t.h)
        };
        let snap = |p: [f32; 4]| [(p[0] * SUBPIXEL).round() / SUBPIXEL, (p[1] * SUBPIXEL).round() / SUBPIXEL, p[2], p[3]];
        let wa = snap(self.to_window(a.pos, 1, th));
        let wb = snap(self.to_window(b.pos, 1, th));
        let wc = snap(self.to_window(c.pos, 1, th));
        // Snapped, a sliver can collapse to nothing (or, at worst, turn over): the GPU draws neither.
        let area = edge(wa, wb, wc[0], wc[1]);
        if area <= 0.0 || !area.is_finite() {
            return;
        }
        let minx = wa[0].min(wb[0]).min(wc[0]).floor().max(0.0) as usize;
        let maxx = (wa[0].max(wb[0]).max(wc[0]).ceil() as i64).clamp(0, tw as i64) as usize;
        let miny = wa[1].min(wb[1]).min(wc[1]).floor().max(0.0) as usize;
        let maxy = (wa[1].max(wb[1]).max(wc[1]).ceil() as i64).clamp(0, th as i64) as usize;
        if minx >= maxx || miny >= maxy {
            return;
        }
        let depth_bias = self.polygon_offset.1 * 1e-6;
        let incl = [top_left(wa, wb), top_left(wb, wc), top_left(wc, wa)];
        for y in miny..maxy {
            for x in minx..maxx {
                let mut covered = [false; 4];
                let mut zs = [0.0f32; 4];
                for (k, &(_, (ox, oy))) in SAMPLES_4X.iter().enumerate() {
                    let (px, py) = (x as f32 + ox, y as f32 + oy);
                    let e = [edge(wa, wb, px, py), edge(wb, wc, px, py), edge(wc, wa, px, py)];
                    if (0..3).all(|i| e[i] > 0.0 || (e[i] == 0.0 && incl[i])) {
                        covered[k] = true;
                        zs[k] = (e[1] * wa[2] + e[2] * wb[2] + e[0] * wc[2]) / area + depth_bias;
                    }
                }
                if !covered.contains(&true) {
                    continue;
                }
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (e_ab, e_bc, e_ca) = (edge(wa, wb, px, py), edge(wb, wc, px, py), edge(wc, wa, px, py));
                let bary = [e_bc / area, e_ca / area, e_ab / area];
                let z = bary[0] * wa[2] + bary[1] * wb[2] + bary[2] * wc[2] + depth_bias;
                let pw = [bary[0] * wa[3], bary[1] * wb[3], bary[2] * wc[3]];
                let sum = pw[0] + pw[1] + pw[2];
                let pw = if sum != 0.0 { [pw[0] / sum, pw[1] / sum, pw[2] / sum] } else { bary };
                let frag_w = if sum != 0.0 { sum } else { 1.0 };
                let vary = interpolate3(&a.varyings, &b.varyings, &c.varyings, pw);
                self.shade_samples(x, y, covered, zs, z, frag_w, front, &vary, ctx);
                if self.aborted.is_some() {
                    return;
                }
            }
        }
    }

    /// One multisampled pixel of [`GlMachine::raster_triangle_msaa`]: scissor, each covered sample's
    /// depth test, the fragment shader once, then blend and write per passing sample.
    #[allow(clippy::too_many_arguments)]
    fn shade_samples(
        &mut self,
        x: usize,
        y: usize,
        covered: [bool; 4],
        zs: [f32; 4],
        z: f32,
        w: f32,
        front_facing: bool,
        vary: &[(String, Val)],
        ctx: &FragCtx,
    ) {
        if self.caps.contains(&SCISSOR_TEST) {
            let sc = self.scissor;
            let (px, py) = (x as i32, y as i32);
            if px < sc.0 || py < sc.1 || px >= sc.0 + sc.2 || py >= sc.1 + sc.3 {
                return;
            }
        }
        let sw = self.current_target_ref().sw();
        let slot = |k: usize| {
            let (sx, sy) = SAMPLES_4X[k].0;
            (y * 2 + sy) * sw + x * 2 + sx
        };
        let depth_on = self.caps.contains(&DEPTH_TEST) && self.attrs_cfg.depth;
        let mut pass = covered;
        if depth_on {
            let t = self.current_target_ref();
            for k in 0..4 {
                if pass[k] && !depth_passes(self.depth_func, zs[k], t.depth[slot(k)]) {
                    pass[k] = false;
                }
            }
        }
        if !pass.contains(&true) {
            return;
        }
        let Some(src) = self.fragment_color([x as f32 + 0.5, y as f32 + 0.5, z, 1.0 / w.max(1e-9)], front_facing, vary, [0.0, 0.0], ctx) else {
            return;
        };
        for k in 0..4 {
            if pass[k] {
                self.write_color(slot(k), src, zs[k], depth_on);
            }
        }
    }

    fn raster_point(&mut self, p: &VertexOut, ctx: &FragCtx) {
        if p.pos[3] <= 0.0 {
            return;
        }
        let (ss, tw, th) = {
            let t = self.current_target_ref();
            (t.ss, t.sw(), t.sh())
        };
        let w = self.to_window(p.pos, ss, th);
        let size = (p.point_size.max(1.0) * ss as f32).round() as i64;
        let half = size / 2;
        let (cx, cy) = (w[0].floor() as i64, w[1].floor() as i64);
        for dy in -half..=half {
            for dx in -half..=half {
                let (x, y) = (cx + dx, cy + dy);
                if x < 0 || y < 0 || x >= tw as i64 || y >= th as i64 {
                    continue;
                }
                // gl_PointCoord: 0..1 across the point, y DOWN, per the ES spec.
                let denom = (size.max(1)) as f32;
                let pc = [
                    (dx + half) as f32 / denom,
                    1.0 - (dy + half) as f32 / denom,
                ];
                self.shade(x as usize, y as usize, w[2], 1.0 / w[3].max(1e-9), true, &p.varyings, pc, ctx);
                if self.aborted.is_some() {
                    return;
                }
            }
        }
    }

    fn raster_line(&mut self, a: &VertexOut, b: &VertexOut, ctx: &FragCtx) {
        let poly = clip_near(&[a.clone(), b.clone()]);
        if poly.len() < 2 {
            return;
        }
        let (ss, tw, th) = {
            let t = self.current_target_ref();
            (t.ss, t.sw(), t.sh())
        };
        let wa = self.to_window(poly[0].pos, ss, th);
        let wb = self.to_window(poly[1].pos, ss, th);
        let steps = ((wb[0] - wa[0]).abs().max((wb[1] - wa[1]).abs()).ceil() as i64).max(1);
        let width = (self.line_width.max(1.0) * ss as f32).round() as i64;
        let half = (width - 1) / 2;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let x = wa[0] + (wb[0] - wa[0]) * t;
            let y = wa[1] + (wb[1] - wa[1]) * t;
            let z = wa[2] + (wb[2] - wa[2]) * t;
            let iw = wa[3] + (wb[3] - wa[3]) * t;
            let vary = interpolate2(&poly[0].varyings, &poly[1].varyings, t);
            for dy in -half..=half {
                for dx in -half..=half {
                    let (px, py) = (x as i64 + dx, y as i64 + dy);
                    if px < 0 || py < 0 || px >= tw as i64 || py >= th as i64 {
                        continue;
                    }
                    self.shade(px as usize, py as usize, z, 1.0 / iw.max(1e-9), true, &vary, [0.0, 0.0], ctx);
                }
            }
            if self.aborted.is_some() {
                return;
            }
        }
    }

    fn current_target_ref(&self) -> &Target {
        let fb = self.cur_framebuffer;
        if fb == 0 {
            return &self.target;
        }
        self.fbo_targets.get(&fb).unwrap_or(&self.target)
    }

    /// One fragment: scissor, depth test, fragment shader, blend, colour mask, write.
    #[allow(clippy::too_many_arguments)]
    fn shade(
        &mut self,
        x: usize,
        y: usize,
        z: f32,
        w: f32,
        front_facing: bool,
        vary: &[(String, Val)],
        point_coord: [f32; 2],
        ctx: &FragCtx,
    ) {
        let ss = self.current_target_ref().ss;
        if self.caps.contains(&SCISSOR_TEST) {
            let sc = self.scissor;
            let (px, py) = ((x / ss) as i32, (y / ss) as i32);
            if px < sc.0 || py < sc.1 || px >= sc.0 + sc.2 || py >= sc.1 + sc.3 {
                return;
            }
        }
        let idx = {
            let t = self.current_target_ref();
            y * t.sw() + x
        };
        let depth_on = self.caps.contains(&DEPTH_TEST) && self.attrs_cfg.depth;
        if depth_on {
            let old = self.current_target_ref().depth[idx];
            if !depth_passes(self.depth_func, z, old) {
                return;
            }
        }

        let frag_coord = [x as f32 / ss as f32 + 0.5, y as f32 / ss as f32 + 0.5, z, 1.0 / w.max(1e-9)];
        let Some(src) = self.fragment_color(frag_coord, front_facing, vary, point_coord, ctx) else { return };
        self.write_color(idx, src, z, depth_on);
    }

    /// The fragment shader, run once: its colour output, or `None` when it discarded or the frame
    /// stopped (an error, or out of budget).
    fn fragment_color(
        &mut self,
        frag_coord: [f32; 4],
        front_facing: bool,
        vary: &[(String, Val)],
        point_coord: [f32; 2],
        ctx: &FragCtx,
    ) -> Option<[f32; 4]> {
        let mut it = Interp::new(&ctx.fs, ctx.env, INVOCATION_FUEL.min(self.fuel.max(1)));
        if it.init_globals().is_err() {
            self.abort("fragment shader failed to initialize");
            return None;
        }
        for (name, v) in ctx.unis {
            it.set(name, v.clone());
        }
        for (name, v) in vary {
            it.set(name, v.clone());
        }
        it.set("gl_FragCoord", Val::F(frag_coord.to_vec()));
        it.set("gl_PointCoord", Val::F(vec![point_coord[0], point_coord[1]]));
        it.set("gl_FrontFacing", Val::scalar_b(front_facing));
        it.set("gl_FragColor", Val::F(vec![0.0, 0.0, 0.0, 0.0]));
        if let Err(e) = it.run_main() {
            self.abort(&describe_run_error(&e));
            return None;
        }
        self.fuel -= INVOCATION_FUEL.min(self.fuel.max(1)) - it.fuel_left();
        if self.fuel <= 0 {
            self.abort("the frame ran out of shader budget");
            return None;
        }
        if it.discarded {
            return None;
        }
        let src = ctx
            .frag_out
            .and_then(|n| it.get(n).cloned())
            .or_else(|| it.get("gl_FragColor").cloned())
            .or_else(|| it.get("gl_FragData").and_then(|v| match v {
                Val::Arr(items) => items.first().cloned(),
                other => Some(other.clone()),
            }))
            .map(|v| v.vec4())
            .unwrap_or([0.0; 4]);
        Some(src)
    }

    /// Blend `src` into the sample at `idx` of the bound target, under the colour mask, and write its
    /// depth. The drawing buffer keeps 8 bits a channel (RGBA8), so what lands there is [`stored`] --
    /// the value the next blend reads as its destination, as on the GPU.
    fn write_color(&mut self, idx: usize, src: [f32; 4], z: f32, depth_on: bool) {
        let dst = self.current_target_ref().color[idx];
        let blended = if self.caps.contains(&BLEND) {
            blend(&self.blend, src, dst)
        } else {
            src
        };
        let cm = self.color_mask;
        let has_alpha = self.attrs_cfg.alpha;
        let dm = self.depth_mask;
        let eight_bit = self.cur_framebuffer == 0;
        let t = self.current_target_mut();
        for c in 0..4 {
            if cm[c] {
                t.color[idx][c] = if eight_bit { stored(blended[c]) } else { blended[c] };
            }
        }
        // A context created with `alpha: false` has no alpha channel to write: the drawing buffer
        // reads back opaque whatever the shader put in .a, which is what the attribute means.
        if !has_alpha {
            t.color[idx][3] = 1.0;
        }
        if dm && depth_on {
            t.depth[idx] = z;
        }
    }

    // ── readback ──────────────────────────────────────────────────────────────────────────────

    /// The default framebuffer as [`Rgba8`] — top-left origin, unpremultiplied, the layout every
    /// other readback in this engine uses. Supersamples are box-filtered down here, which is the
    /// resolve step a multisampled drawing buffer goes through before a page can read it.
    pub fn readback(&self) -> Rgba8 {
        let t = &self.target;
        let (w, h) = (t.w, t.h);
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let px = resolve(t, x, y);
                // Row 0 of the target is the BOTTOM row; Rgba8's row 0 is the top.
                let d = ((h - 1 - y) * w + x) * 4;
                data[d..d + 4].copy_from_slice(&px);
            }
        }
        Rgba8 { width: w as u32, height: h as u32, data }
    }

    /// `readPixels` — a region of the CURRENTLY BOUND framebuffer, in GL's bottom-left row order.
    /// Shares [`Target`] resolution with [`GlMachine::readback`], so for the default framebuffer the
    /// two cannot describe different pixels.
    pub fn read_region(&self, x: i32, y: i32, width: u32, height: u32) -> Vec<u8> {
        let t = self.current_target_ref();
        let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
        for row in 0..height as i64 {
            for col in 0..width as i64 {
                let (sx, sy) = (x as i64 + col, y as i64 + row);
                if sx < 0 || sy < 0 || sx >= t.w as i64 || sy >= t.h as i64 {
                    continue;
                }
                let px = resolve(t, sx as usize, sy as usize);
                let d = ((row as usize) * width as usize + col as usize) * 4;
                for c in 0..4 {
                    out[d + c] = px[c];
                }
            }
        }
        out
    }

    // ── queries ───────────────────────────────────────────────────────────────────────────────

    pub fn query(&mut self, q: &GlQuery) -> GlQueryResult {
        use GlQueryResult as R;
        match q {
            GlQuery::ShaderParameter { shader, pname } => {
                let Some(s) = self.shaders.get(shader) else { return R::None };
                match *pname {
                    COMPILE_STATUS => R::Bool(s.status),
                    DELETE_STATUS => R::Bool(s.deleted),
                    SHADER_TYPE => R::UInt(match s.kind {
                        GlShaderKind::Vertex => VERTEX_SHADER,
                        GlShaderKind::Fragment => FRAGMENT_SHADER,
                    }),
                    _ => R::None,
                }
            }
            GlQuery::ShaderInfoLog { shader } => {
                self.shaders.get(shader).map(|s| R::Str(s.log.clone())).unwrap_or(R::None)
            }
            GlQuery::ShaderSource { shader } => {
                self.shaders.get(shader).map(|s| R::Str(s.source.clone())).unwrap_or(R::None)
            }
            GlQuery::ProgramParameter { program, pname } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                match *pname {
                    LINK_STATUS => R::Bool(p.status),
                    VALIDATE_STATUS => R::Bool(p.validated),
                    DELETE_STATUS => R::Bool(p.deleted),
                    ATTACHED_SHADERS => R::Int(p.shaders.len() as i32),
                    ACTIVE_ATTRIBUTES => R::Int(p.attrs.len() as i32),
                    ACTIVE_UNIFORMS => R::Int(p.unis.len() as i32),
                    0x8A36 => R::Int(p.blocks.len() as i32),       // ACTIVE_UNIFORM_BLOCKS
                    0x8C83 => R::Int(p.tf.len() as i32),           // TRANSFORM_FEEDBACK_VARYINGS
                    0x8C7F => R::UInt(p.tf_mode),                  // TRANSFORM_FEEDBACK_BUFFER_MODE
                    _ => R::None,
                }
            }
            GlQuery::ProgramInfoLog { program } => {
                self.programs.get(program).map(|p| R::Str(p.log.clone())).unwrap_or(R::None)
            }
            GlQuery::AttachedShaders { program } => self
                .programs
                .get(program)
                .map(|p| R::Ints(p.shaders.iter().map(|&s| s as i32).collect()))
                .unwrap_or(R::None),
            GlQuery::AttribLocation { program, name } => {
                let Some(p) = self.programs.get(program) else { return R::Int(-1) };
                R::Int(p.attrs.iter().find(|a| a.name == *name).map(|a| a.location as i32).unwrap_or(-1))
            }
            GlQuery::UniformLocation { program, name } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                if !p.status {
                    return R::None;
                }
                let base = base_name(name);
                // A uniform block's members are set through a buffer, never a location.
                if p.unis.iter().any(|u| u.block >= 0 && (u.name == *name || u.name == base)) {
                    return R::None;
                }
                let known = p.unis.iter().any(|u| u.name == *name)
                    || p.unis.iter().any(|u| u.name == base && matches!(u.ty, Type::Array(..)))
                    || p.unis.iter().any(|u| struct_member_exists(self, u, name));
                if known {
                    // The index is positional only so the page has something to compare; the name is
                    // what actually addresses the uniform (see `GlOp::Uniform`).
                    R::Int(p.unis.iter().position(|u| u.name == base).unwrap_or(0) as i32)
                } else {
                    R::None
                }
            }
            GlQuery::ActiveAttrib { program, index } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                p.attrs
                    .get(*index as usize)
                    .map(|a| R::ActiveInfo { name: a.name.clone(), size: 1, ty: a.ty.gl_enum() })
                    .unwrap_or(R::None)
            }
            GlQuery::ActiveUniform { program, index } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                p.unis
                    .get(*index as usize)
                    .map(|u| {
                        let (size, name) = match &u.ty {
                            Type::Array(_, n) => (*n as i32, format!("{}[0]", u.name)),
                            _ => (1, u.name.clone()),
                        };
                        R::ActiveInfo { name, size, ty: u.ty.gl_enum() }
                    })
                    .unwrap_or(R::None)
            }
            GlQuery::Uniform { program, name } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                match p.values.get(base_name(name)) {
                    Some(Val::I(v)) => R::Ints(v.clone()),
                    Some(Val::U(v)) => R::UInts(v.clone()),
                    Some(Val::B(v)) => R::Bools(v.clone()),
                    Some(v) => R::Floats(v.floats()),
                    None => {
                        // A declared uniform nothing has set reads back as its zero, which is what a
                        // freshly linked program's uniform storage contains.
                        p.unis
                            .iter()
                            .find(|u| u.name == base_name(name))
                            .map(|u| R::Floats(vec![0.0; u.ty.components().max(1)]))
                            .unwrap_or(R::None)
                    }
                }
            }
            GlQuery::Error { consumed } => {
                R::UInt(self.errors.get(*consumed as usize).copied().unwrap_or(NO_ERROR))
            }
            GlQuery::ReadPixels { x, y, width, height } => {
                R::Bytes(self.read_region(*x, *y, *width, *height))
            }
            GlQuery::IsObject { kind, id } => R::Bool(match kind {
                GlObjectKind::Buffer => self.buffers.contains_key(id),
                GlObjectKind::Shader => self.shaders.get(id).map(|s| !s.deleted).unwrap_or(false),
                GlObjectKind::Program => self.programs.get(id).map(|p| !p.deleted).unwrap_or(false),
                GlObjectKind::Texture => self.textures.contains_key(id),
                GlObjectKind::Framebuffer => self.framebuffers.contains_key(id),
                GlObjectKind::Renderbuffer => self.renderbuffers.contains_key(id),
                GlObjectKind::VertexArray => self.vertex_arrays.contains(id),
            }),
            GlQuery::IsEnabled { cap } => R::Bool(self.caps.contains(cap)),
            GlQuery::BufferParameter { target, pname } => {
                let id = self.buffer_for(*target);
                let Some((b, usage)) = self.buffers.get(&id) else { return R::None };
                match *pname {
                    BUFFER_SIZE => R::Int(b.len() as i32),
                    BUFFER_USAGE => R::UInt(*usage),
                    _ => R::None,
                }
            }
            GlQuery::VertexAttrib { index, pname } => {
                let Some(a) = self.attribs.get(*index as usize) else { return R::None };
                match *pname {
                    VERTEX_ATTRIB_ARRAY_ENABLED => R::Bool(a.enabled),
                    VERTEX_ATTRIB_ARRAY_SIZE => R::Int(a.size as i32),
                    VERTEX_ATTRIB_ARRAY_STRIDE => R::Int(a.stride as i32),
                    VERTEX_ATTRIB_ARRAY_TYPE => R::UInt(if a.pointer_set { a.ty } else { FLOAT }),
                    VERTEX_ATTRIB_ARRAY_NORMALIZED => R::Bool(a.normalized),
                    VERTEX_ATTRIB_ARRAY_BUFFER_BINDING => {
                        if a.buffer == 0 {
                            R::None
                        } else {
                            R::UInt(a.buffer)
                        }
                    }
                    CURRENT_VERTEX_ATTRIB => R::Floats(a.constant.to_vec()),
                    _ => R::None,
                }
            }
            GlQuery::VertexAttribOffset { index, .. } => {
                R::Int(self.attribs.get(*index as usize).map(|a| a.offset as i32).unwrap_or(0))
            }
            GlQuery::TexParameter { target, pname } => {
                let Some(id) = self.bound_texture(*target) else { return R::None };
                let Some(t) = self.textures.get(&id) else { return R::None };
                match *pname {
                    TEXTURE_MIN_FILTER => R::UInt(t.min_filter),
                    TEXTURE_MAG_FILTER => R::UInt(t.mag_filter),
                    TEXTURE_WRAP_S => R::UInt(t.wrap_s),
                    TEXTURE_WRAP_T => R::UInt(t.wrap_t),
                    _ => R::None,
                }
            }
            GlQuery::FramebufferStatus { .. } => {
                let fb = self.cur_framebuffer;
                if fb == 0 {
                    return R::UInt(FRAMEBUFFER_COMPLETE);
                }
                match self.framebuffers.get(&fb) {
                    Some(f) if f.color_tex.is_some() || f.color_rb.is_some() => R::UInt(FRAMEBUFFER_COMPLETE),
                    // Nothing attached: INCOMPLETE_ATTACHMENT, which is what GL says and what a
                    // page checking `checkFramebufferStatus` on a fresh FBO reads.
                    Some(_) => R::UInt(FRAMEBUFFER_INCOMPLETE_ATTACHMENT),
                    None => R::None,
                }
            }
            GlQuery::RenderbufferParameter { pname, .. } => {
                let Some(r) = self.renderbuffers.get(&self.cur_renderbuffer) else { return R::None };
                match *pname {
                    RENDERBUFFER_WIDTH => R::Int(r.width as i32),
                    RENDERBUFFER_HEIGHT => R::Int(r.height as i32),
                    RENDERBUFFER_INTERNAL_FORMAT => R::UInt(r.format),
                    _ => R::None,
                }
            }
            GlQuery::FramebufferAttachmentParameter { .. } => R::None,
            GlQuery::UniformType { program, name } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                let base = base_name(name);
                p.unis.iter().find(|u| u.name == *name || u.name == base).map(|u| R::UInt(u.ty.gl_enum())).unwrap_or(R::None)
            }
            GlQuery::UniformBlockIndex { program, name } => {
                let Some(p) = self.programs.get(program) else { return R::UInt(u32::MAX) };
                R::UInt(p.blocks.iter().position(|b| b.name == *name).map(|i| i as u32).unwrap_or(u32::MAX))
            }
            GlQuery::UniformBlockName { program, index } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                p.blocks.get(*index as usize).map(|b| R::Str(b.name.clone())).unwrap_or(R::None)
            }
            GlQuery::UniformBlockParameter { program, index, pname } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                let Some(b) = p.blocks.get(*index as usize) else { return R::None };
                match *pname {
                    0x8A3F => R::UInt(b.binding),                                  // UNIFORM_BLOCK_BINDING
                    0x8A40 => R::UInt(b.size),                                     // UNIFORM_BLOCK_DATA_SIZE
                    0x8A42 => R::UInt(b.members.len() as u32),                     // UNIFORM_BLOCK_ACTIVE_UNIFORMS
                    0x8A43 => R::UInts(b.members.clone()),                         // ..._ACTIVE_UNIFORM_INDICES
                    0x8A44 => R::Bool(b.vs),                                       // ..._REFERENCED_BY_VERTEX_SHADER
                    0x8A46 => R::Bool(b.fs),                                       // ..._REFERENCED_BY_FRAGMENT_SHADER
                    _ => R::None,
                }
            }
            GlQuery::UniformIndices { program, names } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                R::UInts(
                    names
                        .iter()
                        .map(|n| {
                            let base = n.strip_suffix("[0]").unwrap_or(n);
                            p.unis.iter().position(|u| u.name == *n || u.name == base).map(|i| i as u32).unwrap_or(u32::MAX)
                        })
                        .collect(),
                )
            }
            GlQuery::ActiveUniforms { program, indices, pname } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                let mut ints = Vec::new();
                let mut bools = Vec::new();
                for i in indices {
                    let Some(u) = p.unis.get(*i as usize) else { return R::None };
                    let block_decl = (u.block >= 0)
                        .then(|| p.blocks.get(u.block as usize))
                        .flatten()
                        .and_then(|b| {
                            let sh = if b.vs { p.vs.as_ref() } else { p.fs.as_ref() };
                            sh.and_then(|sh| sh.blocks.iter().find(|(n, _)| *n == b.name).map(|(_, m)| (m.clone(), sh.structs.clone())))
                        });
                    let layout = match &block_decl {
                        Some((m, st)) => std140_layout(m, &u.name, st),
                        None => (-1, -1, -1),
                    };
                    match *pname {
                        0x8A37 => ints.push(u.ty.gl_enum() as i64),                                       // UNIFORM_TYPE
                        0x8A38 => ints.push(match &u.ty { Type::Array(_, n) => *n as i64, _ => 1 }),     // UNIFORM_SIZE
                        0x8A3A => ints.push(u.block as i64),                                              // UNIFORM_BLOCK_INDEX
                        0x8A3B => ints.push(layout.0 as i64),                                             // UNIFORM_OFFSET
                        0x8A3C => ints.push(layout.1 as i64),                                             // UNIFORM_ARRAY_STRIDE
                        0x8A3D => ints.push(layout.2 as i64),                                             // UNIFORM_MATRIX_STRIDE
                        0x8A3E => bools.push(false),                                                      // UNIFORM_IS_ROW_MAJOR
                        _ => return R::None,
                    }
                }
                if *pname == 0x8A3E {
                    R::Bools(bools)
                } else if *pname == 0x8A37 {
                    R::UInts(ints.into_iter().map(|v| v as u32).collect())
                } else {
                    R::Ints(ints.into_iter().map(|v| v as i32).collect())
                }
            }
            GlQuery::FragDataLocation { program, name } => {
                let Some(p) = self.programs.get(program) else { return R::Int(-1) };
                if !p.status {
                    return R::Int(-1);
                }
                R::Int(p.frag_outs.iter().position(|n| n == name).map(|i| i as i32).unwrap_or(-1))
            }
            // a GPU's round trips: this pipeline has no GPU to ask
            GlQuery::Get { .. } | GlQuery::Internalformat { .. } => R::None,
            GlQuery::TransformFeedbackVarying { program, index } => {
                let Some(p) = self.programs.get(program) else { return R::None };
                p.tf
                    .get(*index as usize)
                    .map(|(name, ty)| {
                        let size = match ty { Type::Array(_, n) => *n as i32, _ => 1 };
                        R::ActiveInfo { name: name.clone(), size, ty: ty.gl_enum() }
                    })
                    .unwrap_or(R::None)
            }
        }
    }
}

fn struct_member_exists(_m: &GlMachine, u: &UniInfo, name: &str) -> bool {
    // `u.field` / `u[0].field` — the page addresses a member of a struct uniform. The name is the
    // key the uniform op carries, so the check is only that the prefix names a declared uniform.
    name.starts_with(&u.name) && name.len() > u.name.len()
}

/// `u[3]` -> `u`; anything else unchanged.
fn base_name(name: &str) -> &str {
    match name.find('[') {
        Some(i) => &name[..i],
        None => name,
    }
}

fn element_index(name: &str) -> Option<usize> {
    let open = name.find('[')?;
    let close = name.find(']')?;
    name.get(open + 1..close)?.parse().ok()
}

fn uniform_val(v: &GlUniformValue) -> Val {
    match v {
        GlUniformValue::Float { values, .. } => Val::F(values.clone()),
        GlUniformValue::Int { values, .. } => Val::I(values.clone()),
        GlUniformValue::UInt { values, .. } => Val::U(values.clone()),
        GlUniformValue::Matrix { rows, cols, transpose, values } => {
            let (r, c) = (*rows as usize, *cols as usize);
            let v = if *transpose {
                let mut out = vec![0.0; r * c];
                for col in 0..c {
                    for row in 0..r {
                        out[col * r + row] = values.get(row * c + col).copied().unwrap_or(0.0);
                    }
                }
                out
            } else {
                let mut out = values.clone();
                out.resize(r * c, 0.0);
                out
            };
            Val::M { cols: c, rows: r, v }
        }
    }
}

fn describe_run_error(e: &RunError) -> String {
    match e {
        RunError::OutOfFuel => "the shader exceeded its execution budget".to_string(),
        RunError::Unsupported(m) => m.clone(),
    }
}

/// The signed area (times two) of the triangle (v0, v1, p) — positive when p is to the LEFT of the
/// directed edge v0 -> v1, which with y up means "inside" for a counter-clockwise triangle.
fn edge(v0: [f32; 4], v1: [f32; 4], px: f32, py: f32) -> f32 {
    (v1[0] - v0[0]) * (py - v0[1]) - (px - v0[0]) * (v1[1] - v0[1])
}

/// The fill rule, stated with y UP (GL's window orientation, not a y-down framebuffer's): a sample
/// exactly on a directed edge belongs to this triangle when the edge runs downward, or is horizontal
/// and runs right-to-left. The adjacent triangle sees the same edge reversed and so declines it.
fn top_left(v0: [f32; 4], v1: [f32; 4]) -> bool {
    let (dx, dy) = (v1[0] - v0[0], v1[1] - v0[1]);
    dy < 0.0 || (dy == 0.0 && dx < 0.0)
}

/// One resolved pixel: the box filter over its samples, quantized to 8 bits. This is the multisample
/// resolve a drawing buffer goes through before anything can read it, and it is deliberately the ONLY
/// route from the float target to bytes -- `readback` (which `toDataURL` encodes) and `read_region`
/// (which `readPixels` copies) both go through here, so they cannot disagree.
///
/// AS THE GPU RESOLVES (measured, see [`SAMPLES_4X`]): each sample is the 8-bit value its RGBA8 buffer
/// holds, the samples are averaged as 32-bit floats, and the average is converted back with
/// [`unorm8`]. Two coverage-2/4 pixels tell the steps apart: two 255 samples and two zeros resolve to
/// 127 (an average of exactly 0.5, a tie), two 127 samples and two zeros to 64 (127/255 as a float is
/// a hair above 0.498039..., so half of it is past the tie).
fn resolve(t: &Target, x: usize, y: usize) -> [u8; 4] {
    let ss = t.ss;
    if ss == 1 {
        return t.color[y * t.w + x].map(unorm8);
    }
    let inv = 1.0 / (ss * ss) as f32;
    let mut acc = [0.0f32; 4];
    for sy in 0..ss {
        for sx in 0..ss {
            let px = t.color[(y * ss + sy) * t.w * ss + x * ss + sx];
            for c in 0..4 {
                acc[c] += unorm8(px[c]) as f32 / 255.0;
            }
        }
    }
    acc.map(|a| unorm8(a * inv))
}

/// A float colour component as an 8-bit buffer stores it: `v * 255` to the nearest integer, an exact
/// tie going DOWN. Measured on the persona's GPU: a fragment's 0.5 is stored as 127 (`fract
/// (gl_FragCoord.x)` and a bot-detection script's `c.x = .5` both read back 127), and a resolve's exact 0.5 is 127
/// too, where rounding half up -- what this did -- gives 128.
fn unorm8(v: f32) -> u8 {
    let t = v.clamp(0.0, 1.0) as f64 * 255.0;
    let floor = t.floor();
    (if t - floor <= 0.5 { floor } else { floor + 1.0 }) as u8
}

/// [`unorm8`] as the float the buffer's sample then holds.
fn stored(v: f32) -> f32 {
    unorm8(v) as f32 / 255.0
}

fn depth_passes(func: u32, new: f32, old: f32) -> bool {
    match func {
        NEVER => false,
        LESS => new < old,
        EQUAL => new == old,
        LEQUAL => new <= old,
        GREATER => new > old,
        NOTEQUAL => new != old,
        GEQUAL => new >= old,
        ALWAYS => true,
        _ => true,
    }
}

fn blend_factor(f: u32, src: [f32; 4], dst: [f32; 4], cst: [f32; 4], alpha_only: bool) -> [f32; 4] {
    let k = |v: f32| [v, v, v, v];
    match f {
        0 => k(0.0),
        1 => k(1.0),
        0x0300 => src,
        0x0301 => [1.0 - src[0], 1.0 - src[1], 1.0 - src[2], 1.0 - src[3]],
        0x0302 => k(src[3]),
        0x0303 => k(1.0 - src[3]),
        0x0304 => k(dst[3]),
        0x0305 => k(1.0 - dst[3]),
        0x0306 => dst,
        0x0307 => [1.0 - dst[0], 1.0 - dst[1], 1.0 - dst[2], 1.0 - dst[3]],
        0x0308 => {
            let s = src[3].min(1.0 - dst[3]);
            if alpha_only {
                k(1.0)
            } else {
                [s, s, s, 1.0]
            }
        }
        0x8001 => cst,
        0x8002 => [1.0 - cst[0], 1.0 - cst[1], 1.0 - cst[2], 1.0 - cst[3]],
        0x8003 => k(cst[3]),
        0x8004 => k(1.0 - cst[3]),
        _ => k(1.0),
    }
}

fn blend(b: &BlendState, src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let sf_rgb = blend_factor(b.src_rgb, src, dst, b.color, false);
    let df_rgb = blend_factor(b.dst_rgb, src, dst, b.color, false);
    let sf_a = blend_factor(b.src_a, src, dst, b.color, true);
    let df_a = blend_factor(b.dst_a, src, dst, b.color, true);
    let combine = |eq: u32, s: f32, d: f32| match eq {
        FUNC_SUBTRACT => s - d,
        FUNC_REVERSE_SUBTRACT => d - s,
        MIN_EXT => s.min(d),
        MAX_EXT => s.max(d),
        _ => s + d,
    };
    let mut out = [0.0f32; 4];
    for c in 0..3 {
        let (s, d) = match b.eq_rgb {
            MIN_EXT | MAX_EXT => (src[c], dst[c]),
            _ => (src[c] * sf_rgb[c], dst[c] * df_rgb[c]),
        };
        out[c] = combine(b.eq_rgb, s, d);
    }
    let (s, d) = match b.eq_a {
        MIN_EXT | MAX_EXT => (src[3], dst[3]),
        _ => (src[3] * sf_a[3], dst[3] * df_a[3]),
    };
    out[3] = combine(b.eq_a, s, d);
    out
}

// ── vertex plumbing ───────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct VertexOut {
    pos: [f32; 4],
    point_size: f32,
    varyings: Vec<(String, Val)>,
}

struct FragCtx<'a> {
    fs: Rc<Shader>,
    unis: &'a HashMap<String, Val>,
    frag_out: Option<&'a str>,
    env: &'a TexEnv<'a>,
}

/// Clip a primitive against the near plane in CLIP space (`w > eps`). Without this, a vertex behind
/// the eye divides by a negative w and lands on the wrong side of the screen — a triangle that
/// should be partly visible fills the whole viewport instead.
fn clip_near(poly: &[VertexOut]) -> Vec<VertexOut> {
    const EPS: f32 = 1e-5;
    if poly.iter().all(|v| v.pos[3] > EPS) {
        return poly.to_vec();
    }
    let mut out: Vec<VertexOut> = Vec::new();
    for i in 0..poly.len() {
        let a = &poly[i];
        let b = &poly[(i + 1) % poly.len()];
        let (ain, bin) = (a.pos[3] > EPS, b.pos[3] > EPS);
        if ain {
            out.push(a.clone());
        }
        if ain != bin {
            let t = (EPS - a.pos[3]) / (b.pos[3] - a.pos[3]);
            out.push(lerp_vertex(a, b, t));
        }
    }
    if poly.len() == 2 {
        out.truncate(2);
    }
    out
}

fn lerp_vertex(a: &VertexOut, b: &VertexOut, t: f32) -> VertexOut {
    let mut pos = [0.0f32; 4];
    for i in 0..4 {
        pos[i] = a.pos[i] + (b.pos[i] - a.pos[i]) * t;
    }
    VertexOut {
        pos,
        point_size: a.point_size + (b.point_size - a.point_size) * t,
        varyings: interpolate2(&a.varyings, &b.varyings, t),
    }
}

fn interpolate2(a: &[(String, Val)], b: &[(String, Val)], t: f32) -> Vec<(String, Val)> {
    a.iter()
        .enumerate()
        .map(|(i, (n, av))| {
            let bv = b.get(i).map(|(_, v)| v.floats()).unwrap_or_default();
            let af = av.floats();
            let out: Vec<f32> = af
                .iter()
                .enumerate()
                .map(|(k, &x)| x + (bv.get(k).copied().unwrap_or(x) - x) * t)
                .collect();
            (n.clone(), Val::F(out))
        })
        .collect()
}

fn interpolate3(
    a: &[(String, Val)],
    b: &[(String, Val)],
    c: &[(String, Val)],
    w: [f32; 3],
) -> Vec<(String, Val)> {
    a.iter()
        .enumerate()
        .map(|(i, (n, av))| {
            let af = av.floats();
            let bf = b.get(i).map(|(_, v)| v.floats()).unwrap_or_default();
            let cf = c.get(i).map(|(_, v)| v.floats()).unwrap_or_default();
            let out: Vec<f32> = af
                .iter()
                .enumerate()
                .map(|(k, &x)| {
                    x * w[0]
                        + bf.get(k).copied().unwrap_or(x) * w[1]
                        + cf.get(k).copied().unwrap_or(x) * w[2]
                })
                .collect();
            (n.clone(), Val::F(out))
        })
        .collect()
}

// ── texture sampling ──────────────────────────────────────────────────────────────────────────

struct TexEnv<'a> {
    textures: &'a HashMap<u32, TextureObj>,
    /// Copied, not borrowed: the machine keeps mutating itself while the fragment stage samples, and
    /// nothing inside a draw call rebinds a texture unit anyway.
    units: [(u32, u32); MAX_TEXTURE_UNITS],
}

fn wrap(mode: u32, v: f32, n: f32) -> f32 {
    match mode {
        CLAMP_TO_EDGE => v.clamp(0.0, n - 1.0),
        MIRRORED_REPEAT => {
            let period = 2.0 * n;
            let m = v.rem_euclid(period);
            if m < n {
                m
            } else {
                period - m - 1.0
            }
        }
        _ => v.rem_euclid(n),
    }
}

impl TextureSource for TexEnv<'_> {
    fn sample_2d(&self, unit: i32, s: f32, t: f32) -> [f32; 4] {
        let u = unit.clamp(0, MAX_TEXTURE_UNITS as i32 - 1) as usize;
        let id = self.units[u].0;
        let Some(tex) = self.textures.get(&id) else { return [0.0, 0.0, 0.0, 1.0] };
        let Some(img) = tex.images.get(&TEXTURE_2D) else { return [0.0, 0.0, 0.0, 1.0] };
        if img.width == 0 || img.height == 0 {
            return [0.0, 0.0, 0.0, 1.0];
        }
        // GL's t axis runs bottom-up over the image rows as uploaded, and `UNPACK_FLIP_Y_WEBGL` is
        // applied by the recorder before the pixels get here, so row 0 is t=0.
        let (w, h) = (img.width as f32, img.height as f32);
        let (x, y) = (s * w - 0.5, t * h - 0.5);
        let texel = |ix: f32, iy: f32| -> [f32; 4] {
            let px = wrap(tex.wrap_s, ix, w) as usize;
            let py = wrap(tex.wrap_t, iy, h) as usize;
            let i = (py.min(img.height as usize - 1) * img.width as usize + px.min(img.width as usize - 1)) * 4;
            [
                img.data[i] as f32 / 255.0,
                img.data[i + 1] as f32 / 255.0,
                img.data[i + 2] as f32 / 255.0,
                img.data[i + 3] as f32 / 255.0,
            ]
        };
        let linear = matches!(
            tex.mag_filter,
            LINEAR | LINEAR_MIPMAP_LINEAR | LINEAR_MIPMAP_NEAREST
        ) || matches!(tex.min_filter, LINEAR | LINEAR_MIPMAP_LINEAR | LINEAR_MIPMAP_NEAREST);
        let _ = (NEAREST, NEAREST_MIPMAP_NEAREST, NEAREST_MIPMAP_LINEAR);
        if !linear {
            return texel(x.round(), y.round());
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let c00 = texel(x0, y0);
        let c10 = texel(x0 + 1.0, y0);
        let c01 = texel(x0, y0 + 1.0);
        let c11 = texel(x0 + 1.0, y0 + 1.0);
        let mut out = [0.0f32; 4];
        for i in 0..4 {
            let a = c00[i] + (c10[i] - c00[i]) * fx;
            let b = c01[i] + (c11[i] - c01[i]) * fx;
            out[i] = a + (b - a) * fy;
        }
        out
    }

    fn sample_cube(&self, unit: i32, d: [f32; 3]) -> [f32; 4] {
        let u = unit.clamp(0, MAX_TEXTURE_UNITS as i32 - 1) as usize;
        let id = self.units[u].1;
        let Some(tex) = self.textures.get(&id) else { return [0.0, 0.0, 0.0, 1.0] };
        // Major axis selection, then the face's (s, t) — the standard cube-map projection.
        let (ax, ay, az) = (d[0].abs(), d[1].abs(), d[2].abs());
        let (face, sc, tc, ma) = if ax >= ay && ax >= az {
            if d[0] > 0.0 {
                (0, -d[2], -d[1], ax)
            } else {
                (1, d[2], -d[1], ax)
            }
        } else if ay >= az {
            if d[1] > 0.0 {
                (2, d[0], d[2], ay)
            } else {
                (3, d[0], -d[2], ay)
            }
        } else if d[2] > 0.0 {
            (4, d[0], -d[1], az)
        } else {
            (5, -d[0], -d[1], az)
        };
        if ma == 0.0 {
            return [0.0, 0.0, 0.0, 1.0];
        }
        let s = 0.5 * (sc / ma + 1.0);
        let t = 0.5 * (tc / ma + 1.0);
        let key = TEXTURE_CUBE_MAP_POSITIVE_X + face;
        let Some(img) = tex.images.get(&key) else { return [0.0, 0.0, 0.0, 1.0] };
        if img.width == 0 || img.height == 0 {
            return [0.0, 0.0, 0.0, 1.0];
        }
        let px = ((s * img.width as f32) as usize).min(img.width as usize - 1);
        let py = ((t * img.height as f32) as usize).min(img.height as usize - 1);
        let i = (py * img.width as usize + px) * 4;
        [
            img.data[i] as f32 / 255.0,
            img.data[i + 1] as f32 / 255.0,
            img.data[i + 2] as f32 / 255.0,
            img.data[i + 3] as f32 / 255.0,
        ]
    }
}

// ── the entry points the backend calls ────────────────────────────────────────────────────────

/// Execute a command list and return the drawing buffer.
pub fn raster(cmds: &[GlOp], width: u32, height: u32, attrs: &GlContextAttributes) -> Rgba8 {
    let mut m = GlMachine::new(width, height, attrs);
    m.run(cmds);
    m.readback()
}

/// The machine a list's metadata questions were last answered by, kept for the next one
/// ([`query_list`]).
struct KeptQuery {
    list: u64,
    /// How many of the list's ops it has run.
    ran: usize,
    machine: GlMachine,
}

/// How many lists [`query_list`] keeps a machine for: a page queries one or two contexts at a time.
const KEPT_QUERY_LISTS: usize = 4;

/// The machine a list's `readPixels` / `getError` was last answered by -- draws and all -- kept for
/// the next one ([`query_list`]).
struct KeptDraw {
    list: u64,
    ran: usize,
    machine: GlMachine,
}

/// How many lists keep a drawing machine: each holds a whole multisampled buffer (up to
/// [`ANTIALIAS_PIXEL_LIMIT`] pixels of four samples), so fewer than the metadata machines.
const KEPT_DRAW_LISTS: usize = 2;

thread_local! {
    /// [`query_list`]'s machines, the most recently asked last.
    static KEPT_QUERIES: RefCell<Vec<KeptQuery>> = const { RefCell::new(Vec::new()) };
    /// [`query_list`]'s drawing machines, the most recently asked last.
    static KEPT_DRAWS: RefCell<Vec<KeptDraw>> = const { RefCell::new(Vec::new()) };
}

/// [`query`], for the list `list` names (`RenderBackend::query_gl`): a question that skips the draws
/// is answered by the machine that answered the list's last such question, run on over the ops recorded
/// since -- the state a machine reaches is the ops' alone, and the list only grew at its end (the
/// context attributes the machine was made with are read by draws and readbacks only). `getError`
/// and `readPixels`, which replay the draws, and a list with no name replay it from nothing, as
/// [`query`] does.
///
/// Every question replayed the whole list before: www.github.com's globe asks for each
/// uniform's location while its list grows by a frame's ops, and those replays -- every shader compiled
/// again, every uniform write looked up by name -- were 20 % of the page's load.
///
/// `getError` and `readPixels` are answered the same way by a machine that DRAWS, kept per list: the
/// pixels a machine holds are the ops' alone too -- the drawing buffer's size included, which the list
/// carries (`GlOp::ResizeDrawingBuffer`) -- and each run gets a frame's shader budget, as a replay did.
/// They replayed the whole list before: a bot-detection script's challenge draws its proof of work and
/// reads it back once a frame, so every readback rasterized every frame since the context was made --
/// 112 of them took 42 s on the main thread, the longest 0.9 s, and every input the page was sent
/// waited behind them (Chrome: 25 readbacks, 40 ms).
pub fn query_list(cmds: &[GlOp], q: &GlQuery, width: u32, height: u32, attrs: &GlContextAttributes, list: Option<u64>) -> GlQueryResult {
    let list = match list {
        None => return query(cmds, q, width, height, attrs),
        Some(list) => list,
    };
    if matches!(q, GlQuery::Error { .. } | GlQuery::ReadPixels { .. }) {
        return KEPT_DRAWS.with(|kept| {
            let mut kept = kept.borrow_mut();
            let at = kept.iter().position(|k| k.list == list && k.ran <= cmds.len());
            let mut k = match at {
                Some(i) => kept.remove(i),
                None => KeptDraw { list, ran: 0, machine: GlMachine::new(width.max(1), height.max(1), attrs) },
            };
            k.machine.fuel = FRAME_FUEL;
            k.machine.run(&cmds[k.ran..]);
            k.ran = cmds.len();
            let answer = k.machine.query(q);
            kept.retain(|o| o.list != list);
            if kept.len() == KEPT_DRAW_LISTS {
                kept.remove(0);
            }
            kept.push(k);
            answer
        });
    }
    KEPT_QUERIES.with(|kept| {
        let mut kept = kept.borrow_mut();
        let at = kept.iter().position(|k| k.list == list && k.ran <= cmds.len());
        let mut k = match at {
            Some(i) => kept.remove(i),
            None => KeptQuery { list, ran: 0, machine: GlMachine::new(1, 1, attrs) },
        };
        k.machine.run_metadata_only(&cmds[k.ran..]);
        k.ran = cmds.len();
        let answer = k.machine.query(q);
        kept.retain(|o| o.list != list);
        if kept.len() == KEPT_QUERY_LISTS {
            kept.remove(0);
        }
        kept.push(k);
        answer
    })
}

/// Answer one synchronous question about the state a command list has built.
pub fn query(cmds: &[GlOp], q: &GlQuery, width: u32, height: u32, attrs: &GlContextAttributes) -> GlQueryResult {
    // A metadata query needs no framebuffer, but `readPixels` and `getError` replay the draws, and a
    // draw into a 1x1 scratch buffer would answer a different question.
    let mut m = match q {
        GlQuery::Error { .. } | GlQuery::ReadPixels { .. } => GlMachine::new(width.max(1), height.max(1), attrs),
        _ => GlMachine::new(1, 1, attrs),
    };
    match q {
        // `getError` and `readPixels` have to see what the DRAWS did, so they are the two queries
        // that replay them. Everything else is a compile/link/state question and skips rasterizing.
        GlQuery::Error { .. } | GlQuery::ReadPixels { .. } => m.run(cmds),
        _ => m.run_metadata_only(cmds),
    }
    m.query(q)
}

#[cfg(test)]
mod tests;
