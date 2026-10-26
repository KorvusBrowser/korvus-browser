//! One WebGL context's recorded commands played on the GPU: the [`GlOp`]s the page recorded, run on a
//! GLES context of the [`Device`](super::device::Device), and every [`GlQuery`] answered by that context -- the same
//! two calls `gl::raster` / `gl::query_list` make of the software interpreter, with the same result shapes.
//!
//! THE DEFAULT FRAMEBUFFER IS CHROME'S DRAWINGBUFFER: the page's framebuffer 0 is an offscreen one -- RGBA8, or RGB8
//! for `alpha: false`; a packed depth-stencil buffer when either was asked for; 4x multisampled for `antialias` and
//! resolved into a single-sample buffer when it is read (`readPixels` of framebuffer 0, the compositor's readback) --
//! sized by `ResizeDrawingBuffer` (a new size reallocates it cleared, as Blink's `DrawingBuffer::Resize`).
//!
//! OBJECTS: the page's ids are the recorder's (`u32`, 0 = none); each maps to the GL name ANGLE gave it.
//! ERRORS: one list in the order they happened -- the binding's own (`RaiseError`) and the context's (drained
//! after every command) -- read by the binding's cursor (`GlQuery::Error { consumed }`), as the interpreter keeps it.

use std::collections::HashMap;
use std::ffi::{c_void, CString};

use spec::{GlContextAttributes, GlObjectKind, GlOp, GlQuery, GlQueryResult as R, GlShaderKind, GlUniformValue, Rgba8};

use super::gles::Gles;

const NO_ERROR: u32 = 0;
const FRAMEBUFFER: u32 = 0x8D40;
const READ_FRAMEBUFFER: u32 = 0x8CA8;
const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const RENDERBUFFER: u32 = 0x8D41;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const DEPTH_ATTACHMENT: u32 = 0x8D00;
const STENCIL_ATTACHMENT: u32 = 0x8D20;
const RGBA8: u32 = 0x8058;
const RGB8: u32 = 0x8051;
const DEPTH24_STENCIL8: u32 = 0x88F0;
const RGBA: u32 = 0x1908;
const UNSIGNED_BYTE: u32 = 0x1401;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const NEAREST: u32 = 0x2600;
const PACK_ALIGNMENT: u32 = 0x0D05;
const UNPACK_ALIGNMENT: u32 = 0x0CF5;
const VERTEX_SHADER: u32 = 0x8B31;
const FRAGMENT_SHADER: u32 = 0x8B30;
const COMPILE_STATUS: u32 = 0x8B81;
const DELETE_STATUS: u32 = 0x8B80;
const SHADER_TYPE: u32 = 0x8B4F;
const INFO_LOG_LENGTH: u32 = 0x8B84;
const LINK_STATUS: u32 = 0x8B82;
const VALIDATE_STATUS: u32 = 0x8B83;
const ATTACHED_SHADERS: u32 = 0x8B85;
const ACTIVE_UNIFORMS: u32 = 0x8B86;
const ACTIVE_UNIFORM_MAX_LENGTH: u32 = 0x8B87;
const ACTIVE_ATTRIBUTES: u32 = 0x8B89;
const ACTIVE_ATTRIBUTE_MAX_LENGTH: u32 = 0x8B8A;
const ACTIVE_UNIFORM_BLOCKS: u32 = 0x8A36;
const TRANSFORM_FEEDBACK_VARYINGS: u32 = 0x8C83;
const TRANSFORM_FEEDBACK_BUFFER_MODE: u32 = 0x8C7F;
const BUFFER_SIZE: u32 = 0x8764;
const BUFFER_USAGE: u32 = 0x8765;
const VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;
const VERTEX_ATTRIB_ARRAY_SIZE: u32 = 0x8623;
const VERTEX_ATTRIB_ARRAY_STRIDE: u32 = 0x8624;
const VERTEX_ATTRIB_ARRAY_TYPE: u32 = 0x8625;
const VERTEX_ATTRIB_ARRAY_NORMALIZED: u32 = 0x886A;
const VERTEX_ATTRIB_ARRAY_BUFFER_BINDING: u32 = 0x889F;
const VERTEX_ATTRIB_ARRAY_DIVISOR: u32 = 0x88FE;
const VERTEX_ATTRIB_ARRAY_INTEGER: u32 = 0x88FD;
const CURRENT_VERTEX_ATTRIB: u32 = 0x8626;
const VERTEX_ATTRIB_ARRAY_POINTER: u32 = 0x8645;
const RENDERBUFFER_INTERNAL_FORMAT: u32 = 0x8D44;
const UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES: u32 = 0x8A43;
const UNIFORM_BLOCK_REFERENCED_BY_VERTEX_SHADER: u32 = 0x8A44;
const UNIFORM_BLOCK_REFERENCED_BY_FRAGMENT_SHADER: u32 = 0x8A46;
const UNIFORM_BLOCK_ACTIVE_UNIFORMS: u32 = 0x8A42;
const UNIFORM_TYPE: u32 = 0x8A37;
const UNIFORM_IS_ROW_MAJOR: u32 = 0x8A3E;
/// WebGL's own pixel-store parameters (Blink applies them to the pixels before they reach the GPU, as the recorder does).
const WEBGL_PIXEL_STORE: [u32; 4] = [0x9240, 0x9241, 0x9242, 0x9243];

/// What Chrome's passthrough decoder enables in every context (`profile::OPTIONAL_FUNCTIONALITY`), and the two its
/// DrawingBuffer's resolve and the WebGL 1 index type need.
const INTERNAL_EXTENSIONS: [&str; 2] = ["GL_ANGLE_framebuffer_blit", "GL_OES_element_index_uint"];

/// Framebuffer 0.
struct DrawingBuffer {
    width: i32,
    height: i32,
    /// what the page draws into: multisampled when `antialias`
    draw_fbo: u32,
    draw_color: u32,
    depth_stencil: u32,
    /// the single-sample buffer a multisampled one resolves into
    resolve: Option<(u32, u32)>,
}

#[derive(Default)]
struct Names {
    buffers: HashMap<u32, u32>,
    shaders: HashMap<u32, u32>,
    programs: HashMap<u32, u32>,
    textures: HashMap<u32, u32>,
    framebuffers: HashMap<u32, u32>,
    renderbuffers: HashMap<u32, u32>,
    vertex_arrays: HashMap<u32, u32>,
}

