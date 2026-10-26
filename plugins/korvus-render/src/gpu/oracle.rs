//! The device's half of the `deviceOracle.webgl` block WebGL's getters read: every value Blink 150 asks
//! GL for, asked of this PC's GPU through Chrome's ANGLE in a fresh WebGL context of each version -- the limits
//! `getParameter` answers, the shader precision formats, the extensions `getSupportedExtensions` lists, the sample
//! counts `getInternalformatParameter` reports, and `WEBGL_debug_renderer_info`'s two strings. The browser's own
//! strings (`VENDOR` "WebKit", `VERSION` "WebGL 1.0 (OpenGL ES 2.0 Chromium)", ...) are Blink's, not the device's:
//! they stay the persona's.

use serde_json::{json, Map, Value};
use spec::GlContextAttributes;

use super::device::Device;
use super::exec::Exec;
use super::gles::Gles;
use super::profile;

/// How Blink reads a `getParameter` name from GL (`WebGLRenderingContextBase::Get*Parameter`).
#[derive(Clone, Copy)]
enum K {
    Int,
    Uint,
    Float,
    Int64,
    Bool,
    Ints(usize),
    Floats(usize),
    Bools(usize),
}

/// Blink 150's `getParameter` cases that ask GL (`return Get*Parameter(...)`): WebGL 1's switch
/// (`webgl_rendering_context_base.cc`), then the names WebGL 2's adds (`webgl2_rendering_context_base.cc`), plus the
/// three bit depths Blink asks GL for unless the drawing buffer has none.
const V1: &[(u32, K)] = &[
    (0x84e0, K::Uint), // GL_ACTIVE_TEXTURE
    (0x846e, K::Floats(2)), // GL_ALIASED_LINE_WIDTH_RANGE
    (0x846d, K::Floats(2)), // GL_ALIASED_POINT_SIZE_RANGE
    (0x0be2, K::Bool), // GL_BLEND
    (0x8005, K::Floats(4)), // GL_BLEND_COLOR
    (0x80ca, K::Uint), // GL_BLEND_DST_ALPHA
    (0x80c8, K::Uint), // GL_BLEND_DST_RGB
    (0x883d, K::Uint), // GL_BLEND_EQUATION_ALPHA
    (0x8009, K::Uint), // GL_BLEND_EQUATION_RGB
    (0x80cb, K::Uint), // GL_BLEND_SRC_ALPHA
    (0x80c9, K::Uint), // GL_BLEND_SRC_RGB
    (0x0d54, K::Int), // GL_BLUE_BITS
    (0x0c22, K::Floats(4)), // GL_COLOR_CLEAR_VALUE
    (0x0c23, K::Bools(4)), // GL_COLOR_WRITEMASK
    (0x0b44, K::Bool), // GL_CULL_FACE
    (0x0b45, K::Uint), // GL_CULL_FACE_MODE
    (0x0b73, K::Float), // GL_DEPTH_CLEAR_VALUE
    (0x0b74, K::Uint), // GL_DEPTH_FUNC
    (0x0b70, K::Floats(2)), // GL_DEPTH_RANGE
    (0x0b72, K::Bool), // GL_DEPTH_WRITEMASK
    (0x0bd0, K::Bool), // GL_DITHER
    (0x0b46, K::Uint), // GL_FRONT_FACE
    (0x8192, K::Uint), // GL_GENERATE_MIPMAP_HINT
    (0x0d53, K::Int), // GL_GREEN_BITS
    (0x8b9b, K::Int), // GL_IMPLEMENTATION_COLOR_READ_FORMAT
    (0x8b9a, K::Int), // GL_IMPLEMENTATION_COLOR_READ_TYPE
    (0x0b21, K::Float), // GL_LINE_WIDTH
    (0x8b4d, K::Int), // GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS
    (0x851c, K::Int), // GL_MAX_CUBE_MAP_TEXTURE_SIZE
    (0x8dfd, K::Int), // GL_MAX_FRAGMENT_UNIFORM_VECTORS
    (0x84e8, K::Int), // GL_MAX_RENDERBUFFER_SIZE
    (0x8872, K::Int), // GL_MAX_TEXTURE_IMAGE_UNITS
    (0x0d33, K::Int), // GL_MAX_TEXTURE_SIZE
    (0x8dfc, K::Int), // GL_MAX_VARYING_VECTORS
    (0x8869, K::Int), // GL_MAX_VERTEX_ATTRIBS
    (0x8b4c, K::Int), // GL_MAX_VERTEX_TEXTURE_IMAGE_UNITS
    (0x8dfb, K::Int), // GL_MAX_VERTEX_UNIFORM_VECTORS
    (0x0d3a, K::Ints(2)), // GL_MAX_VIEWPORT_DIMS
    (0x0d05, K::Int), // GL_PACK_ALIGNMENT
    (0x8038, K::Float), // GL_POLYGON_OFFSET_FACTOR
    (0x8037, K::Bool), // GL_POLYGON_OFFSET_FILL
    (0x2a00, K::Float), // GL_POLYGON_OFFSET_UNITS
    (0x0d52, K::Int), // GL_RED_BITS
    (0x809e, K::Bool), // GL_SAMPLE_ALPHA_TO_COVERAGE
    (0x80a8, K::Int), // GL_SAMPLE_BUFFERS
    (0x80a0, K::Bool), // GL_SAMPLE_COVERAGE
    (0x80ab, K::Bool), // GL_SAMPLE_COVERAGE_INVERT
    (0x80aa, K::Float), // GL_SAMPLE_COVERAGE_VALUE
    (0x80a9, K::Int), // GL_SAMPLES
    (0x0c10, K::Ints(4)), // GL_SCISSOR_BOX
    (0x0c11, K::Bool), // GL_SCISSOR_TEST
    (0x8801, K::Uint), // GL_STENCIL_BACK_FAIL
    (0x8800, K::Uint), // GL_STENCIL_BACK_FUNC
    (0x8802, K::Uint), // GL_STENCIL_BACK_PASS_DEPTH_FAIL
    (0x8803, K::Uint), // GL_STENCIL_BACK_PASS_DEPTH_PASS
    (0x8ca3, K::Int), // GL_STENCIL_BACK_REF
    (0x8ca4, K::Uint), // GL_STENCIL_BACK_VALUE_MASK
    (0x8ca5, K::Uint), // GL_STENCIL_BACK_WRITEMASK
    (0x0b91, K::Int), // GL_STENCIL_CLEAR_VALUE
    (0x0b94, K::Uint), // GL_STENCIL_FAIL
    (0x0b92, K::Uint), // GL_STENCIL_FUNC
    (0x0b95, K::Uint), // GL_STENCIL_PASS_DEPTH_FAIL
    (0x0b96, K::Uint), // GL_STENCIL_PASS_DEPTH_PASS
    (0x0b97, K::Int), // GL_STENCIL_REF
    (0x0b93, K::Uint), // GL_STENCIL_VALUE_MASK
    (0x0b98, K::Uint), // GL_STENCIL_WRITEMASK
    (0x0d50, K::Int), // GL_SUBPIXEL_BITS
    (0x0cf5, K::Int), // GL_UNPACK_ALIGNMENT
    (0x0ba2, K::Ints(4)), // GL_VIEWPORT
];

