//! The WebGL EXTENSION OBJECTS, and what a LOST context answers.
//!
//! # What was here
//!
//! `getExtension(name)` answered a fresh EMPTY `{}` for every name the persona listed, so
//! `gl.getExtension("WEBGL_lose_context").loseContext` was `undefined` where Chrome has a function,
//! `Object.prototype.toString.call(ext)` read `[object Object]` where Chrome reads
//! `[object WebGLLoseContext]`, and `ext === gl.getExtension(sameName)` was false. An extension a
//! page reads with one expression was absent in every way but truthiness.
//!
//! # The shapes are measured, not written down from the specs
//!
//! Every table below was read off stock Chrome 150.0.7871.46 over a loopback page, three ways, because
//! which extensions a Chrome exposes depends on its GPU backend: headless SwiftShader, headless
//! `--use-angle=gl-egl` on the box's own GPU (which adds `EXT_disjoint_timer_query*`,
//! `KHR_parallel_shader_compile`, `WEBGL_blend_func_extended`, `EXT_render_snorm`,
//! `EXT_texture_norm16`), and `--use-angle=vulkan`. For each: `Object.prototype.toString` of the object
//! (the interface name), every own key of its prototype in order with its descriptor (constants
//! enumerable, not writable, not configurable; methods enumerable, writable, configurable, with their
//! `length`), and a prototype whose own keys have NO `constructor` and whose parent is
//! `Object.prototype` -- these are `[LegacyNoInterfaceObject]` interfaces, and no global names them.
//! `WEBGL_provoking_vertex` is the one no configuration on this box would expose; its table is the
//! vendored webref IDL (`core/korvus-webapi/gen/webref/WEBGL_provoking_vertex.idl`) under Blink's
//! class name.
//!
//! # Which extensions a context OFFERS is the persona's, never this file's
//!
//! `EXTENSIONS` is what this engine CAN hand out. What a context answers `getSupportedExtensions()` and
//! `getExtension()` with is the active bundle's `deviceOracle.webgl.extensions` /
//! `extensionsV2`, exactly as before; a name this file has a table for but the bundle does not list is
//! `null`, and `core/korvus-runtime/tests/canvas_draw_image_live.rs` asserts that every name every
//! bundle lists has a table here.
//!
//! # The methods behave
//!
//! Each extension method is the context call it names under another spelling, run against the
//! extension's OWN context (`drawArraysInstancedANGLE` records the same `DrawArraysInstanced` op
//! `WebGL2RenderingContext.drawArraysInstanced` does). The ones whose effect this pipeline does not
//! model -- per-draw-buffer blend state, multiview, timer queries, a polygon fill mode -- keep the
//! state their getters read back and record nothing, which is the same line `webgl.rs` draws for
//! transform feedback: an op nothing executes would be a fiction.
//!
//! # WEBGL_lose_context, measured
//!
//! ```text
//! loseContext()                  isContextLost() true at once; getError() CONTEXT_LOST_WEBGL (37442)
//!                                once, then NO_ERROR; `webglcontextlost` (WebGLContextEvent,
//!                                cancelable, statusMessage "") fires as a TASK -- after microtasks,
//!                                before a setTimeout(0) queued after the call
//! while lost                     getParameter / getExtension / getSupportedExtensions /
//!                                getContextAttributes / get*Parameter / get*InfoLog null;
//!                                create* still hand back objects; is* false; getAttribLocation -1;
//!                                checkFramebufferStatus FRAMEBUFFER_UNSUPPORTED; drawingBuffer* 0;
//!                                readPixels writes nothing; draws do nothing
//! loseContext() again            INVALID_OPERATION on the lost queue
//! restoreContext(), lost event   INVALID_OPERATION, stays lost
//!   NOT preventDefault-ed
//! restoreContext(), prevented    `webglcontextrestored` as a task; a FRESH context (old objects are
//!                                not objects of it, clearColor is back to 0,0,0,0); the extension
//!                                objects are the same objects
//! restoreContext(), not lost     INVALID_OPERATION
//! ```

use deno_core::v8;
use spec::GlOp;

use super::super::html_canvas_element::{priv_get, priv_set};
use super::{edit, gl_call, int, mint, new_gl_object, null_val, num, push, uint, WEBGL_ID_PRIV};
use crate::binding::protos::{build_fn_data, str_val};
use crate::install::installer::{self, Flavor};
use crate::install::{InterfaceDef, MemberDef};

const CONTEXT_LOST_WEBGL: u32 = 0x9242;
const INVALID_OPERATION: u32 = 0x0502;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const FRAMEBUFFER_UNSUPPORTED: u32 = 0x8CDD;
const WAIT_FAILED: u32 = 0x911D;

/// The context an extension object belongs to, on the extension object.
const EXT_CTX_PRIV: &str = "korvus_gl_ext_ctx";

// ── the state the extension setters keep ────────────────────────────────────────────────────────

const LOWER_LEFT_EXT: u32 = 36001;
const UPPER_LEFT_EXT: u32 = 36002;
const NEGATIVE_ONE_TO_ONE_EXT: u32 = 37726;
const ZERO_TO_ONE_EXT: u32 = 37727;
const CLIP_ORIGIN_EXT: u32 = 37724;
const CLIP_DEPTH_MODE_EXT: u32 = 37725;
const POLYGON_MODE_WEBGL: u32 = 2880;
const LINE_WEBGL: u32 = 6913;
const FILL_WEBGL: u32 = 6914;
const POLYGON_OFFSET_CLAMP_EXT: u32 = 36379;
const FIRST_VERTEX_CONVENTION_WEBGL: u32 = 0x8E4D;
const LAST_VERTEX_CONVENTION_WEBGL: u32 = 0x8E4E;
const PROVOKING_VERTEX_WEBGL: u32 = 0x8E4F;
const FRONT_AND_BACK: u32 = 0x0408;

/// What `clipControlEXT` / `polygonModeWEBGL` / `polygonOffsetClampEXT` / `provokingVertexWEBGL` set and
/// the matching `getParameter` enums read back. The initial values are GL's (measured on Chrome:
/// `CLIP_ORIGIN_EXT` 36001, `POLYGON_MODE_WEBGL` 6914, `POLYGON_OFFSET_CLAMP_EXT` 0).
#[derive(Clone, Debug)]
pub(crate) struct ExtState {
    clip_origin: u32,
    clip_depth: u32,
    polygon_mode: u32,
    polygon_offset_clamp: f64,
    provoking: u32,
}

impl Default for ExtState {
    fn default() -> Self {
        Self {
            clip_origin: LOWER_LEFT_EXT,
            clip_depth: NEGATIVE_ONE_TO_ONE_EXT,
            polygon_mode: FILL_WEBGL,
            polygon_offset_clamp: 0.0,
            provoking: LAST_VERTEX_CONVENTION_WEBGL,
        }
    }
}

// ── the enums an extension gates ────────────────────────────────────────────────────────────────

