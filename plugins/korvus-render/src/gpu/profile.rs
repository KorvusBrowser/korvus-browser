//! What a page reads of the device's WebGL: the extensions a context offers -- Blink's own list, in
//! Blink's registration order, each kept when its `Supported()` holds for the device's ANGLE -- asked of a fresh
//! context of each version, as Chrome's `getSupportedExtensions` asks its context.
//!
//! Blink (`third_party/blink/renderer/modules/webgl`, Chromium 150): `RegisterContextExtensions` of
//! `WebGLRenderingContext` / `WebGL2RenderingContext` lists them; each `X::Supported(context)` asks
//! `Extensions3DUtil::SupportsExtension("GL_...")`, which is true for a name ANGLE lists as requestable
//! (`GL_REQUESTABLE_EXTENSIONS_ANGLE`) or already enabled (`GL_EXTENSIONS`); a draft extension is offered only
//! under a flag Chrome does not set.

use std::collections::HashSet;
use std::ffi::CStr;

use super::gles::Gles;

/// One registered extension: its WebGL name and the GL extensions its `Supported()` needs -- any one of the
/// alternatives, each a set that must all be there. Empty: always (`WEBGL_debug_renderer_info`, `WEBGL_lose_context`).
type Ext = (&'static str, &'static [&'static [&'static str]]);

/// `WebGLRenderingContext::RegisterContextExtensions`, in order.
pub const WEBGL1: &[Ext] = &[
    ("ANGLE_instanced_arrays", &[&["GL_ANGLE_instanced_arrays"]]),
    ("EXT_blend_minmax", &[&["GL_EXT_blend_minmax"]]),
    ("EXT_clip_control", &[&["GL_EXT_clip_control"]]),
    ("EXT_color_buffer_half_float", &[&["GL_OES_texture_half_float", "GL_EXT_color_buffer_half_float"]]),
    ("EXT_depth_clamp", &[&["GL_EXT_depth_clamp"]]),
    ("EXT_disjoint_timer_query", &[&["GL_EXT_disjoint_timer_query"]]),
    ("EXT_float_blend", &[&["GL_EXT_float_blend"]]),
    ("EXT_frag_depth", &[&["GL_EXT_frag_depth"]]),
    ("EXT_polygon_offset_clamp", &[&["GL_EXT_polygon_offset_clamp"]]),
    ("EXT_shader_texture_lod", &[&["GL_EXT_shader_texture_lod"]]),
    ("EXT_texture_compression_bptc", &[&["GL_EXT_texture_compression_bptc"]]),
    ("EXT_texture_compression_rgtc", &[&["GL_EXT_texture_compression_rgtc"]]),
    ("EXT_texture_filter_anisotropic", &[&["GL_EXT_texture_filter_anisotropic"]]),
    ("EXT_texture_mirror_clamp_to_edge", &[&["GL_EXT_texture_mirror_clamp_to_edge"]]),
    ("EXT_sRGB", &[&["GL_EXT_sRGB"]]),
    ("KHR_parallel_shader_compile", &[&["GL_KHR_parallel_shader_compile"]]),
    ("OES_element_index_uint", &[&["GL_OES_element_index_uint"]]),
    ("OES_fbo_render_mipmap", &[&["GL_OES_fbo_render_mipmap"]]),
    ("OES_standard_derivatives", &[&["GL_OES_standard_derivatives"]]),
    ("OES_texture_float", &[&["GL_OES_texture_float"]]),
    ("OES_texture_float_linear", &[&["GL_OES_texture_float_linear"]]),
    ("OES_texture_half_float", &[&["GL_OES_texture_half_float"]]),
    ("OES_texture_half_float_linear", &[&["GL_OES_texture_half_float_linear"]]),
    ("OES_vertex_array_object", &[&["GL_OES_vertex_array_object"]]),
    ("WEBGL_blend_func_extended", &[&["GL_EXT_blend_func_extended"]]),
    ("WEBGL_color_buffer_float", &[&["GL_OES_texture_float", "GL_CHROMIUM_color_buffer_float_rgba"]]),
    ("WEBGL_compressed_texture_astc", &[&["GL_KHR_texture_compression_astc_ldr"]]),
    ("WEBGL_compressed_texture_etc", &[&["GL_ANGLE_compressed_texture_etc"]]),
    ("WEBGL_compressed_texture_etc1", &[&["GL_OES_compressed_ETC1_RGB8_texture"]]),
    ("WEBGL_compressed_texture_pvrtc", &[&["GL_IMG_texture_compression_pvrtc"]]),
    ("WEBGL_compressed_texture_s3tc", S3TC),
    ("WEBGL_compressed_texture_s3tc_srgb", &[&["GL_EXT_texture_compression_s3tc_srgb"]]),
    ("WEBGL_debug_renderer_info", &[]),
    ("WEBGL_debug_shaders", &[&["GL_ANGLE_translated_shader_source"]]),
    ("WEBGL_depth_texture", &[&["GL_OES_packed_depth_stencil", "GL_CHROMIUM_depth_texture"]]),
    ("WEBGL_draw_buffers", &[&["GL_EXT_draw_buffers"]]),
    ("WEBGL_lose_context", &[]),
    ("WEBGL_multi_draw", MULTI_DRAW),
    ("WEBGL_polygon_mode", &[&["GL_ANGLE_polygon_mode"]]),
];