const V2: &[(u32, K)] = &[
    (0x8b8b, K::Uint), // GL_FRAGMENT_SHADER_DERIVATIVE_HINT
    (0x8073, K::Int), // GL_MAX_3D_TEXTURE_SIZE
    (0x88ff, K::Int), // GL_MAX_ARRAY_TEXTURE_LAYERS
    (0x8cdf, K::Int), // GL_MAX_COLOR_ATTACHMENTS
    (0x8a33, K::Int64), // GL_MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS
    (0x8a2e, K::Int), // GL_MAX_COMBINED_UNIFORM_BLOCKS
    (0x8a31, K::Int64), // GL_MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS
    (0x8824, K::Int), // GL_MAX_DRAW_BUFFERS
    (0x8d6b, K::Int64), // GL_MAX_ELEMENT_INDEX
    (0x80e9, K::Int), // GL_MAX_ELEMENTS_INDICES
    (0x80e8, K::Int), // GL_MAX_ELEMENTS_VERTICES
    (0x9125, K::Int), // GL_MAX_FRAGMENT_INPUT_COMPONENTS
    (0x8a2d, K::Int), // GL_MAX_FRAGMENT_UNIFORM_BLOCKS
    (0x8b49, K::Int), // GL_MAX_FRAGMENT_UNIFORM_COMPONENTS
    (0x8905, K::Int), // GL_MAX_PROGRAM_TEXEL_OFFSET
    (0x8d57, K::Int), // GL_MAX_SAMPLES
    (0x9111, K::Int64), // GL_MAX_SERVER_WAIT_TIMEOUT
    (0x84fd, K::Float), // GL_MAX_TEXTURE_LOD_BIAS
    (0x8c8a, K::Int), // GL_MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS
    (0x8c8b, K::Int), // GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS
    (0x8c80, K::Int), // GL_MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS
    (0x8a30, K::Int64), // GL_MAX_UNIFORM_BLOCK_SIZE
    (0x8a2f, K::Int), // GL_MAX_UNIFORM_BUFFER_BINDINGS
    (0x8b4b, K::Int), // GL_MAX_VARYING_COMPONENTS
    (0x9122, K::Int), // GL_MAX_VERTEX_OUTPUT_COMPONENTS
    (0x8a2b, K::Int), // GL_MAX_VERTEX_UNIFORM_BLOCKS
    (0x8b4a, K::Int), // GL_MAX_VERTEX_UNIFORM_COMPONENTS
    (0x8904, K::Int), // GL_MIN_PROGRAM_TEXEL_OFFSET
    (0x0d02, K::Int), // GL_PACK_ROW_LENGTH
    (0x0d04, K::Int), // GL_PACK_SKIP_PIXELS
    (0x0d03, K::Int), // GL_PACK_SKIP_ROWS
    (0x8c89, K::Bool), // GL_RASTERIZER_DISCARD
    (0x8e24, K::Bool), // GL_TRANSFORM_FEEDBACK_ACTIVE
    (0x8e23, K::Bool), // GL_TRANSFORM_FEEDBACK_PAUSED
    (0x8a34, K::Int), // GL_UNIFORM_BUFFER_OFFSET_ALIGNMENT
    (0x806e, K::Int), // GL_UNPACK_IMAGE_HEIGHT
    (0x0cf2, K::Int), // GL_UNPACK_ROW_LENGTH
    (0x806d, K::Int), // GL_UNPACK_SKIP_IMAGES
    (0x0cf4, K::Int), // GL_UNPACK_SKIP_PIXELS
    (0x0cf3, K::Int), // GL_UNPACK_SKIP_ROWS
];