/// `(enum, extension, in WebGL 1, in WebGL 2)`: an enum a context answers only once `getExtension`
/// has enabled that extension -- before, `getParameter` is `null` with INVALID_ENUM, and
/// `enable`/`disable`/`isEnabled`/`hint` are INVALID_ENUM. Measured on Chrome 150 per row (`getParameter`
/// before and after `getExtension`, both context versions); an enum that is CORE in WebGL 2
/// (`MAX_DRAW_BUFFERS`, `FRAGMENT_SHADER_DERIVATIVE_HINT`, `VERTEX_ARRAY_BINDING`) is gated in WebGL 1
/// only. `UNMASKED_VENDOR_WEBGL`/`UNMASKED_RENDERER_WEBGL` are gated too: Chrome answers `null` for
/// them until `WEBGL_debug_renderer_info` is asked for.
const GATES: &[(u32, &str, bool, bool)] = &[
    (37445, "WEBGL_debug_renderer_info", true, true),
    (37446, "WEBGL_debug_renderer_info", true, true),
    (34047, "EXT_texture_filter_anisotropic", true, true),
    (34852, "WEBGL_draw_buffers", true, false),
    (36063, "WEBGL_draw_buffers", true, false),
    (34229, "OES_vertex_array_object", true, false),
    (35723, "OES_standard_derivatives", true, false),
    (35068, "WEBGL_blend_func_extended", true, true),
    (CLIP_ORIGIN_EXT, "EXT_clip_control", true, true),
    (CLIP_DEPTH_MODE_EXT, "EXT_clip_control", true, true),
    (POLYGON_MODE_WEBGL, "WEBGL_polygon_mode", true, true),
    (10754, "WEBGL_polygon_mode", true, true),
    (POLYGON_OFFSET_CLAMP_EXT, "EXT_polygon_offset_clamp", true, true),
    (34383, "EXT_depth_clamp", true, true),
    (36795, "EXT_disjoint_timer_query", true, false),
    (36795, "EXT_disjoint_timer_query_webgl2", false, true),
    (36392, "EXT_disjoint_timer_query", true, false),
    (36392, "EXT_disjoint_timer_query_webgl2", false, true),
    (3378, "WEBGL_clip_cull_distance", false, true),
    (33529, "WEBGL_clip_cull_distance", false, true),
    (33530, "WEBGL_clip_cull_distance", false, true),
    (38449, "OVR_multiview2", false, true),
    (36443, "OES_shader_multisample_interpolation", false, true),
    (36444, "OES_shader_multisample_interpolation", false, true),
    (36445, "OES_shader_multisample_interpolation", false, true),
    (PROVOKING_VERTEX_WEBGL, "WEBGL_provoking_vertex", false, true),
];

/// `WEBGL_draw_buffers`' sixteen `DRAW_BUFFERi_WEBGL` enums, which the table above would list one by one.
fn draw_buffer_gate(pname: u32) -> bool {
    (34853..=34868).contains(&pname)
}

/// `WEBGL_clip_cull_distance`'s `CLIP_DISTANCEi_WEBGL` capabilities.
fn clip_distance_gate(pname: u32) -> bool {
    (12288..=12295).contains(&pname)
}

fn is_webgl_context(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> bool {
    priv_get(scope, ctx, WEBGL_ID_PRIV).is_some()
}

/// Is `pname` an extension enum whose extension this context has NOT enabled?
pub(crate) fn gated(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, pname: u32) -> bool {
    if !is_webgl_context(scope, ctx) {
        return false;
    }
    edit(scope, ctx, |s| {
        let need: Option<&str> = GATES
            .iter()
            .find(|(p, _, v1, v2)| *p == pname && if s.v2 { *v2 } else { *v1 })
            .map(|(_, e, _, _)| *e)
            .or_else(|| (!s.v2 && draw_buffer_gate(pname)).then_some("WEBGL_draw_buffers"))
            .or_else(|| (s.v2 && clip_distance_gate(pname)).then_some("WEBGL_clip_cull_distance"));
        match need {
            Some(e) => !s.enabled.contains(e),
            None => false,
        }
    })
}

/// The part of `getParameter` this file answers: `null` on a lost context, `null` + INVALID_ENUM for a
/// gated enum, and the extension state enums once enabled. `true` = answered.
pub(crate) fn get_parameter(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, pname: u32, rv: &mut v8::ReturnValue) -> bool {
    if !is_webgl_context(scope, ctx) {
        return false;
    }
    if edit(scope, ctx, |s| s.lost) {
        rv.set_null();
        return true;
    }
    if gated(scope, ctx, pname) {
        push(scope, ctx, GlOp::RaiseError { code: INVALID_ENUM });
        rv.set_null();
        return true;
    }
    let (st, v2) = edit(scope, ctx, |s| (s.ext.clone(), s.v2));
    match pname {
        CLIP_ORIGIN_EXT => rv.set_uint32(st.clip_origin),
        CLIP_DEPTH_MODE_EXT => rv.set_uint32(st.clip_depth),
        POLYGON_MODE_WEBGL => rv.set_uint32(st.polygon_mode),
        POLYGON_OFFSET_CLAMP_EXT => rv.set_double(st.polygon_offset_clamp),
        // `WEBGL_provoking_vertex` is a WebGL 2 extension; a WebGL 1 context has no such name.
        PROVOKING_VERTEX_WEBGL if v2 => rv.set_uint32(st.provoking),
        _ => return false,
    }
    true
}

/// Is this context lost? (`false` for anything that is not a WebGL context.)
pub(crate) fn is_lost(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) -> bool {
    is_webgl_context(scope, ctx) && edit(scope, ctx, |s| s.lost)
}

// ── the extension objects ───────────────────────────────────────────────────────────────────────

/// Each extension's prototype, built once per REALM from its table on first use, and kept in a map in
/// that realm ([`crate::binding::protos::realm_map`]). It was kept in `OpState`, which is per ISOLATE,
/// so whichever realm asked first built the prototype for every realm: a frame's extension objects wore
/// the page's, chained to the page's `Object.prototype`. Chrome 150.0.7871.46 gives each realm its own, chained to its own
/// (headless, 2026-09-28: `OES_texture_float`, `WEBGL_lose_context`, `OES_vertex_array_object`,
/// `ANGLE_instanced_arrays` from a same-origin frame).
const EXT_PROTOS: &str = "korvus_gl_ext_protos";

/// The prototype for `def`, created through THE installer from its table (the rows, the tag, the
/// ledger), with the one thing an `[LegacyNoInterfaceObject]` interface lacks taken back off: its
/// prototype has no `constructor`. It is never published -- no bundle's capture lists these names,
/// and Chrome has no global for any of them (`typeof WebGLLoseContext` is `"undefined"`, measured).
fn ext_proto<'s>(scope: &mut v8::PinScope<'s, '_>, def: &'static InterfaceDef, parent: Option<&str>) -> Option<v8::Local<'s, v8::Object>> {
    let map = crate::binding::protos::realm_map(scope, EXT_PROTOS);
    let name = str_val(scope, def.name);
    if let Some(p) = map.get(scope, name).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) {
        return Some(p);
    }
    let parent = parent.and_then(|p| crate::install::live::parent_of(scope, p));
    let holder = v8::Object::new(scope);
    let mut ledger = installer::take_ledger(scope);
    let built = installer::create_interface(scope, holder, def, def.name, parent, None, Flavor::Table, &mut ledger);
    installer::put_ledger(scope, ledger);
    let proto = built.proto;
    let k = str_val(scope, "constructor");
    proto.delete(scope, k);
    map.set(scope, name, proto.into());
    Some(proto)
}

/// The table for a canonical extension name.
fn table_of(name: &str) -> Option<&'static InterfaceDef> {
    EXTENSIONS.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// `getExtension(name)` for a name the persona lists (`canonical` is the list's spelling): the SAME
/// object every call on this context, which also enables the extension's enums.
pub(crate) fn extension_object(
    scope: &mut v8::PinScope,
    ctx: v8::Local<v8::Object>,
    canonical: &str,
    rv: &mut v8::ReturnValue,
) -> bool {
    let slot = format!("korvus_gl_ext:{canonical}");
    if let Some(v) = priv_get(scope, ctx, &slot) {
        rv.set(v);
        return true;
    }
    let Some(def) = table_of(canonical) else { return false };
    let Some(proto) = ext_proto(scope, def, None) else { return false };
    let obj = crate::binding::protos::mint_on(scope, proto.into());
    priv_set(scope, obj, EXT_CTX_PRIV, ctx.into());
    priv_set(scope, ctx, &slot, obj.into());
    let name = canonical.to_string();
    edit(scope, ctx, |s| {
        // the GPU context enables what Blink's `EnsureExtensionEnabled` asks of it, in the list's order
        s.ops.push(GlOp::EnableExtension { name: name.clone() });
        s.enabled_order.push(name.clone());
        s.enabled.insert(name);
    });
    rv.set(obj.into());
    true
}