/// `WebGL2RenderingContext::RegisterContextExtensions`, in order (the two draft ones left out: Chrome offers them
/// only under `--enable-webgl-draft-extensions`).
pub const WEBGL2: &[Ext] = &[
    ("EXT_clip_control", &[&["GL_EXT_clip_control"]]),
    ("EXT_color_buffer_float", &[&["GL_EXT_color_buffer_float"]]),
    ("EXT_color_buffer_half_float", &[&["GL_OES_texture_half_float", "GL_EXT_color_buffer_half_float"]]),
    ("EXT_conservative_depth", &[&["GL_EXT_conservative_depth"]]),
    ("EXT_depth_clamp", &[&["GL_EXT_depth_clamp"]]),
    ("EXT_disjoint_timer_query_webgl2", &[&["GL_EXT_disjoint_timer_query"]]),
    ("EXT_float_blend", &[&["GL_EXT_float_blend"]]),
    ("EXT_polygon_offset_clamp", &[&["GL_EXT_polygon_offset_clamp"]]),
    ("EXT_render_snorm", &[&["GL_EXT_render_snorm"]]),
    ("EXT_texture_compression_bptc", &[&["GL_EXT_texture_compression_bptc"]]),
    ("EXT_texture_compression_rgtc", &[&["GL_EXT_texture_compression_rgtc"]]),
    ("EXT_texture_filter_anisotropic", &[&["GL_EXT_texture_filter_anisotropic"]]),
    ("EXT_texture_mirror_clamp_to_edge", &[&["GL_EXT_texture_mirror_clamp_to_edge"]]),
    ("EXT_texture_norm16", &[&["GL_EXT_texture_norm16"]]),
    ("KHR_parallel_shader_compile", &[&["GL_KHR_parallel_shader_compile"]]),
    ("NV_shader_noperspective_interpolation", &[&["GL_NV_shader_noperspective_interpolation"]]),
    ("OES_draw_buffers_indexed", &[&["GL_OES_draw_buffers_indexed"]]),
    ("OES_sample_variables", &[&["GL_OES_sample_variables"]]),
    ("OES_shader_multisample_interpolation", &[&["GL_OES_shader_multisample_interpolation"]]),
    ("OES_texture_float_linear", &[&["GL_OES_texture_float_linear"]]),
    ("OVR_multiview2", &[&["GL_OVR_multiview2"]]),
    ("WEBGL_blend_func_extended", &[&["GL_EXT_blend_func_extended"]]),
    ("WEBGL_clip_cull_distance", &[&["GL_ANGLE_clip_cull_distance"]]),
    ("WEBGL_compressed_texture_astc", &[&["GL_KHR_texture_compression_astc_ldr"]]),
    ("WEBGL_compressed_texture_etc", &[&["GL_ANGLE_compressed_texture_etc"]]),
    ("WEBGL_compressed_texture_etc1", &[&["GL_OES_compressed_ETC1_RGB8_texture"]]),
    ("WEBGL_compressed_texture_pvrtc", &[&["GL_IMG_texture_compression_pvrtc"]]),
    ("WEBGL_compressed_texture_s3tc", S3TC),
    ("WEBGL_compressed_texture_s3tc_srgb", &[&["GL_EXT_texture_compression_s3tc_srgb"]]),
    ("WEBGL_debug_renderer_info", &[]),
    ("WEBGL_debug_shaders", &[&["GL_ANGLE_translated_shader_source"]]),
    ("WEBGL_lose_context", &[]),
    ("WEBGL_multi_draw", MULTI_DRAW),
    ("WEBGL_polygon_mode", &[&["GL_ANGLE_polygon_mode"]]),
    ("WEBGL_provoking_vertex", &[&["GL_ANGLE_provoking_vertex"]]),
    ("WEBGL_render_shared_exponent", &[&["GL_QCOM_render_shared_exponent"]]),
    ("WEBGL_stencil_texturing", &[&["GL_ANGLE_stencil_texturing"]]),
];