/// Names Blink answers from GL only for a drawing buffer that has the channel (`GL_ALPHA_BITS` & co.).
const BITS: &[u32] = &[0x0D55, 0x0D56, 0x0D57];

/// `WebGL2RenderingContextBase::getParameter(GL_MAX_CLIENT_WAIT_TIMEOUT_WEBGL)`: Blink's `kMaxClientWaitTimeout`.
const MAX_CLIENT_WAIT_TIMEOUT_WEBGL: u32 = 0x9247;

/// The formats `getInternalformatParameter(RENDERBUFFER, f, SAMPLES)` asks GL about (Blink's sized color and depth
/// formats, and those `EXT_texture_norm16` / `EXT_color_buffer_(half_)float` make renderable).
const SAMPLED_FORMATS: &[u32] = &[
    0x8229, 0x822B, 0x8051, 0x8D62, 0x8058, 0x8C43, 0x8057, 0x8056, 0x8059, 0x81A5, 0x81A6, 0x8CAC, 0x88F0, 0x8CAD,
    0x8D48, 0x822A, 0x822C, 0x805B, 0x822D, 0x822F, 0x881A, 0x822E, 0x8230, 0x8814, 0x8C3A,
];
/// What the formats above need enabled before GL answers for them.
const SAMPLED_EXTENSIONS: &[&str] = &["GL_EXT_texture_norm16", "GL_EXT_color_buffer_float", "GL_EXT_color_buffer_half_float"];