/// Every extension method row: a native function named for the row, with the row's `length` and its
/// WebIDL argument count under the extension's own interface name (`'vertexAttribDivisorANGLE' on
/// 'ANGLEInstancedArrays': 2 arguments required`, measured).
pub fn install_gl_extension(scope: &mut v8::PinScope, proto: v8::Local<v8::Object>, member: &'static MemberDef) {
    let data = str_val(scope, member.name);
    let f = build_fn_data(scope, cb_gl_ext, member.name, member.length as i32, data);
    super::guarded_value(scope, proto, member, f);
}

/// Re-prototype an object a context call minted onto an extension's object interface
/// (`createVertexArrayOES` answers a `WebGLVertexArrayObjectOES`, not a `WebGLVertexArrayObject`).
fn reproto(scope: &mut v8::PinScope, rv: &mut v8::ReturnValue, def: &'static InterfaceDef) {
    let v = rv.get(scope);
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else { return };
    if let Some(p) = ext_proto(scope, def, Some("WebGLObject")) {
        o.set_prototype(scope, p.into());
    }
}

fn cb_gl_ext(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {
    let Some(ctx) = priv_get(scope, args.this(), EXT_CTX_PRIV).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else {
        // `ext.loseContext.call({})` and `.call(Object.create(proto))` are both Illegal invocation.
        crate::binding::protos::illegal_invocation(scope);
        return;
    };
    let member = crate::binding::protos::body_data(scope, &args).to_rust_string_lossy(scope);
    let n = args.length();
    match member.as_str() {
        "loseContext" => lose_context(scope, ctx),
        "restoreContext" => restore_context(scope, ctx),

        // ANGLE_instanced_arrays -- the WebGL 2 calls under their WebGL 1 names.
        "drawArraysInstancedANGLE" => gl_call(scope, ctx, "drawArraysInstanced", &args, &mut rv),
        "drawElementsInstancedANGLE" => gl_call(scope, ctx, "drawElementsInstanced", &args, &mut rv),
        "vertexAttribDivisorANGLE" => gl_call(scope, ctx, "vertexAttribDivisor", &args, &mut rv),

        // OES_vertex_array_object. All four are `length` 0 and count no arguments (measured): a
        // missing object is GL's null object.
        "createVertexArrayOES" => {
            gl_call(scope, ctx, "createVertexArray", &args, &mut rv);
            reproto(scope, &mut rv, &WEBGLVERTEXARRAYOBJECTOES);
        }
        "deleteVertexArrayOES" if n > 0 => gl_call(scope, ctx, "deleteVertexArray", &args, &mut rv),
        "isVertexArrayOES" if n > 0 => gl_call(scope, ctx, "isVertexArray", &args, &mut rv),
        "isVertexArrayOES" => rv.set_bool(false),
        "bindVertexArrayOES" if n > 0 => gl_call(scope, ctx, "bindVertexArray", &args, &mut rv),
        "bindVertexArrayOES" => {
            if !is_lost(scope, ctx) {
                push(scope, ctx, GlOp::BindVertexArray { id: 0 });
            }
        }
        "deleteVertexArrayOES" => {}

        // WEBGL_draw_buffers -- `drawBuffers`, which this pipeline (one colour attachment) does not
        // model either; see `webgl.rs`.
        "drawBuffersWEBGL" => gl_call(scope, ctx, "drawBuffers", &args, &mut rv),

        // EXT_disjoint_timer_query(_webgl2). The query OBJECTS are real (created, `isQueryEXT` true
        // until deleted); nothing is ever timed, so a result is never available.
        "createQueryEXT" => {
            gl_call(scope, ctx, "createQuery", &args, &mut rv);
            reproto(scope, &mut rv, &WEBGLTIMERQUERYEXT);
        }
        "deleteQueryEXT" => gl_call(scope, ctx, "deleteQuery", &args, &mut rv),
        "isQueryEXT" => gl_call(scope, ctx, "isQuery", &args, &mut rv),
        "beginQueryEXT" | "endQueryEXT" | "queryCounterEXT" => {}
        "getQueryEXT" => rv.set_null(),
        "getQueryObjectEXT" => {
            const QUERY_RESULT_AVAILABLE_EXT: u32 = 34919;
            if !is_lost(scope, ctx) && uint(scope, &args, 1) == QUERY_RESULT_AVAILABLE_EXT {
                rv.set_bool(false);
            } else {
                rv.set_null();
            }
        }

        // WEBGL_multi_draw: `drawcount` draws, each read out of the lists at its offset. A list too
        // short for `offset + drawcount` is INVALID_OPERATION and draws nothing (measured: no throw).
        "multiDrawArraysWEBGL" | "multiDrawArraysInstancedWEBGL" => multi_draw_arrays(scope, ctx, &args, member.ends_with("InstancedWEBGL")),
        "multiDrawElementsWEBGL" | "multiDrawElementsInstancedWEBGL" => multi_draw_elements(scope, ctx, &args, member.ends_with("InstancedWEBGL")),

        // EXT_polygon_offset_clamp: the offset is the context's; the clamp is state this rasterizer
        // has no stage for, kept for `getParameter(POLYGON_OFFSET_CLAMP_EXT)`.
        "polygonOffsetClampEXT" => {
            if is_lost(scope, ctx) {
                return;
            }
            let (factor, units, clamp) = (num(scope, &args, 0) as f32, num(scope, &args, 1) as f32, num(scope, &args, 2));
            push(scope, ctx, GlOp::PolygonOffset { factor, units });
            edit(scope, ctx, |s| s.ext.polygon_offset_clamp = clamp);
        }
        "clipControlEXT" => {
            if is_lost(scope, ctx) {
                return;
            }
            let (origin, depth) = (uint(scope, &args, 0), uint(scope, &args, 1));
            if !matches!(origin, LOWER_LEFT_EXT | UPPER_LEFT_EXT) || !matches!(depth, NEGATIVE_ONE_TO_ONE_EXT | ZERO_TO_ONE_EXT) {
                push(scope, ctx, GlOp::RaiseError { code: INVALID_ENUM });
                return;
            }
            edit(scope, ctx, |s| {
                s.ext.clip_origin = origin;
                s.ext.clip_depth = depth;
            });
        }
        "polygonModeWEBGL" => {
            if is_lost(scope, ctx) {
                return;
            }
            let (face, mode) = (uint(scope, &args, 0), uint(scope, &args, 1));
            if face != FRONT_AND_BACK || !matches!(mode, LINE_WEBGL | FILL_WEBGL) {
                push(scope, ctx, GlOp::RaiseError { code: INVALID_ENUM });
                return;
            }
            edit(scope, ctx, |s| s.ext.polygon_mode = mode);
        }
        "provokingVertexWEBGL" => {
            if is_lost(scope, ctx) {
                return;
            }
            let mode = uint(scope, &args, 0);
            if !matches!(mode, FIRST_VERTEX_CONVENTION_WEBGL | LAST_VERTEX_CONVENTION_WEBGL) {
                push(scope, ctx, GlOp::RaiseError { code: INVALID_ENUM });
                return;
            }
            edit(scope, ctx, |s| s.ext.provoking = mode);
        }

        // WEBGL_debug_shaders. Chrome answers ANGLE's translation of a COMPILED shader (a SPIR-V dump
        // on this box) and "" for one that was never compiled. There is no ANGLE here to translate
        // with, and a made-up translation is a value no Chrome could produce, so every shader
        // answers the uncompiled "" -- Ruling, recorded in the P10d report.
        "getTranslatedShaderSource" => {
            if is_lost(scope, ctx) {
                rv.set_null();
            } else {
                let s = str_val(scope, "");
                rv.set(s);
            }
        }

        // The members whose effect this single-attachment ES 2.0 rasterizer has no stage for:
        // per-draw-buffer blend/mask state (OES_draw_buffers_indexed), multiview framebuffers
        // (OVR_multiview2). Nothing is recorded, as for transform feedback in `webgl.rs`.
        "blendEquationSeparateiOES" | "blendEquationiOES" | "blendFuncSeparateiOES" | "blendFunciOES"
        | "colorMaskiOES" | "disableiOES" | "enableiOES" | "framebufferTextureMultiviewOVR" => {}

        "getSupportedProfiles" => {
            let a = v8::Array::new(scope, 0);
            rv.set(a.into());
        }
        other => {
            crate::binding::protos::throw_type_error(scope, &format!("Failed to execute '{other}': not implemented by this engine."));
        }
    }
}

/// A `sequence<long>` / `Int32Array` argument, read whole.
fn i32_list(scope: &mut v8::PinScope, v: v8::Local<v8::Value>) -> Option<Vec<i32>> {
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    let len = crate::binding::protos::get_prop(scope, obj, "length").and_then(|l| l.uint32_value(scope))?;
    Some((0..len).map(|i| obj.get_index(scope, i).and_then(|x| x.int32_value(scope)).unwrap_or(0)).collect())
}

/// `lists[offsets[k]..offsets[k] + drawcount]` for every `(list, offset)` pair, or `None` when any is
/// too short (or an offset or the count is negative).
fn windows(lists: &[(Vec<i32>, i32)], drawcount: i32) -> Option<Vec<&[i32]>> {
    if drawcount < 0 {
        return None;
    }
    lists
        .iter()
        .map(|(l, off)| {
            let (off, n) = (usize::try_from(*off).ok()?, drawcount as usize);
            l.get(off..off.checked_add(n)?)
        })
        .collect()
}

fn multi_draw_arrays(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments, instanced: bool) {
    if is_lost(scope, ctx) {
        return;
    }
    // (mode, firsts, firstsOffset, counts, countsOffset, [instanceCounts, instanceCountsOffset,] drawcount)
    let mode = uint(scope, args, 0);
    let mut lists = Vec::new();
    for (li, oi) in if instanced { vec![(1, 2), (3, 4), (5, 6)] } else { vec![(1, 2), (3, 4)] } {
        let Some(l) = i32_list(scope, args.get(li)) else {
            push(scope, ctx, GlOp::RaiseError { code: INVALID_VALUE });
            return;
        };
        lists.push((l, int(scope, args, oi)));
    }
    let drawcount = int(scope, args, if instanced { 7 } else { 5 });
    let Some(w) = windows(&lists, drawcount) else {
        push(scope, ctx, GlOp::RaiseError { code: INVALID_OPERATION });
        return;
    };
    let ops: Vec<GlOp> = (0..drawcount as usize)
        .map(|k| {
            if instanced {
                GlOp::DrawArraysInstanced { mode, first: w[0][k], count: w[1][k], instances: w[2][k] }
            } else {
                GlOp::DrawArrays { mode, first: w[0][k], count: w[1][k] }
            }
        })
        .collect();
    for op in ops {
        push(scope, ctx, op);
    }
}

fn multi_draw_elements(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, args: &v8::FunctionCallbackArguments, instanced: bool) {
    if is_lost(scope, ctx) {
        return;
    }
    // (mode, counts, countsOffset, type, offsets, offsetsOffset, [instanceCounts, instanceCountsOffset,] drawcount)
    let mode = uint(scope, args, 0);
    let ty = uint(scope, args, 3);
    let mut lists = Vec::new();
    for (li, oi) in if instanced { vec![(1, 2), (4, 5), (6, 7)] } else { vec![(1, 2), (4, 5)] } {
        let Some(l) = i32_list(scope, args.get(li)) else {
            push(scope, ctx, GlOp::RaiseError { code: INVALID_VALUE });
            return;
        };
        lists.push((l, int(scope, args, oi)));
    }
    let drawcount = int(scope, args, if instanced { 8 } else { 6 });
    let Some(w) = windows(&lists, drawcount) else {
        push(scope, ctx, GlOp::RaiseError { code: INVALID_OPERATION });
        return;
    };
    let ops: Vec<GlOp> = (0..drawcount as usize)
        .map(|k| {
            let (count, offset) = (w[0][k], w[1][k].max(0) as u32);
            if instanced {
                GlOp::DrawElementsInstanced { mode, count, ty, offset, instances: w[2][k] }
            } else {
                GlOp::DrawElements { mode, count, ty, offset }
            }
        })
        .collect();
    for op in ops {
        push(scope, ctx, op);
    }
}

// ── a lost context ──────────────────────────────────────────────────────────────────────────────

/// Every context call on a LOST context, measured on Chrome 150 (the table in the module doc).
pub(crate) fn lost_call(
    scope: &mut v8::PinScope,
    ctx: v8::Local<v8::Object>,
    member: &str,
    _args: &v8::FunctionCallbackArguments,
    rv: &mut v8::ReturnValue,
) {
    match member {
        "getError" => {
            let e = edit(scope, ctx, |s| s.lost_errors.pop_front()).unwrap_or(0);
            rv.set_uint32(e);
        }
        "isContextLost" => rv.set_bool(true),
        // An object still comes back -- one this context will never call its own.
        "createBuffer" | "createFramebuffer" | "createProgram" | "createRenderbuffer" | "createShader"
        | "createTexture" | "createVertexArray" | "createQuery" | "createSampler" | "createTransformFeedback" => {
            let proto = match member {
                "createBuffer" => "WebGLBuffer",
                "createFramebuffer" => "WebGLFramebuffer",
                "createProgram" => "WebGLProgram",
                "createRenderbuffer" => "WebGLRenderbuffer",
                "createShader" => "WebGLShader",
                "createTexture" => "WebGLTexture",
                "createVertexArray" => "WebGLVertexArrayObject",
                "createQuery" => "WebGLQuery",
                "createSampler" => "WebGLSampler",
                _ => "WebGLTransformFeedback",
            };
            let id = mint(scope, ctx);
            match new_gl_object(scope, proto, id) {
                Some(o) => rv.set(o),
                None => rv.set(null_val(scope)),
            }
        }
        m if m.starts_with("is") => rv.set_bool(false),
        "getAttribLocation" | "getFragDataLocation" | "getUniformBlockIndex" => rv.set_int32(-1),
        "getVertexAttribOffset" => rv.set_uint32(0),
        "checkFramebufferStatus" => rv.set_uint32(FRAMEBUFFER_UNSUPPORTED),
        "clientWaitSync" => rv.set_uint32(WAIT_FAILED),
        m if m.starts_with("get") => rv.set_null(),
        "makeXRCompatible" => {
            let msg = str_val(scope, "InvalidStateError");
            let p = crate::binding::protos::rejected_promise(scope, msg);
            rv.set(p);
        }
        // Every draw, upload, bind and state call: nothing.
        _ => {}
    }
}

/// `WEBGL_lose_context.loseContext()`.
fn lose_context(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) {
    let already = edit(scope, ctx, |s| {
        if s.lost {
            s.lost_errors.push_back(INVALID_OPERATION);
            return true;
        }
        s.lost = true;
        s.restorable = false;
        s.ops.clear();
        s.restart_list();
        s.buffer = (0, 0);
        s.aux.clear();
        s.bound.clear();
        s.errors_read = 0;
        s.lost_errors.clear();
        s.lost_errors.push_back(CONTEXT_LOST_WEBGL);
        false
    });
    if !already {
        queue_context_event(scope, ctx, "webglcontextlost");
    }
}

/// `WEBGL_lose_context.restoreContext()`.
fn restore_context(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>) {
    enum R {
        NotLost,
        Refused,
        Go,
    }
    let r = edit(scope, ctx, |s| {
        if !s.lost {
            R::NotLost
        } else if !s.restorable {
            s.lost_errors.push_back(INVALID_OPERATION);
            R::Refused
        } else {
            s.restorable = false;
            R::Go
        }
    });
    match r {
        R::NotLost => push(scope, ctx, GlOp::RaiseError { code: INVALID_OPERATION }),
        R::Refused => {}
        R::Go => queue_context_event(scope, ctx, "webglcontextrestored"),
    }
}

/// Queue the context event as a TASK -- measured: after the microtasks of the turn that lost the
/// context, before a `setTimeout(0)` queued after it. The ENGINE's task (`rendering::queue_task`), not
/// a timer of the page's: it went through the realm's `setTimeout`, so `loseContext()` took a timer id
/// (Chrome 150: two `setTimeout`s around it are 1 apart, here 2 -- 2026-10-09, the challenge
/// frame loses a WebGL 1 and a WebGL 2 context and then arms its timers).
fn queue_context_event(scope: &mut v8::PinScope, ctx: v8::Local<v8::Object>, ty: &str) {
    let data = v8::Array::new(scope, 2);
    data.set_index(scope, 0, ctx.into());
    let t = str_val(scope, ty);
    data.set_index(scope, 1, t);
    let task = build_fn_data(scope, cb_context_event_task, "", 0, data.into());
    crate::core::intersection_observer::rendering::queue_task(scope, task, 0.0);
}

fn cb_context_event_task(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, _rv: v8::ReturnValue) {
    let Ok(data) = v8::Local::<v8::Array>::try_from(crate::binding::protos::body_data(scope, &args)) else { return };
    let Some(ctx) = data.get_index(scope, 0).and_then(|v| v8::Local::<v8::Object>::try_from(v).ok()) else { return };
    let ty = data.get_index(scope, 1).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let restoring = ty == "webglcontextrestored";
    if restoring {
        // A FRESH context: no objects, default state, nothing queued. The extension objects and the
        // set of enabled extensions survive (measured: `getExtension` hands back the same object).
        edit(scope, ctx, |s| {
            s.lost = false;
            s.lost_errors.clear();
            s.ops.clear();
            s.restart_list();
            s.buffer = (0, 0);
            s.aux.clear();
            s.bound.clear();
            s.errors_read = 0;
            s.ext = ExtState::default();
            s.params = s.params.restored();
        });
        // the restored context's drawing buffer, as a new context's (the list starts with its size), and the
        // extensions that survived the loss enabled on it again: Blink's `LoseExtension(false)` keeps each enabled
        // extension, so its enums stay valid on the new context
        super::record_size(scope, ctx);
        edit(scope, ctx, |s| {
            let names = s.enabled_order.clone();
            s.ops.extend(names.into_iter().map(|name| GlOp::EnableExtension { name }));
        });
    }
    let Some(target) = priv_get(scope, ctx, super::super::canvas_2d::CANVAS_BACKREF_PRIV) else { return };
    let init = crate::binding::protos::init_dict(scope);
    let t: v8::Local<v8::Value> = v8::Boolean::new(scope, true).into();
    crate::binding::protos::set_prop(scope, init, "cancelable", t);
    let empty = str_val(scope, "");
    crate::binding::protos::set_prop(scope, init, "statusMessage", empty);
    let tyv = str_val(scope, &ty);
    let ev = crate::binding::protos::new_global(scope, "WebGLContextEvent", &[tyv, init.into()])
        .or_else(|| crate::binding::protos::new_global(scope, "Event", &[tyv, init.into()]));
    let Some(ev) = ev else { return };
    crate::core::html::html_dialog_element::mark_trusted(scope, ev);
    let not_cancelled = crate::core::events::event_target::et_dispatch(scope, target, ev);
    if !restoring && !not_cancelled {
        edit(scope, ctx, |s| {
            if s.lost {
                s.restorable = true;
            }
        });
    }
}

// ── the two object interfaces the extensions mint ───────────────────────────────────────────────

webapi::interface! {
    "WebGLVertexArrayObjectOES" [interface, both] {
    }
}

webapi::interface! {
    "WebGLTimerQueryEXT" [interface, both] {
    }
}

// ── the tables ──────────────────────────────────────────────────────────────────────────────

webapi::interface! {
    "ANGLEInstancedArrays" [interface, both] {
        constant "VERTEX_ATTRIB_ARRAY_DIVISOR_ANGLE" : Any = 35070,
        method "drawArraysInstancedANGLE" : Any len 4 required 4 = install_gl_extension,
        method "drawElementsInstancedANGLE" : Any len 5 required 5 = install_gl_extension,
        method "vertexAttribDivisorANGLE" : Any len 2 required 2 = install_gl_extension
    }
}

webapi::interface! {
    "EXTBlendMinMax" [interface, both] {
        constant "MIN_EXT" : Any = 32775,
        constant "MAX_EXT" : Any = 32776
    }
}

webapi::interface! {
    "EXTClipControl" [interface, both] {
        constant "LOWER_LEFT_EXT" : Any = 36001,
        constant "UPPER_LEFT_EXT" : Any = 36002,
        constant "NEGATIVE_ONE_TO_ONE_EXT" : Any = 37726,
        constant "ZERO_TO_ONE_EXT" : Any = 37727,
        constant "CLIP_ORIGIN_EXT" : Any = 37724,
        constant "CLIP_DEPTH_MODE_EXT" : Any = 37725,
        method "clipControlEXT" : Any len 2 required 2 = install_gl_extension
    }
}

webapi::interface! {
    "EXTColorBufferFloat" [interface, both] {

    }
}

webapi::interface! {
    "EXTColorBufferHalfFloat" [interface, both] {
        constant "RGBA16F_EXT" : Any = 34842,
        constant "RGB16F_EXT" : Any = 34843,
        constant "FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT" : Any = 33297,
        constant "UNSIGNED_NORMALIZED_EXT" : Any = 35863
    }
}

webapi::interface! {
    "EXTConservativeDepth" [interface, both] {

    }
}

webapi::interface! {
    "EXTDepthClamp" [interface, both] {
        constant "DEPTH_CLAMP_EXT" : Any = 34383
    }
}

webapi::interface! {
    "EXTDisjointTimerQuery" [interface, both] {
        constant "QUERY_COUNTER_BITS_EXT" : Any = 34916,
        constant "CURRENT_QUERY_EXT" : Any = 34917,
        constant "QUERY_RESULT_EXT" : Any = 34918,
        constant "QUERY_RESULT_AVAILABLE_EXT" : Any = 34919,
        constant "TIME_ELAPSED_EXT" : Any = 35007,
        constant "TIMESTAMP_EXT" : Any = 36392,
        constant "GPU_DISJOINT_EXT" : Any = 36795,
        method "beginQueryEXT" : Any len 2 required 2 = install_gl_extension,
        method "createQueryEXT" : Any len 0 required 0 = install_gl_extension,
        method "deleteQueryEXT" : Any len 1 required 1 = install_gl_extension,
        method "endQueryEXT" : Any len 1 required 1 = install_gl_extension,
        method "getQueryEXT" : Any len 2 required 2 = install_gl_extension,
        method "getQueryObjectEXT" : Any len 2 required 2 = install_gl_extension,
        method "isQueryEXT" : Any len 1 required 1 = install_gl_extension,
        method "queryCounterEXT" : Any len 2 required 2 = install_gl_extension
    }
}

webapi::interface! {
    "EXTDisjointTimerQueryWebGL2" [interface, both] {
        constant "QUERY_COUNTER_BITS_EXT" : Any = 34916,
        constant "TIME_ELAPSED_EXT" : Any = 35007,
        constant "TIMESTAMP_EXT" : Any = 36392,
        constant "GPU_DISJOINT_EXT" : Any = 36795,
        method "queryCounterEXT" : Any len 2 required 2 = install_gl_extension
    }
}

webapi::interface! {
    "EXTFloatBlend" [interface, both] {

    }
}

webapi::interface! {
    "EXTFragDepth" [interface, both] {

    }
}

webapi::interface! {
    "EXTPolygonOffsetClamp" [interface, both] {
        constant "POLYGON_OFFSET_CLAMP_EXT" : Any = 36379,
        method "polygonOffsetClampEXT" : Any len 3 required 3 = install_gl_extension
    }
}

webapi::interface! {
    "EXTRenderSnorm" [interface, both] {

    }
}

webapi::interface! {
    "EXTsRGB" [interface, both] {
        constant "SRGB_EXT" : Any = 35904,
        constant "SRGB_ALPHA_EXT" : Any = 35906,
        constant "SRGB8_ALPHA8_EXT" : Any = 35907,
        constant "FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING_EXT" : Any = 33296
    }
}

webapi::interface! {
    "EXTShaderTextureLOD" [interface, both] {

    }
}

webapi::interface! {
    "EXTTextureCompressionBPTC" [interface, both] {
        constant "COMPRESSED_RGBA_BPTC_UNORM_EXT" : Any = 36492,
        constant "COMPRESSED_SRGB_ALPHA_BPTC_UNORM_EXT" : Any = 36493,
        constant "COMPRESSED_RGB_BPTC_SIGNED_FLOAT_EXT" : Any = 36494,
        constant "COMPRESSED_RGB_BPTC_UNSIGNED_FLOAT_EXT" : Any = 36495
    }
}

webapi::interface! {
    "EXTTextureCompressionRGTC" [interface, both] {
        constant "COMPRESSED_RED_RGTC1_EXT" : Any = 36283,
        constant "COMPRESSED_SIGNED_RED_RGTC1_EXT" : Any = 36284,
        constant "COMPRESSED_RED_GREEN_RGTC2_EXT" : Any = 36285,
        constant "COMPRESSED_SIGNED_RED_GREEN_RGTC2_EXT" : Any = 36286
    }
}

webapi::interface! {
    "EXTTextureFilterAnisotropic" [interface, both] {
        constant "TEXTURE_MAX_ANISOTROPY_EXT" : Any = 34046,
        constant "MAX_TEXTURE_MAX_ANISOTROPY_EXT" : Any = 34047
    }
}

webapi::interface! {
    "EXTTextureMirrorClampToEdge" [interface, both] {
        constant "MIRROR_CLAMP_TO_EDGE_EXT" : Any = 34627
    }
}

webapi::interface! {
    "EXTTextureNorm16" [interface, both] {
        constant "R16_EXT" : Any = 33322,
        constant "RG16_EXT" : Any = 33324,
        constant "RGB16_EXT" : Any = 32852,
        constant "RGBA16_EXT" : Any = 32859,
        constant "R16_SNORM_EXT" : Any = 36760,
        constant "RG16_SNORM_EXT" : Any = 36761,
        constant "RGB16_SNORM_EXT" : Any = 36762,
        constant "RGBA16_SNORM_EXT" : Any = 36763
    }
}

webapi::interface! {
    "KHRParallelShaderCompile" [interface, both] {
        constant "COMPLETION_STATUS_KHR" : Any = 37297
    }
}

webapi::interface! {
    "NVShaderNoperspectiveInterpolation" [interface, both] {

    }
}

webapi::interface! {
    "OESDrawBuffersIndexed" [interface, both] {
        method "blendEquationSeparateiOES" : Any len 3 required 3 = install_gl_extension,
        method "blendEquationiOES" : Any len 2 required 2 = install_gl_extension,
        method "blendFuncSeparateiOES" : Any len 5 required 5 = install_gl_extension,
        method "blendFunciOES" : Any len 3 required 3 = install_gl_extension,
        method "colorMaskiOES" : Any len 5 required 5 = install_gl_extension,
        method "disableiOES" : Any len 2 required 2 = install_gl_extension,
        method "enableiOES" : Any len 2 required 2 = install_gl_extension
    }
}

webapi::interface! {
    "OESElementIndexUint" [interface, both] {

    }
}

webapi::interface! {
    "OESFboRenderMipmap" [interface, both] {

    }
}

webapi::interface! {
    "OESSampleVariables" [interface, both] {

    }
}

webapi::interface! {
    "OESShaderMultisampleInterpolation" [interface, both] {
        constant "MIN_FRAGMENT_INTERPOLATION_OFFSET_OES" : Any = 36443,
        constant "MAX_FRAGMENT_INTERPOLATION_OFFSET_OES" : Any = 36444,
        constant "FRAGMENT_INTERPOLATION_OFFSET_BITS_OES" : Any = 36445
    }
}

webapi::interface! {
    "OESStandardDerivatives" [interface, both] {
        constant "FRAGMENT_SHADER_DERIVATIVE_HINT_OES" : Any = 35723
    }
}

webapi::interface! {
    "OESTextureFloat" [interface, both] {

    }
}

webapi::interface! {
    "OESTextureFloatLinear" [interface, both] {

    }
}

webapi::interface! {
    "OESTextureHalfFloat" [interface, both] {
        constant "HALF_FLOAT_OES" : Any = 36193
    }
}

webapi::interface! {
    "OESTextureHalfFloatLinear" [interface, both] {

    }
}

webapi::interface! {
    "OESVertexArrayObject" [interface, both] {
        constant "VERTEX_ARRAY_BINDING_OES" : Any = 34229,
        method "bindVertexArrayOES" : Any len 0 required 0 = install_gl_extension,
        method "createVertexArrayOES" : Any len 0 required 0 = install_gl_extension,
        method "deleteVertexArrayOES" : Any len 0 required 0 = install_gl_extension,
        method "isVertexArrayOES" : Any len 0 required 0 = install_gl_extension
    }
}

webapi::interface! {
    "OVRMultiview2" [interface, both] {
        constant "FRAMEBUFFER_ATTACHMENT_TEXTURE_NUM_VIEWS_OVR" : Any = 38448,
        constant "FRAMEBUFFER_ATTACHMENT_TEXTURE_BASE_VIEW_INDEX_OVR" : Any = 38450,
        constant "MAX_VIEWS_OVR" : Any = 38449,
        constant "FRAMEBUFFER_INCOMPLETE_VIEW_TARGETS_OVR" : Any = 38451,
        method "framebufferTextureMultiviewOVR" : Any len 6 required 6 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLBlendFuncExtended" [interface, both] {
        constant "SRC1_COLOR_WEBGL" : Any = 35065,
        constant "SRC1_ALPHA_WEBGL" : Any = 34185,
        constant "ONE_MINUS_SRC1_COLOR_WEBGL" : Any = 35066,
        constant "ONE_MINUS_SRC1_ALPHA_WEBGL" : Any = 35067,
        constant "MAX_DUAL_SOURCE_DRAW_BUFFERS_WEBGL" : Any = 35068
    }
}

webapi::interface! {
    "WebGLClipCullDistance" [interface, both] {
        constant "MAX_CLIP_DISTANCES_WEBGL" : Any = 3378,
        constant "MAX_CULL_DISTANCES_WEBGL" : Any = 33529,
        constant "MAX_COMBINED_CLIP_AND_CULL_DISTANCES_WEBGL" : Any = 33530,
        constant "CLIP_DISTANCE0_WEBGL" : Any = 12288,
        constant "CLIP_DISTANCE1_WEBGL" : Any = 12289,
        constant "CLIP_DISTANCE2_WEBGL" : Any = 12290,
        constant "CLIP_DISTANCE3_WEBGL" : Any = 12291,
        constant "CLIP_DISTANCE4_WEBGL" : Any = 12292,
        constant "CLIP_DISTANCE5_WEBGL" : Any = 12293,
        constant "CLIP_DISTANCE6_WEBGL" : Any = 12294,
        constant "CLIP_DISTANCE7_WEBGL" : Any = 12295
    }
}

webapi::interface! {
    "WebGLColorBufferFloat" [interface, both] {
        constant "RGBA32F_EXT" : Any = 34836,
        constant "FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT" : Any = 33297,
        constant "UNSIGNED_NORMALIZED_EXT" : Any = 35863
    }
}

webapi::interface! {
    "WebGLCompressedTextureASTC" [interface, both] {
        constant "COMPRESSED_RGBA_ASTC_4x4_KHR" : Any = 37808,
        constant "COMPRESSED_RGBA_ASTC_5x4_KHR" : Any = 37809,
        constant "COMPRESSED_RGBA_ASTC_5x5_KHR" : Any = 37810,
        constant "COMPRESSED_RGBA_ASTC_6x5_KHR" : Any = 37811,
        constant "COMPRESSED_RGBA_ASTC_6x6_KHR" : Any = 37812,
        constant "COMPRESSED_RGBA_ASTC_8x5_KHR" : Any = 37813,
        constant "COMPRESSED_RGBA_ASTC_8x6_KHR" : Any = 37814,
        constant "COMPRESSED_RGBA_ASTC_8x8_KHR" : Any = 37815,
        constant "COMPRESSED_RGBA_ASTC_10x5_KHR" : Any = 37816,
        constant "COMPRESSED_RGBA_ASTC_10x6_KHR" : Any = 37817,
        constant "COMPRESSED_RGBA_ASTC_10x8_KHR" : Any = 37818,
        constant "COMPRESSED_RGBA_ASTC_10x10_KHR" : Any = 37819,
        constant "COMPRESSED_RGBA_ASTC_12x10_KHR" : Any = 37820,
        constant "COMPRESSED_RGBA_ASTC_12x12_KHR" : Any = 37821,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_4x4_KHR" : Any = 37840,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_5x4_KHR" : Any = 37841,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_5x5_KHR" : Any = 37842,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_6x5_KHR" : Any = 37843,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_6x6_KHR" : Any = 37844,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_8x5_KHR" : Any = 37845,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_8x6_KHR" : Any = 37846,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_8x8_KHR" : Any = 37847,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_10x5_KHR" : Any = 37848,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_10x6_KHR" : Any = 37849,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_10x8_KHR" : Any = 37850,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_10x10_KHR" : Any = 37851,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_12x10_KHR" : Any = 37852,
        constant "COMPRESSED_SRGB8_ALPHA8_ASTC_12x12_KHR" : Any = 37853,
        method "getSupportedProfiles" : Any len 0 required 0 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLCompressedTextureETC" [interface, both] {
        constant "COMPRESSED_R11_EAC" : Any = 37488,
        constant "COMPRESSED_SIGNED_R11_EAC" : Any = 37489,
        constant "COMPRESSED_RG11_EAC" : Any = 37490,
        constant "COMPRESSED_SIGNED_RG11_EAC" : Any = 37491,
        constant "COMPRESSED_RGB8_ETC2" : Any = 37492,
        constant "COMPRESSED_SRGB8_ETC2" : Any = 37493,
        constant "COMPRESSED_RGB8_PUNCHTHROUGH_ALPHA1_ETC2" : Any = 37494,
        constant "COMPRESSED_SRGB8_PUNCHTHROUGH_ALPHA1_ETC2" : Any = 37495,
        constant "COMPRESSED_RGBA8_ETC2_EAC" : Any = 37496,
        constant "COMPRESSED_SRGB8_ALPHA8_ETC2_EAC" : Any = 37497
    }
}

webapi::interface! {
    "WebGLCompressedTextureETC1" [interface, both] {
        constant "COMPRESSED_RGB_ETC1_WEBGL" : Any = 36196
    }
}

webapi::interface! {
    "WebGLCompressedTextureS3TC" [interface, both] {
        constant "COMPRESSED_RGB_S3TC_DXT1_EXT" : Any = 33776,
        constant "COMPRESSED_RGBA_S3TC_DXT1_EXT" : Any = 33777,
        constant "COMPRESSED_RGBA_S3TC_DXT3_EXT" : Any = 33778,
        constant "COMPRESSED_RGBA_S3TC_DXT5_EXT" : Any = 33779
    }
}

webapi::interface! {
    "WebGLCompressedTextureS3TCsRGB" [interface, both] {
        constant "COMPRESSED_SRGB_S3TC_DXT1_EXT" : Any = 35916,
        constant "COMPRESSED_SRGB_ALPHA_S3TC_DXT1_EXT" : Any = 35917,
        constant "COMPRESSED_SRGB_ALPHA_S3TC_DXT3_EXT" : Any = 35918,
        constant "COMPRESSED_SRGB_ALPHA_S3TC_DXT5_EXT" : Any = 35919
    }
}

webapi::interface! {
    "WebGLDebugRendererInfo" [interface, both] {
        constant "UNMASKED_VENDOR_WEBGL" : Any = 37445,
        constant "UNMASKED_RENDERER_WEBGL" : Any = 37446
    }
}

webapi::interface! {
    "WebGLDebugShaders" [interface, both] {
        method "getTranslatedShaderSource" : Any len 1 required 1 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLDepthTexture" [interface, both] {
        constant "UNSIGNED_INT_24_8_WEBGL" : Any = 34042
    }
}

webapi::interface! {
    "WebGLDrawBuffers" [interface, both] {
        constant "COLOR_ATTACHMENT0_WEBGL" : Any = 36064,
        constant "COLOR_ATTACHMENT1_WEBGL" : Any = 36065,
        constant "COLOR_ATTACHMENT2_WEBGL" : Any = 36066,
        constant "COLOR_ATTACHMENT3_WEBGL" : Any = 36067,
        constant "COLOR_ATTACHMENT4_WEBGL" : Any = 36068,
        constant "COLOR_ATTACHMENT5_WEBGL" : Any = 36069,
        constant "COLOR_ATTACHMENT6_WEBGL" : Any = 36070,
        constant "COLOR_ATTACHMENT7_WEBGL" : Any = 36071,
        constant "COLOR_ATTACHMENT8_WEBGL" : Any = 36072,
        constant "COLOR_ATTACHMENT9_WEBGL" : Any = 36073,
        constant "COLOR_ATTACHMENT10_WEBGL" : Any = 36074,
        constant "COLOR_ATTACHMENT11_WEBGL" : Any = 36075,
        constant "COLOR_ATTACHMENT12_WEBGL" : Any = 36076,
        constant "COLOR_ATTACHMENT13_WEBGL" : Any = 36077,
        constant "COLOR_ATTACHMENT14_WEBGL" : Any = 36078,
        constant "COLOR_ATTACHMENT15_WEBGL" : Any = 36079,
        constant "DRAW_BUFFER0_WEBGL" : Any = 34853,
        constant "DRAW_BUFFER1_WEBGL" : Any = 34854,
        constant "DRAW_BUFFER2_WEBGL" : Any = 34855,
        constant "DRAW_BUFFER3_WEBGL" : Any = 34856,
        constant "DRAW_BUFFER4_WEBGL" : Any = 34857,
        constant "DRAW_BUFFER5_WEBGL" : Any = 34858,
        constant "DRAW_BUFFER6_WEBGL" : Any = 34859,
        constant "DRAW_BUFFER7_WEBGL" : Any = 34860,
        constant "DRAW_BUFFER8_WEBGL" : Any = 34861,
        constant "DRAW_BUFFER9_WEBGL" : Any = 34862,
        constant "DRAW_BUFFER10_WEBGL" : Any = 34863,
        constant "DRAW_BUFFER11_WEBGL" : Any = 34864,
        constant "DRAW_BUFFER12_WEBGL" : Any = 34865,
        constant "DRAW_BUFFER13_WEBGL" : Any = 34866,
        constant "DRAW_BUFFER14_WEBGL" : Any = 34867,
        constant "DRAW_BUFFER15_WEBGL" : Any = 34868,
        constant "MAX_COLOR_ATTACHMENTS_WEBGL" : Any = 36063,
        constant "MAX_DRAW_BUFFERS_WEBGL" : Any = 34852,
        method "drawBuffersWEBGL" : Any len 1 required 1 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLLoseContext" [interface, both] {
        method "loseContext" : Any len 0 required 0 = install_gl_extension,
        method "restoreContext" : Any len 0 required 0 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLMultiDraw" [interface, both] {
        method "multiDrawArraysInstancedWEBGL" : Any len 8 required 8 = install_gl_extension,
        method "multiDrawArraysWEBGL" : Any len 6 required 6 = install_gl_extension,
        method "multiDrawElementsInstancedWEBGL" : Any len 9 required 9 = install_gl_extension,
        method "multiDrawElementsWEBGL" : Any len 7 required 7 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLPolygonMode" [interface, both] {
        constant "POLYGON_MODE_WEBGL" : Any = 2880,
        constant "POLYGON_OFFSET_LINE_WEBGL" : Any = 10754,
        constant "LINE_WEBGL" : Any = 6913,
        constant "FILL_WEBGL" : Any = 6914,
        method "polygonModeWEBGL" : Any len 2 required 2 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLProvokingVertex" [interface, both] {
        constant "FIRST_VERTEX_CONVENTION_WEBGL" : Any = 36429,
        constant "LAST_VERTEX_CONVENTION_WEBGL" : Any = 36430,
        constant "PROVOKING_VERTEX_WEBGL" : Any = 36431,
        method "provokingVertexWEBGL" : Any len 1 required 1 = install_gl_extension
    }
}

webapi::interface! {
    "WebGLStencilTexturing" [interface, both] {
        constant "DEPTH_STENCIL_TEXTURE_MODE_WEBGL" : Any = 37098,
        constant "STENCIL_INDEX_WEBGL" : Any = 6401
    }
}

/// Every extension this engine can hand out, by the name `getExtension` is asked for, with its
/// table. Which of them a context OFFERS is the persona's list, never this one.
pub(crate) static EXTENSIONS: &[(&str, &crate::install::InterfaceDef)] = &[
    ("ANGLE_instanced_arrays", &ANGLEINSTANCEDARRAYS),
    ("EXT_blend_minmax", &EXTBLENDMINMAX),
    ("EXT_clip_control", &EXTCLIPCONTROL),
    ("EXT_color_buffer_float", &EXTCOLORBUFFERFLOAT),
    ("EXT_color_buffer_half_float", &EXTCOLORBUFFERHALFFLOAT),
    ("EXT_conservative_depth", &EXTCONSERVATIVEDEPTH),
    ("EXT_depth_clamp", &EXTDEPTHCLAMP),
    ("EXT_disjoint_timer_query", &EXTDISJOINTTIMERQUERY),
    ("EXT_disjoint_timer_query_webgl2", &EXTDISJOINTTIMERQUERYWEBGL2),
    ("EXT_float_blend", &EXTFLOATBLEND),
    ("EXT_frag_depth", &EXTFRAGDEPTH),
    ("EXT_polygon_offset_clamp", &EXTPOLYGONOFFSETCLAMP),
    ("EXT_render_snorm", &EXTRENDERSNORM),
    ("EXT_sRGB", &EXTSRGB),
    ("EXT_shader_texture_lod", &EXTSHADERTEXTURELOD),
    ("EXT_texture_compression_bptc", &EXTTEXTURECOMPRESSIONBPTC),
    ("EXT_texture_compression_rgtc", &EXTTEXTURECOMPRESSIONRGTC),
    ("EXT_texture_filter_anisotropic", &EXTTEXTUREFILTERANISOTROPIC),
    ("EXT_texture_mirror_clamp_to_edge", &EXTTEXTUREMIRRORCLAMPTOEDGE),
    ("EXT_texture_norm16", &EXTTEXTURENORM16),
    ("KHR_parallel_shader_compile", &KHRPARALLELSHADERCOMPILE),
    ("NV_shader_noperspective_interpolation", &NVSHADERNOPERSPECTIVEINTERPOLATION),
    ("OES_draw_buffers_indexed", &OESDRAWBUFFERSINDEXED),
    ("OES_element_index_uint", &OESELEMENTINDEXUINT),
    ("OES_fbo_render_mipmap", &OESFBORENDERMIPMAP),
    ("OES_sample_variables", &OESSAMPLEVARIABLES),
    ("OES_shader_multisample_interpolation", &OESSHADERMULTISAMPLEINTERPOLATION),
    ("OES_standard_derivatives", &OESSTANDARDDERIVATIVES),
    ("OES_texture_float", &OESTEXTUREFLOAT),
    ("OES_texture_float_linear", &OESTEXTUREFLOATLINEAR),
    ("OES_texture_half_float", &OESTEXTUREHALFFLOAT),
    ("OES_texture_half_float_linear", &OESTEXTUREHALFFLOATLINEAR),
    ("OES_vertex_array_object", &OESVERTEXARRAYOBJECT),
    ("OVR_multiview2", &OVRMULTIVIEW2),
    ("WEBGL_blend_func_extended", &WEBGLBLENDFUNCEXTENDED),
    ("WEBGL_clip_cull_distance", &WEBGLCLIPCULLDISTANCE),
    ("WEBGL_color_buffer_float", &WEBGLCOLORBUFFERFLOAT),
    ("WEBGL_compressed_texture_astc", &WEBGLCOMPRESSEDTEXTUREASTC),
    ("WEBGL_compressed_texture_etc", &WEBGLCOMPRESSEDTEXTUREETC),
    ("WEBGL_compressed_texture_etc1", &WEBGLCOMPRESSEDTEXTUREETC1),
    ("WEBGL_compressed_texture_s3tc", &WEBGLCOMPRESSEDTEXTURES3TC),
    ("WEBGL_compressed_texture_s3tc_srgb", &WEBGLCOMPRESSEDTEXTURES3TCSRGB),
    ("WEBGL_debug_renderer_info", &WEBGLDEBUGRENDERERINFO),
    ("WEBGL_debug_shaders", &WEBGLDEBUGSHADERS),
    ("WEBGL_depth_texture", &WEBGLDEPTHTEXTURE),
    ("WEBGL_draw_buffers", &WEBGLDRAWBUFFERS),
    ("WEBGL_lose_context", &WEBGLLOSECONTEXT),
    ("WEBGL_multi_draw", &WEBGLMULTIDRAW),
    ("WEBGL_polygon_mode", &WEBGLPOLYGONMODE),
    ("WEBGL_provoking_vertex", &WEBGLPROVOKINGVERTEX),
    ("WEBGL_stencil_texturing", &WEBGLSTENCILTEXTURING),
];

/// The compressed formats the context's enabled extensions add (`COMPRESSED_TEXTURE_FORMATS`) -- the
/// formats the `compressedTex*` calls accept.
pub(crate) fn compressed_formats(s: &super::WebGLState) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for e in &s.enabled_order {
        for f in super::params::compressed_formats(e) {
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}