/// One WebGL context on the GPU.
pub struct Exec {
    attrs: GlContextAttributes,
    db: DrawingBuffer,
    names: Names,
    shader_kind: HashMap<u32, GlShaderKind>,
    shader_source: HashMap<u32, String>,
    deleted_shaders: Vec<u32>,
    deleted_programs: Vec<u32>,
    attached: HashMap<u32, Vec<u32>>,
    renderbuffer_format: HashMap<u32, u32>,
    bound_renderbuffer: u32,
    /// the page's framebuffer bindings (its ids; 0 = the DrawingBuffer)
    draw_fb: u32,
    read_fb: u32,
    errors: Vec<u32>,
    /// how many of the list's commands have run
    pub done: usize,
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl Exec {
    /// The context must be current (its version is the context's: an ES 3 one for WebGL 2).
    pub fn new(gl: &Gles, width: u32, height: u32, attrs: &GlContextAttributes) -> Exec {
        unsafe {
            for e in super::profile::OPTIONAL_FUNCTIONALITY.iter().chain(INTERNAL_EXTENSIONS.iter()) {
                let c = cstr(e);
                (gl.glRequestExtensionANGLE)(c.as_ptr());
            }
        }
        let db = DrawingBuffer { width: 0, height: 0, draw_fbo: 0, draw_color: 0, depth_stencil: 0, resolve: None };
        let mut x = Exec {
            attrs: attrs.clone(),
            db,
            names: Names::default(),
            shader_kind: HashMap::new(),
            shader_source: HashMap::new(),
            deleted_shaders: Vec::new(),
            deleted_programs: Vec::new(),
            attached: HashMap::new(),
            renderbuffer_format: HashMap::new(),
            bound_renderbuffer: 0,
            draw_fb: 0,
            read_fb: 0,
            errors: Vec::new(),
            done: 0,
        };
        x.allocate(gl, width.max(1) as i32, height.max(1) as i32);
        unsafe {
            (gl.glBindFramebuffer)(FRAMEBUFFER, x.db.draw_fbo);
            // WebGL's initial viewport and scissor box are the drawing buffer's (GL's would be the 1x1 pbuffer that
            // made the context current)
            (gl.glViewport)(0, 0, x.db.width, x.db.height);
            (gl.glScissor)(0, 0, x.db.width, x.db.height);
            // the engine's own setup is not the page's: its errors are not the page's either
            while (gl.glGetError)() != NO_ERROR {}
        }
        x
    }

    /// (Re)allocate the DrawingBuffer at `width` x `height`, cleared (robust resource initialization).
    fn allocate(&mut self, gl: &Gles, width: i32, height: i32) {
        unsafe {
            if self.db.draw_fbo != 0 {
                (gl.glDeleteFramebuffers)(1, &self.db.draw_fbo);
                let rbs = [self.db.draw_color, self.db.depth_stencil];
                (gl.glDeleteRenderbuffers)(2, rbs.as_ptr());
                if let Some((f, c)) = self.db.resolve.take() {
                    (gl.glDeleteFramebuffers)(1, &f);
                    (gl.glDeleteRenderbuffers)(1, &c);
                }
            }
            let color_format = if self.attrs.alpha { RGBA8 } else { RGB8 };
            let samples = if self.attrs.antialias { 4 } else { 0 };
            let storage = |rb: u32, format: u32, samples: i32| {
                (gl.glBindRenderbuffer)(RENDERBUFFER, rb);
                if samples > 0 {
                    (gl.glRenderbufferStorageMultisampleANGLE)(RENDERBUFFER, samples, format, width, height);
                } else {
                    (gl.glRenderbufferStorage)(RENDERBUFFER, format, width, height);
                }
            };
            let mut fbo = 0;
            (gl.glGenFramebuffers)(1, &mut fbo);
            let mut rbs = [0u32; 2];
            (gl.glGenRenderbuffers)(2, rbs.as_mut_ptr());
            storage(rbs[0], color_format, samples);
            (gl.glBindFramebuffer)(FRAMEBUFFER, fbo);
            (gl.glFramebufferRenderbuffer)(FRAMEBUFFER, COLOR_ATTACHMENT0, RENDERBUFFER, rbs[0]);
            let depth_stencil = if self.attrs.depth || self.attrs.stencil {
                storage(rbs[1], DEPTH24_STENCIL8, samples);
                (gl.glFramebufferRenderbuffer)(FRAMEBUFFER, DEPTH_ATTACHMENT, RENDERBUFFER, rbs[1]);
                (gl.glFramebufferRenderbuffer)(FRAMEBUFFER, STENCIL_ATTACHMENT, RENDERBUFFER, rbs[1]);
                rbs[1]
            } else {
                (gl.glDeleteRenderbuffers)(1, &rbs[1]);
                0
            };
            let resolve = (samples > 0).then(|| {
                let (mut f, mut c) = (0, 0);
                (gl.glGenFramebuffers)(1, &mut f);
                (gl.glGenRenderbuffers)(1, &mut c);
                storage(c, color_format, 0);
                (gl.glBindFramebuffer)(FRAMEBUFFER, f);
                (gl.glFramebufferRenderbuffer)(FRAMEBUFFER, COLOR_ATTACHMENT0, RENDERBUFFER, c);
                (f, c)
            });
            self.db = DrawingBuffer { width, height, draw_fbo: fbo, draw_color: rbs[0], depth_stencil, resolve };
            // the page's renderbuffer binding is the page's
            (gl.glBindRenderbuffer)(RENDERBUFFER, self.name_of(GlObjectKind::Renderbuffer, self.bound_renderbuffer));
        }
    }

    fn map(&self, kind: GlObjectKind) -> &HashMap<u32, u32> {
        match kind {
            GlObjectKind::Buffer => &self.names.buffers,
            GlObjectKind::Shader => &self.names.shaders,
            GlObjectKind::Program => &self.names.programs,
            GlObjectKind::Texture => &self.names.textures,
            GlObjectKind::Framebuffer => &self.names.framebuffers,
            GlObjectKind::Renderbuffer => &self.names.renderbuffers,
            GlObjectKind::VertexArray => &self.names.vertex_arrays,
        }
    }

    /// The GL name of the page's object `id` (0 for none, or for an id never created).
    fn name_of(&self, kind: GlObjectKind, id: u32) -> u32 {
        if id == 0 {
            return 0;
        }
        self.map(kind).get(&id).copied().unwrap_or(0)
    }

    /// The page's id of GL name `name`.
    fn id_of(&self, kind: GlObjectKind, name: u32) -> u32 {
        self.map(kind).iter().find(|(_, &n)| n == name).map(|(&id, _)| id).unwrap_or(0)
    }

    /// The GL framebuffer the page's framebuffer `id` is (its id 0: the DrawingBuffer's).
    fn fbo(&self, id: u32) -> u32 {
        if id == 0 { self.db.draw_fbo } else { self.name_of(GlObjectKind::Framebuffer, id) }
    }

    fn drain_errors(&mut self, gl: &Gles) {
        loop {
            let e = unsafe { (gl.glGetError)() };
            if e == NO_ERROR {
                return;
            }
            self.errors.push(e);
        }
    }

    /// Run `ops`, the list's commands since the last call.
    pub fn run_new(&mut self, gl: &Gles, ops: &[GlOp]) {
        for op in ops {
            self.op(gl, op);
            self.drain_errors(gl);
        }
        self.done += ops.len();
    }

    /// Run the commands of `ops` not run yet.
    pub fn run(&mut self, gl: &Gles, ops: &[GlOp]) {
        for op in &ops[self.done.min(ops.len())..] {
            self.op(gl, op);
            self.drain_errors(gl);
        }
        self.done = ops.len();
    }

    fn op(&mut self, gl: &Gles, op: &GlOp) {
        use GlObjectKind as K;
        unsafe {
            match op {
                GlOp::CreateBuffer { id } => {
                    let mut n = 0;
                    (gl.glGenBuffers)(1, &mut n);
                    self.names.buffers.insert(*id, n);
                }
                GlOp::DeleteBuffer { id } => {
                    if let Some(n) = self.names.buffers.remove(id) {
                        (gl.glDeleteBuffers)(1, &n);
                    }
                }
                GlOp::CreateShader { id, kind } => {
                    let n = (gl.glCreateShader)(match kind {
                        GlShaderKind::Vertex => VERTEX_SHADER,
                        GlShaderKind::Fragment => FRAGMENT_SHADER,
                    });
                    self.names.shaders.insert(*id, n);
                    self.shader_kind.insert(*id, *kind);
                }
                GlOp::DeleteShader { id } => {
                    if let Some(&n) = self.names.shaders.get(id) {
                        (gl.glDeleteShader)(n);
                        self.deleted_shaders.push(*id);
                    }
                }
                GlOp::ShaderSource { shader, source } => {
                    let c = cstr(source);
                    let p = c.as_ptr();
                    (gl.glShaderSource)(self.name_of(K::Shader, *shader), 1, &p, std::ptr::null());
                    self.shader_source.insert(*shader, source.clone());
                }
                GlOp::CompileShader { shader } => (gl.glCompileShader)(self.name_of(K::Shader, *shader)),
                GlOp::CreateProgram { id } => {
                    let n = (gl.glCreateProgram)();
                    self.names.programs.insert(*id, n);
                }
                GlOp::DeleteProgram { id } => {
                    if let Some(&n) = self.names.programs.get(id) {
                        (gl.glDeleteProgram)(n);
                        self.deleted_programs.push(*id);
                    }
                }
                GlOp::AttachShader { program, shader } => {
                    (gl.glAttachShader)(self.name_of(K::Program, *program), self.name_of(K::Shader, *shader));
                    let list = self.attached.entry(*program).or_default();
                    if !list.contains(shader) {
                        list.push(*shader);
                    }
                }
                GlOp::DetachShader { program, shader } => {
                    (gl.glDetachShader)(self.name_of(K::Program, *program), self.name_of(K::Shader, *shader));
                    if let Some(list) = self.attached.get_mut(program) {
                        list.retain(|s| s != shader);
                    }
                }
                GlOp::BindAttribLocation { program, index, name } => {
                    let c = cstr(name);
                    (gl.glBindAttribLocation)(self.name_of(K::Program, *program), *index, c.as_ptr());
                }
                GlOp::LinkProgram { program } => (gl.glLinkProgram)(self.name_of(K::Program, *program)),
                GlOp::ValidateProgram { program } => (gl.glValidateProgram)(self.name_of(K::Program, *program)),
                GlOp::UseProgram { program } => (gl.glUseProgram)(self.name_of(K::Program, *program)),
                GlOp::CreateTexture { id } => {
                    let mut n = 0;
                    (gl.glGenTextures)(1, &mut n);
                    self.names.textures.insert(*id, n);
                }
                GlOp::DeleteTexture { id } => {
                    if let Some(n) = self.names.textures.remove(id) {
                        (gl.glDeleteTextures)(1, &n);
                    }
                }
                GlOp::CreateFramebuffer { id } => {
                    let mut n = 0;
                    (gl.glGenFramebuffers)(1, &mut n);
                    self.names.framebuffers.insert(*id, n);
                }
                GlOp::DeleteFramebuffer { id } => {
                    if let Some(n) = self.names.framebuffers.remove(id) {
                        (gl.glDeleteFramebuffers)(1, &n);
                        // deleting a bound framebuffer binds the default one
                        if self.draw_fb == *id {
                            self.draw_fb = 0;
                        }
                        if self.read_fb == *id {
                            self.read_fb = 0;
                        }
                        (gl.glBindFramebuffer)(DRAW_FRAMEBUFFER, self.fbo(self.draw_fb));
                        (gl.glBindFramebuffer)(READ_FRAMEBUFFER, self.fbo(self.read_fb));
                    }
                }
                GlOp::CreateRenderbuffer { id } => {
                    let mut n = 0;
                    (gl.glGenRenderbuffers)(1, &mut n);
                    self.names.renderbuffers.insert(*id, n);
                }
                GlOp::DeleteRenderbuffer { id } => {
                    if let Some(n) = self.names.renderbuffers.remove(id) {
                        (gl.glDeleteRenderbuffers)(1, &n);
                        if self.bound_renderbuffer == *id {
                            self.bound_renderbuffer = 0;
                        }
                    }
                }
                GlOp::CreateVertexArray { id } => {
                    let mut n = 0;
                    (gl.glGenVertexArraysOES)(1, &mut n);
                    self.names.vertex_arrays.insert(*id, n);
                }
                GlOp::DeleteVertexArray { id } => {
                    if let Some(n) = self.names.vertex_arrays.remove(id) {
                        (gl.glDeleteVertexArraysOES)(1, &n);
                    }
                }
                GlOp::BindVertexArray { id } => (gl.glBindVertexArrayOES)(self.name_of(K::VertexArray, *id)),
                GlOp::BindBuffer { target, buffer } => (gl.glBindBuffer)(*target, self.name_of(K::Buffer, *buffer)),
                GlOp::BufferData { target, size, data, usage } => {
                    let p = if data.is_empty() { std::ptr::null() } else { data.as_ptr() as *const c_void };
                    (gl.glBufferData)(*target, *size as isize, p, *usage);
                }
                GlOp::BufferSubData { target, offset, data } => {
                    (gl.glBufferSubData)(*target, *offset as isize, data.len() as isize, data.as_ptr() as *const c_void);
                }
                GlOp::EnableVertexAttribArray { index } => (gl.glEnableVertexAttribArray)(*index),
                GlOp::DisableVertexAttribArray { index } => (gl.glDisableVertexAttribArray)(*index),
                GlOp::VertexAttribPointer { index, size, ty, normalized, stride, offset } => {
                    (gl.glVertexAttribPointer)(*index, *size as i32, *ty, *normalized as u8, *stride as i32, *offset as usize as *const c_void);
                }
                GlOp::VertexAttrib { index, values } => (gl.glVertexAttrib4f)(*index, values[0], values[1], values[2], values[3]),
                GlOp::VertexAttribDivisor { index, divisor } => (gl.glVertexAttribDivisorANGLE)(*index, *divisor),
                GlOp::Uniform { program, name, value } => self.uniform(gl, *program, name, value),
                GlOp::TransformFeedbackVaryings { program, varyings, mode } => {
                    let cs: Vec<CString> = varyings.iter().map(|v| cstr(v)).collect();
                    let ps: Vec<*const i8> = cs.iter().map(|c| c.as_ptr()).collect();
                    (gl.glTransformFeedbackVaryings)(self.name_of(K::Program, *program), ps.len() as i32, ps.as_ptr(), *mode);
                }
                GlOp::UniformBlockBinding { program, index, binding } => {
                    (gl.glUniformBlockBinding)(self.name_of(K::Program, *program), *index, *binding);
                }
                GlOp::ActiveTexture { unit } => (gl.glActiveTexture)(*unit),
                GlOp::BindTexture { target, texture } => (gl.glBindTexture)(*target, self.name_of(K::Texture, *texture)),
                // The recorder hands every upload over as tightly packed RGBA8, already flipped, premultiplied and
                // converted as WebGL's pixel-store parameters say.
                GlOp::TexImage2D { target, level, width, height, pixels, .. } => {
                    (gl.glPixelStorei)(UNPACK_ALIGNMENT, 1);
                    let p = pixels.as_ref().map(|p| p.data.as_ptr() as *const c_void).unwrap_or(std::ptr::null());
                    (gl.glTexImage2D)(*target, *level, RGBA as i32, *width as i32, *height as i32, 0, RGBA, UNSIGNED_BYTE, p);
                }
                GlOp::TexSubImage2D { target, level, xoffset, yoffset, pixels, .. } => {
                    (gl.glPixelStorei)(UNPACK_ALIGNMENT, 1);
                    (gl.glTexSubImage2D)(*target, *level, *xoffset, *yoffset, pixels.width as i32, pixels.height as i32, RGBA, UNSIGNED_BYTE, pixels.data.as_ptr() as *const c_void);
                }
                GlOp::TexStorage { target, levels, internal_format, width, height, depth } => {
                    if *depth == 0 {
                        (gl.glTexStorage2D)(*target, *levels, *internal_format, *width, *height)
                    } else {
                        (gl.glTexStorage3D)(*target, *levels, *internal_format, *width, *height, *depth)
                    }
                }
                GlOp::TexParameter { target, pname, param } => (gl.glTexParameterf)(*target, *pname, *param),
                GlOp::GenerateMipmap { target } => (gl.glGenerateMipmap)(*target),
                GlOp::PixelStorei { pname, param } => {
                    // WebGL's own and the (un)packing ones are applied by the recorder and the readback
                    if !WEBGL_PIXEL_STORE.contains(pname) && *pname != UNPACK_ALIGNMENT && *pname != PACK_ALIGNMENT {
                        (gl.glPixelStorei)(*pname, *param);
                    }
                }
                GlOp::BindFramebuffer { target, framebuffer } => {
                    match *target {
                        DRAW_FRAMEBUFFER => self.draw_fb = *framebuffer,
                        READ_FRAMEBUFFER => self.read_fb = *framebuffer,
                        _ => {
                            self.draw_fb = *framebuffer;
                            self.read_fb = *framebuffer;
                        }
                    }
                    (gl.glBindFramebuffer)(*target, self.fbo(*framebuffer));
                }
                GlOp::BindRenderbuffer { target, renderbuffer } => {
                    self.bound_renderbuffer = *renderbuffer;
                    (gl.glBindRenderbuffer)(*target, self.name_of(K::Renderbuffer, *renderbuffer));
                }
                GlOp::RenderbufferStorage { target, internal_format, width, height } => {
                    // WebGL 1's DEPTH_STENCIL is DEPTH24_STENCIL8 on the GPU (Blink's WebGLRenderbuffer)
                    let format = if *internal_format == 0x84F9 { DEPTH24_STENCIL8 } else { *internal_format };
                    (gl.glRenderbufferStorage)(*target, format, *width as i32, *height as i32);
                    self.renderbuffer_format.insert(self.bound_renderbuffer, *internal_format);
                }
                GlOp::FramebufferTexture2D { target, attachment, textarget, texture, level } => {
                    (gl.glFramebufferTexture2D)(*target, *attachment, *textarget, self.name_of(K::Texture, *texture), *level);
                }
                GlOp::FramebufferRenderbuffer { target, attachment, rb_target, renderbuffer } => {
                    let rb = self.name_of(K::Renderbuffer, *renderbuffer);
                    if *attachment == 0x821A {
                        // DEPTH_STENCIL_ATTACHMENT: both halves
                        (gl.glFramebufferRenderbuffer)(*target, DEPTH_ATTACHMENT, *rb_target, rb);
                        (gl.glFramebufferRenderbuffer)(*target, STENCIL_ATTACHMENT, *rb_target, rb);
                    } else {
                        (gl.glFramebufferRenderbuffer)(*target, *attachment, *rb_target, rb);
                    }
                }
                GlOp::Viewport { x, y, width, height } => (gl.glViewport)(*x, *y, *width, *height),
                GlOp::Scissor { x, y, width, height } => (gl.glScissor)(*x, *y, *width, *height),
                GlOp::ClearColor { r, g, b, a } => (gl.glClearColor)(*r, *g, *b, *a),
                GlOp::ClearDepth { depth } => (gl.glClearDepthf)(*depth),
                GlOp::ClearStencil { s } => (gl.glClearStencil)(*s),
                GlOp::Clear { mask } => (gl.glClear)(*mask),
                GlOp::ColorMask { r, g, b, a } => (gl.glColorMask)(*r as u8, *g as u8, *b as u8, *a as u8),
                GlOp::DepthMask { flag } => (gl.glDepthMask)(*flag as u8),
                GlOp::DepthFunc { func } => (gl.glDepthFunc)(*func),
                GlOp::DepthRange { near, far } => (gl.glDepthRangef)(*near, *far),
                GlOp::StencilMask { mask } => (gl.glStencilMask)(*mask),
                GlOp::StencilFunc { face, func, reference, mask } => (gl.glStencilFuncSeparate)(*face, *func, *reference, *mask),
                GlOp::StencilOp { face, fail, zfail, zpass } => (gl.glStencilOpSeparate)(*face, *fail, *zfail, *zpass),
                GlOp::Enable { cap } => (gl.glEnable)(*cap),
                GlOp::Disable { cap } => (gl.glDisable)(*cap),
                GlOp::BlendFuncSeparate { src_rgb, dst_rgb, src_alpha, dst_alpha } => (gl.glBlendFuncSeparate)(*src_rgb, *dst_rgb, *src_alpha, *dst_alpha),
                GlOp::BlendEquationSeparate { rgb, alpha } => (gl.glBlendEquationSeparate)(*rgb, *alpha),
                GlOp::BlendColor { r, g, b, a } => (gl.glBlendColor)(*r, *g, *b, *a),
                GlOp::CullFace { mode } => (gl.glCullFace)(*mode),
                GlOp::FrontFace { mode } => (gl.glFrontFace)(*mode),
                GlOp::LineWidth { width } => (gl.glLineWidth)(*width),
                GlOp::PolygonOffset { factor, units } => (gl.glPolygonOffset)(*factor, *units),
                GlOp::SampleCoverage { value, invert } => (gl.glSampleCoverage)(*value, *invert as u8),
                GlOp::Hint { target, mode } => (gl.glHint)(*target, *mode),
                GlOp::DrawArrays { mode, first, count } => (gl.glDrawArrays)(*mode, *first, *count),
                GlOp::DrawElements { mode, count, ty, offset } => (gl.glDrawElements)(*mode, *count, *ty, *offset as usize as *const c_void),
                GlOp::DrawArraysInstanced { mode, first, count, instances } => (gl.glDrawArraysInstancedANGLE)(*mode, *first, *count, *instances),
                GlOp::DrawElementsInstanced { mode, count, ty, offset, instances } => {
                    (gl.glDrawElementsInstancedANGLE)(*mode, *count, *ty, *offset as usize as *const c_void, *instances)
                }
                GlOp::Finish => (gl.glFinish)(),
                GlOp::Flush => (gl.glFlush)(),
                GlOp::RaiseError { code } => self.errors.push(*code),
                GlOp::ResizeDrawingBuffer { width, height } => {
                    let (w, h) = ((*width).max(1) as i32, (*height).max(1) as i32);
                    // Blink's Reshape: the same size keeps the buffer, a new one reallocates it cleared
                    if (w, h) != (self.db.width, self.db.height) {
                        self.allocate(gl, w, h);
                        (gl.glBindFramebuffer)(DRAW_FRAMEBUFFER, self.fbo(self.draw_fb));
                        (gl.glBindFramebuffer)(READ_FRAMEBUFFER, self.fbo(self.read_fb));
                    }
                }
                GlOp::EnableExtension { name } => self.enable_extension(gl, name, 0),
            }
        }
    }

    /// `getExtension(name)` enabled `name` (Blink's `EnsureExtensionEnabled`): the GL extensions it asks of the
    /// context -- a `GL_CHROMIUM_*` one as the passthrough decoder answers it with ANGLE's -- and the WebGL
    /// extensions it turns on with it.
    fn enable_extension(&mut self, gl: &Gles, name: &str, depth: u32) {
        let Some((_, gl_names, implied)) = super::profile::ENABLES.iter().find(|(n, ..)| *n == name) else { return };
        for g in gl_names.iter().flat_map(|g| std::iter::once(*g).chain(super::profile::angle_names(g).iter().copied())) {
            let c = cstr(g);
            // SAFETY: a NUL-terminated extension name; ANGLE ignores one it cannot enable
            unsafe { (gl.glRequestExtensionANGLE)(c.as_ptr()) };
        }
        if depth < 2 {
            for i in implied.iter() {
                self.enable_extension(gl, i, depth + 1);
            }
        }
        // a name this device cannot enable is the engine's question, not a page error (every earlier op's errors
        // were drained after it)
        if depth == 0 {
            // SAFETY: plain GL call on the current context
            while unsafe { (gl.glGetError)() } != NO_ERROR {}
        }
    }

    fn uniform(&mut self, gl: &Gles, program: u32, name: &str, value: &GlUniformValue) {
        let p = self.name_of(GlObjectKind::Program, program);
        let c = cstr(name);
        unsafe {
            let loc = (gl.glGetUniformLocation)(p, c.as_ptr());
            if loc < 0 {
                return;
            }
            match value {
                GlUniformValue::Float { components, values } => {
                    let n = (values.len() / (*components).max(1) as usize) as i32;
                    let f = match components { 1 => gl.glUniform1fv, 2 => gl.glUniform2fv, 3 => gl.glUniform3fv, _ => gl.glUniform4fv };
                    f(loc, n, values.as_ptr());
                }
                GlUniformValue::Int { components, values } => {
                    let n = (values.len() / (*components).max(1) as usize) as i32;
                    let f = match components { 1 => gl.glUniform1iv, 2 => gl.glUniform2iv, 3 => gl.glUniform3iv, _ => gl.glUniform4iv };
                    f(loc, n, values.as_ptr());
                }
                GlUniformValue::UInt { components, values } => {
                    let n = (values.len() / (*components).max(1) as usize) as i32;
                    let f = match components { 1 => gl.glUniform1uiv, 2 => gl.glUniform2uiv, 3 => gl.glUniform3uiv, _ => gl.glUniform4uiv };
                    f(loc, n, values.as_ptr());
                }
                GlUniformValue::Matrix { rows, cols, transpose, values } => {
                    let n = (values.len() / ((*rows as usize) * (*cols as usize)).max(1)) as i32;
                    // GL's matCxR: C columns, R rows
                    let f = match (cols, rows) {
                        (2, 2) => gl.glUniformMatrix2fv,
                        (3, 3) => gl.glUniformMatrix3fv,
                        (2, 3) => gl.glUniformMatrix2x3fv,
                        (3, 2) => gl.glUniformMatrix3x2fv,
                        (2, 4) => gl.glUniformMatrix2x4fv,
                        (4, 2) => gl.glUniformMatrix4x2fv,
                        (3, 4) => gl.glUniformMatrix3x4fv,
                        (4, 3) => gl.glUniformMatrix4x3fv,
                        _ => gl.glUniformMatrix4fv,
                    };
                    f(loc, n, *transpose as u8, values.as_ptr());
                }
            }
        }
    }

    /// Resolve the DrawingBuffer when it is multisampled; the framebuffer its pixels are read from.
    fn resolved(&mut self, gl: &Gles) -> u32 {
        let Some((resolve_fbo, _)) = self.db.resolve else { return self.db.draw_fbo };
        unsafe {
            (gl.glBindFramebuffer)(READ_FRAMEBUFFER, self.db.draw_fbo);
            (gl.glBindFramebuffer)(DRAW_FRAMEBUFFER, resolve_fbo);
            let (w, h) = (self.db.width, self.db.height);
            (gl.glBlitFramebufferANGLE)(0, 0, w, h, 0, 0, w, h, COLOR_BUFFER_BIT, NEAREST);
        }
        resolve_fbo
    }

    /// Restore the page's framebuffer bindings after the engine's own reads.
    fn rebind(&self, gl: &Gles) {
        unsafe {
            (gl.glBindFramebuffer)(DRAW_FRAMEBUFFER, self.fbo(self.draw_fb));
            (gl.glBindFramebuffer)(READ_FRAMEBUFFER, self.fbo(self.read_fb));
        }
    }

    /// RGBA8 pixels of `fbo`'s color, rows bottom-up as GL reads them.
    fn read(&self, gl: &Gles, fbo: u32, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        unsafe {
            (gl.glBindFramebuffer)(READ_FRAMEBUFFER, fbo);
            (gl.glPixelStorei)(PACK_ALIGNMENT, 1);
            (gl.glReadPixels)(x, y, w as i32, h as i32, RGBA, UNSIGNED_BYTE, out.as_mut_ptr() as *mut c_void);
        }
        out
    }

    /// The DrawingBuffer as the compositor reads it: top row first.
    pub fn readback(&mut self, gl: &Gles) -> Rgba8 {
        let fbo = self.resolved(gl);
        let (w, h) = (self.db.width as u32, self.db.height as u32);
        let rows = self.read(gl, fbo, 0, 0, w, h);
        self.rebind(gl);
        unsafe { while (gl.glGetError)() != NO_ERROR {} }
        let stride = w as usize * 4;
        let mut data = vec![0u8; rows.len()];
        for y in 0..h as usize {
            data[y * stride..(y + 1) * stride].copy_from_slice(&rows[(h as usize - 1 - y) * stride..(h as usize - y) * stride]);
        }
        Rgba8 { width: w, height: h, data }
    }

    fn string_from(len: i32, f: impl FnOnce(i32, *mut i32, *mut i8)) -> String {
        let mut buf = vec![0u8; len.max(1) as usize + 1];
        let mut n = 0;
        f(buf.len() as i32, &mut n, buf.as_mut_ptr() as *mut i8);
        buf.truncate(n.max(0) as usize);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// The active uniform `name` (or `name[0]`) of `program`: (index, size, type).
    fn find_uniform(&self, gl: &Gles, program: u32, name: &str) -> Option<(u32, i32, u32)> {
        let base = name.strip_suffix("[0]").unwrap_or(name);
        unsafe {
            let mut count = 0;
            let mut maxlen = 0;
            (gl.glGetProgramiv)(program, ACTIVE_UNIFORMS, &mut count);
            (gl.glGetProgramiv)(program, ACTIVE_UNIFORM_MAX_LENGTH, &mut maxlen);
            for i in 0..count.max(0) as u32 {
                let (mut size, mut ty) = (0, 0);
                let n = Self::string_from(maxlen, |cap, len, buf| (gl.glGetActiveUniform)(program, i, cap, len, &mut size, &mut ty, buf));
                if n == name || n.strip_suffix("[0]") == Some(base) || n == base {
                    return Some((i, size, ty));
                }
            }
        }
        None
    }

    pub fn query(&mut self, gl: &Gles, q: &GlQuery) -> R {
        let r = unsafe { self.query_inner(gl, q) };
        // the engine's reads raise nothing of the page's
        unsafe { while (gl.glGetError)() != NO_ERROR {} }
        r
    }

    unsafe fn query_inner(&mut self, gl: &Gles, q: &GlQuery) -> R {
        use GlObjectKind as K;
        let iv = |f: &dyn Fn(*mut i32)| -> i32 {
            let mut v = 0;
            f(&mut v);
            v
        };
        match q {
            GlQuery::ShaderParameter { shader, pname } => {
                let Some(&s) = self.names.shaders.get(shader) else { return R::None };
                match *pname {
                    COMPILE_STATUS | DELETE_STATUS => R::Bool(iv(&|v| (gl.glGetShaderiv)(s, *pname, v)) != 0),
                    SHADER_TYPE => R::UInt(match self.shader_kind.get(shader) {
                        Some(GlShaderKind::Vertex) => VERTEX_SHADER,
                        _ => FRAGMENT_SHADER,
                    }),
                    _ => R::None,
                }
            }
            GlQuery::ShaderInfoLog { shader } => {
                let Some(&s) = self.names.shaders.get(shader) else { return R::None };
                let len = iv(&|v| (gl.glGetShaderiv)(s, INFO_LOG_LENGTH, v));
                R::Str(Self::string_from(len, |cap, n, buf| (gl.glGetShaderInfoLog)(s, cap, n, buf)))
            }
            GlQuery::ShaderSource { shader } => self.shader_source.get(shader).map(|s| R::Str(s.clone())).unwrap_or(R::None),
            GlQuery::ProgramParameter { program, pname } => {
                let Some(&p) = self.names.programs.get(program) else { return R::None };
                match *pname {
                    LINK_STATUS | VALIDATE_STATUS | DELETE_STATUS => R::Bool(iv(&|v| (gl.glGetProgramiv)(p, *pname, v)) != 0),
                    ATTACHED_SHADERS | ACTIVE_ATTRIBUTES | ACTIVE_UNIFORMS | ACTIVE_UNIFORM_BLOCKS | TRANSFORM_FEEDBACK_VARYINGS => {
                        R::Int(iv(&|v| (gl.glGetProgramiv)(p, *pname, v)))
                    }
                    TRANSFORM_FEEDBACK_BUFFER_MODE => R::UInt(iv(&|v| (gl.glGetProgramiv)(p, *pname, v)) as u32),
                    _ => R::None,
                }
            }
            GlQuery::ProgramInfoLog { program } => {
                let Some(&p) = self.names.programs.get(program) else { return R::None };
                let len = iv(&|v| (gl.glGetProgramiv)(p, INFO_LOG_LENGTH, v));
                R::Str(Self::string_from(len, |cap, n, buf| (gl.glGetProgramInfoLog)(p, cap, n, buf)))
            }
            GlQuery::AttachedShaders { program } => {
                if !self.names.programs.contains_key(program) {
                    return R::None;
                }
                R::Ints(self.attached.get(program).map(|l| l.iter().map(|&s| s as i32).collect()).unwrap_or_default())
            }
            GlQuery::AttribLocation { program, name } => {
                let c = cstr(name);
                R::Int((gl.glGetAttribLocation)(self.name_of(K::Program, *program), c.as_ptr()))
            }
            GlQuery::UniformLocation { program, name } => {
                let c = cstr(name);
                let loc = (gl.glGetUniformLocation)(self.name_of(K::Program, *program), c.as_ptr());
                if loc < 0 { R::None } else { R::Int(loc) }
            }
            GlQuery::ActiveAttrib { program, index } | GlQuery::ActiveUniform { program, index } => {
                let p = self.name_of(K::Program, *program);
                let attrib = matches!(q, GlQuery::ActiveAttrib { .. });
                let (count_name, max_name) = if attrib { (ACTIVE_ATTRIBUTES, ACTIVE_ATTRIBUTE_MAX_LENGTH) } else { (ACTIVE_UNIFORMS, ACTIVE_UNIFORM_MAX_LENGTH) };
                if *index as i32 >= iv(&|v| (gl.glGetProgramiv)(p, count_name, v)) {
                    return R::None;
                }
                let maxlen = iv(&|v| (gl.glGetProgramiv)(p, max_name, v));
                let (mut size, mut ty) = (0, 0);
                let name = Self::string_from(maxlen, |cap, n, buf| {
                    if attrib {
                        (gl.glGetActiveAttrib)(p, *index, cap, n, &mut size, &mut ty, buf)
                    } else {
                        (gl.glGetActiveUniform)(p, *index, cap, n, &mut size, &mut ty, buf)
                    }
                });
                R::ActiveInfo { name, size, ty }
            }
            GlQuery::Uniform { program, name } => {
                let p = self.name_of(K::Program, *program);
                let Some((_, _, ty)) = self.find_uniform(gl, p, name) else { return R::None };
                let c = cstr(name);
                let loc = (gl.glGetUniformLocation)(p, c.as_ptr());
                if loc < 0 {
                    return R::None;
                }
                let n = uniform_components(ty);
                match uniform_kind(ty) {
                    'b' => {
                        let mut v = vec![0i32; n];
                        (gl.glGetUniformiv)(p, loc, v.as_mut_ptr());
                        R::Bools(v.iter().map(|x| *x != 0).collect())
                    }
                    'i' => {
                        let mut v = vec![0i32; n];
                        (gl.glGetUniformiv)(p, loc, v.as_mut_ptr());
                        R::Ints(v)
                    }
                    'u' => {
                        let mut v = vec![0u32; n];
                        (gl.glGetUniformuiv)(p, loc, v.as_mut_ptr());
                        R::UInts(v)
                    }
                    _ => {
                        let mut v = vec![0f32; n];
                        (gl.glGetUniformfv)(p, loc, v.as_mut_ptr());
                        R::Floats(v)
                    }
                }
            }
            GlQuery::Error { consumed } => R::UInt(self.errors.get(*consumed as usize).copied().unwrap_or(NO_ERROR)),
            GlQuery::ReadPixels { x, y, width, height } => {
                let fbo = if self.read_fb == 0 { self.resolved(gl) } else { self.fbo(self.read_fb) };
                let px = self.read(gl, fbo, *x, *y, *width, *height);
                self.rebind(gl);
                R::Bytes(px)
            }
            GlQuery::IsObject { kind, id } => R::Bool(match kind {
                K::Shader => self.names.shaders.contains_key(id) && !self.deleted_shaders.contains(id),
                K::Program => self.names.programs.contains_key(id) && !self.deleted_programs.contains(id),
                k => self.map(*k).contains_key(id),
            }),
            GlQuery::IsEnabled { cap } => R::Bool((gl.glIsEnabled)(*cap) != 0),
            GlQuery::BufferParameter { target, pname } => {
                let v = iv(&|v| (gl.glGetBufferParameteriv)(*target, *pname, v));
                match *pname {
                    BUFFER_SIZE => R::Int(v),
                    BUFFER_USAGE => R::UInt(v as u32),
                    _ => R::None,
                }
            }
            GlQuery::VertexAttrib { index, pname } => {
                let get = |p: u32| iv(&|v| (gl.glGetVertexAttribiv)(*index, p, v));
                match *pname {
                    VERTEX_ATTRIB_ARRAY_ENABLED | VERTEX_ATTRIB_ARRAY_NORMALIZED | VERTEX_ATTRIB_ARRAY_INTEGER => R::Bool(get(*pname) != 0),
                    VERTEX_ATTRIB_ARRAY_SIZE | VERTEX_ATTRIB_ARRAY_STRIDE | VERTEX_ATTRIB_ARRAY_DIVISOR => R::Int(get(*pname)),
                    VERTEX_ATTRIB_ARRAY_TYPE => R::UInt(get(*pname) as u32),
                    VERTEX_ATTRIB_ARRAY_BUFFER_BINDING => {
                        let id = self.id_of(K::Buffer, get(*pname) as u32);
                        if id == 0 { R::None } else { R::UInt(id) }
                    }
                    CURRENT_VERTEX_ATTRIB => {
                        let mut v = [0f32; 4];
                        (gl.glGetVertexAttribfv)(*index, *pname, v.as_mut_ptr());
                        R::Floats(v.to_vec())
                    }
                    _ => R::None,
                }
            }
            GlQuery::VertexAttribOffset { index, .. } => {
                let mut p: *mut c_void = std::ptr::null_mut();
                (gl.glGetVertexAttribPointerv)(*index, VERTEX_ATTRIB_ARRAY_POINTER, &mut p);
                R::Int(p as usize as i32)
            }
            GlQuery::TexParameter { target, pname } => {
                let mut f = 0f32;
                (gl.glGetTexParameterfv)(*target, *pname, &mut f);
                // TEXTURE_MAX_ANISOTROPY_EXT and the LOD clamps are floats; every other parameter an enum or int
                if matches!(*pname, 0x84FE | 0x813A | 0x813B) { R::Float(f) } else { R::UInt(f as u32) }
            }
            GlQuery::FramebufferStatus { target } => R::UInt((gl.glCheckFramebufferStatus)(*target)),
            GlQuery::RenderbufferParameter { target, pname } => {
                if *pname == RENDERBUFFER_INTERNAL_FORMAT {
                    return R::UInt(self.renderbuffer_format.get(&self.bound_renderbuffer).copied().unwrap_or(0x8056));
                }
                R::Int(iv(&|v| (gl.glGetRenderbufferParameteriv)(*target, *pname, v)))
            }
            // the binding answers it from its own attachment record, as with the software device
            GlQuery::FramebufferAttachmentParameter { .. } => R::None,
            GlQuery::UniformType { program, name } => {
                let p = self.name_of(K::Program, *program);
                self.find_uniform(gl, p, name).map(|(_, _, ty)| R::UInt(ty)).unwrap_or(R::None)
            }
            GlQuery::UniformBlockIndex { program, name } => {
                let c = cstr(name);
                R::UInt((gl.glGetUniformBlockIndex)(self.name_of(K::Program, *program), c.as_ptr()))
            }
            GlQuery::UniformBlockName { program, index } => {
                let p = self.name_of(K::Program, *program);
                if *index as i32 >= iv(&|v| (gl.glGetProgramiv)(p, ACTIVE_UNIFORM_BLOCKS, v)) {
                    return R::None;
                }
                R::Str(Self::string_from(256, |cap, n, buf| (gl.glGetActiveUniformBlockName)(p, *index, cap, n, buf)))
            }
            GlQuery::UniformBlockParameter { program, index, pname } => {
                let p = self.name_of(K::Program, *program);
                match *pname {
                    UNIFORM_BLOCK_ACTIVE_UNIFORM_INDICES => {
                        let n = iv(&|v| (gl.glGetActiveUniformBlockiv)(p, *index, UNIFORM_BLOCK_ACTIVE_UNIFORMS, v));
                        let mut v = vec![0i32; n.max(0) as usize];
                        (gl.glGetActiveUniformBlockiv)(p, *index, *pname, v.as_mut_ptr());
                        R::UInts(v.into_iter().map(|x| x as u32).collect())
                    }
                    UNIFORM_BLOCK_REFERENCED_BY_VERTEX_SHADER | UNIFORM_BLOCK_REFERENCED_BY_FRAGMENT_SHADER => {
                        R::Bool(iv(&|v| (gl.glGetActiveUniformBlockiv)(p, *index, *pname, v)) != 0)
                    }
                    _ => R::UInt(iv(&|v| (gl.glGetActiveUniformBlockiv)(p, *index, *pname, v)) as u32),
                }
            }
            GlQuery::UniformIndices { program, names } => {
                let cs: Vec<CString> = names.iter().map(|n| cstr(n)).collect();
                let ps: Vec<*const i8> = cs.iter().map(|c| c.as_ptr()).collect();
                let mut out = vec![0u32; ps.len()];
                (gl.glGetUniformIndices)(self.name_of(K::Program, *program), ps.len() as i32, ps.as_ptr(), out.as_mut_ptr());
                R::UInts(out)
            }
            GlQuery::ActiveUniforms { program, indices, pname } => {
                let mut out = vec![0i32; indices.len()];
                (gl.glGetActiveUniformsiv)(self.name_of(K::Program, *program), indices.len() as i32, indices.as_ptr(), *pname, out.as_mut_ptr());
                match *pname {
                    UNIFORM_IS_ROW_MAJOR => R::Bools(out.iter().map(|x| *x != 0).collect()),
                    UNIFORM_TYPE => R::UInts(out.iter().map(|x| *x as u32).collect()),
                    _ => R::Ints(out),
                }
            }
            GlQuery::FragDataLocation { program, name } => {
                let c = cstr(name);
                R::Int((gl.glGetFragDataLocation)(self.name_of(K::Program, *program), c.as_ptr()))
            }
            GlQuery::TransformFeedbackVarying { program, index } => {
                let p = self.name_of(K::Program, *program);
                if *index as i32 >= iv(&|v| (gl.glGetProgramiv)(p, TRANSFORM_FEEDBACK_VARYINGS, v)) {
                    return R::None;
                }
                let (mut size, mut ty) = (0, 0);
                let name = Self::string_from(256, |cap, n, buf| (gl.glGetTransformFeedbackVarying)(p, *index, cap, n, &mut size, &mut ty, buf));
                R::ActiveInfo { name, size, ty }
            }
            GlQuery::Get { pname } => super::oracle::get(gl, *pname),
            GlQuery::Internalformat { target, internal_format, pname } => {
                let mut n = 0i32;
                (gl.glGetInternalformativ)(*target, *internal_format, 0x9380, 1, &mut n);
                let mut v = vec![0i32; n.max(0) as usize];
                if *pname == 0x80A9 && n > 0 {
                    (gl.glGetInternalformativ)(*target, *internal_format, *pname, n, v.as_mut_ptr());
                }
                R::Ints(v)
            }
        }
    }
}

/// How many scalars a uniform of GL type `ty` reads back as.
fn uniform_components(ty: u32) -> usize {
    match ty {
        0x8B50 | 0x8B53 | 0x8B57 | 0x8DC6 => 2, // vec2, ivec2, bvec2, uvec2
        0x8B51 | 0x8B54 | 0x8B58 | 0x8DC7 => 3,
        0x8B52 | 0x8B55 | 0x8B59 | 0x8DC8 => 4,
        0x8B5A => 4,                            // mat2
        0x8B5B => 9,                            // mat3
        0x8B5C => 16,                           // mat4
        0x8B65 | 0x8B67 => 6,                   // mat2x3, mat3x2
        0x8B66 | 0x8B69 => 8,                   // mat2x4, mat4x2
        0x8B68 | 0x8B6A => 12,                  // mat3x4, mat4x3
        _ => 1,
    }
}

/// 'f' float, 'i' int / sampler, 'u' uint, 'b' bool.
fn uniform_kind(ty: u32) -> char {
    match ty {
        0x8B56 | 0x8B57 | 0x8B58 | 0x8B59 => 'b',
        0x1404 | 0x8B53 | 0x8B54 | 0x8B55 => 'i',
        0x1405 | 0x8DC6 | 0x8DC7 | 0x8DC8 => 'u',
        0x1406 | 0x8B50 | 0x8B51 | 0x8B52 | 0x8B5A | 0x8B5B | 0x8B5C | 0x8B65..=0x8B6A => 'f',
        _ => 'i', // samplers
    }
}