const PRECISION_SHADERS: &[(&str, u32)] = &[("VERTEX_SHADER", 0x8B31), ("FRAGMENT_SHADER", 0x8B30)];
const PRECISION_TYPES: &[(&str, u32)] = &[
    ("LOW_FLOAT", 0x8DF0),
    ("MEDIUM_FLOAT", 0x8DF1),
    ("HIGH_FLOAT", 0x8DF2),
    ("LOW_INT", 0x8DF3),
    ("MEDIUM_INT", 0x8DF4),
    ("HIGH_INT", 0x8DF5),
];

const RENDERBUFFER: u32 = 0x8D41;
const SAMPLES: u32 = 0x80A9;
const NUM_SAMPLE_COUNTS: u32 = 0x9380;

/// One name's value as GL answers it, in the JSON shape the persona's capture recorded (a typed array as a list).
fn read(gl: &Gles, pname: u32, k: K) -> Value {
    // SAFETY: every buffer is larger than the count GL writes for `pname` (at most 4).
    unsafe {
        match k {
            K::Int => {
                let mut v = [0i32; 4];
                (gl.glGetIntegerv)(pname, v.as_mut_ptr());
                json!(v[0])
            }
            K::Uint => {
                let mut v = [0i32; 4];
                (gl.glGetIntegerv)(pname, v.as_mut_ptr());
                json!(v[0] as u32)
            }
            K::Float => {
                let mut v = [0f32; 4];
                (gl.glGetFloatv)(pname, v.as_mut_ptr());
                json!(v[0] as f64)
            }
            K::Int64 => {
                let mut v = [0i64; 4];
                (gl.glGetInteger64v)(pname, v.as_mut_ptr());
                json!(v[0])
            }
            K::Bool => {
                let mut v = [0u8; 4];
                (gl.glGetBooleanv)(pname, v.as_mut_ptr());
                json!(v[0] != 0)
            }
            K::Ints(n) => {
                let mut v = [0i32; 4];
                (gl.glGetIntegerv)(pname, v.as_mut_ptr());
                json!(v[..n])
            }
            K::Floats(n) => {
                let mut v = [0f32; 4];
                (gl.glGetFloatv)(pname, v.as_mut_ptr());
                json!(v[..n].iter().map(|f| *f as f64).collect::<Vec<_>>())
            }
            K::Bools(n) => {
                let mut v = [0u8; 4];
                (gl.glGetBooleanv)(pname, v.as_mut_ptr());
                json!(v[..n].iter().map(|b| *b != 0).collect::<Vec<_>>())
            }
        }
    }
}