const S3TC: &[&[&str]] = &[
    &["GL_EXT_texture_compression_s3tc"],
    &["GL_EXT_texture_compression_dxt1", "GL_ANGLE_texture_compression_dxt3", "GL_ANGLE_texture_compression_dxt5"],
    &["GL_ANGLE_texture_compression_dxt1", "GL_ANGLE_texture_compression_dxt3", "GL_ANGLE_texture_compression_dxt5"],
];
const MULTI_DRAW: &[&[&str]] = &[&["GL_WEBGL_multi_draw"], &["GL_ANGLE_multi_draw", "GL_ANGLE_instanced_arrays"]];

const GL_EXTENSIONS: u32 = 0x1F03;
const GL_REQUESTABLE_EXTENSIONS_ANGLE: u32 = 0x93A8;

/// The GL extension names a context offers or has: `GL_REQUESTABLE_EXTENSIONS_ANGLE` and `GL_EXTENSIONS`.
pub fn gl_extensions(gl: &Gles) -> HashSet<String> {
    let mut out = HashSet::new();
    for name in [GL_EXTENSIONS, GL_REQUESTABLE_EXTENSIONS_ANGLE] {
        // SAFETY: a current context; glGetString answers a NUL-terminated string or null.
        let p = unsafe { (gl.glGetString)(name) };
        if !p.is_null() {
            let s = unsafe { CStr::from_ptr(p as *const i8) }.to_string_lossy();
            out.extend(s.split_ascii_whitespace().map(str::to_string));
        }
    }
    out
}

/// The WebGL extensions of `table` the device supports, in Blink's order.
pub fn supported(table: &[Ext], gl_exts: &HashSet<String>) -> Vec<&'static str> {
    table
        .iter()
        .filter(|(_, need)| need.is_empty() || need.iter().any(|all| all.iter().all(|e| gl_exts.contains(*e))))
        .map(|(n, _)| *n)
        .collect()
}

/// The extension names Blink's `Extensions3DUtil` sees through Chrome's passthrough decoder: ANGLE's own (enabled and
/// requestable), plus the two `GL_CHROMIUM_*` names Chrome's `FeatureInfo` adds on top and Blink's table asks for
/// (`gpu/command_buffer/service/feature_info.cc`): `GL_CHROMIUM_depth_texture` over `GL_OES_depth_texture` or
/// `GL_ANGLE_depth_texture` (a WebGL 1 context; on ES3 only what the driver exposes), and
/// `GL_CHROMIUM_color_buffer_float_rgba` when `GL_OES_texture_float` and `GL_EXT_color_buffer_float` are there and
/// an RGBA32F texture is framebuffer-complete -- FeatureInfo's own test, made here in the same fresh context.
pub fn blink_extensions(gl: &Gles, es3: bool) -> HashSet<String> {
    let mut exts = gl_extensions(gl);
    if !es3 && (exts.contains("GL_OES_depth_texture") || exts.contains("GL_ANGLE_depth_texture")) {
        exts.insert("GL_CHROMIUM_depth_texture".into());
    }
    if exts.contains("GL_OES_texture_float") && exts.contains("GL_EXT_color_buffer_float") && rgba32f_renderable(gl, es3) {
        exts.insert("GL_CHROMIUM_color_buffer_float_rgba".into());
    }
    exts
}