/// `glGet*(pname)` -- or `glGetString` -- in the current context, typed as Blink reads it (a name Blink reads no
/// other way, as four floats); an `int64` comes back as its 8 little-endian bytes.
pub fn get(gl: &Gles, pname: u32) -> spec::GlQueryResult {
    use spec::GlQueryResult as R;
    if matches!(pname, 0x1F00 | 0x1F01 | 0x1F02 | 0x8B8C) {
        // SAFETY: GL returns a NUL-terminated static string or null
        let p = unsafe { (gl.glGetString)(pname) };
        return if p.is_null() { R::None } else { R::Str(unsafe { std::ffi::CStr::from_ptr(p as *const std::ffi::c_char) }.to_string_lossy().into_owned()) };
    }
    let kind = V1.iter().chain(V2.iter()).find(|(p, _)| *p == pname).map(|(_, k)| *k);
    let v = read(gl, pname, kind.unwrap_or(if BITS.contains(&pname) { K::Int } else { K::Floats(4) }));
    match (kind, v) {
        (Some(K::Int64), Value::Number(n)) => R::Bytes(n.as_i64().unwrap_or(0).to_le_bytes().to_vec()),
        (_, Value::Bool(b)) => R::Bool(b),
        (_, Value::Number(n)) if n.is_f64() => R::Float(n.as_f64().unwrap_or(0.0) as f32),
        (_, Value::Number(n)) => R::Int(n.as_i64().unwrap_or(0) as i32),
        (_, Value::Array(a)) if a.iter().all(|x| x.is_boolean()) => R::Bools(a.iter().map(|x| x.as_bool().unwrap_or(false)).collect()),
        (_, Value::Array(a)) => R::Floats(a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect()),
        _ => R::None,
    }
}

/// `names`, read in the current context (a fresh WebGL context's drawing buffer bound, as Blink's is).
fn params(gl: &Gles, tables: &[&[(u32, K)]]) -> Map<String, Value> {
    let mut out = Map::new();
    let bits: Vec<(u32, K)> = BITS.iter().map(|p| (*p, K::Int)).collect();
    for &(pname, k) in tables.iter().flat_map(|t| t.iter()).chain(bits.iter()) {
        let v = read(gl, pname, k);
        // SAFETY: plain GL call on the current context.
        if unsafe { (gl.glGetError)() } == 0 {
            out.insert(pname.to_string(), v);
        }
    }
    out
}

/// The twelve `getShaderPrecisionFormat` answers, `[rangeMin, rangeMax, precision]` by `SHADER.TYPE`.
fn precision(gl: &Gles) -> Map<String, Value> {
    let mut out = Map::new();
    for (sn, s) in PRECISION_SHADERS {
        for (tn, t) in PRECISION_TYPES {
            let (mut range, mut prec) = ([0i32; 2], 0i32);
            // SAFETY: GL writes two range values and one precision.
            unsafe { (gl.glGetShaderPrecisionFormat)(*s, *t, range.as_mut_ptr(), &mut prec) };
            out.insert(format!("{sn}.{tn}"), json!([range[0], range[1], prec]));
        }
    }
    out
}

/// `getInternalformatParameter(RENDERBUFFER, f, SAMPLES)` for every format GL answers in a WebGL 2 context.
fn internalformat_samples(gl: &Gles) -> Map<String, Value> {
    let mut out = Map::new();
    unsafe {
        for e in SAMPLED_EXTENSIONS {
            let c = std::ffi::CString::new(*e).unwrap_or_default();
            (gl.glRequestExtensionANGLE)(c.as_ptr());
        }
        while (gl.glGetError)() != 0 {}
        for &f in SAMPLED_FORMATS {
            let mut n = 0i32;
            (gl.glGetInternalformativ)(RENDERBUFFER, f, NUM_SAMPLE_COUNTS, 1, &mut n);
            if (gl.glGetError)() != 0 || n < 0 {
                continue;
            }
            let mut v = vec![0i32; n as usize];
            if n > 0 {
                (gl.glGetInternalformativ)(RENDERBUFFER, f, SAMPLES, n, v.as_mut_ptr());
            }
            if (gl.glGetError)() == 0 {
                out.insert(f.to_string(), json!(v));
            }
        }
    }
    out
}

/// The device's `webgl` block: `unmaskedVendor`, `unmaskedRenderer`, `paramsByNum`, `paramsByNumV2`, `extensions`,
/// `extensionsV2`, `precision`, `internalformatSamples`. Runs on the GPU thread, with no context current after it.
pub fn webgl_block(device: &Device) -> Option<Value> {
    // the drawing buffer Blink's context has by default (antialias, alpha, depth)
    let attrs = GlContextAttributes { stencil: false, ..Default::default() };
    let mut block = Map::new();
    block.insert("unmaskedVendor".into(), json!(device.info.vendor));
    block.insert("unmaskedRenderer".into(), json!(device.info.renderer));
    // the same device as Windows Chrome names it, for a Windows persona on this host (`_` keys are not the block's)
    let (vendor, renderer) = d3d11_names(&device.info, &pci_display_devices());
    block.insert("_d3d11Vendor".into(), json!(vendor));
    block.insert("_d3d11Renderer".into(), json!(renderer));
    for es3 in [false, true] {
        let c = device.create_context(if es3 { 3 } else { 2 })?;
        device.make_current(&c);
        let gl = Gles::load(device);
        let _x = Exec::new(&gl, 300, 150, &attrs);
        // the limits first, with the drawing buffer bound: the extension probes bind framebuffers of their own
        let mut p = if es3 { params(&gl, &[V1, V2]) } else { params(&gl, &[V1]) };
        let exts = profile::supported(if es3 { profile::WEBGL2 } else { profile::WEBGL1 }, &profile::blink_extensions(&gl, es3));
        if es3 {
            p.insert(MAX_CLIENT_WAIT_TIMEOUT_WEBGL.to_string(), json!(0));
            block.insert("paramsByNumV2".into(), Value::Object(p));
            block.insert("extensionsV2".into(), json!(exts));
            block.insert("internalformatSamples".into(), Value::Object(internalformat_samples(&gl)));
        } else {
            block.insert("paramsByNum".into(), Value::Object(p));
            block.insert("extensions".into(), json!(exts));
            block.insert("precision".into(), Value::Object(precision(&gl)));
        }
        device.destroy(c);
    }
    Some(Value::Object(block))
}

/// A display controller on this PC's PCI bus: its vendor and device ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PciDisplay {
    pub vendor: u16,
    pub device: u16,
}

/// The display controllers (PCI class 0x03) under `/sys/bus/pci/devices`, in bus order; none off Linux.
pub(crate) fn pci_display_devices() -> Vec<PciDisplay> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/bus/pci/devices") else { return out };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    dirs.sort();
    let hex = |p: &std::path::Path, f: &str| {
        let s = std::fs::read_to_string(p.join(f)).ok()?;
        u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
    };
    for d in dirs {
        if let (Some(class), Some(vendor), Some(device)) = (hex(&d, "class"), hex(&d, "vendor"), hex(&d, "device")) {
            if class >> 16 == 0x03 {
                out.push(PciDisplay { vendor: vendor as u16, device: device as u16 });
            }
        }
    }
    out
}