/// FeatureInfo's probe: a 16x16 RGBA32F texture on a framebuffer -- complete?
fn rgba32f_renderable(gl: &Gles, es3: bool) -> bool {
    const TEXTURE_2D: u32 = 0x0DE1;
    const FRAMEBUFFER: u32 = 0x8D40;
    const RGBA32F: i32 = 0x8814;
    const RGBA: u32 = 0x1908;
    const FLOAT: u32 = 0x1406;
    const TEXTURE_MIN_FILTER: u32 = 0x2801;
    const NEAREST: f32 = 0x2600 as f32;
    // SAFETY: a current context; every object made here is deleted before returning.
    unsafe {
        // the decoder's context: its default-enabled extensions (`kOptionalFunctionalityExtensions`) and the two
        // the probe is about
        for e in OPTIONAL_FUNCTIONALITY.iter().chain(["GL_OES_texture_float", "GL_EXT_color_buffer_float"].iter()) {
            let c = std::ffi::CString::new(*e).unwrap();
            (gl.glRequestExtensionANGLE)(c.as_ptr());
        }
        while (gl.glGetError)() != 0 {}
        let (mut tex, mut fb) = (0, 0);
        (gl.glGenTextures)(1, &mut tex);
        (gl.glGenFramebuffers)(1, &mut fb);
        (gl.glBindTexture)(TEXTURE_2D, tex);
        (gl.glTexParameterf)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
        let _ = es3;
        (gl.glTexImage2D)(TEXTURE_2D, 0, RGBA32F, 16, 16, 0, RGBA, FLOAT, std::ptr::null());
        (gl.glBindFramebuffer)(FRAMEBUFFER, fb);
        (gl.glFramebufferTexture2D)(FRAMEBUFFER, 0x8CE0, TEXTURE_2D, tex, 0);
        let complete = (gl.glCheckFramebufferStatus)(FRAMEBUFFER) == 0x8CD5;
        (gl.glBindFramebuffer)(FRAMEBUFFER, 0);
        (gl.glDeleteFramebuffers)(1, &fb);
        (gl.glDeleteTextures)(1, &tex);
        while (gl.glGetError)() != 0 {}
        complete
    }
}

/// `gles2_cmd_decoder_passthrough.cc` `kOptionalFunctionalityExtensions` (Linux): enabled in every context Chrome's
/// decoder makes.
pub const OPTIONAL_FUNCTIONALITY: &[&str] = &[
    "GL_ANGLE_depth_texture", "GL_ANGLE_framebuffer_multisample", "GL_ANGLE_get_tex_level_parameter",
    "GL_ANGLE_instanced_arrays", "GL_ANGLE_memory_object_flags", "GL_ANGLE_pack_reverse_row_order",
    "GL_ANGLE_translated_shader_source", "GL_EXT_blend_minmax", "GL_EXT_discard_framebuffer",
    "GL_EXT_disjoint_timer_query", "GL_EXT_map_buffer_range", "GL_EXT_multisampled_render_to_texture",
    "GL_EXT_occlusion_query_boolean", "GL_EXT_sRGB", "GL_EXT_sRGB_write_control", "GL_EXT_texture_format_BGRA8888",
    "GL_EXT_texture_norm16", "GL_EXT_texture_rg", "GL_EXT_texture_sRGB_decode", "GL_EXT_texture_storage",
    "GL_EXT_unpack_subimage", "GL_KHR_parallel_shader_compile", "GL_KHR_robust_buffer_access_behavior",
    "GL_OES_mapbuffer", "GL_NV_pack_subimage", "GL_NV_pixel_buffer_object", "GL_OES_depth32",
    "GL_OES_packed_depth_stencil", "GL_OES_rgb8_rgba8", "GL_OES_vertex_array_object", "NV_EGL_stream_consumer_external",
];