/// `WEBGL_debug_renderer_info`'s two strings for this PC's GPU as Windows Chrome reports them -- ANGLE's D3D11
/// renderer, `ANGLE (<vendor>, <adapter> (0x<device id>) Direct3D11 vs_5_0 ps_5_0, D3D11)` behind
/// `Google Inc. (<vendor>)` (measured 2026-10-17, Chrome 152.0.7977.83 on an RTX 3060: `ANGLE (NVIDIA, NVIDIA GeForce
/// RTX 3060 (0x00002487) Direct3D11 vs_5_0 ps_5_0, D3D11)`) -- from what this host's GL says (`info`) and its PCI ids.
/// The adapter is the name the GPU's driver gives it on both systems, without the GL driver's decorations
/// (NVIDIA's `/PCIe/SSE2`, Mesa's `Mesa ` and `(radeonsi, ...)`); the device id is the first display controller of the
/// vendor's (0 when none is found).
pub(crate) fn d3d11_names(info: &super::device::DeviceInfo, pci: &[PciDisplay]) -> (String, String) {
    // `ANGLE (<GL vendor>, <GL renderer>, <GL version>)`
    let inner = info.renderer.strip_prefix("ANGLE (").and_then(|s| s.strip_suffix(')')).unwrap_or(&info.renderer);
    let (gl_vendor, rest) = inner.split_once(", ").unwrap_or(("", inner));
    let gl_renderer = rest.rsplit_once(", ").map(|(r, _)| r).unwrap_or(rest);
    let probe = format!("{gl_vendor} {gl_renderer}").to_ascii_lowercase();
    let (short, id) = if probe.contains("nvidia") {
        ("NVIDIA", 0x10de)
    } else if probe.contains("amd") || probe.contains("radeon") || probe.contains("ati ") {
        ("AMD", 0x1002)
    } else if probe.contains("intel") {
        ("Intel", 0x8086)
    } else {
        (gl_vendor, 0)
    };
    let mut adapter = gl_renderer.split('/').next().unwrap_or(gl_renderer).trim();
    adapter = adapter.strip_prefix("Mesa ").unwrap_or(adapter);
    if let Some((name, _)) = adapter.split_once(" (") {
        adapter = name;
    }
    let device = pci.iter().find(|d| id != 0 && d.vendor == id).map(|d| d.device).unwrap_or(0);
    (
        format!("Google Inc. ({short})"),
        format!("ANGLE ({short}, {adapter} (0x{device:08X}) Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    )
}

#[cfg(test)]
mod d3d11_tests {
    use super::*;
    use crate::gpu::device::DeviceInfo;

    fn info(vendor: &str, renderer: &str) -> DeviceInfo {
        DeviceInfo { vendor: vendor.into(), renderer: renderer.into(), version: String::new() }
    }

    #[test]
    fn this_pcs_rtx_4060_as_windows_chrome_names_it() {
        let pci = [PciDisplay { vendor: 0x1002, device: 0x13c0 }, PciDisplay { vendor: 0x10de, device: 0x2882 }];
        let got = d3d11_names(
            &info("Google Inc. (NVIDIA Corporation)", "ANGLE (NVIDIA Corporation, NVIDIA GeForce RTX 4060/PCIe/SSE2, OpenGL ES 3.2)"),
            &pci,
        );
        assert_eq!(got.0, "Google Inc. (NVIDIA)");
        assert_eq!(got.1, "ANGLE (NVIDIA, NVIDIA GeForce RTX 4060 (0x00002882) Direct3D11 vs_5_0 ps_5_0, D3D11)");
    }

    #[test]
    fn the_measured_windows_string_has_this_shape() {
        // Windows Chrome 152.0.7977.83's own string on the RTX 3060 it was measured on (the persona's capture)
        let pci = [PciDisplay { vendor: 0x10de, device: 0x2487 }];
        let got = d3d11_names(&info("", "ANGLE (NVIDIA Corporation, NVIDIA GeForce RTX 3060/PCIe/SSE2, OpenGL ES 3.2)"), &pci);
        assert_eq!(got.1, "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002487) Direct3D11 vs_5_0 ps_5_0, D3D11)");
    }

    #[test]
    fn mesa_decorations_are_not_the_adapters_name() {
        let pci = [PciDisplay { vendor: 0x1002, device: 0x7480 }];
        let got = d3d11_names(&info("", "ANGLE (AMD, AMD Radeon RX 7600 (radeonsi, navi33, LLVM 17.0.6, DRM 3.54), OpenGL ES 3.2)"), &pci);
        assert_eq!(got.1, "ANGLE (AMD, AMD Radeon RX 7600 (0x00007480) Direct3D11 vs_5_0 ps_5_0, D3D11)");
        let got = d3d11_names(&info("", "ANGLE (Intel, Mesa Intel(R) UHD Graphics 770 (ADL-S GT1), OpenGL ES 3.2)"), &[]);
        assert_eq!(got, ("Google Inc. (Intel)".into(), "ANGLE (Intel, Intel(R) UHD Graphics 770 (0x00000000) Direct3D11 vs_5_0 ps_5_0, D3D11)".into()));
    }
}