/// What enabling each WebGL extension asks of the context (`X::X` -> `EnsureExtensionEnabled("GL_...")`, Chromium 150),
/// and the WebGL extensions it turns on with it (`EnableExtensionIfSupported`). Chrome's decoder answers a
/// `GL_CHROMIUM_*` name with ANGLE's own (see [`angle_names`]).
pub const ENABLES: &[(&str, &[&str], &[&str])] = &[
    ("ANGLE_instanced_arrays", &["GL_ANGLE_instanced_arrays"], &[]),
    ("EXT_blend_minmax", &["GL_EXT_blend_minmax"], &[]),
    ("EXT_clip_control", &["GL_EXT_clip_control"], &[]),
    ("EXT_color_buffer_float", &["GL_EXT_color_buffer_float"], &["EXT_float_blend"]),
    ("EXT_color_buffer_half_float", &["GL_EXT_color_buffer_half_float"], &[]),
    ("EXT_conservative_depth", &["GL_EXT_conservative_depth"], &[]),
    ("EXT_depth_clamp", &["GL_EXT_depth_clamp"], &[]),
    ("EXT_disjoint_timer_query", &["GL_EXT_disjoint_timer_query"], &[]),
    ("EXT_disjoint_timer_query_webgl2", &["GL_EXT_disjoint_timer_query_webgl2"], &[]),
    ("EXT_float_blend", &["GL_EXT_float_blend"], &[]),
    ("EXT_frag_depth", &["GL_EXT_frag_depth"], &[]),
    ("EXT_polygon_offset_clamp", &["GL_EXT_polygon_offset_clamp"], &[]),
    ("EXT_render_snorm", &["GL_EXT_render_snorm"], &[]),
    ("EXT_shader_texture_lod", &["GL_EXT_shader_texture_lod"], &[]),
    ("EXT_sRGB", &["GL_EXT_sRGB"], &[]),
    ("EXT_texture_compression_bptc", &["GL_EXT_texture_compression_bptc"], &[]),
    ("EXT_texture_compression_rgtc", &["GL_EXT_texture_compression_rgtc"], &[]),
    ("EXT_texture_filter_anisotropic", &["GL_EXT_texture_filter_anisotropic"], &[]),
    ("EXT_texture_mirror_clamp_to_edge", &["GL_EXT_texture_mirror_clamp_to_edge"], &[]),
    ("EXT_texture_norm16", &["GL_EXT_texture_norm16"], &[]),
    ("KHR_parallel_shader_compile", &["GL_KHR_parallel_shader_compile"], &[]),
    ("NV_shader_noperspective_interpolation", &["GL_NV_shader_noperspective_interpolation"], &[]),
    ("OES_draw_buffers_indexed", &["GL_OES_draw_buffers_indexed"], &[]),
    ("OES_element_index_uint", &["GL_OES_element_index_uint"], &[]),
    ("OES_fbo_render_mipmap", &["GL_OES_fbo_render_mipmap"], &[]),
    ("OES_sample_variables", &["GL_OES_sample_variables"], &[]),
    ("OES_shader_multisample_interpolation", &["GL_OES_shader_multisample_interpolation"], &[]),
    ("OES_standard_derivatives", &["GL_OES_standard_derivatives"], &[]),
    ("OES_texture_float", &["GL_OES_texture_float"], &["WEBGL_color_buffer_float", "EXT_float_blend"]),
    ("OES_texture_float_linear", &["GL_OES_texture_float_linear"], &[]),
    ("OES_texture_half_float", &["GL_EXT_color_buffer_half_float", "GL_OES_texture_half_float"], &[]),
    ("OES_texture_half_float_linear", &["GL_OES_texture_half_float_linear"], &[]),
    ("OES_vertex_array_object", &["GL_OES_vertex_array_object"], &[]),
    ("OVR_multiview2", &["GL_OVR_multiview2"], &[]),
    ("WEBGL_blend_func_extended", &["GL_EXT_blend_func_extended"], &[]),
    ("WEBGL_clip_cull_distance", &["GL_ANGLE_clip_cull_distance"], &[]),
    ("WEBGL_color_buffer_float", &["GL_CHROMIUM_color_buffer_float_rgb", "GL_CHROMIUM_color_buffer_float_rgba"], &["EXT_float_blend"]),
    ("WEBGL_compressed_texture_astc", &["GL_KHR_texture_compression_astc_hdr", "GL_KHR_texture_compression_astc_ldr"], &[]),
    ("WEBGL_compressed_texture_etc", &["GL_ANGLE_compressed_texture_etc"], &[]),
    ("WEBGL_compressed_texture_etc1", &["GL_OES_compressed_ETC1_RGB8_texture"], &[]),
    ("WEBGL_compressed_texture_pvrtc", &["GL_IMG_texture_compression_pvrtc"], &[]),
    ("WEBGL_compressed_texture_s3tc", &["GL_ANGLE_texture_compression_dxt1", "GL_ANGLE_texture_compression_dxt3", "GL_ANGLE_texture_compression_dxt5", "GL_EXT_texture_compression_dxt1", "GL_EXT_texture_compression_s3tc"], &[]),
    ("WEBGL_compressed_texture_s3tc_srgb", &["GL_EXT_texture_compression_s3tc_srgb"], &[]),
    ("WEBGL_debug_shaders", &["GL_ANGLE_translated_shader_source"], &[]),
    ("WEBGL_depth_texture", &["GL_CHROMIUM_depth_texture"], &[]),
    ("WEBGL_draw_buffers", &["GL_EXT_draw_buffers"], &[]),
    ("WEBGL_draw_instanced_base_vertex_base_instance", &["GL_ANGLE_base_vertex_base_instance", "GL_WEBGL_draw_instanced_base_vertex_base_instance"], &[]),
    ("WEBGL_multi_draw", &["GL_ANGLE_multi_draw", "GL_WEBGL_multi_draw"], &["ANGLE_instanced_arrays"]),
    ("WEBGL_multi_draw_instanced_base_vertex_base_instance", &["GL_ANGLE_base_vertex_base_instance", "GL_ANGLE_multi_draw", "GL_WEBGL_multi_draw", "GL_WEBGL_multi_draw_instanced_base_vertex_base_instance"], &[]),
    ("WEBGL_polygon_mode", &["GL_ANGLE_polygon_mode"], &[]),
    ("WEBGL_provoking_vertex", &["GL_ANGLE_provoking_vertex"], &[]),
    ("WEBGL_render_shared_exponent", &["GL_QCOM_render_shared_exponent"], &[]),
    ("WEBGL_shader_pixel_local_storage", &["GL_ANGLE_shader_pixel_local_storage"], &["OES_draw_buffers_indexed", "EXT_color_buffer_float", "EXT_color_buffer_half_float"]),
    ("WEBGL_stencil_texturing", &["GL_ANGLE_stencil_texturing"], &[]),
];

/// The ANGLE extensions a GL name the WebGL layer asks for is, on Chrome's passthrough decoder.
pub fn angle_names(gl_name: &str) -> &'static [&'static str] {
    match gl_name {
        "GL_CHROMIUM_depth_texture" => &["GL_ANGLE_depth_texture", "GL_OES_depth_texture"],
        "GL_CHROMIUM_color_buffer_float_rgba" | "GL_CHROMIUM_color_buffer_float_rgb" => &["GL_OES_texture_float", "GL_EXT_color_buffer_float"],
        _ => &[],
    }
}
